//! Referentially-intact database subsetting (feature `subset.database-subsetting`).
//!
//! The operator filters selected collections via `subset_conds`; the engine then
//! follows the declared reference graph so that every document required to
//! satisfy a reference of an included document is itself included — even without
//! its own filter. Traversal is document-source-agnostic (a trait), so the
//! closure logic is fully testable without MongoDB. Cyclic and polymorphic
//! references are handled: a visited set bounds cycles, and polymorphic edges
//! resolve their target per document.

use std::collections::{BTreeMap, HashSet};

use bson::{Bson, Document};

use crate::error::Result;
use crate::transform::condition::Condition;

use super::ReferenceGraph;

/// A source of documents the subsetting engine reads from. The real
/// implementation queries MongoDB; tests use an in-memory store.
pub trait DocumentSource {
    /// All documents in a collection.
    fn all(&self, collection: &str) -> Vec<Document>;
    /// Documents whose `field` equals `value`.
    fn find_by(&self, collection: &str, field: &str, value: &Bson) -> Vec<Document>;
}

/// The subsetting engine: a reference graph plus per-collection filter
/// conditions.
pub struct SubsetEngine<'a> {
    graph: &'a ReferenceGraph,
    conds: BTreeMap<String, Condition>,
}

impl<'a> SubsetEngine<'a> {
    /// Build from the graph and raw `subset_conds` (collection -> expression).
    pub fn new(graph: &'a ReferenceGraph, conds: &BTreeMap<String, String>) -> Result<Self> {
        let mut parsed = BTreeMap::new();
        for (collection, expr) in conds {
            parsed.insert(collection.clone(), Condition::parse(expr)?);
        }
        Ok(SubsetEngine {
            graph,
            conds: parsed,
        })
    }

    /// Compute the referentially-intact subset: for every seeded (filtered)
    /// collection, the matching documents, plus all documents transitively
    /// required to satisfy their declared references.
    pub fn compute(&self, source: &dyn DocumentSource) -> BTreeMap<String, Vec<Document>> {
        let mut included: BTreeMap<String, Vec<Document>> = BTreeMap::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut work: Vec<(String, Document)> = Vec::new();

        // Seed from filtered collections.
        for (collection, cond) in &self.conds {
            for doc in source.all(collection) {
                if cond.eval(&doc) {
                    self.admit(collection, doc, &mut included, &mut visited, &mut work);
                }
            }
        }

        // Follow references to their targets (bounded by `visited`).
        while let Some((collection, doc)) = work.pop() {
            for edge in self.graph.references_from(&collection) {
                let target = match edge.resolve_target(&doc) {
                    Some(t) => t,
                    None => continue, // polymorphic with no matching discriminator
                };
                let value = match get_path(&doc, &edge.from_field) {
                    Some(v) => v.clone(),
                    None => continue,
                };
                for target_doc in source.find_by(&target, &edge.to_field, &value) {
                    self.admit(&target, target_doc, &mut included, &mut visited, &mut work);
                }
            }
        }

        included
    }

    fn admit(
        &self,
        collection: &str,
        doc: Document,
        included: &mut BTreeMap<String, Vec<Document>>,
        visited: &mut HashSet<String>,
        work: &mut Vec<(String, Document)>,
    ) {
        let key = format!("{collection}:{}", doc_id(&doc));
        if !visited.insert(key) {
            return; // already included -> bounds cycles.
        }
        included
            .entry(collection.to_string())
            .or_default()
            .push(doc.clone());
        work.push((collection.to_string(), doc));
    }
}

/// A stable identity for a document (its `_id`, or a fallback rendering).
fn doc_id(doc: &Document) -> String {
    match doc.get("_id") {
        Some(v) => v.to_string(),
        None => format!("{doc:?}"),
    }
}

