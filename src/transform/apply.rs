//! Apply configured transformations during dump (feature
//! `transform.apply-transformations`).
//!
//! Ties the engine together: for each collection under `dump.transformation`,
//! the configured transformers run on their fields as documents stream by.
//! Collections without a transformation entry pass through unchanged. A
//! collection may carry a custom `query` filter (restricting which documents are
//! transformed/dumped) and each transformer may override the field's logical
//! type before running.
//!
//! Gap: the offline `query` matcher supports equality filters; the real dump
//! pushes the query to MongoDB. See regeneration-gaps.md.

use std::collections::BTreeMap;

use bson::{Bson, Document};
use serde::Deserialize;

use crate::config::Params;
use crate::error::{Error, Result};
use crate::hash::HashEngine;
use crate::toolkit::{cast, validate_static, ParamType, ParamValues};

use super::condition::Condition;
use super::dynamic::{DynamicParams, DynamicParamsConfig};
use super::Registry;

/// A single transformer rule on a field, as configured.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TransformerRule {
    /// Target field (dotted path).
    #[serde(alias = "column")]
    pub field: String,
    /// Transformer name (must exist in the registry).
    pub name: String,
    #[serde(default)]
    pub params: Params,
    #[serde(default)]
    pub when: Option<String>,
    #[serde(default)]
    pub dynamic_params: DynamicParamsConfig,
    #[serde(default)]
    pub apply_for_references: bool,
    /// Override the field's logical type before the transformer runs.
    #[serde(default)]
    pub type_override: Option<String>,
}

/// A collection's transformation entry.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TransformationConfig {
    pub collection: String,
    #[serde(default)]
    pub when: Option<String>,
    /// Optional custom filter restricting which documents are dumped.
    #[serde(default)]
    pub query: Option<serde_yaml::Value>,
    #[serde(default)]
    pub transformers: Vec<TransformerRule>,
}

enum RuleExec {
    Static(Box<dyn super::Transformer>),
    Dynamic {
        name: String,
        base: ParamValues,
        dynamic: DynamicParams,
    },
}

struct CompiledRule {
    field: String,
    when: Option<Condition>,
    type_override: Option<ParamType>,
    exec: RuleExec,
}

struct CollectionPlan {
    when: Option<Condition>,
    query: Option<Document>,
    rules: Vec<CompiledRule>,
}

/// A compiled transformation plan ready to run against streaming documents.
pub struct TransformationPlan {
    collections: BTreeMap<String, CollectionPlan>,
}

impl TransformationPlan {
    /// Compile config into an executable plan against `registry`.
    pub fn compile(
        configs: &[TransformationConfig],
        registry: &Registry,
        engine: &HashEngine,
    ) -> Result<Self> {
        let mut collections = BTreeMap::new();
        for c in configs {
            let when = match &c.when {
                Some(w) => Some(Condition::parse(w)?),
                None => None,
            };
            let query = match &c.query {
                Some(v) => match cast::yaml_to_bson(v) {
                    Bson::Document(d) => Some(d),
                    _ => {
                        return Err(Error::Transform(
                            "collection query must be a document".into(),
                        ))
                    }
                },
                None => None,
            };
            let mut rules = Vec::new();
            for r in &c.transformers {
                let factory = registry.get(&r.name).ok_or_else(|| {
                    Error::NotFound(format!("transformer '{}' not found", r.name))
                })?;
                let base = validate_static(&factory.parameters, &r.params)?;
                let when = match &r.when {
                    Some(w) => Some(Condition::parse(w)?),
                    None => None,
                };
                let type_override =
                    match &r.type_override {
                        Some(t) => Some(ParamType::from_name(t).ok_or_else(|| {
                            Error::Transform(format!("unknown type override '{t}'"))
                        })?),
                        None => None,
                    };
                let exec = if r.dynamic_params.is_empty() {
                    RuleExec::Static(registry.build(&r.name, &base, engine)?)
                } else {
                    let dynamic =
                        DynamicParams::from_config(&r.dynamic_params, &factory.parameters)?;
                    RuleExec::Dynamic {
                        name: r.name.clone(),
                        base,
                        dynamic,
                    }
                };
                rules.push(CompiledRule {
                    field: r.field.clone(),
                    when,
                    type_override,
                    exec,
                });
            }
            collections.insert(c.collection.clone(), CollectionPlan { when, query, rules });
        }
        Ok(TransformationPlan { collections })
    }

