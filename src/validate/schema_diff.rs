//! Schema drift detection (feature `validate.schema-diff`).
//!
//! `validate --schema` compares the collection structure recorded in storage
//! against the live database's indexes, validators, and options, and reports
//! any drift so it does not silently break a later dump. Fetching the live
//! structure needs a MongoDB connection (behind the `mongo` feature); the diff
//! itself is pure and fully tested here.

use std::collections::BTreeMap;

use bson::Bson;
use serde::{Deserialize, Serialize};

/// An index as captured for drift comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexSpec {
    pub name: String,
    /// Ordered `(field, direction)` key spec.
    pub keys: Vec<(String, i32)>,
    pub unique: bool,
}

/// The structure of one collection, either recorded in storage or read live.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CollectionSchema {
    pub collection: String,
    pub indexes: Vec<IndexSpec>,
    /// The JSON-Schema validator document, if any.
    pub validator: Option<Bson>,
    /// Other collection options (capped, size, …).
    pub options: BTreeMap<String, Bson>,
}

impl CollectionSchema {
    pub fn new(collection: &str) -> Self {
        CollectionSchema {
            collection: collection.to_string(),
            ..Default::default()
        }
    }

    /// Build from the pieces both sides of the comparison already carry: a
    /// dump's recorded collection metadata, and a live collection's structure.
    /// Kept as loose parts so this module needs to know about neither the dump
    /// format nor the MongoDB traits.
    pub fn from_parts(
        collection: &str,
        indexes: Vec<IndexSpec>,
        validator: Option<Bson>,
        options: BTreeMap<String, Bson>,
    ) -> Self {
        CollectionSchema {
            collection: collection.to_string(),
            indexes,
            validator,
            options,
        }
    }

    fn index(&self, name: &str) -> Option<&IndexSpec> {
        self.indexes.iter().find(|i| i.name == name)
    }
}

/// A single detected difference between recorded and live schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaDiff {
    /// An index exists live but is not in the recorded structure.
    IndexAdded(String),
    /// A recorded index is missing live (dropped or renamed).
    IndexRemoved(String),
    /// An index with the same name has different keys/uniqueness.
    IndexChanged(String),
    /// The JSON-Schema validator differs.
    ValidatorChanged,
    /// A collection option changed.
    OptionChanged(String),
}

impl SchemaDiff {
    /// A human-readable line for reporting to the operator.
    pub fn describe(&self, collection: &str) -> String {
        match self {
            SchemaDiff::IndexAdded(n) => {
                format!("{collection}: index '{n}' added live, not recorded")
            }
            SchemaDiff::IndexRemoved(n) => {
                format!("{collection}: recorded index '{n}' is missing live")
            }
            SchemaDiff::IndexChanged(n) => format!("{collection}: index '{n}' definition changed"),
            SchemaDiff::ValidatorChanged => format!("{collection}: validator changed"),
            SchemaDiff::OptionChanged(k) => format!("{collection}: option '{k}' changed"),
        }
    }
}

/// Diff a recorded collection schema against the live one.
pub fn diff_collection(recorded: &CollectionSchema, live: &CollectionSchema) -> Vec<SchemaDiff> {
    let mut diffs = Vec::new();

    // Indexes present in recorded.
    for r in &recorded.indexes {
        match live.index(&r.name) {
            None => diffs.push(SchemaDiff::IndexRemoved(r.name.clone())),
            Some(l) if l != r => diffs.push(SchemaDiff::IndexChanged(r.name.clone())),
            Some(_) => {}
        }
    }
    // Indexes present only live.
    for l in &live.indexes {
        if recorded.index(&l.name).is_none() {
            diffs.push(SchemaDiff::IndexAdded(l.name.clone()));
        }
    }

    if recorded.validator != live.validator {
        diffs.push(SchemaDiff::ValidatorChanged);
    }

    let mut keys: Vec<&String> = recorded.options.keys().chain(live.options.keys()).collect();
    keys.sort();
    keys.dedup();
    for k in keys {
        if recorded.options.get(k) != live.options.get(k) {
            diffs.push(SchemaDiff::OptionChanged(k.clone()));
        }
    }

    diffs
}

