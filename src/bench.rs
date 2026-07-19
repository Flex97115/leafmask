//! Dump/restore benchmark: synthetic "client" documents, real Dump/Restore
//! drivers, local directory storage (no network cost). Pure helpers here;
//! execution against a live MongoDB is in `exec` (feature `mongo`).

use bson::{doc, Document};

use crate::error::{Error, Result};

/// One benchmark measurement (or a linear extrapolation of one).
#[derive(Debug, Clone, PartialEq)]
pub struct BenchRun {
    pub docs: u64,
    pub dump_secs: f64,
    pub restore_secs: f64,
    pub dump_bytes: u64,
    pub estimated: bool,
}

const FIRST_NAMES: [&str; 8] = [
    "alice", "bruno", "chloe", "david", "emma", "farid", "gina", "hugo",
];
const LAST_NAMES: [&str; 8] = [
    "martin", "bernard", "dubois", "thomas", "robert", "richard", "petit", "durand",
];
const CITIES: [&str; 6] = ["Paris", "Lyon", "Marseille", "Lille", "Nantes", "Bordeaux"];
const STATUSES: [&str; 4] = ["active", "inactive", "pending", "churned"];

/// A deterministic synthetic customer document (~10 top-level fields), shaped
/// like a realistic CRM record so the bench reflects real-world payloads.
pub fn client_doc(i: u64) -> Document {
    let first = FIRST_NAMES[(i % 8) as usize];
    let last = LAST_NAMES[((i / 8) % 8) as usize];
    doc! {
        "_id": i as i64,
        "first_name": first,
        "last_name": last,
        "email": format!("{first}.{last}.{i}@example.com"),
        "phone": format!("+33 6 {:02} {:02} {:02} {:02}", i % 100, (i / 100) % 100, (i / 10_000) % 100, (i / 1_000_000) % 100),
        "address": {
            "street": format!("{} rue de la Paix", (i % 200) + 1),
            "city": CITIES[(i % 6) as usize],
            "zip": format!("{:05}", 10_000 + (i % 89_999)),
        },
        "status": STATUSES[(i % 4) as usize],
        "created_at": bson::DateTime::from_millis(1_600_000_000_000 + (i as i64) * 1_000),
        "orders_count": (i % 50) as i32,
        "balance": ((i % 100_000) as f64) / 100.0,
    }
}

/// Parse a comma-separated list of document counts (e.g. "100000,1000000").
pub fn parse_sizes(s: &str) -> Result<Vec<u64>> {
    let sizes: Vec<u64> = s
        .split(',')
        .map(|part| {
            part.trim().parse::<u64>().map_err(|_| {
                Error::Config(format!(
                    "invalid size '{part}': expected an integer document count"
                ))
            })
        })
        .collect::<Result<_>>()?;
    if sizes.is_empty() || sizes.contains(&0) {
        return Err(Error::Config(
            "sizes must be positive document counts".into(),
        ));
    }
    Ok(sizes)
}

/// Linear extrapolation from a measured run (dump/restore scale ~linearly with
/// document count for a fixed document shape).
pub fn extrapolate(base: &BenchRun, target_docs: u64) -> BenchRun {
    let f = target_docs as f64 / base.docs as f64;
    BenchRun {
        docs: target_docs,
        dump_secs: base.dump_secs * f,
        restore_secs: base.restore_secs * f,
        dump_bytes: (base.dump_bytes as f64 * f).round() as u64,
        estimated: true,
    }
}

fn group_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn fmt_secs(s: f64) -> String {
    if s >= 60.0 {
        format!("{}m {:02.0}s", (s / 60.0) as u64, s % 60.0)
    } else {
        format!("{s:.1}s")
    }
}

fn fmt_bytes(b: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let b = b as f64;
    if b >= 1024.0 * MB {
        format!("{:.2} GiB", b / (1024.0 * MB))
    } else {
        format!("{:.1} MiB", b / MB)
    }
}