    /// The fields that have a transformer configured for `collection`.
    pub fn transformed_fields(&self, collection: &str) -> Vec<String> {
        self.collections
            .get(collection)
            .map(|c| c.rules.iter().map(|r| r.field.clone()).collect())
            .unwrap_or_default()
    }

    /// Whether `doc` from `collection` should be dumped, honouring any custom
    /// query filter. Collections without a filter include everything.
    pub fn should_include(&self, collection: &str, doc: &Document) -> bool {
        match self
            .collections
            .get(collection)
            .and_then(|c| c.query.as_ref())
        {
            Some(query) => matches_query(doc, query),
            None => true,
        }
    }

    /// Transform one document from `collection`. A collection with no entry, or
    /// whose collection-level `when` is false, is returned unmodified.
    pub fn transform(
        &self,
        registry: &Registry,
        engine: &HashEngine,
        collection: &str,
        doc: &Document,
    ) -> Result<Document> {
        let plan = match self.collections.get(collection) {
            Some(p) => p,
            None => return Ok(doc.clone()),
        };
        if let Some(w) = &plan.when {
            if !w.eval(doc) {
                return Ok(doc.clone());
            }
        }
        let mut out = doc.clone();
        for rule in &plan.rules {
            if let Some(w) = &rule.when {
                if !w.eval(doc) {
                    continue; // this transformer skips; value passes through.
                }
            }
            let value = get_path(doc, &rule.field).cloned().unwrap_or(Bson::Null);
            let input = match rule.type_override {
                Some(t) => cast::bson_to_typed(&value, t)?,
                None => value,
            };
            let result = match &rule.exec {
                RuleExec::Static(t) => t.transform(&input, doc)?,
                RuleExec::Dynamic {
                    name,
                    base,
                    dynamic,
                } => {
                    let merged = dynamic.resolve(doc, base)?;
                    let t = registry.build(name, &merged, engine)?;
                    t.transform(&input, doc)?
                }
            };
            set_path(&mut out, &rule.field, result);
        }
        Ok(out)
    }
}

/// Equality-only query matcher (offline approximation of a MongoDB filter).
fn matches_query(doc: &Document, query: &Document) -> bool {
    query
        .iter()
        .all(|(k, v)| get_path(doc, k).map(|actual| actual == v).unwrap_or(false))
}

fn get_path<'a>(doc: &'a Document, path: &str) -> Option<&'a Bson> {
    let mut parts = path.split('.');
    let mut current = doc.get(parts.next()?)?;
    for part in parts {
        current = current.as_document()?.get(part)?;
    }
    Some(current)
}

