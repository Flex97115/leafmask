//! Create a logical database dump (feature `dump.create`).
//!
//! Reads collections from a [`MongoSource`], applies configured transformations
//! and per-collection query filters inline while streaming, captures indexes and
//! collection options, and writes a timestamp-identified dump (optionally
//! gzip-compressed) to storage. Binary/large field values are preserved as BSON
//! binary inline with their owning document — no separate GridFS pass.
//!
//! A dump requires `common.tmp_dir`; without it the command fails fast.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use bson::Document;
use chrono::{DateTime, Utc};

use crate::error::{Error, Result};
use crate::hash::HashEngine;
use crate::mongo::{CollectionStructure, MongoSource};
use crate::storage::Storage;
use crate::transform::{apply::TransformationPlan, Registry};

use super::{
    data_path, write_collection_meta, write_metadata, CollectionDataWriter, CollectionMeta,
    CollectionToc, DatabaseToc, DumpMetadata, DumpStatus,
};

/// Filters and options controlling a dump.
#[derive(Debug, Clone, Default)]
pub struct DumpOptions {
    /// Required temporary working directory; without it the dump fails fast.
    pub tmp_dir: Option<String>,
    pub include_databases: Vec<String>,
    pub exclude_databases: Vec<String>,
    pub include_collections: Vec<String>,
    pub exclude_collections: Vec<String>,
    /// Compress collection data with gzip.
    pub gzip: bool,
    /// Number of collections to dump concurrently. `0` or `1` (the default)
    /// dumps sequentially.
    pub parallel_jobs: usize,
    /// Exclude index definitions and collection options from the dump.
    pub no_indexes: bool,
}

impl DumpOptions {
    fn include_db(&self, name: &str) -> bool {
        if self.exclude_databases.iter().any(|d| d == name) {
            return false;
        }
        self.include_databases.is_empty() || self.include_databases.iter().any(|d| d == name)
    }
    fn include_collection(&self, name: &str) -> bool {
        if self.exclude_collections.iter().any(|c| c == name) {
            return false;
        }
        self.include_collections.is_empty() || self.include_collections.iter().any(|c| c == name)
    }
}

/// The dump driver.
pub struct Dump<'a> {
    pub storage: &'a dyn Storage,
    pub source: &'a dyn MongoSource,
    pub registry: &'a Registry,
    pub engine: &'a HashEngine,
    /// Optional transformation plan (anonymization + query filtering).
    pub plan: Option<&'a TransformationPlan>,
    /// Per-collection MongoDB filter (from `subset_conds` and/or a collection's
    /// `query`), pushed down to the source so a narrow filter doesn't require
    /// fetching the whole collection first.
    pub filters: BTreeMap<String, Document>,
    pub options: DumpOptions,
}

/// One collection to dump, with its position in the source's enumeration
/// order — preserved as `restore_order` regardless of which worker thread
/// (or completion order) actually processes it.
struct WorkItem {
    db_index: usize,
    restore_order: u32,
    db: String,
    coll: String,
}

/// A dumped collection's total bytes written (data blob + structure blob)
/// and its TOC entry.
type DumpItemResult = Result<(u64, CollectionToc)>;

impl<'a> Dump<'a> {
    /// Run the dump, stamping it with an id derived from `created_at`.
    pub fn run(&self, created_at: DateTime<Utc>) -> Result<DumpMetadata> {
        let tmp_dir = match self.options.tmp_dir.as_deref() {
            Some(t) if !t.is_empty() => t,
            _ => {
                return Err(Error::Config(
                    "common.tmp_dir must be configured before dumping".into(),
                ))
            }
        };

        let id = created_at.format("%Y%m%dT%H%M%SZ").to_string();

        // Enumerate databases and collections up front, in source order —
        // cheap (just names), and it lets dumping itself be parallelized
        // without disturbing the deterministic TOC order below.
        let mut db_names = Vec::new();
        let mut items = Vec::new();
        for db in self.source.databases()? {
            if !self.options.include_db(&db) {
                continue;
            }
            log::info!("dumping database {db}");
            let db_index = db_names.len();
            let mut restore_order = 0u32;
            for coll in self.source.collections(&db)? {
                if !self.options.include_collection(&coll) {
                    continue;
                }
                items.push(WorkItem {
                    db_index,
                    restore_order,
                    db: db.clone(),
                    coll,
                });
                restore_order += 1;
            }
            db_names.push(db);
        }

        let jobs = self.options.parallel_jobs.max(1);
        let results: Vec<Option<DumpItemResult>> = if jobs <= 1 || items.len() <= 1 {
            items
                .iter()
                .map(|item| Some(self.dump_item(&id, tmp_dir, item)))
                .collect()
        } else {
            self.dump_items_parallel(&id, tmp_dir, &items, jobs)
        };

        let mut total_size = 0u64;
        let mut collections_by_db: Vec<Vec<CollectionToc>> = vec![Vec::new(); db_names.len()];
        for (item, result) in items.iter().zip(results) {
            match result {
                Some(Ok((size, toc))) => {
                    total_size += size;
                    collections_by_db[item.db_index].push(toc);
                }
                Some(Err(e)) => return Err(e),
                // Never claimed: dispatch stopped after an earlier item (in
                // whatever order threads picked them up) failed.
                None => continue,
            }
        }

        let databases = db_names
            .into_iter()
            .zip(collections_by_db)
            .map(|(name, collections)| DatabaseToc { name, collections })
            .collect();

        let meta = DumpMetadata {
            id,
            status: DumpStatus::Done,
            created_at: created_at.to_rfc3339(),
            databases,
            size: total_size,
        };
        write_metadata(self.storage, &meta)?;
        Ok(meta)
    }

