use std::time::Duration;

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};

use super::auth::{
    AdminTokenSource, extract_user_id_from_jwt, mint_admin_token, mint_error_response,
};
use sqlx::SqlitePool;

use crate::{
    auth::{NodeIdentity, WorkflowAuth},
    canton_id::CantonId,
    config::{NodeConfig, Peer},
    consts::topology_propagation_delay_secs,
    db::schema::SchemaRead,
    error::Result,
    server::{
        AppState,
        middleware::require_admin,
        types::{
            ErrorResponse, NodeIdentityPeer, NodeIdentityRequest, NodeIdentityResponse,
            NodeIdentityStatusResponse,
        },
    },
    workflow::node_identity::{
        NODE_PARTY_PREFIX, allocate_node_party, list_user_rights, namespace_matches, node_party_id,
        right_names, rights_flags, scan_node_parties, verify_hosting, wait_for_submission,
    },
};

/// This node's saved identity with its login started, for startup.
///
/// A login the IdP refuses is logged and left unloaded, as
/// `AuthRegistry::new` does for a party. Insecure mode uses the unsafe token.
pub(crate) async fn load_node_identity(
    db: &SqlitePool,
    auth: Option<&WorkflowAuth>,
) -> Option<NodeIdentity> {
    let record = match db.get_node_identity().await {
        Ok(Some(record)) => record,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!("Failed to load the node identity: {e:#}");
            return None;
        }
    };
    if let Some(WorkflowAuth::Mock(registry)) = auth {
        let token = registry.get_by_str("").await.get_token();
        return Some(NodeIdentity::mock(&record, token));
    }
    match NodeIdentity::connect(&record).await {
        Ok(identity) => {
            tracing::info!(
                node_party = %record.node_party_id,
                user_id = %record.user_id,
                "Node login started"
            );
            Some(identity)
        }
        Err(e) => {
            tracing::warn!(
                node_party = %record.node_party_id,
                user_id = %record.user_id,
                "Node login failed: {e}. The node identity stays unloaded until the login works"
            );
            None
        }
    }
}

/// The Ledger API token and users an allocate call works with.
struct Tokens {
    admin_token: String,
    admin_user_id: String,
    user_id: String,
}

fn error_response(mut builder: actix_web::HttpResponseBuilder, error: String) -> HttpResponse {
    builder.json(ErrorResponse { error })
}

fn canton_failed(e: anyhow::Error) -> HttpResponse {
    tracing::warn!("Node identity Canton call failed: {e:#}");
    error_response(
        HttpResponse::InternalServerError(),
        format!("Canton call failed: {e:#}"),
    )
}

fn participant_unresolved() -> HttpResponse {
    error_response(
        HttpResponse::InternalServerError(),
        "This node's participant id is not resolved".to_string(),
    )
}

/// The admin token and users for an allocate call. Insecure mode uses its
/// unsafe token for both. Otherwise the operator's admin client mints a token
/// from the first configured party's IdP.
#[allow(clippy::result_large_err)]
async fn resolve_tokens(
    data: &AppState,
    body: NodeIdentityRequest,
) -> std::result::Result<Tokens, HttpResponse> {
    if let Some(WorkflowAuth::Mock(registry)) = &*data.auth.read().await {
        let mock = registry.get_by_str("").await;
        return Ok(Tokens {
            admin_token: mock.get_token(),
            admin_user_id: mock.user_id().to_string(),
            user_id: body.user_id.unwrap_or_else(|| mock.user_id().to_string()),
        });
    }

    let client_id = body.admin_client_id.as_deref().unwrap_or_default().trim();
    let client_secret = body
        .admin_client_secret
        .as_deref()
        .unwrap_or_default()
        .trim();
    if client_id.is_empty() || client_secret.is_empty() {
        return Err(error_response(
            HttpResponse::BadRequest(),
            "admin_client_id and admin_client_secret are required".to_string(),
        ));
    }
    let (source, default_user_id) = {
        let rows = data.party_credentials.read().await;
        let Some(row) = rows.first() else {
            return Err(error_response(
                HttpResponse::BadRequest(),
                "Configure a party first. Its IdP mints the admin token".to_string(),
            ));
        };
        let source = AdminTokenSource::from_party(row)
            .map_err(|e| error_response(HttpResponse::BadRequest(), e))?;
        (source, row.user_id.clone())
    };

    let endpoint = source.token_endpoint();
    let admin_token = mint_admin_token(
        &data.http_client,
        &endpoint,
        &source,
        client_id,
        client_secret,
    )
    .await
    .map_err(|e| {
        let idp = source.idp();
        tracing::warn!(
            kind = e.kind(),
            idp,
            status = e.status(),
            "Failed to mint admin token for node identity: {e}"
        );
        mint_error_response(e, idp)
    })?;
    Ok(Tokens {
        admin_user_id: extract_user_id_from_jwt(&admin_token)
            .unwrap_or_else(|| client_id.to_string()),
        admin_token,
        user_id: body.user_id.unwrap_or(default_user_id),
    })
}