/// Compare the structure a dump recorded against the live deployment's, and
/// return one readable drift line per difference. An empty result means no
/// drift. `database` and `collection`, when given, narrow the comparison.
///
/// A collection the dump recorded but that no longer exists live is diffed
/// against an empty structure, so its whole recorded structure is reported as
/// missing — that is drift, not an error. A structure read that fails for any
/// other reason propagates, so a connection problem is never silently rendered
/// as "everything was dropped".
pub fn diff_dump_against_live(
    storage: &dyn crate::storage::Storage,
    source: &dyn crate::mongo::MongoSource,
    dump_id: &str,
    database: Option<&str>,
    collection: Option<&str>,
) -> crate::error::Result<Vec<String>> {
    let meta = crate::dump::resolve(storage, dump_id)?;
    let mut report = Vec::new();

    for db in &meta.databases {
        if database.is_some_and(|wanted| wanted != db.name) {
            continue;
        }
        let live_collections = source.collections(&db.name)?;
        for coll in &db.collections {
            if collection.is_some_and(|wanted| wanted != coll.name) {
                continue;
            }
            let recorded_meta =
                crate::dump::read_collection_meta(storage, &meta.id, &db.name, &coll.name)?;
            let recorded = CollectionSchema::from_parts(
                &coll.name,
                recorded_meta.indexes,
                recorded_meta.validator,
                recorded_meta.options,
            );

            let live = if live_collections.contains(&coll.name) {
                let structure = source.read_structure(&db.name, &coll.name)?;
                CollectionSchema::from_parts(
                    &coll.name,
                    structure.indexes,
                    structure.validator,
                    structure.options,
                )
            } else {
                CollectionSchema::new(&coll.name)
            };

            for d in diff_collection(&recorded, &live) {
                report.push(format!("{}.{}", db.name, d.describe(&coll.name)));
            }
        }
    }
    Ok(report)
}

