use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use base64::Engine;
use canton_proto_rs::com::daml::ledger::api::v2::admin::{
    GrantUserRightsRequest, ListUserRightsRequest, Right,
    right::{CanActAs, CanReadAs, Kind},
};
use keycloak::login::{ClientCredentialsParams, client_credentials, password_url};

use crate::{
    auth::WorkflowAuth,
    config::NodeConfig,
    error::Result,
    participant_id::CantonId,
    server::{
        AppState,
        middleware::require_admin,
        types::{
            AuthConfigResponse, AuthStatus, AuthStatusResponse, AuthTestResponse, AuthTestResult,
            ErrorResponse, GrantRightsRequest, GrantRightsResponse, PartyAuthStatus, RightsStatus,
        },
    },
    utils,
};

/// Get frontend auth configuration (keycloak details + whether auth is required)
#[utoipa::path(
    tag = "Authentication",
    responses(
        (status = 200, description = "Auth configuration", body = AuthConfigResponse)
    )
)]
#[get("/auth-config")]
pub async fn get_auth_config(data: web::Data<AppState>) -> impl Responder {
    match &data.config.keycloak {
        Some(config) if !data.test_mode => HttpResponse::Ok().json(AuthConfigResponse {
            auth_required: true,
            keycloak_host: Some(config.url.clone()),
            keycloak_realm: Some(config.realm.clone()),
            keycloak_client_id: Some(config.client_id.clone()),
        }),
        _ => HttpResponse::Ok().json(AuthConfigResponse {
            auth_required: false,
            keycloak_host: None,
            keycloak_realm: None,
            keycloak_client_id: None,
        }),
    }
}

/// Check authentication status for all configured parties
#[utoipa::path(
    tag = "Authentication",
    responses(
        (status = 200, description = "Authentication status for all parties", body = AuthStatusResponse)
    )
)]
#[get("/auth/status")]
pub async fn get_auth_status(data: web::Data<AppState>) -> impl Responder {
    let mut party_statuses = Vec::new();

    let auth = data.auth.read().await;

    // Handle test mode - return mock status
    if let Some(WorkflowAuth::Mock(ref mock_registry)) = *auth {
        let manager = mock_registry.get_by_str("").await;
        // In test mode we don't have a real dec_party / member_party pair,
        // so we surface the mock's member_party_id for both. Real auth flows
        // overwrite this with the configured creds below.
        let mock_member = manager.member_party_id().clone();
        party_statuses.push(PartyAuthStatus {
            dec_party_id: mock_member.clone(),
            member_party_id: mock_member,
            user_id: manager.user_id().to_string(),
            keycloak_url: None,
            keycloak_realm: None,
            status: AuthStatus::Mock,
            rights: None,
        });
        return HttpResponse::Ok().json(AuthStatusResponse {
            parties: party_statuses,
        });
    }

    let party_creds_list = data.party_credentials.read().await;

    // Check each configured party
    for party_creds in party_creds_list.iter() {
        let dec_party_id = party_creds.dec_party_id.clone();
        let member_party_id = party_creds.member_party_id.clone();
        let user_id = party_creds.user_id.clone();

        // Try to get a token from the auth registry
        let (status, token) = match &*auth {
            Some(WorkflowAuth::Keycloak(registry)) => {
                match registry.get(&party_creds.dec_party_id) {
                    Some(tm) => match tm.get_token().await {
                        Ok(t) => (AuthStatus::Authenticated, Some(t)),
                        Err(e) => (
                            AuthStatus::Failed {
                                error: e.to_string(),
                            },
                            None,
                        ),
                    },
                    None => (AuthStatus::NotConfigured, None),
                }
            }
            _ => (AuthStatus::NotConfigured, None),
        };

        // Check user rights if we have a valid token
        let rights = if let Some(ref t) = token {
            check_user_rights(&data.config, t, &user_id, &member_party_id, &dec_party_id)
                .await
                .ok()
        } else {
            None
        };

        party_statuses.push(PartyAuthStatus {
            dec_party_id,
            member_party_id,
            user_id,
            keycloak_url: Some(party_creds.keycloak.url.clone()),
            keycloak_realm: Some(party_creds.keycloak.realm.clone()),
            status,
            rights,
        });
    }

    HttpResponse::Ok().json(AuthStatusResponse {
        parties: party_statuses,
    })
}

/// Extract user_id (sub claim) from JWT token
fn extract_user_id_from_jwt(token: &str) -> Option<String> {
    // JWT format: header.payload.signature
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }

    // Decode the payload (second part) - URL-safe base64 without padding
    let payload = parts[1];
    let padding_needed = (4 - (payload.len() % 4)) % 4;
    let padded = if padding_needed > 0 {
        format!("{}{}", payload, "=".repeat(padding_needed))
    } else {
        payload.to_string()
    };

    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&padded)
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    json.get("sub").and_then(|v| v.as_str()).map(String::from)
}

