//! Transformation engine.
//!
//! A [`Transformer`] maps one field value to a new value, given the whole
//! document for context. Transformers are produced by named factories held in a
//! [`Registry`]; each factory declares typed parameters (via the toolkit) so it
//! is both validated and discoverable through the catalog. Built-in and custom
//! transformers share this machinery.

pub mod builtin;
pub mod custom;
pub mod dynamic;

use std::collections::BTreeMap;

use bson::{Bson, Document};

use crate::error::{Error, Result};
use crate::hash::HashEngine;
use crate::toolkit::{validate_static, ParamDefinition, ParamValues};

/// Maps a field value to its transformed value. `doc` is the full document, so
/// a transformer may read sibling fields.
pub trait Transformer: Send + Sync {
    fn transform(&self, value: &Bson, doc: &Document) -> Result<Bson>;
}

/// A transformer implemented by a closure — keeps built-ins terse.
pub struct FnTransformer<F>(pub F);

impl<F> Transformer for FnTransformer<F>
where
    F: Fn(&Bson, &Document) -> Result<Bson> + Send + Sync,
{
    fn transform(&self, value: &Bson, doc: &Document) -> Result<Bson> {
        (self.0)(value, doc)
    }
}

/// Catalog documentation for one transformer.
#[derive(Debug, Clone)]
pub struct TransformerDoc {
    pub name: String,
    pub description: String,
    pub parameters: Vec<ParamDefinition>,
}

type BuildFn =
    Box<dyn Fn(&ParamValues, &HashEngine) -> Result<Box<dyn Transformer>> + Send + Sync>;

/// A factory that declares a transformer's parameters and builds instances.
pub struct TransformerFactory {
    pub name: String,
    pub description: String,
    pub parameters: Vec<ParamDefinition>,
    build: BuildFn,
}

impl TransformerFactory {
    pub fn new(
        name: &str,
        description: &str,
        parameters: Vec<ParamDefinition>,
        build: impl Fn(&ParamValues, &HashEngine) -> Result<Box<dyn Transformer>> + Send + Sync + 'static,
    ) -> Self {
        TransformerFactory {
            name: name.to_string(),
            description: description.to_string(),
            parameters,
            build: Box::new(build),
        }
    }

    pub fn doc(&self) -> TransformerDoc {
        TransformerDoc {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }
}

/// A registry of transformer factories, keyed by name.
#[derive(Default)]
pub struct Registry {
    factories: BTreeMap<String, TransformerFactory>,
}

impl Registry {
    pub fn new() -> Self {
        Registry::default()
    }

    /// A registry preloaded with every built-in transformer.
    pub fn with_builtins() -> Self {
        let mut r = Registry::new();
        builtin::register_all(&mut r);
        r
    }

    pub fn register(&mut self, f: TransformerFactory) {
        self.factories.insert(f.name.clone(), f);
    }

    pub fn get(&self, name: &str) -> Option<&TransformerFactory> {
        self.factories.get(name)
    }

    /// Transformer names, sorted.
    pub fn names(&self) -> Vec<&str> {
        self.factories.keys().map(String::as_str).collect()
    }

    /// Catalog docs for every registered transformer, sorted by name.
    pub fn docs(&self) -> Vec<TransformerDoc> {
        self.factories.values().map(TransformerFactory::doc).collect()
    }

    /// Validate raw config params against the named transformer and build it.
    pub fn instantiate(
        &self,
        name: &str,
        raw: &crate::config::Params,
        engine: &HashEngine,
    ) -> Result<Box<dyn Transformer>> {
        let factory = self
            .get(name)
            .ok_or_else(|| Error::NotFound(format!("transformer '{name}' not found")))?;
        let values = validate_static(&factory.parameters, raw)?;
        (factory.build)(&values, engine)
    }
}
