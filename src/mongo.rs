//! MongoDB access abstraction.
//!
//! Dump and restore access MongoDB through these traits rather than a wrapped
//! `mongodump`/`mongorestore`, so per-document transformation can be applied
//! inline while streaming. The traits keep the dump/restore logic testable with
//! an in-memory fake ([`InMemoryMongo`]); the real `mongodb`-driver
//! implementation ([`MongoDriver`]) lives behind the `mongo` cargo feature.

use std::collections::BTreeMap;
use std::sync::Mutex;

use bson::{Bson, Document};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::restore::InsertError;
use crate::validate::IndexSpec;

/// A collection's data and structure, as read from or written to MongoDB.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CollectionData {
    pub database: String,
    pub collection: String,
    pub documents: Vec<Document>,
    pub indexes: Vec<IndexSpec>,
    pub validator: Option<Bson>,
    pub options: BTreeMap<String, Bson>,
}

/// A collection's structure (everything but the documents).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CollectionStructure {
    pub indexes: Vec<IndexSpec>,
    pub validator: Option<Bson>,
    pub options: BTreeMap<String, Bson>,
}

/// A read side: enumerate and read collections (for dump / validate).
pub trait MongoSource {
    fn databases(&self) -> Result<Vec<String>>;
    fn collections(&self, database: &str) -> Result<Vec<String>>;
    fn read_collection(&self, database: &str, collection: &str) -> Result<CollectionData>;

    /// Like [`Self::read_collection`], but pushes `filter` down to the source
    /// when it can (a real MongoDB query) instead of fetching every document
    /// and filtering afterward. The default ignores `filter` and delegates.
    fn read_collection_filtered(
        &self,
        database: &str,
        collection: &str,
        filter: Option<&Document>,
    ) -> Result<CollectionData> {
        let _ = filter;
        self.read_collection(database, collection)
    }

    /// The collection's structure (indexes, validator, options) without its
    /// documents. The default materializes the collection; real sources should
    /// override to avoid reading any documents.
    fn read_structure(&self, database: &str, collection: &str) -> Result<CollectionStructure> {
        let data = self.read_collection(database, collection)?;
        Ok(CollectionStructure {
            indexes: data.indexes,
            validator: data.validator,
            options: data.options,
        })
    }

    /// Stream the collection's documents (matching `filter` when the source
    /// supports pushdown), invoking `f` once per document. This is the dump's
    /// read path: memory stays bounded regardless of collection size. The
    /// default materializes via [`Self::read_collection_filtered`].
    fn stream_documents(
        &self,
        database: &str,
        collection: &str,
        filter: Option<&Document>,
        f: &mut dyn FnMut(Document) -> Result<()>,
    ) -> Result<()> {
        for doc in self
            .read_collection_filtered(database, collection, filter)?
            .documents
        {
            f(doc)?;
        }
        Ok(())
    }

    /// Read at most `limit` documents (for previews). The default materializes
    /// then truncates; real sources should push the limit down.
    fn read_sample(&self, database: &str, collection: &str, limit: usize) -> Result<Vec<Document>> {
        let mut docs = self.read_collection(database, collection)?.documents;
        docs.truncate(limit);
        Ok(docs)
    }
}

/// Outcome of a batched insert: how many documents went in, and which failed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BatchInsert {
    pub inserted: u64,
    /// Failed documents as (index within the batch, error). With `ordered`
    /// writes the batch stops at the first failure, so documents after that
    /// index were not attempted.
    pub failures: Vec<(usize, InsertError)>,
}

/// A write side: create collections/indexes and insert documents (for restore).
pub trait MongoSink {
    /// Create the collection if needed, applying validator and options.
    fn ensure_collection(
        &self,
        database: &str,
        collection: &str,
        validator: &Option<Bson>,
        options: &BTreeMap<String, Bson>,
    ) -> Result<()>;

    /// Insert one document. A failure is reported as an [`InsertError`] so the
    /// restore driver can consult the configured error exclusions.
    fn insert(
        &self,
        database: &str,
        collection: &str,
        doc: &Document,
    ) -> std::result::Result<(), InsertError>;