/// Allocate the node party unless this participant already hosts it, then
/// report what the ledger shows. Never grants rights.
#[allow(clippy::result_large_err)]
async fn allocate(
    config: &NodeConfig,
    tokens: &Tokens,
    participant_id: &CantonId,
) -> std::result::Result<NodeIdentityResponse, HttpResponse> {
    let node_party = node_party_id(participant_id);
    let before = verify_hosting(config, &node_party, participant_id)
        .await
        .map_err(canton_failed)?;
    let allocated = if before.has_submission() {
        false
    } else {
        match allocate_node_party(
            config,
            &tokens.admin_token,
            &node_party.prefix,
            &tokens.user_id,
        )
        .await
        .map_err(canton_failed)?
        {
            None => false,
            Some(party) if party == node_party.to_string() => true,
            Some(party) => {
                tracing::error!(
                    expected = %node_party,
                    returned = %party,
                    "Canton allocated a node party id other than the derived one"
                );
                return Err(error_response(
                    HttpResponse::BadGateway(),
                    format!("Canton allocated {party}, expected {node_party}"),
                ));
            }
        }
    };

    let wait = Duration::from_secs(topology_propagation_delay_secs());
    let hosting = wait_for_submission(config, &node_party, participant_id, wait)
        .await
        .map_err(canton_failed)?;
    let user_rights = list_user_rights(config, &tokens.admin_token, &tokens.user_id)
        .await
        .map_err(canton_failed)?;
    let (act_as, read_as) = rights_flags(&user_rights, &node_party.to_string());
    let admin_rights = list_user_rights(config, &tokens.admin_token, &tokens.admin_user_id)
        .await
        .map_err(canton_failed)?;

    let response = NodeIdentityResponse {
        namespace_matches: namespace_matches(&node_party, participant_id),
        node_party_id: node_party,
        participant_id: participant_id.clone(),
        allocated,
        hosting_permission: hosting.permission,
        user_id: tokens.user_id.clone(),
        act_as,
        read_as,
        admin_user_id: tokens.admin_user_id.clone(),
        admin_rights: right_names(&admin_rights),
    };
    tracing::info!(?response, "Node identity checked");
    Ok(response)
}

/// Allocate this node's identity party, or confirm this participant already
/// hosts it. The user gets act-as through AllocateParty alone.
#[utoipa::path(
    tag = "Node identity",
    request_body = NodeIdentityRequest,
    responses(
        (status = 200, description = "Node party allocated or already hosted", body = NodeIdentityResponse),
        (status = 400, description = "Admin credentials or a configured party missing", body = ErrorResponse),
        (status = 401, description = "Missing or invalid session token", body = ErrorResponse),
        (status = 403, description = "Caller is not an admin", body = ErrorResponse),
        (status = 422, description = "Admin credentials rejected by the IdP", body = ErrorResponse),
        (status = 500, description = "Participant id unresolved or a Canton call failed", body = ErrorResponse),
        (status = 502, description = "IdP failed, or Canton allocated another party id", body = ErrorResponse),
        (status = 503, description = "IdP rate-limited the admin token mint", body = ErrorResponse)
    )
)]
#[post("/node-identity")]
pub async fn allocate_node_identity(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<NodeIdentityRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let tokens = match resolve_tokens(&data, body.into_inner()).await {
        Ok(tokens) => tokens,
        Err(resp) => return resp,
    };
    let Some(participant_id) = data.config.node.participant_id.clone() else {
        return participant_unresolved();
    };
    match allocate(&data.config, &tokens, &participant_id).await {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(resp) => resp,
    }
}

