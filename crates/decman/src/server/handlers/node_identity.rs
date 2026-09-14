//! `GET`/`PUT /node-identity` (design D1) and `GET /registry` (design D3).
//!
//! The node identity is a `party_credentials` row with `kind = 'node'`. The
//! PUT is admin-gated, and exempt from authentication only while the table is
//! entirely empty (the same predicate and mutex as `PUT /party-config`, in
//! the auth middleware). Before persisting, the handler reads the head
//! `PartyToParticipant` of the node party and refuses unless this participant
//! hosts it with Submission permission: a Confirmation-only host cannot
//! submit Daml commands, so it cannot serve as a node party.

use actix_web::{HttpRequest, HttpResponse, Responder, get, put, web};
use common::coordination::{NodeIdentityRequest, NodeIdentityResponse, RegistryResponse};

use crate::{
    config::{
        Auth0M2MConfig, CredentialKind, KeycloakConfig, PartyCredentials, default_package_config,
    },
    db::schema::{Commitable, SchemaWrite},
    onledger::{identity::node_credentials, verify_hosting},
    server::{
        AppState, handlers::party_config::reload_auth, middleware::require_admin,
        types::ErrorResponse,
    },
};

/// Read the node identity and how this participant hosts it.
#[utoipa::path(
    tag = "Node identity",
    responses(
        (status = 200, description = "Node identity (or `configured: false`)", body = NodeIdentityResponse),
        (status = 500, description = "Participant id not resolved", body = ErrorResponse)
    )
)]
#[get("/node-identity")]
pub async fn get_node_identity(http_req: HttpRequest, data: web::Data<AppState>) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let Some(participant_id) = data.config.node.participant_id.clone() else {
        return HttpResponse::InternalServerError().json(ErrorResponse {
            error: "participant id not resolved yet".to_string(),
        });
    };
    let node_row = {
        let rows = data.party_credentials.read().await;
        node_credentials(&rows).cloned()
    };
    let Some(row) = node_row else {
        return HttpResponse::Ok().json(NodeIdentityResponse {
            configured: false,
            node_party_id: None,
            participant_id,
            user_id: None,
            hosting_permission: None,
        });
    };
    let hosting_permission =
        match verify_hosting(&data.config, &row.member_party_id, &participant_id).await {
            Ok(check) => check.permission,
            Err(e) => {
                tracing::warn!(error = %e, "hosting check for the node party failed");
                None
            }
        };
    HttpResponse::Ok().json(NodeIdentityResponse {
        configured: true,
        node_party_id: Some(row.member_party_id.clone()),
        participant_id,
        user_id: Some(row.user_id.clone()),
        hosting_permission,
    })
}

/// Set or replace the node identity.
#[utoipa::path(
    tag = "Node identity",
    request_body = NodeIdentityRequest,
    responses(
        (status = 200, description = "Node identity saved", body = NodeIdentityResponse),
        (status = 400, description = "Bad request, or the party is not hosted here with Submission", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[put("/node-identity")]
pub async fn save_node_identity(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<NodeIdentityRequest>,
) -> impl Responder {
    let req = body.into_inner();

    // Bootstrap exemption: the middleware lets the first PUT on a fresh node
    // through unauthenticated. Once any credential row exists, admin only.
    let is_fresh = data.party_credentials.read().await.is_empty();
    if !is_fresh && let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }

    let Some(participant_id) = data.config.node.participant_id.clone() else {
        return HttpResponse::InternalServerError().json(ErrorResponse {
            error: "participant id not resolved yet".to_string(),
        });
    };

    // The one trust check (design D1): this participant hosts the node party
    // with Submission permission in the synchronizer head state.
    match verify_hosting(&data.config, &req.node_party_id, &participant_id).await {
        Ok(check) if check.has_submission() => {}
        Ok(check) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!(
                    "participant {participant_id} does not host {} with Submission permission \
                     ({}); allocate the node party on this participant with Submission \
                     permission first",
                    req.node_party_id,
                    check.describe()
                ),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("failed to read the node party's hosting from topology: {e}"),
            });
        }
    }

    let existing = {
        let rows = data.party_credentials.read().await;
        node_credentials(&rows).cloned()
    };
    let creds = match build_credentials(&req, existing.as_ref(), data.test_mode) {
        Ok(c) => c,
        Err(message) => {
            return HttpResponse::BadRequest().json(ErrorResponse { error: message });
        }
    };

    // Persist: one node row at a time. A node party change deletes the old row.
    {
        let mut tx = match data.db.begin_transaction().await {
            Ok(tx) => tx,
            Err(e) => {
                return HttpResponse::InternalServerError().json(ErrorResponse {
                    error: format!("Failed to begin transaction: {e}"),
                });
            }
        };
        if let Some(old) = existing
            .as_ref()
            .filter(|old| old.dec_party_id != creds.dec_party_id)
            && let Err(e) = tx.delete_party_credentials(&old.dec_party_id).await
        {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to remove the previous node identity: {e}"),
            });
        }
        if let Err(e) = tx.upsert_party_credentials(&creds).await {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to save node identity: {e}"),
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
        pc.retain(|p| p.kind != CredentialKind::Node);
        pc.push(creds.clone());
    }

    if !data.test_mode
        && let Err(e) = reload_auth(&data.party_credentials, &data.auth).await
    {
        tracing::warn!("Failed to reinitialize auth registry: {e}");
    }
    if let Err(e) = data.onledger.reload_identity().await {
        tracing::warn!(error = %e, "node identity saved but failed to load");
    }

    HttpResponse::Ok().json(NodeIdentityResponse {
        configured: true,
        node_party_id: Some(creds.member_party_id.clone()),
        participant_id,
        user_id: Some(creds.user_id.clone()),
        hosting_permission: Some(common::types::Permission::Submission),
    })
}

