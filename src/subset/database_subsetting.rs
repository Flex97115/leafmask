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
///
/// Every method returns a [`Result`]: a read that fails must abort the
/// traversal rather than silently yield a smaller closure, because an
/// incomplete closure is a referentially *broken* subset, not a smaller one.
pub trait DocumentSource {
    /// All documents in a collection.
    fn all(&self, collection: &str) -> Result<Vec<Document>>;
    /// Documents whose `field` equals `value`.
    fn find_by(&self, collection: &str, field: &str, value: &Bson) -> Result<Vec<Document>>;
    /// The documents of `collection` matching `cond` — the traversal's seed.
    /// The default fetches everything and evaluates in memory; a real source
    /// should override to push [`Condition::to_filter`] down to the server so a
    /// narrow condition never reads the whole collection.
    fn seed(&self, collection: &str, cond: &Condition) -> Result<Vec<Document>> {
        Ok(self
            .all(collection)?
            .into_iter()
            .filter(|d| cond.eval(d))
            .collect())
    }
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
    ///
    /// Materializes every included document. On a real dump prefer
    /// [`Self::compute_ids`], which keeps only the identities and lets the dump
    /// stream the documents themselves.
    pub fn compute(&self, source: &dyn DocumentSource) -> Result<BTreeMap<String, Vec<Document>>> {
        let mut included: BTreeMap<String, Vec<Document>> = BTreeMap::new();
        self.traverse(source, &mut |collection, doc| {
            included
                .entry(collection.to_string())
                .or_default()
                .push(doc.clone());
        })?;
        Ok(included)
    }

    /// Compute the same closure as [`Self::compute`] but keep only each
    /// included document's `_id`, so the caller can push an `_id`-membership
    /// filter down to the server and stream the documents instead of holding
    /// them in memory. A document with no `_id` cannot be addressed by such a
    /// filter and is skipped.
    pub fn compute_ids(&self, source: &dyn DocumentSource) -> Result<BTreeMap<String, Vec<Bson>>> {
        let mut ids: BTreeMap<String, Vec<Bson>> = BTreeMap::new();
        self.traverse(source, &mut |collection, doc| {
            if let Some(id) = doc.get("_id") {
                ids.entry(collection.to_string())
                    .or_default()
                    .push(id.clone());
            }
        })?;
        Ok(ids)
    }

    /// Walk the closure, invoking `on_admit` once per included document. Shared
    /// by [`Self::compute`] and [`Self::compute_ids`] so both see exactly the
    /// same traversal, cycle bounding, and polymorphic resolution.
    fn traverse(
        &self,
        source: &dyn DocumentSource,
        on_admit: &mut dyn FnMut(&str, &Document),
    ) -> Result<()> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut work: Vec<(String, Document)> = Vec::new();

        // Seed from filtered collections. `seed` lets a real source push the
        // condition down instead of fetching the collection and discarding it.
        for (collection, cond) in &self.conds {
            for doc in source.seed(collection, cond)? {
                admit(collection, doc, &mut visited, &mut work, on_admit);
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
                for target_doc in source.find_by(&target, &edge.to_field, &value)? {
                    admit(&target, target_doc, &mut visited, &mut work, on_admit);
                }
            }
        }
        Ok(())
    }
}

