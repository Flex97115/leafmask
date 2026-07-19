//! Dump domain: the on-storage dump metadata / table-of-contents format shared
//! by every command that reads or writes dumps, plus dump creation and dump
//! management (list/show/delete).

pub mod create;
pub mod management;

pub use create::{Dump, DumpOptions};

use std::io::{Read, Write};

use bson::Document;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::mongo::CollectionData;
use crate::storage::Storage;

/// Completion status of a dump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DumpStatus {
    /// Completed successfully.
    Done,
    /// Started but did not finish.
    Failed,
    /// Currently running.
    InProgress,
    /// Status could not be determined.
    Unknown,
}

impl DumpStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            DumpStatus::Done => "done",
            DumpStatus::Failed => "failed",
            DumpStatus::InProgress => "in-progress",
            DumpStatus::Unknown => "unknown",
        }
    }
}

/// One collection's entry in the table of contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionToc {
    pub name: String,
    #[serde(default)]
    pub document_count: u64,
    #[serde(default)]
    pub indexes: Vec<String>,
    /// Position in the restore order (documents before indexes/validators).
    #[serde(default)]
    pub restore_order: u32,
}

/// One database's entry in the table of contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseToc {
    pub name: String,
    #[serde(default)]
    pub collections: Vec<CollectionToc>,
}

/// A dump's metadata document, stored at `<id>/metadata.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DumpMetadata {
    pub id: String,
    pub status: DumpStatus,
    /// RFC3339 creation timestamp; the dump id is derived from it.
    pub created_at: String,
    #[serde(default)]
    pub databases: Vec<DatabaseToc>,
    #[serde(default)]
    pub size: u64,
}

impl DumpMetadata {
    /// The storage path of a dump's metadata file.
    pub fn metadata_path(id: &str) -> String {
        format!("{id}/metadata.json")
    }

    /// A readable table-of-contents rendering for `show-dump`.
    pub fn render_toc(&self) -> String {
        let mut out = format!(
            "dump {}\n  status: {}\n  created_at: {}\n  size: {} bytes\n",
            self.id,
            self.status.as_str(),
            self.created_at,
            self.size
        );
        for db in &self.databases {
            out.push_str(&format!("  database {}:\n", db.name));
            let mut collections = db.collections.clone();
            collections.sort_by_key(|c| c.restore_order);
            for c in &collections {
                out.push_str(&format!(
                    "    - {} ({} docs, {} indexes) [restore #{}]\n",
                    c.name,
                    c.document_count,
                    c.indexes.len(),
                    c.restore_order
                ));
                for idx in &c.indexes {
                    out.push_str(&format!("        index: {idx}\n"));
                }
            }
        }
        out
    }
}

/// Persist a dump's metadata to storage.
pub fn write_metadata(storage: &dyn Storage, meta: &DumpMetadata) -> Result<()> {
    let json = serde_json::to_vec_pretty(meta).map_err(|e| Error::Storage(e.to_string()))?;
    storage.put(&DumpMetadata::metadata_path(&meta.id), &json)
}

/// Read one dump's metadata from storage. Errors with `NotFound` if the dump or
/// its metadata is absent.
pub fn read_metadata(storage: &dyn Storage, id: &str) -> Result<DumpMetadata> {
    let bytes = storage
        .get(&DumpMetadata::metadata_path(id))
        .map_err(|e| match e {
            Error::NotFound(_) => Error::NotFound(format!("dump '{id}' not found")),
            other => other,
        })?;
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::Storage(format!("corrupt metadata for '{id}': {e}")))
}

/// Read metadata for every dump present in storage. Dumps whose metadata cannot
/// be read are surfaced as `Unknown`-status stubs rather than failing the whole
/// listing.
pub fn list_metadata(storage: &dyn Storage) -> Result<Vec<DumpMetadata>> {
    let mut out = Vec::new();
    for id in storage.list_dumps()? {
        match read_metadata(storage, &id) {
            Ok(meta) => out.push(meta),
            Err(_) => out.push(DumpMetadata {
                id: id.clone(),
                status: DumpStatus::Unknown,
                created_at: String::new(),
                databases: Vec::new(),
                size: 0,
            }),
        }
    }
    Ok(out)
}