/// Check user rights for both member party and decentralized party
async fn check_user_rights(
    config: &NodeConfig,
    token: &str,
    user_id: &str,
    member_party_id: &CantonId,
    dec_party_id: &CantonId,
) -> Result<RightsStatus> {
    let mut client = utils::create_user_client(config, Some(token.to_string())).await?;

    // For M2M auth, the actual user_id in Canton is from JWT's 'sub' claim
    let effective_user_id = extract_user_id_from_jwt(token).unwrap_or_else(|| user_id.to_string());

    let member_party_id_str = member_party_id.to_string();
    let dec_party_id_str = dec_party_id.to_string();

    tracing::debug!(
        "Checking rights for user_id={effective_user_id} (configured: {user_id}), member_party={member_party_id}, dec_party={dec_party_id}"
    );

    let response = client
        .list_user_rights(tonic::Request::new(ListUserRightsRequest {
            user_id: effective_user_id.clone(),
            identity_provider_id: String::new(),
        }))
        .await?
        .into_inner();

    tracing::debug!(
        "ListUserRights for {effective_user_id} returned {} rights",
        response.rights.len()
    );

    let mut member_party_act_as = false;
    let mut member_party_read_as = false;
    let mut dec_party_act_as = false;
    let mut dec_party_read_as = false;

    for right in response.rights {
        match right.kind {
            Some(Kind::CanActAs(CanActAs { ref party })) => {
                tracing::debug!("  CanActAs: {party}");
                if party == &member_party_id_str {
                    member_party_act_as = true;
                }
                if party == &dec_party_id_str {
                    dec_party_act_as = true;
                }
            }
            Some(Kind::CanReadAs(CanReadAs { ref party })) => {
                tracing::debug!("  CanReadAs: {party}");
                if party == &member_party_id_str {
                    member_party_read_as = true;
                }
                if party == &dec_party_id_str {
                    dec_party_read_as = true;
                }
            }
            _ => {}
        }
    }

    Ok(RightsStatus {
        member_party_act_as,
        member_party_read_as,
        dec_party_act_as,
        dec_party_read_as,
    })
}

/// Test authentication by attempting to get a fresh token
#[utoipa::path(
    tag = "Authentication",
    responses(
        (status = 200, description = "Authentication test results", body = AuthTestResponse)
    )
)]
#[post("/auth/test")]
pub async fn test_auth(data: web::Data<AppState>) -> impl Responder {
    let mut results = Vec::new();

    // Handle test mode - mock auth always succeeds
    let auth = data.auth.read().await;
    if let Some(WorkflowAuth::Mock(ref mock_registry)) = *auth {
        // No real dec_party in mock — surface the mock's member party so the
        // wire format stays a valid CantonId.
        let manager = mock_registry.get_by_str("").await;
        results.push(AuthTestResult {
            party_id: manager.member_party_id().clone(),
            success: true,
            error: None,
        });
        return HttpResponse::Ok().json(AuthTestResponse { results });
    }
    drop(auth);

    let party_creds_list = data.party_credentials.read().await;
    for party_creds in party_creds_list.iter() {
        let dec_party_id = party_creds.dec_party_id.clone();

        // Attempt fresh authentication
        let result = test_keycloak_auth(&party_creds.keycloak).await;

        results.push(AuthTestResult {
            party_id: dec_party_id,
            success: result.is_ok(),
            error: result.err(),
        });
    }

    HttpResponse::Ok().json(AuthTestResponse { results })
}

