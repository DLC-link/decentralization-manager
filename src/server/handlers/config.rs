use actix_web::{HttpResponse, Responder, get, post, web};

use crate::{
    config::{NetworkConfig, Peer},
    server::AppState,
};

/// Get the network configuration
#[get("/network-config")]
pub async fn get_network_config(data: web::Data<AppState>) -> impl Responder {
    match data.config.load_network_config().await {
        Ok(network_config) => HttpResponse::Ok().json(network_config),
        Err(e) => {
            tracing::error!("Failed to load network config: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to load network config: {e}")
            }))
        }
    }
}

/// Save the network configuration (peers list)
#[post("/network-config")]
pub async fn save_network_config(
    data: web::Data<AppState>,
    body: web::Json<Vec<Peer>>,
) -> impl Responder {
    let network_config = NetworkConfig {
        peers: body.into_inner(),
    };

    match data.config.save_network_config(&network_config).await {
        Ok(()) => {
            tracing::info!(
                "Saved network config with {} peers",
                network_config.peers.len()
            );
            HttpResponse::Ok().json(serde_json::json!({ "success": true }))
        }
        Err(e) => {
            tracing::error!("Failed to save network config: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to save network config: {e}")
            }))
        }
    }
}

/// Get the node configuration
#[get("/node-config")]
pub async fn get_node_config(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(&data.config)
}
