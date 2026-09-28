use actix_web::{HttpRequest, HttpResponse, Responder, get, http::header::RETRY_AFTER, post, web};
use base64::Engine;
use canton_proto_rs::com::daml::ledger::api::v2::admin::{
    GrantUserRightsRequest, ListUserRightsRequest, Right,
    right::{CanActAs, CanReadAs, Kind},
};
use keycloak::login::token_url;
use reqwest::StatusCode;
use serde::Deserialize;

use crate::{
    auth::{WorkflowAuth, auth0_token_body, auth0_token_url, truncate_for_log},
    canton_id::CantonId,
    config::{NodeConfig, PartyCredentials},
    error::Result,
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

/// Get frontend auth configuration (provider details + whether auth is required).
///
/// Auth0 takes precedence over Keycloak when both are configured — each node
/// operator is expected to set exactly one of `DECPM_AUTH0_*` or
/// `DECPM_KEYCLOAK_*`.
#[utoipa::path(
    tag = "Authentication",
    responses(
        (status = 200, description = "Auth configuration", body = AuthConfigResponse)
    )
)]
#[get("/auth-config")]
pub async fn get_auth_config(data: web::Data<AppState>) -> impl Responder {
    if data.test_mode {
        return HttpResponse::Ok().json(AuthConfigResponse {
            auth_required: false,
            keycloak_host: None,
            keycloak_realm: None,
            keycloak_client_id: None,
            auth0_domain: None,
            auth0_client_id: None,
            auth0_audience: None,
            auth0_scope: None,
        });
    }

    if let Some(config) = &data.config.auth0 {
        return HttpResponse::Ok().json(AuthConfigResponse {
            auth_required: true,
            keycloak_host: None,
            keycloak_realm: None,
            keycloak_client_id: None,
            auth0_domain: Some(config.domain.clone()),
            auth0_client_id: Some(config.client_id.clone()),
            auth0_audience: config.audience.clone(),
            auth0_scope: config.scope.clone(),
        });
    }

    match &data.config.keycloak {
        Some(config) => HttpResponse::Ok().json(AuthConfigResponse {
            auth_required: true,
            keycloak_host: Some(config.url.clone()),
            keycloak_realm: Some(config.realm.clone()),
            keycloak_client_id: Some(config.client_id.clone()),
            auth0_domain: None,
            auth0_client_id: None,
            auth0_audience: None,
            auth0_scope: None,
        }),
        None => HttpResponse::Ok().json(AuthConfigResponse {
            auth_required: false,
            keycloak_host: None,
            keycloak_realm: None,
            keycloak_client_id: None,
            auth0_domain: None,
            auth0_client_id: None,
            auth0_audience: None,
            auth0_scope: None,
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
        let party_creds_list = data.party_credentials.read().await;
        if party_creds_list.is_empty() {
            // No party configured yet: surface the mock registry's member party
            // as a placeholder so the UI has something to show.
            let manager = mock_registry.get_by_str("").await;
            let mock_member = manager.member_party_id().clone();
            party_statuses.push(PartyAuthStatus {
                dec_party_id: mock_member.clone(),
                member_party_id: mock_member,
                user_id: manager.user_id().to_string(),
                keycloak_url: None,
                keycloak_realm: None,
                auth0_domain: None,
                auth0_audience: None,
                status: AuthStatus::Mock,
                rights: None,
            });
        } else {
            // Test mode mints a token for any configured party, so report each
            // configured party as Mock-authenticated with the full canned rights
            // (matching the mock `grant_rights` path). Lets the UI recognise the
            // real dec party — the hardcoded placeholder above never matched it,
            // so auth-gated actions (member-party discovery, contract deploy)
            // stayed blocked even after `/party-config` succeeded.
            for creds in party_creds_list.iter() {
                party_statuses.push(PartyAuthStatus {
                    dec_party_id: creds.dec_party_id.clone(),
                    member_party_id: creds.member_party_id.clone(),
                    user_id: creds.user_id.clone(),
                    keycloak_url: None,
                    keycloak_realm: None,
                    auth0_domain: None,
                    auth0_audience: None,
                    status: AuthStatus::Mock,
                    rights: Some(RightsStatus {
                        member_party_act_as: true,
                        member_party_read_as: true,
                        dec_party_act_as: true,
                        dec_party_read_as: true,
                    }),
                });
            }
        }
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

        let (kc_url, kc_realm, auth0_domain, auth0_audience) =
            if let Some(ref a) = party_creds.auth0 {
                (None, None, Some(a.domain.clone()), Some(a.audience.clone()))
            } else {
                (
                    Some(party_creds.keycloak.url.clone()),
                    Some(party_creds.keycloak.realm.clone()),
                    None,
                    None,
                )
            };
        party_statuses.push(PartyAuthStatus {
            dec_party_id,
            member_party_id,
            user_id,
            keycloak_url: kc_url,
            keycloak_realm: kc_realm,
            auth0_domain,
            auth0_audience,
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

        // Attempt fresh authentication — Auth0 path when configured, else Keycloak.
        let result = if let Some(ref auth0) = party_creds.auth0 {
            crate::auth::auth0_client_credentials(&data.http_client, auth0)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        } else {
            test_keycloak_auth(&party_creds.keycloak).await
        };

        results.push(AuthTestResult {
            party_id: dec_party_id,
            success: result.is_ok(),
            error: result.err(),
        });
    }

    HttpResponse::Ok().json(AuthTestResponse { results })
}

/// Where the grant-rights admin token gets minted, resolved from whichever IdP
/// the party is actually configured with.
///
/// `/auth/test` and the workflow token managers already branch this way; the
/// grant-rights mint was the one path that assumed Keycloak, and an Auth0 party
/// leaves `PartyCredentials.keycloak` empty (#259).
#[derive(Debug)]
enum AdminTokenSource {
    /// Auth0 `client_credentials`. Auth0 requires an explicit `audience`, and
    /// the admin token is presented to the same ledger API as the party's own
    /// token — Canton validates `aud` against its configured target audience —
    /// so the party's audience is the one that can work.
    Auth0 { domain: String, audience: String },
    /// Keycloak `client_credentials` against the realm's token endpoint.
    Keycloak { url: String, realm: String },
}

impl AdminTokenSource {
    /// Mirrors the precedence the rest of the file uses: Auth0 when the party
    /// has a tenant, Keycloak otherwise.
    ///
    /// # Errors
    ///
    /// Returns a message naming what is missing when the party carries neither
    /// a usable Auth0 tenant nor a Keycloak realm. Previously such a party
    /// produced a relative token URL that failed inside reqwest as an opaque
    /// "builder error".
    fn from_party(creds: &PartyCredentials) -> std::result::Result<Self, String> {
        if let Some(auth0) = &creds.auth0 {
            let domain = auth0.domain.trim();
            let audience = auth0.audience.trim();
            if domain.is_empty() || audience.is_empty() {
                return Err(format!(
                    "Party {party} has an incomplete Auth0 configuration \
                     (domain{domain_state}, audience{audience_state}); \
                     re-enter the party credentials before granting rights",
                    party = creds.dec_party_id,
                    domain_state = if domain.is_empty() {
                        " missing"
                    } else {
                        " set"
                    },
                    audience_state = if audience.is_empty() {
                        " missing"
                    } else {
                        " set"
                    },
                ));
            }
            return Ok(Self::Auth0 {
                domain: domain.to_string(),
                audience: audience.to_string(),
            });
        }

        let url = creds.keycloak.url.trim();
        let realm = creds.keycloak.realm.trim();
        if url.is_empty() || realm.is_empty() {
            return Err(format!(
                "Party {party} has neither an Auth0 tenant nor a Keycloak realm configured, \
                 so there is no token endpoint to mint an admin token from",
                party = creds.dec_party_id,
            ));
        }
        Ok(Self::Keycloak {
            url: url.to_string(),
            realm: realm.to_string(),
        })
    }

    /// The absolute `client_credentials` token endpoint for this source.
    fn token_endpoint(&self) -> String {
        match self {
            Self::Auth0 { domain, .. } => auth0_token_url(domain),
            Self::Keycloak { url, realm } => token_url(url, realm),
        }
    }

    /// IdP name for operator-facing messages, so a failed mint says which
    /// system rejected the credentials.
    fn idp(&self) -> &'static str {
        match self {
            Self::Auth0 { .. } => "Auth0",
            Self::Keycloak { .. } => "Keycloak",
        }
    }
}

/// Why an admin token mint failed.
#[derive(Debug, thiserror::Error)]
enum AdminMintError {
    /// The IdP answered 400, 401 or 403: the supplied client id or secret is wrong.
    #[error("{message}")]
    Rejected { status: u16, message: String },
    /// The IdP answered 429. `retry_after` is its `Retry-After` header, if any.
    #[error("{message}")]
    RateLimited {
        retry_after: Option<String>,
        message: String,
    },
    /// The IdP was unreachable, failed, or sent a response we cannot parse.
    #[error("{message}")]
    Upstream {
        status: Option<u16>,
        message: String,
    },
}

impl AdminMintError {
    fn kind(&self) -> &'static str {
        match self {
            Self::Rejected { .. } => "rejected",
            Self::RateLimited { .. } | Self::Upstream { .. } => "upstream",
        }
    }

    fn status(&self) -> Option<u16> {
        match self {
            Self::Rejected { status, .. } => Some(*status),
            Self::RateLimited { .. } => Some(StatusCode::TOO_MANY_REQUESTS.as_u16()),
            Self::Upstream { status, .. } => *status,
        }
    }
}

#[derive(Deserialize)]
struct AdminTokenResponse {
    access_token: String,
}

/// Mint an admin access token at `token_url` in the request shape of `source`,
/// using the operator-supplied client credentials.
///
/// # Errors
///
/// Returns [`AdminMintError::Rejected`] when the IdP refuses the credentials and
/// [`AdminMintError::Upstream`] for transport failures, other error statuses and
/// unparseable responses.
async fn mint_admin_token(
    http: &reqwest::Client,
    token_url: &str,
    source: &AdminTokenSource,
    client_id: &str,
    client_secret: &str,
) -> std::result::Result<String, AdminMintError> {
    let request = http.post(token_url);
    let request = match source {
        AdminTokenSource::Auth0 { audience, .. } => {
            request.json(&auth0_token_body(client_id, client_secret, audience))
        }
        AdminTokenSource::Keycloak { .. } => request.form(&[
            ("grant_type", "client_credentials"),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ]),
    };

    let response = request.send().await.map_err(|e| AdminMintError::Upstream {
        status: None,
        message: format!("Token request failed: {e}"),
    })?;
    let status = response.status();
    if !status.is_success() {
        let retry_after = response
            .headers()
            .get(RETRY_AFTER.as_str())
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = response.text().await.unwrap_or_default();
        let message = format!(
            "Token endpoint returned {status}: {body}",
            body = truncate_for_log(&body),
        );
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(AdminMintError::RateLimited {
                retry_after,
                message,
            });
        }
        let status = status.as_u16();
        return Err(if matches!(status, 400 | 401 | 403) {
            AdminMintError::Rejected { status, message }
        } else {
            AdminMintError::Upstream {
                status: Some(status),
                message,
            }
        });
    }

    response
        .json::<AdminTokenResponse>()
        .await
        .map(|token| token.access_token)
        .map_err(|e| AdminMintError::Upstream {
            status: Some(status.as_u16()),
            message: format!("Token response parse failed: {e}"),
        })
}

/// Grant actAs + readAs rights on the member party and the dec party to the
/// configured coordinator user, using the participant admin API
#[utoipa::path(
    tag = "Authentication",
    request_body = GrantRightsRequest,
    responses(
        (status = 200, description = "Rights granted; current rights returned", body = GrantRightsResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Missing or invalid session token", body = ErrorResponse),
        (status = 403, description = "Caller is not an admin", body = ErrorResponse),
        (status = 404, description = "Party not configured", body = ErrorResponse),
        (status = 422, description = "Admin credentials rejected by the IdP", body = ErrorResponse),
        (status = 500, description = "Grant failed", body = ErrorResponse),
        (status = 502, description = "IdP unreachable or failed", body = ErrorResponse),
        (status = 503, description = "IdP rate-limited the admin token mint; retry after the \
            `Retry-After` header when present", body = ErrorResponse)
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
    let admin_source = match AdminTokenSource::from_party(party_creds) {
        Ok(source) => source,
        Err(error) => {
            tracing::warn!("Cannot mint a grant-rights admin token: {error}");
            drop(party_creds_list);
            drop(auth);
            return HttpResponse::BadRequest().json(ErrorResponse { error });
        }
    };

    drop(party_creds_list);
    drop(auth);

    let admin_token = match mint_admin_token(
        &data.http_client,
        &admin_source.token_endpoint(),
        &admin_source,
        &admin_client_id,
        &admin_client_secret,
    )
    .await
    {
        Ok(token) => token,
        Err(e) => {
            // The error carries the IdP response body, so keep it in the logs
            // and return a generic message that cannot reflect secrets.
            let idp = admin_source.idp();
            tracing::warn!(
                kind = e.kind(),
                idp,
                status = e.status(),
                party = %dec_party_id,
                "Failed to mint admin token for grant-rights: {e}"
            );
            return match e {
                AdminMintError::Rejected { .. } => {
                    HttpResponse::UnprocessableEntity().json(ErrorResponse {
                        error: format!("Admin {idp} auth failed"),
                    })
                }
                AdminMintError::RateLimited { retry_after, .. } => {
                    let mut response = HttpResponse::ServiceUnavailable();
                    if let Some(retry_after) = retry_after {
                        response.insert_header((RETRY_AFTER, retry_after));
                    }
                    response.json(ErrorResponse {
                        error: format!("Admin {idp} token endpoint is rate-limiting requests"),
                    })
                }
                AdminMintError::Upstream { .. } => HttpResponse::BadGateway().json(ErrorResponse {
                    error: format!("Admin {idp} token endpoint failed"),
                }),
            };
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
    // Exactly the rights this dec party needs, and nothing wider: actAs + readAs
    // on the member party and on the dec party.
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
    let url = keycloak::login::token_url(&config.url, &config.realm);

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
        http::{
            StatusCode,
            header::{AUTHORIZATION, HeaderMap, RETRY_AFTER},
        },
        test::{self, TestRequest},
        web::Data,
    };
    use serde_json::{Value, json};
    use sqlx::SqlitePool;
    use tokio::sync::{Mutex, RwLock};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, body_string, header, method, path, path_regex},
    };

    use super::{AdminMintError, AdminTokenSource, grant_rights, mint_admin_token};
    use crate::{
        auth::{AuthRegistry, MockAuthRegistry, MockValidator, TokenValidator, WorkflowAuth},
        canton_id::CantonId,
        config::{
            Auth0Config, Auth0M2MConfig, KeycloakConfig, NodeConfig, PackageConfig,
            PartyCredentials,
        },
        server::{AppState, middleware::AuthMiddleware},
    };

    /// Build an `AppState` configured for handler-level tests:
    /// - in-memory sqlite (no migrations needed for grant_rights paths)
    /// - `MockValidator` accepts any token, mints an "admin"-roled principal
    /// - `WorkflowAuth::Mock` so `grant_rights` hits its test-mode short-circuit
    /// - `admin_role` is configurable so the require-admin gate can be exercised
    async fn build_state(admin_role: Option<&str>) -> Data<AppState> {
        build_state_with(NodeConfig::default(), admin_role, true).await
    }

    async fn build_state_with(
        config: NodeConfig,
        admin_role: Option<&str>,
        test_mode: bool,
    ) -> Data<AppState> {
        let db = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        let party_credentials = Arc::new(RwLock::new(Vec::new()));
        Data::new(AppState {
            db,
            config,
            peer_status: Arc::new(RwLock::new(HashMap::new())),
            last_seen: Arc::new(RwLock::new(HashMap::new())),
            peer_job_sender: tokio::sync::mpsc::unbounded_channel().0,
            workflows: crate::server::WorkflowRegistry::new(),
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
            relay_sessions: Arc::new(
                crate::workflow::party_replication::relay::RelaySessions::new(),
            ),
            test_mode,
            refreshing_prefixes: Arc::new(RwLock::new(HashSet::new())),
            discovery_permits: Arc::new(tokio::sync::Semaphore::new(
                crate::server::handlers::MAX_CONCURRENT_DISCOVERIES,
            )),
            discovery_generations: Arc::new(RwLock::new(HashMap::new())),
            discovery_completed: Arc::new(RwLock::new(HashMap::new())),
            http_client: reqwest::Client::new(),
            health_cache: crate::server::HealthCache::new(),
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

    const AUTH0_DOMAIN: &str = "bitsafe-test.eu.auth0.com";
    const LEDGER_AUDIENCE: &str = "https://canton.network.global";

    /// Party credentials as they look once the operator picks Auth0: `auth0`
    /// carries the tenant, and `keycloak` stays at its `#[serde(default)]`
    /// empty shape because nothing fills it in.
    fn auth0_party() -> PartyCredentials {
        PartyCredentials {
            dec_party_id: CantonId::parse(VALID_CANTON_ID).expect("valid dec party id"),
            member_party_id: CantonId::parse(VALID_CANTON_ID).expect("valid member party id"),
            user_id: "attestor-1".to_string(),
            keycloak: KeycloakConfig::default(),
            auth0: Some(Auth0M2MConfig {
                domain: AUTH0_DOMAIN.to_string(),
                audience: LEDGER_AUDIENCE.to_string(),
                client_id: "party-m2m".to_string(),
                client_secret: "party-secret".to_string(),
            }),
            packages: PackageConfig::default(),
        }
    }

    /// A Keycloak-configured party: `auth0` unset, realm details filled in.
    fn keycloak_party() -> PartyCredentials {
        PartyCredentials {
            keycloak: KeycloakConfig {
                url: "https://keycloak.example.com".to_string(),
                realm: "decman".to_string(),
                client_id: "party-client".to_string(),
                ..KeycloakConfig::default()
            },
            auth0: None,
            ..auth0_party()
        }
    }

    fn endpoint_of(creds: &PartyCredentials) -> String {
        match AdminTokenSource::from_party(creds) {
            Ok(source) => source.token_endpoint(),
            Err(e) => panic!("expected a usable admin token source, got: {e}"),
        }
    }

    /// #259: the endpoint used to be built from the party's Keycloak config
    /// unconditionally, so an Auth0 party produced the relative
    /// `/realms//protocol/openid-connect/token`. reqwest refuses to send a
    /// relative URL, which is the operator-visible "Keycloak
    /// client_credentials login request error: builder error" — and the admin
    /// client id and secret from the dialog never left the process.
    ///
    /// The admin token has to come from whichever IdP the party is actually
    /// configured with, so the endpoint must be absolute and must point there.
    #[test]
    fn admin_token_endpoint_for_an_auth0_party_targets_auth0() {
        let creds = auth0_party();
        let endpoint = endpoint_of(&creds);

        let parsed = match reqwest::Url::parse(&endpoint) {
            Ok(url) => url,
            Err(e) => panic!("admin-token endpoint {endpoint:?} is not a usable URL: {e}"),
        };
        assert_eq!(
            parsed.host_str(),
            Some(AUTH0_DOMAIN),
            "admin token must be minted from the party's own IdP, got {endpoint}"
        );
        assert_eq!(parsed.path(), "/oauth/token");
    }

    /// Auth0 rejects a `client_credentials` request without an `audience`, and
    /// the admin token is presented to the same ledger API as the party's own
    /// token — so it has to carry the party's audience.
    #[test]
    fn an_auth0_admin_token_carries_the_party_audience() {
        match AdminTokenSource::from_party(&auth0_party()) {
            Ok(AdminTokenSource::Auth0 { audience, domain }) => {
                assert_eq!(audience, LEDGER_AUDIENCE);
                assert_eq!(domain, AUTH0_DOMAIN);
            }
            other => panic!("expected an Auth0 source, got {other:?}"),
        }
    }

    /// The Keycloak path is the one that worked before and must be untouched.
    #[test]
    fn a_keycloak_party_still_mints_from_its_realm() {
        assert_eq!(
            endpoint_of(&keycloak_party()),
            "https://keycloak.example.com/realms/decman/protocol/openid-connect/token"
        );
    }

    /// A party with neither IdP configured used to reach reqwest as a relative
    /// URL and fail as an opaque "builder error". Name the gap instead.
    #[test]
    fn a_party_with_no_idp_is_rejected_by_name() {
        let creds = PartyCredentials {
            keycloak: KeycloakConfig::default(),
            auth0: None,
            ..auth0_party()
        };

        let message = match AdminTokenSource::from_party(&creds) {
            Ok(source) => panic!(
                "expected no token source, got {endpoint}",
                endpoint = source.token_endpoint()
            ),
            Err(e) => e,
        };
        assert!(
            message.contains("neither an Auth0 tenant nor a Keycloak realm"),
            "unhelpful error: {message}"
        );
    }

    /// Half-filled Auth0 credentials would otherwise mint against
    /// `https:///oauth/token` or send an empty audience Auth0 rejects.
    #[test]
    fn an_incomplete_auth0_config_is_rejected_by_name() {
        let mut creds = auth0_party();
        if let Some(auth0) = creds.auth0.as_mut() {
            auth0.audience = String::new();
        }

        let message = match AdminTokenSource::from_party(&creds) {
            Ok(source) => panic!(
                "expected no token source, got {endpoint}",
                endpoint = source.token_endpoint()
            ),
            Err(e) => e,
        };
        assert!(
            message.contains("audience missing"),
            "unhelpful error: {message}"
        );
    }

    const KEYCLOAK_TOKEN_PATH: &str = "/realms/decman/protocol/openid-connect/token";

    /// The Keycloak mint must stay a form-encoded `client_credentials` request;
    /// the mock only answers the exact form, so any drift fails the mint.
    #[tokio::test]
    async fn keycloak_admin_mint_sends_the_client_credentials_form() -> anyhow::Result<()> {
        let idp = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(KEYCLOAK_TOKEN_PATH))
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .and(body_string(
                "grant_type=client_credentials&client_id=validator-admin&client_secret=admin-secret",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"access_token": "admin-token", "expires_in": 300})),
            )
            .expect(1)
            .mount(&idp)
            .await;
        let mut party = keycloak_party();
        party.keycloak.url = idp.uri();
        let source = AdminTokenSource::from_party(&party).map_err(anyhow::Error::msg)?;

        let token = mint_admin_token(
            &reqwest::Client::new(),
            &source.token_endpoint(),
            &source,
            "validator-admin",
            "admin-secret",
        )
        .await?;

        assert_eq!(token, "admin-token");
        Ok(())
    }

    /// Auth0 takes JSON, not a form, and refuses a request without `audience`.
    #[tokio::test]
    async fn auth0_admin_mint_sends_the_client_credentials_json() -> anyhow::Result<()> {
        let idp = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(header("content-type", "application/json"))
            .and(body_json(json!({
                "grant_type": "client_credentials",
                "client_id": "validator-admin",
                "client_secret": "admin-secret",
                "audience": LEDGER_AUDIENCE,
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"access_token": "admin-token", "expires_in": 300})),
            )
            .expect(1)
            .mount(&idp)
            .await;
        let source = AdminTokenSource::from_party(&auth0_party()).map_err(anyhow::Error::msg)?;

        let token = mint_admin_token(
            &reqwest::Client::new(),
            &format!("{base}/oauth/token", base = idp.uri()),
            &source,
            "validator-admin",
            "admin-secret",
        )
        .await?;

        assert_eq!(token, "admin-token");
        Ok(())
    }

    #[tokio::test]
    async fn admin_mint_keeps_the_idp_retry_after() -> anyhow::Result<()> {
        let idp = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(KEYCLOAK_TOKEN_PATH))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "30"))
            .mount(&idp)
            .await;
        let mut party = keycloak_party();
        party.keycloak.url = idp.uri();
        let source = AdminTokenSource::from_party(&party).map_err(anyhow::Error::msg)?;

        let result = mint_admin_token(
            &reqwest::Client::new(),
            &source.token_endpoint(),
            &source,
            "validator-admin",
            "admin-secret",
        )
        .await;

        match result {
            Err(AdminMintError::RateLimited { retry_after, .. }) => {
                assert_eq!(retry_after.as_deref(), Some("30"));
            }
            other => panic!("expected a rate-limit error, got {other:?}"),
        }
        Ok(())
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

    /// Call grant-rights on a Keycloak party whose token endpoint issues the
    /// party token, then answers the admin mint with `admin_mint`.
    async fn grant_rights_with_admin_mint(
        admin_mint: ResponseTemplate,
    ) -> anyhow::Result<(StatusCode, HeaderMap, Value)> {
        let keycloak = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r".*/token$"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"access_token": "party-token", "expires_in": 300})),
            )
            .mount(&keycloak)
            .await;

        let mut party = keycloak_party();
        party.keycloak.url = keycloak.uri();
        party.keycloak.client_secret = Some("party-secret".to_string());
        let registry = AuthRegistry::new(std::slice::from_ref(&party)).await?;

        keycloak.reset().await;
        Mock::given(method("POST"))
            .and(path_regex(r".*/token$"))
            .respond_with(admin_mint)
            .mount(&keycloak)
            .await;

        let state = build_state_with(NodeConfig::default(), None, false).await;
        *state.auth.write().await = Some(WorkflowAuth::Keycloak(Arc::new(registry)));
        state.party_credentials.write().await.push(party);
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
                "admin_client_secret": "wrong-secret",
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        let status = resp.status();
        let headers = resp.headers().clone();
        let body: Value = test::read_body_json(resp).await;
        Ok((status, headers, body))
    }

    /// #421: a rejected admin client id or secret is a problem with the
    /// request body, not with the caller's session. A 401 here made the UI
    /// treat it as an expired session and log the operator out.
    #[actix_web::test]
    async fn grant_rights_rejects_bad_admin_credentials_without_401() -> anyhow::Result<()> {
        let (status, _, body) = grant_rights_with_admin_mint(
            ResponseTemplate::new(401).set_body_json(json!({"error": "unauthorized_client"})),
        )
        .await?;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"], "Admin Keycloak auth failed");
        Ok(())
    }

    #[actix_web::test]
    async fn grant_rights_reports_idp_server_error_as_bad_gateway() -> anyhow::Result<()> {
        let (status, _, body) = grant_rights_with_admin_mint(ResponseTemplate::new(503)).await?;

        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["error"], "Admin Keycloak token endpoint failed");
        Ok(())
    }

    #[actix_web::test]
    async fn grant_rights_reports_malformed_token_response_as_bad_gateway() -> anyhow::Result<()> {
        let (status, _, body) =
            grant_rights_with_admin_mint(ResponseTemplate::new(200).set_body_string("not json"))
                .await?;

        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["error"], "Admin Keycloak token endpoint failed");
        Ok(())
    }

    /// Retry-After is only a hint; the caller has to see it to back off.
    #[actix_web::test]
    async fn grant_rights_reports_idp_rate_limit_as_unavailable() -> anyhow::Result<()> {
        let (status, headers, body) = grant_rights_with_admin_mint(
            ResponseTemplate::new(429).insert_header("Retry-After", "30"),
        )
        .await?;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            headers.get(RETRY_AFTER).map(|v| v.as_bytes()),
            Some(b"30".as_slice())
        );
        assert_eq!(
            body["error"],
            "Admin Keycloak token endpoint is rate-limiting requests"
        );
        Ok(())
    }

    #[actix_web::test]
    async fn grant_rights_rate_limit_without_retry_after_sends_none() -> anyhow::Result<()> {
        let (status, headers, _) = grant_rights_with_admin_mint(ResponseTemplate::new(429)).await?;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(headers.get(RETRY_AFTER).is_none());
        Ok(())
    }

    fn auth0_config(scope: Option<&str>) -> NodeConfig {
        let mut config = NodeConfig::default();
        config.auth0 = Some(Auth0Config {
            domain: "tenant.eu.auth0.com".to_string(),
            client_id: "spa-client-id".to_string(),
            audience: Some("https://decman-api.example".to_string()),
            scope: scope.map(str::to_string),
        });
        config
    }

    /// The SPA cannot ask Auth0 for an RBAC-granted admin scope unless the
    /// backend names it, because `auth0-spa-js` only sends what it is given.
    /// `/auth-config` is the only channel that carries deploy-time env vars to
    /// the browser, so the configured scope has to survive the round trip.
    #[actix_web::test]
    async fn auth_config_surfaces_the_configured_auth0_scope() {
        let state = build_state_with(auth0_config(Some("decman-admin")), None, false).await;
        let app =
            test::init_service(App::new().app_data(state).service(super::get_auth_config)).await;
        let req = TestRequest::get().uri("/auth-config").to_request();
        let body: Value = test::call_and_read_body_json(&app, req).await;

        assert_eq!(body["auth0_scope"], "decman-admin");
        assert_eq!(body["auth0_domain"], "tenant.eu.auth0.com");
        assert_eq!(body["auth_required"], true);
    }

    /// An operator who sets no scope must get no `scope` key at all, so the
    /// SPA falls through to `auth0-spa-js`'s own "openid profile email"
    /// default instead of requesting the empty string.
    #[actix_web::test]
    async fn auth_config_omits_auth0_scope_when_unset() {
        let state = build_state_with(auth0_config(None), None, false).await;
        let app =
            test::init_service(App::new().app_data(state).service(super::get_auth_config)).await;
        let req = TestRequest::get().uri("/auth-config").to_request();
        let body: Value = test::call_and_read_body_json(&app, req).await;

        assert!(
            body.get("auth0_scope").is_none(),
            "auth0_scope should be omitted when unset, got {body}"
        );
        assert_eq!(body["auth0_domain"], "tenant.eu.auth0.com");
    }
}
