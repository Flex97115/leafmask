//! Create a logical database dump (feature `dump.create`).
//!
//! Reads collections from a [`MongoSource`], applies configured transformations
//! and per-collection query filters inline while streaming, captures indexes and
//! collection options, and writes a timestamp-identified dump (optionally
//! gzip-compressed) to storage. Binary/large field values are preserved as BSON
//! binary inline with their owning document — no separate GridFS pass.
//!
//! A dump requires `common.tmp_dir`; without it the command fails fast.

use chrono::{DateTime, Utc};

use crate::error::{Error, Result};
use crate::hash::HashEngine;
use crate::mongo::{CollectionData, MongoSource};
use crate::storage::Storage;
use crate::transform::{apply::TransformationPlan, Registry};

use super::{
    write_collection_data, write_metadata, CollectionToc, DatabaseToc, DumpMetadata, DumpStatus,
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
    /// Number of parallel jobs (accepted; execution is sequential here).
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
    pub options: DumpOptions,
}

impl<'a> Dump<'a> {
    /// Run the dump, stamping it with an id derived from `created_at`.
    pub fn run(&self, created_at: DateTime<Utc>) -> Result<DumpMetadata> {
        if self.options.tmp_dir.as_deref().unwrap_or("").is_empty() {
            return Err(Error::Config(
                "common.tmp_dir must be configured before dumping".into(),
            ));
        }

        let id = created_at.format("%Y%m%dT%H%M%SZ").to_string();
        let mut databases = Vec::new();
        let mut total_size = 0u64;

        for db in self.source.databases() {
            if !self.options.include_db(&db) {
                continue;
            }
            let mut collections = Vec::new();
            let mut order = 0u32;
            for coll in self.source.collections(&db) {
                if !self.options.include_collection(&coll) {
                    continue;
                }
                let source_data = self.source.read_collection(&db, &coll)?;
                let data = self.build_collection(&coll, source_data)?;

                let toc = CollectionToc {
                    name: coll.clone(),
                    document_count: data.documents.len() as u64,
                    indexes: data.indexes.iter().map(|i| i.name.clone()).collect(),
                    restore_order: order,
                };
                order += 1;

                total_size += write_collection_data(self.storage, &id, &data, self.options.gzip)?;
                collections.push(toc);
            }
            databases.push(DatabaseToc {
                name: db,
                collections,
            });
        }

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

    /// Apply query filtering and transformations to one collection's documents,
    /// and capture (or drop) its indexes/options.
    fn build_collection(&self, coll: &str, source: CollectionData) -> Result<CollectionData> {
        let mut documents = Vec::with_capacity(source.documents.len());
        for doc in &source.documents {
            if let Some(plan) = self.plan {
                if !plan.should_include(coll, doc) {
                    continue; // custom query filter excludes this document.
                }
                documents.push(plan.transform(self.registry, self.engine, coll, doc)?);
            } else {
                documents.push(doc.clone());
            }
        }

        let (indexes, validator, options) = if self.options.no_indexes {
            (Vec::new(), None, Default::default())
        } else {
            (source.indexes, source.validator, source.options)
        };

        Ok(CollectionData {
            database: source.database,
            collection: source.collection,
            documents,
            indexes,
            validator,
            options,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dump::{read_collection_full, read_metadata};
    use crate::mongo::InMemoryMongo;
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
        let opts = DumpOptions { tmp_dir: Some("/tmp".into()), ..Default::default() };

        let meta = dump(&s, &m, &r, &e, opts).run(at("2026-07-18T12:00:00Z")).unwrap();
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
            tmp_dir: Some("/tmp".into()),
            exclude_collections: vec!["secrets".into()],
            ..Default::default()
        };
        let meta = dump(&s, &m, &r, &e, opts).run(at("2026-07-18T12:00:00Z")).unwrap();
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
            Bson::Binary(Binary { subtype: BinarySubtype::Generic, bytes: vec![1, 2, 3, 4, 5] }),
        );
        m.seed(CollectionData {
            database: "app".into(),
            collection: "files".into(),
            documents: vec![d.clone()],
            ..Default::default()
        });
        let (r, e) = (Registry::with_builtins(), HashEngine::new("s"));
        let opts = DumpOptions { tmp_dir: Some("/tmp".into()), gzip: true, ..Default::default() };
        let meta = dump(&s, &m, &r, &e, opts).run(at("2026-07-18T12:00:00Z")).unwrap();

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
        let opts = DumpOptions { tmp_dir: None, ..Default::default() };
        let err = dump(&s, &m, &r, &e, opts).run(at("2026-07-18T12:00:00Z")).unwrap_err();
        assert!(err.to_string().contains("tmp_dir"), "{err}");
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
            options: DumpOptions { tmp_dir: Some("/tmp".into()), ..Default::default() },
        };
        let meta = d.run(at("2026-07-18T12:00:00Z")).unwrap();
        let full = read_collection_full(&s, &meta.id, "app", "users").unwrap();
        for doc in &full.documents {
            assert!(doc.get_str("email").unwrap().chars().all(|c| c == '*'));
        }
    }
}
