//! The rotate-key workflow: start, status, cancel and invites.

use std::collections::{BTreeSet, HashSet};

use actix_web::{HttpRequest, HttpResponse, Responder, get, post, web};
use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::crypto::admin::v30::vault_service_client::VaultServiceClient;
use sqlx::SqlitePool;
use tonic::transport;

use super::{
    cancel::cancel_workflow_state,
    invites::{InviteLabels, Recipients, invite_log, send_invites},
    launch::{CoordinatorRequest, RunEnd, StatusOrder, launch_coordinator, run_log},
    preflight::{
        claim_party_for_start, format_incompatible_peers, incompatible_versions, lacking_feature,
        probe_invitee_health,
    },
    runs::{kind_status, now_secs, register_and_persist},
};
use crate::{
    canton_id::{CantonId, Fingerprint},
    config::NodeConfig,
    consts::ROTATE_KEY,
    db::schema::SchemaRead,
    error::Result,
    noise::{Message, MessageType},
    server::{
        AppState,
        error::ApiError,
        middleware::require_admin,
        types::{
            ErrorResponse, MessageResponse, RotateKeyInvitePayload, RotateKeyRequest,
            WorkflowInstance, WorkflowKind, WorkflowProgress, WorkflowResponse, WorkflowRole,
            WorkflowStatusResponse,
        },
    },
    signing::custody,
    utils,
    workflow::{self, RotateKeyStep},
};

// ============================================================================
// Rotate-Key Workflow
// ============================================================================