/// Check one node's party by its exact id in this node's topology.
async fn check_node(
    config: &NodeConfig,
    participant_id: CantonId,
    name: String,
) -> Result<NodeIdentityPeer> {
    let node_party = node_party_id(&participant_id);
    let check = verify_hosting(config, &node_party, &participant_id).await?;
    let namespace_matches = namespace_matches(&node_party, &participant_id);
    Ok(NodeIdentityPeer {
        verified: check.mapping_exists && namespace_matches && check.has_submission(),
        found: check.mapping_exists,
        namespace_matches,
        hosting_permission: check.permission,
        participant_id,
        name,
        node_party_id: node_party,
    })
}

async fn identity_status(
    config: &NodeConfig,
    participant_id: &CantonId,
    peers: Vec<Peer>,
) -> Result<NodeIdentityStatusResponse> {
    let (own, others): (Vec<Peer>, Vec<Peer>) = peers
        .into_iter()
        .partition(|p| &p.participant_id == participant_id);
    let own_name = own.into_iter().next().map(|p| p.name).unwrap_or_default();
    let self_ = check_node(config, participant_id.clone(), own_name).await?;
    let mut checked = Vec::with_capacity(others.len());
    for peer in others {
        checked.push(check_node(config, peer.participant_id, peer.name).await?);
    }
    Ok(NodeIdentityStatusResponse {
        self_,
        peers: checked,
        scan_filter: NODE_PARTY_PREFIX.to_string(),
        scan: scan_node_parties(config).await?,
    })
}

/// Check this node's and every configured peer's identity party in this
/// node's topology, and list every party the node-party prefix matches.
#[utoipa::path(
    tag = "Node identity",
    responses(
        (status = 200, description = "Node identity check", body = NodeIdentityStatusResponse),
        (status = 401, description = "Missing or invalid session token", body = ErrorResponse),
        (status = 403, description = "Caller is not an admin", body = ErrorResponse),
        (status = 500, description = "Participant id unresolved or a read failed", body = ErrorResponse)
    )
)]
#[get("/node-identity")]
pub async fn get_node_identity(http_req: HttpRequest, data: web::Data<AppState>) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let Some(participant_id) = data.config.node.participant_id.clone() else {
        return participant_unresolved();
    };
    let peers = match data.db.get_all_peers().await {
        Ok(peers) => peers,
        Err(e) => {
            tracing::warn!("Failed to load peers for node identity: {e:#}");
            return error_response(
                HttpResponse::InternalServerError(),
                format!("Failed to load peers: {e:#}"),
            );
        }
    };
    match identity_status(&data.config, &participant_id, peers).await {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(e) => canton_failed(e),
    }
}

#[cfg(test)]
mod tests {
    use actix_web::{
        App,
        http::{StatusCode, header::AUTHORIZATION},
        test::{self, TestRequest},
    };
    use std::sync::Arc;

