mod handlers;
mod queries;
mod types;

use actix_web::{App, HttpServer, web};

use crate::{config::NodeConfig, error::Result};

pub use types::*;

/// Application state shared across all handlers
pub struct AppState {
    pub config: NodeConfig,
}

/// Start the HTTP server
pub async fn start_server(host: &str, port: u16, config: NodeConfig) -> Result {
    let app_state = web::Data::new(AppState { config });

    tracing::info!("Starting HTTP server on {host}:{port}");

    HttpServer::new(move || {
        App::new()
            .app_data(app_state.clone())
            .service(handlers::get_network_config)
            .service(handlers::get_node_config)
            .service(handlers::get_decentralized_parties)
    })
    .bind((host, port))?
    .run()
    .await?;

    Ok(())
}
