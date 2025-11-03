mod cli;

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

    // Load configuration if provided
    if let Some(config_path) = &args.config {
        tracing::info!("Loading configuration from: {}", config_path.display());

        let _config = grpc_test::config::Config::from_file(config_path).await?;
        // TODO: Pass config to step functions
    }

    // Execute the requested command
    match args.command {
        Commands::All => steps::run_all_steps().await?,
        Commands::UploadDars => steps::upload_dars().await?,
        Commands::GenerateKeys => steps::generate_keys().await?,
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
