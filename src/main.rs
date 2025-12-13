mod cli;

use tracing_subscriber::{
    filter::{EnvFilter, LevelFilter},
    prelude::*,
};

use cli::{Cli, Commands, Parser};
use dec_party_onboarding::{config::NodeConfig, error::Result};

#[tokio::main]
async fn main() -> Result {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(filter))
        .init();

    let args = Cli::parse();

    let path = args
        .config
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Configuration file is required. Use -c <config-file>"))?;
    tracing::info!("Loading configuration from: {path}", path = path.display());
    let config = NodeConfig::from_file(path).await?;

    match args.command {
        Commands::Serve { ref host, port } => {
            dec_party_onboarding::server::start_server(host, port, config).await?;
        }
    }

    tracing::info!("Command completed successfully");
    Ok(())
}