/// Start a rotate-key workflow: replace this node's Daml signing key for a
/// decentralized party with a new key.
///
/// The vault must keep its keys in KMS, unless this node sets
/// `DECPM_ALLOW_SOFTWARE_PARTY_KEYS` or runs in insecure mode. The members
/// and thresholds stay the same, and every member signs.
#[utoipa::path(
    tag = "Workflows",
    request_body = RotateKeyRequest,
    responses(
        (status = 202, description = "Rotate-key workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request, such as a participant without KMS and without the opt-out", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress, stale membership, a member on an older build or not a configured peer, or this node's key is not the party's", body = ErrorResponse),
        (status = 502, description = "Canton's admin API could not be reached", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/rotate-key")]
pub async fn start_rotate_key(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<RotateKeyRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    start_rotate_key_run(&data, &body)
        .await
        .unwrap_or_else(ApiError::into_response)
}

/// Check a rotate-key request against the cached party, the vault and the
/// chain, then register the run and start it.
async fn start_rotate_key_run(
    data: &web::Data<AppState>,
    body: &RotateKeyRequest,
) -> Result<HttpResponse, ApiError> {
    let party = body.decentralized_party_id.clone();
    tracing::info!("Rotate-key request received: party={party}");

    let members: HashSet<CantonId> = data
        .db
        .get_dec_party_participants(&party)
        .await
        .map_err(|e| {
            tracing::error!("Failed to load dec party participants for rotate-key: {e}");
            ApiError::Internal("Failed to load decentralized party members".to_string())
        })?
        .iter()
        .filter_map(|r| CantonId::parse(&r.participant_uid).ok())
        .collect();
    if members.is_empty() {
        return Err(ApiError::Conflict(format!(
            "No cached membership for {party}. Try refreshing /decentralized-parties first."
        )));
    }
    if !members.contains(data.config.participant_id()) {
        return Err(ApiError::BadRequest(format!(
            "This node does not host {party}, so it holds no Daml key of it to rotate"
        )));
    }
    if members.len() < 2 {
        return Err(ApiError::BadRequest(format!(
            "Cannot rotate a key of {party}: it has only one member"
        )));
    }

    if let Err(e) = require_kms_for_rotation(&data.config, &data.db).await {
        let error = format!("{e:#}");
        return Err(if canton_unreachable(&e) {
            ApiError::BadGateway(error)
        } else {
            ApiError::BadRequest(error)
        });
    }
    let rotation = rotation_baseline(&data.config, &data.db, &party, &members)
        .await
        .map_err(|e| ApiError::Conflict(format!("{e:#}")))?;
    #[cfg(feature = "test-mode")]
    let rotation = match injected_rotated_key().await {
        Some(rotated_key) => RotationBaseline {
            rotated_key,
            ..rotation
        },
        None => rotation,
    };

    let invitees: Vec<CantonId> = data
        .db
        .get_all_peers()
        .await
        .map_err(|e| {
            tracing::error!("Failed to load peers for rotate-key: {e}");
            ApiError::Internal("Failed to load peers".to_string())
        })?
        .into_iter()
        .map(|p| p.participant_id)
        .filter(|p| members.contains(p) && p != data.config.participant_id())
        .collect();
    // Every other member must record the new key. A member that is not a
    // configured peer would keep the old key on record for this node.
    let unreachable = members_missing_from_peers(&members, &invitees, data.config.participant_id());
    if !unreachable.is_empty() {
        return Err(ApiError::Conflict(format!(
            "Cannot rotate this node's key of {party}: every other member must record the new \
             key, but these members are not configured peers: {list}. Add them as peers first",
            list = unreachable.join(", ")
        )));
    }

    let instance_name = format!("{}-rotate-key-{}", party.prefix, now_secs());
    let instance = WorkflowInstance::new(
        instance_name.clone(),
        WorkflowKind::RotateKey,
        WorkflowRole::Coordinator,
    );
    let rotate_config = workflow::RotateKeyConfig {
        decentralized_party_id: party.clone(),
        rotated_key: rotation.rotated_key,
        threshold: rotation.threshold,
        confirmation_threshold: rotation.confirmation_threshold,
        instance_name: instance_name.clone(),
    };

    let _start = claim_party_for_start(data, &party).await?;
    let health = probe_invitee_health(&data.config, &data.db, &invitees).await;
    let incompatible = incompatible_versions(&health);
    if !incompatible.is_empty() {
        return Err(ApiError::Conflict(format_incompatible_peers(&incompatible)));
    }
    // An older peer cannot decode the invitation, so the run would stall
    // after the invites went out.
    let lacking = lacking_feature(&health, ROTATE_KEY);
    if !lacking.is_empty() {
        return Err(ApiError::Conflict(format_peers_lacking_rotate_key(
            &lacking,
        )));
    }
    register_and_persist(
        data,
        &instance,
        RotateKeyStep::WaitingForPeers,
        &rotate_config,
        &invitees,
        Some(party.clone()),
    )
    .await?;

    let state = &instance.http;
    *state.invited_peers.write().await = invitees.clone();

    let invites = {
        let config = data.config.clone();
        let db = data.db.clone();
        let rotate_config = rotate_config.clone();
        Box::pin(
            async move { send_rotate_key_invites(&config, &db, &rotate_config, &invitees).await },
        )
    };
    launch_coordinator(
        data,
        &instance,
        CoordinatorRequest::RotateKey(rotate_config),
        None,
        invites,
        RunEnd {
            order: StatusOrder::RowFirstLockingEachAlone,
            log: run_log!(),
            after_success: None,
        },
    )
    .await;

    Ok(HttpResponse::Accepted().json(WorkflowResponse {
        status: WorkflowProgress::InProgress,
        message: "Rotate-key workflow started".to_string(),
        instance_name,
    }))
}

/// Get the current status of the rotate-key workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Rotate-key workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/rotate-key/status")]
pub async fn get_rotate_key_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(kind_status(&data, WorkflowKind::RotateKey).await)
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/rotate-key/cancel")]
pub async fn cancel_rotate_key(http_req: HttpRequest, data: web::Data<AppState>) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(&data, "Rotate-key", WorkflowKind::RotateKey).await
}

/// Refuse a rotation before any invitation goes out when the new key could
/// not live in KMS.
async fn require_kms_for_rotation(config: &NodeConfig, db: &SqlitePool) -> Result {
    let mut vault_client = VaultServiceClient::new(config.admin_channel().await?);
    custody::require_kms_participant(config, db, &mut vault_client).await
}

/// Localnet-only fault: name another key as the one this node replaces, so
/// the e2e suite can show that the peers refuse the proposal. A per-node file
/// lets the harness set the fault without a restart. The release binary does
/// not compile this hook.
#[cfg(feature = "test-mode")]
async fn injected_rotated_key() -> Option<Fingerprint> {
    let path = std::env::var_os("DECPM_TEST_ROTATE_KEY_REPLACE_FILE")?;
    let fingerprint: Fingerprint = tokio::fs::read_to_string(path)
        .await
        .ok()?
        .trim()
        .parse()
        .ok()?;
    tracing::warn!("Test fault: this rotation names {fingerprint} as the key it replaces");
    Some(fingerprint)
}

/// What a rotation pins when it starts.
struct RotationBaseline {
    rotated_key: Fingerprint,
    threshold: i32,
    confirmation_threshold: i32,
}

/// Whether an error came from reaching Canton, not from a rule refusal.
fn canton_unreachable(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<tonic::Status>() || cause.is::<transport::Error>())
}

/// The hosts that differ between the cached members and the chain, or `None`
/// when both name the same hosts.
fn stale_members(cached: &HashSet<CantonId>, on_chain: &[String]) -> Option<String> {
    let cached: BTreeSet<String> = cached.iter().map(ToString::to_string).collect();
    let on_chain: BTreeSet<String> = on_chain.iter().cloned().collect();
    (cached != on_chain).then(|| format!("cache {cached:?}, chain {on_chain:?}"))
}

/// Read this node's current Daml key for `party` and the party's thresholds.
///
/// The invitations go to the cached members, and the peers compare the
/// proposal with the chain. So the cache must name the chain's hosts, or the
/// run would fail late, after it created a key.
///
/// # Errors
///
/// Errors when the topology cannot be read, when the cache names other hosts
/// than the chain, when this node's key is unknown, or when the party does
/// not carry it.
async fn rotation_baseline(
    config: &NodeConfig,
    db: &SqlitePool,
    party: &CantonId,
    members: &HashSet<CantonId>,
) -> Result<RotationBaseline> {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let (_, p2p) =
        workflow::topology::fetch_p2p_mapping_at_head(config, &synchronizer_id, party).await?;
    let hosts: Vec<String> = p2p
        .participants
        .iter()
        .map(|p| p.participant_uid.clone())
        .collect();
    if let Some(difference) = stale_members(members, &hosts) {
        anyhow::bail!(
            "This node's cached members of {party} differ from the chain ({difference}). \
             Refresh /decentralized-parties and retry"
        );
    }

    workflow::rotate_key::config::require_complete_inline_keys(&p2p, party)?;
    let own = workflow::contracts::steps::sign::own_daml_key(config, db, party).await?;
    let mut keys = p2p
        .party_signing_keys
        .as_ref()
        .map(|k| k.keys.clone())
        .unwrap_or_default();
    let legacy = if keys.len() < p2p.participants.len() {
        workflow::topology::fetch_party_to_key_mapping(config, &synchronizer_id, party).await?
    } else {
        None
    };
    keys.extend(
        legacy
            .as_ref()
            .map(|mapping| mapping.signing_keys.clone())
            .unwrap_or_default(),
    );
    let signing_threshold =
        workflow::rotate_key::config::signing_threshold_in_force(&p2p, legacy.as_ref())
            .ok_or_else(|| anyhow::anyhow!("{party} carries no signing keys to rotate"))?;
    if !keys
        .iter()
        .any(|key| utils::compute_fingerprint(key) == own.fingerprint)
    {
        anyhow::bail!(
            "This node's Daml key {fingerprint} is not among {party}'s signing keys, so there \
             is no key of its to rotate",
            fingerprint = own.fingerprint
        );
    }

    Ok(RotationBaseline {
        rotated_key: own.fingerprint,
        threshold: i32::try_from(signing_threshold)?,
        confirmation_threshold: i32::try_from(p2p.threshold)?,
    })
}

/// The rotate-key invite. Its member set is the invitees and then this
/// node.
pub(super) fn rotate_key_invite(
    coordinator: &CantonId,
    rotate_config: &workflow::RotateKeyConfig,
    invitees: &[CantonId],
) -> Result<Message> {
    let mut participants = invitees.to_vec();
    participants.push(coordinator.clone());
    let payload = RotateKeyInvitePayload {
        dec_party_id: rotate_config.decentralized_party_id.clone(),
        rotated_key: rotate_config.rotated_key.to_string(),
        threshold: rotate_config.threshold,
        confirmation_threshold: rotate_config.confirmation_threshold,
        participants,
        workflow_instance: Some(rotate_config.instance_name.clone()),
    };
    Ok(Message::new(
        MessageType::InviteRotateKey,
        serde_json::to_vec(&payload).context("encode RotateKeyInvitePayload")?,
    ))
}

/// Send rotate-key invites over Noise to every other member.
async fn send_rotate_key_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    rotate_config: &workflow::RotateKeyConfig,
    invitees: &[CantonId],
) -> Result {
    send_invites(
        config,
        db,
        Recipients::Members(invitees),
        InviteLabels {
            kind: "rotate-key",
            skip: "rotate-key ",
            failed: "rotate-key ",
            address: false,
            empty_key_warning: false,
        },
        invite_log!(),
        || rotate_key_invite(config.participant_id(), rotate_config, invitees),
    )
    .await
}

