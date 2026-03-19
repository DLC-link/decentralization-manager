use actix_web::{HttpResponse, Responder, get, post, web};

use crate::{
    config::{NetworkConfig, NodeConfig, Peer},
    server::{
        AppState,
        types::{ErrorResponse, SuccessResponse},
    },
};

/// Get the network configuration
#[utoipa::path(
    tag = "Configuration",
    responses(
        (status = 200, description = "Network configuration", body = NetworkConfig),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/network-config")]
pub async fn get_network_config(data: web::Data<AppState>) -> impl Responder {
    match data.config.load_network_config().await {
        Ok(network_config) => HttpResponse::Ok().json(network_config),
        Err(e) => {
            tracing::error!("Failed to load network config: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to load network config: {e}"),
            })
        }
    }
}

/// Save the network configuration (peers list)
#[utoipa::path(
    tag = "Configuration",
    request_body = Vec<Peer>,
    responses(
        (status = 200, description = "Network config saved", body = SuccessResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
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
            HttpResponse::Ok().json(SuccessResponse { success: true })
        }
        Err(e) => {
            tracing::error!("Failed to save network config: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to save network config: {e}"),
            })
        }
    }
}

/// Get the node configuration
#[utoipa::path(
    tag = "Configuration",
    responses(
        (status = 200, description = "Node configuration", body = NodeConfig)
    )
)]
#[get("/node-config")]
pub async fn get_node_config(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(&data.config)
}
