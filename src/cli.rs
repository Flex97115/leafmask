//! CLI definition and dispatch.

use clap::{Args, Parser, Subcommand};

/// Leafmask command-line interface.
#[derive(Debug, Parser)]
#[command(name = "leafmask", version, about, long_about = None)]
pub struct Cli {
    /// Path to the YAML configuration file. Falls back to $LEAFMASK_CONFIG.
    #[arg(long, global = true, env = "LEAFMASK_CONFIG")]
    pub config: Option<std::path::PathBuf>,

    /// MongoDB connection URI (overrides `mongodb.uri` in config).
    #[arg(long, global = true, env = "LEAFMASK_MONGO_URI")]
    pub uri: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

/// Flags for the `dump` command.
#[derive(Debug, Args)]
pub struct DumpArgs {
    /// Compress collection data with gzip.
    #[arg(long)]
    pub gzip: bool,
    /// Restrict to these databases (repeatable).
    #[arg(long = "include-db")]
    pub include_db: Vec<String>,
    /// Skip these databases (repeatable).
    #[arg(long = "exclude-db")]
    pub exclude_db: Vec<String>,
    /// Restrict to these collections (repeatable).
    #[arg(long = "include-collection")]
    pub include_collection: Vec<String>,
    /// Skip these collections (repeatable).
    #[arg(long = "exclude-collection")]
    pub exclude_collection: Vec<String>,
    /// Number of parallel jobs (accepted; execution is sequential).
    #[arg(long, default_value_t = 1)]
    pub jobs: usize,
    /// Exclude index definitions and collection options.
    #[arg(long)]
    pub no_indexes: bool,
}

/// Flags for the `restore` command.
#[derive(Debug, Args)]
pub struct RestoreArgs {
    /// Dump id to restore, or `latest`.
    pub id: String,
    #[arg(long = "include-collection")]
    pub include_collection: Vec<String>,
    #[arg(long = "exclude-collection")]
    pub exclude_collection: Vec<String>,
    #[arg(long = "include-index")]
    pub include_index: Vec<String>,
    #[arg(long = "exclude-index")]
    pub exclude_index: Vec<String>,
    /// Documents per bulk-insert batch.
    #[arg(long, default_value_t = 1000)]
    pub batch_size: usize,
    /// Use ordered bulk writes.
    #[arg(long)]
    pub ordered: bool,
    /// Create indexes/validators after documents.
    #[arg(long)]
    pub dependency_order: bool,
    /// Abort the whole restore on a non-excluded error.
    #[arg(long)]
    pub exit_on_error: bool,
    /// Drop each restored collection before recreating it from the dump.
    #[arg(long)]
    pub clean: bool,
}

/// Flags for the `validate` command.
#[derive(Debug, Args)]
pub struct ValidateArgs {
    /// Preview transformations against sampled documents.
    #[arg(long)]
    pub data: bool,
    /// Database to sample from.
    #[arg(long)]
    pub database: String,
    /// Collection to sample from.
    #[arg(long)]
    pub collection: String,
    /// Maximum documents to sample.
    #[arg(long, default_value_t = 10)]
    pub rows_limit: usize,
    /// Output format: text or json.
    #[arg(long, default_value = "text")]
    pub format: String,
    /// Per-document layout: vertical or horizontal.
    #[arg(long, default_value = "vertical")]
    pub table_format: String,
    /// Only show fields that have a transformer configured.
    #[arg(long)]
    pub transformed_only: bool,
    /// Fail on any unresolved validation warning.
    #[arg(long)]
    pub strict: bool,
}

/// Top-level subcommands. Individual features flesh these out.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// List available transformers.
    ListTransformers,
    /// Show one transformer's documentation.
    ShowTransformer {
        /// Transformer name to look up.
        name: String,
    },
    /// List dumps held in the configured storage.
    ListDumps,
    /// Show a dump's metadata / table of contents (id or `latest`).
    ShowDump {
        /// Dump id, or `latest`.
        id: String,
    },
    /// Delete a dump by id, or prune dumps by a retention policy.
    Delete {
        /// Delete this specific dump id.
        #[arg(long)]
        id: Option<String>,
        /// Keep only the N most recent completed dumps.
        #[arg(long)]
        retain_recent: Option<usize>,
        /// Delete dumps created strictly before this RFC3339 date.
        #[arg(long)]
        before_date: Option<String>,
        /// Delete dumps that did not complete successfully.
        #[arg(long)]
        prune_failed: bool,
        /// Also delete unknown/in-progress dumps.
        #[arg(long)]
        prune_unsafe: bool,
        /// Report what would be deleted without deleting anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Create a logical dump of the configured MongoDB (requires --features mongo).
    Dump(DumpArgs),
    /// Restore a dump into the configured MongoDB (requires --features mongo).
    Restore(RestoreArgs),
    /// Validate/preview transformations against live data (requires --features mongo).
    Validate(ValidateArgs),
}

/// Load the config if one was provided (or discoverable via env).
fn maybe_config(cli: &Cli) -> crate::Result<Option<crate::config::Config>> {
    match crate::config::locate(cli.config.clone(), None) {
        Ok(path) => Ok(Some(crate::config::load(&path)?)),
        // No config is fine for commands that do not require one.
        Err(_) => Ok(None),
    }
}

/// Load config and open the configured storage backend (required for any
/// command that reads or writes dumps).
fn open_storage(cli: &Cli) -> crate::Result<std::sync::Arc<dyn crate::storage::Storage>> {
    crate::storage::open_from_config(&require_config(cli)?.storage)
}

/// Load the config, erroring clearly if none was provided.
fn require_config(cli: &Cli) -> crate::Result<crate::config::Config> {
    let path = crate::config::locate(cli.config.clone(), None)?;
    crate::config::load(&path)
}

/// Resolve the MongoDB URI: `--uri` wins, then `mongodb.uri` in config.
#[cfg_attr(not(feature = "mongo"), allow(dead_code))]
fn resolve_uri(cli: &Cli, config: &crate::config::Config) -> crate::Result<String> {
    cli.uri
        .clone()
        .or_else(|| config.mongodb.uri.clone())
        .ok_or_else(|| {
            crate::Error::Config("MongoDB URI required: pass --uri or set mongodb.uri".into())
        })
}

/// Resolve a repeatable filter list: a non-empty CLI value overrides the
/// config value entirely (same precedence as `--uri` over `mongodb.uri`,
/// applied per-list instead of per-scalar). An empty CLI value falls back to
/// whatever the config declared, including empty.
#[cfg_attr(not(feature = "mongo"), allow(dead_code))]
fn resolve_list(cli: Vec<String>, config: Vec<String>) -> Vec<String> {
    if cli.is_empty() {
        config
    } else {
        cli
    }
}

/// Parse the `dump.transformation` section into a plan-ready config list.
#[cfg_attr(not(feature = "mongo"), allow(dead_code))]
fn transformation_configs(
    config: &crate::config::Config,
) -> crate::Result<Vec<crate::transform::apply::TransformationConfig>> {
    if config.dump.is_null() {
        return Ok(Vec::new());
    }
    #[derive(serde::Deserialize, Default)]
    struct DumpSection {
        #[serde(default)]
        transformation: Vec<crate::transform::apply::TransformationConfig>,
    }
    let section: DumpSection = serde_yaml::from_value(config.dump.clone())
        .map_err(|e| crate::Error::Config(format!("dump.transformation: {e}")))?;
    Ok(section.transformation)
}

/// Parse `dump.subset_conds` (collection -> expression string) into the
/// per-collection MongoDB filters `Dump.filters` expects, so the condition is
/// pushed down to the server instead of every document being fetched and
/// discarded after the fact.
#[cfg_attr(not(feature = "mongo"), allow(dead_code))]
fn dump_subset_filters(
    config: &crate::config::Config,
) -> crate::Result<std::collections::BTreeMap<String, bson::Document>> {
    if config.dump.is_null() {
        return Ok(Default::default());
    }
    #[derive(serde::Deserialize, Default)]
    struct DumpSection {
        #[serde(default)]
        subset_conds: std::collections::BTreeMap<String, String>,
    }
    let section: DumpSection = serde_yaml::from_value(config.dump.clone())
        .map_err(|e| crate::Error::Config(format!("dump.subset_conds: {e}")))?;
    section
        .subset_conds
        .into_iter()
        .map(|(collection, expr)| {
            let filter = crate::transform::condition::Condition::parse(&expr)?.to_filter();
            Ok((collection, filter))
        })
        .collect()
}

/// Four filter lists in `(include, exclude, ...)` pairs, as returned by
/// `dump_filter_lists`/`restore_filter_lists`.
type FilterListQuad = (Vec<String>, Vec<String>, Vec<String>, Vec<String>);

/// Parse `dump.include_databases` / `exclude_databases` / `include_collections`
/// / `exclude_collections` from config. These are the YAML fallback for the
/// `--include-db`/`--exclude-db`/`--include-collection`/`--exclude-collection`
/// flags — see `resolve_list` for the override precedence.
#[cfg_attr(not(feature = "mongo"), allow(dead_code))]
fn dump_filter_lists(config: &crate::config::Config) -> crate::Result<FilterListQuad> {
    if config.dump.is_null() {
        return Ok(Default::default());
    }
    #[derive(serde::Deserialize, Default)]
    struct DumpSection {
        #[serde(default)]
        include_databases: Vec<String>,
        #[serde(default)]
        exclude_databases: Vec<String>,
        #[serde(default)]
        include_collections: Vec<String>,
        #[serde(default)]
        exclude_collections: Vec<String>,
    }
    let section: DumpSection = serde_yaml::from_value(config.dump.clone())
        .map_err(|e| crate::Error::Config(format!("dump filters: {e}")))?;
    Ok((
        section.include_databases,
        section.exclude_databases,
        section.include_collections,
        section.exclude_collections,
    ))
}

/// Parse `restore.include_collections` / `exclude_collections` /
/// `include_indexes` / `exclude_indexes` from config. These are the YAML
/// fallback for the `--include-collection`/`--exclude-collection`/
/// `--include-index`/`--exclude-index` flags — see `resolve_list` for the
/// override precedence. Parsed separately from the `insert_error_exclusions`
/// / `scripts` section already handled inside `cmd_restore`, mirroring how
/// `dump.transformation` and `dump.subset_conds` are each parsed by their own
/// function above.
#[cfg_attr(not(feature = "mongo"), allow(dead_code))]
fn restore_filter_lists(config: &crate::config::Config) -> crate::Result<FilterListQuad> {
    if config.restore.is_null() {
        return Ok(Default::default());
    }
    #[derive(serde::Deserialize, Default)]
    struct RestoreFilterSection {
        #[serde(default)]
        include_collections: Vec<String>,
        #[serde(default)]
        exclude_collections: Vec<String>,
        #[serde(default)]
        include_indexes: Vec<String>,
        #[serde(default)]
        exclude_indexes: Vec<String>,
    }
    let section: RestoreFilterSection = serde_yaml::from_value(config.restore.clone())
        .map_err(|e| crate::Error::Config(format!("restore filters: {e}")))?;
    Ok((
        section.include_collections,
        section.exclude_collections,
        section.include_indexes,
        section.exclude_indexes,
    ))
}

/// Entry point invoked by `main`.
pub fn run(cli: Cli) -> crate::Result<()> {
    match &cli.command {
        Command::ListTransformers => {
            let config = maybe_config(&cli)?;
            let registry = crate::catalog::build_registry(config.as_ref())?;
            println!("{}", crate::catalog::list(&registry));
            Ok(())
        }
        Command::ShowTransformer { name } => {
            let config = maybe_config(&cli)?;
            let registry = crate::catalog::build_registry(config.as_ref())?;
            print!("{}", crate::catalog::show(&registry, name)?);
            Ok(())
        }
        Command::ListDumps => {
            let storage = open_storage(&cli)?;
            println!("{}", crate::dump::management::list_dumps(storage.as_ref())?);
            Ok(())
        }
        Command::ShowDump { id } => {
            let storage = open_storage(&cli)?;
            print!(
                "{}",
                crate::dump::management::show_dump(storage.as_ref(), id)?
            );
            Ok(())
        }
        Command::Delete {
            id,
            retain_recent,
            before_date,
            prune_failed,
            prune_unsafe,
            dry_run,
        } => {
            let storage = open_storage(&cli)?;
            if let Some(id) = id {
                if *dry_run {
                    println!("would delete {id}");
                } else {
                    crate::dump::management::delete_dump(storage.as_ref(), id)?;
                    println!("deleted {id}");
                }
                return Ok(());
            }
            let before = match before_date {
                Some(s) => Some(
                    chrono::DateTime::parse_from_rfc3339(s)
                        .map_err(|e| crate::Error::Config(format!("invalid --before-date: {e}")))?
                        .with_timezone(&chrono::Utc),
                ),
                None => None,
            };
            let policy = crate::dump::management::RetentionPolicy {
                retain_recent: *retain_recent,
                retain_for: None,
                before_date: before,
                prune_failed: *prune_failed,
                prune_unsafe: *prune_unsafe,
            };
            let deleted = crate::dump::management::prune(
                storage.as_ref(),
                &policy,
                chrono::Utc::now(),
                *dry_run,
            )?;
            let verb = if *dry_run { "would delete" } else { "deleted" };
            for id in deleted {
                println!("{verb} {id}");
            }
            Ok(())
        }
        Command::Dump(args) => cmd_dump(&cli, args),
        Command::Restore(args) => cmd_restore(&cli, args),
        Command::Validate(args) => cmd_validate(&cli, args),
    }
}

// ---------------------------------------------------------------------------
// MongoDB-backed commands. Compiled with the real driver only when the `mongo`
// feature is enabled; otherwise they fail fast with an actionable message.
// ---------------------------------------------------------------------------

#[cfg(not(feature = "mongo"))]
fn mongo_unavailable(cmd: &str) -> crate::Result<()> {
    Err(crate::Error::Config(format!(
        "`{cmd}` needs a MongoDB connection; rebuild leafmask with --features mongo"
    )))
}

#[cfg(not(feature = "mongo"))]
fn cmd_dump(_cli: &Cli, _args: &DumpArgs) -> crate::Result<()> {
    mongo_unavailable("dump")
}
#[cfg(not(feature = "mongo"))]
fn cmd_restore(_cli: &Cli, _args: &RestoreArgs) -> crate::Result<()> {
    mongo_unavailable("restore")
}
#[cfg(not(feature = "mongo"))]
fn cmd_validate(_cli: &Cli, _args: &ValidateArgs) -> crate::Result<()> {
    mongo_unavailable("validate")
}

#[cfg(feature = "mongo")]
fn engine_from(config: &crate::config::Config) -> crate::hash::HashEngine {
    let (salt, is_default) = config.common.resolve_salt();
    if is_default {
        log::warn!(
            "common.salt is not set: using the built-in default salt, which is public and makes \
             deterministic anonymization reversible. Set common.salt to a private value."
        );
    }
    crate::hash::HashEngine::new(salt)
}

#[cfg(feature = "mongo")]
fn cmd_dump(cli: &Cli, args: &DumpArgs) -> crate::Result<()> {
    use crate::dump::{Dump, DumpOptions};
    use crate::transform::apply::TransformationPlan;

    let config = require_config(cli)?;
    let storage = crate::storage::open_from_config(&config.storage)?;
    let driver = crate::mongo::MongoDriver::connect(&resolve_uri(cli, &config)?)?;
    let registry = crate::catalog::build_registry(Some(&config))?;
    let engine = engine_from(&config);

    let configs = transformation_configs(&config)?;
    let plan = TransformationPlan::compile(&configs, &registry, &engine)?;

    let (cfg_include_db, cfg_exclude_db, cfg_include_coll, cfg_exclude_coll) =
        dump_filter_lists(&config)?;
    let options = DumpOptions {
        tmp_dir: config.common.tmp_dir.clone(),
        include_databases: resolve_list(args.include_db.clone(), cfg_include_db),
        exclude_databases: resolve_list(args.exclude_db.clone(), cfg_exclude_db),
        include_collections: resolve_list(args.include_collection.clone(), cfg_include_coll),
        exclude_collections: resolve_list(args.exclude_collection.clone(), cfg_exclude_coll),
        gzip: args.gzip,
        parallel_jobs: args.jobs,
        no_indexes: args.no_indexes,
    };
    let dump = Dump {
        storage: storage.as_ref(),
        source: &driver,
        registry: &registry,
        engine: &engine,
        plan: if configs.is_empty() {
            None
        } else {
            Some(&plan)
        },
        filters: dump_subset_filters(&config)?,
        options,
    };
    let meta = dump.run(chrono::Utc::now())?;
    println!("created dump {} ({} bytes)", meta.id, meta.size);
    Ok(())
}

#[cfg(feature = "mongo")]
fn cmd_restore(cli: &Cli, args: &RestoreArgs) -> crate::Result<()> {
    use crate::restore::{ErrorExclusions, ProcessScriptRunner, Restore, RestoreOptions, Scripts};

    let config = require_config(cli)?;
    let storage = crate::storage::open_from_config(&config.storage)?;
    let uri = resolve_uri(cli, &config)?;
    let driver = crate::mongo::MongoDriver::connect(&uri)?;

    #[derive(serde::Deserialize, Default)]
    struct RestoreSection {
        #[serde(default)]
        insert_error_exclusions: ErrorExclusions,
        #[serde(default)]
        scripts: Scripts,
        #[serde(default)]
        clean: bool,
    }
    let section: RestoreSection = if config.restore.is_null() {
        RestoreSection::default()
    } else {
        serde_yaml::from_value(config.restore.clone())
            .map_err(|e| crate::Error::Config(format!("restore: {e}")))?
    };

    let (cfg_include_coll, cfg_exclude_coll, cfg_include_idx, cfg_exclude_idx) =
        restore_filter_lists(&config)?;
    let options = RestoreOptions {
        include_collections: resolve_list(args.include_collection.clone(), cfg_include_coll),
        exclude_collections: resolve_list(args.exclude_collection.clone(), cfg_exclude_coll),
        include_indexes: resolve_list(args.include_index.clone(), cfg_include_idx),
        exclude_indexes: resolve_list(args.exclude_index.clone(), cfg_exclude_idx),
        batch_size: args.batch_size,
        ordered: args.ordered,
        dependency_order: args.dependency_order,
        exit_on_error: args.exit_on_error,
        clean: args.clean || section.clean,
    };
    let restore = Restore {
        storage: storage.as_ref(),
        sink: &driver,
        exclusions: section.insert_error_exclusions,
        scripts: section.scripts,
        options,
    };
    let report = restore.run(&args.id, &ProcessScriptRunner::new(uri))?;
    println!(
        "restored: {} inserted, {} skipped, {} indexes",
        report.inserted, report.skipped, report.indexes_created
    );
    Ok(())
}

#[cfg(feature = "mongo")]
fn cmd_validate(cli: &Cli, args: &ValidateArgs) -> crate::Result<()> {
    use crate::mongo::MongoSource;
    use crate::transform::apply::TransformationPlan;
    use crate::validate::{parse_format, parse_table_format, preview, PreviewOptions, Warnings};

    if !args.data {
        return Err(crate::Error::Validation(
            "nothing to do: pass --data to preview transformations".into(),
        ));
    }
    // Validate output options up front, before any database work.
    let format = parse_format(&args.format)?;
    let table_format = parse_table_format(&args.table_format)?;

    let config = require_config(cli)?;
    let driver = crate::mongo::MongoDriver::connect(&resolve_uri(cli, &config)?)?;
    let registry = crate::catalog::build_registry(Some(&config))?;
    let engine = engine_from(&config);
    let configs = transformation_configs(&config)?;
    let plan = TransformationPlan::compile(&configs, &registry, &engine)?;

    let docs = driver
        .read_collection(&args.database, &args.collection)?
        .documents;
    let opts = PreviewOptions {
        rows_limit: args.rows_limit,
        format,
        table_format,
        transformed_only: args.transformed_only,
        strict: args.strict,
    };
    let out = preview(
        &plan,
        &registry,
        &engine,
        &args.collection,
        &docs,
        &Warnings::new(),
        &config.resolved_warnings,
        &opts,
    )?;
    print!("{out}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_list_falls_back_to_config_when_cli_is_empty() {
        let result = resolve_list(vec![], vec!["a".to_string(), "b".to_string()]);
        assert_eq!(result, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn resolve_list_cli_overrides_nonempty_config_entirely() {
        let result = resolve_list(
            vec!["x".to_string()],
            vec!["a".to_string(), "b".to_string()],
        );
        assert_eq!(result, vec!["x".to_string()]);
    }

    #[test]
    fn resolve_list_both_empty_is_empty() {
        let result: Vec<String> = resolve_list(vec![], vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn dump_filter_lists_parses_all_four_fields() {
        let config = crate::config::parse_str(
            "dump:\n  include_databases: [shop]\n  exclude_databases: [logs]\n  include_collections: [users]\n  exclude_collections: [sessions]\n",
        )
        .unwrap();
        let (include_db, exclude_db, include_coll, exclude_coll) =
            dump_filter_lists(&config).unwrap();
        assert_eq!(include_db, vec!["shop".to_string()]);
        assert_eq!(exclude_db, vec!["logs".to_string()]);
        assert_eq!(include_coll, vec!["users".to_string()]);
        assert_eq!(exclude_coll, vec!["sessions".to_string()]);
    }

    #[test]
    fn dump_filter_lists_defaults_to_empty_without_dump_section() {
        let config = crate::config::parse_str("common:\n  tmp_dir: /tmp\n").unwrap();
        let (include_db, exclude_db, include_coll, exclude_coll) =
            dump_filter_lists(&config).unwrap();
        assert!(include_db.is_empty());
        assert!(exclude_db.is_empty());
        assert!(include_coll.is_empty());
        assert!(exclude_coll.is_empty());
    }

    #[test]
    fn dump_filter_lists_ignores_unrelated_dump_fields() {
        let config =
            crate::config::parse_str("dump:\n  transformation: []\n  include_databases: [shop]\n")
                .unwrap();
        let (include_db, _, _, _) = dump_filter_lists(&config).unwrap();
        assert_eq!(include_db, vec!["shop".to_string()]);
    }

    #[test]
    fn dump_subset_filters_translates_expressions_to_mongo_filters() {
        let config =
            crate::config::parse_str("dump:\n  subset_conds:\n    events: \"value >= 3\"\n")
                .unwrap();
        let filters = dump_subset_filters(&config).unwrap();
        let expected = crate::transform::condition::Condition::parse("value >= 3")
            .unwrap()
            .to_filter();
        assert_eq!(filters.get("events"), Some(&expected));
    }

    #[test]
    fn dump_subset_filters_defaults_to_empty_without_subset_conds() {
        let config = crate::config::parse_str("dump:\n  include_databases: [shop]\n").unwrap();
        let filters = dump_subset_filters(&config).unwrap();
        assert!(filters.is_empty());
    }

    #[test]
    fn restore_filter_lists_parses_all_four_fields() {
        let config = crate::config::parse_str(
            "restore:\n  include_collections: [users]\n  exclude_collections: [sessions]\n  include_indexes: [email_idx]\n  exclude_indexes: [legacy_idx]\n",
        )
        .unwrap();
        let (include_coll, exclude_coll, include_idx, exclude_idx) =
            restore_filter_lists(&config).unwrap();
        assert_eq!(include_coll, vec!["users".to_string()]);
        assert_eq!(exclude_coll, vec!["sessions".to_string()]);
        assert_eq!(include_idx, vec!["email_idx".to_string()]);
        assert_eq!(exclude_idx, vec!["legacy_idx".to_string()]);
    }

    #[test]
    fn restore_filter_lists_defaults_to_empty_without_restore_section() {
        let config = crate::config::parse_str("common:\n  tmp_dir: /tmp\n").unwrap();
        let (include_coll, exclude_coll, include_idx, exclude_idx) =
            restore_filter_lists(&config).unwrap();
        assert!(include_coll.is_empty());
        assert!(exclude_coll.is_empty());
        assert!(include_idx.is_empty());
        assert!(exclude_idx.is_empty());
    }

    #[test]
    fn restore_filter_lists_ignores_unrelated_restore_fields() {
        let config = crate::config::parse_str(
            "restore:\n  insert_error_exclusions:\n    global_error_codes: [11000]\n  include_collections: [users]\n",
        )
        .unwrap();
        let (include_coll, _, _, _) = restore_filter_lists(&config).unwrap();
        assert_eq!(include_coll, vec!["users".to_string()]);
    }
}