/// Render measured + estimated runs as a table (markdown or aligned text).
pub fn render_table(runs: &[BenchRun], markdown: bool) -> String {
    let mut rows: Vec<[String; 5]> = Vec::with_capacity(runs.len());
    for r in runs {
        let docs = if r.estimated {
            if markdown {
                format!("{} *(estimated)*", group_thousands(r.docs))
            } else {
                format!("{} (estimated)", group_thousands(r.docs))
            }
        } else {
            group_thousands(r.docs)
        };
        let docs_per_sec = if r.dump_secs <= 0.0 {
            "n/a".to_string()
        } else {
            group_thousands((r.docs as f64 / r.dump_secs) as u64)
        };
        rows.push([
            docs,
            fmt_secs(r.dump_secs),
            fmt_secs(r.restore_secs),
            fmt_bytes(r.dump_bytes),
            docs_per_sec,
        ]);
    }
    let header = ["Documents", "Dump", "Restore", "Dump size", "Docs/s (dump)"];
    if markdown {
        let mut out = format!("| {} |\n", header.join(" | "));
        out.push_str(&format!("|{}\n", "---|".repeat(header.len())));
        for r in rows {
            out.push_str(&format!("| {} |\n", r.join(" | ")));
        }
        out
    } else {
        let mut widths = header.map(str::len);
        for r in &rows {
            for (w, cell) in widths.iter_mut().zip(r.iter()) {
                *w = (*w).max(cell.len());
            }
        }
        let line = |cells: &[String]| -> String {
            cells
                .iter()
                .zip(widths.iter())
                .map(|(c, w)| format!("{c:<w$}"))
                .collect::<Vec<_>>()
                .join("  ")
                + "\n"
        };
        let mut out = line(&header.map(String::from));
        for r in rows {
            out.push_str(&line(&r));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Same index -> same document, so runs are reproducible.
    #[test]
    fn client_doc_is_deterministic_and_client_shaped() {
        let a = client_doc(42);
        assert_eq!(a, client_doc(42));
        assert_ne!(a, client_doc(43));
        assert_eq!(a.len(), 10); // ~10 propriétés, comme une vraie fiche client
        assert!(a.get_document("address").unwrap().get_str("city").is_ok());
        assert!(a.get_str("email").unwrap().contains('@'));
    }

    #[test]
    fn parse_sizes_accepts_comma_list_and_rejects_junk() {
        assert_eq!(
            parse_sizes("100000,1000000").unwrap(),
            vec![100_000, 1_000_000]
        );
        assert_eq!(parse_sizes(" 500 ").unwrap(), vec![500]);
        assert!(parse_sizes("abc").is_err());
        assert!(parse_sizes("").is_err());
        assert!(parse_sizes("0").is_err()); // zero-doc run is meaningless
    }

    #[test]
    fn extrapolate_scales_linearly_and_flags_estimate() {
        let base = BenchRun {
            docs: 1_000_000,
            dump_secs: 10.0,
            restore_secs: 20.0,
            dump_bytes: 50_000_000,
            estimated: false,
        };
        let est = extrapolate(&base, 10_000_000);
        assert_eq!(est.docs, 10_000_000);
        assert!((est.dump_secs - 100.0).abs() < 1e-9);
        assert!((est.restore_secs - 200.0).abs() < 1e-9);
        assert_eq!(est.dump_bytes, 500_000_000);
        assert!(est.estimated);
    }

    #[test]
    fn render_table_markdown_marks_estimates() {
        let runs = vec![
            BenchRun {
                docs: 100_000,
                dump_secs: 1.5,
                restore_secs: 2.5,
                dump_bytes: 5_000_000,
                estimated: false,
            },
            BenchRun {
                docs: 10_000_000,
                dump_secs: 150.0,
                restore_secs: 250.0,
                dump_bytes: 500_000_000,
                estimated: true,
            },
        ];
        let md = render_table(&runs, true);
        assert!(md.contains("| Documents |"));
        assert!(md.contains("100,000"));
        assert!(md.contains("10,000,000 *(estimated)*"));
        let txt = render_table(&runs, false);
        assert!(txt.contains("estimated"));
    }

    #[test]
    fn render_table_shows_n_a_for_zero_duration() {
        let runs = vec![BenchRun {
            docs: 1_000,
            dump_secs: 0.0,
            restore_secs: 5.0,
            dump_bytes: 10_000,
            estimated: false,
        }];
        let md = render_table(&runs, true);
        assert!(md.contains("n/a"));
        let txt = render_table(&runs, false);
        assert!(txt.contains("n/a"));
    }
}