/// Grant actAs + readAs rights on the member party and the dec party to the
/// configured coordinator user, using the participant admin API
#[utoipa::path(
    tag = "Authentication",
    request_body = GrantRightsRequest,
    responses(
        (status = 200, description = "Rights granted; current rights returned", body = GrantRightsResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 404, description = "Party not configured", body = ErrorResponse),
        (status = 500, description = "Grant failed", body = ErrorResponse)
    )
)]
#[post("/auth/grant-rights")]
pub async fn grant_rights(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<GrantRightsRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }

    let admin_client_id = body.admin_client_id.trim().to_string();
    let admin_client_secret = body.admin_client_secret.trim().to_string();
    if admin_client_id.is_empty() || admin_client_secret.is_empty() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "admin_client_id and admin_client_secret are required".to_string(),
        });
    }

    let auth = data.auth.read().await;
    if matches!(*auth, Some(WorkflowAuth::Mock(_))) {
        return HttpResponse::Ok().json(GrantRightsResponse {
            rights: RightsStatus {
                member_party_act_as: true,
                member_party_read_as: true,
                dec_party_act_as: true,
                dec_party_read_as: true,
            },
        });
    }

    let party_creds_list = data.party_credentials.read().await;
    let Some(party_creds) = party_creds_list
        .iter()
        .find(|c| c.dec_party_id == body.dec_party_id)
    else {
        return HttpResponse::NotFound().json(ErrorResponse {
            error: format!(
                "Party {dec_party_id} is not configured",
                dec_party_id = body.dec_party_id
            ),
        });
    };

    let party_token = match &*auth {
        Some(WorkflowAuth::Keycloak(registry)) => match registry.get(&party_creds.dec_party_id) {
            Some(tm) => match tm.get_token().await {
                Ok(t) => t,
                Err(e) => {
                    return HttpResponse::InternalServerError().json(ErrorResponse {
                        error: format!("Failed to get auth token: {e}"),
                    });
                }
            },
            None => {
                return HttpResponse::InternalServerError().json(ErrorResponse {
                    error: "No token manager configured for this party".to_string(),
                });
            }
        },
        _ => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Auth not configured".to_string(),
            });
        }
    };

    let member_party_id = party_creds.member_party_id.clone();
    let dec_party_id = party_creds.dec_party_id.clone();
    let user_id = party_creds.user_id.clone();
    let token_url = password_url(&party_creds.keycloak.url, &party_creds.keycloak.realm);

    drop(party_creds_list);
    drop(auth);

    let admin_token = match client_credentials(ClientCredentialsParams {
        url: token_url,
        client_id: admin_client_id,
        client_secret: admin_client_secret,
    })
    .await
    {
        Ok(resp) => resp.access_token,
        Err(e) => {
            // Full chain to logs (the keycloak crate's Display can include
            // request URL / response body — keep it server-side); generic
            // message in the response so we don't surface reflected secrets.
            tracing::warn!("Failed to mint admin token for grant-rights: {e:#}");
            return HttpResponse::Unauthorized().json(ErrorResponse {
                error: "Admin Keycloak auth failed".into(),
            });
        }
    };

    match grant_user_rights(
        &data.config,
        &admin_token,
        &party_token,
        &user_id,
        &member_party_id,
        &dec_party_id,
    )
    .await
    {
        Ok(rights) => HttpResponse::Ok().json(GrantRightsResponse { rights }),
        Err(e) => {
            tracing::error!("Failed to grant rights: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to grant rights".into(),
            })
        }
    }
}

/// Grant actAs+readAs for both parties to the user using the admin token,
/// then re-check rights using the per-party token (read-only).
async fn grant_user_rights(
    config: &NodeConfig,
    admin_token: &str,
    party_token: &str,
    user_id: &str,
    member_party_id: &CantonId,
    dec_party_id: &CantonId,
) -> Result<RightsStatus> {
    let mut client = utils::create_user_client(config, Some(admin_token.to_string())).await?;

    let effective_user_id =
        extract_user_id_from_jwt(party_token).unwrap_or_else(|| user_id.to_string());

    tracing::info!(
        "Granting rights for user_id={effective_user_id}, \
         member_party={member_party_id}, dec_party={dec_party_id}"
    );

    let member_party_id_str = member_party_id.to_string();
    let dec_party_id_str = dec_party_id.to_string();
    let rights = vec![
        right_act_as(&member_party_id_str),
        right_read_as(&member_party_id_str),
        right_act_as(&dec_party_id_str),
        right_read_as(&dec_party_id_str),
    ];

    let response = client
        .grant_user_rights(tonic::Request::new(GrantUserRightsRequest {
            user_id: effective_user_id.clone(),
            rights,
            identity_provider_id: String::new(),
        }))
        .await?
        .into_inner();

    tracing::info!(
        "GrantUserRights newly granted {count} right(s)",
        count = response.newly_granted_rights.len()
    );

    check_user_rights(config, party_token, user_id, member_party_id, dec_party_id).await
}

fn right_act_as(party: &str) -> Right {
    Right {
        kind: Some(Kind::CanActAs(CanActAs {
            party: party.to_string(),
        })),
    }
}

fn right_read_as(party: &str) -> Right {
    Right {
        kind: Some(Kind::CanReadAs(CanReadAs {
            party: party.to_string(),
        })),
    }
}

