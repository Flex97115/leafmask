//! Subsetting domain: virtual reference declarations, the dependency graph, and
//! the referentially-intact subsetting engine.

pub mod virtual_references;

pub use virtual_references::{
    PolymorphicCase, RefTarget, ReferenceGraph, VirtualReference, VirtualReferenceEntry,
};
