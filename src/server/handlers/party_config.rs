use std::sync::Arc;

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, put, web};
use canton_proto_rs::com::daml::ledger::api::v2::admin::GetUserRequest;
use keycloak::login::{
    ClientCredentialsParams, PasswordParams, client_credentials, password, password_url,
};
use tokio::sync::RwLock;

use crate::{
    auth::{AuthRegistry, WorkflowAuth},
    config::{KeycloakConfig, PartyCredentials, default_package_config},
    db::schema::{Commitable, SchemaWrite},
    error::Result,
    participant_id::CantonId,
    server::{
        AppState,
        middleware::require_admin,
        types::{
            DiscoverMemberPartyRequest, DiscoverMemberPartyResponse, ErrorResponse,
            PartyConfigRequest, PartyConfigResponse, SuccessResponse,
        },
    },
    utils,
};

/// Get party configuration (secrets masked)
#[utoipa::path(
    tag = "Configuration",
    params(("dec_party_id" = String, Path, description = "Decentralized party ID")),
    responses(
        (status = 200, description = "Party configuration (or defaults if not configured)", body = PartyConfigResponse),
        (status = 400, description = "Invalid dec_party_id", body = ErrorResponse)
    )
)]
#[get("/party-config/{dec_party_id}")]
pub async fn get_party_config(
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    let dec_party_id_str = path.into_inner();

    let dec_party_id = match CantonId::parse(&dec_party_id_str) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("Invalid dec_party_id: {e}"),
            });
        }
    };

    let party_creds = data.party_credentials.read().await;
    let creds = party_creds.iter().find(|p| p.dec_party_id == dec_party_id);

    match creds {
        Some(c) => HttpResponse::Ok().json(PartyConfigResponse {
            dec_party_id: c.dec_party_id.clone(),
            member_party_id: Some(c.member_party_id.clone()),
            user_id: Some(c.user_id.clone()),
            keycloak_url: c.keycloak.url.clone(),
            keycloak_realm: c.keycloak.realm.clone(),
            keycloak_client_id: c.keycloak.client_id.clone(),
            has_client_secret: c.keycloak.client_secret.is_some(),
            has_username: c.keycloak.username.is_some(),
            has_password: c.keycloak.password.is_some(),
            packages: default_package_config(),
        }),
        None => {
            let kc_defaults = data.config.canton.network.keycloak_defaults();
            let packages = default_package_config();
            HttpResponse::Ok().json(PartyConfigResponse {
                dec_party_id,
                member_party_id: None,
                user_id: None,
                keycloak_url: kc_defaults.url,
                keycloak_realm: kc_defaults.realm,
                keycloak_client_id: String::new(),
                has_client_secret: false,
                has_username: false,
                has_password: false,
                packages,
            })
        }
    }
}

/// Save or update party configuration
#[utoipa::path(
    tag = "Configuration",
    request_body = PartyConfigRequest,
    responses(
        (status = 200, description = "Party configuration saved", body = SuccessResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[put("/party-config")]
pub async fn save_party_config(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<PartyConfigRequest>,
) -> impl Responder {
    let req = body.into_inner();

    // Credential merge: None = keep existing, Some("") = clear, Some(val) = set
    let existing_keycloak = {
        let party_creds = data.party_credentials.read().await;
        party_creds
            .iter()
            .find(|p| p.dec_party_id == req.dec_party_id)
            .map(|p| p.keycloak.clone())
    };

    // Bootstrap exemption: the middleware lets first-run PUT /party-config
    // through unauthenticated. Once any party credential exists, require the
    // caller to carry the admin role.
    let is_fresh = data.party_credentials.read().await.is_empty();
    if !is_fresh && let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }

    let keycloak = KeycloakConfig {
        url: req.keycloak_url,
        realm: req.keycloak_realm,
        client_id: req.keycloak_client_id,
        client_secret: merge_optional_secret(
            req.keycloak_client_secret,
            existing_keycloak
                .as_ref()
                .and_then(|k| k.client_secret.clone()),
        ),
        username: merge_optional_secret(
            req.keycloak_username,
            existing_keycloak.as_ref().and_then(|k| k.username.clone()),
        ),
        password: merge_optional_secret(
            req.keycloak_password,
            existing_keycloak.as_ref().and_then(|k| k.password.clone()),
        ),
    };

    let creds = PartyCredentials {
        dec_party_id: req.dec_party_id.clone(),
        member_party_id: req.member_party_id,
        user_id: req.user_id,
        keycloak,
        packages: default_package_config(),
    };

    // Primary write: save to database
    {
        let mut tx = match data.db.begin_transaction().await {
            Ok(tx) => tx,
            Err(e) => {
                return HttpResponse::InternalServerError().json(ErrorResponse {
                    error: format!("Failed to begin transaction: {e}"),
                });
            }
        };
        if let Err(e) = tx.upsert_party_credentials(&creds).await {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to save party credentials: {e}"),
            });
        }
        if let Err(e) = Commitable::commit(tx).await {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to commit transaction: {e}"),
            });
        }
    }

    {
        let mut pc = data.party_credentials.write().await;
        if let Some(existing) = pc.iter_mut().find(|p| p.dec_party_id == req.dec_party_id) {
            *existing = creds;
        } else {
            pc.push(creds);
        }
    }

    if !data.test_mode
        && let Err(e) = reload_auth(&data.party_credentials, &data.auth).await
    {
        tracing::warn!("Failed to reinitialize auth registry: {e}");
    }

    HttpResponse::Ok().json(SuccessResponse { success: true })
}

