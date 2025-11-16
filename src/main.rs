mod cli;

use tracing_subscriber::{
    filter::{EnvFilter, LevelFilter},
    prelude::*,
};

use cli::{Cli, Commands, Parser};

use grpc_test::{dirs::WorkflowDirs, error::Result, network_config::NodeConfig, steps};

use cli::{Cli, Commands, Parser};

use grpc_test::{dirs::WorkflowDirs, error::Result, steps};

#[tokio::main]
async fn main() -> Result {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(filter))
        .init();

    let args = Cli::parse();

    // Handle keygen command early (doesn't require config)
    if let Commands::Keygen { ref output } = args.command {
        grpc_test::noise::generate_keypair(output).await?;
        return Ok(());
    }

    // Load configuration (required for all other commands)
    let node_config = if let Some(config_path) = &args.config {
        tracing::info!("Loading configuration from: {}", config_path.display());
        NodeConfig::from_file(config_path).await?
    } else {
        anyhow::bail!("Configuration file is required. Use -c <config-file>");
    };

    // Initialize directory paths
    let dirs = WorkflowDirs::new();
    dirs.create_required_dirs().await?;

    // Execute the requested command
    match args.command {
        Commands::Keygen { .. } => unreachable!("Keygen handled earlier"),
        Commands::Start => unimplemented!("Start command not yet implemented"),
        Commands::All => steps::run_all_steps(&node_config, &dirs).await?,
        Commands::UploadDars => steps::upload_dars(&node_config, &dirs).await?,
        Commands::GenerateKeys => steps::generate_keys(&node_config, &dirs).await?,
        Commands::CreateProposals => steps::create_proposals(&node_config, &dirs).await?,
        Commands::SignDnsProposals => steps::sign_dns_proposals(&node_config, &dirs).await?,
        Commands::SubmitDnsProposals => steps::submit_dns_proposals(&node_config, &dirs).await?,
        Commands::SignP2pPtkProposals => steps::sign_p2p_ptk_proposals(&node_config, &dirs).await?,
        Commands::SubmitFinalProposals => {
            steps::submit_final_proposals(&node_config, &dirs).await?
        }
        Commands::PrepareSubmissions => steps::prepare_submissions(&node_config, &dirs).await?,
        Commands::SignSubmissions => steps::sign_submissions(&node_config, &dirs).await?,
        Commands::ExecuteSubmissions => steps::execute_submissions(&node_config, &dirs).await?,
    }

    tracing::info!("Command completed successfully");
    Ok(())
}
