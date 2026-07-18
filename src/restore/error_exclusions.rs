//! Insert error exclusions (feature `restore.error-exclusions`).
//!
//! During data restoration an insert can fail (most commonly a duplicate-key
//! `E11000`). The operator can declare specific error codes or unique-index
//! names — globally or per collection — that should be tolerated and skipped
//! instead of aborting the restore. This module is the pure decision function;
//! the restore driver consults it per failed document.

use serde::Deserialize;

/// A single insert failure, distilled to the parts exclusions match on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InsertError {
    /// The MongoDB error code (e.g. `11000` for a duplicate key), if known.
    pub code: Option<i32>,
    /// The offending unique-index name, if the error identified one.
    pub index_name: Option<String>,
}

impl InsertError {
    pub fn code(code: i32) -> Self {
        InsertError {
            code: Some(code),
            index_name: None,
        }
    }
    pub fn duplicate_key(index_name: &str) -> Self {
        InsertError {
            code: Some(11000),
            index_name: Some(index_name.to_string()),
        }
    }
}

/// Per-collection exclusion rule.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CollectionExclusion {
    /// Collection these exclusions apply to.
    pub collection: String,
    /// Tolerated error codes for this collection.
    #[serde(default)]
    pub error_codes: Vec<i32>,
    /// Tolerated unique-index names for this collection.
    #[serde(default)]
    pub index_names: Vec<String>,
}

/// The full set of insert error exclusions, as configured under
/// `restore.insert_error_exclusions`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ErrorExclusions {
    /// Error codes tolerated across every collection.
    #[serde(default)]
    pub global_error_codes: Vec<i32>,
    /// Index names tolerated across every collection.
    #[serde(default)]
    pub global_index_names: Vec<String>,
    /// Collection-specific exclusions.
    #[serde(default)]
    pub collections: Vec<CollectionExclusion>,
}

impl ErrorExclusions {
    /// Whether a failed insert into `collection` should be logged-and-skipped
    /// (true) rather than aborting the restore (false).
    pub fn is_excluded(&self, collection: &str, err: &InsertError) -> bool {
        if self.matches_code_global(err) || self.matches_index_global(err) {
            return true;
        }
        for c in &self.collections {
            if c.collection == collection {
                if let Some(code) = err.code {
                    if c.error_codes.contains(&code) {
                        return true;
                    }
                }
                if let Some(idx) = &err.index_name {
                    if c.index_names.iter().any(|n| n == idx) {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn matches_code_global(&self, err: &InsertError) -> bool {
        err.code
            .map(|c| self.global_error_codes.contains(&c))
            .unwrap_or(false)
    }

    fn matches_index_global(&self, err: &InsertError) -> bool {
        err.index_name
            .as_ref()
            .map(|idx| self.global_index_names.iter().any(|n| n == idx))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> ErrorExclusions {
        serde_yaml::from_str(src).unwrap()
    }

    // Acceptance: an insert error matching a configured excluded index name or
    // error code for its collection is skipped.
    #[test]
    fn matching_collection_exclusion_is_skipped() {
        let ex = parse(
            "collections:\n  - collection: users\n    error_codes: [11000]\n    index_names: [email_unique]\n",
        );
        assert!(ex.is_excluded("users", &InsertError::code(11000)));
        assert!(ex.is_excluded("users", &InsertError::duplicate_key("email_unique")));
    }

    // Acceptance: an insert error not matching any exclusion still aborts.
    #[test]
    fn non_matching_error_is_not_excluded() {
        let ex = parse("collections:\n  - collection: users\n    error_codes: [11000]\n");
        // different code
        assert!(!ex.is_excluded("users", &InsertError::code(121)));
        // right code, wrong collection
        assert!(!ex.is_excluded("orders", &InsertError::code(11000)));
        // empty config excludes nothing.
        assert!(!ErrorExclusions::default().is_excluded("users", &InsertError::code(11000)));
    }

    // Acceptance: global exclusions apply across all collections in addition to
    // collection-specific ones.
    #[test]
    fn global_exclusions_apply_everywhere() {
        let ex = parse("global_error_codes: [11000]\nglobal_index_names: [tenant_idx]\n");
        assert!(ex.is_excluded("users", &InsertError::code(11000)));
        assert!(ex.is_excluded("orders", &InsertError::code(11000)));
        assert!(ex.is_excluded("anything", &InsertError::duplicate_key("tenant_idx")));
        assert!(!ex.is_excluded("users", &InsertError::code(99)));
    }
}
