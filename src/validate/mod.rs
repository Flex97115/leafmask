//! Validation domain: configuration warnings, schema-diff, and the preview.

pub mod schema_diff;
pub mod warnings;

pub use schema_diff::{CollectionSchema, IndexSpec, SchemaDiff};
pub use warnings::{Severity, Warning, Warnings};
