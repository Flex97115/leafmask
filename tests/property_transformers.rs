//! Property tests covering the whole transformer registry at once.
//!
//! Determinism is the product's central promise: masking a given input always
//! yields the same output, on any machine, in any dump run, on any worker
//! thread — that is what keeps references consistent across a subset. A unit
//! test asserts it for the transformers someone remembered to cover; this
//! suite asserts it for *every registered transformer*, including ones added
//! later, against BSON value shapes no fixture enumerates.
//!
//! Runs with default features: no MongoDB, no network.
//!
//! ```sh
//! cargo test --test property_transformers
//! ```

mod support;

use bson::Document;
use leafmask::config::Params;
use leafmask::hash::HashEngine;
use leafmask::toolkit::{ParamDefinition, ParamType};
use leafmask::transform::Registry;
use proptest::prelude::*;
use support::strategies;

/// A plausible YAML value for a required parameter of the given type, so every
/// factory in the registry can be instantiated without hand-maintaining a
/// per-transformer fixture table. Optional parameters are left out on purpose
/// — their declared defaults are part of what is under test.
fn sample_param(def: &ParamDefinition) -> serde_yaml::Value {
    use serde_yaml::Value as Y;
    match (def.name.as_str(), def.kind) {
        // `max` must exceed `min` for the ranged transformers to be meaningful.
        ("max", ParamType::Int) => Y::Number(1_000.into()),
        ("max", ParamType::Float) => Y::Number(1_000.0.into()),
        ("min", ParamType::Int) => Y::Number(0.into()),
        ("min", ParamType::Float) => Y::Number(0.0.into()),
        // A regexp parameter has to compile.
        (_, ParamType::String) if def.name.contains("regex") => Y::String("[aeiou]".into()),
        (_, ParamType::String) => Y::String("x".into()),
        (_, ParamType::Int) => Y::Number(4.into()),
        (_, ParamType::Float) => Y::Number(0.25.into()),
        (_, ParamType::Bool) => Y::Bool(true),
        (_, ParamType::Bytes) | (_, ParamType::Binary) => Y::String("payload".into()),
        (_, ParamType::ObjectId) => Y::String("07070707070707070707070c".into()),
        (_, ParamType::Decimal128) => Y::String("1.5".into()),
        (_, ParamType::DateTime) => Y::String("2020-09-13T12:26:40Z".into()),
        (_, ParamType::Any) => Y::String("x".into()),
    }
}

/// Every registered transformer, paired with synthesized parameters. Returns
/// `(name, params)` rather than built transformers so each property can build
/// its own instances from independently-constructed engines.
fn all_transformer_params() -> Vec<(String, Params)> {
    let registry = Registry::with_builtins();
    registry
        .names()
        .into_iter()
        .map(|name| {
            let factory = registry.get(name).expect("factory just listed");
            let pairs = factory
                .parameters
                .iter()
                .filter(|def| def.required)
                .map(|def| (def.name.clone(), sample_param(def)))
                .collect();
            (name.to_string(), Params::from_pairs(pairs))
        })
        .collect()
}