/// Resolve a dump reference: a literal id, or `latest` -> the most recently
/// created `Done` dump.
pub fn resolve(storage: &dyn Storage, id_or_latest: &str) -> Result<DumpMetadata> {
    if id_or_latest == "latest" {
        let mut done: Vec<DumpMetadata> = list_metadata(storage)?
            .into_iter()
            .filter(|m| m.status == DumpStatus::Done)
            .collect();
        done.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        done.into_iter()
            .next()
            .ok_or_else(|| Error::NotFound("no completed dump found for 'latest'".into()))
    } else {
        read_metadata(storage, id_or_latest)
    }
}

/// The storage path of a collection's data blob within a dump.
pub fn data_path(db: &str, collection: &str, gzip: bool) -> String {
    let ext = if gzip { "bson.gz" } else { "bson" };
    format!("data/{db}/{collection}.{ext}")
}

/// The storage path of a collection's structure blob within a dump.
pub fn meta_path(db: &str, collection: &str) -> String {
    format!("data/{db}/{collection}.meta.bson")
}

/// A collection's structure and document count, stored beside its data blob.
/// Kept separate from the documents so the data blob can be a plain
/// concatenation of BSON documents — streamable in bounded memory and free of
/// the i32 size cap a single wrapper document would impose.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CollectionMeta {
    pub database: String,
    pub collection: String,
    #[serde(default)]
    pub document_count: u64,
    #[serde(default)]
    pub indexes: Vec<crate::validate::IndexSpec>,
    #[serde(default)]
    pub validator: Option<bson::Bson>,
    #[serde(default)]
    pub options: std::collections::BTreeMap<String, bson::Bson>,
}

/// Persist a collection's structure blob. Returns its size in bytes.
pub fn write_collection_meta(
    storage: &dyn Storage,
    dump_id: &str,
    meta: &CollectionMeta,
) -> Result<u64> {
    let raw = bson::to_vec(meta).map_err(|e| Error::Storage(e.to_string()))?;
    let path = format!("{dump_id}/{}", meta_path(&meta.database, &meta.collection));
    storage.put(&path, &raw)?;
    Ok(raw.len() as u64)
}

/// Read a collection's structure blob back from a dump.
pub fn read_collection_meta(
    storage: &dyn Storage,
    dump_id: &str,
    db: &str,
    collection: &str,
) -> Result<CollectionMeta> {
    let raw = storage.get(&format!("{dump_id}/{}", meta_path(db, collection)))?;
    bson::from_slice(&raw).map_err(|e| Error::Storage(format!("corrupt collection meta: {e}")))
}

/// Upper bound on a single document's serialized size when reading a dump.
/// MongoDB caps user documents at 16 MiB; anything past this margin means the
/// blob is corrupt, and failing beats attempting a multi-gigabyte allocation.
const MAX_DOCUMENT_SIZE: usize = 64 * 1024 * 1024;

/// Streams documents out of a collection data blob one at a time, so restore
/// memory stays bounded by the insert batch size, not the collection size.
pub struct DocumentReader {
    inner: Box<dyn Read + Send>,
}