/// Record a document as included, unless it already was — the visited set is
/// what bounds cyclic reference chains.
fn admit(
    collection: &str,
    doc: Document,
    visited: &mut HashSet<String>,
    work: &mut Vec<(String, Document)>,
    on_admit: &mut dyn FnMut(&str, &Document),
) {
    let key = format!("{collection}:{}", doc_id(&doc));
    if !visited.insert(key) {
        return; // already included -> bounds cycles.
    }
    on_admit(collection, &doc);
    work.push((collection.to_string(), doc));
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
            self.data
                .entry(collection.to_string())
                .or_default()
                .push(doc);
        }
    }
    impl DocumentSource for MemSource {
        fn all(&self, collection: &str) -> Result<Vec<Document>> {
            Ok(self.data.get(collection).cloned().unwrap_or_default())
        }
        fn find_by(&self, collection: &str, field: &str, value: &Bson) -> Result<Vec<Document>> {
            Ok(self
                .all(collection)?
                .into_iter()
                .filter(|d| get_path(d, field) == Some(value))
                .collect())
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
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // Acceptance: subset_conds selects only matching docs; referenced docs in
    // related collections are pulled in even without their own filter.
    #[test]
    fn filters_and_pulls_referenced_documents() {
        let g = graph(
            "- collection: orders\n  references:\n    - field: user_id\n      references_collection: users\n",
        );
        let mut src = MemSource::default();
        src.insert(
            "orders",
            doc(&[
                ("_id", Bson::Int64(1)),
                ("user_id", Bson::Int64(10)),
                ("region", Bson::String("EU".into())),
            ]),
        );
        src.insert(
            "orders",
            doc(&[
                ("_id", Bson::Int64(2)),
                ("user_id", Bson::Int64(20)),
                ("region", Bson::String("US".into())),
            ]),
        );
        src.insert(
            "users",
            doc(&[
                ("_id", Bson::Int64(10)),
                ("name", Bson::String("eu-user".into())),
            ]),
        );
        src.insert(
            "users",
            doc(&[
                ("_id", Bson::Int64(20)),
                ("name", Bson::String("us-user".into())),
            ]),
        );

        let engine = SubsetEngine::new(&g, &conds(&[("orders", "region == 'EU'")])).unwrap();
        let out = engine.compute(&src).unwrap();

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
        src.insert(
            "users",
            doc(&[
                ("_id", Bson::Int64(1)),
                ("manager_id", Bson::Int64(2)),
                ("seed", Bson::Boolean(true)),
            ]),
        );
        src.insert(
            "users",
            doc(&[("_id", Bson::Int64(2)), ("manager_id", Bson::Int64(1))]),
        );

        let engine = SubsetEngine::new(&g, &conds(&[("users", "seed == true")])).unwrap();
        let out = engine.compute(&src).unwrap(); // must not hang
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
        src.insert(
            "comments",
            doc(&[
                ("_id", Bson::Int64(1)),
                ("parent_id", Bson::Int64(100)),
                ("parent_type", Bson::String("post".into())),
                ("keep", Bson::Boolean(true)),
            ]),
        );
        src.insert(
            "comments",
            doc(&[
                ("_id", Bson::Int64(2)),
                ("parent_id", Bson::Int64(200)),
                ("parent_type", Bson::String("photo".into())),
                ("keep", Bson::Boolean(true)),
            ]),
        );
        src.insert("posts", doc(&[("_id", Bson::Int64(100))]));
        src.insert("photos", doc(&[("_id", Bson::Int64(200))]));
        src.insert("posts", doc(&[("_id", Bson::Int64(999))])); // unrelated

        let engine = SubsetEngine::new(&g, &conds(&[("comments", "keep == true")])).unwrap();
        let out = engine.compute(&src).unwrap();

        assert_eq!(out["posts"].len(), 1);
        assert_eq!(out["posts"][0].get_i64("_id").unwrap(), 100);
        assert_eq!(out["photos"].len(), 1);
        assert_eq!(out["photos"][0].get_i64("_id").unwrap(), 200);
    }

    // Acceptance: compute_ids yields exactly the identities compute() would
    // include — same closure, same cycle bounding — so a dump can push an
    // `_id`-membership filter down instead of materializing the documents.
    #[test]
    fn compute_ids_matches_compute_but_keeps_only_identities() {
        let g = graph(
            "- collection: orders\n  references:\n    - field: user_id\n      references_collection: users\n",
        );
        let mut src = MemSource::default();
        src.insert(
            "orders",
            doc(&[
                ("_id", Bson::Int64(1)),
                ("user_id", Bson::Int64(10)),
                ("region", Bson::String("EU".into())),
            ]),
        );
        src.insert(
            "orders",
            doc(&[
                ("_id", Bson::Int64(2)),
                ("user_id", Bson::Int64(20)),
                ("region", Bson::String("US".into())),
            ]),
        );
        src.insert("users", doc(&[("_id", Bson::Int64(10))]));
        src.insert("users", doc(&[("_id", Bson::Int64(20))]));

        let engine = SubsetEngine::new(&g, &conds(&[("orders", "region == 'EU'")])).unwrap();
        let ids = engine.compute_ids(&src).unwrap();
        let docs = engine.compute(&src).unwrap();

        assert_eq!(ids["orders"], vec![Bson::Int64(1)]);
        assert_eq!(ids["users"], vec![Bson::Int64(10)]);
        // identical closure, just projected to `_id`.
        for (collection, documents) in &docs {
            let from_docs: Vec<Bson> = documents
                .iter()
                .map(|d| d.get("_id").unwrap().clone())
                .collect();
            assert_eq!(&from_docs, &ids[collection], "collection {collection}");
        }
    }

    // A document with no `_id` cannot be addressed by an `_id` filter, so it is
    // left out of the id closure rather than silently widening the dump.
    #[test]
    fn compute_ids_skips_documents_without_an_id() {
        let g = graph("- collection: events\n  references: []\n");
        let mut src = MemSource::default();
        src.insert("events", doc(&[("keep", Bson::Boolean(true))]));
        src.insert(
            "events",
            doc(&[("_id", Bson::Int64(1)), ("keep", Bson::Boolean(true))]),
        );

        let engine = SubsetEngine::new(&g, &conds(&[("events", "keep == true")])).unwrap();
        let ids = engine.compute_ids(&src).unwrap();
        assert_eq!(ids["events"], vec![Bson::Int64(1)]);
        // both were traversed, only the addressable one is reported.
        assert_eq!(engine.compute(&src).unwrap()["events"].len(), 2);
    }

    /// A source whose reference lookups always fail, to prove a read error
    /// aborts the traversal.
    struct FailingSource;
    impl DocumentSource for FailingSource {
        fn all(&self, _collection: &str) -> Result<Vec<Document>> {
            Ok(vec![doc(&[
                ("_id", Bson::Int64(1)),
                ("user_id", Bson::Int64(10)),
                ("keep", Bson::Boolean(true)),
            ])])
        }
        fn find_by(&self, _c: &str, _f: &str, _v: &Bson) -> Result<Vec<Document>> {
            Err(crate::Error::Mongo("connection lost".into()))
        }
    }

    // A failed read must abort the traversal: an incomplete closure is a
    // referentially BROKEN subset, not merely a smaller one, so it must never
    // be handed back as if it were a valid result.
    #[test]
    fn read_failure_aborts_instead_of_yielding_a_broken_closure() {
        let g = graph(
            "- collection: orders\n  references:\n    - field: user_id\n      references_collection: users\n",
        );
        let engine = SubsetEngine::new(&g, &conds(&[("orders", "keep == true")])).unwrap();
        let err = engine.compute_ids(&FailingSource).unwrap_err();
        assert!(err.to_string().contains("connection lost"), "{err}");
        assert!(engine.compute(&FailingSource).is_err());
    }

    /// A source that records whether the seed was pushed down or evaluated
    /// in memory.
    struct SeedSpy {
        inner: MemSource,
        pushed_down: std::cell::Cell<bool>,
    }
    impl DocumentSource for SeedSpy {
        fn all(&self, collection: &str) -> Result<Vec<Document>> {
            self.inner.all(collection)
        }
        fn find_by(&self, collection: &str, field: &str, value: &Bson) -> Result<Vec<Document>> {
            self.inner.find_by(collection, field, value)
        }
        fn seed(&self, collection: &str, cond: &Condition) -> Result<Vec<Document>> {
            self.pushed_down.set(true);
            // stand-in for a server-side filter: translate and match manually.
            let _ = cond.to_filter();
            Ok(self
                .inner
                .all(collection)?
                .into_iter()
                .filter(|d| cond.eval(d))
                .collect())
        }
    }

    // The traversal must seed through `seed`, so a real source can push the
    // condition to the server instead of reading the whole collection back.
    #[test]
    fn seeding_goes_through_the_overridable_seed_hook() {
        let g = graph("- collection: orders\n  references: []\n");
        let mut inner = MemSource::default();
        inner.insert(
            "orders",
            doc(&[
                ("_id", Bson::Int64(1)),
                ("region", Bson::String("EU".into())),
            ]),
        );
        inner.insert(
            "orders",
            doc(&[
                ("_id", Bson::Int64(2)),
                ("region", Bson::String("US".into())),
            ]),
        );
        let src = SeedSpy {
            inner,
            pushed_down: std::cell::Cell::new(false),
        };

        let engine = SubsetEngine::new(&g, &conds(&[("orders", "region == 'EU'")])).unwrap();
        let ids = engine.compute_ids(&src).unwrap();
        assert!(src.pushed_down.get(), "seed() was bypassed");
        assert_eq!(ids["orders"], vec![Bson::Int64(1)]);
    }
}
