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

/// The salt used when `common.salt` is left unset. It is a fixed, public
/// constant, so anonymization run without an explicit salt is reversible by
/// anyone who knows leafmask — callers should warn when they fall back to it.
pub const DEFAULT_SALT: &str = "leafmask";

impl Common {
    /// The effective anonymization salt and whether it is the insecure built-in
    /// default. When the boolean is `true`, the operator did not configure
    /// `common.salt` and deterministic transformations are reversible.
    pub fn resolve_salt(&self) -> (String, bool) {
        match &self.salt {
            Some(s) if !s.is_empty() => (s.clone(), false),
            _ => (DEFAULT_SALT.to_string(), true),
        }
    }
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
    /// Raw virtual-reference declarations (MongoDB enforces no foreign keys, so
    /// every inter-collection relationship subsetting and transformation
    /// propagation follow is declared here), refined by the subset features.
    #[serde(default)]
    pub virtual_references: serde_yaml::Value,
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
        // Copy the whole UTF-8 character starting at byte `i` — never a lone
        // byte, which would corrupt any multibyte character into mojibake.
        // `${`, `}` and env-name bytes are all ASCII, so scanning on bytes
        // above stays correct.
        let ch = raw[i..].chars().next().expect("i is a char boundary");
        out.push(ch);
        i += ch.len_utf8();
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
/// Keys accepted inside the `dump:` section.
///
/// The section is deserialized as a raw `serde_yaml::Value` and then refined by
/// several narrow structs in `cli.rs`, each picking out only the keys it needs
/// (`transformation`, `subset_conds`, the four filter lists). No single one of
/// them can carry `deny_unknown_fields` without rejecting the others' perfectly
/// legitimate keys, so this list stands in for it. Keep it in sync with those
/// structs — a key missing here is rejected even though it works.
const DUMP_KEYS: &[&str] = &[
    "transformation",
    "subset_conds",
    "include_databases",
    "exclude_databases",
    "include_collections",
    "exclude_collections",
];

/// Keys accepted inside the `restore:` section. Same reasoning as [`DUMP_KEYS`]:
/// split across `cmd_restore`'s own section struct and `restore_filter_lists`.
const RESTORE_KEYS: &[&str] = &[
    "insert_error_exclusions",
    "scripts",
    "clean",
    "include_collections",
    "exclude_collections",
    "include_indexes",
    "exclude_indexes",
];

/// Reject unknown keys inside a raw section, matching serde's message shape.
///
/// Silently ignoring one of these is not a cosmetic problem: a `dump:` section
/// whose `transformation` key is misspelled applies no transformers at all, and
/// the dump then completes successfully while writing unmasked production data.
/// Failing the run is the only safe outcome.
fn check_section_keys(section: &str, value: &serde_yaml::Value, known: &[&str]) -> Result<()> {
    // A null (absent) section, or one that is not a mapping at all, is left to
    // the narrow structs to report — they produce a better type error than
    // anything this function could say.
    let Some(mapping) = value.as_mapping() else {
        return Ok(());
    };
    for key in mapping.keys() {
        let Some(name) = key.as_str() else { continue };
        if !known.contains(&name) {
            let expected = known
                .iter()
                .map(|k| format!("`{k}`"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(Error::Config(format!(
                "{section}: unknown field `{name}`, expected one of {expected}"
            )));
        }
    }
    Ok(())
}

pub fn parse_str(interpolated: &str) -> Result<Config> {
    let config =
        serde_yaml::from_str::<Config>(interpolated).map_err(|e| Error::Config(e.to_string()))?;
    // Top-level keys are already covered by `deny_unknown_fields` on `Config`;
    // these two sections are raw values, so they need checking by hand.
    check_section_keys("dump", &config.dump, DUMP_KEYS)?;
    check_section_keys("restore", &config.restore, RESTORE_KEYS)?;
    Ok(config)
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

    // Non-ASCII bytes in the file must survive interpolation untouched: a config
    // value such as a password or path with accented/multibyte characters must
    // not be mangled into mojibake.
    #[test]
    fn interpolation_preserves_non_ascii() {
        assert_eq!(
            interpolate_env("café ☕ ${X}", |_| Some("naïve".into())),
            "café ☕ naïve"
        );
        // Multibyte characters immediately around a placeholder.
        assert_eq!(interpolate_env("é${X}é", |_| None), "éé");
        // A whole config value round-trips through the loader unchanged.
        let mut env = BTreeMap::new();
        env.insert("PW".to_string(), "pÀsswörd".to_string());
        let cfg = load_str_with_env("mongodb:\n  uri: mongodb://user:${PW}@h\n", &env).unwrap();
        assert_eq!(
            cfg.mongodb.uri.as_deref(),
            Some("mongodb://user:pÀsswörd@h")
        );
    }

    // A configured salt is used as-is; an absent or empty salt falls back to
    // the insecure public default and is flagged so callers can warn.
    #[test]
    fn resolve_salt_flags_the_insecure_default() {
        let configured = Common {
            salt: Some("pepper".into()),
            ..Default::default()
        };
        assert_eq!(configured.resolve_salt(), ("pepper".to_string(), false));

        let (salt, is_default) = Common::default().resolve_salt();
        assert_eq!(salt, DEFAULT_SALT);
        assert!(is_default);

        // An explicitly empty salt is treated as unset, not as a real salt.
        let empty = Common {
            salt: Some(String::new()),
            ..Default::default()
        };
        assert!(empty.resolve_salt().1);
    }

    // `virtual_references` is a declared key: MongoDB enforces no foreign keys,
    // so this section is the only way to express a relationship, and before it
    // existed `deny_unknown_fields` rejected any config that declared one.
    #[test]
    fn accepts_virtual_references_section() {
        let cfg = parse_str(
            "virtual_references:\n  - collection: orders\n    references:\n      - field: user_id\n        references_collection: users\n",
        )
        .unwrap();
        assert!(!cfg.virtual_references.is_null());
        // and it round-trips into the typed entries the subset features use.
        let entries: Vec<crate::subset::VirtualReferenceEntry> =
            serde_yaml::from_value(cfg.virtual_references).unwrap();
        assert_eq!(entries[0].collection, "orders");
        assert_eq!(entries[0].references[0].field, "user_id");
    }

    // Acceptance: an unknown key is rejected as an error.
    #[test]
    fn rejects_unknown_keys() {
        let err = parse_str("common:\n  tmp_dir: /t\nbogus_key: 1\n").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("bogus_key"), "message was: {msg}");
    }

    // The failure this guards against is the worst one this tool has: a
    // mistyped `transformation` key means no transformers are applied, the
    // dump succeeds, and unmasked production data is written out looking for
    // all the world like an anonymized dump.
    #[test]
    fn rejects_a_mistyped_transformation_section() {
        let err = parse_str(
            "dump:\n  transformations:\n    - collection: users\n      transformers:\n        - field: email\n          name: random_email\n",
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("transformations") && msg.contains("transformation"),
            "the error must name the bad key and offer the real one, got: {msg}"
        );
    }

    // Every key the `dump:` and `restore:` sections are refined into by cli.rs
    // must still parse. This is the guard against the check above being too
    // strict: rejecting a legitimate key would break working configs.
    #[test]
    fn accepts_every_documented_dump_and_restore_key() {
        let cfg = parse_str(
            "dump:\n\
             \x20 transformation: []\n\
             \x20 subset_conds: {}\n\
             \x20 include_databases: [a]\n\
             \x20 exclude_databases: [b]\n\
             \x20 include_collections: [c]\n\
             \x20 exclude_collections: [d]\n\
             restore:\n\
             \x20 clean: true\n\
             \x20 insert_error_exclusions: {}\n\
             \x20 scripts: {}\n\
             \x20 include_collections: [e]\n\
             \x20 exclude_collections: [f]\n\
             \x20 include_indexes: [g]\n\
             \x20 exclude_indexes: [h]\n",
        )
        .expect("every known key must be accepted");
        assert!(!cfg.dump.is_null() && !cfg.restore.is_null());
    }

    // An unknown key under `restore:` is rejected the same way.
    #[test]
    fn rejects_unknown_keys_inside_the_restore_section() {
        let err = parse_str("restore:\n  cleanup: true\n").unwrap_err();
        assert!(err.to_string().contains("cleanup"), "message was: {err}");
    }

    // The config in the docs must actually load. It drifted before: it showed
    // `subset_conds` as a top-level section when the code reads it from inside
    // `dump:`, so anyone copying the documented example hit
    // "unknown field `subset_conds`" on their first run.
    // Note this couples the test suite to `docs/`. The release build is
    // unaffected (`#[cfg(test)]` is not compiled by `cargo build`, which is all
    // the Dockerfile runs — `.dockerignore` drops `*.md`), but `cargo test`
    // does need the file present.
    #[test]
    fn the_documented_example_config_parses() {
        let markdown = include_str!("../docs/config-example.md");
        let yaml = markdown
            .split("```yaml")
            .nth(1)
            // The fence carries an info string (`title="leafmask.yaml"`);
            // the YAML itself starts on the next line.
            .and_then(|rest| rest.split_once('\n'))
            .and_then(|(_info, body)| body.split("```").next())
            .expect("docs/config-example.md must contain a ```yaml block");

        // The example interpolates ${VAR}s; supply them rather than depending
        // on the developer's environment.
        let env: BTreeMap<String, String> = [
            ("LEAFMASK_SALT", "salt"),
            ("MONGO_URI", "mongodb://localhost:27017"),
            ("AWS_ACCESS_KEY_ID", "id"),
            ("AWS_SECRET_ACCESS_KEY", "secret"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        let cfg = load_str_with_env(yaml, &env)
            .unwrap_or_else(|e| panic!("documented example config does not load: {e}"));

        // And the sections it advertises really are the ones the code reads.
        let transformations: Vec<crate::transform::apply::TransformationConfig> =
            serde_yaml::from_value(
                cfg.dump
                    .get("transformation")
                    .cloned()
                    .expect("example declares dump.transformation"),
            )
            .expect("dump.transformation decodes");
        assert!(!transformations.is_empty());
        assert!(
            cfg.dump.get("subset_conds").is_some(),
            "example must keep subset_conds inside the dump section"
        );
    }

    // Absent or empty sections stay valid — neither is a typo.
    #[test]
    fn absent_and_empty_sections_are_accepted() {
        parse_str("common:\n  tmp_dir: /t\n").expect("absent sections");
        parse_str("dump:\nrestore:\n").expect("null sections");
        parse_str("dump: {}\nrestore: {}\n").expect("empty sections");
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