/// Operator-facing 409 message naming each peer that cannot take part in a
/// key rotation.
fn format_peers_lacking_rotate_key(lacking: &[(String, String)]) -> String {
    let list = lacking
        .iter()
        .map(|(id, reason)| format!("{id} ({reason})"))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "Cannot start the key rotation: every other member must run a DecMan version that \
         knows the rotate-key workflow, and older builds cannot read its invitation. Blocking \
         peer(s): {list}. Upgrade them and retry."
    )
}

/// The members, other than this node, that are not among the invitees.
fn members_missing_from_peers(
    members: &HashSet<CantonId>,
    invitees: &[CantonId],
    own_id: &CantonId,
) -> Vec<String> {
    let mut missing: Vec<String> = members
        .iter()
        .filter(|m| *m != own_id && !invitees.contains(m))
        .map(ToString::to_string)
        .collect();
    missing.sort();
    missing
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use actix_web::{App, HttpMessage, http::StatusCode, test as actix_test};

    use super::{
        super::{
            preflight::InviteeHealth,
            test_support::{cache_party, health_reply, host_row, pid},
        },
        *,
    };
    use crate::{
        auth::Principal,
        consts::{FEATURES, OWNER_ONLY_KICK},
        db::{
            MIGRATOR,
            rows::DecPartyParticipantRow,
            schema::{Commitable, SchemaWrite},
        },
    };

    /// This build lists the rotation, so a peer on it passes the gate. A peer
    /// on an older build, or one that did not answer, blocks the start, and
    /// the message names it.
    #[test]
    fn a_rotation_needs_every_peer_to_list_the_feature() {
        assert!(FEATURES.contains(&ROTATE_KEY));
        let replies: Vec<InviteeHealth> = vec![
            ("p2".to_string(), Ok(health_reply("1.15.0", FEATURES))),
            (
                "p3".to_string(),
                Ok(health_reply("1.14.0", &[OWNER_ONLY_KICK])),
            ),
            ("p4".to_string(), Err("timed out".to_string())),
        ];
        let lacking = lacking_feature(&replies, ROTATE_KEY);
        assert_eq!(
            lacking,
            vec![
                ("p3".to_string(), "running version 1.14.0".to_string()),
                ("p4".to_string(), "could not verify: timed out".to_string()),
            ]
        );
        let message = format_peers_lacking_rotate_key(&lacking);
        assert!(
            message.contains("p3 (running version 1.14.0)") && message.contains("p4 ("),
            "{message}"
        );
    }

    /// The rotate-key start checks that run before any topology read, on a
    /// node whose admin API is a fake Canton.
    mod start_rotate_key_refusals {
        use super::*;
        use crate::signing::{
            custody::SoftwareKeys,
            fake_admin_api::{FakeAdminApi, TestNode, VaultKey},
            key_store::STORE_OFF_HINT,
        };

        /// A node `pid(1)` on `fake`, with `members` cached as the hosts of
        /// party `pid(9)`. The node's files live as long as the `TestNode`.
        async fn node(
            pool: SqlitePool,
            fake: &FakeAdminApi,
            rule: SoftwareKeys,
            members: &[CantonId],
        ) -> anyhow::Result<(web::Data<AppState>, TestNode)> {
            let party = pid(9)?;
            cache_party(&pool, &party, 1, None).await?;
            let rows: Vec<DecPartyParticipantRow> = members
                .iter()
                .map(|member| DecPartyParticipantRow {
                    dec_party_id: party.to_string(),
                    ..host_row(member, None)
                })
                .collect();
            let mut tx = pool.begin_transaction().await?;
            tx.replace_dec_party_participants(&party, &rows).await?;
            Commitable::commit(tx).await?;

            let mut test_node = fake.node(rule, false).await?;
            test_node.config.node.participant_id = Some(pid(1)?);
            let state = Arc::try_unwrap(AppState::for_test(None).await?.into_inner())
                .map_err(|_| anyhow::anyhow!("a fresh test state has one owner"))?;
            let data = web::Data::new(AppState {
                db: pool,
                config: test_node.config.clone(),
                ..state
            });
            Ok((data, test_node))
        }

        /// Ask `data` to rotate its key of `pid(9)`, and return the status
        /// and the error it gives.
        async fn rotate(data: web::Data<AppState>) -> anyhow::Result<(StatusCode, String)> {
            let db = data.db.clone();
            let app =
                actix_test::init_service(App::new().app_data(data).service(start_rotate_key)).await;
            let request = actix_test::TestRequest::post()
                .uri("/rotate-key")
                .set_json(serde_json::json!({ "decentralized_party_id": pid(9)?.to_string() }))
                .to_request();
            request.extensions_mut().insert(Principal {
                sub: "admin".to_string(),
                issuer: "test".to_string(),
                roles: Vec::new(),
                email: None,
            });

            let response = actix_test::call_service(&app, request).await;
            let status = response.status();
            let body: serde_json::Value = actix_test::read_body_json(response).await;

            let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_runs")
                .fetch_one(&db)
                .await?;
            assert_eq!(runs, 0, "a refused rotation persisted a run");
            Ok((
                status,
                body["error"].as_str().unwrap_or_default().to_string(),
            ))
        }

        /// The vault keeps its keys in KMS, so only the membership can refuse.
        async fn kms_vault() -> anyhow::Result<FakeAdminApi> {
            FakeAdminApi::start(vec![VaultKey::kms("participant", 1)?]).await
        }

        #[sqlx::test(migrator = "MIGRATOR")]
        async fn refuses_a_node_that_is_not_a_member(pool: SqlitePool) -> anyhow::Result<()> {
            let fake = kms_vault().await?;
            let (data, _node) =
                node(pool, &fake, SoftwareKeys::Refused, &[pid(2)?, pid(3)?]).await?;

            let (status, error) = rotate(data).await?;

            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(error.contains("This node does not host"), "{error}");
            Ok(())
        }

        #[sqlx::test(migrator = "MIGRATOR")]
        async fn refuses_a_party_with_one_member(pool: SqlitePool) -> anyhow::Result<()> {
            let fake = kms_vault().await?;
            let (data, _node) = node(pool, &fake, SoftwareKeys::Refused, &[pid(1)?]).await?;

            let (status, error) = rotate(data).await?;

            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(error.contains("it has only one member"), "{error}");
            Ok(())
        }

        #[sqlx::test(migrator = "MIGRATOR")]
        async fn refuses_a_node_without_kms(pool: SqlitePool) -> anyhow::Result<()> {
            let fake = FakeAdminApi::start(vec![VaultKey::software("participant", 1)]).await?;
            let (data, _node) =
                node(pool, &fake, SoftwareKeys::Refused, &[pid(1)?, pid(2)?]).await?;

            let (status, error) = rotate(data).await?;

            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(error.contains("software vault, not in a KMS"), "{error}");
            Ok(())
        }

        #[sqlx::test(migrator = "MIGRATOR")]
        async fn refuses_the_opt_out_without_a_key_store(pool: SqlitePool) -> anyhow::Result<()> {
            let fake = FakeAdminApi::start(vec![VaultKey::software("participant", 1)]).await?;
            let (data, _node) =
                node(pool, &fake, SoftwareKeys::Stored, &[pid(1)?, pid(2)?]).await?;

            let (status, error) = rotate(data).await?;

            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(error.contains(STORE_OFF_HINT), "{error}");
            Ok(())
        }
    }

    /// A member that is not a configured peer gets no invitation, so it could
    /// not record the new key. This node itself never counts as missing.
    #[test]
    fn a_rotation_needs_every_other_member_as_a_peer() -> anyhow::Result<()> {
        let members: HashSet<CantonId> = [pid(1)?, pid(2)?, pid(3)?].into_iter().collect();
        assert_eq!(
            members_missing_from_peers(&members, &[pid(2)?], &pid(1)?),
            vec![pid(3)?.to_string()]
        );
        assert!(members_missing_from_peers(&members, &[pid(2)?, pid(3)?], &pid(1)?).is_empty());
        Ok(())
    }

    /// A cache that names other hosts than the chain would send invitations
    /// to the wrong members, and the peers would refuse late.
    #[test]
    fn a_rotation_needs_the_cached_hosts_to_match_the_chain() -> anyhow::Result<()> {
        let cached: HashSet<CantonId> = [pid(1)?, pid(2)?].into_iter().collect();
        assert!(stale_members(&cached, &[pid(2)?.to_string(), pid(1)?.to_string()]).is_none());
        let difference = stale_members(&cached, &[pid(1)?.to_string(), pid(3)?.to_string()])
            .context("a different host must count as stale")?;
        assert!(difference.contains("validator-3"), "{difference}");
        Ok(())
    }

    /// An unreachable Canton is not the operator's mistake, so it is no 400.
    #[test]
    fn only_a_canton_transport_error_counts_as_unreachable() {
        let refused = anyhow::anyhow!("This participant keeps its keys in Canton's software vault");
        assert!(!canton_unreachable(&refused));
        let unreachable = anyhow::Error::from(tonic::Status::unavailable("connection refused"))
            .context("listing the vault's keys");
        assert!(canton_unreachable(&unreachable));
    }
}