/// Set a (possibly dotted) path, creating intermediate documents as needed.
fn set_path(doc: &mut Document, path: &str, value: Bson) {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.len() == 1 {
        doc.insert(parts[0], value);
        return;
    }
    let head = parts[0];
    let rest = parts[1..].join(".");
    let child = match doc.get_mut(head) {
        Some(Bson::Document(d)) => d,
        _ => {
            doc.insert(head, Document::new());
            doc.get_mut(head).unwrap().as_document_mut().unwrap()
        }
    };
    set_path(child, &rest, value);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(src: &str) -> (TransformationPlan, Registry, HashEngine) {
        let configs: Vec<TransformationConfig> = serde_yaml::from_str(src).unwrap();
        let registry = Registry::with_builtins();
        let engine = HashEngine::new("salt");
        let plan = TransformationPlan::compile(&configs, &registry, &engine).unwrap();
        (plan, registry, engine)
    }

    fn doc(pairs: &[(&str, Bson)]) -> Document {
        let mut d = Document::new();
        for (k, v) in pairs {
            d.insert(*k, v.clone());
        }
        d
    }

    // Acceptance: configured transformers run on matching fields.
    #[test]
    fn transforms_configured_fields() {
        let (p, r, e) =
            plan("- collection: users\n  transformers:\n    - field: email\n      name: masking\n");
        let out = p
            .transform(
                &r,
                &e,
                "users",
                &doc(&[("email", Bson::String("bob@x.com".into()))]),
            )
            .unwrap();
        assert_eq!(out.get_str("email").unwrap(), "*********");
    }

    // Acceptance: collections without an entry are unmodified.
    #[test]
    fn untouched_collections_pass_through() {
        let (p, r, e) =
            plan("- collection: users\n  transformers:\n    - field: email\n      name: masking\n");
        let original = doc(&[("total", Bson::Int64(100))]);
        assert_eq!(p.transform(&r, &e, "orders", &original).unwrap(), original);
    }

    // Acceptance: a custom query restricts which documents are included.
    #[test]
    fn custom_query_filters_documents() {
        let (p, _r, _e) = plan(
            "- collection: users\n  query:\n    active: true\n  transformers:\n    - field: email\n      name: set_null\n",
        );
        assert!(p.should_include("users", &doc(&[("active", Bson::Boolean(true))])));
        assert!(!p.should_include("users", &doc(&[("active", Bson::Boolean(false))])));
        // collections without a query include everything.
        assert!(p.should_include("orders", &doc(&[])));
    }

    // Acceptance: field types can be overridden for the transformer.
    #[test]
    fn type_override_changes_operating_type() {
        // masking works on strings; the field is an int, overridden to string.
        let (p, r, e) = plan(
            "- collection: users\n  transformers:\n    - field: pin\n      name: masking\n      type_override: string\n",
        );
        let out = p
            .transform(&r, &e, "users", &doc(&[("pin", Bson::Int64(12345))]))
            .unwrap();
        assert_eq!(out.get_str("pin").unwrap(), "*****");
    }

    // Transformer-level and collection-level `when` gate application.
    #[test]
    fn conditions_gate_transformation() {
        let (p, r, e) = plan(
            "- collection: users\n  transformers:\n    - field: email\n      name: set_null\n      when: \"opted_out == true\"\n",
        );
        // opted_out true -> transformed to null.
        let out = p
            .transform(
                &r,
                &e,
                "users",
                &doc(&[
                    ("email", Bson::String("a@b.c".into())),
                    ("opted_out", Bson::Boolean(true)),
                ]),
            )
            .unwrap();
        assert_eq!(out.get("email"), Some(&Bson::Null));
        // opted_out false -> passes through unmodified.
        let keep = doc(&[
            ("email", Bson::String("a@b.c".into())),
            ("opted_out", Bson::Boolean(false)),
        ]);
        assert_eq!(p.transform(&r, &e, "users", &keep).unwrap(), keep);
    }

    // Dynamic params resolve per-document at transform time.
    #[test]
    fn dynamic_params_resolve_per_document() {
        let (p, r, e) = plan(
            "- collection: events\n  transformers:\n    - field: ends_at\n      name: random_int\n      params:\n        min: 0\n        max: 1\n      dynamic_params:\n        max:\n          field: cap\n",
        );
        // cap drives the upper bound; with min=0,max=cap=0 the only value is 0.
        let out = p
            .transform(
                &r,
                &e,
                "events",
                &doc(&[("ends_at", Bson::Int64(999)), ("cap", Bson::Int64(0))]),
            )
            .unwrap();
        assert_eq!(out.get("ends_at"), Some(&Bson::Int64(0)));
    }
}
