//! Storage abstraction and pluggable backend selection.
//!
//! Every command that reads or writes dumps goes through the [`Storage`] trait
//! and never knows which physical backend is active. Backends are selected at
//! runtime from config (`storage.type`). The local directory backend is always
//! available; the S3, Azure, and SSH backends are behind cargo features.

pub mod directory;
pub mod s3;
pub mod azure;
pub mod ssh;
pub mod selection;

pub use directory::DirectoryStorage;
pub use selection::open_from_config;

use crate::error::Result;

/// A dump held in storage, as surfaced to `list-dumps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpEntry {
    /// The dump ID (its top-level directory / key prefix).
    pub id: String,
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

    /// Delete everything under `prefix` (a whole dump dir or a single blob).
    fn delete(&self, prefix: &str) -> Result<()>;

    /// List blob paths under `prefix`, recursively, relative to the root.
    fn list(&self, prefix: &str) -> Result<Vec<String>>;

    /// Total byte size stored under `prefix`.
    fn size(&self, prefix: &str) -> Result<u64>;
}
