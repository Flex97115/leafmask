//! Built-in transformer library (feature `transform.builtin-transformers`).
//!
//! Every transformer is registered with a name, description, and typed
//! parameter list (discoverable through the catalog). Hash- and
//! deterministic-random-based transformers derive their output purely from the
//! input value and the engine's salt, so the same input yields the same output
//! across runs. Transformers operate natively on BSON types (ObjectId,
//! Decimal128, ISODate, binary).
//!
//! Gap: the source ships a Go-template transformer; here `template` supports
//! `{{ field }}` substitution from the document, not the full Go template
//! language — see regeneration-gaps.md.

use bson::{oid::ObjectId, Binary, Bson, DateTime, Document};
use regex::Regex;

use crate::error::{Error, Result};
use crate::hash::HashEngine;
use crate::toolkit::{cast, ParamDefinition, ParamType};

use super::{FnTransformer, Registry, Transformer, TransformerFactory};

const FIRST_NAMES: &[&str] = &[
    "Alex", "Blake", "Casey", "Devin", "Erin", "Frankie", "Gale", "Harper", "Ira", "Jamie",
];
const LAST_NAMES: &[&str] = &[
    "Ash", "Brooks", "Cole", "Diaz", "Eng", "Ford", "Green", "Hale", "Ibarra", "Jones",
];
const COMPANIES: &[&str] = &["Acme", "Globex", "Initech", "Umbrella", "Hooli", "Soylent"];

/// A hashed float in `[0, 1)`.
fn unit(engine: &HashEngine, input: &[u8]) -> f64 {
    (engine.hash_u64(input) >> 11) as f64 / (1u64 << 53) as f64
}

fn boxed<F>(f: F) -> Box<dyn Transformer>
where
    F: Fn(&Bson, &Document) -> Result<Bson> + Send + Sync + 'static,
{
    Box::new(FnTransformer(f))
}

