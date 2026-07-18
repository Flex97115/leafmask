//! Dynamic (field-referencing) transformer parameters
//! (feature `transform.dynamic-parameters`).
//!
//! A parameter can be derived from another field's value on the same document
//! at transform time (e.g. so `created_at` is always before `updated_at`).
//! Dynamic values are cast to the transformer's expected type exactly like
//! static ones. A dynamic parameter that references a field the collection does
//! not have is a config-validation error, never a runtime panic.

use std::collections::{BTreeMap, HashSet};

use bson::{Bson, Document};
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::toolkit::{cast, ParamDefinition, ParamType, ParamValues};

/// Where a dynamic parameter reads its value from.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DynSource {
    /// The referenced field path (dotted for nested fields).
    #[serde(alias = "column")]
    pub field: String,
}

/// Raw `dynamic_params` config: parameter name -> source field.
pub type DynamicParamsConfig = BTreeMap<String, DynSource>;

/// A resolved dynamic parameter binding.
#[derive(Debug, Clone, PartialEq)]
pub struct DynamicBinding {
    pub param: String,
    pub field: String,
    pub kind: ParamType,
}

/// The dynamic parameters configured for one transformer.
#[derive(Debug, Clone, Default)]
pub struct DynamicParams {
    bindings: Vec<DynamicBinding>,
}

impl DynamicParams {
    /// Build from config, resolving each parameter's expected type from the
    /// transformer's declared parameter list. A dynamic parameter that names a
    /// parameter the transformer does not declare is an error.
    pub fn from_config(cfg: &DynamicParamsConfig, defs: &[ParamDefinition]) -> Result<Self> {
        let mut bindings = Vec::new();
        for (param, source) in cfg {
            let def = defs.iter().find(|d| &d.name == param).ok_or_else(|| {
                Error::Parameter(format!(
                    "dynamic parameter '{param}' is not a parameter of this transformer"
                ))
            })?;
            bindings.push(DynamicBinding {
                param: param.clone(),
                field: source.field.clone(),
                kind: def.kind,
            });
        }
        Ok(DynamicParams { bindings })
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Validate that every referenced field exists in the collection's known
    /// field set. This is the config-time check that turns a typo into a clear
    /// error instead of a runtime surprise.
    pub fn validate_fields(&self, known_fields: &HashSet<String>) -> Result<()> {
        for b in &self.bindings {
            let head = b.field.split('.').next().unwrap_or(&b.field);
            if !known_fields.contains(head) && !known_fields.contains(&b.field) {
                return Err(Error::Parameter(format!(
                    "dynamic parameter '{}' references unknown field '{}'",
                    b.param, b.field
                )));
            }
        }
        Ok(())
    }

    /// Resolve the dynamic parameters against `doc`, layering them onto `base`
    /// static values. A missing field at runtime is an error, not a panic.
    pub fn resolve(&self, doc: &Document, base: &ParamValues) -> Result<ParamValues> {
        let mut values = base.clone();
        for b in &self.bindings {
            let raw = get_path(doc, &b.field).ok_or_else(|| {
                Error::Parameter(format!(
                    "dynamic parameter '{}' field '{}' is absent from the document",
                    b.param, b.field
                ))
            })?;
            let typed = cast::bson_to_typed(raw, b.kind).map_err(|e| {
                Error::Parameter(format!("dynamic parameter '{}': {e}", b.param))
            })?;
            values.insert(b.param.clone(), typed);
        }
        Ok(values)
    }
}

/// Read a (possibly dotted) field path out of a document.
fn get_path<'a>(doc: &'a Document, path: &str) -> Option<&'a Bson> {
    let mut parts = path.split('.');
    let first = parts.next()?;
    let mut current = doc.get(first)?;
    for part in parts {
        current = current.as_document()?.get(part)?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs() -> Vec<ParamDefinition> {
        vec![
            ParamDefinition::required("min", ParamType::DateTime, "lower bound"),
            ParamDefinition::required("count", ParamType::Int, "n"),
        ]
    }
    fn cfg(src: &str) -> DynamicParamsConfig {
        serde_yaml::from_str(src).unwrap()
    }

    // Acceptance: a dynamic param resolves to the referenced field's current
    // document value, cast to the expected type.
    #[test]
    fn resolves_and_casts_from_document() {
        let dp = DynamicParams::from_config(&cfg("min:\n  field: created_at\n"), &defs()).unwrap();
        let mut doc = Document::new();
        let created = bson::DateTime::parse_rfc3339_str("2021-05-05T00:00:00Z").unwrap();
        doc.insert("created_at", created);

        let resolved = dp.resolve(&doc, &ParamValues::default()).unwrap();
        assert_eq!(resolved.get("min"), Some(&Bson::DateTime(created)));
    }

    // Casting also applies to values that need conversion (string -> int).
    #[test]
    fn casts_like_static_params() {
        let dp = DynamicParams::from_config(&cfg("count:\n  field: n\n"), &defs()).unwrap();
        let mut doc = Document::new();
        doc.insert("n", "7"); // stored as string, expected int
        let resolved = dp.resolve(&doc, &ParamValues::default()).unwrap();
        assert_eq!(resolved.get_i64("count"), Some(7));
    }

    // Acceptance: referencing a nonexistent field is a validation error, not a
    // runtime panic.
    #[test]
    fn unknown_field_is_a_validation_error() {
        let dp = DynamicParams::from_config(&cfg("min:\n  field: nope\n"), &defs()).unwrap();
        let known: HashSet<String> = ["created_at".to_string(), "n".to_string()].into_iter().collect();
        let err = dp.validate_fields(&known).unwrap_err();
        assert!(err.to_string().contains("unknown field 'nope'"), "{err}");

        // and at runtime a missing field errors rather than panics.
        let err = dp.resolve(&Document::new(), &ParamValues::default()).unwrap_err();
        assert!(err.to_string().contains("absent"), "{err}");
    }

    // A dynamic param that names a non-parameter is rejected up front.
    #[test]
    fn unknown_parameter_is_rejected() {
        let err = DynamicParams::from_config(&cfg("bogus:\n  field: x\n"), &defs()).unwrap_err();
        assert!(err.to_string().contains("not a parameter"), "{err}");
    }
}
