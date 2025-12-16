mod cli;

use tracing_subscriber::{
    filter::{EnvFilter, LevelFilter},
    prelude::*,
};

use cli::{Cli, Commands, Parser};
use dec_party_manager::{config::NodeConfig, error::Result};

#[tokio::main]
async fn main() -> Result {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(filter))
        .init();

    let args = Cli::parse();

    tracing::info!(
        "Loading configuration from: {path}",
        path = args.dir.display()
    );
    let config = NodeConfig::from_dir(&args.dir).await?;

    match args.command {
        Commands::Serve { ref host, port } => {
            dec_party_manager::server::start_server(host, port, config).await?;
        }
    }

    tracing::info!("Command completed successfully");
    Ok(())
}
