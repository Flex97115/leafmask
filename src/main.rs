use clap::Parser;
use leafmask::cli::{self, Cli};

fn main() {
    let cli = Cli::parse();
    if let Err(e) = cli::run(cli) {
        eprintln!("leafmask: {e}");
        std::process::exit(1);
    }
}
