//! Validation domain: configuration warnings, schema-diff, and the preview.

pub mod preview;
pub mod schema_diff;
pub mod warnings;

pub use preview::{parse_format, parse_table_format, preview, Format, PreviewOptions, TableFormat};
pub use schema_diff::{CollectionSchema, IndexSpec, SchemaDiff};
pub use warnings::{Severity, Warning, Warnings};
