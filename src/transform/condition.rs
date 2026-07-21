//! Conditional transformation (feature `transform.transformation-condition`).
//!
//! A `when` expression gates a transformer, or an entire collection's
//! transformation entry: it only applies to documents for which the expression
//! is truthy; other documents pass through unmodified. Expressions reference the
//! document's own fields.
//!
//! Gap: the source evaluates full Go/expr expressions. This evaluator supports
//! field references, the comparison operators `== != > < >= <=`, list
//! membership (`in` / `not in` against a bracketed literal list, e.g. `age in
//! [18, 21]`), boolean `and`/`or`, and bare-field truthiness — no parentheses
//! or arithmetic. See regeneration-gaps.md.

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

    /// Translate this condition into a native MongoDB filter document, for
    /// server-side pushdown (e.g. `subset_conds` on a real dump) instead of
    /// fetching every document and evaluating [`Self::eval`] in Rust.
    pub fn to_filter(&self) -> Document {
        filter_or(&self.expr)
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
    for op in ["not in", "in"] {
        if let Some(idx) = find_word_op(term, op) {
            let lhs = term[..idx].trim();
            let rhs = term[idx + op.len()..].trim();
            return eval_compare(lhs, op, rhs, doc);
        }
    }
    // bare field -> truthiness.
    truthy(get_path(doc, term.trim()))
}

fn filter_or(expr: &str) -> Document {
    let mut parts: Vec<Document> = split_top(expr, " or ")
        .iter()
        .map(|c| filter_and(c))
        .collect();
    if parts.len() == 1 {
        return parts.remove(0);
    }
    let mut d = Document::new();
    d.insert(
        "$or",
        parts.into_iter().map(Bson::Document).collect::<Vec<_>>(),
    );
    d
}

fn filter_and(expr: &str) -> Document {
    let mut parts: Vec<Document> = split_top(expr, " and ")
        .iter()
        .map(|c| filter_term(c.trim()))
        .collect();
    if parts.len() == 1 {
        return parts.remove(0);
    }
    let mut d = Document::new();
    d.insert(
        "$and",
        parts.into_iter().map(Bson::Document).collect::<Vec<_>>(),
    );
    d
}

fn filter_term(term: &str) -> Document {
    for op in ["==", "!=", ">=", "<=", ">", "<"] {
        if let Some(idx) = find_op(term, op) {
            let lhs = term[..idx].trim();
            let rhs = term[idx + op.len()..].trim();
            return filter_compare(lhs, op, rhs);
        }
    }
    for op in ["not in", "in"] {
        if let Some(idx) = find_word_op(term, op) {
            let lhs = term[..idx].trim();
            let rhs = term[idx + op.len()..].trim();
            return filter_compare(lhs, op, rhs);
        }
    }
    // bare field -> truthiness: present, and not one of the "falsy" values.
    let mut cond = Document::new();
    cond.insert("$exists", true);
    cond.insert(
        "$nin",
        vec![
            Bson::Null,
            Bson::Boolean(false),
            Bson::Int32(0),
            Bson::String(String::new()),
        ],
    );
    let mut d = Document::new();
    d.insert(term.trim(), cond);
    d
}

fn filter_compare(lhs: &str, op: &str, rhs: &str) -> Document {
    let mut d = Document::new();
    if op == "in" || op == "not in" {
        let list: Vec<Bson> = parse_list(rhs)
            .unwrap_or_default()
            .iter()
            .map(|v| match v {
                Bson::String(s) => literal_to_filter_bson(&format!("'{s}'")),
                other => other.clone(),
            })
            .collect();
        let mongo_op = if op == "in" { "$in" } else { "$nin" };
        let mut inner = Document::new();
        inner.insert(mongo_op, list);
        d.insert(lhs, inner);
        return d;
    }
    let value = literal_to_filter_bson(rhs);
    match op {
        "==" => {
            d.insert(lhs, value);
        }
        "!=" => {
            let mut inner = Document::new();
            inner.insert("$ne", value);
            d.insert(lhs, inner);
        }
        ">" | "<" | ">=" | "<=" => {
            let mongo_op = match op {
                ">" => "$gt",
                "<" => "$lt",
                ">=" => "$gte",
                _ => "$lte",
            };
            let mut inner = Document::new();
            inner.insert(mongo_op, value);
            d.insert(lhs, inner);
        }
        _ => unreachable!("filter_term only dispatches known comparison operators"),
    }
    d
}

