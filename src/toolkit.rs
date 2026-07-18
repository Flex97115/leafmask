//! Transformer parameter definition and validation SDK
//! (feature `transform.parameter-framework`).
//!
//! This is the "toolkit" every transformer — built-in or custom — uses to
//! declare, cast, and validate its configuration parameters against the target
//! field's BSON type. It has two halves:
//!   * declaration + static validation (`ParamDefinition`, `validate_static`);
//!   * casting between BSON native types and the generic values transformers
//!     work with (`cast`).

use std::collections::BTreeMap;
use std::str::FromStr;

use bson::{Binary, Bson, DateTime, Decimal128, oid::ObjectId, spec::BinarySubtype};

use crate::config::Params;
use crate::error::{Error, Result};

/// The logical type a parameter (or a field, after an override) is expected to
/// have. These map onto the BSON native types transformers operate on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    String,
    Int,
    Float,
    Bool,
    Bytes,
    ObjectId,
    Decimal128,
    DateTime,
    Binary,
    /// Accept the value as-is (any BSON).
    Any,
}

impl ParamType {
    /// Human-readable name for catalog output and error messages.
    pub fn name(&self) -> &'static str {
        match self {
            ParamType::String => "string",
            ParamType::Int => "int",
            ParamType::Float => "float",
            ParamType::Bool => "bool",
            ParamType::Bytes => "bytes",
            ParamType::ObjectId => "objectId",
            ParamType::Decimal128 => "decimal128",
            ParamType::DateTime => "datetime",
            ParamType::Binary => "binary",
            ParamType::Any => "any",
        }
    }
}

/// A single declared parameter of a transformer.
#[derive(Debug, Clone)]
pub struct ParamDefinition {
    pub name: String,
    pub kind: ParamType,
    pub required: bool,
    pub default: Option<Bson>,
    pub description: String,
}

impl ParamDefinition {
    /// Builder for a required parameter.
    pub fn required(name: &str, kind: ParamType, description: &str) -> Self {
        ParamDefinition {
            name: name.to_string(),
            kind,
            required: true,
            default: None,
            description: description.to_string(),
        }
    }

    /// Builder for an optional parameter with a default.
    pub fn optional(name: &str, kind: ParamType, default: Bson, description: &str) -> Self {
        ParamDefinition {
            name: name.to_string(),
            kind,
            required: false,
            default: Some(default),
            description: description.to_string(),
        }
    }
}

/// The validated, decoded static parameter values handed to a transformer at
/// document-transform time.
#[derive(Debug, Clone, Default)]
pub struct ParamValues {
    map: BTreeMap<String, Bson>,
}

impl ParamValues {
    pub fn get(&self, name: &str) -> Option<&Bson> {
        self.map.get(name)
    }
    pub fn get_str(&self, name: &str) -> Option<&str> {
        self.map.get(name).and_then(Bson::as_str)
    }
    pub fn get_i64(&self, name: &str) -> Option<i64> {
        self.map.get(name).and_then(cast::to_i64)
    }
    pub fn get_f64(&self, name: &str) -> Option<f64> {
        self.map.get(name).and_then(cast::to_f64)
    }
    pub fn get_bool(&self, name: &str) -> Option<bool> {
        self.map.get(name).and_then(Bson::as_bool)
    }
    /// Insert a value directly (used by dynamic-parameter resolution).
    pub fn insert(&mut self, name: impl Into<String>, value: Bson) {
        self.map.insert(name.into(), value);
    }
}

/// Validate configured static parameters against a transformer's declared
/// parameter list. A value that cannot be cast to its declared type, or a
/// missing required parameter with no default, is a config-time error.
pub fn validate_static(defs: &[ParamDefinition], provided: &Params) -> Result<ParamValues> {
    let mut out = ParamValues::default();
    for def in defs {
        match provided.get(&def.name) {
            Some(raw) => {
                let bson = cast::yaml_to_typed(raw, def.kind).map_err(|e| {
                    Error::Parameter(format!(
                        "parameter '{}' expects {}: {e}",
                        def.name,
                        def.kind.name()
                    ))
                })?;
                out.map.insert(def.name.clone(), bson);
            }
            None => {
                if let Some(default) = &def.default {
                    out.map.insert(def.name.clone(), default.clone());
                } else if def.required {
                    return Err(Error::Parameter(format!(
                        "required parameter '{}' is missing",
                        def.name
                    )));
                }
            }
        }
    }
    Ok(out)
}

