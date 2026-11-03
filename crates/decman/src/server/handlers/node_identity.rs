use std::{sync::Arc, time::Duration};

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use sqlx::SqlitePool;

use super::auth::{
    AdminMintError, AdminTokenSource, extract_user_id_from_jwt, mint_admin_token,
    mint_error_response, right_act_as, right_read_as,
};
use crate::{
    auth::{AuthError, MockTokenManager, NodeIdentity, NodeTokenSource, WorkflowAuth},
    canton_id::CantonId,
    config::{Auth0M2MConfig, IdpClient, KeycloakConfig, NodeConfig, NodeIdentityRecord, Peer},
    consts::topology_propagation_delay_secs,
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
    server::{
        AppState,
        middleware::require_admin,
        types::{
            ErrorResponse, NodeIdentityPeer, NodeIdentityRequest, NodeIdentityResponse,
            NodeIdentityStatusResponse, NodeLoginIdp, NodeLoginStatus, NodeLoginSummary,
            StoredNodeIdentity,
        },
    },
    workflow::node_identity::{
        NODE_PARTY_PREFIX, allocate_node_party, create_user_if_missing, grant_user_rights,
        list_user_rights, namespace_matches, node_party_id, rights_flags, scan_node_parties,
        verify_hosting, wait_for_submission,
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

/// The logins one setup call uses.
#[allow(clippy::large_enum_variant)]
enum Logins {
    /// Insecure mode presents the unsafe token for both logins.
    Unsafe(Arc<MockTokenManager>),
    /// The node login, and the admin client at the same IdP.
    Idp {
        node: IdpClient,
        admin_client_id: String,
        admin_client_secret: String,
    },
}

/// The tokens and user one setup call works with.
struct Tokens {
    admin_token: String,
    /// The Ledger API user the node login authenticates as.
    user_id: String,
}

fn error_response(mut builder: actix_web::HttpResponseBuilder, error: String) -> HttpResponse {
    builder.json(ErrorResponse { error })
}

fn bad_request(error: impl Into<String>) -> HttpResponse {
    error_response(HttpResponse::BadRequest(), error.into())
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

fn stored_identity_unreadable(e: anyhow::Error) -> HttpResponse {
    tracing::warn!("Failed to load the stored node identity: {e:#}");
    error_response(
        HttpResponse::InternalServerError(),
        "Failed to load the stored node identity".to_string(),
    )
}

fn idp_name(login: &IdpClient) -> &'static str {
    match login {
        IdpClient::Keycloak(_) => "Keycloak",
        IdpClient::Auth0(_) => "Auth0",
    }
}

/// `login` without its secret.
fn login_summary(login: &IdpClient) -> NodeLoginSummary {
    match login {
        IdpClient::Keycloak(k) => NodeLoginSummary {
            idp: NodeLoginIdp::Keycloak,
            client_id: k.client_id.clone(),
            keycloak_url: Some(k.url.clone()),
            keycloak_realm: Some(k.realm.clone()),
            auth0_domain: None,
            auth0_audience: None,
        },
        IdpClient::Auth0(a) => NodeLoginSummary {
            idp: NodeLoginIdp::Auth0,
            client_id: a.client_id.clone(),
            keycloak_url: None,
            keycloak_realm: None,
            auth0_domain: Some(a.domain.clone()),
            auth0_audience: Some(a.audience.clone()),
        },
    }
}

/// A request field, trimmed. Empty counts as omitted.
fn given(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

/// A secret from the request. Empty counts as omitted.
fn given_secret(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|v| !v.is_empty())
}

/// The request's value, or the stored one when the request omits it.
fn merged(value: &Option<String>, stored: Option<&str>) -> Option<String> {
    given(value)
        .or(stored.filter(|v| !v.is_empty()))
        .map(str::to_string)
}

/// The login a setup request asks for: its fields over the stored login of
/// the same IdP. A stored secret never moves to a new URL or domain, because
/// the next login would send it there.
fn requested_login(
    body: &NodeIdentityRequest,
    stored: Option<&IdpClient>,
) -> std::result::Result<IdpClient, String> {
    let keycloak_given = [
        &body.keycloak_url,
        &body.keycloak_realm,
        &body.keycloak_client_id,
        &body.keycloak_client_secret,
    ]
    .into_iter()
    .any(|v| given(v).is_some());
    let auth0_given = [
        &body.auth0_domain,
        &body.auth0_audience,
        &body.auth0_client_id,
        &body.auth0_client_secret,
    ]
    .into_iter()
    .any(|v| given(v).is_some());
    let use_auth0 = match (keycloak_given, auth0_given, stored) {
        (true, true, _) => {
            return Err("Set either the Keycloak or the Auth0 fields, not both".to_string());
        }
        (false, true, _) | (false, false, Some(IdpClient::Auth0(_))) => true,
        (true, false, _) | (false, false, Some(IdpClient::Keycloak(_))) => false,
        (false, false, None) => {
            return Err("Set the Keycloak or the Auth0 fields of the node login".to_string());
        }
    };

    if use_auth0 {
        let stored = match stored {
            Some(IdpClient::Auth0(a)) => Some(a),
            _ => None,
        };
        let domain = merged(&body.auth0_domain, stored.map(|a| a.domain.as_str()));
        let new_secret = given_secret(&body.auth0_client_secret);
        if stored.is_some_and(|a| Some(&a.domain) != domain.as_ref()) && new_secret.is_none() {
            return Err("auth0_domain changed; resubmit with a fresh \
                        auth0_client_secret in the same request"
                .to_string());
        }
        let login = (
            domain,
            merged(&body.auth0_audience, stored.map(|a| a.audience.as_str())),
            merged(&body.auth0_client_id, stored.map(|a| a.client_id.as_str())),
            new_secret
                .or(stored.map(|a| a.client_secret.as_str()))
                .map(str::to_string),
        );
        let (Some(domain), Some(audience), Some(client_id), Some(client_secret)) = login else {
            return Err("auth0_domain, auth0_audience, auth0_client_id and \
                        auth0_client_secret are required"
                .to_string());
        };
        return Ok(IdpClient::Auth0(Auth0M2MConfig {
            domain,
            audience,
            client_id,
            client_secret,
        }));
    }

    let stored = match stored {
        Some(IdpClient::Keycloak(k)) => Some(k),
        _ => None,
    };
    let url = merged(&body.keycloak_url, stored.map(|k| k.url.as_str()));
    let new_secret = given_secret(&body.keycloak_client_secret);
    if stored.is_some_and(|k| Some(&k.url) != url.as_ref()) && new_secret.is_none() {
        return Err("keycloak_url changed; resubmit with a fresh \
                    keycloak_client_secret in the same request"
            .to_string());
    }
    let login = (
        url,
        merged(&body.keycloak_realm, stored.map(|k| k.realm.as_str())),
        merged(
            &body.keycloak_client_id,
            stored.map(|k| k.client_id.as_str()),
        ),
        new_secret
            .or(stored.and_then(|k| k.client_secret.as_deref()))
            .map(str::to_string),
    );
    let (Some(url), Some(realm), Some(client_id), Some(client_secret)) = login else {
        return Err("keycloak_url, keycloak_realm, keycloak_client_id and \
                    keycloak_client_secret are required"
            .to_string());
    };
    Ok(IdpClient::Keycloak(KeycloakConfig {
        url,
        realm,
        client_id,
        client_secret: Some(client_secret),
        ..KeycloakConfig::default()
    }))
}

/// The admin client id and secret of a setup request.
#[allow(clippy::result_large_err)]
fn admin_credentials(
    body: &NodeIdentityRequest,
) -> std::result::Result<(&str, &str), HttpResponse> {
    match (
        given(&body.admin_client_id),
        given_secret(&body.admin_client_secret),
    ) {
        (Some(id), Some(secret)) => Ok((id, secret)),
        _ => Err(bad_request(
            "admin_client_id and admin_client_secret are required",
        )),
    }
}

/// Mint the node login's token and an admin token, both at the node login's
/// IdP. The node login's `sub` names its Ledger API user.
#[allow(clippy::result_large_err)]
async fn log_in(
    data: &AppState,
    login: &IdpClient,
    admin_client_id: &str,
    admin_client_secret: &str,
) -> std::result::Result<Tokens, HttpResponse> {
    let source = AdminTokenSource::from_idp(login)
        .map_err(|_| bad_request("The node login names no token endpoint"))?;
    let idp = source.idp();
    let endpoint = source.token_endpoint();
    let mint_failed = |who: &'static str| {
        move |e: AdminMintError| {
            tracing::warn!(
                kind = e.kind(),
                idp,
                status = e.status(),
                "Failed to mint the {who} token for node identity: {e}"
            );
            mint_error_response(e, who, idp)
        }
    };

    let node_token = mint_admin_token(
        &data.http_client,
        &endpoint,
        &source,
        login.client_id(),
        login.client_secret(),
    )
    .await
    .map_err(mint_failed("Node login"))?;
    let Some(user_id) = extract_user_id_from_jwt(&node_token) else {
        return Err(error_response(
            HttpResponse::UnprocessableEntity(),
            format!("The node login's {idp} token names no user (no `sub` claim)"),
        ));
    };
    let admin_token = mint_admin_token(
        &data.http_client,
        &endpoint,
        &source,
        admin_client_id,
        admin_client_secret,
    )
    .await
    .map_err(mint_failed("Admin"))?;
    Ok(Tokens {
        admin_token,
        user_id,
    })
}

/// Allocate the node party unless this participant already hosts it, then
/// report what the ledger shows. Grants no rights itself.
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

    Ok(NodeIdentityResponse {
        namespace_matches: namespace_matches(&node_party, participant_id),
        node_party_id: node_party,
        participant_id: participant_id.clone(),
        allocated,
        hosting_permission: hosting.permission,
        user_id: tokens.user_id.clone(),
        act_as,
        read_as,
        rights_granted: false,
        login: None,
    })
}