/// Like [`parse_literal`], but a quoted value that parses as RFC3339 becomes a
/// real BSON datetime, so a range comparison against a `Bson::DateTime` field
/// matches server-side (mirrors the coercion `eval_compare` does via
/// `as_millis` for the in-memory evaluator).
fn literal_to_filter_bson(s: &str) -> Bson {
    match parse_literal(s) {
        Bson::String(text) => match bson::DateTime::parse_rfc3339_str(&text) {
            Ok(dt) => Bson::DateTime(dt),
            Err(_) => Bson::String(text),
        },
        other => other,
    }
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

/// Like [`find_op`], but only matches `op` (an alphabetic word or
/// space-separated phrase like `"not in"`) when it's not embedded inside a
/// larger identifier — the character before and after the match, if any,
/// must not be alphanumeric or `_`.
fn find_word_op(term: &str, op: &str) -> Option<usize> {
    let chars: Vec<char> = term.chars().collect();
    let op_chars: Vec<char> = op.chars().collect();
    let mut quote: Option<char> = None;
    let mut byte_idx = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else if chars[i..].starts_with(&op_chars[..]) {
            let before_ok = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
            let after_idx = i + op_chars.len();
            let after_ok = after_idx >= chars.len()
                || !(chars[after_idx].is_alphanumeric() || chars[after_idx] == '_');
            if before_ok && after_ok {
                return Some(byte_idx);
            }
        }
        byte_idx += c.len_utf8();
        i += 1;
    }
    None
}

/// Parse a bracketed literal list, e.g. `['gery', 'sophie']` or `[30, 20]`,
/// into per-element `Bson` values using the same literal grammar as a scalar
/// RHS. Returns `None` if `s` isn't bracket-delimited.
fn parse_list(s: &str) -> Option<Vec<Bson>> {
    let s = s.trim();
    if !s.starts_with('[') || !s.ends_with(']') {
        return None;
    }
    let interior = &s[1..s.len() - 1];
    if interior.trim().is_empty() {
        return Some(Vec::new());
    }
    Some(
        split_top(interior, ",")
            .iter()
            .map(|piece| parse_literal(piece.trim()))
            .collect(),
    )
}

fn eval_compare(lhs: &str, op: &str, rhs: &str, doc: &Document) -> bool {
    let left = get_path(doc, lhs).cloned().unwrap_or(Bson::Null);
    match op {
        "in" | "not in" => {
            let list = parse_list(rhs).unwrap_or_default();
            let found = list.iter().any(|v| bson_eq(&left, v));
            if op == "in" {
                found
            } else {
                !found
            }
        }
        _ => {
            let right = parse_literal(rhs);
            match op {
                "==" => bson_eq(&left, &right),
                "!=" => !bson_eq(&left, &right),
                _ => {
                    let nums = as_f64(&left).zip(as_f64(&right)).or_else(|| {
                        as_millis(&left)
                            .zip(as_millis(&right))
                            .map(|(a, b)| (a as f64, b as f64))
                    });
                    match nums {
                        Some((a, b)) => match op {
                            ">" => a > b,
                            "<" => a < b,
                            ">=" => a >= b,
                            "<=" => a <= b,
                            _ => false,
                        },
                        None => false,
                    }
                }
            }
        }
    }
}