    /// Insert a batch of documents in one server round-trip where the backend
    /// supports it — the restore hot path. `ordered` stops the batch at its
    /// first failure; unordered attempts every document and reports all
    /// failures. Per-document failures come back in the [`BatchInsert`] (they
    /// are not an `Err`); `Err` is reserved for whole-batch failures like a
    /// lost connection. The default falls back to per-document [`Self::insert`].
    fn insert_many(
        &self,
        database: &str,
        collection: &str,
        docs: &[Document],
        ordered: bool,
    ) -> Result<BatchInsert> {
        let mut out = BatchInsert::default();
        for (i, doc) in docs.iter().enumerate() {
            match self.insert(database, collection, doc) {
                Ok(()) => out.inserted += 1,
                Err(e) => {
                    out.failures.push((i, e));
                    if ordered {
                        break;
                    }
                }
            }
        }
        Ok(out)
    }

    /// Create an index on a collection.
    fn create_index(&self, database: &str, collection: &str, index: &IndexSpec) -> Result<()>;
}

/// An in-memory MongoDB stand-in implementing both source and sink, used by the
/// dump and restore unit tests. Enforces unique `_id` so duplicate-key errors
/// (the common restore case) can be exercised.
#[derive(Default)]
pub struct InMemoryMongo {
    inner: Mutex<BTreeMap<(String, String), CollectionData>>,
}

impl InMemoryMongo {
    pub fn new() -> Self {
        InMemoryMongo::default()
    }

    /// Seed a collection (for use as a dump source or a restore target).
    pub fn seed(&self, data: CollectionData) {
        self.inner
            .lock()
            .unwrap()
            .insert((data.database.clone(), data.collection.clone()), data);
    }

    /// Snapshot a collection's current documents (for assertions).
    pub fn documents(&self, database: &str, collection: &str) -> Vec<Document> {
        self.inner
            .lock()
            .unwrap()
            .get(&(database.to_string(), collection.to_string()))
            .map(|c| c.documents.clone())
            .unwrap_or_default()
    }
}

impl MongoSource for InMemoryMongo {
    fn databases(&self) -> Result<Vec<String>> {
        let mut dbs: Vec<String> = self
            .inner
            .lock()
            .unwrap()
            .keys()
            .map(|(db, _)| db.clone())
            .collect();
        dbs.sort();
        dbs.dedup();
        Ok(dbs)
    }

    fn collections(&self, database: &str) -> Result<Vec<String>> {
        let mut cols: Vec<String> = self
            .inner
            .lock()
            .unwrap()
            .keys()
            .filter(|(db, _)| db == database)
            .map(|(_, c)| c.clone())
            .collect();
        cols.sort();
        Ok(cols)
    }

    fn read_collection(&self, database: &str, collection: &str) -> Result<CollectionData> {
        self.inner
            .lock()
            .unwrap()
            .get(&(database.to_string(), collection.to_string()))
            .cloned()
            .ok_or_else(|| {
                crate::Error::Mongo(format!("collection {database}.{collection} not found"))
            })
    }
}

impl MongoSink for InMemoryMongo {
    fn ensure_collection(
        &self,
        database: &str,
        collection: &str,
        validator: &Option<Bson>,
        options: &BTreeMap<String, Bson>,
    ) -> Result<()> {
        let mut guard = self.inner.lock().unwrap();
        guard
            .entry((database.to_string(), collection.to_string()))
            .or_insert_with(|| CollectionData {
                database: database.to_string(),
                collection: collection.to_string(),
                validator: validator.clone(),
                options: options.clone(),
                ..Default::default()
            });
        Ok(())
    }

    fn insert(
        &self,
        database: &str,
        collection: &str,
        doc: &Document,
    ) -> std::result::Result<(), InsertError> {
        let mut guard = self.inner.lock().unwrap();
        let entry = guard
            .entry((database.to_string(), collection.to_string()))
            .or_insert_with(|| CollectionData {
                database: database.to_string(),
                collection: collection.to_string(),
                ..Default::default()
            });
        if let Some(id) = doc.get("_id") {
            if entry.documents.iter().any(|d| d.get("_id") == Some(id)) {
                return Err(InsertError::duplicate_key("_id_"));
            }
        }
        entry.documents.push(doc.clone());
        Ok(())
    }

