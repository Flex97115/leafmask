//! A [`DocumentSource`] backed by a real [`MongoSource`], so the subsetting
//! engine's closure (feature `subset.database-subsetting`) can be computed
//! against a live deployment.
//!
//! Two things keep this pass cheap, and both matter on a production-sized
//! database:
//!   * conditions and reference lookups are pushed down as MongoDB filters, so
//!     the server does the narrowing instead of every document being fetched
//!     and discarded;
//!   * only `_id` and the fields that actually carry a reference are
//!     projected, so the pass costs a few fields per document rather than the
//!     documents themselves — which is what makes it safe to run before a
//!     streaming dump.

use std::collections::BTreeMap;

use bson::{Bson, Document};

use crate::error::Result;
use crate::mongo::MongoSource;
use crate::transform::condition::Condition;

use super::database_subsetting::DocumentSource;
use super::ReferenceGraph;

/// Reads the subsetting closure's documents out of one database of a live
/// deployment.
pub struct MongoDocumentSource<'a> {
    source: &'a dyn MongoSource,
    database: &'a str,
    /// Per collection, the fields the traversal needs: `_id` plus every field
    /// declared as carrying a reference, and every discriminator a polymorphic
    /// reference resolves on.
    projections: BTreeMap<String, Vec<String>>,
}

impl<'a> MongoDocumentSource<'a> {
    /// Build a source over `database`, projecting exactly what `graph` needs.
    pub fn new(source: &'a dyn MongoSource, database: &'a str, graph: &ReferenceGraph) -> Self {
        let mut projections: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for edge in graph.edges() {
            let fields = projections
                .entry(edge.from_collection.clone())
                .or_insert_with(|| vec!["_id".to_string()]);
            fields.push(edge.from_field.clone());
            // A polymorphic edge decides its target from a discriminator field,
            // so that field has to come back too or the target never resolves.
            if let super::RefTarget::Polymorphic(cases) = &edge.target {
                for (discriminator, _, _) in cases {
                    fields.push(discriminator.clone());
                }
            }
            // Targets are looked up by their referenced field, and admitted
            // documents are themselves traversed, so the target collection
            // needs at least its identity projected.
            for target in edge.target_collections() {
                let target_fields = projections
                    .entry(target)
                    .or_insert_with(|| vec!["_id".to_string()]);
                target_fields.push(edge.to_field.clone());
            }
        }
        for fields in projections.values_mut() {
            fields.sort();
            fields.dedup();
        }
        MongoDocumentSource {
            source,
            database,
            projections,
        }
    }

    /// The fields to project for `collection`. An empty list means "no
    /// projection" — the whole document — which is what a collection with no
    /// declared reference gets.
    fn projection(&self, collection: &str) -> &[String] {
        self.projections
            .get(collection)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn collect(&self, collection: &str, filter: Option<&Document>) -> Result<Vec<Document>> {
        let mut out = Vec::new();
        self.source.stream_documents_projected(
            self.database,
            collection,
            filter,
            self.projection(collection),
            &mut |doc| {
                out.push(doc);
                Ok(())
            },
        )?;
        Ok(out)
    }
}

impl DocumentSource for MongoDocumentSource<'_> {
    fn all(&self, collection: &str) -> Result<Vec<Document>> {
        self.collect(collection, None)
    }

    fn find_by(&self, collection: &str, field: &str, value: &Bson) -> Result<Vec<Document>> {
        let mut filter = Document::new();
        filter.insert(field, value.clone());
        self.collect(collection, Some(&filter))
    }

