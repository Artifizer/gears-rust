mod registered_gears;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use toolkit::bootstrap::{AppConfig, run_server};

/// Standalone server for the object-storage example gear.
#[derive(Parser)]
#[command(name = "object-storage-server")]
#[command(about = "Object-storage example gear under Operation Governance")]
struct Cli {
    /// Path to the configuration file (see config/object-storage.yaml).
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Log verbosity level (-v debug, -vv trace)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Print effective configuration and exit
    #[arg(long)]
    print_config: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut config = AppConfig::load_or_default(cli.config.as_ref())?;
    config.apply_cli_overrides(cli.verbose);
    if cli.print_config {
        tracing::info!("Effective configuration:\n{}", config.to_yaml()?);
        return Ok(());
    }
    run_server(config).await
}
