mod cli;

use tracing_subscriber::{filter::EnvFilter, prelude::*};

use cli::{Cli, Commands, Parser};
use dec_party_manager::{config::NodeConfig, error::Result};

#[tokio::main]
async fn main() -> Result {
    let filter = EnvFilter::try_new("info,tokio_noise=error,hyper_noise=error")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));

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
        Commands::Serve {
            ref host,
            port,
            test,
        } => {
            dec_party_manager::server::start_server(host, port, config, test).await?;
        }
    }

    tracing::info!("Command completed successfully");
    Ok(())
}
