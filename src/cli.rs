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
}

/// Load the config if one was provided (or discoverable via env).
fn maybe_config(cli: &Cli) -> crate::Result<Option<crate::config::Config>> {
    match crate::config::locate(cli.config.clone(), None) {
        Ok(path) => Ok(Some(crate::config::load(&path)?)),
        // No config is fine for commands that do not require one.
        Err(_) => Ok(None),
    }
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
    }
}
