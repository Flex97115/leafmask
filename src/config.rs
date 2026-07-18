//! Configuration file loading (feature `config.loading`).
//!
//! Behaviour reproduced from the source product:
//!   * the config file is located via `--config` or, failing that, the
//!     `LEAFMASK_CONFIG` environment variable;
//!   * `${VAR}` occurrences inside the file are interpolated from the
//!     environment before parsing;
//!   * unknown keys are rejected rather than silently ignored
//!     (`#[serde(deny_unknown_fields)]`);
//!   * transformer parameters preserve their original key casing and are also
//!     retrievable case-insensitively — the Rust equivalent of working around
//!     viper's key lower-casing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

/// Case-preserving, case-insensitively-queryable transformer parameter bag.
///
/// serde_yaml preserves key casing on its own, so unlike the Go/viper original
/// there is no lower-casing to undo; this type additionally guarantees that a
/// parameter can be looked up regardless of the case used, which is what the
/// acceptance criterion asks for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Params {
    entries: Vec<(String, serde_yaml::Value)>,
}

impl Params {
    /// Look a parameter up by name, ignoring ASCII case.
    pub fn get(&self, key: &str) -> Option<&serde_yaml::Value> {
        self.entries
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    }

    /// True when no parameters were configured.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate parameters in their original declared order and casing.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &serde_yaml::Value)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Build from raw key/value pairs (used by tests and dynamic resolution).
    pub fn from_pairs(pairs: Vec<(String, serde_yaml::Value)>) -> Self {
        Params { entries: pairs }
    }
}

impl<'de> Deserialize<'de> for Params {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Deserialize as an ordered mapping so original casing/order survive.
        let mapping = serde_yaml::Mapping::deserialize(deserializer)?;
        let mut entries = Vec::with_capacity(mapping.len());
        for (k, v) in mapping {
            let key = match k {
                serde_yaml::Value::String(s) => s,
                other => serde_yaml::to_string(&other)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default(),
            };
            entries.push((key, v));
        }
        Ok(Params { entries })
    }
}

/// The `common` config section.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Common {
    /// Temporary working directory; required before a real dump runs.
    #[serde(default)]
    pub tmp_dir: Option<String>,
    /// Stable salt/seed for deterministic transformations across runs.
    #[serde(default)]
    pub salt: Option<String>,
}

/// The `storage` config section — a discriminated backend plus its raw params.
/// The concrete backends interpret `params` themselves.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct StorageConfig {
    /// Backend discriminator: `directory`, `s3`, `azure`, or `ssh`.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// Backend-specific settings, kept raw and case-preserving.
    #[serde(default, flatten)]
    pub params: Params,
}

/// The `mongodb` config section — how to reach the target deployment.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MongoConfig {
    /// Connection URI, e.g. `mongodb://localhost:27017`.
    #[serde(default)]
    pub uri: Option<String>,
}

/// Root configuration document.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub common: Common,
    #[serde(default)]
    pub mongodb: MongoConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    /// Warning identifiers the operator has explicitly resolved.
    #[serde(default)]
    pub resolved_warnings: Vec<String>,
    /// Raw dump section, refined by the dump/transform/subset features.
    #[serde(default)]
    pub dump: serde_yaml::Value,
    /// Raw restore section, refined by the restore features.
    #[serde(default)]
    pub restore: serde_yaml::Value,
    /// Raw custom-transformer declarations, refined by that feature.
    #[serde(default)]
    pub custom_transformers: serde_yaml::Value,
    /// Raw validate section, refined by the validate features.
    #[serde(default)]
    pub validate: serde_yaml::Value,
}

/// Locate the config file: the explicit `--config` path wins, otherwise the
/// `LEAFMASK_CONFIG` environment variable is consulted.
pub fn locate(explicit: Option<PathBuf>, env_var: Option<String>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    if let Some(v) = env_var {
        if !v.is_empty() {
            return Ok(PathBuf::from(v));
        }
    }
    Err(Error::Config(
        "no config file: pass --config or set LEAFMASK_CONFIG".into(),
    ))
}