    fn create_index(&self, database: &str, collection: &str, index: &IndexSpec) -> Result<()> {
        let mut guard = self.inner.lock().unwrap();
        let entry = guard
            .entry((database.to_string(), collection.to_string()))
            .or_insert_with(|| CollectionData {
                database: database.to_string(),
                collection: collection.to_string(),
                ..Default::default()
            });
        if !entry.indexes.iter().any(|i| i.name == index.name) {
            entry.indexes.push(index.clone());
        }
        Ok(())
    }
}

#[cfg(feature = "mongo")]
pub use driver::MongoDriver;

/// The real MongoDB adapter, backed by the async `mongodb` driver and driven
/// from the synchronous traits via a dedicated Tokio runtime. Compiled only with
/// the `mongo` feature.
///
/// Note: reads use a plain `find` (point-in-time snapshot sessions require a
/// replica set); against a replica set this could be upgraded to a snapshot
/// read concern.
#[cfg(feature = "mongo")]
mod driver {
    use super::*;
    use crate::error::Error;
    use mongodb::options::IndexOptions;
    use mongodb::{Client, IndexModel};
    use std::future::IntoFuture;
    use tokio::runtime::Runtime;

    pub struct MongoDriver {
        client: Client,
        rt: Runtime,
    }

    impl MongoDriver {
        /// Connect to a MongoDB deployment by URI (e.g.
        /// `mongodb://localhost:27017`).
        pub fn connect(uri: &str) -> Result<Self> {
            let rt = Runtime::new().map_err(|e| Error::Mongo(e.to_string()))?;
            let client = rt
                .block_on(Client::with_uri_str(uri))
                .map_err(|e| Error::Mongo(format!("connect: {e}")))?;
            Ok(MongoDriver { client, rt })
        }

        fn coll(&self, db: &str, coll: &str) -> mongodb::Collection<Document> {
            self.client.database(db).collection::<Document>(coll)
        }

        /// Drop an entire database (used for test isolation and cleanup).
        pub fn drop_database(&self, db: &str) -> Result<()> {
            self.rt
                .block_on(self.client.database(db).drop().into_future())
                .map_err(|e| Error::Mongo(format!("drop database: {e}")))
        }
    }

    impl MongoSource for MongoDriver {
        fn databases(&self) -> Result<Vec<String>> {
            let names = self
                .rt
                .block_on(self.client.list_database_names().into_future())
                .map_err(|e| Error::Mongo(format!("listDatabases: {e}")))?;
            Ok(names
                .into_iter()
                .filter(|d| !matches!(d.as_str(), "admin" | "config" | "local"))
                .collect())
        }

        fn collections(&self, database: &str) -> Result<Vec<String>> {
            self.rt
                .block_on(
                    self.client
                        .database(database)
                        .list_collection_names()
                        .into_future(),
                )
                .map_err(|e| Error::Mongo(format!("listCollections: {e}")))
        }

        fn read_collection(&self, database: &str, collection: &str) -> Result<CollectionData> {
            self.read_collection_filtered(database, collection, None)
        }

        fn read_collection_filtered(
            &self,
            database: &str,
            collection: &str,
            filter: Option<&Document>,
        ) -> Result<CollectionData> {
            let structure = self.read_structure(database, collection)?;
            let mut documents = Vec::new();
            self.stream_documents(database, collection, filter, &mut |doc| {
                documents.push(doc);
                Ok(())
            })?;
            Ok(CollectionData {
                database: database.to_string(),
                collection: collection.to_string(),
                documents,
                indexes: structure.indexes,
                validator: structure.validator,
                options: structure.options,
            })
        }