/// The registry as this node sees it: own entry, configured peers, and
/// inbound entries from operators that added this node first.
#[utoipa::path(
    tag = "Node identity",
    responses(
        (status = 200, description = "Registry view", body = RegistryResponse),
        (status = 409, description = "No node identity configured", body = ErrorResponse),
        (status = 500, description = "Ledger or topology read failed", body = ErrorResponse)
    )
)]
#[get("/registry")]
pub async fn get_registry(http_req: HttpRequest, data: web::Data<AppState>) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    if data.onledger.identity().await.is_none() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: "node identity not configured; set one with PUT /node-identity first"
                .to_string(),
        });
    }
    match data.onledger.read_registry().await {
        Ok(view) => HttpResponse::Ok().json(view),
        Err(e) => {
            tracing::error!(error = %e, "GET /registry failed");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("failed to read the registry: {e}"),
            })
        }
    }
}

/// Turn the request into the `kind = 'node'` row, with the same secret-merge
/// and IdP-change rules `PUT /party-config` applies: an omitted secret keeps
/// the stored one, an empty one clears it, and a changed IdP host never
/// inherits a stored secret.
fn build_credentials(
    req: &NodeIdentityRequest,
    existing: Option<&PartyCredentials>,
    test_mode: bool,
) -> Result<PartyCredentials, String> {
    let same_party = existing.filter(|e| e.member_party_id == req.node_party_id);
    let existing_keycloak = same_party.map(|e| e.keycloak.clone());
    let existing_auth0 = same_party.and_then(|e| e.auth0.clone());
    let present = |s: &Option<String>| s.as_deref().is_some_and(|v| !v.is_empty());

    let base = |keycloak: KeycloakConfig, auth0: Option<Auth0M2MConfig>| PartyCredentials {
        kind: CredentialKind::Node,
        dec_party_id: req.node_party_id.clone(),
        member_party_id: req.node_party_id.clone(),
        user_id: req.user_id.clone(),
        keycloak,
        auth0,
        packages: default_package_config(),
    };

    if let Some(domain) = req.auth0_domain.as_deref().filter(|s| !s.is_empty()) {
        let Some(audience) = req.auth0_audience.as_deref().filter(|s| !s.is_empty()) else {
            return Err("auth0_audience is required".to_string());
        };
        let Some(client_id) = req.auth0_client_id.as_deref().filter(|s| !s.is_empty()) else {
            return Err("auth0_client_id is required".to_string());
        };
        let domain_changed = existing_auth0.as_ref().is_some_and(|e| e.domain != domain);
        if domain_changed && !present(&req.auth0_client_secret) {
            return Err(
                "auth0_domain changed; resubmit with a fresh auth0_client_secret \
                        in the same request"
                    .to_string(),
            );
        }
        let client_secret = match req.auth0_client_secret.as_deref().filter(|s| !s.is_empty()) {
            Some(s) => s.to_string(),
            None => existing_auth0
                .as_ref()
                .map(|a| a.client_secret.clone())
                .unwrap_or_default(),
        };
        if client_secret.is_empty() {
            return Err("auth0_client_secret is required for first-time Auth0 setup".to_string());
        }
        return Ok(base(
            KeycloakConfig::default(),
            Some(Auth0M2MConfig {
                domain: domain.to_string(),
                audience: audience.to_string(),
                client_id: client_id.to_string(),
                client_secret,
            }),
        ));
    }

    if !test_mode
        && (req.keycloak_url.trim().is_empty()
            || req.keycloak_realm.trim().is_empty()
            || req.keycloak_client_id.trim().is_empty())
    {
        return Err(
            "keycloak_url, keycloak_realm, and keycloak_client_id are required \
                    (or supply auth0_domain to use the Auth0 path)"
                .to_string(),
        );
    }

    let url_changed = existing_keycloak
        .as_ref()
        .is_some_and(|e| e.url != req.keycloak_url);
    let fresh_password_pair = present(&req.keycloak_username) && present(&req.keycloak_password);
    if url_changed && !present(&req.keycloak_client_secret) && !fresh_password_pair {
        return Err(
            "keycloak_url changed; resubmit with a fresh keycloak_client_secret \
                    (or keycloak_username + keycloak_password) in the same request"
                .to_string(),
        );
    }

    let carry = |new: &Option<String>, old: Option<String>| -> Option<String> {
        if url_changed {
            return new.clone().filter(|s| !s.is_empty());
        }
        match new {
            None => old,
            Some(v) if v.is_empty() => None,
            Some(v) => Some(v.clone()),
        }
    };
    let keycloak = KeycloakConfig {
        url: req.keycloak_url.clone(),
        internal_url: None,
        realm: req.keycloak_realm.clone(),
        client_id: req.keycloak_client_id.clone(),
        client_secret: carry(
            &req.keycloak_client_secret,
            existing_keycloak
                .as_ref()
                .and_then(|k| k.client_secret.clone()),
        ),
        username: carry(
            &req.keycloak_username,
            existing_keycloak.as_ref().and_then(|k| k.username.clone()),
        ),
        password: carry(
            &req.keycloak_password,
            existing_keycloak.as_ref().and_then(|k| k.password.clone()),
        ),
    };
    Ok(base(keycloak, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canton_id::CantonId;

    const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

    fn node_party() -> CantonId {
        CantonId::parse(&format!("node::{NS}")).expect("valid id")
    }

    fn keycloak_request(secret: Option<&str>) -> NodeIdentityRequest {
        NodeIdentityRequest {
            node_party_id: node_party(),
            user_id: "node-user".into(),
            keycloak_url: "https://kc.example.com".into(),
            keycloak_realm: "decman".into(),
            keycloak_client_id: "node-client".into(),
            keycloak_client_secret: secret.map(str::to_string),
            keycloak_username: None,
            keycloak_password: None,
            auth0_domain: None,
            auth0_audience: None,
            auth0_client_id: None,
            auth0_client_secret: None,
        }
    }

    #[test]
    fn a_node_row_stores_the_node_party_in_both_columns() {
        let creds = build_credentials(&keycloak_request(Some("s3cret")), None, false).expect("ok");
        assert_eq!(creds.kind, CredentialKind::Node);
        assert_eq!(creds.dec_party_id, node_party());
        assert_eq!(creds.member_party_id, node_party());
        assert_eq!(creds.keycloak.client_secret.as_deref(), Some("s3cret"));
    }

    #[test]
    fn an_omitted_secret_keeps_the_stored_one_for_the_same_idp() {
        let existing =
            build_credentials(&keycloak_request(Some("s3cret")), None, false).expect("ok");
        let updated =
            build_credentials(&keycloak_request(None), Some(&existing), false).expect("ok");
        assert_eq!(updated.keycloak.client_secret.as_deref(), Some("s3cret"));
        let cleared =
            build_credentials(&keycloak_request(Some("")), Some(&existing), false).expect("ok");
        assert_eq!(cleared.keycloak.client_secret, None);
    }

    #[test]
    fn a_changed_keycloak_url_never_inherits_the_secret() {
        let existing =
            build_credentials(&keycloak_request(Some("s3cret")), None, false).expect("ok");
        let mut moved = keycloak_request(None);
        moved.keycloak_url = "https://attacker.example.com".into();
        let err = build_credentials(&moved, Some(&existing), false).expect_err("must refuse");
        assert!(err.contains("keycloak_url changed"), "{err}");
    }

    #[test]
    fn a_missing_keycloak_realm_is_rejected_outside_test_mode() {
        let mut req = keycloak_request(Some("s"));
        req.keycloak_realm = String::new();
        assert!(build_credentials(&req, None, false).is_err());
        assert!(build_credentials(&req, None, true).is_ok());
    }

    #[test]
    fn auth0_requires_a_secret_on_first_setup() {
        let mut req = keycloak_request(None);
        req.auth0_domain = Some("tenant.eu.auth0.com".into());
        req.auth0_audience = Some("https://canton.network.global".into());
        req.auth0_client_id = Some("m2m".into());
        let err = build_credentials(&req, None, false).expect_err("no secret");
        assert!(err.contains("auth0_client_secret"), "{err}");
        req.auth0_client_secret = Some("top".into());
        let creds = build_credentials(&req, None, false).expect("ok");
        assert_eq!(
            creds.auth0.map(|a| a.client_secret),
            Some("top".to_string())
        );
    }
}
