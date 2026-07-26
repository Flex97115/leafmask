//! Transformation propagation to referencing fields
//! (feature `transform.transformation-inheritance`).
//!
//! A transformer marked `apply_for_references` is automatically applied to every
//! field in other collections that holds a declared reference to the transformed
//! field, so the referencing (foreign-key) values stay consistent with the
//! transformed ones — without repeating the configuration. Because the inherited
//! transformer is the identical transformer (same name, params, and salt), an
//! equal input value produces an equal output on both sides.
//!
//! A referencing field that already has its own explicitly configured
//! transformation is never overridden by an inherited one.

use crate::config::Params;
use crate::subset::ReferenceGraph;

/// One configured field transformation.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldTransform {
    pub collection: String,
    pub field: String,
    pub transformer_name: String,
    pub params: Params,
    /// Whether this transformer should propagate to referencing fields.
    pub apply_for_references: bool,
    /// True when produced by inheritance rather than explicit config.
    pub inherited: bool,
}

impl FieldTransform {
    pub fn new(collection: &str, field: &str, transformer_name: &str) -> Self {
        FieldTransform {
            collection: collection.to_string(),
            field: field.to_string(),
            transformer_name: transformer_name.to_string(),
            params: Params::default(),
            apply_for_references: false,
            inherited: false,
        }
    }
}

/// Expand explicit transformations with inherited ones derived from the
/// reference graph. Explicit transforms always win; inherited transforms are
/// added only for referencing fields that have no explicit transform.
pub fn expand_inherited(
    explicit: &[FieldTransform],
    graph: &ReferenceGraph,
) -> Vec<FieldTransform> {
    let has_explicit = |collection: &str, field: &str| {
        explicit
            .iter()
            .any(|t| t.collection == collection && t.field == field)
    };

    let mut out = explicit.to_vec();

    for src in explicit.iter().filter(|t| t.apply_for_references) {
        for edge in graph.edges() {
            // Does this edge point at the transformed (collection, field)?
            let points_here = edge.to_field == src.field
                && edge
                    .target_collections()
                    .iter()
                    .any(|c| c == &src.collection);
            if !points_here {
                continue;
            }
            // The referencing field inherits — unless it has its own transform,
            // or we already added it.
            if has_explicit(&edge.from_collection, &edge.from_field)
                || out.iter().any(|t| {
                    t.collection == edge.from_collection
                        && t.field == edge.from_field
                        && t.inherited
                })
            {
                continue;
            }
            out.push(FieldTransform {
                collection: edge.from_collection.clone(),
                field: edge.from_field.clone(),
                transformer_name: src.transformer_name.clone(),
                params: src.params.clone(),
                apply_for_references: false,
                inherited: true,
            });
        }
    }

    out
}

/// Expand a configured transformation list with the rules inherited through
/// `graph`, so `apply_for_references` actually reaches the dump instead of
/// being a parsed-and-ignored flag.
///
/// An inherited rule copies the source rule's transformer name and parameters
/// and nothing else. `when` and `dynamic_params` are deliberately dropped:
/// both read fields of the document being transformed, and the referencing
/// document is a different document in a different collection — carrying them
/// over would make the two sides diverge, defeating the very consistency this
/// feature exists to provide.
pub fn expand_transformation_configs(
    configs: &[super::apply::TransformationConfig],
    graph: &ReferenceGraph,
) -> Vec<super::apply::TransformationConfig> {
    use super::apply::{TransformationConfig, TransformerRule};

    let explicit: Vec<FieldTransform> = configs
        .iter()
        .flat_map(|c| {
            c.transformers.iter().map(|r| FieldTransform {
                collection: c.collection.clone(),
                field: r.field.clone(),
                transformer_name: r.name.clone(),
                params: r.params.clone(),
                apply_for_references: r.apply_for_references,
                inherited: false,
            })
        })
        .collect();

    let mut out = configs.to_vec();
    for t in expand_inherited(&explicit, graph)
        .iter()
        .filter(|t| t.inherited)
    {
        let rule = TransformerRule {
            field: t.field.clone(),
            name: t.transformer_name.clone(),
            params: t.params.clone(),
            when: None,
            dynamic_params: Default::default(),
            // An inherited rule never propagates further: one hop only, or a
            // reference chain would rewrite collections nobody configured.
            apply_for_references: false,
            type_override: None,
        };
        match out.iter_mut().find(|c| c.collection == t.collection) {
            Some(existing) => existing.transformers.push(rule),
            None => out.push(TransformationConfig {
                collection: t.collection.clone(),
                when: None,
                query: None,
                transformers: vec![rule],
            }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subset::VirtualReferenceEntry;

    fn graph(src: &str) -> ReferenceGraph {
        let entries: Vec<VirtualReferenceEntry> = serde_yaml::from_str(src).unwrap();
        ReferenceGraph::from_entries(&entries)
    }

    // Acceptance: a transformer with apply_for_references applies to referencing
    // fields, keeping referential values consistent.
    #[test]
    fn propagates_to_referencing_fields() {
        let g = graph(
            "- collection: orders\n  references:\n    - field: user_id\n      references_collection: users\n",
        );
        let mut src = FieldTransform::new("users", "_id", "random_object_id");
        src.apply_for_references = true;

        let expanded = expand_inherited(&[src], &g);
        // orders.user_id now inherits the same transformer.
        let inherited = expanded
            .iter()
            .find(|t| t.collection == "orders" && t.field == "user_id")
            .expect("orders.user_id should inherit");
        assert_eq!(inherited.transformer_name, "random_object_id");
        assert!(inherited.inherited);
    }

    // Acceptance: a referencing field with its own explicit transformation is
    // not overridden.
    #[test]
    fn explicit_transform_is_not_overridden() {
        let g = graph(
            "- collection: orders\n  references:\n    - field: user_id\n      references_collection: users\n",
        );
        let mut src = FieldTransform::new("users", "_id", "random_object_id");
        src.apply_for_references = true;
        let explicit_ref = FieldTransform::new("orders", "user_id", "set_null");

        let expanded = expand_inherited(&[src, explicit_ref], &g);
        let order_transforms: Vec<&FieldTransform> = expanded
            .iter()
            .filter(|t| t.collection == "orders" && t.field == "user_id")
            .collect();
        // exactly one, and it is the explicit one.
        assert_eq!(order_transforms.len(), 1);
        assert_eq!(order_transforms[0].transformer_name, "set_null");
        assert!(!order_transforms[0].inherited);
    }

    // Without apply_for_references, nothing is propagated.
    #[test]
    fn no_propagation_without_flag() {
        let g = graph(
            "- collection: orders\n  references:\n    - field: user_id\n      references_collection: users\n",
        );
        let src = FieldTransform::new("users", "_id", "random_object_id");
        let expanded = expand_inherited(&[src], &g);
        assert!(!expanded.iter().any(|t| t.collection == "orders"));
    }
}