/// Merge an optional secret field: None = keep existing, Some("") = clear, Some(val) = set
fn merge_optional_secret(new: Option<String>, existing: Option<String>) -> Option<String> {
    match new {
        None => existing,
        Some(val) if val.is_empty() => None,
        some => some,
    }
}

/// Reinitialize the auth registry from current party credentials
pub async fn reload_auth(
    party_credentials: &Arc<RwLock<Vec<PartyCredentials>>>,
    auth_lock: &Arc<RwLock<Option<WorkflowAuth>>>,
) -> Result {
    let creds_snapshot = party_credentials.read().await.clone();

    if creds_snapshot.is_empty() {
        let mut auth = auth_lock.write().await;
        *auth = None;
        tracing::info!("Auth registry cleared (no party credentials)");
    } else {
        let registry = AuthRegistry::new(&creds_snapshot).await?;
        let mut auth = auth_lock.write().await;
        *auth = Some(WorkflowAuth::Keycloak(Arc::new(registry)));
        tracing::info!(
            "Auth registry reinitialized with {} parties",
            creds_snapshot.len()
        );
    }
    Ok(())
}

/// Discover the member party of the user the given Keycloak credentials
/// authenticate as, by minting a token + calling Canton's
/// `UserManagementService.GetUser`. Used to pre-fill the Member Party ID
/// field in the Party Configuration dialog.
#[utoipa::path(
    tag = "Configuration",
    request_body = DiscoverMemberPartyRequest,
    responses(
        (status = 200, description = "Discovered user record", body = DiscoverMemberPartyResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Keycloak auth failed", body = ErrorResponse),
        (status = 500, description = "Canton call failed", body = ErrorResponse)
    )
)]
#[post("/party-config/discover-member-party")]
pub async fn discover_member_party(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<DiscoverMemberPartyRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }

    let token_url = password_url(&body.keycloak_url, &body.keycloak_realm);

    let token_result = if let Some(secret) = body
        .keycloak_client_secret
        .as_ref()
        .filter(|s| !s.is_empty())
    {
        client_credentials(ClientCredentialsParams {
            url: token_url,
            client_id: body.keycloak_client_id.clone(),
            client_secret: secret.clone(),
        })
        .await
    } else if let (Some(username), Some(pw)) = (
        body.keycloak_username.as_ref().filter(|s| !s.is_empty()),
        body.keycloak_password.as_ref().filter(|s| !s.is_empty()),
    ) {
        password(PasswordParams {
            url: token_url,
            client_id: body.keycloak_client_id.clone(),
            username: username.clone(),
            password: pw.clone(),
        })
        .await
    } else {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "Provide either keycloak_client_secret or keycloak_username + keycloak_password"
                .to_string(),
        });
    };

    let token = match token_result {
        Ok(resp) => resp.access_token,
        Err(e) => {
            // Full chain to logs (keycloak Display can echo request URL or
            // response body); generic message to clients so we don't surface
            // reflected creds.
            tracing::warn!("Keycloak auth failed during member-party discovery: {e:#}");
            return HttpResponse::Unauthorized().json(ErrorResponse {
                error: "Keycloak auth failed".into(),
            });
        }
    };

    let mut client = match utils::create_user_client(&data.config, Some(token)).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to create user client: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to create user client".into(),
            });
        }
    };

    let response = match client
        .get_user(tonic::Request::new(GetUserRequest {
            user_id: String::new(),
            identity_provider_id: String::new(),
        }))
        .await
    {
        Ok(r) => r.into_inner(),
        Err(e) => {
            tracing::error!("Canton GetUser failed: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Canton GetUser failed".into(),
            });
        }
    };

    let Some(user) = response.user else {
        return HttpResponse::InternalServerError().json(ErrorResponse {
            error: "Canton returned no user".to_string(),
        });
    };

    let primary_party = if user.primary_party.is_empty() {
        None
    } else {
        CantonId::parse(&user.primary_party).ok()
    };

    let description = user
        .metadata
        .as_ref()
        .and_then(|m| m.annotations.get("description").cloned())
        .filter(|s| !s.is_empty());

    HttpResponse::Ok().json(DiscoverMemberPartyResponse {
        user_id: user.id,
        primary_party,
        description,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        sync::Arc,
    };

    use actix_web::{
        App,
        http::StatusCode,
        test::{self, TestRequest},
        web::Data,
    };
    use serde_json::json;
    use sqlx::SqlitePool;
    use tokio::sync::{Mutex, Notify, RwLock};

    use super::discover_member_party;
    use crate::{
        auth::{MockAuthRegistry, MockValidator, TokenValidator, WorkflowAuth},
        config::NodeConfig,
        server::{AppState, ListenerControl},
    };

    /// `discover_member_party` is admin-gated. Drive the handler without the
    /// `AuthMiddleware` wrap so no `Principal` is attached to the request,
    /// then assert `require_admin`'s 401 fires before any Keycloak/Canton
    /// call is attempted. This is the production handler-as-last-line-of-
    /// defense path: even if a request slips through (or middleware is
    /// misconfigured), the credential-handling code stays gated.
    #[actix_web::test]
    async fn discover_member_party_rejects_request_without_principal() {
        let db = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        let party_credentials = Arc::new(RwLock::new(Vec::new()));
        let state = Data::new(AppState {
            db,
            config: NodeConfig::default(),
            peer_status: Arc::new(RwLock::new(HashMap::new())),
            last_seen: Arc::new(RwLock::new(HashMap::new())),
            noise_listener_control: Arc::new(RwLock::new(ListenerControl {
                should_pause: false,
            })),
            noise_listener_notify: Arc::new(Notify::new()),
            onboarding_trigger: Arc::new(Notify::new()),
            kick_trigger: Arc::new(Notify::new()),
            contracts_trigger: Arc::new(Notify::new()),
            dars_trigger: Arc::new(Notify::new()),
            coordinator_pubkey: Arc::new(RwLock::new(None)),
            peer_run_instance: Arc::new(RwLock::new(None)),
            pending_invitations: Arc::new(RwLock::new(Vec::new())),
            auth: Arc::new(RwLock::new(Some(WorkflowAuth::Mock(Arc::new(
                MockAuthRegistry::new(party_credentials.clone()),
            ))))),
            token_validator: TokenValidator::Mock(Arc::new(MockValidator::new(
                "decman-admin".to_string(),
            ))),
            admin_role: Some("decman-admin".to_string()),
            party_credentials,
            bootstrap_mu: Arc::new(Mutex::new(())),
            test_mode: true,
            refreshing_prefixes: Arc::new(RwLock::new(HashSet::new())),
            http_client: reqwest::Client::new(),
        });
        let app =
            test::init_service(App::new().app_data(state).service(discover_member_party)).await;
        let req = TestRequest::post()
            .uri("/party-config/discover-member-party")
            .set_json(json!({
                "keycloak_url": "https://example.invalid",
                "keycloak_realm": "test",
                "keycloak_client_id": "validator-admin",
                "keycloak_client_secret": "secret",
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