        fn read_structure(&self, database: &str, collection: &str) -> Result<CollectionStructure> {
            let db = self.client.database(database);
            let coll = self.coll(database, collection);
            self.rt.block_on(async {
                // Indexes.
                let mut indexes = Vec::new();
                if let Ok(mut idx_cursor) = coll.list_indexes().await {
                    while idx_cursor
                        .advance()
                        .await
                        .map_err(|e| Error::Mongo(e.to_string()))?
                    {
                        let model = idx_cursor
                            .deserialize_current()
                            .map_err(|e| Error::Mongo(e.to_string()))?;
                        let keys: Vec<(String, i32)> = model
                            .keys
                            .iter()
                            .map(|(k, v)| {
                                (
                                    k.clone(),
                                    v.as_i32()
                                        .or_else(|| v.as_i64().map(|n| n as i32))
                                        .unwrap_or(1),
                                )
                            })
                            .collect();
                        let name = model
                            .options
                            .as_ref()
                            .and_then(|o| o.name.clone())
                            .unwrap_or_else(|| default_index_name(&keys));
                        let unique = model
                            .options
                            .as_ref()
                            .and_then(|o| o.unique)
                            .unwrap_or(false);
                        indexes.push(IndexSpec { name, keys, unique });
                    }
                }

                // Validator + collection options via listCollections.
                let (validator, options) = read_options(&db, collection).await?;

                Ok(CollectionStructure {
                    indexes,
                    validator,
                    options,
                })
            })
        }

        fn stream_documents(
            &self,
            database: &str,
            collection: &str,
            filter: Option<&Document>,
            f: &mut dyn FnMut(Document) -> Result<()>,
        ) -> Result<()> {
            let coll = self.coll(database, collection);
            let query_filter = filter.cloned().unwrap_or_default();
            self.rt.block_on(async {
                // `query_filter` is pushed to MongoDB directly (rather than
                // fetching everything and filtering in Rust) so a narrow
                // subset_conds/query doesn't require loading the whole
                // collection; the cursor hands documents over one at a time.
                let mut cursor = coll
                    .find(query_filter)
                    .await
                    .map_err(|e| Error::Mongo(format!("find: {e}")))?;
                while cursor
                    .advance()
                    .await
                    .map_err(|e| Error::Mongo(e.to_string()))?
                {
                    let doc = cursor
                        .deserialize_current()
                        .map_err(|e| Error::Mongo(e.to_string()))?;
                    f(doc)?;
                }
                Ok(())
            })
        }

        fn read_sample(
            &self,
            database: &str,
            collection: &str,
            limit: usize,
        ) -> Result<Vec<Document>> {
            let coll = self.coll(database, collection);
            self.rt.block_on(async {
                let mut cursor = coll
                    .find(Document::new())
                    .limit(limit as i64)
                    .await
                    .map_err(|e| Error::Mongo(format!("find: {e}")))?;
                let mut documents = Vec::new();
                while cursor
                    .advance()
                    .await
                    .map_err(|e| Error::Mongo(e.to_string()))?
                {
                    documents.push(
                        cursor
                            .deserialize_current()
                            .map_err(|e| Error::Mongo(e.to_string()))?,
                    );
                }
                Ok(documents)
            })
        }
    }

    impl MongoSink for MongoDriver {
        fn ensure_collection(
            &self,
            database: &str,
            collection: &str,
            validator: &Option<Bson>,
            options: &BTreeMap<String, Bson>,
        ) -> Result<()> {
            let db = self.client.database(database);
            let mut cmd = bson::doc! { "create": collection };
            if let Some(v) = validator {
                cmd.insert("validator", v.clone());
            }
            for (k, v) in options {
                cmd.insert(k.clone(), v.clone());
            }
            match self.rt.block_on(db.run_command(cmd).into_future()) {
                Ok(_) => Ok(()),
                // NamespaceExists (48) — collection already there, fine.
                Err(e) if error_code(&e) == Some(48) => Ok(()),
                Err(e) => Err(Error::Mongo(format!("create collection: {e}"))),
            }
        }

        fn insert(
            &self,
            database: &str,
            collection: &str,
            doc: &Document,
        ) -> std::result::Result<(), InsertError> {
            let coll = self.coll(database, collection);
            match self.rt.block_on(coll.insert_one(doc.clone()).into_future()) {
                Ok(_) => Ok(()),
                Err(e) => Err(map_insert_error(&e)),
            }
        }

