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

/// Entry point invoked by `main`. Returns a process exit code.
pub fn run(cli: Cli) -> crate::Result<()> {
    match cli.command {
        Command::ListTransformers | Command::ShowTransformer { .. } => {
            // Wired up by the catalog features.
            Ok(())
        }
    }
}
