//! Validate and preview transformations without dumping
//! (feature `validate.transformation-preview`).
//!
//! Runs the configured transformations against a limited sample of documents and
//! renders a before/after diff — in text or JSON, with per-document diffs laid
//! out vertically or horizontally — without producing a real dump. `--format`
//! and `--table-format` are validated up front, before any database work.
//! `--transformed-only` restricts the preview to fields that actually have a
//! transformer. `--strict` turns any unresolved warning into a non-zero exit.

use bson::{Bson, Document};

use crate::error::{Error, Result};
use crate::hash::HashEngine;
use crate::transform::{apply::TransformationPlan, Registry};

use super::warnings::Warnings;

/// Output serialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
}

/// Per-document layout for text output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableFormat {
    Vertical,
    Horizontal,
}

/// Parse `--format`, failing fast on an unknown value.
pub fn parse_format(s: &str) -> Result<Format> {
    match s {
        "text" => Ok(Format::Text),
        "json" => Ok(Format::Json),
        other => Err(Error::Validation(format!(
            "invalid --format '{other}' (expected text or json)"
        ))),
    }
}

/// Parse `--table-format`, failing fast on an unknown value.
pub fn parse_table_format(s: &str) -> Result<TableFormat> {
    match s {
        "vertical" => Ok(TableFormat::Vertical),
        "horizontal" => Ok(TableFormat::Horizontal),
        other => Err(Error::Validation(format!(
            "invalid --table-format '{other}' (expected vertical or horizontal)"
        ))),
    }
}

/// Options for a preview run.
#[derive(Debug, Clone)]
pub struct PreviewOptions {
    pub rows_limit: usize,
    pub format: Format,
    pub table_format: TableFormat,
    pub transformed_only: bool,
    pub strict: bool,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        PreviewOptions {
            rows_limit: 10,
            format: Format::Text,
            table_format: TableFormat::Vertical,
            transformed_only: false,
            strict: false,
        }
    }
}

/// One field's before/after in the preview.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDiff {
    pub field: String,
    pub before: Bson,
    pub after: Bson,
    pub changed: bool,
}

/// Run the preview and render it to a string. `--strict` with any unresolved
/// warning fails the command.
#[allow(clippy::too_many_arguments)] // each input is a distinct, unrelated concern
pub fn preview(
    plan: &TransformationPlan,
    registry: &Registry,
    engine: &HashEngine,
    collection: &str,
    docs: &[Document],
    warnings: &Warnings,
    resolved: &[String],
    opts: &PreviewOptions,
) -> Result<String> {
    if opts.strict && warnings.has_unresolved(resolved) {
        return Err(Error::Validation(
            "unresolved validation warnings (--strict)".into(),
        ));
    }

    let fields: Vec<String> = plan.transformed_fields(collection);
    let mut per_doc: Vec<Vec<FieldDiff>> = Vec::new();

    for doc in docs.iter().take(opts.rows_limit) {
        let transformed = plan.transform(registry, engine, collection, doc)?;
        let field_names: Vec<String> = if opts.transformed_only {
            fields.clone()
        } else {
            doc.keys().cloned().collect()
        };
        let mut diffs = Vec::new();
        for field in field_names {
            let before = doc.get(&field).cloned().unwrap_or(Bson::Null);
            let after = transformed.get(&field).cloned().unwrap_or(Bson::Null);
            let changed = before != after;
            diffs.push(FieldDiff {
                field,
                before,
                after,
                changed,
            });
        }
        per_doc.push(diffs);
    }

    Ok(match opts.format {
        Format::Json => render_json(&per_doc),
        Format::Text => render_text(&per_doc, opts.table_format),
    })
}