/// Replace every `${VAR}` in `raw` with the value returned by `lookup`.
/// A referenced-but-undefined variable expands to the empty string, matching
/// the source product's `os.ExpandEnv` behaviour.
pub fn interpolate_env<F>(raw: &str, lookup: F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    // Hand-rolled scan so we do not pull a regex just for `${NAME}`.
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if let Some(close) = raw[i + 2..].find('}') {
                let name = &raw[i + 2..i + 2 + close];
                if is_valid_env_name(name) {
                    out.push_str(&lookup(name).unwrap_or_default());
                    i = i + 2 + close + 1;
                    continue;
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn is_valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !name.as_bytes()[0].is_ascii_digit()
}

/// Parse an already-interpolated YAML string into `Config`, rejecting unknown
/// keys. Kept generic-free for a clear error surface.
pub fn parse_str(interpolated: &str) -> Result<Config> {
    serde_yaml::from_str::<Config>(interpolated).map_err(|e| Error::Config(e.to_string()))
}

/// Full load pipeline: read the file at `path`, interpolate `${VAR}` from the
/// process environment, then parse into `Config`.
pub fn load(path: &Path) -> Result<Config> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| Error::Config(format!("cannot read {}: {e}", path.display())))?;
    let interpolated = interpolate_env(&raw, |name| std::env::var(name).ok());
    parse_str(&interpolated)
}

/// Convenience used by tests: interpolate with an explicit environment map.
pub fn load_str_with_env(raw: &str, env: &BTreeMap<String, String>) -> Result<Config> {
    let interpolated = interpolate_env(raw, |name| env.get(name).cloned());
    parse_str(&interpolated)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Acceptance: a config file passed via --config is loaded and decoded.
    #[test]
    fn loads_config_from_explicit_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leafmask.yaml");
        std::fs::write(&path, "common:\n  tmp_dir: /var/tmp/leafmask\n").unwrap();

        let located = locate(Some(path.clone()), None).unwrap();
        assert_eq!(located, path);
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.common.tmp_dir.as_deref(), Some("/var/tmp/leafmask"));
    }

    // Acceptance: without --config, LEAFMASK_CONFIG locates the file.
    #[test]
    fn falls_back_to_env_var_for_location() {
        let located = locate(None, Some("/etc/leafmask/config.yaml".into())).unwrap();
        assert_eq!(located, PathBuf::from("/etc/leafmask/config.yaml"));

        // Neither provided is an error, not a silent default.
        assert!(locate(None, None).is_err());
        assert!(locate(None, Some(String::new())).is_err());
    }

    // Acceptance: ${VAR} inside the file is replaced before use.
    #[test]
    fn interpolates_environment_variables() {
        let mut env = BTreeMap::new();
        env.insert("LM_TMP".to_string(), "/snapshots".to_string());
        let cfg = load_str_with_env("common:\n  tmp_dir: ${LM_TMP}\n", &env).unwrap();
        assert_eq!(cfg.common.tmp_dir.as_deref(), Some("/snapshots"));

        // Undefined variable expands to empty, does not blow up.
        assert_eq!(interpolate_env("${MISSING}/x", |_| None), "/x");
        assert_eq!(interpolate_env("a $ b {c}", |_| None), "a $ b {c}");
    }

    // Acceptance: an unknown key is rejected as an error.
    #[test]
    fn rejects_unknown_keys() {
        let err = parse_str("common:\n  tmp_dir: /t\nbogus_key: 1\n").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("bogus_key"), "message was: {msg}");
    }

    // Acceptance: transformer params parse correctly regardless of key casing.
    #[test]
    fn params_preserve_case_and_query_case_insensitively() {
        let params: Params = serde_yaml::from_str("Column: email\nKEEP_Null: true\n").unwrap();
        // Original casing is preserved on iteration...
        let keys: Vec<&str> = params.iter().map(|(k, _)| k).collect();
        assert_eq!(keys, vec!["Column", "KEEP_Null"]);
        // ...and lookup is case-insensitive.
        assert_eq!(params.get("column").and_then(|v| v.as_str()), Some("email"));
        assert_eq!(
            params.get("keep_null").and_then(|v| v.as_bool()),
            Some(true)
        );
    }
}
