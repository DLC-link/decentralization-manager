use actix_web::{HttpResponse, Responder, get, post, web};
use serde::Serialize;

use sqlx::SqlitePool;

use crate::{
    config::{NetworkConfig, NodeConfig, Peer},
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
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
    match data.db.get_all_peers().await {
        Ok(peers) => HttpResponse::Ok().json(NetworkConfig::from_peers(peers)),
        Err(e) => {
            tracing::error!("Failed to load peers from database: {e}");
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
    let peers = body.into_inner();

    // Primary write: save to database
    if let Err(e) = save_peers_to_db(&data.db, &peers).await {
        tracing::error!("Failed to save peers to database: {e}");
        return HttpResponse::InternalServerError().json(ErrorResponse {
            error: format!("Failed to save network config: {e}"),
        });
    }

    tracing::info!("Saved network config with {} peers", peers.len());
    HttpResponse::Ok().json(SuccessResponse { success: true })
}

/// Node configuration response (includes runtime flags)
#[derive(Serialize)]
struct NodeConfigResponse<'a> {
    #[serde(flatten)]
    config: &'a NodeConfig,
    test_mode: bool,
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
    HttpResponse::Ok().json(NodeConfigResponse {
        config: &data.config,
        test_mode: data.test_mode,
    })
}

async fn save_peers_to_db(db: &SqlitePool, peers: &[Peer]) -> Result {
    let mut tx = db.begin_transaction().await?;
    tx.delete_all_peers().await?;
    for peer in peers {
        tx.insert_peer(peer).await?;
    }
    Commitable::commit(tx).await
}