/// Diff a whole database's worth of recorded vs live schemas, returning the
/// drift lines. An empty result means no drift.
pub fn diff_database(recorded: &[CollectionSchema], live: &[CollectionSchema]) -> Vec<String> {
    let mut report = Vec::new();
    for r in recorded {
        let empty = CollectionSchema::new(&r.collection);
        let l = live
            .iter()
            .find(|c| c.collection == r.collection)
            .unwrap_or(&empty);
        for d in diff_collection(r, l) {
            report.push(d.describe(&r.collection));
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx(name: &str, unique: bool) -> IndexSpec {
        IndexSpec {
            name: name.to_string(),
            keys: vec![(name.trim_end_matches("_idx").to_string(), 1)],
            unique,
        }
    }

    // Acceptance: a detected schema difference is reported.
    #[test]
    fn reports_index_and_validator_drift() {
        let mut recorded = CollectionSchema::new("users");
        recorded.indexes = vec![idx("email_idx", true), idx("name_idx", false)];
        recorded.validator = Some(Bson::String("v1".into()));

        let mut live = CollectionSchema::new("users");
        // email_idx dropped/renamed, name_idx now unique, validator changed.
        live.indexes = vec![idx("name_idx", true), idx("age_idx", false)];
        live.validator = Some(Bson::String("v2".into()));

        let diffs = diff_collection(&recorded, &live);
        assert!(diffs.contains(&SchemaDiff::IndexRemoved("email_idx".into())));
        assert!(diffs.contains(&SchemaDiff::IndexChanged("name_idx".into())));
        assert!(diffs.contains(&SchemaDiff::IndexAdded("age_idx".into())));
        assert!(diffs.contains(&SchemaDiff::ValidatorChanged));

        // reported as readable lines through diff_database.
        let report = diff_database(&[recorded], &[live]);
        assert!(!report.is_empty());
        assert!(report.iter().any(|l| l.contains("email_idx")));
    }

    // No drift when schemas match: no false positives.
    #[test]
    fn identical_schemas_have_no_drift() {
        let mut s = CollectionSchema::new("users");
        s.indexes = vec![idx("email_idx", true)];
        s.validator = Some(Bson::String("v1".into()));
        s.options.insert("capped".into(), Bson::Boolean(true));

        assert!(diff_collection(&s, &s.clone()).is_empty());
        assert!(diff_database(&[s.clone()], &[s]).is_empty());
    }

    /// Build a one-collection dump in directory storage whose recorded
    /// structure carries `indexes`.
    fn dump_with(indexes: Vec<IndexSpec>) -> (tempfile::TempDir, crate::storage::DirectoryStorage) {
        use crate::dump::{write_collection_data, write_metadata};
        use crate::dump::{CollectionToc, DatabaseToc, DumpMetadata, DumpStatus};
        use crate::mongo::CollectionData;

        let dir = tempfile::tempdir().unwrap();
        let s = crate::storage::DirectoryStorage::new(dir.path()).unwrap();
        write_collection_data(
            &s,
            "d1",
            &CollectionData {
                database: "shop".into(),
                collection: "users".into(),
                indexes: indexes.clone(),
                ..Default::default()
            },
            false,
        )
        .unwrap();
        write_metadata(
            &s,
            &DumpMetadata {
                id: "d1".into(),
                status: DumpStatus::Done,
                created_at: "2026-07-01T00:00:00Z".into(),
                databases: vec![DatabaseToc {
                    name: "shop".into(),
                    collections: vec![CollectionToc {
                        name: "users".into(),
                        document_count: 0,
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

    // Acceptance: the drift a dump has accumulated against the live database is
    // reported per collection, naming the database, the collection, and the
    // specific index.
    #[test]
    fn diffs_a_stored_dump_against_a_live_deployment() {
        use crate::mongo::{CollectionData, InMemoryMongo};

        let (_d, storage) = dump_with(vec![idx("email_idx", true)]);
        let live = InMemoryMongo::new();
        // live has a different index than the dump recorded.
        live.seed(CollectionData {
            database: "shop".into(),
            collection: "users".into(),
            indexes: vec![idx("name_idx", false)],
            ..Default::default()
        });

        let report = diff_dump_against_live(&storage, &live, "d1", None, None).unwrap();
        assert!(
            report.iter().any(|l| l.contains("shop.users")
                && l.contains("email_idx")
                && l.contains("missing live")),
            "{report:?}"
        );
        assert!(
            report
                .iter()
                .any(|l| l.contains("name_idx") && l.contains("not recorded")),
            "{report:?}"
        );
    }

    // No drift between a dump and the deployment it came from: silence, not a
    // stream of false positives.
    #[test]
    fn matching_dump_and_live_report_no_drift() {
        use crate::mongo::{CollectionData, InMemoryMongo};

        let (_d, storage) = dump_with(vec![idx("email_idx", true)]);
        let live = InMemoryMongo::new();
        live.seed(CollectionData {
            database: "shop".into(),
            collection: "users".into(),
            indexes: vec![idx("email_idx", true)],
            ..Default::default()
        });
        assert!(diff_dump_against_live(&storage, &live, "d1", None, None)
            .unwrap()
            .is_empty());
    }

    // A collection the dump recorded but that no longer exists live is drift
    // (everything it had is gone), not an error — and the narrowing arguments
    // restrict what is compared.
    #[test]
    fn missing_live_collection_is_drift_and_filters_narrow_the_comparison() {
        use crate::mongo::InMemoryMongo;

        let (_d, storage) = dump_with(vec![idx("email_idx", true)]);
        let empty_live = InMemoryMongo::new();

        let report = diff_dump_against_live(&storage, &empty_live, "d1", None, None).unwrap();
        assert!(report.iter().any(|l| l.contains("email_idx")), "{report:?}");

        // narrowing to another database or collection compares nothing.
        assert!(
            diff_dump_against_live(&storage, &empty_live, "d1", Some("other"), None)
                .unwrap()
                .is_empty()
        );
        assert!(
            diff_dump_against_live(&storage, &empty_live, "d1", None, Some("orders"))
                .unwrap()
                .is_empty()
        );
    }

    // Option drift is caught too.
    #[test]
    fn reports_option_drift() {
        let mut recorded = CollectionSchema::new("logs");
        recorded
            .options
            .insert("capped".into(), Bson::Boolean(false));
        let mut live = CollectionSchema::new("logs");
        live.options.insert("capped".into(), Bson::Boolean(true));
        assert_eq!(
            diff_collection(&recorded, &live),
            vec![SchemaDiff::OptionChanged("capped".into())]
        );
    }
}