/// Give the login's user act-as and read-as on the node party. AllocateParty
/// grants act-as only to the user of the call that creates the party, so a
/// later login needs this: after a login change, a reset database, or a
/// retried setup.
#[allow(clippy::result_large_err)]
async fn grant_missing_rights(
    config: &NodeConfig,
    tokens: &Tokens,
    response: &mut NodeIdentityResponse,
) -> std::result::Result<(), HttpResponse> {
    let node_party = response.node_party_id.to_string();
    let granted = grant_user_rights(
        config,
        &tokens.admin_token,
        &tokens.user_id,
        vec![right_act_as(&node_party), right_read_as(&node_party)],
    )
    .await
    .map_err(canton_failed)?;
    tracing::info!(
        user_id = %tokens.user_id,
        granted,
        "Granted the node login act-as and read-as on the node party"
    );
    let rights = list_user_rights(config, &tokens.admin_token, &tokens.user_id)
        .await
        .map_err(canton_failed)?;
    (response.act_as, response.read_as) = rights_flags(&rights, &node_party);
    response.rights_granted = true;
    Ok(())
}

#[allow(clippy::result_large_err)]
async fn save(
    data: &AppState,
    record: &NodeIdentityRecord,
) -> std::result::Result<(), HttpResponse> {
    let saved = async {
        let mut tx = data.db.begin_transaction().await?;
        tx.upsert_node_identity(record).await?;
        Commitable::commit(tx).await
    };
    saved.await.map_err(|e| {
        tracing::warn!("Failed to save the node identity: {e:#}");
        error_response(
            HttpResponse::InternalServerError(),
            "Failed to save the node identity".to_string(),
        )
    })
}

