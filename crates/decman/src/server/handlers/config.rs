use actix_web::{
    HttpRequest, HttpResponse, Responder, get,
    http::header::{CacheControl, CacheDirective},
    post, web,
};
use prometheus::{Encoder, TextEncoder};
use serde::Serialize;

use sqlx::SqlitePool;

use crate::{
    config::{NetworkConfig, NodeConfig, Peer},
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
    server::{
        AppState,
        middleware::require_admin,
        types::{ErrorResponse, LivenessResponse, SuccessResponse},
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
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/network-config")]
pub async fn save_network_config(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<Vec<Peer>>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
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
///
/// `config` is owned (not borrowed) so the type can derive `ts_rs::TS` for the
/// frontend type generator — `TS` needs an owned, `'static` type. The handler
/// clones the node config once per request, which is cheap relative to the I/O.
#[derive(Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct NodeConfigResponse {
    #[serde(flatten)]
    config: NodeConfig,
    test_mode: bool,
    /// Cargo package semver. This is the *compatibility* version peers gate on
    /// (`MIN_PEER_VERSION`), not necessarily the release identity — see
    /// `build_version`.
    version: &'static str,
    /// Display build identity: the git tag on release images, the short commit
    /// SHA on per-commit images, or `<semver>-dev` outside CI. This is what the
    /// Config tab's Version column and the header build-info easter egg show.
    build_version: &'static str,
    /// When this image was built (RFC 3339), if CI stamped it; `None` outside CI.
    #[serde(skip_serializing_if = "Option::is_none")]
    build_time: Option<&'static str>,
}

/// Get the node configuration
#[utoipa::path(
    tag = "Configuration",
    responses(
        (status = 200, description = "Node configuration", body = NodeConfigResponse)
    )
)]
#[get("/node-config")]
pub async fn get_node_config(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(NodeConfigResponse {
        config: data.config.clone(),
        test_mode: data.test_mode,
        version: crate::build_info::SEMVER,
        build_version: crate::build_info::build_version(),
        build_time: crate::build_info::build_time(),
    })
}

/// Liveness probe. Returns `200 {"status":"ok"}` and does no I/O, so the
/// frontend can ping it to measure its own round-trip latency to this node
/// (filling the "you" row of the peers table, where peer latency comes from
/// Noise health probes). Public — no auth — so the timing reflects transport
/// plus handler overhead only, and so it doubles as a container liveness probe.
#[utoipa::path(
    tag = "Configuration",
    responses(
        (status = 200, description = "Service is alive", body = LivenessResponse)
    )
)]
#[get("/healthz")]
pub async fn healthz() -> impl Responder {
    // `no-store` so an intermediary cache/proxy can't serve a cached 200 and
    // skew the latency the frontend measures (and a liveness probe shouldn't
    // be cacheable anyway). The frontend also sends `no-store`; this is the
    // server-side half.
    HttpResponse::Ok()
        .insert_header(CacheControl(vec![CacheDirective::NoStore]))
        .json(LivenessResponse {
            status: "ok".to_string(),
        })
}

/// Prometheus exposition for the collector to scrape, served on
/// `NodeConfig::metrics_port` rather than the API port, whose ingress forwards
/// every path to every tailnet user.
pub async fn metrics() -> impl Responder {
    let mut buffer = Vec::new();
    match TextEncoder::new().encode(&prometheus::gather(), &mut buffer) {
        Ok(()) => HttpResponse::Ok()
            .content_type("text/plain; version=0.0.4; charset=utf-8")
            .body(buffer),
        Err(e) => {
            tracing::warn!(error = %e, "encoding metrics failed");
            HttpResponse::InternalServerError().finish()
        }
    }
}

async fn save_peers_to_db(db: &SqlitePool, peers: &[Peer]) -> Result {
    let mut tx = db.begin_transaction().await?;
    tx.delete_all_peers().await?;
    for peer in peers {
        tx.insert_peer(peer).await?;
    }
    Commitable::commit(tx).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{App, http::StatusCode};
    use utoipa::PartialSchema;

    // The `/node-config` OpenAPI response is documented as `NodeConfigResponse`,
    // not the flattened `NodeConfig`. Guard that its schema builds (flatten can
    // fail at spec-assembly, not compile time) and actually documents the build
    // identity fields, so the generated Swagger stays honest.
    #[test]
    fn node_config_response_schema_documents_build_fields() {
        let schema = NodeConfigResponse::schema();
        let json = serde_json::to_string(&schema).expect("schema should serialize");
        for field in ["version", "build_version", "build_time"] {
            assert!(
                json.contains(field),
                "OpenAPI schema missing `{field}`: {json}"
            );
        }
    }

    // The collector scrapes this endpoint, so it must answer 200 with the
    // Prometheus text format and carry the reward automation's families. A handler
    // that compiles but serves an empty or JSON body would leave every alert
    // evaluating nothing.
    #[actix_web::test]
    async fn metrics_endpoint_serves_the_prometheus_text_format() {
        use actix_web::test;

        crate::server::reward_automation::register_metrics();
        let app = test::init_service(App::new().route("/metrics", web::get().to(metrics))).await;

        let response =
            test::call_service(&app, test::TestRequest::get().uri("/metrics").to_request()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            content_type.starts_with("text/plain"),
            "content type was {content_type}"
        );

        let body = String::from_utf8(test::read_body(response).await.to_vec())
            .unwrap_or_else(|e| panic!("body is not UTF-8: {e}"));
        assert!(
            body.contains("decman_reward_heartbeat_total"),
            "body did not describe the heartbeat: {body}"
        );
    }
}
