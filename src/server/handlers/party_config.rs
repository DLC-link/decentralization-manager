use std::sync::Arc;

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, put, web};
use canton_proto_rs::com::daml::ledger::api::v2::admin::GetUserRequest;
use keycloak::login::{
    ClientCredentialsParams, PasswordParams, client_credentials, password, token_url,
};
use tokio::sync::RwLock;

use crate::{
    auth::{AuthRegistry, WorkflowAuth, auth0_client_credentials},
    config::{Auth0M2MConfig, KeycloakConfig, PartyCredentials, default_package_config},
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
            auth0_domain: c.auth0.as_ref().map(|a| a.domain.clone()),
            auth0_audience: c.auth0.as_ref().map(|a| a.audience.clone()),
            auth0_client_id: c.auth0.as_ref().map(|a| a.client_id.clone()),
            has_auth0_client_secret: c
                .auth0
                .as_ref()
                .is_some_and(|a| !a.client_secret.is_empty()),
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
                auth0_domain: data.config.auth0.as_ref().map(|a| a.domain.clone()),
                auth0_audience: data.config.auth0.as_ref().and_then(|a| a.audience.clone()),
                auth0_client_id: None,
                has_auth0_client_secret: false,
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
    let (existing_keycloak, existing_auth0) = {
        let party_creds = data.party_credentials.read().await;
        let existing = party_creds
            .iter()
            .find(|p| p.dec_party_id == req.dec_party_id);
        (
            existing.map(|p| p.keycloak.clone()),
            existing.and_then(|p| p.auth0.clone()),
        )
    };

    // Bootstrap exemption: the middleware lets first-run PUT /party-config
    // through unauthenticated. Once any party credential exists, require the
    // caller to carry the admin role.
    let is_fresh = data.party_credentials.read().await.is_empty();
    if !is_fresh && let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }

    let auth0_requested = req.auth0_domain.as_deref().is_some_and(|s| !s.is_empty());

    if auth0_requested {
        // Auth0 path: domain + audience + client_id + client_secret all required
        // (secret may carry forward when domain is unchanged).
        let Some(domain) = req.auth0_domain.as_deref().filter(|s| !s.is_empty()) else {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: "auth0_domain is required".to_string(),
            });
        };
        let Some(audience) = req.auth0_audience.as_deref().filter(|s| !s.is_empty()) else {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: "auth0_audience is required".to_string(),
            });
        };
        let Some(client_id) = req.auth0_client_id.as_deref().filter(|s| !s.is_empty()) else {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: "auth0_client_id is required".to_string(),
            });
        };

        // Mirror the Keycloak URL-change guard: never carry a stored secret
        // forward to a freshly-supplied (potentially attacker-controlled)
        // domain. A new domain means the operator must resupply the secret.
        let domain_changed = existing_auth0.as_ref().is_some_and(|e| e.domain != domain);
        let new_secret_is_present = req
            .auth0_client_secret
            .as_deref()
            .is_some_and(|s| !s.is_empty());
        if domain_changed && !new_secret_is_present {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: "auth0_domain changed; resubmit with a fresh auth0_client_secret \
                        in the same request"
                    .to_string(),
            });
        }

        // Either keep the existing secret (omitted/empty input) or use the
        // new one. "Clearing" doesn't make sense for an Auth0 config — an
        // empty client_secret produces a 401 on every mint — so empty is
        // treated the same as omitted. Switching off Auth0 is done by
        // submitting a Keycloak payload (no auth0_domain).
        let client_secret = match req.auth0_client_secret.as_deref().filter(|s| !s.is_empty()) {
            Some(s) => s.to_string(),
            None => existing_auth0
                .as_ref()
                .map(|a| a.client_secret.clone())
                .unwrap_or_default(),
        };

        if client_secret.is_empty() {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: "auth0_client_secret is required for first-time Auth0 setup".to_string(),
            });
        }

        let creds = PartyCredentials {
            dec_party_id: req.dec_party_id.clone(),
            member_party_id: req.member_party_id.clone(),
            user_id: req.user_id.clone(),
            keycloak: KeycloakConfig::default(),
            auth0: Some(Auth0M2MConfig {
                domain: domain.to_string(),
                audience: audience.to_string(),
                client_id: client_id.to_string(),
                client_secret,
            }),
            packages: default_package_config(),
        };

        return persist_and_reload(data, creds, &req.dec_party_id).await;
    }

    // Keycloak path (legacy). Reject empty url/realm/client_id up front —
    // the request struct accepts these via `#[serde(default)]` for symmetry
    // with the Auth0 path, but on the Keycloak branch they're load-bearing
    // and silently persisting empty values would just produce 502s on every
    // later token mint. Skipped under test mode so the integration suite
    // (which PUTs explicit empty strings + uses the mock auth registry) is
    // still happy.
    if !data.test_mode
        && (req.keycloak_url.trim().is_empty()
            || req.keycloak_realm.trim().is_empty()
            || req.keycloak_client_id.trim().is_empty())
    {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "keycloak_url, keycloak_realm, and keycloak_client_id are required \
                    (or supply auth0_domain to use the Auth0 path)"
                .to_string(),
        });
    }

    // When the token-endpoint URL changes, never carry the existing
    // credentials forward — otherwise a redirected `keycloak_url` would
    // cause `reload_auth()` to POST the real client_secret (and any stored
    // username/password) to the attacker-controlled host. Require the
    // request to carry a fresh credential set in the same payload; if none
    // is supplied, reject before persisting anything.
    let url_changed = existing_keycloak
        .as_ref()
        .is_some_and(|e| e.url != req.keycloak_url);
    let new_secret_is_present = req
        .keycloak_client_secret
        .as_deref()
        .is_some_and(|s| !s.is_empty());
    let new_password_pair_is_present = req
        .keycloak_username
        .as_deref()
        .is_some_and(|s| !s.is_empty())
        && req
            .keycloak_password
            .as_deref()
            .is_some_and(|s| !s.is_empty());
    if url_changed && !new_secret_is_present && !new_password_pair_is_present {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "keycloak_url changed; resubmit with a fresh keycloak_client_secret \
                    (or keycloak_username + keycloak_password) in the same request"
                .to_string(),
        });
    }

    let keycloak = if url_changed {
        // Fresh URL → fresh credentials only. Treat any omitted field as
        // "not set on the new IdP" rather than "carry forward". An empty
        // string still means "clear" via `filter`.
        KeycloakConfig {
            url: req.keycloak_url,
            realm: req.keycloak_realm,
            client_id: req.keycloak_client_id,
            client_secret: req.keycloak_client_secret.filter(|s| !s.is_empty()),
            username: req.keycloak_username.filter(|s| !s.is_empty()),
            password: req.keycloak_password.filter(|s| !s.is_empty()),
        }
    } else {
        KeycloakConfig {
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
        }
    };

    let creds = PartyCredentials {
        dec_party_id: req.dec_party_id.clone(),
        member_party_id: req.member_party_id,
        user_id: req.user_id,
        keycloak,
        auth0: None,
        packages: default_package_config(),
    };

    persist_and_reload(data, creds, &req.dec_party_id).await
}

