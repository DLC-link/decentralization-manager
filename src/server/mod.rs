mod assets;
mod handlers;
mod queries;
mod types;

use std::{collections::HashMap, sync::Arc, time::Duration};

use actix_cors::Cors;
use actix_web::{App, HttpServer, web};
use tokio::sync::RwLock;

use crate::{config::NodeConfig, error::Result};

pub use types::*;

/// Application state shared across all handlers
pub struct AppState {
    pub config: NodeConfig,
    pub peer_status: Arc<RwLock<HashMap<String, bool>>>,
    pub noise_listener_control: Arc<RwLock<ListenerControl>>,
}

/// Control mechanism for the Noise port listener
pub struct ListenerControl {
    pub should_pause: bool,
    pub notify: tokio::sync::Notify,
}

/// Start the HTTP server and a heartbeat system for peer status tracking
pub async fn start_server(host: &str, port: u16, config: NodeConfig) -> Result {
    let peer_status = Arc::new(RwLock::new(HashMap::new()));
    let listener_control = Arc::new(RwLock::new(ListenerControl {
        should_pause: false,
        notify: tokio::sync::Notify::new(),
    }));

    let app_state = web::Data::new(AppState {
        config: config.clone(),
        peer_status: peer_status.clone(),
        noise_listener_control: listener_control.clone(),
    });
    let kick_state = web::Data::new(Arc::new(handlers::KickWorkflowState::new()));
    let onboarding_state = web::Data::new(Arc::new(handlers::OnboardingWorkflowState::new()));

    // Start heartbeat background task
    let heartbeat_config = config.clone();
    let heartbeat_status = peer_status.clone();
    let heartbeat_control = listener_control.clone();
    tokio::spawn(async move {
        run_heartbeat(heartbeat_config, heartbeat_status, heartbeat_control).await;
    });

    tracing::info!("Starting HTTP server on {host}:{port}");
    tracing::info!("Frontend available at http://{host}:{port}/");

    HttpServer::new(move || {
        let cors = Cors::permissive();

        App::new()
            .wrap(cors)
            .app_data(app_state.clone())
            .app_data(kick_state.clone())
            .app_data(onboarding_state.clone())
            .service(handlers::get_network_config)
            .service(handlers::get_node_config)
            .service(handlers::get_decentralized_parties)
            .service(handlers::get_participants_status)
            .service(handlers::start_kick)
            .service(handlers::get_kick_status)
            .service(handlers::start_onboarding)
            .service(handlers::get_onboarding_status)
            .service(handlers::get_key_status)
            .service(handlers::generate_keys)
            .service(assets::serve_frontend)
    })
    .bind((host, port))?
    .run()
    .await?;

    Ok(())
}

/// Background task that pings all peers every 5 seconds via Noise protocol
async fn run_heartbeat(
    config: NodeConfig,
    peer_status: Arc<RwLock<HashMap<String, bool>>>,
    listener_control: Arc<RwLock<ListenerControl>>,
) {
    use tokio::net::TcpListener;

    let network_config = match config.load_network_config().await {
        Ok(nc) => nc,
        Err(e) => {
            tracing::error!("Failed to load network config for heartbeat listener: {e}");
            return;
        }
    };

    let listen_addr = match network_config.get_participant(&config.node.node_id) {
        Some(p) => format!("{}:{}", config.node.listen_address, p.port),
        None => {
            tracing::error!("Current node not found in network config");
            return;
        }
    };

    // Listener management loop
    let listener_control_spawn = listener_control.clone();
    tokio::spawn(async move {
        loop {
            // Wait for permission to bind
            let should_pause = {
                let control = listener_control_spawn.read().await;
                control.should_pause
            };

            if should_pause {
                tracing::info!("Noise listener paused for workflow");
                // Wait for notification to resume
                listener_control_spawn.read().await.notify.notified().await;
                tracing::info!("Resuming Noise listener");
                continue;
            }

            // Try to bind listener
            match TcpListener::bind(&listen_addr).await {
                Ok(listener) => {
                    tracing::info!("Heartbeat listener started on {listen_addr}");

                    loop {
                        tokio::select! {
                            result = listener.accept() => {
                                if let Ok((socket, _)) = result {
                                    drop(socket); // Just accept and close for ping
                                }
                            }
                            _ = async {
                                loop {
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                    let control = listener_control_spawn.read().await;
                                    if control.should_pause {
                                        break;
                                    }
                                }
                            } => {
                                tracing::info!("Stopping listener for workflow");
                                drop(listener);
                                break; // Exit inner loop to rebind later
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to bind heartbeat listener on {listen_addr}: {e}, retrying in 5s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });

    // Ping peers every 5 seconds
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        // Load network config
        let network_config = match config.load_network_config().await {
            Ok(nc) => nc,
            Err(e) => {
                tracing::debug!("Failed to load network config for heartbeat: {e}");
                continue;
            }
        };

        // Ping all peers in parallel
        let current_node_id = &config.node.node_id;
        let futures: Vec<_> = network_config
            .participants
            .iter()
            .filter(|p| p.id != *current_node_id)
            .map(|participant| {
                let id = participant.id.clone();
                let address = participant.address.clone();
                let port = participant.port;

                async move {
                    // Try to connect to peer's Noise port
                    let addr = format!("{address}:{port}");
                    let active = tokio::time::timeout(
                        Duration::from_secs(2),
                        tokio::net::TcpStream::connect(&addr),
                    )
                    .await
                    .map(|r| r.is_ok())
                    .unwrap_or(false);

                    (id, active)
                }
            })
            .collect();

        let results = futures::future::join_all(futures).await;

        // Update peer status cache
        let mut status_map = peer_status.write().await;
        for (id, active) in results {
            status_map.insert(id, active);
        }
    }
}