        fn insert_many(
            &self,
            database: &str,
            collection: &str,
            docs: &[Document],
            ordered: bool,
        ) -> Result<BatchInsert> {
            use mongodb::error::ErrorKind;

            if docs.is_empty() {
                return Ok(BatchInsert::default());
            }
            let coll = self.coll(database, collection);
            match self.rt.block_on(
                coll.insert_many(docs.to_vec())
                    .ordered(ordered)
                    .into_future(),
            ) {
                Ok(res) => Ok(BatchInsert {
                    inserted: res.inserted_ids.len() as u64,
                    failures: Vec::new(),
                }),
                Err(e) => match e.kind.as_ref() {
                    ErrorKind::InsertMany(ime) if ime.write_errors.is_some() => {
                        let failures: Vec<(usize, InsertError)> = ime
                            .write_errors
                            .as_deref()
                            .unwrap_or_default()
                            .iter()
                            .map(|we| {
                                (
                                    we.index,
                                    InsertError {
                                        code: Some(we.code),
                                        index_name: index_name_from_message(&we.message),
                                    },
                                )
                            })
                            .collect();
                        // Ordered writes stop at the first failure: everything
                        // before it went in. Unordered attempts every document.
                        let inserted = if ordered {
                            failures.first().map(|(i, _)| *i).unwrap_or(0)
                        } else {
                            docs.len() - failures.len()
                        };
                        Ok(BatchInsert {
                            inserted: inserted as u64,
                            failures,
                        })
                    }
                    _ => Err(Error::Mongo(format!("insert_many: {e}"))),
                },
            }
        }

        fn create_index(&self, database: &str, collection: &str, index: &IndexSpec) -> Result<()> {
            let coll = self.coll(database, collection);
            let mut keys = Document::new();
            for (field, dir) in &index.keys {
                keys.insert(field.clone(), *dir);
            }
            let opts = IndexOptions::builder()
                .name(index.name.clone())
                .unique(index.unique)
                .build();
            let model = IndexModel::builder().keys(keys).options(opts).build();
            self.rt
                .block_on(coll.create_index(model).into_future())
                .map(|_| ())
                .map_err(|e| Error::Mongo(format!("create index: {e}")))
        }
    }

    fn default_index_name(keys: &[(String, i32)]) -> String {
        keys.iter()
            .map(|(k, v)| format!("{k}_{v}"))
            .collect::<Vec<_>>()
            .join("_")
    }

    async fn read_options(
        db: &mongodb::Database,
        collection: &str,
    ) -> Result<(Option<Bson>, BTreeMap<String, Bson>)> {
        let cmd = bson::doc! {
            "listCollections": 1.0,
            "filter": { "name": collection },
        };
        let result = db
            .run_command(cmd)
            .await
            .map_err(|e| Error::Mongo(format!("listCollections: {e}")))?;
        let mut validator = None;
        let mut options = BTreeMap::new();
        if let Ok(cursor) = result.get_document("cursor") {
            if let Ok(batch) = cursor.get_array("firstBatch") {
                if let Some(first) = batch.first().and_then(|b| b.as_document()) {
                    if let Ok(opts) = first.get_document("options") {
                        for (k, v) in opts {
                            if k == "validator" {
                                validator = Some(v.clone());
                            } else {
                                options.insert(k.clone(), v.clone());
                            }
                        }
                    }
                }
            }
        }
        Ok((validator, options))
    }

    fn error_code(e: &mongodb::error::Error) -> Option<i32> {
        use mongodb::error::{ErrorKind, WriteFailure};
        match e.kind.as_ref() {
            ErrorKind::Command(cmd) => Some(cmd.code),
            ErrorKind::Write(WriteFailure::WriteError(we)) => Some(we.code),
            _ => None,
        }
    }

    fn map_insert_error(e: &mongodb::error::Error) -> InsertError {
        use mongodb::error::{ErrorKind, WriteFailure};
        let (code, message) = match e.kind.as_ref() {
            ErrorKind::Write(WriteFailure::WriteError(we)) => (Some(we.code), we.message.clone()),
            other => (None, other.to_string()),
        };
        InsertError {
            code,
            index_name: index_name_from_message(&message),
        }
    }

    /// Extract the offending index name from an E11000 message when present.
    fn index_name_from_message(message: &str) -> Option<String> {
        message
            .split("index:")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .map(|s| s.to_string())
    }
}
