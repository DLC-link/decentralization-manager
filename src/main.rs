mod cli;

use tracing_subscriber::{filter::EnvFilter, prelude::*};

use cli::{Cli, Commands, Parser};
use dec_party_manager::{config::NodeConfig, db, error::Result, utils};

#[tokio::main]
async fn main() -> Result {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("dec_party_manager=info,tokio_noise=error,hyper_noise=error")
    });

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(filter))
        .init();

    let args = Cli::parse();

    tracing::info!(
        "Loading configuration from: {path}",
        path = args.dir.display()
    );
    let mut config = NodeConfig::from_dir(&args.dir).await?;

    // Resolve participant_id from Canton if not configured
    utils::resolve_participant_id(&mut config).await?;

    tracing::info!("Running as participant: {}", config.participant_id());

    // Initialize database
    let db_path = config.db_path();
    tracing::info!("Connecting to database at {}", db_path.display());
    let pool = db::connect(&db_path).await?;

    tracing::info!("Running database migrations");
    db::MIGRATOR.run(&pool).await?;

    match args.command {
        Commands::Serve {
            ref host,
            port,
            test,
        } => {
            dec_party_manager::server::start_server(host, port, config, test, pool).await?;
        }
    }

    tracing::info!("Command completed successfully");
    Ok(())
}
