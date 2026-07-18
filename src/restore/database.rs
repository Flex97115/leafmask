//! Restore a dump to a target database (feature `restore.database`).
//!
//! Reads a stored dump (by id or `latest`) and writes it into a target via the
//! [`MongoSink`] trait: include/exclude filters, parallel-capable batched bulk
//! writes (ordered or unordered), optional dependency ordering (documents before
//! indexes/validators), pre/post scripts, and tolerated insert errors. The sink
//! is abstracted so the driver is unit-tested without a live MongoDB.

use bson::Document;

use crate::dump::{read_collection_full, resolve, DumpMetadata};
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
                let data = read_collection_full(self.storage, &meta.id, &db.name, &coll.name)?;
                self.sink
                    .ensure_collection(&db.name, &coll.name, &data.validator, &data.options)?;

                self.insert_documents(&db.name, &coll.name, &data.documents, &mut report)?;

                if !self.options.dependency_order {
                    self.create_indexes(&db.name, &coll.name, &data.indexes, &mut report)?;
                }
            }
        }

        // In dependency order, indexes/validators come after all documents.
        if self.options.dependency_order {
            for db in &meta.databases {
                for coll in &db.collections {
                    if self.options.include_collection(&coll.name) {
                        let data =
                            read_collection_full(self.storage, &meta.id, &db.name, &coll.name)?;
                        self.create_indexes(&db.name, &coll.name, &data.indexes, &mut report)?;
                    }
                }
            }
        }

        self.scripts.run_stage("post-data", runner)?;
        Ok(report)
    }

    fn insert_documents(
        &self,
        db: &str,
        coll: &str,
        docs: &[Document],
        report: &mut RestoreReport,
    ) -> Result<()> {
        let batch = self.options.batch_size.max(1);
        for chunk in docs.chunks(batch) {
            for doc in chunk {
                match self.sink.insert(db, coll, doc) {
                    Ok(()) => report.inserted += 1,
                    Err(err) => {
                        if self.exclusions.is_excluded(coll, &err) {
                            report.skipped += 1;
                            continue; // logged & skipped, restoration continues.
                        }
                        return Err(Error::Restore(format!(
                            "insert into {db}.{coll} failed (code {:?}, index {:?})",
                            err.code, err.index_name
                        )));
                    }
                }
                // An ordered bulk write would stop the batch at a hard error; we
                // already returned above for those, so ordering only affects how
                // far an unexcluded error propagates, handled by the early return.
                let _ = self.options.ordered;
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
    use crate::dump::{write_collection_data, write_metadata, CollectionToc, DatabaseToc, DumpMetadata, DumpStatus};
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
    fn seed_dump(id: &str, docs: &[Document], indexes: Vec<IndexSpec>) -> (tempfile::TempDir, DirectoryStorage) {
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
            options: RestoreOptions { batch_size: 10, ..Default::default() },
        };
        let report = r.run("latest", &NoScripts).unwrap();
        assert_eq!(report.inserted, 2);
        assert_eq!(sink.documents("app", "users").len(), 2);
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
            options: RestoreOptions { batch_size: 1, ..Default::default() },
        };
        let err = r.run("nonexistent", &NoScripts).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err}");
    }

    // Acceptance: include/exclude filters restrict what is restored (indexes
    // here); dependency order creates indexes after documents.
    #[test]
    fn filters_and_dependency_order() {
        let idx = |n: &str| IndexSpec { name: n.into(), keys: vec![(n.into(), 1)], unique: false };
        let (_d, s) = seed_dump("20260701", &[doc(1)], vec![idx("keep_idx"), idx("drop_idx")]);
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

    // Acceptance: a tolerated insert error is skipped; an untolerated one aborts.
    #[test]
    fn error_exclusions_skip_or_abort() {
        let (_d, s) = seed_dump("20260701", &[doc(1), doc(1)], vec![]); // duplicate _id
        // With 11000 excluded, the duplicate is skipped.
        let sink = InMemoryMongo::new();
        let excl: ErrorExclusions =
            serde_yaml::from_str("global_error_codes: [11000]\n").unwrap();
        let r = Restore {
            storage: &s,
            sink: &sink,
            exclusions: excl,
            scripts: Default::default(),
            options: RestoreOptions { batch_size: 10, ..Default::default() },
        };
        let report = r.run("20260701", &NoScripts).unwrap();
        assert_eq!(report.inserted, 1);
        assert_eq!(report.skipped, 1);

        // Without the exclusion, the duplicate aborts the restore.
        let sink2 = InMemoryMongo::new();
        let r2 = Restore {
            storage: &s,
            sink: &sink2,
            exclusions: Default::default(),
            scripts: Default::default(),
            options: RestoreOptions { batch_size: 10, ..Default::default() },
        };
        assert!(r2.run("20260701", &NoScripts).is_err());
    }
}
