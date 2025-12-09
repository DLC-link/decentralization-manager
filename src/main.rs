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

    if let Commands::Keygen { ref output } = args.command {
        dec_party_onboarding::noise::generate_keypair(output).await?;
        tracing::info!("Command completed successfully");
        return Ok(());
    }

    let path = args
        .config
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Configuration file is required. Use -c <config-file>"))?;
    tracing::info!("Loading configuration from: {}", path.display());
    let config = NodeConfig::from_file(path).await?;

    match args.command {
        Commands::Keygen { .. } => unreachable!(),
        Commands::QueryParties {
            ref party_id_prefix,
        } => {
            dec_party_onboarding::query_parties::query_parties(&config, party_id_prefix).await?;
        }
        Commands::Serve { ref host, port } => {
            dec_party_onboarding::server::start_server(host, port, config).await?;
        }
        Commands::Onboarding => {
            workflow::start_node(config, workflow::WorkflowType::Onboarding, None).await?;
        }
        Commands::Contracts => {
            workflow::start_node(config, workflow::WorkflowType::Contracts, None).await?;
        }
        Commands::Kick {
            decentralized_party_id,
            participant_id,
            namespace_fingerprint,
        } => {
            let kick_config = workflow::KickConfig::new(
                decentralized_party_id,
                participant_id,
                namespace_fingerprint,
            );
            workflow::start_node(config, workflow::WorkflowType::Kick, Some(kick_config)).await?;
        }
    }

    tracing::info!("Command completed successfully");
    Ok(())
}