/// Set up the node identity: log in, create the login's Ledger API user,
/// allocate the node party, then store the login and start using it.
#[allow(clippy::result_large_err)]
async fn set_up(
    data: &AppState,
    body: NodeIdentityRequest,
) -> std::result::Result<NodeIdentityResponse, HttpResponse> {
    let stored = data
        .db
        .get_node_identity()
        .await
        .map_err(stored_identity_unreadable)?;
    let logins = match &*data.auth.read().await {
        Some(WorkflowAuth::Mock(registry)) => Logins::Unsafe(registry.get_by_str("").await),
        _ => {
            let (admin_client_id, admin_client_secret) = admin_credentials(&body)?;
            let stored_login = stored.as_ref().and_then(|r| r.login.as_ref());
            Logins::Idp {
                node: requested_login(&body, stored_login).map_err(bad_request)?,
                admin_client_id: admin_client_id.to_string(),
                admin_client_secret: admin_client_secret.to_string(),
            }
        }
    };
    let Some(participant_id) = data.config.node.participant_id.clone() else {
        return Err(participant_unresolved());
    };
    let tokens = match &logins {
        Logins::Unsafe(mock) => Tokens {
            admin_token: mock.get_token(),
            user_id: mock.user_id().to_string(),
        },
        Logins::Idp {
            node,
            admin_client_id,
            admin_client_secret,
        } => log_in(data, node, admin_client_id, admin_client_secret).await?,
    };

    if create_user_if_missing(&data.config, &tokens.admin_token, &tokens.user_id)
        .await
        .map_err(canton_failed)?
    {
        tracing::info!(user_id = %tokens.user_id, "Created the node login's Ledger API user");
    }
    let mut response = allocate(&data.config, &tokens, &participant_id).await?;
    if let Some(previous) = stored.as_ref().filter(|r| r.user_id != tokens.user_id) {
        tracing::info!(
            previous_user = %previous.user_id,
            user_id = %tokens.user_id,
            node_party = %response.node_party_id,
            "The node login moved to another user. The previous user keeps its rights"
        );
    }
    if !response.act_as {
        grant_missing_rights(&data.config, &tokens, &mut response).await?;
    }

    let now = chrono::Utc::now().timestamp();
    let record = NodeIdentityRecord {
        node_party_id: response.node_party_id.clone(),
        participant_id,
        user_id: tokens.user_id.clone(),
        login: match &logins {
            Logins::Unsafe(_) => None,
            Logins::Idp { node, .. } => Some(node.clone()),
        },
        created_at: now,
        updated_at: now,
    };
    let identity = match &logins {
        Logins::Unsafe(mock) => NodeIdentity::mock(&record, mock.get_token()),
        Logins::Idp { node, .. } => NodeIdentity::connect(&record).await.map_err(|e| {
            tracing::warn!("The node login failed after its first mint: {e}");
            error_response(
                HttpResponse::BadGateway(),
                format!(
                    "Node login {idp} token endpoint failed",
                    idp = idp_name(node)
                ),
            )
        })?,
    };
    // Holding the slot across the save keeps the active login in step with
    // the stored one when two setups race.
    let mut slot = data.node_identity.write().await;
    save(data, &record).await?;
    *slot = Some(identity);
    drop(slot);

    response.login = record.login.as_ref().map(login_summary);
    tracing::info!(?response, "Node identity set up");
    Ok(response)
}