#[cfg(test)]
mod start_rotate_key_tests {
    //! Pins every answer `POST /rotate-key` gives before it starts a run:
    //! the status code, the headers and the exact JSON error body. A fake
    //! Canton admin API serves the vault and the party's topology.

    use std::sync::Arc;

    use actix_web::http::StatusCode;
    use canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::SigningKeysWithThreshold,
        protocol::v30::{
            PartyToParticipant, enums::ParticipantPermission,
            party_to_participant::HostingParticipant,
        },
    };
    use serde_json::json;

    use super::{
        super::test_support::{
            Answer, TestNode, break_table, cache_hosted_party, closed_port, in_progress_run,
            insert_peers, insert_run, peer, pid, post_as, post_as_admin, refuse_run_writes,
            test_node,
        },
        *,
    };
    use crate::{
        db::MIGRATOR,
        noise::NoiseKeypair,
        signing::{
            custody::SoftwareKeys,
            fake_admin_api::{self, FakeAdminApi, VaultKey},
        },
    };

    type Result<T = ()> = anyhow::Result<T>;

    /// A node `pid(1)` whose admin API is a fake Canton that holds this
    /// node's KMS key for party `pid(9)`. The party is hosted on the chain
    /// by `pid(1)` and `pid(2)`, and the cache names `cached` as its hosts.
    /// The fake and the node's files live as long as the returned values.
    struct RotationNode {
        data: web::Data<AppState>,
        _fake: FakeAdminApi,
        _files: fake_admin_api::TestNode,
    }

    async fn rotation_node(pool: SqlitePool, cached: &[CantonId]) -> Result<RotationNode> {
        let party = pid(9)?;
        let daml = VaultKey::kms("party-daml-transactions", 2)?;
        let fake =
            FakeAdminApi::start(vec![VaultKey::kms("participant", 1)?, daml.clone()]).await?;
        let mut files = fake.node(SoftwareKeys::Refused, false).await?;
        files.config.node.participant_id = Some(pid(1)?);
        tokio::fs::create_dir_all(files.config.data_dir()).await?;
        NoiseKeypair::generate()
            .save_to_file(files.config.key_file_path())
            .await?;
        files.record_daml_key(&pool, &party, &daml).await?;
        let other = VaultKey::kms("other-daml-transactions", 3)?;
        fake.publish_p2p(
            1,
            PartyToParticipant {
                party: party.to_string(),
                threshold: 1,
                participants: [pid(1)?, pid(2)?]
                    .iter()
                    .map(|id| HostingParticipant {
                        participant_uid: id.to_string(),
                        permission: ParticipantPermission::Confirmation as i32,
                        onboarding: None,
                    })
                    .collect(),
                party_signing_keys: Some(SigningKeysWithThreshold {
                    keys: vec![daml.public_key.clone(), other.public_key],
                    threshold: 1,
                }),
            },
        );
        let hosts: Vec<(&CantonId, Option<&str>)> = cached.iter().map(|id| (id, None)).collect();
        cache_hosted_party(&pool, &party, 1, &hosts, &[]).await?;
        let state = Arc::try_unwrap(AppState::for_test(None).await?.into_inner())
            .map_err(|_| anyhow::anyhow!("a fresh test state has one owner"))?;
        let data = web::Data::new(AppState {
            db: pool,
            config: files.config.clone(),
            ..state
        });
        Ok(RotationNode {
            data,
            _fake: fake,
            _files: files,
        })
    }

    /// As [`rotation_node`], with the chain's hosts cached and `pid(2)` a
    /// peer, whose Noise key parses only if `reachable_key`.
    async fn ready_node(pool: SqlitePool, reachable_key: bool) -> Result<RotationNode> {
        insert_peers(&pool, &[peer(&pid(2)?, closed_port()?, reachable_key)]).await?;
        rotation_node(pool, &[pid(1)?, pid(2)?]).await
    }

    fn request() -> Result<serde_json::Value> {
        Ok(json!({ "decentralized_party_id": pid(9)?.to_string() }))
    }

    async fn rotate(data: &web::Data<AppState>) -> Result<Answer> {
        Ok(post_as_admin(data, start_rotate_key, "/rotate-key", request()?).await)
    }

    /// A node on closed Canton ports with `members` cached as the hosts.
    async fn plain_node(pool: SqlitePool, members: &[CantonId]) -> Result<TestNode> {
        let hosts: Vec<(&CantonId, Option<&str>)> = members.iter().map(|id| (id, None)).collect();
        cache_hosted_party(&pool, &pid(9)?, 1, &hosts, &[]).await?;
        test_node(pool, &pid(1)?).await
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_anonymous_caller_is_unauthorized(pool: SqlitePool) -> Result {
        let node = plain_node(pool, &[pid(1)?, pid(2)?]).await?;
        post_as(
            &node.data,
            start_rotate_key,
            "/rotate-key",
            request()?,
            None,
        )
        .await
        .assert_error(StatusCode::UNAUTHORIZED, "authentication required");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_member_read_answers_500(pool: SqlitePool) -> Result {
        let node = plain_node(pool.clone(), &[pid(1)?, pid(2)?]).await?;
        break_table(&pool, "dec_party_participant").await?;
        rotate(&node.data).await?.assert_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to load decentralized party members",
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_party_without_cached_members_is_a_conflict(pool: SqlitePool) -> Result {
        let node = test_node(pool, &pid(1)?).await?;
        let party = pid(9)?;
        rotate(&node.data).await?.assert_error(
            StatusCode::CONFLICT,
            &format!(
                "No cached membership for {party}. Try refreshing /decentralized-parties first."
            ),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_node_that_does_not_host_the_party_is_a_bad_request(pool: SqlitePool) -> Result {
        let node = plain_node(pool, &[pid(2)?, pid(3)?]).await?;
        let party = pid(9)?;
        rotate(&node.data).await?.assert_error(
            StatusCode::BAD_REQUEST,
            &format!("This node does not host {party}, so it holds no Daml key of it to rotate"),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_lone_member_is_a_bad_request(pool: SqlitePool) -> Result {
        let node = plain_node(pool, &[pid(1)?]).await?;
        let party = pid(9)?;
        rotate(&node.data).await?.assert_error(
            StatusCode::BAD_REQUEST,
            &format!("Cannot rotate a key of {party}: it has only one member"),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_unreachable_canton_is_a_bad_gateway(pool: SqlitePool) -> Result {
        let node = plain_node(pool.clone(), &[pid(1)?, pid(2)?]).await?;
        let error = require_kms_for_rotation(&node.data.config, &pool)
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("a closed admin port must fail the check"))?;
        assert!(canton_unreachable(&error), "{error:#}");
        rotate(&node.data)
            .await?
            .assert_error(StatusCode::BAD_GATEWAY, &format!("{error:#}"));
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_participant_without_kms_is_a_bad_request(pool: SqlitePool) -> Result {
        let fake = FakeAdminApi::start(vec![VaultKey::software("participant", 1)]).await?;
        let mut files = fake.node(SoftwareKeys::Refused, false).await?;
        files.config.node.participant_id = Some(pid(1)?);
        let node = plain_node(pool.clone(), &[pid(1)?, pid(2)?]).await?;
        let state = Arc::try_unwrap(AppState::for_test(None).await?.into_inner())
            .map_err(|_| anyhow::anyhow!("a fresh test state has one owner"))?;
        let data = web::Data::new(AppState {
            db: pool.clone(),
            config: files.config.clone(),
            ..state
        });
        drop(node);
        let error = require_kms_for_rotation(&data.config, &pool)
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("a software vault must fail the check"))?;
        assert!(!canton_unreachable(&error), "{error:#}");
        rotate(&data)
            .await?
            .assert_error(StatusCode::BAD_REQUEST, &format!("{error:#}"));
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_cache_that_differs_from_the_chain_is_a_conflict(pool: SqlitePool) -> Result {
        let node = rotation_node(pool, &[pid(1)?, pid(3)?]).await?;
        let (party, p1, p2, p3) = (pid(9)?, pid(1)?, pid(2)?, pid(3)?);
        rotate(&node.data).await?.assert_error(
            StatusCode::CONFLICT,
            &format!(
                "This node's cached members of {party} differ from the chain (cache \
                 {{\"{p1}\", \"{p3}\"}}, chain {{\"{p1}\", \"{p2}\"}}). Refresh \
                 /decentralized-parties and retry"
            ),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_peer_read_answers_500(pool: SqlitePool) -> Result {
        let node = ready_node(pool.clone(), false).await?;
        break_table(&pool, "peers").await?;
        rotate(&node.data)
            .await?
            .assert_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to load peers");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_member_that_is_not_a_peer_is_a_conflict(pool: SqlitePool) -> Result {
        let node = rotation_node(pool, &[pid(1)?, pid(2)?]).await?;
        let (party, p2) = (pid(9)?, pid(2)?);
        rotate(&node.data).await?.assert_error(
            StatusCode::CONFLICT,
            &format!(
                "Cannot rotate this node's key of {party}: every other member must record the \
                 new key, but these members are not configured peers: {p2}. Add them as peers \
                 first"
            ),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_party_with_a_run_in_flight_is_a_conflict(pool: SqlitePool) -> Result {
        let party = pid(9)?;
        let node = ready_node(pool.clone(), false).await?;
        insert_run(&pool, &in_progress_run("busy-run", &party)).await?;
        rotate(&node.data).await?.assert_error(
            StatusCode::CONFLICT,
            &format!(
                "Party {party} already has a Contracts workflow in flight (run busy-run); \
                 wait for it to finish or cancel it from its coordinator"
            ),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_party_another_request_is_starting_is_a_conflict(pool: SqlitePool) -> Result {
        let party = pid(9)?;
        let node = ready_node(pool, false).await?;
        let _claim = node
            .data
            .workflows
            .claim_start(&party)
            .ok_or_else(|| anyhow::anyhow!("a fresh registry has no claims"))?;
        rotate(&node.data).await?.assert_error(
            StatusCode::CONFLICT,
            &format!(
                "Another request is starting a workflow for {party}; retry once it has started"
            ),
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_unverifiable_invitee_is_a_conflict(pool: SqlitePool) -> Result {
        let node = ready_node(pool.clone(), true).await?;
        let probe = probe_invitee_health(&node.data.config, &pool, &[pid(2)?]).await;
        let expected = format_incompatible_peers(&incompatible_versions(&probe));
        assert!(expected.contains(&pid(2)?.to_string()), "{expected}");
        rotate(&node.data)
            .await?
            .assert_error(StatusCode::CONFLICT, &expected);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_run_write_is_a_conflict_and_leaves_no_run(pool: SqlitePool) -> Result {
        let node = ready_node(pool.clone(), false).await?;
        refuse_run_writes(&pool).await?;
        let answer = rotate(&node.data).await?;
        assert_eq!(answer.status, StatusCode::CONFLICT, "{}", answer.body);
        assert_eq!(answer.content_type.as_deref(), Some("application/json"));
        let body = answer.json()?;
        let error = body["error"].as_str().unwrap_or_default();
        assert!(
            error.starts_with("Failed to start rotate-key workflow: ")
                && error.contains("workflow_runs"),
            "{error}"
        );
        assert_eq!(body.as_object().map(|o| o.len()), Some(1), "{body}");
        assert!(node.data.workflows.snapshot().is_empty());
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_valid_rotation_is_accepted(pool: SqlitePool) -> Result {
        let node = ready_node(pool, false).await?;
        let answer = rotate(&node.data).await?;
        assert_eq!(answer.status, StatusCode::ACCEPTED, "{}", answer.body);
        assert_eq!(answer.content_type.as_deref(), Some("application/json"));
        let body = answer.json()?;
        let instance = body["instance_name"].as_str().unwrap_or_default();
        assert!(instance.starts_with("validator-9-rotate-key-"), "{body}");
        assert_eq!(
            answer.body,
            format!(
                r#"{{"status":"inprogress","message":"Rotate-key workflow started","instance_name":"{instance}"}}"#
            )
        );
        Ok(())
    }
}