async fn test_keycloak_auth(
    config: &crate::config::KeycloakConfig,
) -> std::result::Result<(), String> {
    let url = keycloak::login::password_url(&config.url, &config.realm);

    // Use client_credentials if client_secret is set, otherwise password flow
    if let Some(ref client_secret) = config.client_secret {
        keycloak::login::client_credentials(keycloak::login::ClientCredentialsParams {
            url,
            client_id: config.client_id.clone(),
            client_secret: client_secret.clone(),
        })
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
    } else {
        let username = config
            .username
            .as_ref()
            .ok_or_else(|| "Missing username for password flow".to_string())?;
        let password = config
            .password
            .as_ref()
            .ok_or_else(|| "Missing password for password flow".to_string())?;

        keycloak::login::password(keycloak::login::PasswordParams {
            client_id: config.client_id.clone(),
            username: username.clone(),
            password: password.clone(),
            url,
        })
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        sync::Arc,
    };

    use actix_web::{
        App,
        http::{StatusCode, header::AUTHORIZATION},
        test::{self, TestRequest},
        web::Data,
    };
    use serde_json::{Value, json};
    use sqlx::SqlitePool;
    use tokio::sync::{Mutex, Notify, RwLock};

    use super::grant_rights;
    use crate::{
        auth::{MockAuthRegistry, MockValidator, TokenValidator, WorkflowAuth},
        config::NodeConfig,
        server::{AppState, ListenerControl, middleware::AuthMiddleware},
    };

    /// Build an `AppState` configured for handler-level tests:
    /// - in-memory sqlite (no migrations needed for grant_rights paths)
    /// - `MockValidator` accepts any token, mints an "admin"-roled principal
    /// - `WorkflowAuth::Mock` so `grant_rights` hits its test-mode short-circuit
    /// - `admin_role` is configurable so the require-admin gate can be exercised
    async fn build_state(admin_role: Option<&str>) -> Data<AppState> {
        let db = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        let party_credentials = Arc::new(RwLock::new(Vec::new()));
        Data::new(AppState {
            db,
            config: NodeConfig::default(),
            peer_status: Arc::new(RwLock::new(HashMap::new())),
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
            admin_role: admin_role.map(str::to_string),
            party_credentials,
            bootstrap_mu: Arc::new(Mutex::new(())),
            workflow_in_flight: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            test_mode: true,
            refreshing_prefixes: Arc::new(RwLock::new(HashSet::new())),
            http_client: reqwest::Client::new(),
        })
    }

    /// `dec_party_id` deserializes via `CantonId::parse`, which requires a
    /// `prefix::<68-hex-char namespace>` shape (34 bytes). Pin a fixed valid
    /// value so the JSON extractor doesn't 400 before the handler runs.
    const VALID_CANTON_ID: &str =
        "test-network::12200000000000000000000000000000000000000000000000000000000000000000";

    /// In mock mode, `grant_rights` short-circuits and returns canned
    /// `RightsStatus` with all four rights `true`. This proves the handler
    /// is reachable and the security-sensitive Keycloak path is bypassed
    /// when we tell it to be — operators running with `--features test-mode`
    /// rely on this for swagger and CI smoke tests.
    #[actix_web::test]
    async fn grant_rights_mock_mode_returns_canned_rights() {
        let state = build_state(None).await;
        let app = test::init_service(
            App::new()
                .app_data(state)
                .wrap(AuthMiddleware)
                .service(grant_rights),
        )
        .await;
        let req = TestRequest::post()
            .uri("/auth/grant-rights")
            .insert_header((AUTHORIZATION, "Bearer any-token"))
            .set_json(json!({
                "dec_party_id": VALID_CANTON_ID,
                "admin_client_id": "validator-admin",
                "admin_client_secret": "secret",
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        let rights = body
            .get("rights")
            .expect("response carries `rights` object");
        for field in [
            "member_party_act_as",
            "member_party_read_as",
            "dec_party_act_as",
            "dec_party_read_as",
        ] {
            assert_eq!(
                rights.get(field),
                Some(&Value::Bool(true)),
                "expected canned `{field}: true` in mock-mode RightsStatus, got {body}"
            );
        }
    }

    /// `require_admin` rejects requests that arrive without a `Principal`
    /// attached. We skip the `AuthMiddleware` wrap here so no principal is
    /// injected — that's the production path when a request slips past auth
    /// (e.g. middleware misconfigured) and the handler is the last line of
    /// defense. With `admin_role = Some(...)`, the response is 401 from
    /// `require_admin`'s own guard, before any body validation runs.
    #[actix_web::test]
    async fn grant_rights_rejects_request_without_principal() {
        let state = build_state(Some("decman-admin")).await;
        let app = test::init_service(App::new().app_data(state).service(grant_rights)).await;
        let req = TestRequest::post()
            .uri("/auth/grant-rights")
            .set_json(json!({
                "dec_party_id": VALID_CANTON_ID,
                "admin_client_id": "validator-admin",
                "admin_client_secret": "secret",
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
