mod assets;
mod handlers;
mod queries;
mod types;

use std::sync::Arc;

use actix_cors::Cors;
use actix_web::{App, HttpServer, web};
use tokio::net::TcpListener;

use crate::{config::NodeConfig, error::Result};

pub use types::*;

/// Application state shared across all handlers
pub struct AppState {
    pub config: NodeConfig,
}

/// Start the HTTP server and a simple TCP ping server on the Noise port
pub async fn start_server(host: &str, port: u16, config: NodeConfig) -> Result {
    let app_state = web::Data::new(AppState {
        config: config.clone(),
    });
    let kick_state = web::Data::new(Arc::new(handlers::KickWorkflowState::new()));

    // Load network config to get our Noise port
    let network_config = config.load_network_config().await?;
    let node_id = &config.node.node_id;

    // Start a simple TCP listener on the Noise port for status checks
    if let Some(participant) = network_config.get_participant(node_id) {
        let listen_addr = format!("{}:{}", config.node.listen_address, participant.port);
        tokio::spawn(async move {
            match TcpListener::bind(&listen_addr).await {
                Ok(listener) => {
                    tracing::info!("Noise port listener started on {listen_addr}");
                    loop {
                        // Accept connections and immediately close them (just for ping)
                        if let Ok((socket, _)) = listener.accept().await {
                            drop(socket);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to bind Noise port listener on {listen_addr}: {e}");
                }
            }
        });
    }

    tracing::info!("Starting HTTP server on {host}:{port}");
    tracing::info!("Frontend available at http://{host}:{port}/");

    HttpServer::new(move || {
        let cors = Cors::permissive();

        App::new()
            .wrap(cors)
            .app_data(app_state.clone())
            .app_data(kick_state.clone())
            .service(handlers::get_network_config)
            .service(handlers::get_node_config)
            .service(handlers::get_decentralized_parties)
            .service(handlers::get_participants_status)
            .service(handlers::start_kick)
            .service(handlers::get_kick_status)
            .service(handlers::get_key_status)
            .service(handlers::generate_keys)
            .service(assets::serve_frontend)
    })
    .bind((host, port))?
    .run()
    .await?;

    Ok(())
}
