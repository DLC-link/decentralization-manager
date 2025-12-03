mod cli;

use tracing_subscriber::{
    filter::{EnvFilter, LevelFilter},
    prelude::*,
};

use cli::{Cli, Commands, Parser};
use dec_party_onboarding::{config::NodeConfig, error::Result, workflow};

#[tokio::main]
async fn main() -> Result {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(filter))
        .init();

    let args = Cli::parse();

    match args.command {
        Commands::Keygen { ref output } => {
            dec_party_onboarding::noise::generate_keypair(output).await?;
        }
        Commands::Onboarding | Commands::Contracts => {
            let node_config = args.config.as_ref().ok_or_else(|| {
                anyhow::anyhow!("Configuration file is required. Use -c <config-file>")
            })?;

            tracing::info!("Loading configuration from: {}", node_config.display());
            let config = NodeConfig::from_file(node_config).await?;

            let workflow_type = match args.command {
                Commands::Onboarding => workflow::WorkflowType::Onboarding,
                Commands::Contracts => workflow::WorkflowType::Contracts,
                _ => unreachable!(),
            };

            workflow::start_node(config, workflow_type).await?;
        }
    }

    tracing::info!("Command completed successfully");
    Ok(())
}
