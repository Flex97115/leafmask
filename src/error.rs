//! Shared error type for the whole crate.

use std::fmt;

/// The crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// A single error type surfaced to the operator. Each variant carries a
/// human-readable message; the CLI turns any error into a non-zero exit with a
/// clear message rather than a panic.
#[derive(Debug)]
pub enum Error {
    /// Configuration loading / validation failure.
    Config(String),
    /// A transformer parameter could not be declared, cast, or validated.
    Parameter(String),
    /// A transformer failed at declaration or transform time.
    Transform(String),
    /// A storage backend operation failed.
    Storage(String),
    /// A requested dump / transformer / resource was not found.
    NotFound(String),
    /// Restore-time failure.
    Restore(String),
    /// Subsetting failure.
    Subset(String),
    /// Validation failure (e.g. `--strict` with unresolved warnings).
    Validation(String),
    /// MongoDB access failure.
    Mongo(String),
    /// Any underlying I/O error.
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Config(m) => write!(f, "config error: {m}"),
            Error::Parameter(m) => write!(f, "parameter error: {m}"),
            Error::Transform(m) => write!(f, "transform error: {m}"),
            Error::Storage(m) => write!(f, "storage error: {m}"),
            Error::NotFound(m) => write!(f, "not found: {m}"),
            Error::Restore(m) => write!(f, "restore error: {m}"),
            Error::Subset(m) => write!(f, "subset error: {m}"),
            Error::Validation(m) => write!(f, "validation error: {m}"),
            Error::Mongo(m) => write!(f, "mongodb error: {m}"),
            Error::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