proptest! {
    /// Same salt + same input => same output, for every transformer, built
    /// from two independently-constructed engines. This is the cross-run,
    /// cross-machine, cross-worker-thread guarantee stated in the README.
    #[test]
    fn every_transformer_is_deterministic(
        value in strategies::value(),
        doc in strategies::document(),
    ) {
        for (name, params) in all_transformer_params() {
            let left = Registry::with_builtins()
                .instantiate(&name, &params, &HashEngine::new("pepper"));
            let right = Registry::with_builtins()
                .instantiate(&name, &params, &HashEngine::new("pepper"));

            let (left, right) = match (left, right) {
                (Ok(l), Ok(r)) => (l, r),
                // A factory that rejects the synthesized parameters is out of
                // scope here; `every_transformer_instantiates` covers that.
                _ => continue,
            };

            match (left.transform(&value, &doc), right.transform(&value, &doc)) {
                (Ok(a), Ok(b)) => prop_assert_eq!(
                    &a, &b,
                    "transformer '{}' is not deterministic: {:?} vs {:?}", name, a, b
                ),
                // Rejecting an incompatible input type is fine, as long as
                // both instances agree that it is an error.
                (Err(_), Err(_)) => {}
                (a, b) => prop_assert!(
                    false,
                    "transformer '{}' disagrees with itself: {:?} vs {:?}", name, a, b
                ),
            }
        }
    }

    /// No BSON input may make a transformer panic. A panic inside a dump
    /// worker takes down the whole run mid-stream, leaving a partial dump;
    /// an `Err` is reported and attributable to a field.
    #[test]
    fn no_transformer_panics_on_any_bson_input(
        value in strategies::value(),
        doc in strategies::document(),
    ) {
        for (name, params) in all_transformer_params() {
            let engine = HashEngine::new("pepper");
            let Ok(transformer) = Registry::with_builtins().instantiate(&name, &params, &engine)
            else {
                continue;
            };
            // Err is an acceptable outcome; unwinding is not. proptest runs
            // each case in-process, so a panic here fails the test with the
            // shrunk input that triggered it.
            let _ = transformer.transform(&value, &doc);
        }
    }

    /// Different salts must actually change output. A transformer that ignores
    /// the engine would silently produce identical "anonymized" values across
    /// every deployment sharing this tool — the salt would be decorative.
    #[test]
    fn salt_changes_output_for_hash_derived_transformers(
        value in strategies::scalar(),
    ) {
        // Only transformers whose output is hash-derived by contract; the
        // others (set_null, replace, masking with a fixed char) are constant
        // by design.
        const HASH_DERIVED: &[&str] = &[
            "hash",
            "random_int",
            "random_float",
            "random_email",
            "random_person",
            "random_object_id",
            "random_bytes",
        ];
        let params_by_name = all_transformer_params();
        let mut differed = 0;
        for (name, params) in &params_by_name {
            if !HASH_DERIVED.contains(&name.as_str()) {
                continue;
            }
            let a = Registry::with_builtins()
                .instantiate(name, params, &HashEngine::new("salt-a"));
            let b = Registry::with_builtins()
                .instantiate(name, params, &HashEngine::new("salt-b"));
            let (Ok(a), Ok(b)) = (a, b) else { continue };
            let doc = Document::new();
            if let (Ok(x), Ok(y)) = (a.transform(&value, &doc), b.transform(&value, &doc)) {
                if x != y {
                    differed += 1;
                }
            }
        }
        // Some inputs legitimately collide for a given transformer (a small
        // `random_int` range, say), so require the salt to matter *somewhere*
        // rather than everywhere.
        prop_assert!(
            differed > 0,
            "no hash-derived transformer changed output when the salt changed, for {value:?}"
        );
    }
}

/// Every transformer in the registry must build from its own declared
/// parameter contract. A factory whose declared parameters do not actually
/// satisfy its builder is a catalog lie: `list-transformers` documents
/// something the user cannot instantiate.
#[test]
fn every_transformer_instantiates_from_its_declared_parameters() {
    let engine = HashEngine::new("pepper");
    let mut failures = Vec::new();
    for (name, params) in all_transformer_params() {
        if let Err(e) = Registry::with_builtins().instantiate(&name, &params, &engine) {
            failures.push(format!("{name}: {e}"));
        }
    }
    assert!(
        failures.is_empty(),
        "transformers that cannot be built from their declared parameters:\n  {}",
        failures.join("\n  ")
    );
}

/// The catalog must stay self-describing: every transformer has a non-empty
/// description, and every parameter is documented and typed. This is what
/// `show-transformer` prints, and it is the only contract a config author has.
#[test]
fn catalog_documents_every_transformer_and_parameter() {
    let registry = Registry::with_builtins();
    let docs = registry.docs();
    assert!(!docs.is_empty(), "registry has no built-in transformers");

    for doc in docs {
        assert!(
            !doc.description.trim().is_empty(),
            "transformer '{}' has no description",
            doc.name
        );
        for param in &doc.parameters {
            assert!(
                !param.description.trim().is_empty(),
                "parameter '{}' of '{}' has no description",
                param.name,
                doc.name
            );
            assert!(
                param.required || param.default.is_some(),
                "parameter '{}' of '{}' is optional but declares no default",
                param.name,
                doc.name
            );
        }
    }
}
