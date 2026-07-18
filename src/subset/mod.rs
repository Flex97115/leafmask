//! Subsetting domain: virtual reference declarations, the dependency graph, and
//! the referentially-intact subsetting engine.

pub mod database_subsetting;
pub mod virtual_references;

pub use database_subsetting::{DocumentSource, SubsetEngine};
pub use virtual_references::{
    PolymorphicCase, RefTarget, ReferenceGraph, VirtualReference, VirtualReferenceEntry,
};
