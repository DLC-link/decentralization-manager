mod cli;

use tracing_subscriber::{
    filter::{EnvFilter, LevelFilter},
    prelude::*,
};

use cli::{Cli, Commands, Parser};

use grpc_test::{dirs::WorkflowDirs, error::Result, steps};

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

    // Load configuration (required)
    let config = if let Some(config_path) = &args.config {
        tracing::info!("Loading configuration from: {}", config_path.display());
        grpc_test::config::Config::from_file(config_path).await?
    } else {
        anyhow::bail!("Configuration file is required. Use -c <config-file>");
    };

    // Initialize directory paths
    let dirs = WorkflowDirs::new();
    dirs.create_required_dirs().await?;

    // Execute the requested command
    match args.command {
        Commands::All => steps::run_all_steps(&config, &dirs).await?,
        Commands::UploadDars => steps::upload_dars(&config, &dirs).await?,
        Commands::GenerateKeys => steps::generate_keys(&config, &dirs).await?,
        Commands::CreateProposals => steps::create_proposals(&config, &dirs).await?,
        Commands::SignDnsProposals => steps::sign_dns_proposals(&config, &dirs).await?,
        Commands::SubmitDnsProposals => steps::submit_dns_proposals(&config, &dirs).await?,
        Commands::SignP2pPtkProposals => steps::sign_p2p_ptk_proposals(&config, &dirs).await?,
        Commands::SubmitFinalProposals => steps::submit_final_proposals(&config, &dirs).await?,
        Commands::PrepareSubmissions => steps::prepare_submissions(&config, &dirs).await?,
        Commands::SignSubmissions => steps::sign_submissions(&config, &dirs).await?,
        Commands::ExecuteSubmissions => steps::execute_submissions(&config, &dirs).await?,
    }

    tracing::info!("Command completed successfully");
    Ok(())
}