impl DocumentReader {
    /// Read the next document, or `None` at (clean) end of stream.
    pub fn next_document(&mut self) -> Result<Option<Document>> {
        let mut len_buf = [0u8; 4];
        let mut filled = 0;
        while filled < 4 {
            let n = self
                .inner
                .read(&mut len_buf[filled..])
                .map_err(|e| Error::Storage(e.to_string()))?;
            if n == 0 {
                if filled == 0 {
                    return Ok(None); // clean end of stream.
                }
                return Err(Error::Storage(
                    "corrupt dump: truncated document length".into(),
                ));
            }
            filled += n;
        }
        let len = i32::from_le_bytes(len_buf) as isize;
        if !(5..=MAX_DOCUMENT_SIZE as isize).contains(&len) {
            return Err(Error::Storage(format!(
                "corrupt dump: invalid document length {len}"
            )));
        }
        let mut buf = vec![0u8; len as usize];
        buf[..4].copy_from_slice(&len_buf);
        self.inner
            .read_exact(&mut buf[4..])
            .map_err(|e| Error::Storage(format!("corrupt dump: {e}")))?;
        let doc = bson::from_slice(&buf).map_err(|e| Error::Storage(e.to_string()))?;
        Ok(Some(doc))
    }

    /// Read up to `n` documents; an empty vec means end of stream.
    pub fn next_batch(&mut self, n: usize) -> Result<Vec<Document>> {
        let mut out = Vec::with_capacity(n.min(1024));
        while out.len() < n.max(1) {
            match self.next_document()? {
                Some(d) => out.push(d),
                None => break,
            }
        }
        Ok(out)
    }
}

/// Open a streaming reader over a collection's documents, transparently
/// handling the gzip and plain variants.
pub fn open_document_reader(
    storage: &dyn Storage,
    dump_id: &str,
    db: &str,
    collection: &str,
) -> Result<DocumentReader> {
    let gz_path = format!("{dump_id}/{}", data_path(db, collection, true));
    let plain_path = format!("{dump_id}/{}", data_path(db, collection, false));

    let inner: Box<dyn Read + Send> = if storage.exists(&gz_path)? {
        Box::new(flate2::read::GzDecoder::new(storage.get_reader(&gz_path)?))
    } else {
        storage.get_reader(&plain_path)?
    };
    Ok(DocumentReader { inner })
}

/// Streaming writer for a collection's data blob: documents are appended one
/// at a time (optionally gzip-compressed) to a spool file under `tmp_dir`,
/// then uploaded to storage in one pass. Memory stays bounded per document.
pub struct CollectionDataWriter {
    spool: std::path::PathBuf,
    inner: Option<SpoolWriter>,
    count: u64,
}

enum SpoolWriter {
    Plain(std::io::BufWriter<std::fs::File>),
    Gzip(flate2::write::GzEncoder<std::io::BufWriter<std::fs::File>>),
}

impl CollectionDataWriter {
    /// Open a spool file for `db`.`collection` under `tmp_dir` (created if
    /// missing).
    pub fn create(tmp_dir: &str, db: &str, collection: &str, gzip: bool) -> Result<Self> {
        std::fs::create_dir_all(tmp_dir)
            .map_err(|e| Error::Storage(format!("cannot create tmp_dir {tmp_dir}: {e}")))?;
        // pid + per-process counter keeps concurrent dumps (and parallel
        // tests) from ever sharing a spool file.
        static SPOOL_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SPOOL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let spool = std::path::Path::new(tmp_dir).join(format!(
            "leafmask-{}-{seq}-{db}-{collection}.spool",
            std::process::id()
        ));
        let file = std::fs::File::create(&spool)
            .map_err(|e| Error::Storage(format!("cannot create spool file: {e}")))?;
        let buf = std::io::BufWriter::new(file);
        let inner = if gzip {
            SpoolWriter::Gzip(flate2::write::GzEncoder::new(
                buf,
                flate2::Compression::default(),
            ))
        } else {
            SpoolWriter::Plain(buf)
        };
        Ok(CollectionDataWriter {
            spool,
            inner: Some(inner),
            count: 0,
        })
    }

    /// Append one document to the blob.
    pub fn write_document(&mut self, doc: &Document) -> Result<()> {
        let w: &mut dyn Write = match self.inner.as_mut().expect("writer not finished") {
            SpoolWriter::Plain(w) => w,
            SpoolWriter::Gzip(w) => w,
        };
        doc.to_writer(w)
            .map_err(|e| Error::Storage(e.to_string()))?;
        self.count += 1;
        Ok(())
    }

