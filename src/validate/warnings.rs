//! Transformer configuration warnings (feature `validate.warnings`).
//!
//! Validation surfaces structured, individually-identifiable warnings rather
//! than opaque errors. Each warning has a stable `id` derived from its content,
//! so the operator can list it under `resolved_warnings` in the config to stop
//! it being reported. `--warnings` forces every warning to be shown, including
//! resolved ones.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

/// How serious a warning is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

/// A single structured validation warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// Stable content-derived identifier (listable in `resolved_warnings`).
    pub id: String,
    pub severity: Severity,
    pub message: String,
    /// Structured context (e.g. collection, field, transformer).
    pub meta: BTreeMap<String, String>,
}

impl Warning {
    /// Create a warning; its `id` is computed from severity + message + meta so
    /// the same issue always produces the same identifier across runs.
    pub fn new(severity: Severity, message: impl Into<String>) -> Self {
        let message = message.into();
        let id = compute_id(severity, &message, &BTreeMap::new());
        Warning {
            id,
            severity,
            message,
            meta: BTreeMap::new(),
        }
    }

    /// Attach a piece of structured context (recomputes the id).
    pub fn with_meta(mut self, key: &str, value: impl Into<String>) -> Self {
        self.meta.insert(key.to_string(), value.into());
        self.id = compute_id(self.severity, &self.message, &self.meta);
        self
    }

    /// Whether this warning is at least `Error` severity.
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

fn compute_id(severity: Severity, message: &str, meta: &BTreeMap<String, String>) -> String {
    let mut h = Sha256::new();
    h.update(severity.as_str().as_bytes());
    h.update(b"\0");
    h.update(message.as_bytes());
    // BTreeMap iterates in sorted key order → deterministic.
    for (k, v) in meta {
        h.update(b"\0");
        h.update(k.as_bytes());
        h.update(b"=");
        h.update(v.as_bytes());
    }
    hex::encode(&h.finalize()[..6])
}

/// A collection of warnings produced during validation.
#[derive(Debug, Clone, Default)]
pub struct Warnings {
    items: Vec<Warning>,
}

impl Warnings {
    pub fn new() -> Self {
        Warnings::default()
    }

    pub fn push(&mut self, w: Warning) {
        self.items.push(w);
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn all(&self) -> &[Warning] {
        &self.items
    }

    /// The warnings that should actually be shown to the operator: everything
    /// when `show_all` (the `--warnings` flag) is set, otherwise everything
    /// except those whose id appears in `resolved`.
    pub fn visible<'a>(&'a self, resolved: &[String], show_all: bool) -> Vec<&'a Warning> {
        self.items
            .iter()
            .filter(|w| show_all || !resolved.iter().any(|r| r == &w.id))
            .collect()
    }

    /// Whether any warning remains unresolved (used by `--strict`).
    pub fn has_unresolved(&self, resolved: &[String]) -> bool {
        self.items
            .iter()
            .any(|w| !resolved.iter().any(|r| r == &w.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Acceptance: a detected issue is a distinct, identifiable warning.
    #[test]
    fn warnings_are_identifiable_and_distinct() {
        let a = Warning::new(Severity::Warning, "field has no transformer")
            .with_meta("collection", "users")
            .with_meta("field", "ssn");
        let b = Warning::new(Severity::Warning, "field has no transformer")
            .with_meta("collection", "orders")
            .with_meta("field", "total");

        assert!(!a.id.is_empty());
        assert_ne!(a.id, b.id, "different context -> different id");

        // id is stable: rebuilding the same warning yields the same id.
        let a2 = Warning::new(Severity::Warning, "field has no transformer")
            .with_meta("field", "ssn")
            .with_meta("collection", "users");
        assert_eq!(a.id, a2.id);
    }

    // Acceptance: a resolved warning id is no longer shown.
    #[test]
    fn resolved_warnings_are_suppressed() {
        let mut ws = Warnings::new();
        let w = Warning::new(Severity::Warning, "risky config");
        let id = w.id.clone();
        ws.push(w);
        ws.push(Warning::new(Severity::Warning, "another"));

        let visible = ws.visible(&[id.clone()], false);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].message, "another");

        // and with everything resolved, nothing is unresolved (for --strict).
        let all_ids: Vec<String> = ws.all().iter().map(|w| w.id.clone()).collect();
        assert!(!ws.has_unresolved(&all_ids));
        assert!(ws.has_unresolved(&[id]));
    }

    // Acceptance: with --warnings, all warnings are printed even if resolved.
    #[test]
    fn show_all_overrides_resolution() {
        let mut ws = Warnings::new();
        let w = Warning::new(Severity::Warning, "risky config");
        let id = w.id.clone();
        ws.push(w);
        // resolved would normally hide it, but show_all keeps it.
        assert_eq!(ws.visible(&[id], true).len(), 1);
    }
}
