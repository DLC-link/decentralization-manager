use std::sync::Arc;

use actix_web::{HttpRequest, HttpResponse, Responder, get, put, web};
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
        types::{ErrorResponse, PartyConfigRequest, PartyConfigResponse, SuccessResponse},
    },
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
            member_party_id: c.member_party_id.clone(),
            user_id: c.user_id.clone(),
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
                dec_party_id: dec_party_id.clone(),
                member_party_id: dec_party_id,
                user_id: "CoordinatorUser".to_string(),
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
    if !is_fresh && let Err(resp) = require_admin(&http_req, &data.admin_role) {
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