/// Coerce a BSON datetime, or a string parseable as RFC3339, to epoch millis.
/// Lets `created_at >= '2026-07-16T22:00:00Z'`-style range conditions compare
/// a document's real `Bson::DateTime` against a quoted literal.
fn as_millis(b: &Bson) -> Option<i64> {
    match b {
        Bson::DateTime(dt) => Some(dt.timestamp_millis()),
        Bson::String(s) => bson::DateTime::parse_rfc3339_str(s)
            .ok()
            .map(|dt| dt.timestamp_millis()),
        _ => None,
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
    if let (Some(x), Some(y)) = (as_millis(a), as_millis(b)) {
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

    // Acceptance: a datetime field can be range-filtered against RFC3339
    // literals, e.g. for a subset_conds day-window condition.
    #[test]
    fn datetime_range_comparison() {
        let c = Condition::parse(
            "created_at >= '2026-07-17T00:00:00Z' and created_at < '2026-07-18T00:00:00Z'",
        )
        .unwrap();
        let in_range =
            Bson::DateTime(bson::DateTime::parse_rfc3339_str("2026-07-17T12:00:00Z").unwrap());
        let before =
            Bson::DateTime(bson::DateTime::parse_rfc3339_str("2026-07-16T23:59:59Z").unwrap());
        let after =
            Bson::DateTime(bson::DateTime::parse_rfc3339_str("2026-07-18T00:00:00Z").unwrap());
        assert!(c.eval(&doc(&[("created_at", in_range)])));
        assert!(!c.eval(&doc(&[("created_at", before)])));
        assert!(!c.eval(&doc(&[("created_at", after)])));
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

    // Acceptance: to_filter translates the same expression subset_conds uses
    // into a native MongoDB filter, so a dump can push the condition down to
    // the server instead of fetching the whole collection first.
    #[test]
    fn to_filter_translates_comparisons_and_boolean_composition() {
        let c = Condition::parse("status == 'active'").unwrap();
        let mut want = Document::new();
        want.insert("status", "active");
        assert_eq!(c.to_filter(), want);

        let c = Condition::parse("age >= 18 and country == 'US'").unwrap();
        let mut age_cond = Document::new();
        age_cond.insert("$gte", 18i64);
        let mut age_doc = Document::new();
        age_doc.insert("age", age_cond);
        let mut country_doc = Document::new();
        country_doc.insert("country", "US");
        let mut want = Document::new();
        want.insert(
            "$and",
            vec![Bson::Document(age_doc), Bson::Document(country_doc)],
        );
        assert_eq!(c.to_filter(), want);
    }

    // Acceptance: a quoted RFC3339 literal becomes a real BSON datetime in the
    // filter, so a range condition on a datetime field actually matches
    // server-side (a plain string literal would never equal a Bson::DateTime).
    #[test]
    fn to_filter_coerces_rfc3339_literals_to_datetime() {
        let c = Condition::parse(
            "created_at >= '2026-07-17T00:00:00Z' and created_at < '2026-07-18T00:00:00Z'",
        )
        .unwrap();
        let filter = c.to_filter();
        let and = filter.get_array("$and").unwrap();
        let lower = and[0]
            .as_document()
            .unwrap()
            .get_document("created_at")
            .unwrap();
        assert_eq!(
            lower.get("$gte"),
            Some(&Bson::DateTime(
                bson::DateTime::parse_rfc3339_str("2026-07-17T00:00:00Z").unwrap()
            ))
        );
    }

    // Acceptance: a word-operator like "in" must not false-match inside an
    // identifier substring (e.g. "domain" contains "in" as raw text) — only
    // a whitespace/boundary-delimited occurrence counts.
    #[test]
    fn find_word_op_respects_boundaries() {
        assert_eq!(find_word_op("age in", "in"), Some(4));
        assert_eq!(find_word_op("domain in", "in"), Some(7));
        assert_eq!(find_word_op("domain", "in"), None);
        assert_eq!(find_word_op("training == true", "in"), None);
    }

    // Acceptance: a bracketed literal list becomes a Vec<Bson> using the same
    // per-element literal grammar as a scalar RHS (quoted string, bool, null,
    // int, float, bare-string fallback).
    #[test]
    fn parse_list_parses_bracketed_literals() {
        assert_eq!(
            parse_list("['gery', 'sophie']"),
            Some(vec![
                Bson::String("gery".into()),
                Bson::String("sophie".into())
            ])
        );
        assert_eq!(
            parse_list("[30, 20]"),
            Some(vec![Bson::Int64(30), Bson::Int64(20)])
        );
        assert_eq!(parse_list("not a list"), None);
    }

    // Acceptance: an empty bracket literal parses as an empty list, not a
    // one-element list containing an empty string — `field in []` must never
    // match, `field not in []` must always match.
    #[test]
    fn parse_list_empty_brackets_is_empty_list() {
        assert_eq!(parse_list("[]"), Some(Vec::new()));
        assert_eq!(parse_list("[ ]"), Some(Vec::new()));
    }

    // Acceptance: `in` matches when the field's value equals any element of
    // the literal list; `not in` is the negation. Numeric coercion (Int64 vs
    // Double) behaves the same as `==` already does via bson_eq.
    #[test]
    fn in_and_not_in_eval() {
        let c = Condition::parse("client_name in ['gery', 'sophie']").unwrap();
        assert!(c.eval(&doc(&[("client_name", Bson::String("gery".into()))])));
        assert!(c.eval(&doc(&[("client_name", Bson::String("sophie".into()))])));
        assert!(!c.eval(&doc(&[("client_name", Bson::String("marc".into()))])));
        // absent field never matches `in` unless the list contains null.
        assert!(!c.eval(&doc(&[])));

        let c = Condition::parse("client_age in [30, 20]").unwrap();
        assert!(c.eval(&doc(&[("client_age", Bson::Int64(30))])));
        assert!(c.eval(&doc(&[("client_age", Bson::Double(20.0))])));
        assert!(!c.eval(&doc(&[("client_age", Bson::Int64(25))])));

        let c = Condition::parse("client_age not in [13, 14]").unwrap();
        assert!(c.eval(&doc(&[("client_age", Bson::Int64(30))])));
        assert!(!c.eval(&doc(&[("client_age", Bson::Int64(13))])));
        // absent field: `not in` is true (mirrors $nin treating missing as
        // not-in unless the list itself contains null).
        assert!(c.eval(&doc(&[])));
    }

    // Acceptance: an `in` op must not false-match inside a field name that
    // contains "in" as raw text (e.g. "domain").
    #[test]
    fn in_does_not_false_match_inside_identifier() {
        let c = Condition::parse("domain in ['example.com']").unwrap();
        assert!(c.eval(&doc(&[("domain", Bson::String("example.com".into()))])));
        assert!(!c.eval(&doc(&[("domain", Bson::String("other.com".into()))])));
    }

    // Acceptance: to_filter translates `in`/`not in` into native Mongo
    // $in/$nin, so subset_conds can push a list-membership filter to the
    // server instead of fetching every document.
    #[test]
    fn to_filter_translates_in_and_not_in() {
        let c = Condition::parse("client_name in ['gery', 'sophie']").unwrap();
        let mut want = Document::new();
        want.insert(
            "client_name",
            doc_in(vec![
                Bson::String("gery".into()),
                Bson::String("sophie".into()),
            ]),
        );
        assert_eq!(c.to_filter(), want);

        let c = Condition::parse("client_age not in [13, 14]").unwrap();
        let mut inner = Document::new();
        inner.insert("$nin", vec![Bson::Int64(13), Bson::Int64(14)]);
        let mut want = Document::new();
        want.insert("client_age", inner);
        assert_eq!(c.to_filter(), want);
    }

    // Acceptance: a quoted RFC3339 string inside an `in` list is upgraded to
    // a real BSON datetime, same as a scalar comparison, so a list of date
    // literals matches server-side against a real Bson::DateTime field.
    #[test]
    fn to_filter_in_list_coerces_rfc3339_literals() {
        let c = Condition::parse("created_at in ['2026-07-17T00:00:00Z', '2026-07-18T00:00:00Z']")
            .unwrap();
        let filter = c.to_filter();
        let inner = filter.get_document("created_at").unwrap();
        let list = inner.get_array("$in").unwrap();
        assert_eq!(
            list[0],
            Bson::DateTime(bson::DateTime::parse_rfc3339_str("2026-07-17T00:00:00Z").unwrap())
        );
    }

    fn doc_in(values: Vec<Bson>) -> Document {
        let mut d = Document::new();
        d.insert("$in", values);
        d
    }
}