/// Set up this node's identity party and the login that acts as it.
///
/// The node login is one IdP client. Its token's `sub` names the Ledger API
/// user, which is created if missing. It gets act-as through AllocateParty, or
/// through a grant when the node party already exists.
/// The admin token comes from the same IdP. The login is stored; the admin
/// credentials are not.
#[utoipa::path(
    tag = "Node identity",
    request_body = NodeIdentityRequest,
    responses(
        (status = 200, description = "Node identity set up", body = NodeIdentityResponse),
        (status = 400, description = "Admin credentials or login fields missing, or a stored secret would move to a new URL", body = ErrorResponse),
        (status = 401, description = "Missing or invalid session token", body = ErrorResponse),
        (status = 403, description = "Caller is not an admin", body = ErrorResponse),
        (status = 422, description = "The IdP rejected the node login or the admin credentials", body = ErrorResponse),
        (status = 500, description = "Participant id unresolved, or a Canton call or the save failed", body = ErrorResponse),
        (status = 502, description = "IdP failed, or Canton allocated another party id", body = ErrorResponse),
        (status = 503, description = "IdP rate-limited a token mint", body = ErrorResponse)
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
    match set_up(&data, body.into_inner()).await {
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
        identity: None,
    })
}

fn login_failed(record: &NodeIdentityRecord, e: &AuthError) -> NodeLoginStatus {
    tracing::warn!(user_id = %record.user_id, "Node login failed: {e}");
    let error = match &record.login {
        Some(login) => format!(
            "The {idp} node login failed. The decman log has the IdP's answer",
            idp = idp_name(login)
        ),
        None => "No node login is stored".to_string(),
    };
    NodeLoginStatus::Failed { error }
}

