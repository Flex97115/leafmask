//! MongoDB access abstraction.
//!
//! Dump and restore access MongoDB through these traits rather than a wrapped
//! `mongodump`/`mongorestore`, so per-document transformation can be applied
//! inline while streaming. The traits keep the dump/restore logic testable with
//! an in-memory fake; the real `mongodb`-driver implementation lives behind the
//! `mongo` cargo feature.

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

/// A read side: enumerate and read collections (for dump / validate).
pub trait MongoSource {
    fn databases(&self) -> Vec<String>;
    fn collections(&self, database: &str) -> Vec<String>;
    fn read_collection(&self, database: &str, collection: &str) -> Result<CollectionData>;
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
    fn databases(&self) -> Vec<String> {
        let mut dbs: Vec<String> = self
            .inner
            .lock()
            .unwrap()
            .keys()
            .map(|(db, _)| db.clone())
            .collect();
        dbs.sort();
        dbs.dedup();
        dbs
    }

    fn collections(&self, database: &str) -> Vec<String> {
        let mut cols: Vec<String> = self
            .inner
            .lock()
            .unwrap()
            .keys()
            .filter(|(db, _)| db == database)
            .map(|(_, c)| c.clone())
            .collect();
        cols.sort();
        cols
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
