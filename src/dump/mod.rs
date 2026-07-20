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
/// at a time (optionally gzip-compressed). Backends that support it (S3,
/// Azure) stream straight into a backgrounded multipart upload, so the
/// upload overlaps with whatever is still being written; other backends
/// fall back to a local spool file, uploaded in one pass once writing is
/// done. Memory stays bounded per document either way.
pub struct CollectionDataWriter {
    inner: Option<SinkWriter>,
    count: u64,
    target_path: String,
}

enum RawSink {
    Spool {
        path: std::path::PathBuf,
        file: std::io::BufWriter<std::fs::File>,
    },
    Multipart(Box<dyn crate::storage::MultipartWriter>),
}

impl Write for RawSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            RawSink::Spool { file, .. } => file.write(buf),
            RawSink::Multipart(w) => w.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            RawSink::Spool { file, .. } => file.flush(),
            RawSink::Multipart(w) => w.flush(),
        }
    }
}

enum SinkWriter {
    Plain(RawSink),
    Gzip(flate2::write::GzEncoder<RawSink>),
}

impl CollectionDataWriter {
    /// Open a writer for `db`.`collection`, targeting `path` in `storage`.
    /// Uses a streaming multipart upload when `storage` supports it
    /// ([`Storage::multipart_writer`]); otherwise spools to a local file
    /// under `tmp_dir` (created if missing) and uploads it whole in
    /// [`Self::finish`].
    pub fn create(
        storage: &dyn Storage,
        path: &str,
        tmp_dir: &str,
        db: &str,
        collection: &str,
        gzip: bool,
    ) -> Result<Self> {
        let raw = match storage.multipart_writer(path)? {
            Some(w) => RawSink::Multipart(w),
            None => {
                std::fs::create_dir_all(tmp_dir)
                    .map_err(|e| Error::Storage(format!("cannot create tmp_dir {tmp_dir}: {e}")))?;
                // pid + per-process counter keeps concurrent dumps (and
                // parallel tests) from ever sharing a spool file.
                static SPOOL_SEQ: std::sync::atomic::AtomicU64 =
                    std::sync::atomic::AtomicU64::new(0);
                let seq = SPOOL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let spool_path = std::path::Path::new(tmp_dir).join(format!(
                    "leafmask-{}-{seq}-{db}-{collection}.spool",
                    std::process::id()
                ));
                let file = std::fs::File::create(&spool_path)
                    .map_err(|e| Error::Storage(format!("cannot create spool file: {e}")))?;
                RawSink::Spool {
                    path: spool_path,
                    file: std::io::BufWriter::new(file),
                }
            }
        };
        let inner = if gzip {
            SinkWriter::Gzip(flate2::write::GzEncoder::new(
                raw,
                flate2::Compression::default(),
            ))
        } else {
            SinkWriter::Plain(raw)
        };
        Ok(CollectionDataWriter {
            inner: Some(inner),
            count: 0,
            target_path: path.to_string(),
        })
    }