/// Whether the node login yields a token now. A login that startup left
/// unloaded gets one more try here, and is kept when it works.
async fn login_status(data: &AppState, record: &NodeIdentityRecord) -> NodeLoginStatus {
    let loaded = data.node_identity.read().await.clone();
    let identity = match loaded {
        Some(identity) => identity,
        None => match NodeIdentity::connect(record).await {
            Ok(identity) => {
                let mut slot = data.node_identity.write().await;
                // A setup call that finished meanwhile holds the newer login.
                if slot.is_none() {
                    *slot = Some(identity.clone());
                }
                identity
            }
            Err(e) => return login_failed(record, &e),
        },
    };
    match &identity.tokens {
        NodeTokenSource::Mock(_) => NodeLoginStatus::Mock,
        NodeTokenSource::Idp(_) => match identity.token().await {
            Ok(_) => NodeLoginStatus::Ok,
            Err(e) => login_failed(record, &e),
        },
    }
}

async fn stored_summary(
    data: &AppState,
    record: NodeIdentityRecord,
    participant_id: &CantonId,
) -> StoredNodeIdentity {
    StoredNodeIdentity {
        participant_matches: &record.participant_id == participant_id,
        login: record.login.as_ref().map(login_summary),
        token: login_status(data, &record).await,
        node_party_id: record.node_party_id,
        participant_id: record.participant_id,
        user_id: record.user_id,
        updated_at: record.updated_at,
    }
}

