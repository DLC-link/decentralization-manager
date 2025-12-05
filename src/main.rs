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
        Commands::QueryParties { ref party_id_prefix } => {
            let node_config = args.config.as_ref().ok_or_else(|| {
                anyhow::anyhow!("Configuration file is required. Use -c <config-file>")
            })?;

            tracing::info!("Loading configuration from: {}", node_config.display());
            let config = NodeConfig::from_file(node_config).await?;

            dec_party_onboarding::query_parties::query_parties(&config, party_id_prefix).await?;
        }
        Commands::Onboarding | Commands::Contracts | Commands::Kick { .. } => {
            let node_config = args.config.as_ref().ok_or_else(|| {
                anyhow::anyhow!("Configuration file is required. Use -c <config-file>")
            })?;

            tracing::info!("Loading configuration from: {}", node_config.display());
            let config = NodeConfig::from_file(node_config).await?;

            match args.command {
                Commands::Onboarding => {
                    workflow::start_node(config, workflow::WorkflowType::Onboarding, None).await?;
                }
                Commands::Contracts => {
                    workflow::start_node(config, workflow::WorkflowType::Contracts, None).await?;
                }
                Commands::Kick {
                    decentralized_party_id,
                    participant_id,
                } => {
                    if participant_id.is_empty() {
                        anyhow::bail!("At least one --participant-id must be specified");
                    }

                    let kick_config =
                        workflow::KickConfig::new(decentralized_party_id, participant_id);
                    workflow::start_node(config, workflow::WorkflowType::Kick, Some(kick_config))
                        .await?;
                }
                _ => unreachable!(),
            }
        }
    }

    tracing::info!("Command completed successfully");
    Ok(())
}
