use clap::Parser;
use leafmask::cli::{self, Cli};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .format_target(false)
        .init();

    let cli = Cli::parse();
    if let Err(e) = cli::run(cli) {
        eprintln!("leafmask: {e}");
        std::process::exit(1);
    }
}
