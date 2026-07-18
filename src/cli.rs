//! CLI definition and dispatch (populated per-feature).

use clap::{Parser, Subcommand};

/// Leafmask command-line interface.
#[derive(Debug, Parser)]
#[command(name = "leafmask", version, about, long_about = None)]
pub struct Cli {
    /// Path to the YAML configuration file. Falls back to $LEAFMASK_CONFIG.
    #[arg(long, global = true, env = "LEAFMASK_CONFIG")]
    pub config: Option<std::path::PathBuf>,

    #[command(subcommand)]
    pub command: Command,
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
    let path = crate::config::locate(cli.config.clone(), None)?;
    let config = crate::config::load(&path)?;
    crate::storage::open_from_config(&config.storage)
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
            print!("{}", crate::dump::management::show_dump(storage.as_ref(), id)?);
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
    }
}
