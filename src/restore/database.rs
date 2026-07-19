//! Restore a dump to a target database (feature `restore.database`).
//!
//! Reads a stored dump (by id or `latest`) and writes it into a target via the
//! [`MongoSink`] trait: include/exclude filters, batched bulk writes (ordered
//! or unordered), optional dependency ordering (documents before
//! indexes/validators), pre/post scripts, and tolerated insert errors.
//! Documents are streamed out of storage and inserted `batch_size` at a time,
//! so restoring a collection never holds more than one batch in memory. The
//! sink is abstracted so the driver is unit-tested without a live MongoDB.

use bson::Document;

use crate::dump::{open_document_reader, read_collection_meta, resolve, DumpMetadata};
use crate::error::{Error, Result};
use crate::mongo::MongoSink;
use crate::storage::Storage;
use crate::validate::IndexSpec;

use super::error_exclusions::ErrorExclusions;
use super::scripts::{ScriptRunner, Scripts};

/// Filters and options controlling a restore.
#[derive(Debug, Clone, Default)]
pub struct RestoreOptions {
    pub include_collections: Vec<String>,
    pub exclude_collections: Vec<String>,
    pub include_indexes: Vec<String>,
    pub exclude_indexes: Vec<String>,
    /// Bulk write batch size (documents per insert batch).
    pub batch_size: usize,
    /// Ordered bulk writes stop at the first non-excluded error in a batch.
    pub ordered: bool,
    /// Create indexes/validators after a collection's documents.
    pub dependency_order: bool,
    /// Abort the whole restore (not just the collection) on an error.
    pub exit_on_error: bool,
    /// Drop each restored collection (data and indexes) before recreating it
    /// from the dump, so re-running a restore against the same target is
    /// idempotent instead of hitting `_id` duplicate-key errors.
    pub clean: bool,
}

impl RestoreOptions {
    fn include_collection(&self, name: &str) -> bool {
        if self.exclude_collections.iter().any(|c| c == name) {
            return false;
        }
        self.include_collections.is_empty() || self.include_collections.iter().any(|c| c == name)
    }
    fn include_index(&self, name: &str) -> bool {
        if self.exclude_indexes.iter().any(|i| i == name) {
            return false;
        }
        self.include_indexes.is_empty() || self.include_indexes.iter().any(|i| i == name)
    }
}

/// The restore driver.
pub struct Restore<'a> {
    pub storage: &'a dyn Storage,
    pub sink: &'a dyn MongoSink,
    pub exclusions: ErrorExclusions,
    pub scripts: Scripts,
    pub options: RestoreOptions,
}

/// A summary of what a restore did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreReport {
    pub inserted: u64,
    pub skipped: u64,
    pub indexes_created: u64,
    /// Collections (`db.coll`) aborted by a non-excluded insert error. Empty
    /// on a fully successful restore; the CLI exits non-zero otherwise.
    pub failed: Vec<String>,
}

impl<'a> Restore<'a> {
    /// Run the restore of the dump identified by `id_or_latest`.
    pub fn run(&self, id_or_latest: &str, runner: &dyn ScriptRunner) -> Result<RestoreReport> {
        let meta: DumpMetadata = resolve(self.storage, id_or_latest)?;
        let mut report = RestoreReport::default();

        self.scripts.run_stage("pre-data", runner)?;

        for db in &meta.databases {
            for coll in &db.collections {
                if !self.options.include_collection(&coll.name) {
                    continue;
                }
                let cmeta = read_collection_meta(self.storage, &meta.id, &db.name, &coll.name)?;
                log::info!(
                    "  {}.{}: restoring {} documents",
                    db.name,
                    coll.name,
                    cmeta.document_count
                );
                if self.options.clean {
                    self.sink.drop_collection(&db.name, &coll.name)?;
                }
                self.sink.ensure_collection(
                    &db.name,
                    &coll.name,
                    &cmeta.validator,
                    &cmeta.options,
                )?;

                match self.insert_documents(&meta.id, &db.name, &coll.name, &mut report) {
                    Ok(()) => {}
                    Err(e) if self.options.exit_on_error => return Err(e),
                    Err(e) => {
                        // The collection is reported failed; the restore moves
                        // on to the next collection (mongorestore semantics).
                        log::error!("  {}.{}: {e}", db.name, coll.name);
                        report.failed.push(format!("{}.{}", db.name, coll.name));
                        continue;
                    }
                }

                if !self.options.dependency_order {
                    self.create_indexes(&db.name, &coll.name, &cmeta.indexes, &mut report)?;
                }
            }
        }

        // In dependency order, indexes/validators come after all documents.
        // Only the (small) structure blob is re-read, never the documents.
        if self.options.dependency_order {
            for db in &meta.databases {
                for coll in &db.collections {
                    if self.options.include_collection(&coll.name) {
                        let cmeta =
                            read_collection_meta(self.storage, &meta.id, &db.name, &coll.name)?;
                        self.create_indexes(&db.name, &coll.name, &cmeta.indexes, &mut report)?;
                    }
                }
            }
        }

        self.scripts.run_stage("post-data", runner)?;
        Ok(report)
    }