    use anyhow::Context;
    use serde_json::json;
    use tokio::sync::RwLock;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path_regex},
    };

    use super::*;
    use crate::{
        auth::MockAuthRegistry,
        config::{IdpClient, KeycloakConfig, NodeIdentityRecord},
        db::schema::{Commitable, SchemaWrite},
        server::middleware::AuthMiddleware,
    };

    const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

    fn keycloak_login(url: &str) -> IdpClient {
        IdpClient::Keycloak(KeycloakConfig {
            url: url.to_string(),
            realm: "canton".to_string(),
            client_id: "node-client".to_string(),
            client_secret: Some("node-secret".to_string()),
            ..KeycloakConfig::default()
        })
    }

    fn stored_identity(login: Option<IdpClient>) -> anyhow::Result<NodeIdentityRecord> {
        Ok(NodeIdentityRecord {
            node_party_id: CantonId::parse(&format!("decman-node-participant1::{NS}"))?,
            participant_id: CantonId::parse(&format!("participant1::{NS}"))?,
            user_id: "node-login".to_string(),
            login,
            created_at: 100,
            updated_at: 100,
        })
    }

    async fn save(db: &SqlitePool, record: &NodeIdentityRecord) -> anyhow::Result<()> {
        let mut tx = db.begin_transaction().await?;
        tx.upsert_node_identity(record).await?;
        Commitable::commit(tx).await?;
        Ok(())
    }

    /// A Keycloak stand-in that answers every token request with `response`.
    async fn keycloak_answering(response: ResponseTemplate) -> MockServer {
        let idp = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r".*/token$"))
            .respond_with(response)
            .mount(&idp)
            .await;
        idp
    }

    fn token_response(token: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({"access_token": token, "expires_in": 300}))
    }

    /// Without the auth middleware no principal reaches the handlers, so the
    /// admin gate must refuse before any Canton or IdP call.
    #[actix_web::test]
    async fn node_identity_rejects_request_without_principal() -> anyhow::Result<()> {
        let state = AppState::for_test(None).await?;
        let app = test::init_service(
            App::new()
                .app_data(state)
                .service(allocate_node_identity)
                .service(get_node_identity),
        )
        .await;

        let post = TestRequest::post()
            .uri("/node-identity")
            .set_json(json!({}))
            .to_request();
        assert_eq!(
            test::call_service(&app, post).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let get = TestRequest::get().uri("/node-identity").to_request();
        assert_eq!(
            test::call_service(&app, get).await.status(),
            StatusCode::UNAUTHORIZED
        );
        Ok(())
    }

    /// Outside insecure mode the admin token needs the operator's admin client
    /// and a configured party whose IdP mints it.
    #[actix_web::test]
    async fn allocate_needs_admin_credentials_and_a_party_outside_insecure_mode()
    -> anyhow::Result<()> {
        let state = AppState::for_test(None).await?;
        let app = test::init_service(
            App::new()
                .app_data(state)
                .wrap(AuthMiddleware)
                .service(allocate_node_identity),
        )
        .await;

        for body in [
            json!({}),
            json!({"admin_client_id": "validator-admin", "admin_client_secret": "secret"}),
        ] {
            let req = TestRequest::post()
                .uri("/node-identity")
                .insert_header((AUTHORIZATION, "Bearer any-token"))
                .set_json(&body)
                .to_request();
            let resp = test::call_service(&app, req).await;
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "body {body}");
        }
        Ok(())
    }

    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn startup_loads_nothing_before_an_identity_is_saved(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        assert!(load_node_identity(&pool, None).await.is_none());
        Ok(())
    }

    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn startup_logs_in_with_the_stored_client(pool: SqlitePool) -> anyhow::Result<()> {
        let idp = keycloak_answering(token_response("node-token")).await;
        save(&pool, &stored_identity(Some(keycloak_login(&idp.uri())))?).await?;

        let identity = load_node_identity(&pool, None)
            .await
            .context("no identity loaded")?;

        assert_eq!(identity.user_id, "node-login");
        assert_eq!(identity.token().await?, "node-token");
        Ok(())
    }

    /// One refused login must not stop the node from starting.
    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn startup_leaves_a_refused_login_unloaded(pool: SqlitePool) -> anyhow::Result<()> {
        let idp = keycloak_answering(ResponseTemplate::new(401)).await;
        save(&pool, &stored_identity(Some(keycloak_login(&idp.uri())))?).await?;

        assert!(load_node_identity(&pool, None).await.is_none());
        Ok(())
    }

    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn startup_in_insecure_mode_uses_the_unsafe_token(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        save(&pool, &stored_identity(None)?).await?;
        let registry = MockAuthRegistry::new(Arc::new(RwLock::new(Vec::new())));
        let unsafe_token = registry.get_by_str("").await.get_token();
        let auth = WorkflowAuth::Mock(Arc::new(registry));

        let identity = load_node_identity(&pool, Some(&auth))
            .await
            .context("no identity loaded")?;

        assert_eq!(identity.user_id, "node-login");
        assert_eq!(identity.token().await?, unsafe_token);
        Ok(())
    }
}