fn get_path<'a>(doc: &'a Document, path: &str) -> Option<&'a Bson> {
    let mut parts = path.split('.');
    let mut current = doc.get(parts.next()?)?;
    for part in parts {
        current = current.as_document()?.get(part)?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subset::VirtualReferenceEntry;

    /// In-memory document store keyed by collection.
    #[derive(Default)]
    struct MemSource {
        data: BTreeMap<String, Vec<Document>>,
    }
    impl MemSource {
        fn insert(&mut self, collection: &str, doc: Document) {
            self.data.entry(collection.to_string()).or_default().push(doc);
        }
    }
    impl DocumentSource for MemSource {
        fn all(&self, collection: &str) -> Vec<Document> {
            self.data.get(collection).cloned().unwrap_or_default()
        }
        fn find_by(&self, collection: &str, field: &str, value: &Bson) -> Vec<Document> {
            self.all(collection)
                .into_iter()
                .filter(|d| get_path(d, field) == Some(value))
                .collect()
        }
    }

    fn doc(pairs: &[(&str, Bson)]) -> Document {
        let mut d = Document::new();
        for (k, v) in pairs {
            d.insert(*k, v.clone());
        }
        d
    }
    fn graph(src: &str) -> ReferenceGraph {
        let entries: Vec<VirtualReferenceEntry> = serde_yaml::from_str(src).unwrap();
        ReferenceGraph::from_entries(&entries)
    }
    fn conds(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    // Acceptance: subset_conds selects only matching docs; referenced docs in
    // related collections are pulled in even without their own filter.
    #[test]
    fn filters_and_pulls_referenced_documents() {
        let g = graph(
            "- collection: orders\n  references:\n    - field: user_id\n      references_collection: users\n",
        );
        let mut src = MemSource::default();
        src.insert("orders", doc(&[("_id", Bson::Int64(1)), ("user_id", Bson::Int64(10)), ("region", Bson::String("EU".into()))]));
        src.insert("orders", doc(&[("_id", Bson::Int64(2)), ("user_id", Bson::Int64(20)), ("region", Bson::String("US".into()))]));
        src.insert("users", doc(&[("_id", Bson::Int64(10)), ("name", Bson::String("eu-user".into()))]));
        src.insert("users", doc(&[("_id", Bson::Int64(20)), ("name", Bson::String("us-user".into()))]));

        let engine = SubsetEngine::new(&g, &conds(&[("orders", "region == 'EU'")])).unwrap();
        let out = engine.compute(&src);

        // only the EU order.
        assert_eq!(out["orders"].len(), 1);
        assert_eq!(out["orders"][0].get_i64("_id").unwrap(), 1);
        // and only the user it references, though users had no filter.
        assert_eq!(out["users"].len(), 1);
        assert_eq!(out["users"][0].get_i64("_id").unwrap(), 10);
    }

    // Acceptance: a cyclic chain of references terminates.
    #[test]
    fn cyclic_references_terminate() {
        // users.manager_id -> users._id (self-cycle).
        let g = graph(
            "- collection: users\n  references:\n    - field: manager_id\n      references_collection: users\n",
        );
        let mut src = MemSource::default();
        // 1 -> 2 -> 1 cycle.
        src.insert("users", doc(&[("_id", Bson::Int64(1)), ("manager_id", Bson::Int64(2)), ("seed", Bson::Boolean(true))]));
        src.insert("users", doc(&[("_id", Bson::Int64(2)), ("manager_id", Bson::Int64(1))]));

        let engine = SubsetEngine::new(&g, &conds(&[("users", "seed == true")])).unwrap();
        let out = engine.compute(&src); // must not hang
        // both users are included exactly once.
        assert_eq!(out["users"].len(), 2);
    }

    // Acceptance: a polymorphic reference resolves to the correct target.
    #[test]
    fn polymorphic_reference_pulls_correct_collection() {
        let g = graph(
            "- collection: comments\n  references:\n    - field: parent_id\n      polymorphic_exprs:\n        - field: parent_type\n          value: post\n          references_collection: posts\n        - field: parent_type\n          value: photo\n          references_collection: photos\n",
        );
        let mut src = MemSource::default();
        src.insert("comments", doc(&[("_id", Bson::Int64(1)), ("parent_id", Bson::Int64(100)), ("parent_type", Bson::String("post".into())), ("keep", Bson::Boolean(true))]));
        src.insert("comments", doc(&[("_id", Bson::Int64(2)), ("parent_id", Bson::Int64(200)), ("parent_type", Bson::String("photo".into())), ("keep", Bson::Boolean(true))]));
        src.insert("posts", doc(&[("_id", Bson::Int64(100))]));
        src.insert("photos", doc(&[("_id", Bson::Int64(200))]));
        src.insert("posts", doc(&[("_id", Bson::Int64(999))])); // unrelated

        let engine = SubsetEngine::new(&g, &conds(&[("comments", "keep == true")])).unwrap();
        let out = engine.compute(&src);

        assert_eq!(out["posts"].len(), 1);
        assert_eq!(out["posts"][0].get_i64("_id").unwrap(), 100);
        assert_eq!(out["photos"].len(), 1);
        assert_eq!(out["photos"][0].get_i64("_id").unwrap(), 200);
    }
}