    /// Stream a collection's documents out of the dump and insert them
    /// `batch_size` at a time through [`MongoSink::insert_many`].
    fn insert_documents(
        &self,
        dump_id: &str,
        db: &str,
        coll: &str,
        report: &mut RestoreReport,
    ) -> Result<()> {
        let batch_size = self.options.batch_size.max(1);
        let mut reader = open_document_reader(self.storage, dump_id, db, coll)?;
        loop {
            let batch = reader.next_batch(batch_size)?;
            if batch.is_empty() {
                return Ok(());
            }
            self.insert_batch(db, coll, &batch, report)?;
            if report.inserted % 100_000 < batch_size as u64 && report.inserted >= 100_000 {
                log::info!("  {db}.{coll}: {} documents...", report.inserted);
            }
        }
    }

    /// Insert one batch, honouring error exclusions. With ordered writes an
    /// excluded failure stops the server-side batch, so the remainder is
    /// re-submitted starting right after the failed document.
    fn insert_batch(
        &self,
        db: &str,
        coll: &str,
        batch: &[Document],
        report: &mut RestoreReport,
    ) -> Result<()> {
        let mut start = 0;
        while start < batch.len() {
            let out = self
                .sink
                .insert_many(db, coll, &batch[start..], self.options.ordered)?;
            report.inserted += out.inserted;
            if out.failures.is_empty() {
                return Ok(());
            }
            for (_, err) in &out.failures {
                if self.exclusions.is_excluded(coll, err) {
                    report.skipped += 1; // logged & skipped, restoration continues.
                } else {
                    return Err(Error::Restore(format!(
                        "insert into {db}.{coll} failed (code {:?}, index {:?})",
                        err.code, err.index_name
                    )));
                }
            }
            if self.options.ordered {
                start += out.failures[0].0 + 1;
            } else {
                return Ok(()); // unordered attempted every document.
            }
        }
        Ok(())
    }

