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
            SchemaDiff::IndexAdded(n) => format!("{collection}: index '{n}' added live, not recorded"),
            SchemaDiff::IndexRemoved(n) => format!("{collection}: recorded index '{n}' is missing live"),
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

/// Diff a whole database's worth of recorded vs live schemas, returning the
/// drift lines. An empty result means no drift.
pub fn diff_database(
    recorded: &[CollectionSchema],
    live: &[CollectionSchema],
) -> Vec<String> {
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

    // Option drift is caught too.
    #[test]
    fn reports_option_drift() {
        let mut recorded = CollectionSchema::new("logs");
        recorded.options.insert("capped".into(), Bson::Boolean(false));
        let mut live = CollectionSchema::new("logs");
        live.options.insert("capped".into(), Bson::Boolean(true));
        assert_eq!(
            diff_collection(&recorded, &live),
            vec![SchemaDiff::OptionChanged("capped".into())]
        );
    }
}
