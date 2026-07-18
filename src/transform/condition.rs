//! Conditional transformation (feature `transform.transformation-condition`).
//!
//! A `when` expression gates a transformer, or an entire collection's
//! transformation entry: it only applies to documents for which the expression
//! is truthy; other documents pass through unmodified. Expressions reference the
//! document's own fields.
//!
//! Gap: the source evaluates full Go/expr expressions. This evaluator supports
//! field references, the comparison operators `== != > < >= <=`, boolean
//! `and`/`or`, and bare-field truthiness — no parentheses or arithmetic. See
//! regeneration-gaps.md.

use bson::{Bson, Document};

use crate::error::{Error, Result};

/// A parsed `when` condition.
#[derive(Debug, Clone)]
pub struct Condition {
    expr: String,
}

impl Condition {
    /// Parse a condition expression. Parsing is lenient; evaluation resolves
    /// fields against each document.
    pub fn parse(expr: &str) -> Result<Self> {
        if expr.trim().is_empty() {
            return Err(Error::Transform("empty when expression".into()));
        }
        Ok(Condition {
            expr: expr.to_string(),
        })
    }

    /// Evaluate the condition against a document.
    pub fn eval(&self, doc: &Document) -> bool {
        eval_or(&self.expr, doc)
    }
}

fn eval_or(expr: &str, doc: &Document) -> bool {
    split_top(expr, " or ").iter().any(|c| eval_and(c, doc))
}

fn eval_and(expr: &str, doc: &Document) -> bool {
    split_top(expr, " and ")
        .iter()
        .all(|c| eval_term(c.trim(), doc))
}

/// Split on `sep`, but only outside of single/double quotes.
fn split_top(expr: &str, sep: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let bytes: Vec<char> = expr.chars().collect();
    let sep_chars: Vec<char> = sep.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            current.push(c);
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            current.push(c);
            i += 1;
            continue;
        }
        if bytes[i..].starts_with(&sep_chars[..]) {
            parts.push(current.clone());
            current.clear();
            i += sep_chars.len();
            continue;
        }
        current.push(c);
        i += 1;
    }
    parts.push(current);
    parts
}

fn eval_term(term: &str, doc: &Document) -> bool {
    for op in ["==", "!=", ">=", "<=", ">", "<"] {
        if let Some(idx) = find_op(term, op) {
            let lhs = term[..idx].trim();
            let rhs = term[idx + op.len()..].trim();
            return eval_compare(lhs, op, rhs, doc);
        }
    }
    // bare field -> truthiness.
    truthy(get_path(doc, term.trim()))
}

/// Find `op` at the top level (outside quotes).
fn find_op(term: &str, op: &str) -> Option<usize> {
    let bytes: Vec<char> = term.chars().collect();
    let op_chars: Vec<char> = op.chars().collect();
    let mut quote: Option<char> = None;
    let mut byte_idx = 0;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else if bytes[i..].starts_with(&op_chars[..]) {
            // avoid matching '>' inside '>=' etc.: caller tries longer ops first.
            return Some(byte_idx);
        }
        byte_idx += c.len_utf8();
        i += 1;
    }
    None
}

fn eval_compare(lhs: &str, op: &str, rhs: &str, doc: &Document) -> bool {
    let left = get_path(doc, lhs).cloned().unwrap_or(Bson::Null);
    let right = parse_literal(rhs);
    match op {
        "==" => bson_eq(&left, &right),
        "!=" => !bson_eq(&left, &right),
        _ => match (as_f64(&left), as_f64(&right)) {
            (Some(a), Some(b)) => match op {
                ">" => a > b,
                "<" => a < b,
                ">=" => a >= b,
                "<=" => a <= b,
                _ => false,
            },
            _ => false,
        },
    }
}

