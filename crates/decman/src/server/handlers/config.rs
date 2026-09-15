use actix_web::{
    HttpRequest, HttpResponse, Responder, get,
    http::header::{CacheControl, CacheDirective},
    post, web,
};
use prometheus::{Encoder, TextEncoder};
use serde::Serialize;

use sqlx::SqlitePool;

use common::coordination::{CoordinationDarPhase, CoordinationDarStatus};

use crate::{
    config::{NetworkConfig, NodeConfig, Peer},
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
    onledger::dars,
    server::{
        AppState,
        middleware::require_admin,
        node_health::NodeHealthResponse,
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
        (status = 400, description = "A peer carries no node party", body = ErrorResponse),
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

    let without_party = peers_without_node_party(&peers);
    if !without_party.is_empty() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "These peers carry no node party, so no workflow can invite them: {}",
                without_party.join(", ")
            ),
        });
    }

    // Primary write: save to database
    if let Err(e) = save_peers_to_db(&data.db, &peers).await {
        tracing::error!("Failed to save peers to database: {e}");
        return HttpResponse::InternalServerError().json(ErrorResponse {
            error: format!("Failed to save network config: {e}"),
        });
    }

    tracing::info!("Saved network config with {} peers", peers.len());

    // The registry entry names the peers as observers (design D3), so a
    // changed peer list means a changed entry. Best effort and off the
    // request path: the observer republishes on its next tick anyway.
    let onledger = data.onledger.clone();
    tokio::spawn(async move {
        if onledger.identity().await.is_none() {
            return;
        }
        if let Err(e) = onledger.publish_registry_entry().await {
            tracing::warn!(error = %format!("{e:#}"), "registry entry not republished after a peers change");
        }
    });
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
    /// Cargo package semver, the version this node publishes in its registry
    /// entry; not necessarily the release identity — see `build_version`.
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

/// Per-hop health of this node and the participant it drives, plus the state
/// of the startup coordination-DAR upload (design D8).
///
/// Answers from a shared snapshot that is at most `SNAPSHOT_TTL` old, so the
/// Config tab's poll costs one pair of gRPC probes per TTL however many
/// browsers are watching. Never fails: an endpoint that could not be reached is
/// reported as such in the body rather than as a 5xx, because "the Admin API is
/// down" is the answer, not an error.
#[utoipa::path(
    tag = "Configuration",
    responses(
        (status = 200, description = "Node and participant health", body = NodeHealthResponse)
    )
)]
#[get("/node-health")]
pub async fn get_node_health(data: web::Data<AppState>) -> impl Responder {
    let mut health = data.health_cache.get(&data.config).await;
    health.coordination_dar = Some(coordination_dar_status(&dars::startup_upload_state().await));
    HttpResponse::Ok()
        .insert_header(CacheControl(vec![CacheDirective::NoStore]))
        .json(health)
}

/// The wire view of the startup task's state (pure).
fn coordination_dar_status(state: &dars::StartupUploadState) -> CoordinationDarStatus {
    let phase = match state.phase {
        dars::StartupUploadPhase::Pending => CoordinationDarPhase::Pending,
        dars::StartupUploadPhase::Disabled => CoordinationDarPhase::Disabled,
        dars::StartupUploadPhase::Uploading => CoordinationDarPhase::Uploading,
        dars::StartupUploadPhase::Ready => CoordinationDarPhase::Ready,
    };
    CoordinationDarStatus {
        phase,
        filename: state.filename.clone(),
        main_package_id: state.main_package_id.clone(),
        uploaded: state.uploaded,
        vetted: state.vetted,
        attempts: state.attempts,
        last_error: state.last_error.clone(),
        updated_at: state.updated_at,
    }
}

/// Liveness probe. Returns `200 {"status":"ok"}` and does no I/O, so the
/// frontend can ping it to measure its own round-trip latency to this node
/// (filling the "you" row of the peers table; peers show their heartbeat age
/// instead). Public — no auth — so the timing reflects transport plus
/// handler overhead only, and so it doubles as a container liveness probe.
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

/// Participant IDs of peers that carry no node party.
///
/// Such a row stores fine and is then skipped by every coordination path: the
/// node cannot name the peer as an observer, so it never appears in a run. The
/// write is the last place to catch it.
fn peers_without_node_party(peers: &[Peer]) -> Vec<String> {
    peers
        .iter()
        .filter(|p| p.party.as_ref().is_none_or(|q| q.to_string().is_empty()))
        .map(|p| p.participant_id.to_string())
        .collect()
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
    use common::canton_id::CantonId;
    use utoipa::PartialSchema;

    fn peer(prefix: &str, party: Option<&str>) -> Result<Peer> {
        Ok(Peer {
            participant_id: CantonId::parse(&format!("{prefix}::{ns}", ns = "a".repeat(68)))?,
            name: prefix.to_string(),
            party: party
                .map(|p| CantonId::parse(&format!("{p}::{ns}", ns = "b".repeat(68))))
                .transpose()?,
        })
    }

    // A peer with no node party stores fine and is then skipped by every
    // coordination path, so the POST has to refuse it.
    #[test]
    fn peers_without_a_node_party_are_named_by_participant_id() -> Result {
        let peers = [peer("good", Some("node"))?, peer("bare", None)?];

        let bad = peers_without_node_party(&peers);

        assert_eq!(bad.len(), 1, "only the bare peer should be named: {bad:?}");
        assert!(bad.iter().all(|id| !id.starts_with("good::")));
        Ok(())
    }

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

    /// Every phase of the startup task has a wire value, and the fields the
    /// UI shows copy across unchanged.
    #[test]
    fn coordination_dar_status_mirrors_the_task_state() {
        let state = dars::StartupUploadState {
            phase: dars::StartupUploadPhase::Uploading,
            filename: "decman-coordination-v1-0.1.0.dar".into(),
            main_package_id: "abc".into(),
            uploaded: true,
            vetted: false,
            attempts: 3,
            last_error: Some("not connected".into()),
            updated_at: 42,
        };
        let view = coordination_dar_status(&state);
        assert_eq!(view.phase, CoordinationDarPhase::Uploading);
        assert!(view.uploaded);
        assert!(!view.vetted);
        assert_eq!(view.attempts, 3);
        assert_eq!(view.last_error.as_deref(), Some("not connected"));
        assert_eq!(view.updated_at, 42);
        for (phase, expected) in [
            (
                dars::StartupUploadPhase::Pending,
                CoordinationDarPhase::Pending,
            ),
            (
                dars::StartupUploadPhase::Disabled,
                CoordinationDarPhase::Disabled,
            ),
            (dars::StartupUploadPhase::Ready, CoordinationDarPhase::Ready),
        ] {
            let view = coordination_dar_status(&dars::StartupUploadState {
                phase,
                ..state.clone()
            });
            assert_eq!(view.phase, expected);
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