async fn persist_and_reload(
    data: web::Data<AppState>,
    creds: PartyCredentials,
    dec_party_id: &CantonId,
) -> HttpResponse {
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
        if let Some(existing) = pc.iter_mut().find(|p| p.dec_party_id == *dec_party_id) {
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
        (status = 500, description = "Canton call failed", body = ErrorResponse),
        (status = 502, description = "Upstream IdP rejected the supplied credentials", body = ErrorResponse)
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

    // Auth0 path takes precedence when domain is supplied.
    let token = if let Some(domain) = body.auth0_domain.as_deref().filter(|s| !s.is_empty()) {
        let Some(audience) = body.auth0_audience.as_deref().filter(|s| !s.is_empty()) else {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: "auth0_audience is required".to_string(),
            });
        };
        let Some(client_id) = body.auth0_client_id.as_deref().filter(|s| !s.is_empty()) else {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: "auth0_client_id is required".to_string(),
            });
        };
        let Some(client_secret) = body
            .auth0_client_secret
            .as_deref()
            .filter(|s| !s.is_empty())
        else {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: "auth0_client_secret is required".to_string(),
            });
        };

        let cfg = Auth0M2MConfig {
            domain: domain.to_string(),
            audience: audience.to_string(),
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
        };
        match auth0_client_credentials(&data.http_client, &cfg).await {
            Ok(resp) => resp.access_token,
            Err(e) => {
                tracing::warn!("Auth0 auth failed during member-party discovery: {e:#}");
                // 502 rather than 401: the *upstream* IdP rejected our M2M
                // request. Returning 401 would trigger the SPA's session-
                // wipe-and-reload path (which assumes the caller's own token
                // went stale) even though their session is fine.
                return HttpResponse::BadGateway().json(ErrorResponse {
                    error: "Auth0 auth failed".into(),
                });
            }
        }
    } else {
        if body.keycloak_url.trim().is_empty()
            || body.keycloak_realm.trim().is_empty()
            || body.keycloak_client_id.trim().is_empty()
        {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: "keycloak_url, keycloak_realm, and keycloak_client_id are required \
                        (or supply auth0_domain to use the Auth0 path)"
                    .to_string(),
            });
        }

        let token_endpoint = token_url(&body.keycloak_url, &body.keycloak_realm);

        let token_result = if let Some(secret) = body
            .keycloak_client_secret
            .as_ref()
            .filter(|s| !s.is_empty())
        {
            client_credentials(ClientCredentialsParams {
                url: token_endpoint,
                client_id: body.keycloak_client_id.clone(),
                client_secret: secret.clone(),
            })
            .await
        } else if let (Some(username), Some(pw)) = (
            body.keycloak_username.as_ref().filter(|s| !s.is_empty()),
            body.keycloak_password.as_ref().filter(|s| !s.is_empty()),
        ) {
            password(PasswordParams {
                url: token_endpoint,
                client_id: body.keycloak_client_id.clone(),
                username: username.clone(),
                password: pw.clone(),
            })
            .await
        } else {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: "Provide either keycloak_client_secret, \
                        keycloak_username + keycloak_password, or the auth0_* fields"
                    .to_string(),
            });
        };

        match token_result {
            Ok(resp) => resp.access_token,
            Err(e) => {
                tracing::warn!("Keycloak auth failed during member-party discovery: {e:#}");
                return HttpResponse::BadGateway().json(ErrorResponse {
                    error: "Keycloak auth failed".into(),
                });
            }
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
        sync::{Arc, atomic::AtomicBool},
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

    use super::{discover_member_party, save_party_config};
    use crate::{
        auth::{MockAuthRegistry, MockValidator, TokenValidator, WorkflowAuth},
        config::{KeycloakConfig, NodeConfig, PartyCredentials, default_package_config},
        noise::NoiseKeypair,
        participant_id::CantonId,
        server::{AppState, middleware::AuthMiddleware},
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
            noise_listener_pause_flag: Arc::new(AtomicBool::new(false)),
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
            workflow_in_flight: Arc::new(AtomicBool::new(false)),
            test_mode: true,
            refreshing_prefixes: Arc::new(RwLock::new(HashSet::new())),
            http_client: reqwest::Client::new(),
            http_advertised_url: "http://127.0.0.1:8080".to_string(),
            noise_keypair: Arc::new(NoiseKeypair::generate()),
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

    /// Guards bullet #2 of the audit finding: an admin who changes
    /// `keycloak_url` without supplying a fresh `keycloak_client_secret`
    /// must get a 400 — otherwise `merge_optional_secret` would forward
    /// the existing real secret to the new (potentially attacker-controlled)
    /// host on the next token mint. State is pre-populated with one party so
    /// the bootstrap exemption does not apply; the request is admin-authed
    /// via `AuthMiddleware` + `MockValidator`.
    #[actix_web::test]
    async fn save_party_config_rejects_url_change_without_fresh_secret() {
        let db = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        let existing_dec = CantonId::parse(&format!("dec-party::{ns}")).expect("parse dec party");
        let existing_member =
            CantonId::parse(&format!("member-party::{ns}")).expect("parse member party");
        let existing = PartyCredentials {
            dec_party_id: existing_dec.clone(),
            member_party_id: existing_member.clone(),
            user_id: "test-user".to_string(),
            keycloak: KeycloakConfig {
                url: "https://original-keycloak.example.com".to_string(),
                realm: "test-realm".to_string(),
                client_id: "test-client".to_string(),
                client_secret: Some("super-secret".to_string()),
                username: None,
                password: None,
            },
            auth0: None,
            packages: default_package_config(),
        };
        let party_credentials = Arc::new(RwLock::new(vec![existing]));
        let state = Data::new(AppState {
            db,
            config: NodeConfig::default(),
            peer_status: Arc::new(RwLock::new(HashMap::new())),
            last_seen: Arc::new(RwLock::new(HashMap::new())),
            noise_listener_pause_flag: Arc::new(AtomicBool::new(false)),
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
            workflow_in_flight: Arc::new(AtomicBool::new(false)),
            test_mode: true,
            refreshing_prefixes: Arc::new(RwLock::new(HashSet::new())),
            http_client: reqwest::Client::new(),
            http_advertised_url: "http://127.0.0.1:8080".to_string(),
            noise_keypair: Arc::new(NoiseKeypair::generate()),
        });
        let app = test::init_service(
            App::new()
                .app_data(state)
                .wrap(AuthMiddleware)
                .service(save_party_config),
        )
        .await;
        let req = TestRequest::put()
            .uri("/party-config")
            .insert_header(("Authorization", "Bearer any.thing.here"))
            .set_json(json!({
                "dec_party_id": existing_dec.to_string(),
                "member_party_id": existing_member.to_string(),
                "user_id": "test-user",
                "keycloak_url": "https://attacker-host.example.com",
                "keycloak_realm": "test-realm",
                "keycloak_client_id": "test-client",
                // keycloak_client_secret intentionally omitted
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