    fn seed(&self, collection: &str, cond: &Condition) -> Result<Vec<Document>> {
        // Pushed down as a real MongoDB query rather than fetched and filtered
        // in Rust: a narrow seed condition must not cost a full collection scan
        // returned over the wire.
        self.collect(collection, Some(&cond.to_filter()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mongo::{CollectionData, InMemoryMongo};
    use crate::subset::{SubsetEngine, VirtualReferenceEntry};
    use std::sync::Mutex;

    fn graph(src: &str) -> ReferenceGraph {
        let entries: Vec<VirtualReferenceEntry> = serde_yaml::from_str(src).unwrap();
        ReferenceGraph::from_entries(&entries)
    }

    fn doc(pairs: &[(&str, Bson)]) -> Document {
        let mut d = Document::new();
        for (k, v) in pairs {
            d.insert(*k, v.clone());
        }
        d
    }

    fn seeded() -> InMemoryMongo {
        let m = InMemoryMongo::new();
        m.seed(CollectionData {
            database: "shop".into(),
            collection: "orders".into(),
            documents: vec![
                doc(&[
                    ("_id", Bson::Int64(1)),
                    ("user_id", Bson::Int64(10)),
                    ("region", Bson::String("EU".into())),
                ]),
                doc(&[
                    ("_id", Bson::Int64(2)),
                    ("user_id", Bson::Int64(20)),
                    ("region", Bson::String("US".into())),
                ]),
            ],
            ..Default::default()
        });
        m.seed(CollectionData {
            database: "shop".into(),
            collection: "users".into(),
            documents: vec![
                doc(&[("_id", Bson::Int64(10))]),
                doc(&[("_id", Bson::Int64(20))]),
            ],
            ..Default::default()
        });
        m
    }

    // The closure computed against a Mongo-backed source is the same one the
    // in-memory engine tests assert: the filtered orders plus exactly the users
    // they reference.
    #[test]
    fn computes_the_closure_against_a_mongo_source() {
        let g = graph(
            "- collection: orders\n  references:\n    - field: user_id\n      references_collection: users\n",
        );
        let m = seeded();
        let src = MongoDocumentSource::new(&m, "shop", &g);
        let conds: BTreeMap<String, String> =
            [("orders".to_string(), "region == 'EU'".to_string())]
                .into_iter()
                .collect();

        let ids = SubsetEngine::new(&g, &conds)
            .unwrap()
            .compute_ids(&src)
            .unwrap();
        assert_eq!(ids["orders"], vec![Bson::Int64(1)]);
        assert_eq!(ids["users"], vec![Bson::Int64(10)]);
    }

    // The projection must ask for `_id`, every reference-bearing field, and any
    // polymorphic discriminator — and nothing else. Projecting too little
    // breaks the traversal silently; projecting everything defeats the point of
    // the pass being cheap.
    #[test]
    fn projects_only_identity_reference_and_discriminator_fields() {
        let g = graph(
            "- collection: comments\n  references:\n    - field: parent_id\n      polymorphic_exprs:\n        - field: parent_type\n          value: post\n          references_collection: posts\n",
        );
        let m = InMemoryMongo::new();
        let src = MongoDocumentSource::new(&m, "blog", &g);

        assert_eq!(
            src.projection("comments"),
            ["_id", "parent_id", "parent_type"]
        );
        // the target collection is looked up (and traversed) by its referenced
        // field, which defaults to `_id`.
        assert_eq!(src.projection("posts"), ["_id"]);
        // a collection carrying no declared reference is not projected at all.
        assert!(src.projection("unrelated").is_empty());
    }

    /// Records every (filter, projection) pair handed to the source, to prove
    /// the narrowing happens server-side rather than in Rust.
    struct RecordingSource {
        inner: InMemoryMongo,
        calls: Mutex<Vec<(Option<Document>, Vec<String>)>>,
    }
    impl MongoSource for RecordingSource {
        fn databases(&self) -> Result<Vec<String>> {
            self.inner.databases()
        }
        fn collections(&self, database: &str) -> Result<Vec<String>> {
            self.inner.collections(database)
        }
        fn read_collection(&self, database: &str, collection: &str) -> Result<CollectionData> {
            self.inner.read_collection(database, collection)
        }
        fn stream_documents_projected(
            &self,
            database: &str,
            collection: &str,
            filter: Option<&Document>,
            projection: &[String],
            f: &mut dyn FnMut(Document) -> Result<()>,
        ) -> Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push((filter.cloned(), projection.to_vec()));
            self.inner
                .stream_documents_projected(database, collection, filter, projection, f)
        }
    }

    // The seed condition must reach the source as a native MongoDB filter, and
    // a reference lookup as an equality filter on the referenced field — never
    // as an unfiltered read the engine narrows afterwards.
    #[test]
    fn pushes_the_condition_and_reference_lookups_down() {
        let g = graph(
            "- collection: orders\n  references:\n    - field: user_id\n      references_collection: users\n",
        );
        let recording = RecordingSource {
            inner: seeded(),
            calls: Mutex::new(Vec::new()),
        };
        let src = MongoDocumentSource::new(&recording, "shop", &g);
        let conds: BTreeMap<String, String> =
            [("orders".to_string(), "region == 'EU'".to_string())]
                .into_iter()
                .collect();
        SubsetEngine::new(&g, &conds)
            .unwrap()
            .compute_ids(&src)
            .unwrap();

        let calls = recording.calls.lock().unwrap();
        // seed: the condition itself, translated.
        let mut expected_seed = Document::new();
        expected_seed.insert("region", "EU");
        assert_eq!(calls[0].0.as_ref(), Some(&expected_seed));
        assert_eq!(calls[0].1, ["_id", "user_id"]);
        // reference lookup: equality on the referenced field.
        let mut expected_lookup = Document::new();
        expected_lookup.insert("_id", Bson::Int64(10));
        assert_eq!(calls[1].0.as_ref(), Some(&expected_lookup));
        // nothing was ever read unfiltered.
        assert!(
            calls.iter().all(|(filter, _)| filter.is_some()),
            "an unfiltered read reached the source: {calls:?}"
        );
    }
}