    fn create_indexes(
        &self,
        db: &str,
        coll: &str,
        indexes: &[IndexSpec],
        report: &mut RestoreReport,
    ) -> Result<()> {
        for idx in indexes {
            // The default `_id_` index is created automatically with the
            // collection and cannot be (re)declared explicitly.
            if idx.name == "_id_" {
                continue;
            }
            if self.options.include_index(&idx.name) {
                self.sink.create_index(db, coll, idx)?;
                report.indexes_created += 1;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dump::{
        write_collection_data, write_metadata, CollectionToc, DatabaseToc, DumpMetadata, DumpStatus,
    };
    use crate::mongo::{CollectionData, InMemoryMongo, MongoSource};
    use crate::restore::scripts::ScriptRunner;
    use crate::storage::DirectoryStorage;
    use bson::{Bson, Document};

    struct NoScripts;
    impl ScriptRunner for NoScripts {
        fn run_mongosh(&self, _: &str) -> Result<()> {
            Ok(())
        }
        fn run_mongosh_file(&self, _: &str) -> Result<()> {
            Ok(())
        }
        fn run_command(&self, _: &[String]) -> Result<()> {
            Ok(())
        }
    }

    fn doc(id: i64) -> Document {
        let mut d = Document::new();
        d.insert("_id", Bson::Int64(id));
        d.insert("v", Bson::String(format!("row{id}")));
        d
    }

    /// Build a dump in directory storage and return (tempdir, storage).
    fn seed_dump(
        id: &str,
        docs: &[Document],
        indexes: Vec<IndexSpec>,
    ) -> (tempfile::TempDir, DirectoryStorage) {
        let dir = tempfile::tempdir().unwrap();
        let s = DirectoryStorage::new(dir.path()).unwrap();
        let data = CollectionData {
            database: "app".into(),
            collection: "users".into(),
            documents: docs.to_vec(),
            indexes: indexes.clone(),
            validator: None,
            options: Default::default(),
        };
        write_collection_data(&s, id, &data, false).unwrap();
        write_metadata(
            &s,
            &DumpMetadata {
                id: id.into(),
                status: DumpStatus::Done,
                created_at: "2026-07-01T00:00:00Z".into(),
                databases: vec![DatabaseToc {
                    name: "app".into(),
                    collections: vec![CollectionToc {
                        name: "users".into(),
                        document_count: docs.len() as u64,
                        indexes: indexes.iter().map(|i| i.name.clone()).collect(),
                        restore_order: 0,
                    }],
                }],
                size: 0,
            },
        )
        .unwrap();
        (dir, s)
    }

    // Acceptance: 'latest' selects the most recent completed dump and restores
    // its documents into the target.
    #[test]
    fn restores_latest_into_target() {
        let (_d, s) = seed_dump("20260701", &[doc(1), doc(2)], vec![]);
        let sink = InMemoryMongo::new();
        let r = Restore {
            storage: &s,
            sink: &sink,
            exclusions: Default::default(),
            scripts: Default::default(),
            options: RestoreOptions {
                batch_size: 10,
                ..Default::default()
            },
        };
        let report = r.run("latest", &NoScripts).unwrap();
        assert_eq!(report.inserted, 2);
        assert_eq!(sink.documents("app", "users").len(), 2);
    }

    // Acceptance: re-running a restore into a target that already holds data
    // from a previous run hits `_id` duplicate-key errors and fails the
    // collection when `clean` is not set.
    #[test]
    fn without_clean_preexisting_data_causes_duplicate_key_failure() {
        let (_d, s) = seed_dump("20260701", &[doc(1), doc(2)], vec![]);
        let sink = InMemoryMongo::new();
        sink.seed(CollectionData {
            database: "app".into(),
            collection: "users".into(),
            documents: vec![doc(1)],
            ..Default::default()
        });
        let r = Restore {
            storage: &s,
            sink: &sink,
            exclusions: Default::default(),
            scripts: Default::default(),
            options: RestoreOptions {
                batch_size: 10,
                ..Default::default()
            },
        };
        let report = r.run("latest", &NoScripts).unwrap();
        assert_eq!(report.failed, vec!["app.users".to_string()]);
    }

    // `clean: true` drops the collection (including stale data from a
    // previous run) before restoring, so the same restore is idempotent.
    #[test]
    fn clean_drops_collection_before_restore() {
        let (_d, s) = seed_dump("20260701", &[doc(1), doc(2)], vec![]);
        let sink = InMemoryMongo::new();
        sink.seed(CollectionData {
            database: "app".into(),
            collection: "users".into(),
            documents: vec![doc(1), doc(99)],
            ..Default::default()
        });
        let r = Restore {
            storage: &s,
            sink: &sink,
            exclusions: Default::default(),
            scripts: Default::default(),
            options: RestoreOptions {
                batch_size: 10,
                clean: true,
                ..Default::default()
            },
        };
        let report = r.run("latest", &NoScripts).unwrap();
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(report.inserted, 2);
        let mut ids: Vec<i64> = sink
            .documents("app", "users")
            .iter()
            .map(|d| d.get_i64("_id").unwrap())
            .collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2]);
    }

    // Acceptance: an explicit missing dump id fails with a clear error.
    #[test]
    fn missing_dump_is_clear_error() {
        let (_d, s) = seed_dump("20260701", &[doc(1)], vec![]);
        let sink = InMemoryMongo::new();
        let r = Restore {
            storage: &s,
            sink: &sink,
            exclusions: Default::default(),
            scripts: Default::default(),
            options: RestoreOptions {
                batch_size: 1,
                ..Default::default()
            },
        };
        let err = r.run("nonexistent", &NoScripts).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err}");
    }

    // Acceptance: include/exclude filters restrict what is restored (indexes
    // here); dependency order creates indexes after documents.
    #[test]
    fn filters_and_dependency_order() {
        let idx = |n: &str| IndexSpec {
            name: n.into(),
            keys: vec![(n.into(), 1)],
            unique: false,
        };
        let (_d, s) = seed_dump(
            "20260701",
            &[doc(1)],
            vec![idx("keep_idx"), idx("drop_idx")],
        );
        let sink = InMemoryMongo::new();
        let r = Restore {
            storage: &s,
            sink: &sink,
            exclusions: Default::default(),
            scripts: Default::default(),
            options: RestoreOptions {
                batch_size: 5,
                dependency_order: true,
                exclude_indexes: vec!["drop_idx".into()],
                ..Default::default()
            },
        };
        let report = r.run("20260701", &NoScripts).unwrap();
        assert_eq!(report.indexes_created, 1);
        let created: Vec<String> = sink
            .read_collection("app", "users")
            .unwrap()
            .indexes
            .iter()
            .map(|i| i.name.clone())
            .collect();
        assert_eq!(created, vec!["keep_idx".to_string()]);
    }

