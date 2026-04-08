mod cli;

use std::path::PathBuf;

use dec_party_manager::{config::{KeycloakConfig, NodeConfig}, db, error::Result, utils};
use tracing_subscriber::{filter::EnvFilter, prelude::*};

use cli::{Cli, Commands, Parser};

/// Extract the --dir / -d value from raw args before clap parses,
/// so we can load the .env file from that directory first.
fn find_dir_arg() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    for i in 0..args.len() {
        if (args[i] == "-d" || args[i] == "--dir") && i + 1 < args.len() {
            return PathBuf::from(&args[i + 1]);
        }
    }
    PathBuf::from(".")
}

#[tokio::main]
async fn main() -> Result {
    // Load .env from the root directory before clap parses,
    // so DECPM_* env vars are available for clap's env feature
    let dir = find_dir_arg();
    let env_path = dir.join(".env");
    if env_path.exists() {
        dotenvy::from_path(&env_path).ok();
    }

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

    // Apply CLI/env var overrides
    match &args.command {
        Commands::Serve {
            participant_id,
            listen_address,
            noise_port,
            public_address,
            canton_admin_host,
            canton_admin_port,
            canton_ledger_host,
            canton_ledger_port,
            canton_synchronizer,
            canton_network,
            keycloak_url,
            keycloak_realm,
            keycloak_client_id,
            timeout_handshake,
            timeout_message,
            timeout_retry_attempts,
            timeout_retry_delay,
            ..
        } => {
            if let Some(id) = participant_id {
                config.node.participant_id = Some(id.clone());
            }
            if let Some(addr) = listen_address {
                config.node.listen_address = addr.clone();
            }
            if let Some(p) = noise_port {
                config.node.port = *p;
            }
            if let Some(addr) = public_address {
                config.node.public_address = Some(addr.clone());
            }
            if let Some(host) = canton_admin_host {
                config.canton.admin_api_host = host.clone();
            }
            if let Some(p) = canton_admin_port {
                config.canton.admin_api_port = *p;
            }
            if let Some(host) = canton_ledger_host {
                config.canton.ledger_api_host = host.clone();
            }
            if let Some(p) = canton_ledger_port {
                config.canton.ledger_api_port = *p;
            }
            if let Some(sync) = canton_synchronizer {
                config.canton.synchronizer = sync.clone();
            }
            if let Some(net) = canton_network {
                config.canton.network = net.clone();
            }
            if keycloak_url.is_some() || keycloak_realm.is_some() || keycloak_client_id.is_some() {
                let kc = config.keycloak.get_or_insert(KeycloakConfig {
                    url: String::new(),
                    realm: String::new(),
                    client_id: String::new(),
                    client_secret: None,
                    username: None,
                    password: None,
                });
                if let Some(url) = keycloak_url {
                    kc.url = url.clone();
                }
                if let Some(realm) = keycloak_realm {
                    kc.realm = realm.clone();
                }
                if let Some(client_id) = keycloak_client_id {
                    kc.client_id = client_id.clone();
                }
            }
            if let Some(v) = timeout_handshake {
                config.timeouts.handshake_timeout_secs = *v;
            }
            if let Some(v) = timeout_message {
                config.timeouts.message_timeout_secs = *v;
            }
            if let Some(v) = timeout_retry_attempts {
                config.timeouts.connection_retry_attempts = *v;
            }
            if let Some(v) = timeout_retry_delay {
                config.timeouts.connection_retry_delay_secs = *v;
            }
        }
    }

    // Resolve participant_id from Canton if not configured
    utils::resolve_participant_id(&mut config).await?;

    tracing::info!("Running as participant: {}", config.participant_id());

    // Initialize database
    let db_path = match &args.command {
        Commands::Serve { db, .. } => db.clone().unwrap_or_else(|| config.db_path()),
    };
    tracing::info!("Connecting to database at {}", db_path.display());
    let pool = db::connect(&db_path).await?;

    tracing::info!("Running database migrations");
    db::MIGRATOR.run(&pool).await?;

    db::seed::seed_from_config(&pool, &config).await?;

    match args.command {
        Commands::Serve {
            ref host,
            port,
            test,
            ..
        } => {
            dec_party_manager::server::start_server(host, port, config, test, pool).await?;
        }
    }

    tracing::info!("Command completed successfully");
    Ok(())
}