/// Register every built-in transformer into `r`.
pub fn register_all(r: &mut Registry) {
    // --- hashing -----------------------------------------------------------
    r.register(TransformerFactory::new(
        "hash",
        "Replace the value with a deterministic hex hash of the original.",
        vec![ParamDefinition::optional(
            "length",
            ParamType::Int,
            Bson::Int64(16),
            "number of hex characters to keep",
        )],
        |p, engine| {
            let len = p.get_i64("length").unwrap_or(16).max(1) as usize;
            let engine = engine.clone();
            Ok(boxed(move |v, _| {
                let bytes = engine.hash_bytes(&cast::to_hashable_bytes(v), len / 2 + 1);
                Ok(Bson::String(hex::encode(bytes)[..len].to_string()))
            }))
        },
    ));

    // --- deterministic random numbers -------------------------------------
    r.register(TransformerFactory::new(
        "random_int",
        "Deterministically generate an integer within [min, max].",
        vec![
            ParamDefinition::required("min", ParamType::Int, "lower bound"),
            ParamDefinition::required("max", ParamType::Int, "upper bound"),
        ],
        |p, engine| {
            let min = p.get_i64("min").unwrap();
            let max = p.get_i64("max").unwrap();
            let engine = engine.clone();
            Ok(boxed(move |v, _| {
                Ok(Bson::Int64(engine.ranged_i64(
                    &cast::to_hashable_bytes(v),
                    min,
                    max,
                )))
            }))
        },
    ));

    r.register(TransformerFactory::new(
        "random_float",
        "Deterministically generate a float within [min, max].",
        vec![
            ParamDefinition::required("min", ParamType::Float, "lower bound"),
            ParamDefinition::required("max", ParamType::Float, "upper bound"),
        ],
        |p, engine| {
            let min = p.get_f64("min").unwrap();
            let max = p.get_f64("max").unwrap();
            let engine = engine.clone();
            Ok(boxed(move |v, _| {
                let u = unit(&engine, &cast::to_hashable_bytes(v));
                Ok(Bson::Double(min + u * (max - min)))
            }))
        },
    ));

    // --- noise (perturb, keep type) ---------------------------------------
    r.register(TransformerFactory::new(
        "noise_int",
        "Perturb an integer by up to ±ratio of its magnitude, keeping the int type.",
        vec![ParamDefinition::optional(
            "ratio",
            ParamType::Float,
            Bson::Double(0.1),
            "maximum fractional perturbation",
        )],
        |p, engine| {
            let ratio = p.get_f64("ratio").unwrap_or(0.1);
            let engine = engine.clone();
            Ok(boxed(move |v, _| {
                let base = cast::to_i64(v)
                    .ok_or_else(|| Error::Transform("noise_int needs an integer".into()))?;
                let bytes = cast::to_hashable_bytes(v);
                let signed = unit(&engine, &bytes) * 2.0 - 1.0; // [-1, 1)
                let delta = (base as f64 * ratio * signed).round() as i64;
                Ok(Bson::Int64(base + delta))
            }))
        },
    ));

    r.register(TransformerFactory::new(
        "noise_float",
        "Perturb a float by up to ±ratio of its magnitude, keeping the double type.",
        vec![ParamDefinition::optional(
            "ratio",
            ParamType::Float,
            Bson::Double(0.1),
            "maximum fractional perturbation",
        )],
        |p, engine| {
            let ratio = p.get_f64("ratio").unwrap_or(0.1);
            let engine = engine.clone();
            Ok(boxed(move |v, _| {
                let base = cast::to_f64(v)
                    .ok_or_else(|| Error::Transform("noise_float needs a number".into()))?;
                let signed = unit(&engine, &cast::to_hashable_bytes(v)) * 2.0 - 1.0;
                Ok(Bson::Double(base + base * ratio * signed))
            }))
        },
    ));

    r.register(TransformerFactory::new(
        "noise_date",
        "Shift a date by up to ±max_days, keeping the datetime type.",
        vec![ParamDefinition::optional(
            "max_days",
            ParamType::Int,
            Bson::Int64(30),
            "maximum absolute shift in days",
        )],
        |p, engine| {
            let max_days = p.get_i64("max_days").unwrap_or(30);
            let engine = engine.clone();
            Ok(boxed(move |v, _| match v {
                Bson::DateTime(dt) => {
                    let shift = engine.ranged_i64(&cast::to_hashable_bytes(v), -max_days, max_days);
                    let ms = dt.timestamp_millis() + shift * 86_400_000;
                    Ok(Bson::DateTime(DateTime::from_millis(ms)))
                }
                _ => Err(Error::Transform("noise_date needs a datetime".into())),
            }))
        },
    ));

    // --- masking / regexp / replace / null --------------------------------
    r.register(TransformerFactory::new(
        "masking",
        "Replace every character of a string with a mask character, keeping length.",
        vec![ParamDefinition::optional(
            "char",
            ParamType::String,
            Bson::String("*".into()),
            "mask character",
        )],
        |p, _| {
            let ch = p
                .get_str("char")
                .unwrap_or("*")
                .chars()
                .next()
                .unwrap_or('*');
            Ok(boxed(move |v, _| {
                let s = v.as_str().unwrap_or("");
                Ok(Bson::String(ch.to_string().repeat(s.chars().count())))
            }))
        },
    ));

    r.register(TransformerFactory::new(
        "regexp_replace",
        "Apply a regular-expression replacement to a string value.",
        vec![
            ParamDefinition::required("regexp", ParamType::String, "pattern"),
            ParamDefinition::required("replace", ParamType::String, "replacement"),
        ],
        |p, _| {
            let re = Regex::new(p.get_str("regexp").unwrap())
                .map_err(|e| Error::Transform(format!("invalid regexp: {e}")))?;
            let replace = p.get_str("replace").unwrap().to_string();
            Ok(boxed(move |v, _| {
                let s = v.as_str().unwrap_or("");
                Ok(Bson::String(
                    re.replace_all(s, replace.as_str()).into_owned(),
                ))
            }))
        },
    ));

    r.register(TransformerFactory::new(
        "replace",
        "Replace the value with a fixed configured value.",
        vec![ParamDefinition::required(
            "value",
            ParamType::Any,
            "replacement value",
        )],
        |p, _| {
            let value = p.get("value").cloned().unwrap_or(Bson::Null);
            Ok(boxed(move |_, _| Ok(value.clone())))
        },
    ));

    r.register(TransformerFactory::new(
        "set_null",
        "Set the value to null.",
        vec![],
        |_, _| Ok(boxed(|_, _| Ok(Bson::Null))),
    ));

    // --- generators --------------------------------------------------------
    r.register(TransformerFactory::new(
        "random_email",
        "Deterministically generate an email address.",
        vec![ParamDefinition::optional(
            "domain",
            ParamType::String,
            Bson::String("example.com".into()),
            "email domain",
        )],
        |p, engine| {
            let domain = p.get_str("domain").unwrap_or("example.com").to_string();
            let engine = engine.clone();
            Ok(boxed(move |v, _| {
                let h = engine.hash_u64(&cast::to_hashable_bytes(v));
                Ok(Bson::String(format!("user{h:x}@{domain}")))
            }))
        },
    ));

    r.register(TransformerFactory::new(
        "random_person",
        "Deterministically generate consistent person fields (first_name, last_name, email) into an embedded document.",
        vec![],
        |_, engine| {
            let engine = engine.clone();
            Ok(boxed(move |v, _| {
                let bytes = cast::to_hashable_bytes(v);
                let first = FIRST_NAMES[engine.index(&bytes, FIRST_NAMES.len())];
                let last = LAST_NAMES[engine.index(&[&bytes[..], b"last"].concat(), LAST_NAMES.len())];
                let mut out = Document::new();
                out.insert("first_name", first);
                out.insert("last_name", last);
                out.insert(
                    "email",
                    format!("{}.{}@example.com", first.to_lowercase(), last.to_lowercase()),
                );
                Ok(Bson::Document(out))
            }))
        },
    ));

    r.register(TransformerFactory::new(
        "random_company",
        "Deterministically generate a company name.",
        vec![],
        |_, engine| {
            let engine = engine.clone();
            Ok(boxed(move |v, _| {
                let name = COMPANIES[engine.index(&cast::to_hashable_bytes(v), COMPANIES.len())];
                Ok(Bson::String(name.to_string()))
            }))
        },
    ));

    r.register(TransformerFactory::new(
        "random_object_id",
        "Deterministically transform an ObjectId while keeping it a valid ObjectId.",
        vec![],
        |_, engine| {
            let engine = engine.clone();
            Ok(boxed(move |v, _| {
                let bytes = engine.hash_bytes(&cast::to_hashable_bytes(v), 12);
                let mut arr = [0u8; 12];
                arr.copy_from_slice(&bytes);
                Ok(Bson::ObjectId(ObjectId::from_bytes(arr)))
            }))
        },
    ));

    r.register(TransformerFactory::new(
        "random_bytes",
        "Deterministically generate binary data of a given length.",
        vec![ParamDefinition::optional(
            "length",
            ParamType::Int,
            Bson::Int64(16),
            "number of bytes",
        )],
        |p, engine| {
            let len = p.get_i64("length").unwrap_or(16).max(0) as usize;
            let engine = engine.clone();
            Ok(boxed(move |v, _| {
                Ok(Bson::Binary(Binary {
                    subtype: bson::spec::BinarySubtype::Generic,
                    bytes: engine.hash_bytes(&cast::to_hashable_bytes(v), len),
                }))
            }))
        },
    ));

    // --- template ----------------------------------------------------------
    r.register(TransformerFactory::new(
        "template",
        "Render a template with {{ field }} placeholders substituted from the document.",
        vec![ParamDefinition::required(
            "template",
            ParamType::String,
            "template text",
        )],
        |p, _| {
            let template = p.get_str("template").unwrap().to_string();
            let re = Regex::new(r"\{\{\s*\.?(\w+)\s*\}\}").unwrap();
            Ok(boxed(move |_, doc| {
                let out = re.replace_all(&template, |caps: &regex::Captures| {
                    let field = &caps[1];
                    match doc.get(field) {
                        Some(Bson::String(s)) => s.clone(),
                        Some(b) => b.to_string(),
                        None => String::new(),
                    }
                });
                Ok(Bson::String(out.into_owned()))
            }))
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Params;

    fn params(src: &str) -> Params {
        serde_yaml::from_str(src).unwrap()
    }
    fn engine() -> HashEngine {
        HashEngine::new("test-salt")
    }

    // Acceptance: each built-in is registered with name, description, typed
    // parameters, discoverable via the catalog surface.
    #[test]
    fn builtins_are_registered_and_documented() {
        let r = Registry::with_builtins();
        let names = r.names();
        for expected in [
            "hash",
            "random_int",
            "noise_date",
            "masking",
            "random_person",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
        let doc = r.get("random_int").unwrap().doc();
        assert_eq!(doc.name, "random_int");
        assert!(!doc.description.is_empty());
        assert_eq!(doc.parameters.len(), 2);
        assert!(doc.parameters.iter().any(|p| p.name == "min" && p.required));
    }

    // Acceptance: hash/deterministic-random produce the same output for the same
    // input and salt across separate instantiations.
    #[test]
    fn deterministic_across_runs() {
        let doc = Document::new();
        for (name, cfg) in [
            ("hash", ""),
            ("random_int", "min: 1\nmax: 1000000\n"),
            ("random_email", ""),
        ] {
            let a = Registry::with_builtins()
                .instantiate(name, &params(cfg), &engine())
                .unwrap();
            let b = Registry::with_builtins()
                .instantiate(name, &params(cfg), &engine())
                .unwrap();
            let v = Bson::String("alice@corp.com".into());
            assert_eq!(
                a.transform(&v, &doc).unwrap(),
                b.transform(&v, &doc).unwrap(),
                "{name} not deterministic"
            );
        }
    }

    // Acceptance: noise perturbs within range while keeping the BSON type.
    #[test]
    fn noise_keeps_type_and_range() {
        let r = Registry::with_builtins();
        let doc = Document::new();

        let t = r
            .instantiate("noise_int", &params("ratio: 0.1\n"), &engine())
            .unwrap();
        let out = t.transform(&Bson::Int64(1000), &doc).unwrap();
        match out {
            Bson::Int64(v) => assert!((900..=1100).contains(&v), "out of noise range: {v}"),
            other => panic!("expected int, got {other:?}"),
        }

        let t = r
            .instantiate("noise_date", &params("max_days: 5\n"), &engine())
            .unwrap();
        let base = DateTime::from_millis(1_600_000_000_000);
        let out = t.transform(&Bson::DateTime(base), &doc).unwrap();
        match out {
            Bson::DateTime(dt) => {
                let diff = (dt.timestamp_millis() - base.timestamp_millis()).abs();
                assert!(diff <= 5 * 86_400_000);
            }
            other => panic!("expected datetime, got {other:?}"),
        }
    }

    // Acceptance: structured generators populate multiple related fields
    // consistently.
    #[test]
    fn structured_person_is_consistent() {
        let r = Registry::with_builtins();
        let t = r
            .instantiate("random_person", &params(""), &engine())
            .unwrap();
        let out = t
            .transform(&Bson::String("seed".into()), &Document::new())
            .unwrap();
        let d = out.as_document().unwrap();
        let first = d.get_str("first_name").unwrap().to_lowercase();
        let last = d.get_str("last_name").unwrap().to_lowercase();
        // email is derived from the same first/last -> internally consistent.
        assert_eq!(
            d.get_str("email").unwrap(),
            format!("{first}.{last}@example.com")
        );
    }

    // Acceptance: an ObjectId field is transformed and stays a valid ObjectId.
    #[test]
    fn object_id_stays_valid() {
        let r = Registry::with_builtins();
        let t = r
            .instantiate("random_object_id", &params(""), &engine())
            .unwrap();
        let src = ObjectId::parse_str("5f9d7a3b2c1e4a6b8d0f1234").unwrap();
        let out = t.transform(&Bson::ObjectId(src), &Document::new()).unwrap();
        match out {
            Bson::ObjectId(o) => assert_ne!(o, src),
            other => panic!("expected ObjectId, got {other:?}"),
        }
    }

    #[test]
    fn masking_and_template_and_regexp() {
        let r = Registry::with_builtins();
        let doc = {
            let mut d = Document::new();
            d.insert("city", "Paris");
            d
        };
        let mask = r.instantiate("masking", &params(""), &engine()).unwrap();
        assert_eq!(
            mask.transform(&Bson::String("secret".into()), &doc)
                .unwrap(),
            Bson::String("******".into())
        );
        let tmpl = r
            .instantiate(
                "template",
                &params("template: 'lives in {{ city }}'\n"),
                &engine(),
            )
            .unwrap();
        assert_eq!(
            tmpl.transform(&Bson::Null, &doc).unwrap(),
            Bson::String("lives in Paris".into())
        );
        let rx = r
            .instantiate(
                "regexp_replace",
                &params("regexp: '[0-9]'\nreplace: X\n"),
                &engine(),
            )
            .unwrap();
        assert_eq!(
            rx.transform(&Bson::String("a1b2".into()), &doc).unwrap(),
            Bson::String("aXbX".into())
        );
    }
}