    /// Append one document to the blob.
    pub fn write_document(&mut self, doc: &Document) -> Result<()> {
        let w: &mut dyn Write = match self.inner.as_mut().expect("writer not finished") {
            SinkWriter::Plain(w) => w,
            SinkWriter::Gzip(w) => w,
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

    /// Finish the blob. For the multipart path this waits for the upload to
    /// complete; for the spool-file path this uploads the finished local
    /// file to `storage` (removed afterwards). Returns the blob's size in
    /// bytes.
    pub fn finish(mut self, storage: &dyn Storage) -> Result<u64> {
        let raw = match self.inner.take().expect("writer already finished") {
            SinkWriter::Plain(raw) => raw,
            SinkWriter::Gzip(enc) => enc.finish().map_err(|e| Error::Storage(e.to_string()))?,
        };
        match raw {
            RawSink::Spool {
                path: spool_path,
                mut file,
            } => {
                file.flush().map_err(|e| Error::Storage(e.to_string()))?;
                drop(file);
                let size = std::fs::metadata(&spool_path)
                    .map_err(|e| Error::Storage(e.to_string()))?
                    .len();
                storage.put_file(&self.target_path, &spool_path)?;
                let _ = std::fs::remove_file(&spool_path);
                Ok(size)
            }
            RawSink::Multipart(w) => w.finish(),
        }
    }
}

impl Drop for CollectionDataWriter {
    fn drop(&mut self) {
        // A writer dropped without finish() (error path): clean up
        // whatever was started rather than leaving a local spool file or a
        // dangling multipart upload behind.
        match self.inner.take() {
            Some(SinkWriter::Plain(RawSink::Spool { path, .. })) => {
                let _ = std::fs::remove_file(&path);
            }
            Some(SinkWriter::Plain(RawSink::Multipart(w))) => w.abort(),
            Some(SinkWriter::Gzip(enc)) => {
                // GzEncoder's own Drop impl unconditionally attempts a trailer
                // write if it was never consumed via finish() — get_ref() alone
                // doesn't prevent that. So call finish() ourselves here: it lets
                // that trailer write land on the still-valid resource, and gives
                // us the RawSink back to clean up properly. Capture the spool
                // path first in case finish() itself fails (e.g. disk error) and
                // never returns the RawSink at all.
                let spool_path = match enc.get_ref() {
                    RawSink::Spool { path, .. } => Some(path.clone()),
                    RawSink::Multipart(_) => None,
                };
                match enc.finish() {
                    Ok(RawSink::Spool { path, .. }) => {
                        let _ = std::fs::remove_file(&path);
                    }
                    Ok(RawSink::Multipart(w)) => w.abort(),
                    Err(_) => {
                        if let Some(path) = spool_path {
                            let _ = std::fs::remove_file(&path);
                        }
                        // Multipart case: finish() failing here means the write to
                        // the multipart writer failed, which only happens once its
                        // background thread has already exited — and every exit
                        // path in that thread already calls the sink's abort (or
                        // complete); nothing further to do.
                    }
                }
            }
            None => {}
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

    // When the storage backend offers a multipart writer, CollectionDataWriter
    // must stream straight into it instead of spooling to a local file first
    // — proven here by pointing tmp_dir at a path that's never created.
    #[test]
    fn create_uses_multipart_writer_when_storage_offers_one() {
        use crate::storage::MultipartWriter;
        use std::sync::{Arc, Mutex};

        struct FakeMultipartWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for FakeMultipartWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl MultipartWriter for FakeMultipartWriter {
            fn finish(self: Box<Self>) -> Result<u64> {
                Ok(self.0.lock().unwrap().len() as u64)
            }
            fn abort(&self) {}
        }

        struct FakeMultipartStorage {
            uploaded: Arc<Mutex<Vec<u8>>>,
        }
        impl crate::storage::Storage for FakeMultipartStorage {
            fn list_dumps(&self) -> Result<Vec<String>> {
                unimplemented!()
            }
            fn get(&self, _path: &str) -> Result<Vec<u8>> {
                unimplemented!()
            }
            fn put(&self, _path: &str, _data: &[u8]) -> Result<()> {
                unimplemented!()
            }
            fn exists(&self, _path: &str) -> Result<bool> {
                unimplemented!()
            }
            fn delete(&self, _prefix: &str) -> Result<()> {
                unimplemented!()
            }
            fn list(&self, _prefix: &str) -> Result<Vec<String>> {
                unimplemented!()
            }
            fn size(&self, _prefix: &str) -> Result<u64> {
                unimplemented!()
            }
            fn multipart_writer(&self, _path: &str) -> Result<Option<Box<dyn MultipartWriter>>> {
                Ok(Some(Box::new(FakeMultipartWriter(self.uploaded.clone()))))
            }
        }

        let uploaded = Arc::new(Mutex::new(Vec::new()));
        let storage = FakeMultipartStorage {
            uploaded: uploaded.clone(),
        };

        let mut writer = CollectionDataWriter::create(
            &storage,
            "d1/data/app/users.bson",
            "/does/not/exist/unused-when-multipart",
            "app",
            "users",
            false,
        )
        .unwrap();
        writer.write_document(&users(1).documents[0]).unwrap();
        let size = writer.finish(&storage).unwrap();

        assert_eq!(size, uploaded.lock().unwrap().len() as u64);
        assert!(!uploaded.lock().unwrap().is_empty());
    }

    // Dropping a writer without finish() must not let flate2's own Drop
    // impl (which unconditionally attempts a trailer write once the
    // encoder itself is dropped) write into an already-removed spool file
    // or an already-aborted multipart writer. Covers the three Drop cases
    // that had no coverage before this fix: gzip+multipart, gzip+spool,
    // and plain+multipart (plain+spool is already covered by
    // dump::create::tests::jobs_parallel_dump_propagates_error_and_cleans_up).
    #[test]
    fn drop_without_finish_cleans_up_every_sink_variant() {
        use crate::storage::MultipartWriter;
        use std::sync::{Arc, Mutex};

        struct TrackingMultipartWriter {
            data: Arc<Mutex<Vec<u8>>>,
            aborted: Arc<Mutex<bool>>,
        }
        impl Write for TrackingMultipartWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.data.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl MultipartWriter for TrackingMultipartWriter {
            fn finish(self: Box<Self>) -> Result<u64> {
                Ok(self.data.lock().unwrap().len() as u64)
            }
            fn abort(&self) {
                *self.aborted.lock().unwrap() = true;
            }
        }

        struct TrackingStorage {
            aborted: Arc<Mutex<bool>>,
        }
        impl crate::storage::Storage for TrackingStorage {
            fn list_dumps(&self) -> Result<Vec<String>> {
                unimplemented!()
            }
            fn get(&self, _path: &str) -> Result<Vec<u8>> {
                unimplemented!()
            }
            fn put(&self, _path: &str, _data: &[u8]) -> Result<()> {
                unimplemented!()
            }
            fn exists(&self, _path: &str) -> Result<bool> {
                unimplemented!()
            }
            fn delete(&self, _prefix: &str) -> Result<()> {
                unimplemented!()
            }
            fn list(&self, _prefix: &str) -> Result<Vec<String>> {
                unimplemented!()
            }
            fn size(&self, _prefix: &str) -> Result<u64> {
                unimplemented!()
            }
            fn multipart_writer(&self, _path: &str) -> Result<Option<Box<dyn MultipartWriter>>> {
                Ok(Some(Box::new(TrackingMultipartWriter {
                    data: Arc::new(Mutex::new(Vec::new())),
                    aborted: self.aborted.clone(),
                })))
            }
        }

        // Gzip + multipart: dropped without finish() must call abort()
        // exactly once, with no panic from a trailer write landing on an
        // already-aborted resource.
        let aborted = Arc::new(Mutex::new(false));
        let storage = TrackingStorage {
            aborted: aborted.clone(),
        };
        let writer = CollectionDataWriter::create(
            &storage,
            "d1/data/app/users.bson.gz",
            "/does/not/exist/unused-when-multipart",
            "app",
            "users",
            true,
        )
        .unwrap();
        drop(writer);
        assert!(
            *aborted.lock().unwrap(),
            "expected abort() on gzip+multipart drop"
        );

        // Plain + multipart: dropped without finish() must also call
        // abort() (no GzEncoder involved at all here).
        let aborted2 = Arc::new(Mutex::new(false));
        let storage2 = TrackingStorage {
            aborted: aborted2.clone(),
        };
        let writer2 = CollectionDataWriter::create(
            &storage2,
            "d1/data/app/users.bson",
            "/does/not/exist/unused-when-multipart",
            "app",
            "users",
            false,
        )
        .unwrap();
        drop(writer2);
        assert!(
            *aborted2.lock().unwrap(),
            "expected abort() on plain+multipart drop"
        );

        // Gzip + spool: dropped without finish() must remove the local
        // spool file (the trailer write from flate2's own Drop lands in
        // the file before it's removed, which is fine — the file is
        // deleted either way).
        let dir = tempfile::tempdir().unwrap();
        let storage3 = DirectoryStorage::new(dir.path()).unwrap();
        let tmp_dir = dir.path().join("spool");
        let writer3 = CollectionDataWriter::create(
            &storage3,
            "d1/data/app/users.bson.gz",
            tmp_dir.to_str().unwrap(),
            "app",
            "users",
            true,
        )
        .unwrap();
        drop(writer3);
        let leftover: Vec<_> = std::fs::read_dir(&tmp_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            leftover.is_empty(),
            "expected no leftover spool files, found {leftover:?}"
        );
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