    /// Documents written so far.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Finish the blob and upload it to `path` in storage. Returns the blob's
    /// size in bytes. The spool file is removed afterwards.
    pub fn finish(mut self, storage: &dyn Storage, path: &str) -> Result<u64> {
        let mut buf = match self.inner.take().expect("writer already finished") {
            SpoolWriter::Plain(w) => w,
            SpoolWriter::Gzip(enc) => enc.finish().map_err(|e| Error::Storage(e.to_string()))?,
        };
        buf.flush().map_err(|e| Error::Storage(e.to_string()))?;
        drop(buf);
        let size = std::fs::metadata(&self.spool)
            .map_err(|e| Error::Storage(e.to_string()))?
            .len();
        storage.put_file(path, &self.spool)?;
        let _ = std::fs::remove_file(&self.spool);
        Ok(size)
    }
}

impl Drop for CollectionDataWriter {
    fn drop(&mut self) {
        // A writer dropped without finish() (error path) leaves no spool behind.
        if self.inner.take().is_some() {
            let _ = std::fs::remove_file(&self.spool);
        }
    }
}

/// Write a collection's data and structure into the dump from an in-memory
/// [`CollectionData`] (tests and small fixtures; the real dump streams through
/// [`CollectionDataWriter`] instead). Returns bytes written.
pub fn write_collection_data(
    storage: &dyn Storage,
    dump_id: &str,
    data: &CollectionData,
    gzip: bool,
) -> Result<u64> {
    let mut raw = Vec::new();
    for doc in &data.documents {
        doc.to_writer(&mut raw)
            .map_err(|e| Error::Storage(e.to_string()))?;
    }
    let bytes = if gzip {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&raw)
            .map_err(|e| Error::Storage(e.to_string()))?;
        enc.finish().map_err(|e| Error::Storage(e.to_string()))?
    } else {
        raw
    };
    let path = format!(
        "{dump_id}/{}",
        data_path(&data.database, &data.collection, gzip)
    );
    let size = bytes.len() as u64;
    storage.put(&path, &bytes)?;
    let meta_size = write_collection_meta(
        storage,
        dump_id,
        &CollectionMeta {
            database: data.database.clone(),
            collection: data.collection.clone(),
            document_count: data.documents.len() as u64,
            indexes: data.indexes.clone(),
            validator: data.validator.clone(),
            options: data.options.clone(),
        },
    )?;
    Ok(size + meta_size)
}

/// Read a collection's full data (documents, indexes, validator, options) back
/// from a dump, transparently handling the gzip and plain variants. Materializes
/// every document — callers on a potentially large collection should stream via
/// [`open_document_reader`] instead.
pub fn read_collection_full(
    storage: &dyn Storage,
    dump_id: &str,
    db: &str,
    collection: &str,
) -> Result<CollectionData> {
    let meta = read_collection_meta(storage, dump_id, db, collection)?;
    let mut reader = open_document_reader(storage, dump_id, db, collection)?;
    let mut documents = Vec::new();
    while let Some(doc) = reader.next_document()? {
        documents.push(doc);
    }
    Ok(CollectionData {
        database: meta.database,
        collection: meta.collection,
        documents,
        indexes: meta.indexes,
        validator: meta.validator,
        options: meta.options,
    })
}

