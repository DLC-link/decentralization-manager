use actix_web::{HttpResponse, Responder, get, post, web};
use base64::Engine;
use canton_proto_rs::com::daml::ledger::api::v2::admin::{
    ListUserRightsRequest,
    right::{CanActAs, CanReadAs, Kind},
};

use crate::{
    auth::WorkflowAuth,
    config::NodeConfig,
    error::Result,
    server::{
        AppState,
        types::{
            AuthConfigResponse, AuthStatus, AuthStatusResponse, AuthTestResponse, AuthTestResult,
            PartyAuthStatus, RightsStatus,
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
        party_statuses.push(PartyAuthStatus {
            dec_party_id: "(test mode)".to_string(),
            member_party_id: "(test mode)".to_string(),
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
        let dec_party_id = party_creds.dec_party_id.to_string();
        let member_party_id = party_creds.member_party_id.to_string();
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
    member_party_id: &str,
    dec_party_id: &str,
) -> Result<RightsStatus> {
    let mut client = utils::create_user_client(config, Some(token.to_string())).await?;

    // For M2M auth, the actual user_id in Canton is from JWT's 'sub' claim
    let effective_user_id = extract_user_id_from_jwt(token).unwrap_or_else(|| user_id.to_string());

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
                if party == member_party_id {
                    member_party_act_as = true;
                }
                if party == dec_party_id {
                    dec_party_act_as = true;
                }
            }
            Some(Kind::CanReadAs(CanReadAs { ref party })) => {
                tracing::debug!("  CanReadAs: {party}");
                if party == member_party_id {
                    member_party_read_as = true;
                }
                if party == dec_party_id {
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
    if matches!(*auth, Some(WorkflowAuth::Mock(_))) {
        results.push(AuthTestResult {
            party_id: "(test mode)".to_string(),
            success: true,
            error: None,
        });
        return HttpResponse::Ok().json(AuthTestResponse { results });
    }
    drop(auth);

    let party_creds_list = data.party_credentials.read().await;
    for party_creds in party_creds_list.iter() {
        let dec_party_id = party_creds.dec_party_id.to_string();

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