    // Acceptance: a tolerated insert error is skipped; an untolerated one fails
    // the collection (surfaced in the report) without silently succeeding.
    #[test]
    fn error_exclusions_skip_or_fail_collection() {
        let (_d, s) = seed_dump("20260701", &[doc(1), doc(1)], vec![]); // duplicate _id
                                                                        // With 11000 excluded, the duplicate is skipped.
        let sink = InMemoryMongo::new();
        let excl: ErrorExclusions = serde_yaml::from_str("global_error_codes: [11000]\n").unwrap();
        let r = Restore {
            storage: &s,
            sink: &sink,
            exclusions: excl,
            scripts: Default::default(),
            options: RestoreOptions {
                batch_size: 10,
                ..Default::default()
            },
        };
        let report = r.run("20260701", &NoScripts).unwrap();
        assert_eq!(report.inserted, 1);
        assert_eq!(report.skipped, 1);
        assert!(report.failed.is_empty());

        // Without the exclusion, the collection is reported failed.
        let sink2 = InMemoryMongo::new();
        let r2 = Restore {
            storage: &s,
            sink: &sink2,
            exclusions: Default::default(),
            scripts: Default::default(),
            options: RestoreOptions {
                batch_size: 10,
                ..Default::default()
            },
        };
        let report = r2.run("20260701", &NoScripts).unwrap();
        assert_eq!(report.failed, vec!["app.users".to_string()]);

        // With --exit-on-error, the same failure aborts the whole restore.
        let sink3 = InMemoryMongo::new();
        let r3 = Restore {
            storage: &s,
            sink: &sink3,
            exclusions: Default::default(),
            scripts: Default::default(),
            options: RestoreOptions {
                batch_size: 10,
                exit_on_error: true,
                ..Default::default()
            },
        };
        assert!(r3.run("20260701", &NoScripts).is_err());
    }

    /// A sink that records the size of every insert_many batch it receives.
    struct BatchSpy {
        inner: InMemoryMongo,
        batches: std::sync::Mutex<Vec<usize>>,
    }
    impl MongoSink for BatchSpy {
        fn ensure_collection(
            &self,
            database: &str,
            collection: &str,
            validator: &Option<Bson>,
            options: &std::collections::BTreeMap<String, Bson>,
        ) -> Result<()> {
            self.inner
                .ensure_collection(database, collection, validator, options)
        }
        fn insert(
            &self,
            database: &str,
            collection: &str,
            doc: &Document,
        ) -> std::result::Result<(), crate::restore::InsertError> {
            self.inner.insert(database, collection, doc)
        }
        fn insert_many(
            &self,
            database: &str,
            collection: &str,
            docs: &[Document],
            ordered: bool,
        ) -> Result<crate::mongo::BatchInsert> {
            self.batches.lock().unwrap().push(docs.len());
            self.inner.insert_many(database, collection, docs, ordered)
        }
        fn create_index(&self, database: &str, collection: &str, index: &IndexSpec) -> Result<()> {
            self.inner.create_index(database, collection, index)
        }
        fn drop_collection(&self, database: &str, collection: &str) -> Result<()> {
            self.inner.drop_collection(database, collection)
        }
    }

    // The restore hot path must go through batched insert_many calls sized by
    // --batch-size, not one server round-trip per document.
    #[test]
    fn inserts_in_batches_of_batch_size() {
        let docs: Vec<Document> = (1..=25).map(doc).collect();
        let (_d, s) = seed_dump("20260701", &docs, vec![]);
        let sink = BatchSpy {
            inner: InMemoryMongo::new(),
            batches: Default::default(),
        };
        let r = Restore {
            storage: &s,
            sink: &sink,
            exclusions: Default::default(),
            scripts: Default::default(),
            options: RestoreOptions {
                batch_size: 10,
                ..Default::default()
            },
        };
        let report = r.run("20260701", &NoScripts).unwrap();
        assert_eq!(report.inserted, 25);
        assert_eq!(*sink.batches.lock().unwrap(), vec![10, 10, 5]);
    }

    // Ordered writes stop a batch at its first failure; an excluded failure
    // must resume the batch right after the failed document, not lose the rest.
    #[test]
    fn ordered_batch_resumes_after_excluded_failure() {
        let (_d, s) = seed_dump("20260701", &[doc(1), doc(1), doc(2)], vec![]);
        let sink = InMemoryMongo::new();
        let excl: ErrorExclusions = serde_yaml::from_str("global_error_codes: [11000]\n").unwrap();
        let r = Restore {
            storage: &s,
            sink: &sink,
            exclusions: excl,
            scripts: Default::default(),
            options: RestoreOptions {
                batch_size: 10,
                ordered: true,
                ..Default::default()
            },
        };
        let report = r.run("20260701", &NoScripts).unwrap();
        assert_eq!(report.inserted, 2);
        assert_eq!(report.skipped, 1);
        assert_eq!(sink.documents("app", "users").len(), 2);
    }
}
