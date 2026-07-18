//! Transformer catalog (features `catalog.list-transformers`,
//! `catalog.show-transformer`).
//!
//! Discovery over the [`Registry`]: list every transformer (built-in and any
//! custom transformer declared in the loaded config) with a short description,
//! or show one transformer's full parameter documentation.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::hash::HashEngine;
use crate::transform::{custom::register_custom, Registry};

/// Build a registry of built-ins plus any custom transformers declared in the
/// config's `custom_transformers` section.
pub fn build_registry(config: Option<&Config>) -> Result<Registry> {
    let mut registry = Registry::with_builtins();
    if let Some(cfg) = config {
        if !cfg.custom_transformers.is_null() {
            let defs: Vec<crate::transform::custom::CustomTransformerDef> =
                serde_yaml::from_value(cfg.custom_transformers.clone())
                    .map_err(|e| Error::Config(format!("custom_transformers: {e}")))?;
            register_custom(&mut registry, &defs)?;
        }
    }
    Ok(registry)
}

/// Render the transformer listing: one `name — description` line per
/// transformer, sorted by name.
pub fn list(registry: &Registry) -> String {
    registry
        .docs()
        .into_iter()
        .map(|d| format!("{} — {}", d.name, d.description))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render one transformer's full documentation, or a clear not-found error.
pub fn show(registry: &Registry, name: &str) -> Result<String> {
    let factory = registry
        .get(name)
        .ok_or_else(|| Error::NotFound(format!("transformer '{name}' not found")))?;
    let doc = factory.doc();
    let mut out = format!("{}\n  {}\n", doc.name, doc.description);
    if doc.parameters.is_empty() {
        out.push_str("  (no parameters)\n");
    } else {
        out.push_str("  parameters:\n");
        for p in &doc.parameters {
            let req = if p.required { "required" } else { "optional" };
            out.push_str(&format!(
                "    - {} : {} ({}) — {}\n",
                p.name,
                p.kind.name(),
                req,
                p.description
            ));
        }
    }
    Ok(out)
}

/// Helper used by the CLI: a registry needs an engine only to instantiate;
/// listing/showing does not, but callers often have one handy.
pub fn default_engine() -> HashEngine {
    HashEngine::new("catalog")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Acceptance (list): every built-in appears with its description; custom
    // transformers from config appear too.
    #[test]
    fn lists_builtin_and_custom() {
        let cfg: Config = serde_yaml::from_str(
            "custom_transformers:\n  - name: my_masker\n    driver: template\n    template: 'x'\n",
        )
        .unwrap();
        let registry = build_registry(Some(&cfg)).unwrap();
        let listing = list(&registry);
        assert!(listing.contains("hash — "));
        assert!(listing.contains("random_person — "));
        assert!(listing.contains("my_masker — "));
    }

    // Acceptance (show): a known name prints params, their types, and whether
    // they are required.
    #[test]
    fn shows_known_transformer_params() {
        let registry = build_registry(None).unwrap();
        let text = show(&registry, "random_int").unwrap();
        assert!(text.contains("random_int"));
        assert!(text.contains("min : int (required)"));
        assert!(text.contains("max : int (required)"));

        // a paramless transformer is rendered clearly.
        assert!(show(&registry, "set_null")
            .unwrap()
            .contains("no parameters"));
    }

    // Acceptance (show): an unknown name fails with a clear not-found error.
    #[test]
    fn unknown_transformer_is_not_found() {
        let registry = build_registry(None).unwrap();
        let err = show(&registry, "does_not_exist").unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
        assert!(err.to_string().contains("does_not_exist"));
    }
}