/// Check this node's and every configured peer's identity party in this
/// node's topology, list every party the node-party prefix matches, and
/// report the stored identity with a live check of its login.
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
    let stored = match data.db.get_node_identity().await {
        Ok(stored) => stored,
        Err(e) => return stored_identity_unreadable(e),
    };
    let mut status = match identity_status(&data.config, &participant_id, peers).await {
        Ok(status) => status,
        Err(e) => return canton_failed(e),
    };
    if let Some(record) = stored {
        status.identity = Some(stored_summary(&data, record, &participant_id).await);
    }
    HttpResponse::Ok().json(status)
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
        matchers::{body_string, method, path_regex},
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

    const ADMIN: (&str, &str) = ("validator-admin", "admin-secret");

    /// A state outside insecure mode, on `pool`, with its participant id
    /// resolved, so a setup call gets as far as the IdP.
    fn secure_state(pool: SqlitePool) -> anyhow::Result<web::Data<AppState>> {
        let (state, _) = AppState::test_state(None, pool);
        let mut config = NodeConfig::default();
        config.node.participant_id = Some(CantonId::parse(&format!("participant1::{NS}"))?);
        Ok(web::Data::new(AppState { config, ..state }))
    }

    async fn post_setup(
        state: &web::Data<AppState>,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let app = test::init_service(
            App::new()
                .app_data(state.clone())
                .wrap(AuthMiddleware)
                .service(allocate_node_identity),
        )
        .await;
        let req = TestRequest::post()
            .uri("/node-identity")
            .insert_header((AUTHORIZATION, "Bearer any-token"))
            .set_json(&body)
            .to_request();
        let resp = test::call_service(&app, req).await;
        let status = resp.status();
        (status, test::read_body_json(resp).await)
    }

    /// A Keycloak setup body for `url`, with the admin client.
    fn keycloak_body(url: &str) -> serde_json::Value {
        json!({
            "admin_client_id": ADMIN.0,
            "admin_client_secret": ADMIN.1,
            "keycloak_url": url,
            "keycloak_realm": "canton",
            "keycloak_client_id": "node-client",
            "keycloak_client_secret": "node-secret",
        })
    }

    /// The form a Keycloak `client_credentials` mint sends.
    fn client_form(client_id: &str, client_secret: &str) -> String {
        format!("grant_type=client_credentials&client_id={client_id}&client_secret={client_secret}")
    }

    /// A JWT-shaped token whose payload names `sub`.
    fn token_for(sub: &str) -> String {
        use base64::Engine;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(json!({ "sub": sub }).to_string());
        format!("header.{payload}.signature")
    }

    async fn nothing_stored(state: &web::Data<AppState>) -> anyhow::Result<bool> {
        Ok(state.db.get_node_identity().await?.is_none()
            && state.node_identity.read().await.is_none())
    }

    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn setup_needs_admin_credentials_outside_insecure_mode(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let state = secure_state(pool)?;
        let mut only_secret = keycloak_body("https://kc.example");
        only_secret["admin_client_id"] = json!("");

        for body in [json!({}), json!({"admin_client_id": ADMIN.0}), only_secret] {
            let (status, answer) = post_setup(&state, body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "body {body}");
            assert_eq!(
                answer["error"], "admin_client_id and admin_client_secret are required",
                "body {body}"
            );
        }
        Ok(())
    }

    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn setup_needs_one_complete_login(pool: SqlitePool) -> anyhow::Result<()> {
        let state = secure_state(pool)?;
        let admin = json!({"admin_client_id": ADMIN.0, "admin_client_secret": ADMIN.1});
        let with = |fields: serde_json::Value| {
            let mut body = admin.clone();
            if let (Some(body), Some(fields)) = (body.as_object_mut(), fields.as_object()) {
                body.extend(fields.clone());
            }
            body
        };
        let cases = [
            (with(json!({})), "Set the Keycloak or the Auth0 fields"),
            (
                with(json!({"keycloak_url": "https://kc.example"})),
                "keycloak_url, keycloak_realm, keycloak_client_id and keycloak_client_secret \
                 are required",
            ),
            (
                with(json!({"auth0_domain": "tenant.us.auth0.com", "auth0_client_id": "m2m"})),
                "auth0_domain, auth0_audience, auth0_client_id and auth0_client_secret \
                 are required",
            ),
            (
                with(json!({"keycloak_url": "https://kc.example", "auth0_domain": "t.auth0.com"})),
                "Set either the Keycloak or the Auth0 fields, not both",
            ),
        ];
        for (body, error) in cases {
            let (status, answer) = post_setup(&state, body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "body {body}");
            assert!(
                answer["error"]
                    .as_str()
                    .is_some_and(|e| e.starts_with(error)),
                "body {body} answered {answer}"
            );
        }
        Ok(())
    }

    /// A refused node login is a problem with the request body, not with the
    /// caller's session, so it must not be a 401.
    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn a_refused_node_login_is_422_and_stores_nothing(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let idp = keycloak_answering(
            ResponseTemplate::new(401).set_body_json(json!({"error": "unauthorized_client"})),
        )
        .await;
        let state = secure_state(pool)?;

        let (status, answer) = post_setup(&state, keycloak_body(&idp.uri())).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(answer["error"], "Node login Keycloak auth failed");
        assert!(nothing_stored(&state).await?);
        Ok(())
    }

    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn refused_admin_credentials_are_422_and_store_nothing(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let idp = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string(client_form("node-client", "node-secret")))
            .respond_with(token_response(&token_for("node-user")))
            .expect(1)
            .mount(&idp)
            .await;
        Mock::given(method("POST"))
            .and(body_string(client_form(ADMIN.0, ADMIN.1)))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&idp)
            .await;
        let state = secure_state(pool)?;

        let (status, answer) = post_setup(&state, keycloak_body(&idp.uri())).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(answer["error"], "Admin Keycloak auth failed");
        assert!(nothing_stored(&state).await?);
        Ok(())
    }

    /// Canton takes the token's `sub` as the user id, so a token without one
    /// cannot act as any user.
    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn a_node_token_without_a_subject_is_422(pool: SqlitePool) -> anyhow::Result<()> {
        let idp = keycloak_answering(token_response("opaque-token")).await;
        let state = secure_state(pool)?;

        let (status, answer) = post_setup(&state, keycloak_body(&idp.uri())).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            answer["error"]
                .as_str()
                .is_some_and(|e| e.contains("no `sub` claim")),
            "{answer}"
        );
        assert!(nothing_stored(&state).await?);
        Ok(())
    }

    /// The stored secret must never reach a new URL, because the URL comes
    /// from the request. The new IdP sees no request at all.
    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn a_new_url_without_a_new_secret_is_refused(pool: SqlitePool) -> anyhow::Result<()> {
        let new_idp = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(token_response(&token_for("node-user")))
            .expect(0)
            .mount(&new_idp)
            .await;
        save(
            &pool,
            &stored_identity(Some(keycloak_login("https://kc.example")))?,
        )
        .await?;
        let state = secure_state(pool)?;
        let mut body = keycloak_body(&new_idp.uri());
        body["keycloak_client_secret"] = json!(null);

        let (status, answer) = post_setup(&state, body).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            answer["error"]
                .as_str()
                .is_some_and(|e| e.starts_with("keycloak_url changed")),
            "{answer}"
        );
        Ok(())
    }

    /// A setup call with only the admin client logs in with the stored login,
    /// secret included.
    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn an_omitted_login_reuses_the_stored_one(pool: SqlitePool) -> anyhow::Result<()> {
        let idp = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(
                r"^/realms/canton/protocol/openid-connect/token$",
            ))
            .and(body_string(client_form("node-client", "node-secret")))
            .respond_with(token_response(&token_for("node-user")))
            .expect(1)
            .mount(&idp)
            .await;
        Mock::given(method("POST"))
            .and(body_string(client_form(ADMIN.0, ADMIN.1)))
            .respond_with(ResponseTemplate::new(401))
            .mount(&idp)
            .await;
        save(&pool, &stored_identity(Some(keycloak_login(&idp.uri())))?).await?;
        let state = secure_state(pool)?;

        let (status, answer) = post_setup(
            &state,
            json!({"admin_client_id": ADMIN.0, "admin_client_secret": ADMIN.1}),
        )
        .await;

        // The admin mint is the first step after the node login.
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(answer["error"], "Admin Keycloak auth failed");
        Ok(())
    }

    fn auth0_login(domain: &str, secret: &str) -> IdpClient {
        IdpClient::Auth0(Auth0M2MConfig {
            domain: domain.to_string(),
            audience: "https://canton.network.global".to_string(),
            client_id: "node-m2m".to_string(),
            client_secret: secret.to_string(),
        })
    }

    #[test]
    fn requested_login_keeps_what_the_request_omits() -> anyhow::Result<()> {
        let stored = auth0_login("tenant.us.auth0.com", "stored-secret");
        let body = NodeIdentityRequest {
            auth0_client_id: Some("rotated-m2m".to_string()),
            ..NodeIdentityRequest::default()
        };

        let login = requested_login(&body, Some(&stored)).map_err(anyhow::Error::msg)?;

        let IdpClient::Auth0(a) = login else {
            panic!("expected an Auth0 login, got {login:?}");
        };
        assert_eq!(a.domain, "tenant.us.auth0.com");
        assert_eq!(a.client_id, "rotated-m2m");
        assert_eq!(a.client_secret, "stored-secret");
        Ok(())
    }

    #[test]
    fn a_new_auth0_domain_needs_a_new_secret() {
        let stored = auth0_login("tenant.us.auth0.com", "stored-secret");
        let body = NodeIdentityRequest {
            auth0_domain: Some("attacker.example".to_string()),
            ..NodeIdentityRequest::default()
        };

        let result = requested_login(&body, Some(&stored));

        assert!(
            result
                .as_ref()
                .is_err_and(|e| e.starts_with("auth0_domain changed")),
            "{result:?}"
        );
    }

    /// A secret stored for one IdP is never sent to the other.
    #[test]
    fn switching_idp_needs_a_new_secret() {
        let stored = auth0_login("tenant.us.auth0.com", "stored-secret");
        let body = NodeIdentityRequest {
            keycloak_url: Some("https://kc.example".to_string()),
            keycloak_realm: Some("canton".to_string()),
            keycloak_client_id: Some("node-client".to_string()),
            ..NodeIdentityRequest::default()
        };

        let result = requested_login(&body, Some(&stored));

        assert!(
            result.as_ref().is_err_and(|e| e.ends_with("are required")),
            "{result:?}"
        );
    }

    #[test]
    fn admin_credentials_keep_the_secret_as_sent() {
        let body = NodeIdentityRequest {
            admin_client_id: Some(" validator-admin ".to_string()),
            admin_client_secret: Some(" s3cret ".to_string()),
            ..NodeIdentityRequest::default()
        };

        let credentials = admin_credentials(&body).ok();

        assert_eq!(credentials, Some(("validator-admin", " s3cret ")));
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

    /// Startup left the login unloaded. The status check tries it again and
    /// keeps it once it works.
    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn the_status_check_retries_an_unloaded_login(pool: SqlitePool) -> anyhow::Result<()> {
        let idp = keycloak_answering(token_response("node-token")).await;
        let record = stored_identity(Some(keycloak_login(&idp.uri())))?;
        let state = secure_state(pool)?;

        assert_eq!(login_status(&state, &record).await, NodeLoginStatus::Ok);
        let loaded = state.node_identity.read().await.clone();
        assert_eq!(
            loaded.context("the login was not kept")?.token().await?,
            "node-token"
        );
        Ok(())
    }

    /// The IdP's answer stays in the log. The status names only the IdP.
    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn the_status_check_reports_a_refused_login(pool: SqlitePool) -> anyhow::Result<()> {
        let idp = keycloak_answering(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error": "client node-client is disabled"})),
        )
        .await;
        let record = stored_identity(Some(keycloak_login(&idp.uri())))?;
        let state = secure_state(pool)?;

        let status = login_status(&state, &record).await;

        let NodeLoginStatus::Failed { error } = status else {
            panic!("expected a failed login, got {status:?}");
        };
        assert_eq!(
            error,
            "The Keycloak node login failed. The decman log has the IdP's answer"
        );
        assert!(state.node_identity.read().await.is_none());
        Ok(())
    }

    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn the_status_check_reports_the_unsafe_token(pool: SqlitePool) -> anyhow::Result<()> {
        let record = stored_identity(None)?;
        let state = secure_state(pool)?;
        *state.node_identity.write().await = Some(NodeIdentity::mock(&record, "t".to_string()));

        assert_eq!(login_status(&state, &record).await, NodeLoginStatus::Mock);
        Ok(())
    }
}