fn render_json(per_doc: &[Vec<FieldDiff>]) -> String {
    let docs: Vec<serde_json::Value> = per_doc
        .iter()
        .map(|diffs| {
            serde_json::Value::Array(
                diffs
                    .iter()
                    .map(|d| {
                        serde_json::json!({
                            "field": d.field,
                            "before": d.before.to_string(),
                            "after": d.after.to_string(),
                            "changed": d.changed,
                        })
                    })
                    .collect(),
            )
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Array(docs)).unwrap_or_default()
}

fn render_text(per_doc: &[Vec<FieldDiff>], layout: TableFormat) -> String {
    let mut out = String::new();
    for (i, diffs) in per_doc.iter().enumerate() {
        out.push_str(&format!("document #{i}\n"));
        match layout {
            TableFormat::Vertical => {
                for d in diffs {
                    let mark = if d.changed { "*" } else { " " };
                    out.push_str(&format!(
                        "  {mark} {}: {} -> {}\n",
                        d.field, d.before, d.after
                    ));
                }
            }
            TableFormat::Horizontal => {
                let header: Vec<&str> = diffs.iter().map(|d| d.field.as_str()).collect();
                let before: Vec<String> = diffs.iter().map(|d| d.before.to_string()).collect();
                let after: Vec<String> = diffs.iter().map(|d| d.after.to_string()).collect();
                out.push_str(&format!("  field : {}\n", header.join(" | ")));
                out.push_str(&format!("  before: {}\n", before.join(" | ")));
                out.push_str(&format!("  after : {}\n", after.join(" | ")));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform::apply::TransformationConfig;
    use crate::validate::warnings::{Severity, Warning};

    fn setup() -> (TransformationPlan, Registry, HashEngine) {
        let registry = Registry::with_builtins();
        let engine = HashEngine::new("salt");
        let configs: Vec<TransformationConfig> = serde_yaml::from_str(
            "- collection: users\n  transformers:\n    - field: email\n      name: masking\n",
        )
        .unwrap();
        let plan = TransformationPlan::compile(&configs, &registry, &engine).unwrap();
        (plan, registry, engine)
    }

    fn docs() -> Vec<Document> {
        (0..3)
            .map(|i| {
                let mut d = Document::new();
                d.insert("_id", Bson::Int64(i));
                d.insert("email", Bson::String(format!("user{i}@x.com")));
                d.insert("age", Bson::Int64(30 + i));
                d
            })
            .collect()
    }

    // Acceptance: shows original and transformed value per field, up to
    // --rows-limit documents.
    #[test]
    fn shows_before_after_up_to_rows_limit() {
        let (plan, r, e) = setup();
        let opts = PreviewOptions {
            rows_limit: 2,
            ..Default::default()
        };
        let out = preview(
            &plan,
            &r,
            &e,
            "users",
            &docs(),
            &Warnings::new(),
            &[],
            &opts,
        )
        .unwrap();
        // only 2 of 3 documents.
        assert!(out.contains("document #0"));
        assert!(out.contains("document #1"));
        assert!(!out.contains("document #2"));
        // shows the email transform before -> after (masked to same length).
        assert!(out.contains("email:"));
        assert!(out.contains("user0@x.com")); // original value shown
        assert!(out.contains(&"*".repeat("user0@x.com".len()))); // masked value shown
    }

    // Acceptance: text/json formats and vertical/horizontal layouts.
    #[test]
    fn supports_formats_and_layouts() {
        let (plan, r, e) = setup();
        let d = docs();

        let json = preview(
            &plan,
            &r,
            &e,
            "users",
            &d,
            &Warnings::new(),
            &[],
            &PreviewOptions {
                format: Format::Json,
                ..Default::default()
            },
        )
        .unwrap();
        // valid JSON.
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_array());

        let horizontal = preview(
            &plan,
            &r,
            &e,
            "users",
            &d,
            &Warnings::new(),
            &[],
            &PreviewOptions {
                table_format: TableFormat::Horizontal,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(horizontal.contains("before:"));
        assert!(horizontal.contains("after :"));
    }

    // Acceptance: --transformed-only restricts to configured fields.
    #[test]
    fn transformed_only_restricts_fields() {
        let (plan, r, e) = setup();
        let opts = PreviewOptions {
            transformed_only: true,
            ..Default::default()
        };
        let out = preview(
            &plan,
            &r,
            &e,
            "users",
            &docs(),
            &Warnings::new(),
            &[],
            &opts,
        )
        .unwrap();
        assert!(out.contains("email:"));
        // age has no transformer -> excluded from transformed-only view.
        assert!(!out.contains("age:"));
    }

    // Acceptance: invalid --format / --table-format fail fast, before any work.
    #[test]
    fn invalid_format_options_fail_fast() {
        assert!(parse_format("yaml").is_err());
        assert!(parse_table_format("diagonal").is_err());
        assert_eq!(parse_format("json").unwrap(), Format::Json);
        assert_eq!(
            parse_table_format("vertical").unwrap(),
            TableFormat::Vertical
        );
    }

    // Acceptance: --strict makes an unresolved warning fail the command.
    #[test]
    fn strict_fails_on_unresolved_warning() {
        let (plan, r, e) = setup();
        let mut warnings = Warnings::new();
        warnings.push(Warning::new(Severity::Warning, "risky"));
        let opts = PreviewOptions {
            strict: true,
            ..Default::default()
        };
        let err = preview(&plan, &r, &e, "users", &docs(), &warnings, &[], &opts).unwrap_err();
        assert!(err.to_string().contains("strict"), "{err}");

        // resolved -> no failure.
        let resolved: Vec<String> = warnings.all().iter().map(|w| w.id.clone()).collect();
        assert!(preview(&plan, &r, &e, "users", &docs(), &warnings, &resolved, &opts).is_ok());
    }
}