/// Read just a collection's documents back from a dump.
pub fn read_collection_data(
    storage: &dyn Storage,
    dump_id: &str,
    db: &str,
    collection: &str,
) -> Result<Vec<Document>> {
    Ok(read_collection_full(storage, dump_id, db, collection)?.documents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::DirectoryStorage;

    fn users(n: i64) -> CollectionData {
        let mut documents = Vec::new();
        for i in 0..n {
            let mut d = Document::new();
            d.insert("_id", i);
            d.insert("email", format!("u{i}@x.com"));
            documents.push(d);
        }
        CollectionData {
            database: "app".into(),
            collection: "users".into(),
            documents,
            ..Default::default()
        }
    }

    // The data blob must be a plain concatenation of BSON documents (mongodump
    // framing), not one wrapper document: a single wrapper document caps the
    // whole collection at BSON's i32 size limit (2 GiB) and forces the entire
    // collection into memory. Structure lives in a separate small meta blob.
    #[test]
    fn data_blob_is_concatenated_bson_documents() {
        let dir = tempfile::tempdir().unwrap();
        let s = DirectoryStorage::new(dir.path()).unwrap();
        write_collection_data(&s, "d1", &users(3), false).unwrap();

        // Raw file parses as exactly 3 consecutive top-level BSON documents.
        let raw = s
            .get(&format!("d1/{}", data_path("app", "users", false)))
            .unwrap();
        let mut cursor = std::io::Cursor::new(&raw[..]);
        let mut seen = 0;
        while (cursor.position() as usize) < raw.len() {
            let d = Document::from_reader(&mut cursor).unwrap();
            assert!(d.contains_key("_id"), "expected a data document, got {d}");
            seen += 1;
        }
        assert_eq!(seen, 3);

        // Structure is stored beside the data.
        let meta = read_collection_meta(&s, "d1", "app", "users").unwrap();
        assert_eq!(meta.document_count, 3);
    }

    // Documents stream back one at a time (gzip-transparently), so restore
    // never has to hold a whole collection in memory.
    #[test]
    fn document_reader_streams_plain_and_gzip() {
        for gzip in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let s = DirectoryStorage::new(dir.path()).unwrap();
            write_collection_data(&s, "d1", &users(5), gzip).unwrap();

            let mut r = open_document_reader(&s, "d1", "app", "users").unwrap();
            let mut ids = Vec::new();
            while let Some(doc) = r.next_document().unwrap() {
                ids.push(doc.get_i64("_id").unwrap());
            }
            assert_eq!(ids, vec![0, 1, 2, 3, 4], "gzip={gzip}");

            // Batched reads honour the batch size and drain to empty.
            let mut r = open_document_reader(&s, "d1", "app", "users").unwrap();
            assert_eq!(r.next_batch(2).unwrap().len(), 2);
            assert_eq!(r.next_batch(2).unwrap().len(), 2);
            assert_eq!(r.next_batch(2).unwrap().len(), 1);
            assert!(r.next_batch(2).unwrap().is_empty());
        }
    }

    fn meta(id: &str, status: DumpStatus, created: &str) -> DumpMetadata {
        DumpMetadata {
            id: id.to_string(),
            status,
            created_at: created.to_string(),
            databases: vec![DatabaseToc {
                name: "app".into(),
                collections: vec![CollectionToc {
                    name: "users".into(),
                    document_count: 5,
                    indexes: vec!["email_idx".into()],
                    restore_order: 0,
                }],
            }],
            size: 100,
        }
    }

    #[test]
    fn writes_reads_and_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let s = DirectoryStorage::new(dir.path()).unwrap();
        write_metadata(
            &s,
            &meta("20260101T000000Z", DumpStatus::Done, "2026-01-01T00:00:00Z"),
        )
        .unwrap();
        write_metadata(
            &s,
            &meta("20260102T000000Z", DumpStatus::Done, "2026-01-02T00:00:00Z"),
        )
        .unwrap();

        let read = read_metadata(&s, "20260101T000000Z").unwrap();
        assert_eq!(read.status, DumpStatus::Done);
        assert!(read.render_toc().contains("email_idx"));

        // latest -> most recent Done.
        assert_eq!(resolve(&s, "latest").unwrap().id, "20260102T000000Z");
        // nonexistent -> clear error.
        assert!(matches!(read_metadata(&s, "nope"), Err(Error::NotFound(_))));
    }
}
