mod cli;

use std::path::PathBuf;

use tracing_subscriber::{
    filter::{EnvFilter, LevelFilter},
    prelude::*,
};

use cli::{Cli, Commands, Parser};

use grpc_test::{error::Result, steps};

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

    // Default paths
    let dars_dir = PathBuf::from("./dars");
    let out_dir = PathBuf::from("./out");
    let keys_dir = out_dir.join("keys");
    let ids_dir = out_dir.join("ids");
    let step_2_dir = out_dir.join("step_2");
    let step_2a_dir = out_dir.join("step_2a");
    let step_2a_signed_dir = step_2a_dir.join("signed-proposals");
    let step_3_dir = out_dir.join("step_3");
    let step_3a_dir = out_dir.join("step_3a");
    let step_3a_signed_dir = step_3a_dir.join("signed-proposals");

    // Create directories if they don't exist
    if !out_dir.exists() {
        tokio::fs::create_dir_all(&out_dir).await?;
    }
    if !keys_dir.exists() {
        tokio::fs::create_dir_all(&keys_dir).await?;
    }
    if !ids_dir.exists() {
        tokio::fs::create_dir_all(&ids_dir).await?;
    }

    // Execute the requested command
    match args.command {
        Commands::All => {
            steps::run_all_steps(&config, &dars_dir, &keys_dir, &ids_dir, &out_dir).await?
        }
        Commands::UploadDars => steps::upload_dars(&config, &dars_dir).await?,
        Commands::GenerateKeys => steps::generate_keys(&config, &keys_dir, &ids_dir).await?,
        Commands::CreateProposals => {
            steps::create_proposals(&config, &keys_dir, &ids_dir, &out_dir).await?
        }
        Commands::SignDnsProposals => {
            steps::sign_dns_proposals(&config, &step_2_dir, &step_2a_signed_dir, &ids_dir).await?
        }
        Commands::SubmitDnsProposals => {
            steps::submit_dns_proposals(&config, &step_2_dir, &step_2a_dir).await?
        }
        Commands::SignP2pPtkProposals => {
            steps::sign_p2p_ptk_proposals(&config, &step_3_dir, &step_3a_signed_dir, &ids_dir)
                .await?
        }
        Commands::SubmitFinalProposals => {
            steps::submit_final_proposals(&config, &step_3_dir, &step_3a_dir).await?
        }
        Commands::PrepareSubmissions => steps::prepare_submissions(&config, &out_dir).await?,
        Commands::SignSubmissions => steps::sign_submissions().await?,
        Commands::ExecuteSubmissions => steps::execute_submissions().await?,
    }

    tracing::info!("Command completed successfully");
    Ok(())
}