    /// Dump `items` across `jobs` worker threads pulling from a shared
    /// cursor. On the first failure, no new item is claimed, but items
    /// already claimed run to completion — so a mid-dump error never cuts
    /// off a spool file or upload in progress. Returns one slot per item,
    /// `None` where dispatch stopped before that item was ever claimed.
    fn dump_items_parallel(
        &self,
        id: &str,
        tmp_dir: &str,
        items: &[WorkItem],
        jobs: usize,
    ) -> Vec<Option<DumpItemResult>> {
        let jobs = jobs.min(items.len());
        let results: Vec<Mutex<Option<DumpItemResult>>> =
            (0..items.len()).map(|_| Mutex::new(None)).collect();
        let next = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);

        std::thread::scope(|scope| {
            for _ in 0..jobs {
                scope.spawn(|| loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= items.len() {
                        break;
                    }
                    let result = self.dump_item(id, tmp_dir, &items[i]);
                    if let Err(e) = &result {
                        // Items already claimed (possibly still running, e.g.
                        // a large in-flight collection on another thread)
                        // finish normally, so this failure may not surface as
                        // the run's final error for a while yet — log it now.
                        log::error!("  {}.{}: dump failed: {e}", items[i].db, items[i].coll);
                        stop.store(true, Ordering::Relaxed);
                    }
                    *results[i].lock().unwrap() = Some(result);
                });
            }
        });

        results
            .into_iter()
            .map(|m| m.into_inner().unwrap())
            .collect()
    }

    /// Dump one collection's data and structure.
    fn dump_item(&self, id: &str, tmp_dir: &str, item: &WorkItem) -> DumpItemResult {
        let (db, coll) = (item.db.as_str(), item.coll.as_str());
        let (count, mut size) = self.dump_collection(id, tmp_dir, db, coll)?;

        let structure = if self.options.no_indexes {
            CollectionStructure::default()
        } else {
            self.source.read_structure(db, coll)?
        };
        let toc = CollectionToc {
            name: coll.to_string(),
            document_count: count,
            indexes: structure.indexes.iter().map(|i| i.name.clone()).collect(),
            restore_order: item.restore_order,
        };

        size += write_collection_meta(
            self.storage,
            id,
            &CollectionMeta {
                database: db.to_string(),
                collection: coll.to_string(),
                document_count: count,
                indexes: structure.indexes,
                validator: structure.validator,
                options: structure.options,
            },
        )?;
        Ok((size, toc))
    }

    /// Stream one collection's documents from the source into storage —
    /// filter pushdown, per-document transformation, and (optionally gzipped)
    /// spooling all happen inline, so memory stays bounded regardless of the
    /// collection's size. Returns (documents written, blob bytes).
    fn dump_collection(&self, id: &str, tmp_dir: &str, db: &str, coll: &str) -> Result<(u64, u64)> {
        let path = format!("{id}/{}", data_path(db, coll, self.options.gzip));
        let mut writer = CollectionDataWriter::create(
            self.storage,
            &path,
            tmp_dir,
            db,
            coll,
            self.options.gzip,
        )?;
        let mut read = 0u64;
        self.source
            .stream_documents(db, coll, self.filters.get(coll), &mut |doc| {
                read += 1;
                let out = match self.plan {
                    Some(plan) => {
                        if !plan.should_include(coll, &doc) {
                            return Ok(()); // custom query filter excludes this document.
                        }
                        plan.transform(self.registry, self.engine, coll, &doc)?
                    }
                    None => doc,
                };
                writer.write_document(&out)?;
                if writer.count() % 100_000 == 0 {
                    log::info!("  {db}.{coll}: {} documents...", writer.count());
                }
                Ok(())
            })?;

        let count = writer.count();
        let size = writer.finish(self.storage)?;
        if count != read {
            log::info!("  {db}.{coll}: {count} of {read} documents");
        } else {
            log::info!("  {db}.{coll}: {count} documents");
        }
        Ok((count, size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dump::{read_collection_full, read_metadata};
    use crate::mongo::{CollectionData, InMemoryMongo};
    use crate::storage::DirectoryStorage;
    use crate::validate::IndexSpec;
    use bson::{spec::BinarySubtype, Binary, Bson, Document};

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn user(id: i64, email: &str) -> Document {
        let mut d = Document::new();
        d.insert("_id", Bson::Int64(id));
        d.insert("email", Bson::String(email.into()));
        d
    }

    fn source_with_users() -> InMemoryMongo {
        let m = InMemoryMongo::new();
        m.seed(CollectionData {
            database: "app".into(),
            collection: "users".into(),
            documents: vec![user(1, "a@x.com"), user(2, "b@x.com")],
            indexes: vec![IndexSpec {
                name: "email_idx".into(),
                keys: vec![("email".into(), 1)],
                unique: true,
            }],
            validator: Some(Bson::String("schema".into())),
            options: Default::default(),
        });
        m
    }

    fn dump<'a>(
        storage: &'a DirectoryStorage,
        source: &'a InMemoryMongo,
        registry: &'a Registry,
        engine: &'a HashEngine,
        options: DumpOptions,
    ) -> Dump<'a> {
        Dump {
            storage,
            source,
            registry,
            engine,
            plan: None,
            filters: BTreeMap::new(),
            options,
        }
    }

    // Acceptance: produces a timestamp-identified dump in storage; indexes and
    // collection options are captured.
    #[test]
    fn produces_dump_with_metadata_and_structure() {
        let dir = tempfile::tempdir().unwrap();
        let s = DirectoryStorage::new(dir.path()).unwrap();
        let m = source_with_users();
        let (r, e) = (Registry::with_builtins(), HashEngine::new("s"));
        let opts = DumpOptions {
            tmp_dir: Some(std::env::temp_dir().display().to_string()),
            ..Default::default()
        };

        let meta = dump(&s, &m, &r, &e, opts)
            .run(at("2026-07-18T12:00:00Z"))
            .unwrap();
        assert_eq!(meta.id, "20260718T120000Z");
        assert_eq!(meta.status, DumpStatus::Done);

        // metadata is readable back; structure captured.
        let read = read_metadata(&s, &meta.id).unwrap();
        let coll = &read.databases[0].collections[0];
        assert_eq!(coll.document_count, 2);
        assert_eq!(coll.indexes, vec!["email_idx".to_string()]);

        let full = read_collection_full(&s, &meta.id, "app", "users").unwrap();
        assert_eq!(full.validator, Some(Bson::String("schema".into())));
        assert!(full.indexes[0].unique);
    }

    // Acceptance: include/exclude filters restrict what is dumped.
    #[test]
    fn filters_restrict_collections() {
        let dir = tempfile::tempdir().unwrap();
        let s = DirectoryStorage::new(dir.path()).unwrap();
        let m = source_with_users();
        m.seed(CollectionData {
            database: "app".into(),
            collection: "secrets".into(),
            documents: vec![user(9, "s@x.com")],
            ..Default::default()
        });
        let (r, e) = (Registry::with_builtins(), HashEngine::new("s"));
        let opts = DumpOptions {
            tmp_dir: Some(std::env::temp_dir().display().to_string()),
            exclude_collections: vec!["secrets".into()],
            ..Default::default()
        };
        let meta = dump(&s, &m, &r, &e, opts)
            .run(at("2026-07-18T12:00:00Z"))
            .unwrap();
        let names: Vec<&str> = meta.databases[0]
            .collections
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, vec!["users"]);
    }

    // Acceptance: binary field values are captured inline as BSON binary
    // (also exercises gzip compression).
    #[test]
    fn binary_fields_survive_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s = DirectoryStorage::new(dir.path()).unwrap();
        let m = InMemoryMongo::new();
        let mut d = Document::new();
        d.insert("_id", Bson::Int64(1));
        d.insert(
            "blob",
            Bson::Binary(Binary {
                subtype: BinarySubtype::Generic,
                bytes: vec![1, 2, 3, 4, 5],
            }),
        );
        m.seed(CollectionData {
            database: "app".into(),
            collection: "files".into(),
            documents: vec![d.clone()],
            ..Default::default()
        });
        let (r, e) = (Registry::with_builtins(), HashEngine::new("s"));
        let opts = DumpOptions {
            tmp_dir: Some(std::env::temp_dir().display().to_string()),
            gzip: true,
            ..Default::default()
        };
        let meta = dump(&s, &m, &r, &e, opts)
            .run(at("2026-07-18T12:00:00Z"))
            .unwrap();

        let full = read_collection_full(&s, &meta.id, "app", "files").unwrap();
        assert_eq!(full.documents[0], d);
    }

    // Acceptance: dump fails fast if tmp_dir is not configured.
    #[test]
    fn missing_tmp_dir_fails_fast() {
        let dir = tempfile::tempdir().unwrap();
        let s = DirectoryStorage::new(dir.path()).unwrap();
        let m = source_with_users();
        let (r, e) = (Registry::with_builtins(), HashEngine::new("s"));
        let opts = DumpOptions {
            tmp_dir: None,
            ..Default::default()
        };
        let err = dump(&s, &m, &r, &e, opts)
            .run(at("2026-07-18T12:00:00Z"))
            .unwrap_err();
        assert!(err.to_string().contains("tmp_dir"), "{err}");
    }

    // The dump must go through the streaming source APIs (structure + document
    // stream), never the materialize-everything read_collection: a source that
    // panics on read_collection still dumps fine.
    #[test]
    fn dump_streams_documents_instead_of_materializing() {
        use crate::mongo::CollectionStructure;

        struct StreamOnly;
        impl MongoSource for StreamOnly {
            fn databases(&self) -> crate::error::Result<Vec<String>> {
                Ok(vec!["app".into()])
            }
            fn collections(&self, _db: &str) -> crate::error::Result<Vec<String>> {
                Ok(vec!["users".into()])
            }
            fn read_collection(
                &self,
                _db: &str,
                _coll: &str,
            ) -> crate::error::Result<CollectionData> {
                panic!("dump must not materialize whole collections");
            }
            fn read_structure(
                &self,
                _db: &str,
                _coll: &str,
            ) -> crate::error::Result<CollectionStructure> {
                Ok(CollectionStructure::default())
            }
            fn stream_documents(
                &self,
                _db: &str,
                _coll: &str,
                _filter: Option<&Document>,
                f: &mut dyn FnMut(Document) -> crate::error::Result<()>,
            ) -> crate::error::Result<()> {
                for i in 0..10i64 {
                    let mut d = Document::new();
                    d.insert("_id", i);
                    f(d)?;
                }
                Ok(())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let s = DirectoryStorage::new(dir.path()).unwrap();
        let (r, e) = (Registry::with_builtins(), HashEngine::new("s"));
        let tmp = tempfile::tempdir().unwrap();
        let d = Dump {
            storage: &s,
            source: &StreamOnly,
            registry: &r,
            engine: &e,
            plan: None,
            filters: BTreeMap::new(),
            options: DumpOptions {
                tmp_dir: Some(tmp.path().display().to_string()),
                ..Default::default()
            },
        };
        let meta = d.run(at("2026-07-18T12:00:00Z")).unwrap();
        assert_eq!(meta.databases[0].collections[0].document_count, 10);
        let docs = crate::dump::read_collection_data(&s, &meta.id, "app", "users").unwrap();
        assert_eq!(docs.len(), 10);
        // The spool file is cleaned up from tmp_dir after upload.
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    // Transformations are applied inline while dumping.
    #[test]
    fn transformations_applied_during_dump() {
        let dir = tempfile::tempdir().unwrap();
        let s = DirectoryStorage::new(dir.path()).unwrap();
        let m = source_with_users();
        let r = Registry::with_builtins();
        let e = HashEngine::new("s");
        let configs: Vec<crate::transform::apply::TransformationConfig> = serde_yaml::from_str(
            "- collection: users\n  transformers:\n    - field: email\n      name: masking\n",
        )
        .unwrap();
        let plan = TransformationPlan::compile(&configs, &r, &e).unwrap();
        let d = Dump {
            storage: &s,
            source: &m,
            registry: &r,
            engine: &e,
            plan: Some(&plan),
            filters: BTreeMap::new(),
            options: DumpOptions {
                tmp_dir: Some(std::env::temp_dir().display().to_string()),
                ..Default::default()
            },
        };
        let meta = d.run(at("2026-07-18T12:00:00Z")).unwrap();
        let full = read_collection_full(&s, &meta.id, "app", "users").unwrap();
        for doc in &full.documents {
            assert!(doc.get_str("email").unwrap().chars().all(|c| c == '*'));
        }
    }

    fn source_with_n_collections(n: usize) -> InMemoryMongo {
        let m = InMemoryMongo::new();
        for i in 0..n {
            let documents = (0..=i as i64)
                .map(|id| {
                    let mut d = Document::new();
                    d.insert("_id", id);
                    d
                })
                .collect();
            m.seed(CollectionData {
                database: "app".into(),
                collection: format!("c{i}"),
                documents,
                ..Default::default()
            });
        }
        m
    }

    // Acceptance: `--jobs` (parallel_jobs > 1) dumps every collection and
    // produces byte-for-byte the same metadata as a sequential dump — same
    // document counts, same restore_order (source enumeration order, not
    // completion order), same total size.
    #[test]
    fn jobs_parallel_dump_matches_sequential() {
        let (r, e) = (Registry::with_builtins(), HashEngine::new("s"));

        let sequential_dir = tempfile::tempdir().unwrap();
        let s1 = DirectoryStorage::new(sequential_dir.path()).unwrap();
        let m1 = source_with_n_collections(5);
        let opts1 = DumpOptions {
            tmp_dir: Some(std::env::temp_dir().display().to_string()),
            parallel_jobs: 1,
            ..Default::default()
        };
        let meta1 = dump(&s1, &m1, &r, &e, opts1)
            .run(at("2026-07-18T12:00:00Z"))
            .unwrap();

        let parallel_dir = tempfile::tempdir().unwrap();
        let s2 = DirectoryStorage::new(parallel_dir.path()).unwrap();
        let m2 = source_with_n_collections(5);
        // More jobs than collections: exercises the "fewer items than
        // threads" path too.
        let opts2 = DumpOptions {
            tmp_dir: Some(std::env::temp_dir().display().to_string()),
            parallel_jobs: 8,
            ..Default::default()
        };
        let meta2 = dump(&s2, &m2, &r, &e, opts2)
            .run(at("2026-07-18T12:00:00Z"))
            .unwrap();

        assert_eq!(meta1, meta2);
        assert_eq!(meta1.databases[0].collections.len(), 5);
        for (i, coll) in meta1.databases[0].collections.iter().enumerate() {
            assert_eq!(coll.name, format!("c{i}"));
            assert_eq!(coll.restore_order, i as u32);
            assert_eq!(coll.document_count, i as u64 + 1);
        }
    }

    // A source that always fails on one specific collection, to exercise the
    // parallel dump's error path.
    struct FailingSource;

    impl MongoSource for FailingSource {
        fn databases(&self) -> crate::error::Result<Vec<String>> {
            Ok(vec!["app".into()])
        }
        fn collections(&self, _db: &str) -> crate::error::Result<Vec<String>> {
            Ok((0..5).map(|i| format!("c{i}")).collect())
        }
        fn read_collection(&self, _db: &str, _coll: &str) -> crate::error::Result<CollectionData> {
            panic!("dump must not materialize whole collections");
        }
        fn read_structure(
            &self,
            _db: &str,
            _coll: &str,
        ) -> crate::error::Result<CollectionStructure> {
            Ok(CollectionStructure::default())
        }
        fn stream_documents(
            &self,
            _db: &str,
            coll: &str,
            _filter: Option<&Document>,
            f: &mut dyn FnMut(Document) -> crate::error::Result<()>,
        ) -> crate::error::Result<()> {
            if coll == "c3" {
                return Err(Error::Mongo("boom".into()));
            }
            let mut d = Document::new();
            d.insert("_id", 1i64);
            f(d)
        }
    }

    // Acceptance: a failure in one collection during a parallel dump still
    // surfaces as an error from `run`, and every spool file — including ones
    // from collections that finished successfully alongside the failure — is
    // cleaned up rather than left behind in tmp_dir.
    #[test]
    fn jobs_parallel_dump_propagates_error_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let s = DirectoryStorage::new(dir.path()).unwrap();
        let (r, e) = (Registry::with_builtins(), HashEngine::new("s"));
        let tmp = tempfile::tempdir().unwrap();
        let d = Dump {
            storage: &s,
            source: &FailingSource,
            registry: &r,
            engine: &e,
            plan: None,
            filters: BTreeMap::new(),
            options: DumpOptions {
                tmp_dir: Some(tmp.path().display().to_string()),
                parallel_jobs: 4,
                ..Default::default()
            },
        };
        let err = d.run(at("2026-07-18T12:00:00Z")).unwrap_err();
        assert!(err.to_string().contains("boom"), "{err}");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }
}
