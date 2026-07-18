//! Virtual (non-FK) reference declaration (feature `subset.virtual-references`).
//!
//! MongoDB does not enforce foreign keys, so relationships between collections
//! are declared explicitly under `virtual_references`. The subsetting engine
//! treats these declarations exactly as a relational engine treats foreign
//! keys: as edges of a dependency graph to follow.
//!
//! A reference may be `not_null` (affecting how strictly subsetting must satisfy
//! it) and may be *polymorphic*, pointing to different target collections
//! depending on a discriminator field's value.
//!
//! Gap: the source product expresses polymorphic targets as free-form
//! expressions. Here they are modelled as `field == value -> collection` cases,
//! which covers the documented discriminator use — see regeneration-gaps.md.

use bson::{Bson, Document};
use serde::Deserialize;

use crate::toolkit::cast;

/// A polymorphic discriminator case: when `field` equals `value`, the reference
/// points at `references_collection`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PolymorphicCase {
    pub field: String,
    pub value: serde_yaml::Value,
    pub references_collection: String,
}

/// One declared reference on a holding collection.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReferenceDef {
    /// The field on the holding collection that carries the referencing value.
    pub field: String,
    /// The fixed target collection (omitted when polymorphic).
    #[serde(default)]
    pub references_collection: Option<String>,
    /// The target field being referenced (defaults to `_id`).
    #[serde(default = "default_id")]
    pub references_field: String,
    /// Whether the reference must be satisfied (non-null).
    #[serde(default)]
    pub not_null: bool,
    /// Polymorphic discriminator cases (empty for a fixed reference).
    #[serde(default)]
    pub polymorphic_exprs: Vec<PolymorphicCase>,
}

fn default_id() -> String {
    "_id".to_string()
}

/// A `virtual_references` entry: a holding collection and its references.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VirtualReferenceEntry {
    pub collection: String,
    #[serde(default)]
    pub references: Vec<ReferenceDef>,
}

/// The target of a reference: a fixed collection, or polymorphic cases.
#[derive(Debug, Clone, PartialEq)]
pub enum RefTarget {
    Fixed(String),
    Polymorphic(Vec<(String, Bson, String)>),
}

/// A single directed reference edge in the dependency graph.
#[derive(Debug, Clone, PartialEq)]
pub struct VirtualReference {
    pub from_collection: String,
    pub from_field: String,
    pub to_field: String,
    pub not_null: bool,
    pub target: RefTarget,
}

impl VirtualReference {
    /// Resolve the concrete target collection for a given holding document.
    /// Fixed references ignore the document; polymorphic ones consult the
    /// discriminator field. Returns `None` when no case matches.
    pub fn resolve_target(&self, doc: &Document) -> Option<String> {
        match &self.target {
            RefTarget::Fixed(c) => Some(c.clone()),
            RefTarget::Polymorphic(cases) => {
                for (field, value, collection) in cases {
                    if doc.get(field.as_str()) == Some(value) {
                        return Some(collection.clone());
                    }
                }
                None
            }
        }
    }

    /// All collections this edge could point at (for graph construction).
    pub fn target_collections(&self) -> Vec<String> {
        match &self.target {
            RefTarget::Fixed(c) => vec![c.clone()],
            RefTarget::Polymorphic(cases) => cases.iter().map(|(_, _, c)| c.clone()).collect(),
        }
    }
}

/// The dependency graph built from all declared virtual references.
#[derive(Debug, Clone, Default)]
pub struct ReferenceGraph {
    edges: Vec<VirtualReference>,
}

impl ReferenceGraph {
    /// Build the graph from parsed config entries.
    pub fn from_entries(entries: &[VirtualReferenceEntry]) -> Self {
        let mut edges = Vec::new();
        for entry in entries {
            for r in &entry.references {
                let target = if !r.polymorphic_exprs.is_empty() {
                    RefTarget::Polymorphic(
                        r.polymorphic_exprs
                            .iter()
                            .map(|c| {
                                (
                                    c.field.clone(),
                                    cast::yaml_to_bson(&c.value),
                                    c.references_collection.clone(),
                                )
                            })
                            .collect(),
                    )
                } else if let Some(c) = &r.references_collection {
                    RefTarget::Fixed(c.clone())
                } else {
                    // A reference with neither a fixed target nor polymorphic
                    // cases is inert; skip it.
                    continue;
                };
                edges.push(VirtualReference {
                    from_collection: entry.collection.clone(),
                    from_field: r.field.clone(),
                    to_field: r.references_field.clone(),
                    not_null: r.not_null,
                    target,
                });
            }
        }
        ReferenceGraph { edges }
    }

    /// Outgoing references declared on `collection`.
    pub fn references_from(&self, collection: &str) -> Vec<&VirtualReference> {
        self.edges
            .iter()
            .filter(|e| e.from_collection == collection)
            .collect()
    }

    /// All edges.
    pub fn edges(&self) -> &[VirtualReference] {
        &self.edges
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(src: &str) -> Vec<VirtualReferenceEntry> {
        serde_yaml::from_str(src).unwrap()
    }

    // Acceptance: a declared relationship becomes an edge the graph follows.
    #[test]
    fn declaration_becomes_a_graph_edge() {
        let g = ReferenceGraph::from_entries(&entries(
            "- collection: orders\n  references:\n    - field: user_id\n      references_collection: users\n",
        ));
        let refs = g.references_from("orders");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].from_field, "user_id");
        assert_eq!(refs[0].to_field, "_id"); // default
        assert_eq!(refs[0].target, RefTarget::Fixed("users".into()));
    }

    // Acceptance: a reference can be marked not_null.
    #[test]
    fn not_null_is_recorded() {
        let g = ReferenceGraph::from_entries(&entries(
            "- collection: orders\n  references:\n    - field: user_id\n      references_collection: users\n      not_null: true\n",
        ));
        assert!(g.references_from("orders")[0].not_null);
        // defaults to false.
        let g2 = ReferenceGraph::from_entries(&entries(
            "- collection: orders\n  references:\n    - field: user_id\n      references_collection: users\n",
        ));
        assert!(!g2.references_from("orders")[0].not_null);
    }

    // Acceptance: a polymorphic reference points at different collections
    // depending on a discriminator field.
    #[test]
    fn polymorphic_reference_resolves_by_discriminator() {
        let g = ReferenceGraph::from_entries(&entries(
            "- collection: comments\n  references:\n    - field: parent_id\n      polymorphic_exprs:\n        - field: parent_type\n          value: post\n          references_collection: posts\n        - field: parent_type\n          value: photo\n          references_collection: photos\n",
        ));
        let edge = g.references_from("comments")[0];

        let mut on_post = Document::new();
        on_post.insert("parent_type", "post");
        assert_eq!(edge.resolve_target(&on_post).as_deref(), Some("posts"));

        let mut on_photo = Document::new();
        on_photo.insert("parent_type", "photo");
        assert_eq!(edge.resolve_target(&on_photo).as_deref(), Some("photos"));

        // no matching discriminator -> unresolved.
        let mut other = Document::new();
        other.insert("parent_type", "video");
        assert_eq!(edge.resolve_target(&other), None);

        // both possible targets are visible to graph construction.
        let mut tc = edge.target_collections();
        tc.sort();
        assert_eq!(tc, vec!["photos".to_string(), "posts".to_string()]);
    }
}