fn parse_literal(s: &str) -> Bson {
    let s = s.trim();
    if (s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"')) {
        return Bson::String(s[1..s.len() - 1].to_string());
    }
    match s {
        "true" => return Bson::Boolean(true),
        "false" => return Bson::Boolean(false),
        "null" => return Bson::Null,
        _ => {}
    }
    if let Ok(i) = s.parse::<i64>() {
        return Bson::Int64(i);
    }
    if let Ok(f) = s.parse::<f64>() {
        return Bson::Double(f);
    }
    Bson::String(s.to_string())
}

fn as_f64(b: &Bson) -> Option<f64> {
    match b {
        Bson::Int32(v) => Some(*v as f64),
        Bson::Int64(v) => Some(*v as f64),
        Bson::Double(v) => Some(*v),
        _ => None,
    }
}

fn bson_eq(a: &Bson, b: &Bson) -> bool {
    if let (Some(x), Some(y)) = (as_f64(a), as_f64(b)) {
        return x == y;
    }
    a == b
}

fn truthy(v: Option<&Bson>) -> bool {
    match v {
        None | Some(Bson::Null) => false,
        Some(Bson::Boolean(b)) => *b,
        Some(Bson::String(s)) => !s.is_empty(),
        Some(Bson::Int32(n)) => *n != 0,
        Some(Bson::Int64(n)) => *n != 0,
        Some(Bson::Double(n)) => *n != 0.0,
        Some(_) => true,
    }
}

fn get_path<'a>(doc: &'a Document, path: &str) -> Option<&'a Bson> {
    let mut parts = path.split('.');
    let mut current = doc.get(parts.next()?)?;
    for part in parts {
        current = current.as_document()?.get(part)?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(pairs: &[(&str, Bson)]) -> Document {
        let mut d = Document::new();
        for (k, v) in pairs {
            d.insert(*k, v.clone());
        }
        d
    }

    // Acceptance: a `when` runs only for documents where it is truthy; others
    // are excluded (the caller then passes them through unmodified).
    #[test]
    fn gates_on_field_equality() {
        let c = Condition::parse("status == 'active'").unwrap();
        assert!(c.eval(&doc(&[("status", Bson::String("active".into()))])));
        assert!(!c.eval(&doc(&[("status", Bson::String("inactive".into()))])));
        // absent field is not equal.
        assert!(!c.eval(&doc(&[])));
    }

    // Acceptance: the condition references the document's own field values,
    // including numeric comparisons and boolean composition.
    #[test]
    fn numeric_and_boolean_composition() {
        let c = Condition::parse("age >= 18 and country == 'US'").unwrap();
        assert!(c.eval(&doc(&[
            ("age", Bson::Int64(21)),
            ("country", Bson::String("US".into()))
        ])));
        assert!(!c.eval(&doc(&[
            ("age", Bson::Int64(16)),
            ("country", Bson::String("US".into()))
        ])));

        let c = Condition::parse("age < 13 or vip == true").unwrap();
        assert!(c.eval(&doc(&[
            ("age", Bson::Int64(10)),
            ("vip", Bson::Boolean(false))
        ])));
        assert!(c.eval(&doc(&[
            ("age", Bson::Int64(40)),
            ("vip", Bson::Boolean(true))
        ])));
        assert!(!c.eval(&doc(&[
            ("age", Bson::Int64(40)),
            ("vip", Bson::Boolean(false))
        ])));
    }

    // Acceptance: a collection-level `when` can skip the whole document — same
    // evaluation, used by the caller to bypass transformation entirely.
    #[test]
    fn bare_field_truthiness_and_nested_paths() {
        let c = Condition::parse("flags.enabled").unwrap();
        let mut inner = Document::new();
        inner.insert("enabled", true);
        assert!(c.eval(&doc(&[("flags", Bson::Document(inner))])));

        let mut inner = Document::new();
        inner.insert("enabled", false);
        assert!(!c.eval(&doc(&[("flags", Bson::Document(inner))])));

        assert!(Condition::parse("").is_err());
    }
}