/// Casting helpers between BSON native types and the generic values transformers
/// operate on.
pub mod cast {
    use super::*;

    /// Cast any numeric BSON value to `i64`.
    pub fn to_i64(b: &Bson) -> Option<i64> {
        match b {
            Bson::Int32(v) => Some(*v as i64),
            Bson::Int64(v) => Some(*v),
            Bson::Double(v) => Some(*v as i64),
            Bson::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Cast any numeric BSON value to `f64`.
    pub fn to_f64(b: &Bson) -> Option<f64> {
        match b {
            Bson::Int32(v) => Some(*v as f64),
            Bson::Int64(v) => Some(*v as f64),
            Bson::Double(v) => Some(*v),
            Bson::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Render a BSON value to the string a transformer would hash/mask over.
    pub fn to_hashable_bytes(b: &Bson) -> Vec<u8> {
        match b {
            Bson::String(s) => s.as_bytes().to_vec(),
            Bson::Int32(v) => v.to_be_bytes().to_vec(),
            Bson::Int64(v) => v.to_be_bytes().to_vec(),
            Bson::Double(v) => v.to_be_bytes().to_vec(),
            Bson::Boolean(v) => vec![*v as u8],
            Bson::ObjectId(o) => o.bytes().to_vec(),
            Bson::Binary(bin) => bin.bytes.clone(),
            Bson::DateTime(dt) => dt.timestamp_millis().to_be_bytes().to_vec(),
            Bson::Decimal128(d) => d.bytes().to_vec(),
            Bson::Null => Vec::new(),
            other => other.to_string().into_bytes(),
        }
    }

    /// Convert a raw YAML config value into a typed BSON value for `kind`.
    pub fn yaml_to_typed(v: &serde_yaml::Value, kind: ParamType) -> Result<Bson> {
        use serde_yaml::Value as Y;
        match kind {
            ParamType::Any => Ok(yaml_to_bson(v)),
            ParamType::String => v
                .as_str()
                .map(|s| Bson::String(s.to_string()))
                .or_else(|| scalar_to_string(v).map(Bson::String))
                .ok_or_else(|| Error::Parameter("not a string".into())),
            ParamType::Int => match v {
                Y::Number(n) => n
                    .as_i64()
                    .map(Bson::Int64)
                    .ok_or_else(|| Error::Parameter("not an integer".into())),
                Y::String(s) => s
                    .parse::<i64>()
                    .map(Bson::Int64)
                    .map_err(|_| Error::Parameter(format!("'{s}' is not an integer"))),
                _ => Err(Error::Parameter("not an integer".into())),
            },
            ParamType::Float => match v {
                Y::Number(n) => n
                    .as_f64()
                    .map(Bson::Double)
                    .ok_or_else(|| Error::Parameter("not a float".into())),
                Y::String(s) => s
                    .parse::<f64>()
                    .map(Bson::Double)
                    .map_err(|_| Error::Parameter(format!("'{s}' is not a float"))),
                _ => Err(Error::Parameter("not a float".into())),
            },
            ParamType::Bool => v
                .as_bool()
                .map(Bson::Boolean)
                .ok_or_else(|| Error::Parameter("not a bool".into())),
            ParamType::Bytes => v
                .as_str()
                .map(|s| Bson::Binary(Binary {
                    subtype: BinarySubtype::Generic,
                    bytes: s.as_bytes().to_vec(),
                }))
                .ok_or_else(|| Error::Parameter("bytes must be given as a string".into())),
            ParamType::ObjectId => {
                let s = v
                    .as_str()
                    .ok_or_else(|| Error::Parameter("objectId must be a hex string".into()))?;
                ObjectId::parse_str(s)
                    .map(Bson::ObjectId)
                    .map_err(|e| Error::Parameter(format!("invalid objectId: {e}")))
            }
            ParamType::Decimal128 => {
                let s = v
                    .as_str()
                    .or_else(|| v.as_i64().map(|_| ""))
                    .ok_or_else(|| Error::Parameter("decimal128 must be a string".into()))?;
                let text = if s.is_empty() {
                    scalar_to_string(v).unwrap_or_default()
                } else {
                    s.to_string()
                };
                Decimal128::from_str(&text)
                    .map(Bson::Decimal128)
                    .map_err(|e| Error::Parameter(format!("invalid decimal128: {e}")))
            }
            ParamType::DateTime => {
                let s = v
                    .as_str()
                    .ok_or_else(|| Error::Parameter("datetime must be an RFC3339 string".into()))?;
                DateTime::parse_rfc3339_str(s)
                    .map(Bson::DateTime)
                    .map_err(|e| Error::Parameter(format!("invalid datetime: {e}")))
            }
            ParamType::Binary => v
                .as_str()
                .map(|s| Bson::Binary(Binary {
                    subtype: BinarySubtype::Generic,
                    bytes: s.as_bytes().to_vec(),
                }))
                .ok_or_else(|| Error::Parameter("binary must be given as a string".into())),
        }
    }

    /// Cast a BSON value already read from a document into the type a
    /// transformer expects (used for dynamic parameters resolved at transform
    /// time). Mirrors [`yaml_to_typed`] but works from BSON.
    pub fn bson_to_typed(v: &Bson, kind: ParamType) -> Result<Bson> {
        match kind {
            ParamType::Any => Ok(v.clone()),
            ParamType::String => match v {
                Bson::String(_) => Ok(v.clone()),
                Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Boolean(_) => {
                    Ok(Bson::String(v.to_string()))
                }
                _ => Err(Error::Parameter("value is not a string".into())),
            },
            ParamType::Int => to_i64(v)
                .map(Bson::Int64)
                .ok_or_else(|| Error::Parameter("value is not an integer".into())),
            ParamType::Float => to_f64(v)
                .map(Bson::Double)
                .ok_or_else(|| Error::Parameter("value is not a float".into())),
            ParamType::Bool => v
                .as_bool()
                .map(Bson::Boolean)
                .ok_or_else(|| Error::Parameter("value is not a bool".into())),
            ParamType::ObjectId => match v {
                Bson::ObjectId(_) => Ok(v.clone()),
                Bson::String(s) => ObjectId::parse_str(s)
                    .map(Bson::ObjectId)
                    .map_err(|e| Error::Parameter(format!("invalid objectId: {e}"))),
                _ => Err(Error::Parameter("value is not an objectId".into())),
            },
            ParamType::DateTime => match v {
                Bson::DateTime(_) => Ok(v.clone()),
                Bson::String(s) => DateTime::parse_rfc3339_str(s)
                    .map(Bson::DateTime)
                    .map_err(|e| Error::Parameter(format!("invalid datetime: {e}"))),
                _ => Err(Error::Parameter("value is not a datetime".into())),
            },
            ParamType::Decimal128 => match v {
                Bson::Decimal128(_) => Ok(v.clone()),
                Bson::String(s) => Decimal128::from_str(s)
                    .map(Bson::Decimal128)
                    .map_err(|e| Error::Parameter(format!("invalid decimal128: {e}"))),
                _ => Err(Error::Parameter("value is not a decimal128".into())),
            },
            ParamType::Binary | ParamType::Bytes => match v {
                Bson::Binary(_) => Ok(v.clone()),
                Bson::String(s) => Ok(Bson::Binary(Binary {
                    subtype: BinarySubtype::Generic,
                    bytes: s.as_bytes().to_vec(),
                })),
                _ => Err(Error::Parameter("value is not binary".into())),
            },
        }
    }

    fn scalar_to_string(v: &serde_yaml::Value) -> Option<String> {
        use serde_yaml::Value as Y;
        match v {
            Y::String(s) => Some(s.clone()),
            Y::Bool(b) => Some(b.to_string()),
            Y::Number(n) => Some(n.to_string()),
            _ => None,
        }
    }

    /// Recursively convert a YAML value into a BSON value (for `Any` params and
    /// generic document handling).
    pub fn yaml_to_bson(v: &serde_yaml::Value) -> Bson {
        use serde_yaml::Value as Y;
        match v {
            Y::Null => Bson::Null,
            Y::Bool(b) => Bson::Boolean(*b),
            Y::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Bson::Int64(i)
                } else {
                    Bson::Double(n.as_f64().unwrap_or(f64::NAN))
                }
            }
            Y::String(s) => Bson::String(s.clone()),
            Y::Sequence(seq) => Bson::Array(seq.iter().map(yaml_to_bson).collect()),
            Y::Mapping(m) => {
                let mut doc = bson::Document::new();
                for (k, val) in m {
                    let key = k.as_str().map(str::to_string).unwrap_or_else(|| {
                        serde_yaml::to_string(k).unwrap_or_default().trim().to_string()
                    });
                    doc.insert(key, yaml_to_bson(val));
                }
                Bson::Document(doc)
            }
            Y::Tagged(t) => yaml_to_bson(&t.value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::cast::*;
    use super::*;
    use serde_yaml::from_str as yaml;

    fn params(src: &str) -> Params {
        yaml::<Params>(src).unwrap()
    }

    // Acceptance: a value that cannot be cast to the declared type is rejected
    // at config-validation time.
    #[test]
    fn rejects_uncastable_value() {
        let defs = [ParamDefinition::required("count", ParamType::Int, "how many")];
        let err = validate_static(&defs, &params("count: not-a-number\n")).unwrap_err();
        assert!(err.to_string().contains("count"), "{err}");

        // A missing required parameter is also an error.
        let err = validate_static(&defs, &params("other: 1\n")).unwrap_err();
        assert!(err.to_string().contains("missing"), "{err}");
    }

    // Acceptance: static parameter values are decoded and available.
    #[test]
    fn decodes_static_values() {
        let defs = [
            ParamDefinition::required("column", ParamType::String, "field"),
            ParamDefinition::required("min", ParamType::Int, "lower"),
            ParamDefinition::optional("keep_null", ParamType::Bool, Bson::Boolean(true), "n"),
        ];
        let values =
            validate_static(&defs, &params("column: email\nmin: 3\n")).unwrap();
        assert_eq!(values.get_str("column"), Some("email"));
        assert_eq!(values.get_i64("min"), Some(3));
        // default applied when not provided.
        assert_eq!(values.get_bool("keep_null"), Some(true));
    }

    // Acceptance: casting between BSON native types and generic values works.
    #[test]
    fn casts_bson_native_types() {
        // numeric widening.
        assert_eq!(to_i64(&Bson::Int32(7)), Some(7));
        assert_eq!(to_i64(&Bson::Double(9.9)), Some(9));
        assert_eq!(to_f64(&Bson::Int64(4)), Some(4.0));

        // objectId round-trips through config casting and stays valid.
        let oid = yaml_to_typed(
            &yaml::<serde_yaml::Value>("'5f9d7a3b2c1e4a6b8d0f1234'").unwrap(),
            ParamType::ObjectId,
        )
        .unwrap();
        assert!(matches!(oid, Bson::ObjectId(_)));

        // decimal128, datetime, binary all decode from strings.
        assert!(matches!(
            yaml_to_typed(&serde_yaml::Value::String("3.14".into()), ParamType::Decimal128).unwrap(),
            Bson::Decimal128(_)
        ));
        assert!(matches!(
            yaml_to_typed(
                &serde_yaml::Value::String("2020-01-02T03:04:05Z".into()),
                ParamType::DateTime
            )
            .unwrap(),
            Bson::DateTime(_)
        ));
        assert!(matches!(
            yaml_to_typed(&serde_yaml::Value::String("abc".into()), ParamType::Binary).unwrap(),
            Bson::Binary(_)
        ));

        // hashable bytes are stable and type-aware.
        assert_eq!(to_hashable_bytes(&Bson::String("x".into())), b"x");
        assert_eq!(to_hashable_bytes(&Bson::Int32(1)).len(), 4);
    }

    #[test]
    fn invalid_objectid_rejected() {
        let err = yaml_to_typed(&serde_yaml::Value::String("zzz".into()), ParamType::ObjectId)
            .unwrap_err();
        assert!(err.to_string().contains("objectId"), "{err}");
    }
}
