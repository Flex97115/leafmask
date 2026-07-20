//! Storage abstraction and pluggable backend selection.
//!
//! Every command that reads or writes dumps goes through the [`Storage`] trait
//! and never knows which physical backend is active. Backends are selected at
//! runtime from config (`storage.type`). The local directory backend is always
//! available; the S3, Azure, and SSH backends are behind cargo features.

pub mod azure;
pub mod directory;
pub mod s3;
pub mod selection;
pub mod ssh;

pub use directory::DirectoryStorage;
pub use selection::open_from_config;

use crate::error::Result;

/// Create a unique local spool file for a streamed download, so remote
/// backends can hand back a bounded-memory reader. Returns the open file
/// (write mode) and its path; callers reopen for reading and remove it.
#[cfg_attr(not(any(feature = "s3", feature = "ssh")), allow(dead_code))]
pub(crate) fn download_spool() -> Result<(std::fs::File, std::path::PathBuf)> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("leafmask-dl-{}-{seq}", std::process::id()));
    let file = std::fs::File::create(&path)
        .map_err(|e| crate::Error::Storage(format!("cannot create download spool: {e}")))?;
    Ok((file, path))
}

/// Reopen a finished download spool for reading and delete it from disk (the
/// open handle keeps the data readable on unix).
#[cfg_attr(not(any(feature = "s3", feature = "ssh")), allow(dead_code))]
pub(crate) fn open_and_remove_spool(
    path: &std::path::Path,
) -> Result<Box<dyn std::io::Read + Send>> {
    let file = std::fs::File::open(path)
        .map_err(|e| crate::Error::Storage(format!("cannot reopen download spool: {e}")))?;
    let _ = std::fs::remove_file(path);
    Ok(Box::new(std::io::BufReader::new(file)))
}

/// A dump held in storage, as surfaced to `list-dumps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpEntry {
    /// The dump ID (its top-level directory / key prefix).
    pub id: String,
}

/// A streaming, backgrounded write target: the implementation uploads
/// buffered chunks on its own thread while the caller keeps writing,
/// instead of blocking until the whole blob is assembled locally.
pub trait MultipartWriter: std::io::Write + Send {
    /// Flush the last partial chunk, wait for all in-flight uploads and the
    /// final completion call, and return the total bytes written.
    fn finish(self: Box<Self>) -> Result<u64>;
    /// Called instead of [`Self::finish`] when the write failed partway
    /// through (a transform error, a Mongo cursor error, ...) — cancels
    /// whatever was started server-side so nothing is left dangling.
    fn abort(&self);
}

/// The common storage interface, equivalent to the source product's
/// `storages.Storager`. Paths are always `/`-separated and relative to the
/// backend's configured root; the top-level path segment is the dump ID.
pub trait Storage: Send + Sync {
    /// List the dump IDs present (the top-level entries).
    fn list_dumps(&self) -> Result<Vec<String>>;

    /// Read the blob at `path`. Errors with [`crate::Error::NotFound`] if absent.
    fn get(&self, path: &str) -> Result<Vec<u8>>;

    /// Write `data` at `path`, creating any intermediate structure.
    fn put(&self, path: &str, data: &[u8]) -> Result<()>;

    /// Whether a blob exists at `path`.
    fn exists(&self, path: &str) -> Result<bool>;

    /// Upload the local file at `local` to `path`, streaming when the backend
    /// supports it. The local file may be consumed (moved) by the call. The
    /// default buffers the whole file through [`Storage::put`]; backends that
    /// can stream from disk should override to keep memory bounded on
    /// multi-gigabyte collection blobs.
    fn put_file(&self, path: &str, local: &std::path::Path) -> Result<()> {
        let data = std::fs::read(local)
            .map_err(|e| crate::Error::Storage(format!("cannot read {}: {e}", local.display())))?;
        self.put(path, &data)
    }

    /// Open a streaming reader over the blob at `path`. The default buffers the
    /// whole blob via [`Storage::get`]; backends that can stream should
    /// override so restoring a huge collection never holds it all in memory.
    fn get_reader(&self, path: &str) -> Result<Box<dyn std::io::Read + Send>> {
        Ok(Box::new(std::io::Cursor::new(self.get(path)?)))
    }

    /// Delete everything under `prefix` (a whole dump dir or a single blob).
    fn delete(&self, prefix: &str) -> Result<()>;

    /// List blob paths under `prefix`, recursively, relative to the root.
    fn list(&self, prefix: &str) -> Result<Vec<String>>;

    /// Total byte size stored under `prefix`.
    fn size(&self, prefix: &str) -> Result<u64>;

    /// Open a streaming multipart writer at `path`, for backends that can
    /// overlap upload with writes. `None` by default — the caller falls
    /// back to spool-then-[`Storage::put_file`]. S3 and Azure override
    /// this.
    fn multipart_writer(&self, _path: &str) -> Result<Option<Box<dyn MultipartWriter>>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopStorage;
    impl Storage for NoopStorage {
        fn list_dumps(&self) -> Result<Vec<String>> {
            Ok(vec![])
        }
        fn get(&self, _path: &str) -> Result<Vec<u8>> {
            Err(crate::Error::NotFound("n/a".into()))
        }
        fn put(&self, _path: &str, _data: &[u8]) -> Result<()> {
            Ok(())
        }
        fn exists(&self, _path: &str) -> Result<bool> {
            Ok(false)
        }
        fn delete(&self, _prefix: &str) -> Result<()> {
            Ok(())
        }
        fn list(&self, _prefix: &str) -> Result<Vec<String>> {
            Ok(vec![])
        }
        fn size(&self, _prefix: &str) -> Result<u64> {
            Ok(0)
        }
    }

    // Backends that don't override multipart_writer() must fall back to
    // `None`, so CollectionDataWriter's spool-then-put_file path is
    // unaffected for them (directory, ssh).
    #[test]
    fn multipart_writer_defaults_to_none() {
        let storage = NoopStorage;
        assert!(storage.multipart_writer("some/path").unwrap().is_none());
    }
}
