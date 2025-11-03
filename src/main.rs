mod cli;

use std::path::PathBuf;

use cli::{Cli, Commands, Parser};

use grpc_test::{error::Result, steps};

use cli::{Cli, Commands, Parser};

use grpc_test::{dirs::WorkflowDirs, error::Result, steps};

#[tokio::main]
async fn main() -> Result {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_ids(false)
        .with_level(true)
        .init();

    let args = Cli::parse();

    // Load configuration (required)
    let config = if let Some(config_path) = &args.config {
        tracing::info!("Loading configuration from: {}", config_path.display());
        grpc_test::config::Config::from_file(config_path).await?
    } else {
        anyhow::bail!("Configuration file is required. Use -c <config-file>");
    };

    // Default paths
    let dars_dir = PathBuf::from("./dars");
    let keys_dir = PathBuf::from("./keys");

    // Create keys directory if it doesn't exist
    if !keys_dir.exists() {
        tokio::fs::create_dir_all(&keys_dir).await?;
    }

    // Execute the requested command
    match args.command {
        Commands::All => steps::run_all_steps(&config).await?,
        Commands::UploadDars => steps::upload_dars(&config, &dars_dir).await?,
        Commands::GenerateKeys => steps::generate_keys(&config, &keys_dir).await?,
        Commands::CreateProposals => steps::create_proposals().await?,
        Commands::SignDnsProposals => steps::sign_dns_proposals().await?,
        Commands::SubmitDnsProposals => steps::submit_dns_proposals().await?,
        Commands::SignP2pPtkProposals => steps::sign_p2p_ptk_proposals().await?,
        Commands::SubmitFinalProposals => steps::submit_final_proposals().await?,
        Commands::PrepareSubmissions => steps::prepare_submissions().await?,
        Commands::SignSubmissions => steps::sign_submissions().await?,
        Commands::ExecuteSubmissions => steps::execute_submissions().await?,
    }

    tracing::info!("Command completed successfully");
    Ok(())
}
