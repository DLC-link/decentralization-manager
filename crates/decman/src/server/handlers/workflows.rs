use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, LazyLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use actix_web::{HttpRequest, HttpResponse, Responder, get, http::StatusCode, post, web};

use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD};
use canton_proto_rs::com::digitalasset::canton::protocol::v30::PartyToParticipant;
use prometheus::IntCounterVec;
use sqlx::SqlitePool;

use super::parties::resolve_owner_keys_from_peers;
use canton_proto_rs::com::digitalasset::canton::crypto::admin::v30::vault_service_client::VaultServiceClient;

use crate::{
    canton_id::{CantonId, Fingerprint, validate_party_id_prefix},
    config::{NetworkConfig, NodeConfig},
    consts::{MIN_PEER_VERSION, OWNER_ONLY_KICK, ROTATE_KEY},
    db::{
        rows::{DecPartyOwnerRow, DecPartyParticipantRow},
        schema::{Commitable, SchemaRead, SchemaWrite},
    },
    error::Result,
    noise::{
        MAX_CHUNKED_TOTAL_SIZE, Message, MessageType, NoiseKeypair, parse_public_key,
        send_noise_message, send_noise_message_with_retry,
    },
    server::{
        AppState, external_parties,
        health::{HealthReply, classify_health_reply},
        middleware::require_admin,
        package_inventory::fetch_dar_for_package,
        respawn_coordinator,
        types::{
            AcsTransferProgress, AddPartyInvitePayload, AddPartyRequest,
            ChangeThresholdInvitePayload, ChangeThresholdRequest, ContractsInvitePayload,
            ContractsRequest, DarsInvitePayload, DarsRequest, DistributePackageRequest,
            ErrorResponse, ExternalPartiesResponse, ExternalPartyHost, ExternalPartyInfo,
            KickInvitePayload, KickRequest, KickResponse, KickStatus, MessageResponse,
            MissingEdgeKind, MissingPeerEdge, OnboardingInvitePayload, OnboardingMeshErrorResponse,
            OnboardingRequest, OnboardingResponse, OnboardingStatus, RotateKeyInvitePayload,
            RotateKeyRequest, SuccessResponse, WorkflowGuard, WorkflowInstance, WorkflowKind,
            WorkflowProgress, WorkflowResponse, WorkflowRole, WorkflowRun, WorkflowRunsResponse,
            WorkflowStatusResponse, permission_from_proto,
        },
    },
    signing::custody,
    utils,
    workflow::{
        self, AddPartyStep, ChangeThresholdStep, ContractsStep, DarsStep, KickStep, OnboardingStep,
        RotateKeyStep,
        contracts::DarFile,
        signing_keys::{
            KeyClaimError, ensure_survivor_keys_preserved, kick_key_claims,
            known_signing_keys_by_member, members_requiring_key_adoption,
            recorded_signing_keys_by_member, requires_signing_key_adoption,
        },
        state::WorkflowStep,
        storage::WorkflowStorage,
        thresholds::{
            PartyThresholds, carried_confirmation_threshold, check_confirmation_threshold,
        },
        topology,
    },
};

// ============================================================================
// Workflow State Types
// ============================================================================

// Per-kind workflow state is now tracked per-instance in
// `AppState.workflows` (`WorkflowRegistry`) rather than via singleton
// `HttpWorkflowState<S>` aliases.

// ============================================================================
// Workflow run persistence helpers
// ============================================================================

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Insert the coordinator-side `workflow_runs` row for a freshly-started run.
/// Duplicate-instance protection lives in the registry insert that precedes
/// this call (migration 000013 dropped the one-InProgress-per-(kind, role)
/// unique index — concurrent same-kind runs are allowed); a failure here is a
/// genuine persistence error and bubbles back as 409.
async fn insert_coordinator_run<S, C>(
    db: &SqlitePool,
    instance_name: &str,
    kind: WorkflowKind,
    initial_step: S,
    config: &C,
    invitees: &[CantonId],
    dec_party_id: Option<CantonId>,
) -> Result
where
    S: WorkflowStep,
    C: serde::Serialize,
{
    let now = now_secs();
    let run = WorkflowRun {
        instance_name: instance_name.to_string(),
        kind,
        role: WorkflowRole::Coordinator,
        status: WorkflowProgress::InProgress,
        current_step: initial_step.step_name().to_string(),
        step_index: initial_step.step_index(),
        step_total: S::step_total(),
        config_json: serde_json::to_string(config)
            .map_err(|e| anyhow::anyhow!("encode workflow config: {e}"))?,
        coordinator_pubkey: None,
        coordinator_instance: None,
        coordinator_name: None,
        expected_peers: invitees.to_vec(),
        completed_peers: Vec::new(),
        connected_peers: Vec::new(),
        acs_progress: None,
        dec_party_id,
        prefix: None,
        participants: Vec::new(),
        previous_threshold: None,
        new_threshold: None,
        kicked_participant: None,
        kicked_owner_key: None,
        added_participant: None,
        package_names: Vec::new(),
        dar_filenames: Vec::new(),
        error: None,
        dismissed: false,
        created_at: now,
        updated_at: now,
    };

    let mut tx = db.begin_transaction().await?;
    tx.upsert_workflow_run(&run).await?;
    Commitable::commit(tx).await
}

/// Register a freshly-built coordinator run in the shared registry and persist
/// its `workflow_runs` row — the register-or-409 / persist-or-unregister
/// contract every `start_*` handler needs, kept in one place so a change to it
/// can't drift between the four handlers.
///
/// The registry insert is the dedup gate: a duplicate `instance_name` is
/// rejected here BEFORE we persist, so a racing same-instance request can't
/// upsert (and clobber) the in-flight run's row. If the persist then fails the
/// registry entry is removed, so a run that never started can't leak and block
/// future starts. On either failure returns the 409 `HttpResponse` the handler
/// should return; on success the run is both registered and persisted.
#[allow(clippy::result_large_err)]
async fn register_and_persist<S, C>(
    data: &web::Data<AppState>,
    instance: &Arc<WorkflowInstance>,
    initial_step: S,
    config: &C,
    invitees: &[CantonId],
    dec_party_id: Option<CantonId>,
) -> std::result::Result<(), HttpResponse>
where
    S: WorkflowStep,
    C: serde::Serialize,
{
    let kind = instance.kind;
    let instance_name = &instance.instance_name;
    let label = match kind {
        WorkflowKind::Onboarding => "onboarding",
        WorkflowKind::Kick => "kick",
        WorkflowKind::Contracts => "contracts",
        WorkflowKind::Dars => "DARs",
        WorkflowKind::AddParty => "add-party",
        WorkflowKind::ChangeThreshold => "change-threshold",
        WorkflowKind::RotateKey => "rotate-key",
    };

    if !data.workflows.insert(instance.clone()) {
        return Err(HttpResponse::Conflict().json(ErrorResponse {
            error: format!("A workflow with instance {instance_name} is already running"),
        }));
    }

    if let Err(e) = insert_coordinator_run(
        &data.db,
        instance_name,
        kind,
        initial_step,
        config,
        invitees,
        dec_party_id,
    )
    .await
    {
        data.workflows.remove(instance_name);
        tracing::warn!("Failed to persist {label} workflow run: {e:#}");
        return Err(HttpResponse::Conflict().json(ErrorResponse {
            error: format!("Failed to start {label} workflow: {e}"),
        }));
    }

    count_run_started(kind);
    Ok(())
}

/// Flip the persisted run's status to Completed. Errors are logged and
/// swallowed — the spawned task can't usefully react to a DB error here.
pub(crate) async fn mark_run_completed(db: &SqlitePool, kind: WorkflowKind, instance_name: &str) {
    match finish_run(db, instance_name, WorkflowProgress::Completed, None).await {
        Ok(true) => count_run_finished(kind, RunOutcome::Completed),
        Ok(false) => {}
        Err(e) => tracing::warn!("Failed to mark run {instance_name} completed: {e:#}"),
    }
}

/// Flip the persisted run's status to Failed with a message.
pub(crate) async fn mark_run_failed(
    db: &SqlitePool,
    kind: WorkflowKind,
    instance_name: &str,
    error: &str,
) {
    match finish_run(db, instance_name, WorkflowProgress::Failed, Some(error)).await {
        Ok(true) => count_run_finished(kind, RunOutcome::Failed),
        Ok(false) => {}
        Err(e) => tracing::warn!("Failed to mark run {instance_name} failed: {e:#}"),
    }
}

/// Flip the persisted run's status to Cancelled. `Ok(false)` means the run
/// had already ended, so nothing was cancelled.
async fn mark_run_cancelled(
    db: &SqlitePool,
    kind: WorkflowKind,
    instance_name: &str,
) -> Result<bool> {
    let cancelled = finish_run(db, instance_name, WorkflowProgress::Cancelled, None).await?;
    if cancelled {
        count_run_finished(kind, RunOutcome::Cancelled);
    }
    Ok(cancelled)
}

/// Record a terminal status on a run that has not ended yet. The first terminal
/// write wins, so a run counts under one outcome only. A failed write counts
/// nothing and leaves the row in progress, which recovery resumes on restart.
async fn finish_run(
    db: &SqlitePool,
    instance_name: &str,
    status: WorkflowProgress,
    error: Option<&str>,
) -> Result<bool> {
    let mut tx = db.begin_transaction().await?;
    let finished = tx
        .finish_workflow_run(instance_name, status, error, now_secs())
        .await?;
    Commitable::commit(tx).await?;
    if !finished {
        tracing::warn!(
            instance_name,
            %status,
            "run already has a terminal status; this status was not recorded"
        );
    }
    Ok(finished)
}

// ============================================================================
// Workflow run metrics
// ============================================================================

// Coordinator runs only. A peer row can read Cancelled and then Failed for the
// same run, so peer runs have no single point that sees one outcome per run.
// A peer's decline records Failed through `mark_run_failed`, so the run's later
// end records and counts nothing.

static RUNS_STARTED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_workflow_runs_started_total",
        "Workflow runs this node started as coordinator. A retry counts as a new start. A run resumed after a restart does not.",
        &["kind"]
    )
    .expect("metric name is a unique literal")
});

static RUNS_FINISHED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_workflow_runs_finished_total",
        "Workflow runs this node coordinated that ended, by outcome: completed, failed or cancelled.",
        &["kind", "outcome"]
    )
    .expect("metric name is a unique literal")
});

/// Every kind, for exporting each series at zero. Extend it with
/// [`run_kind_label`], whose match fails to compile for a new kind.
const RUN_KINDS: [WorkflowKind; 7] = [
    WorkflowKind::Onboarding,
    WorkflowKind::Kick,
    WorkflowKind::Contracts,
    WorkflowKind::Dars,
    WorkflowKind::AddParty,
    WorkflowKind::ChangeThreshold,
    WorkflowKind::RotateKey,
];

/// The `kind` label, fixed per variant so the series stay bounded.
fn run_kind_label(kind: WorkflowKind) -> &'static str {
    match kind {
        WorkflowKind::Onboarding => "onboarding",
        WorkflowKind::Kick => "kick",
        WorkflowKind::Contracts => "contracts",
        WorkflowKind::Dars => "dars",
        WorkflowKind::AddParty => "add_party",
        WorkflowKind::ChangeThreshold => "change_threshold",
        WorkflowKind::RotateKey => "rotate_key",
    }
}

#[derive(Clone, Copy)]
enum RunOutcome {
    Completed,
    Failed,
    Cancelled,
}

impl RunOutcome {
    const ALL: [Self; 3] = [Self::Completed, Self::Failed, Self::Cancelled];

    fn label(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

fn count_run_started(kind: WorkflowKind) {
    RUNS_STARTED
        .with_label_values(&[run_kind_label(kind)])
        .inc();
}

fn count_run_finished(kind: WorkflowKind, outcome: RunOutcome) {
    RUNS_FINISHED
        .with_label_values(&[run_kind_label(kind), outcome.label()])
        .inc();
}

/// Registers both families at startup and exports every kind and outcome at
/// zero, so a dashboard reads zero rather than no data before the first run.
pub(crate) fn register_run_metrics() {
    for kind in RUN_KINDS {
        RUNS_STARTED
            .with_label_values(&[run_kind_label(kind)])
            .inc_by(0);
        for outcome in RunOutcome::ALL {
            RUNS_FINISHED
                .with_label_values(&[run_kind_label(kind), outcome.label()])
                .inc_by(0);
        }
    }
}

// ============================================================================
// Kick Workflow
// ============================================================================

/// A party's member count, as a threshold bound has to see it.
///
/// `members` is the cached membership and `self_id` is this node. The
/// coordinator signs the proposals, so it is necessarily a member — but it does
/// not appear in its own `peers` table, so a member set derived from that list
/// omits it. Counting it explicitly keeps the bound right however `members` was
/// built, including the fallback that substitutes the configured peer set when
/// nothing is cached.
fn party_member_count(members: &HashSet<CantonId>, self_id: &CantonId) -> usize {
    members.len() + usize::from(!members.contains(self_id))
}

/// The highest threshold a kick may set.
///
/// The namespace threshold counts the owners left, and the signing threshold
/// that follows it counts the hosts' keys left. A host's kick takes one of
/// each. An owner that hosts nothing leaves every host in place.
fn kick_threshold_bound(target_hosts: bool, member_count: usize, owner_count: usize) -> i32 {
    let hosts_left = member_count.saturating_sub(usize::from(target_hosts));
    let owners_left = if owner_count == 0 {
        // A party missing from the owner cache bounds by its hosts alone, as
        // the kick always did.
        hosts_left
    } else {
        owner_count - 1
    };
    i32::try_from(hosts_left.min(owners_left)).unwrap_or(i32::MAX)
}

/// The members that stay after a kick, split by whether this node can invite
/// them. Neither list holds this node or the kicked host.
#[derive(Debug, PartialEq)]
struct KickSigners {
    /// Members that are configured peers. The kick invites these to sign.
    invitees: Vec<CantonId>,
    /// Members that are not configured peers. The kick cannot reach these.
    not_peers: Vec<CantonId>,
}

/// Split the members that stay after a kick into invitees and members that
/// are not peers. The kicked host never signs, so its peer entry plays no part.
fn kick_signers(
    members: &HashSet<CantonId>,
    peer_ids: &HashSet<CantonId>,
    self_id: &CantonId,
    kicked_host: Option<&CantonId>,
) -> KickSigners {
    let (mut invitees, mut not_peers): (Vec<CantonId>, Vec<CantonId>) = members
        .iter()
        .filter(|member| *member != self_id && Some(*member) != kicked_host)
        .cloned()
        .partition(|member| peer_ids.contains(member));
    invitees.sort();
    not_peers.sort();
    KickSigners {
        invitees,
        not_peers,
    }
}

/// Why a kick cannot collect enough signatures, or `None` when it can.
///
/// This node and each invitee sign. Both the current namespace threshold
/// (`current`, `None` when the party is not cached) and the requested one
/// must be reachable, because the P2P mapping is submitted after the new
/// namespace takes effect.
fn kick_quorum_refusal(
    current: Option<usize>,
    requested: usize,
    signers: &KickSigners,
) -> Option<String> {
    let peers = signers.invitees.len();
    let available = 1 + peers;
    let current_limits = current.is_some_and(|threshold| threshold > available);
    let requested_limits = requested > available;
    let mut message = if current_limits || requested_limits {
        let limit = if let Some(threshold) = current.filter(|_| current_limits) {
            format!("Cannot kick: the party's current namespace threshold is {threshold}.")
        } else {
            format!("Cannot kick: the requested new_threshold is {requested}.")
        };
        match peers {
            0 => format!("{limit} Only this node can sign."),
            1 => format!("{limit} Only 2 members can sign: this node and 1 configured peer."),
            _ => format!(
                "{limit} Only {available} members can sign: this node and {peers} configured \
                 peers."
            ),
        }
    } else if peers == 0 {
        "Cannot kick: this node has no other hosting member to co-sign the kick.".to_string()
    } else {
        return None;
    };
    for id in &signers.not_peers {
        message.push_str(&format!(
            " {id} is a member of the party but is not a configured peer of this node."
        ));
    }
    // Every member that stays signs once it is a peer. The run also needs a
    // co-signer besides this node.
    let reachable = available + signers.not_peers.len();
    let required = current.unwrap_or(1).max(requested).max(2);
    if !signers.not_peers.is_empty() && reachable >= required {
        message.push_str(match signers.not_peers.len() {
            1 => " Add it as a peer, then retry.",
            _ => " Add them as peers, then retry.",
        });
    } else if current_limits {
        message.push_str(
            " Lower the threshold with Change Threshold first, while every host can still sign.",
        );
        if requested_limits {
            message.push_str(&format!(
                " Then pick a new_threshold of {available} or lower."
            ));
        }
    } else if requested_limits {
        message.push_str(&format!(" Pick a new_threshold of {available} or lower."));
    }
    Some(message)
}

/// A namespace quorum cannot authorize a new inline signing key on behalf
/// of an unreachable member. Read topology only when a survivor is not a peer.
async fn kick_key_adoption_refusal(
    config: &NodeConfig,
    db: &SqlitePool,
    party: &CantonId,
    not_peers: &[CantonId],
    removed: &str,
    kicked: Option<&CantonId>,
) -> Result<Option<String>> {
    if not_peers.is_empty() {
        return Ok(None);
    }
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let mapping = topology::fetch_p2p_mapping(config, &synchronizer_id, party).await?;
    kick_key_adoption_refusal_for_mapping(config, db, party, not_peers, removed, kicked, &mapping)
        .await
}

async fn kick_key_adoption_refusal_for_mapping(
    config: &NodeConfig,
    db: &SqlitePool,
    party: &CantonId,
    not_peers: &[CantonId],
    removed: &str,
    kicked: Option<&CantonId>,
    mapping: &PartyToParticipant,
) -> Result<Option<String>> {
    let inline = mapping
        .party_signing_keys
        .as_ref()
        .map_or(&[][..], |keys| keys.keys.as_slice());
    let kicked = kicked.map(ToString::to_string);
    let survivors: Vec<String> = mapping
        .participants
        .iter()
        .filter(|host| Some(&host.participant_uid) != kicked.as_ref())
        .map(|host| host.participant_uid.clone())
        .collect();
    let members: Vec<String> = not_peers
        .iter()
        .map(ToString::to_string)
        .filter(|member| survivors.contains(member))
        .collect();
    // With no inline keys every missing survivor must sign adoption. No
    // attribution lookup can make that kick safe to start without them.
    if inline.is_empty() {
        return Ok(kick_missing_adopters(&members));
    }
    let claims = known_signing_keys_by_member(config, db, party, inline).await?;
    let claims = kick_key_claims(Some(removed), &survivors, &claims, &members)?;
    if !requires_signing_key_adoption(mapping) {
        ensure_survivor_keys_preserved(inline, &survivors, &claims)?;
        return Ok(None);
    }
    let required = members_requiring_key_adoption(mapping, &members, &claims)?;
    Ok(kick_missing_adopters(&required))
}

fn kick_missing_adopters(required: &[String]) -> Option<String> {
    let (owner, add) = match required {
        [] => return None,
        [member] => (
            format!("{member} owns such a key but is not a configured peer of this node."),
            "Add it as a peer, then retry.",
        ),
        several => (
            format!(
                "{members} own such keys but are not configured peers of this node.",
                members = several.join(", ")
            ),
            "Add them as peers, then retry.",
        ),
    };
    Some(format!(
        "Cannot kick: the party still keeps signing keys in the older key mapping. The kick \
         moves them into the hosting list. The owner of each moved key must sign. {owner} {add}"
    ))
}

/// Refuse a kick whose proposal could not keep each remaining member's
/// signing key. Only a record says which key is whose, so every remaining
/// host other than this node needs one. This node reads its own key from its
/// vault when it builds the proposal. Reads only the database.
///
/// # Errors
///
/// [`KeyClaimError`] when a key is not on record or two remaining members
/// claim one key. Any other error is a database failure.
async fn check_kick_survivor_keys(
    db: &SqlitePool,
    party: &CantonId,
    self_id: &CantonId,
    signers: &KickSigners,
    removed: &str,
) -> Result {
    let claims = recorded_signing_keys_by_member(db, party).await?;
    let survivors: Vec<String> = signers
        .invitees
        .iter()
        .chain(&signers.not_peers)
        .chain(std::iter::once(self_id))
        .map(ToString::to_string)
        .collect();
    kick_key_claims(Some(removed), &survivors, &claims, &[])?;

    let unknown = |members: &[CantonId]| -> Vec<String> {
        members
            .iter()
            .map(ToString::to_string)
            .filter(|member| !claims.contains_key(member))
            .collect()
    };
    let peers = unknown(&signers.invitees);
    let not_peers = unknown(&signers.not_peers);
    if peers.is_empty() && not_peers.is_empty() {
        return Ok(());
    }
    let mut message = format!(
        "Cannot kick: this node has no record of the signing key of {members}. The kick must \
         keep the key of each member that stays.",
        members = peers
            .iter()
            .chain(&not_peers)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
    match not_peers.as_slice() {
        [] => message
            .push_str(" Refresh /decentralized-parties so each peer reports its key, then retry."),
        [member] => message.push_str(&format!(
            " {member} is not a configured peer of this node, so a refresh cannot read its \
             key. Add it as a peer, refresh /decentralized-parties, then retry."
        )),
        several => message.push_str(&format!(
            " {members} are not configured peers of this node, so a refresh cannot read their \
             keys. Add them as peers, refresh /decentralized-parties, then retry.",
            members = several.join(", ")
        )),
    }
    Err(KeyClaimError::Unresolved(message).into())
}

/// The response to a failed signing-key check of a kick. A key-claim refusal
/// says what to do, so it goes back as it is. Any other failure is logged and
/// answered with generic text.
fn kick_key_check_failure(party: &CantonId, e: &anyhow::Error) -> HttpResponse {
    match e.downcast_ref::<KeyClaimError>() {
        Some(KeyClaimError::Conflict(error)) => {
            tracing::warn!("Kick of {party} refused: {error}");
            HttpResponse::Conflict().json(ErrorResponse {
                error: error.clone(),
            })
        }
        Some(KeyClaimError::Unresolved(error)) => {
            tracing::info!("Kick of {party} refused: {error}");
            HttpResponse::ServiceUnavailable().json(ErrorResponse {
                error: error.clone(),
            })
        }
        None => {
            tracing::error!("Cannot check the signing keys of {party} for a kick: {e:#}");
            HttpResponse::ServiceUnavailable().json(ErrorResponse {
                error: "Cannot check the party's signing keys right now. Retry later.".to_string(),
            })
        }
    }
}

/// The member a kick removes.
#[derive(Debug, PartialEq)]
struct KickTarget {
    /// The namespace owner key the kick removes.
    owner_key: String,
    /// The participant named for the member, when one is known.
    participant: Option<CantonId>,
    /// Whether that participant hosts the party, and so leaves the mapping too.
    hosts: bool,
}

#[derive(Debug, PartialEq)]
enum KickTargetError {
    BadRequest(String),
    Conflict(String),
}

impl KickTargetError {
    fn into_response(self) -> HttpResponse {
        match self {
            Self::BadRequest(error) => HttpResponse::BadRequest().json(ErrorResponse { error }),
            Self::Conflict(error) => HttpResponse::Conflict().json(ErrorResponse { error }),
        }
    }
}

/// Work out which member a kick request names, from the cached membership.
///
/// A host is named by its participant, and its owner key comes from the cache.
/// An owner that hosts nothing is named by its owner key, or by the
/// participant that claimed the key. Such a kick waits until every host's own
/// key is known, because until then the key may belong to a host whose key
/// has not been resolved. A claim does not settle it: any peer can claim any
/// key, and every peer refuses to sign until it knows each host's key.
///
/// A host's cached key counts as known only while it is still an owner. A host
/// that rotated its key would otherwise have its new key read as unclaimed.
fn resolve_kick_target(
    participant: Option<&CantonId>,
    owner_key: Option<&str>,
    hosts: &[DecPartyParticipantRow],
    owners: &[DecPartyOwnerRow],
) -> std::result::Result<KickTarget, KickTargetError> {
    const REFRESH: &str = "Try refreshing /decentralized-parties first.";
    let host_of = |uid: &CantonId| {
        let uid = uid.to_string();
        hosts.iter().find(|host| host.participant_uid == uid)
    };
    // A party missing from the owner cache has nothing to check a key
    // against, so its hosts' cached keys stand as they are.
    let known_key = |host: &DecPartyParticipantRow| {
        host.owner_key
            .clone()
            .filter(|key| owners.is_empty() || owners.iter().any(|o| o.owner_key == *key))
    };
    let unresolved_host = |key: &str| {
        hosts
            .iter()
            .find(|host| known_key(host).is_none())
            .map(|host| {
                KickTargetError::Conflict(format!(
                    "Cannot tell whether owner key {key} belongs to host {uid}, whose current \
                     owner key has not been resolved yet. {REFRESH}",
                    uid = host.participant_uid
                ))
            })
    };

    let Some(key) = owner_key else {
        let Some(participant) = participant else {
            return Err(KickTargetError::BadRequest(
                "Name the member to kick with participant_id, owner_key, or both".to_string(),
            ));
        };
        if let Some(host) = host_of(participant) {
            return match known_key(host) {
                Some(key) => Ok(KickTarget {
                    owner_key: key,
                    participant: Some(participant.clone()),
                    hosts: true,
                }),
                None => Err(KickTargetError::Conflict(format!(
                    "Participant {participant} hosts the party, but its current owner key has \
                     not been resolved yet. {REFRESH}"
                ))),
            };
        }
        let uid = participant.to_string();
        let claimed: Vec<&DecPartyOwnerRow> = owners
            .iter()
            .filter(|owner| owner.participant_uid.as_deref() == Some(uid.as_str()))
            .collect();
        return match claimed.as_slice() {
            [] => Err(KickTargetError::BadRequest(format!(
                "Cannot kick {participant}: not a member of this decentralized party"
            ))),
            [owner] => match unresolved_host(&owner.owner_key) {
                Some(error) => Err(error),
                None => Ok(KickTarget {
                    owner_key: owner.owner_key.clone(),
                    participant: Some(participant.clone()),
                    hosts: false,
                }),
            },
            several => Err(KickTargetError::Conflict(format!(
                "Participant {participant} claims {count} owner keys of this party. Name the \
                 one to remove with owner_key",
                count = several.len()
            ))),
        };
    };

    let Some(owner) = owners.iter().find(|owner| owner.owner_key == key) else {
        return Err(KickTargetError::BadRequest(format!(
            "Owner key {key} is not an owner of this decentralized party. {REFRESH}"
        )));
    };

    if let Some(host) = hosts
        .iter()
        .find(|host| host.owner_key.as_deref() == Some(key))
    {
        let uid = CantonId::parse(&host.participant_uid).map_err(|e| {
            KickTargetError::Conflict(format!(
                "Owner key {key} belongs to host {uid}, whose cached id does not parse: {e}",
                uid = host.participant_uid
            ))
        })?;
        if let Some(participant) = participant
            && *participant != uid
        {
            return Err(KickTargetError::BadRequest(format!(
                "Owner key {key} belongs to {uid}, not {participant}"
            )));
        }
        return Ok(KickTarget {
            owner_key: key.to_string(),
            participant: Some(uid),
            hosts: true,
        });
    }

    let claimant = owner
        .participant_uid
        .as_deref()
        .and_then(|uid| CantonId::parse(uid).ok());
    if let Some(claimant) = &claimant
        && host_of(claimant).is_some()
    {
        return Err(KickTargetError::Conflict(format!(
            "Owner key {key} was claimed by host {claimant}, but the cache holds another key \
             for it. {REFRESH}"
        )));
    }
    if let Some(error) = unresolved_host(key) {
        return Err(error);
    }
    if let Some(participant) = participant {
        if host_of(participant).is_some() {
            return Err(KickTargetError::BadRequest(format!(
                "Participant {participant} hosts the party under another owner key, not {key}"
            )));
        }
        match &claimant {
            Some(claimant) if claimant == participant => {}
            Some(claimant) => {
                return Err(KickTargetError::BadRequest(format!(
                    "Owner key {key} was claimed by {claimant}, not {participant}"
                )));
            }
            None => {
                return Err(KickTargetError::BadRequest(format!(
                    "No peer has claimed owner key {key}, so it cannot be tied to \
                     {participant}. Name only the owner key"
                )));
            }
        }
    }
    Ok(KickTarget {
        owner_key: key.to_string(),
        participant: claimant,
        hosts: false,
    })
}

/// The namespace threshold of the cached party, or `None` when the party is
/// not cached.
async fn cached_namespace_threshold(db: &SqlitePool, party: &CantonId) -> Result<Option<usize>> {
    let party_id = party.to_string();
    let Some(row) = db
        .get_dec_parties_by_prefix(&party.prefix)
        .await?
        .into_iter()
        .find(|row| row.party_id == party_id)
    else {
        return Ok(None);
    };
    Ok(Some(usize::try_from(row.threshold)?))
}

/// The confirmation threshold a kick or add-party carries over, worked out
/// from the cached party. A party missing from the cache counts as one whose
/// thresholds match; the coordinator checks the chain before it signs.
async fn cached_confirmation_threshold(
    db: &SqlitePool,
    party: &CantonId,
    host_count: usize,
) -> Result<Option<i32>> {
    let party_id = party.to_string();
    let Some(row) = db
        .get_dec_parties_by_prefix(&party.prefix)
        .await?
        .into_iter()
        .find(|row| row.party_id == party_id)
    else {
        return Ok(None);
    };
    let current = PartyThresholds::new(
        i32::try_from(row.threshold)?,
        row.confirmation_threshold.map(i32::try_from).transpose()?,
    );
    Ok(carried_confirmation_threshold(current, host_count))
}

/// Start a kick workflow to remove a participant from a decentralized party
#[utoipa::path(
    tag = "Workflows",
    request_body = KickRequest,
    responses(
        (status = 202, description = "Kick workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress, owner key not yet resolved for the target participant, or two remaining members claim one signing key", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
        (status = 503, description = "A remaining member's signing key is not on record, or the party's signing keys cannot be read right now", body = ErrorResponse)
    )
)]
#[post("/kick")]
pub async fn start_kick(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<KickRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }

    tracing::info!(
        "Kick request received: party={}, participant_to_kick={:?}, owner_key={:?}, \
         threshold={}",
        body.decentralized_party_id,
        body.participant_id,
        body.owner_key,
        body.new_threshold
    );

    // KickRequest's CantonId fields validate during deserialization, so by
    // the time we get here both ids are well-formed.
    let decentralized_party_id = body.decentralized_party_id.clone();
    let self_id = data.config.participant_id().clone();

    if body.participant_id.as_ref() == Some(&self_id) {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "Cannot kick yourself".to_string(),
        });
    }

    let member_rows = match data
        .db
        .get_dec_party_participants(&decentralized_party_id)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!("Failed to load dec party participants for kick: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to load decentralized party members".to_string(),
            });
        }
    };
    let owner_rows = match data.db.get_dec_party_owners(&decentralized_party_id).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!("Failed to load dec party owners for kick: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to load decentralized party owners".to_string(),
            });
        }
    };

    // The server works out the owner key from the cache rather than taking
    // it from the client, so an empty prefill becomes a clear error.
    let target = match resolve_kick_target(
        body.participant_id.as_ref(),
        body.owner_key.as_deref(),
        &member_rows,
        &owner_rows,
    ) {
        Ok(target) => target,
        Err(e) => return e.into_response(),
    };
    if target.participant.as_ref() == Some(&self_id) {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "Cannot kick yourself".to_string(),
        });
    }
    // The participant that leaves the hosting mapping, if the member hosts.
    let kicked_host = target.participant.clone().filter(|_| target.hosts);

    // The peer list decides which members the kick can invite. A DB error
    // here is fatal, because without it the signer count is unknown.
    let peers = match data.db.get_all_peers().await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Failed to load peers for kick: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to load peers".to_string(),
            });
        }
    };

    // Scope the kick to this dec party's actual members, not every peer this
    // node has configured. A party can be a strict subset of the configured
    // mesh (see start_onboarding), and basing the signer set / threshold bound
    // / invites on the full mesh would invite outsiders and stall
    // WaitingForPeers on peers that were never part of the party. Fall back to
    // the full peer set only if no cached row yields a usable id — membership
    // not cached yet, or every `participant_uid` in a legacy/invalid format —
    // since scoping to an empty member set would reject the kick with a
    // misleading "need at least 2 party members" error.
    let party_member_ids: HashSet<CantonId> = {
        let parsed: HashSet<CantonId> = member_rows
            .iter()
            .filter_map(|r| CantonId::parse(&r.participant_uid).ok())
            .collect();
        if parsed.is_empty() {
            tracing::warn!(
                "No usable cached participants for {decentralized_party_id} \
                 ({count} rows); falling back to all configured peers for \
                 kick scoping",
                count = member_rows.len()
            );
            peers.iter().map(|p| p.participant_id.clone()).collect()
        } else {
            parsed
        }
    };
    // The target was checked against the membership above. Its peer entry
    // does not matter, because the kick never contacts it. Each member that
    // stays has to sign, and the kick reaches a member only through its peer
    // entry.
    let peer_ids: HashSet<CantonId> = peers.into_iter().map(|p| p.participant_id).collect();
    let signers = kick_signers(&party_member_ids, &peer_ids, &self_id, kicked_host.as_ref());
    let member_count = party_member_count(&party_member_ids, &self_id);
    let namespace_threshold =
        match cached_namespace_threshold(&data.db, &decentralized_party_id).await {
            Ok(threshold) => threshold,
            Err(e) => {
                tracing::error!("Failed to read the cached threshold for kick: {e}");
                return HttpResponse::InternalServerError().json(ErrorResponse {
                    error: "Failed to read the party's cached thresholds".to_string(),
                });
            }
        };

    // Only the hosts sign. An owner that hosts nothing cannot be asked, so
    // the namespace threshold in force must be reachable without it.
    if !target.hosts
        && let Some(threshold) = namespace_threshold
        && threshold > member_count
    {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "Cannot kick owner key {key}: the party's namespace threshold is \
                 {threshold}, but only its {member_count} hosting members can sign. \
                 The owner being removed would have to sign too, and a kick cannot \
                 ask it",
                key = target.owner_key
            ),
        });
    }

    // Validate `new_threshold` before persisting anything. Negative or zero
    // values corrupt topology submission: DNS `authorize()` accepts a bare
    // i32 while the subsequent P2P proposal converts via `try_into()` to
    // u32 and fails partway, leaving the DNS write committed with no
    // rollback. The upper bound is the post-kick member count — there
    // must be at least as many remaining signers as the threshold needs.
    //
    // Counted from the party's OWN member set, not from the invitees. They
    // exclude this node, which is itself a member — it signs the proposals.
    // Bounding by them undercounts by one, and a party whose threshold
    // already equals its member count then has no acceptable value at all:
    // the peers reject anything below the current threshold, and this check
    // rejects anything at or above it. The add-party and change-threshold
    // bounds below already count the party set.
    //
    // The party's signing threshold follows the namespace one and counts the
    // hosts' keys, so the hosts left after the kick bound it as well as the
    // owners left. An owner that hosts nothing leaves every host in place.
    let post_kick_member_count = member_count - usize::from(target.hosts);
    let max_threshold = kick_threshold_bound(target.hosts, member_count, owner_rows.len());
    if body.new_threshold < 1 || body.new_threshold > max_threshold {
        let reason = if target.hosts {
            format!("party member count {member_count}, minus the participant being kicked")
        } else {
            format!(
                "the party keeps its {member_count} hosts and {owners} owners",
                owners = owner_rows.len().saturating_sub(1)
            )
        };
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "new_threshold must be between 1 and {max_threshold} ({reason}); got {got}",
                got = body.new_threshold,
            ),
        });
    }

    // The DNS changes first. Its signatures satisfy the current threshold,
    // and the following P2P update must satisfy the proposed threshold too.
    if let Some(error) =
        kick_quorum_refusal(namespace_threshold, body.new_threshold as usize, &signers)
    {
        return HttpResponse::BadRequest().json(ErrorResponse { error });
    }
    if !signers.not_peers.is_empty() {
        tracing::info!(
            "Kick of {decentralized_party_id} proceeds without members that are not \
             configured peers: {ids}",
            ids = signers
                .not_peers
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let confirmation_threshold = match cached_confirmation_threshold(
        &data.db,
        &decentralized_party_id,
        post_kick_member_count,
    )
    .await
    {
        Ok(threshold) => threshold,
        Err(e) => {
            tracing::error!("Failed to read the cached thresholds for kick: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to read the party's cached thresholds".to_string(),
            });
        }
    };

    // Fixed before the InProgress flip so a concurrent /kick/cancel cannot
    // observe InProgress while we're still preparing.
    let invitees = signers.invitees.clone();

    // Compute instance name + config up-front so the persisted workflow_runs row
    // can carry the same identifier the coordinator task will use.
    let timestamp = now_secs();
    let instance_name = format!("{}-kick-{timestamp}", decentralized_party_id.prefix);

    // Register this run in the shared registry. A duplicate `instance_name`
    // (extremely unlikely given the timestamp) is the only rejection — multiple
    // distinct kick runs may now proceed concurrently.
    let instance = WorkflowInstance::new(
        instance_name.clone(),
        WorkflowKind::Kick,
        WorkflowRole::Coordinator,
    );
    let kick_state = &instance.http;

    let kick_config = workflow::KickConfig::new(
        decentralized_party_id.clone(),
        target.participant,
        target.owner_key,
        body.new_threshold,
        confirmation_threshold,
        body.previous_threshold,
        instance_name.clone(),
    );

    // Refuse a second workflow targeting the SAME decentralized party — the
    // party's topology mutations must not interleave (two kicks, or a kick
    // racing a contracts deployment, conflict at the Canton layer with
    // confusing step failures).
    if let Some((run, kind)) =
        find_inprogress_run_for_party(&data.db, &decentralized_party_id).await
    {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "Party {decentralized_party_id} already has a {kind} workflow in flight \
                 (run {run}); wait for it to finish or cancel it first"
            ),
        });
    }

    let removed = kick_config.participant_id.as_ref().map_or_else(
        || kick_config.namespace_fingerprint.clone(),
        ToString::to_string,
    );
    if let Err(e) = check_kick_survivor_keys(
        &data.db,
        &decentralized_party_id,
        &self_id,
        &signers,
        &removed,
    )
    .await
    {
        return kick_key_check_failure(&decentralized_party_id, &e);
    }
    match kick_key_adoption_refusal(
        &data.config,
        &data.db,
        &decentralized_party_id,
        &signers.not_peers,
        &removed,
        kick_config.participant_id.as_ref(),
    )
    .await
    {
        Ok(Some(error)) => return HttpResponse::BadRequest().json(ErrorResponse { error }),
        Ok(None) => {}
        Err(e) => return kick_key_check_failure(&decentralized_party_id, &e),
    }

    // Refuse to invite any peer that can't speak the concurrent-workflows wire
    // format — NO invites are sent if even one invitee fails the version gate.
    let health = probe_invitee_health(&data.config, &data.db, &invitees).await;
    let incompatible = incompatible_versions(&health);
    if !incompatible.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format_incompatible_peers(&incompatible),
        });
    }
    // An older peer refuses an owner-only kick with an error that does not
    // say why, so the run would fail after the invites went out.
    if !target.hosts {
        let lacking = lacking_feature(&health, OWNER_ONLY_KICK);
        if !lacking.is_empty() {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: format_peers_lacking_owner_only_kick(&lacking),
            });
        }
    }

    // Register + persist atomically w.r.t. duplicates (registry insert dedups
    // before the upsert; a persist failure unregisters so nothing leaks).
    if let Err(resp) = register_and_persist(
        &data,
        &instance,
        KickStep::WaitingForPeers,
        &kick_config,
        &invitees,
        Some(decentralized_party_id.clone()),
    )
    .await
    {
        return resp;
    }

    *kick_state.invited_peers.write().await = invitees;

    // Spawn the kick workflow in the background
    let config = data.config.clone();
    let db = data.db.clone();
    let kick_state_clone = instance.http.clone();
    let instance_for_coord = instance.clone();
    let kind = instance.kind;
    let workflows = data.workflows.clone();
    let last_seen = data.last_seen.clone();
    let instance_for_task = instance_name.clone();

    // See start_dars below for the rationale: abort_handle, status, and error
    // are flipped under simultaneously-held locks so a concurrent /kick/cancel
    // can never observe "status=InProgress + abort_handle=None" and bail.
    let mut abort_guard = kick_state.abort_handle.lock().await;
    let mut status_guard = kick_state.status.write().await;
    let mut error_guard = kick_state.error.write().await;

    let join_handle = tokio::spawn(async move {
        // Removes this run from the registry on return (success/failure/abort).
        let _workflow_guard = WorkflowGuard::new(workflows, instance_for_task.clone());

        // Send kick invites to the surviving party members before starting workflow
        let invite_result = send_kick_invites(&config, &db, &kick_config, &signers).await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send kick invites: {e}");
            let msg = format!("Failed to send invites: {e}");
            {
                let mut status = kick_state_clone.status.write().await;
                let mut error = kick_state_clone.error.write().await;
                *status = KickStatus::Failed;
                *error = Some(msg.clone());
            }
            mark_run_failed(&db, kind, &instance_for_task, &msg).await;
            return;
        }

        // Give peers time to start their peer workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let result = workflow::start_coordinator(
            config,
            db.clone(),
            workflow::WorkflowType::Kick,
            None, // No onboarding config
            Some(kick_config),
            None, // No contracts config
            None, // No dars config
            None, // No add-party config
            None, // No change-threshold config
            None, // No rotate-key config
            None, // No auth registry for kick
            last_seen,
            instance_for_coord,
        )
        .await;

        // Update in-memory state in tight scopes — never hold the RwLock
        // across a DB await. /kick/status acquires a read lock to serve
        // every poll; if a writer holds the lock during the DB write, every
        // concurrent read blocks for that duration on a slow runner.
        match result {
            Ok(_) => {
                {
                    let mut status = kick_state_clone.status.write().await;
                    *status = KickStatus::Completed;
                }
                tracing::info!("Kick workflow completed successfully");
                mark_run_completed(&db, kind, &instance_for_task).await;
            }
            Err(e) => {
                let msg = format!("{e}");
                {
                    let mut status = kick_state_clone.status.write().await;
                    let mut error = kick_state_clone.error.write().await;
                    *status = KickStatus::Failed;
                    *error = Some(msg.clone());
                }
                tracing::error!("Kick workflow failed: {e}");
                mark_run_failed(&db, kind, &instance_for_task, &msg).await;
            }
        }
    });
    *abort_guard = Some(join_handle.abort_handle());
    *status_guard = KickStatus::InProgress;
    *error_guard = None;
    drop(error_guard);
    drop(status_guard);
    drop(abort_guard);

    HttpResponse::Accepted().json(KickResponse {
        status: KickStatus::InProgress,
        message: "Kick workflow started".to_string(),
        instance_name,
    })
}

/// Get the current status of the kick workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Kick workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/kick/status")]
pub async fn get_kick_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(kind_status(&data, WorkflowKind::Kick).await)
}

/// Pick the coordinator run of `kind` that the legacy per-kind `/{kind}/status`
/// and `/{kind}/cancel` endpoints act on. `snapshot()` is HashMap-ordered, so we
/// sort by `instance_name` and take the lowest — a stable choice so repeated
/// polls and cancels don't flip between concurrent same-kind runs from call to
/// call. Callers that mean a specific run use the per-instance endpoints. Shared
/// by `kind_status` and `cancel_workflow_state` so both always pick the same
/// instance.
fn pick_coordinator_run(
    data: &web::Data<AppState>,
    kind: WorkflowKind,
) -> Option<Arc<WorkflowInstance>> {
    let mut runs: Vec<_> = data
        .workflows
        .snapshot()
        .into_iter()
        .filter(|i| i.kind == kind && i.role == WorkflowRole::Coordinator)
        .collect();
    runs.sort_by(|a, b| a.instance_name.cmp(&b.instance_name));
    runs.into_iter().next()
}

/// Summarize the status of a coordinator run of `kind` for the legacy
/// per-kind `/{kind}/status` endpoints.
///
/// While a run is live it is in the registry, so report its in-memory status.
/// But the registry entry is removed the moment a run reaches a terminal
/// status (its `WorkflowGuard` drops), so once finished there is nothing in the
/// registry — fall back to the **latest persisted `workflow_runs` row** of this
/// kind, which carries the terminal Completed/Failed/Cancelled status (and is
/// the same source the `/workflows` feed uses). Without this fallback a poller
/// watching `/{kind}/status` would never observe completion. With concurrent
/// runs of one kind these endpoints are necessarily coarse — callers wanting
/// per-instance detail should use `GET /workflows`.
async fn kind_status(data: &web::Data<AppState>, kind: WorkflowKind) -> WorkflowStatusResponse {
    if let Some(inst) = pick_coordinator_run(data, kind) {
        return WorkflowStatusResponse {
            status: *inst.http.status.read().await,
            error: inst.http.error.read().await.clone(),
        };
    }
    // No live run registered — report the latest persisted coordinator run of
    // this kind (terminal status survives in the DB after deregistration).
    if let Ok(runs) = SchemaRead::get_visible_workflow_runs(&data.db).await
        && let Some(run) = runs
            .into_iter()
            .filter(|r| r.kind == kind && r.role == WorkflowRole::Coordinator)
            .max_by_key(|r| r.created_at)
    {
        return WorkflowStatusResponse {
            status: run.status,
            error: run.error,
        };
    }
    WorkflowStatusResponse {
        status: WorkflowProgress::default(),
        error: None,
    }
}

/// Pin the complete surviving host set in the accepted invitation. Members
/// that are not peers remain on the topology even though they cannot sign.
fn kick_invite_payload(
    kick_config: &workflow::KickConfig,
    signers: &KickSigners,
    coordinator: &CantonId,
) -> KickInvitePayload {
    let mut participants: Vec<CantonId> = signers
        .invitees
        .iter()
        .chain(&signers.not_peers)
        .cloned()
        .collect();
    participants.push(coordinator.clone());
    participants.sort();
    participants.dedup();
    KickInvitePayload {
        dec_party_id: kick_config.decentralized_party_id.clone(),
        kicked_participant: kick_config.participant_id.clone(),
        kicked_owner_key: Some(kick_config.namespace_fingerprint.clone()),
        new_threshold: kick_config.new_threshold,
        previous_threshold: kick_config.previous_threshold,
        confirmation_threshold: kick_config.confirmation_threshold,
        participants,
        workflow_instance: Some(kick_config.instance_name.clone()),
    }
}

/// Send kick invites only to the reachable members selected for this run.
async fn send_kick_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    kick_config: &workflow::KickConfig,
    signers: &KickSigners,
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let current_participant_id = config.participant_id();
    let kicked_participant = kick_config.participant_id.as_ref();
    let invitee_set: HashSet<&CantonId> = signers.invitees.iter().collect();
    let payload = kick_invite_payload(kick_config, signers, current_participant_id);
    let payload_bytes = serde_json::to_vec(&payload).context("encode KickInvitePayload")?;
    let invite_message = Message::new(MessageType::InviteKick, payload_bytes);

    tracing::info!(
        "Kick invites: self={current_participant_id}, kicked={kicked_participant:?}, \
         owner key={}",
        kick_config.namespace_fingerprint
    );

    for peer in &network_config.peers {
        tracing::debug!(
            "Checking peer {}: self_match={}, kicked_match={}",
            peer.participant_id,
            peer.participant_id == *current_participant_id,
            Some(&peer.participant_id) == kicked_participant
        );

        // Only invite this run's surviving party members (see start_kick). The
        // self/kicked skips below are now subsumed by this but kept for clarity.
        if !invitee_set.contains(&peer.participant_id) {
            continue;
        }

        // Skip self
        if peer.participant_id == *current_participant_id {
            tracing::debug!("Skipping {} - this is self", peer.participant_id);
            continue;
        }

        // Skip the peer being kicked (they won't participate in the kick workflow)
        if Some(&peer.participant_id) == kicked_participant {
            tracing::info!(
                "Skipping invite to {} - they are being kicked",
                peer.participant_id
            );
            continue;
        }

        if peer.public_key.is_empty() {
            tracing::warn!(
                "Skipping invite to {} - no public key configured",
                peer.participant_id
            );
            continue;
        }

        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!(
                    "Skipping invite to {} - invalid public key: {e}",
                    peer.participant_id
                );
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);
        // Use participant_id as identity (must match what server expects in peer_keys lookup)
        let identity = config.participant_id().to_string();

        tracing::info!(
            "Sending kick invite to {} at {}:{}",
            peer.participant_id,
            peer.address,
            peer.port
        );

        match send_noise_message(
            &peer.address,
            peer.port,
            &psk,
            identity.as_bytes(),
            &invite_message,
        )
        .await
        {
            Ok(response) => {
                interpret_invite_reply(&peer.participant_id, "kick", &response)?;
            }
            Err(e) => {
                tracing::error!("Failed to send kick invite to {}: {e}", peer.participant_id);
            }
        }
    }

    Ok(())
}

// ============================================================================
// Add-Party Workflow
// ============================================================================

/// Start an add-party workflow to add a new member to a decentralized party
#[utoipa::path(
    tag = "Workflows",
    request_body = AddPartyRequest,
    responses(
        (status = 202, description = "Add-party workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress for this party, or the participant is already a member", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/add-party")]
pub async fn start_add_party(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<AddPartyRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }

    tracing::info!(
        "Add-party request received: party={}, new_participant={}, threshold={}",
        body.decentralized_party_id,
        body.new_participant_id,
        body.new_threshold
    );

    let decentralized_party_id = body.decentralized_party_id.clone();
    let new_participant_id = body.new_participant_id.clone();

    if new_participant_id == *data.config.participant_id() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "Cannot add yourself as the new participant".to_string(),
        });
    }

    let peers = match data.db.get_all_peers().await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Failed to load peers for add-party: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to load peers".to_string(),
            });
        }
    };

    // The new member must be a configured, known peer — invites travel over
    // Noise, so a participant this node can't reach can't join.
    if !peers.iter().any(|p| p.participant_id == new_participant_id) {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "Participant {new_participant_id} is not a configured peer of this node"
            ),
        });
    }

    // Scope the existing-member set to the party's cached membership (same
    // rationale as start_kick — the party can be a strict subset of the
    // configured mesh).
    let party_member_ids: HashSet<CantonId> = match data
        .db
        .get_dec_party_participants(&decentralized_party_id)
        .await
    {
        Ok(rows) => rows
            .iter()
            .filter_map(|r| CantonId::parse(&r.participant_uid).ok())
            .collect(),
        Err(e) => {
            tracing::error!("Failed to load dec party participants for add-party: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to load decentralized party members".to_string(),
            });
        }
    };
    if party_member_ids.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "No cached membership for {decentralized_party_id}. Try refreshing \
                 /decentralized-parties first."
            ),
        });
    }
    if party_member_ids.contains(&new_participant_id) {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "Participant {new_participant_id} is already a member of \
                 {decentralized_party_id}"
            ),
        });
    }
    if !party_member_ids.contains(data.config.participant_id()) {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "This node is not a member of {decentralized_party_id}; only an existing \
                 member can coordinate adding one"
            ),
        });
    }

    // Bound the new threshold by the members that can actually sign, which
    // excludes the one being added. It carries Canton's onboarding marker until
    // its ACS import completes, so it confirms nothing — and a P2P add at
    // threshold = the post-add member count is the known full-threshold bug:
    // the write never becomes effective, and the run stalls with no signal
    // saying why.
    //
    // Raising the threshold to include the new member is a second step, after
    // its marker clears, via the change-threshold workflow. The deeper
    // validation against the live DNS owner set happens in ExportState.
    let signing_member_count = party_member_ids.len() as i32;
    if body.new_threshold < 1 || body.new_threshold > signing_member_count {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "new_threshold must be between 1 and {signing_member_count} (the members that \
                 can sign; the member being added cannot until its ACS import completes); got \
                 {got}. To include it, run change-threshold once its onboarding marker clears",
                got = body.new_threshold,
            ),
        });
    }

    let confirmation_threshold = match cached_confirmation_threshold(
        &data.db,
        &decentralized_party_id,
        party_member_ids.len() + 1,
    )
    .await
    {
        Ok(threshold) => threshold,
        Err(e) => {
            tracing::error!("Failed to read the cached thresholds for add-party: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to read the party's cached thresholds".to_string(),
            });
        }
    };

    // Invitees: every existing member except self, plus the new member.
    let mut invitees: Vec<CantonId> = peers
        .into_iter()
        .map(|p| p.participant_id)
        .filter(|p| party_member_ids.contains(p) && p != data.config.participant_id())
        .collect();
    invitees.push(new_participant_id.clone());

    let timestamp = now_secs();
    let instance_name = format!("{}-add-party-{timestamp}", decentralized_party_id.prefix);

    let instance = WorkflowInstance::new(
        instance_name.clone(),
        WorkflowKind::AddParty,
        WorkflowRole::Coordinator,
    );
    let add_party_state = &instance.http;

    let add_party_config = workflow::AddPartyConfig::new(
        decentralized_party_id.clone(),
        new_participant_id.clone(),
        body.new_threshold,
        confirmation_threshold,
        body.previous_threshold,
        instance_name.clone(),
    );

    // Refuse a second workflow targeting the SAME decentralized party (see
    // start_kick for the rationale — topology mutations must not interleave).
    if let Some((run, kind)) =
        find_inprogress_run_for_party(&data.db, &decentralized_party_id).await
    {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "Party {decentralized_party_id} already has a {kind} workflow in flight \
                 (run {run}); wait for it to finish or cancel it first"
            ),
        });
    }

    // Refuse to invite any peer that can't speak the concurrent-workflows wire
    // format — NO invites are sent if even one invitee fails the version gate.
    let incompatible = preflight_incompatible_peers(&data.config, &data.db, &invitees).await;
    if !incompatible.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format_incompatible_peers(&incompatible),
        });
    }

    // Register + persist atomically w.r.t. duplicates (registry insert dedups
    // before the upsert; a persist failure unregisters so nothing leaks).
    if let Err(resp) = register_and_persist(
        &data,
        &instance,
        AddPartyStep::WaitingForPeers,
        &add_party_config,
        &invitees,
        Some(decentralized_party_id.clone()),
    )
    .await
    {
        return resp;
    }

    let invitees_for_invites = invitees.clone();
    *add_party_state.invited_peers.write().await = invitees;

    let config = data.config.clone();
    let db = data.db.clone();
    let add_party_state_clone = instance.http.clone();
    let instance_for_coord = instance.clone();
    let kind = instance.kind;
    let workflows = data.workflows.clone();
    let last_seen = data.last_seen.clone();
    let instance_for_task = instance_name.clone();
    // Ledger token source for the begin-offset capture (see
    // `current_ledger_offset`); the workflow proceeds without it.
    let workflow_auth = data.auth.read().await.clone();

    // abort_handle/status/error are flipped under simultaneously-held locks —
    // see start_dars for the cancel-race rationale.
    let mut abort_guard = add_party_state.abort_handle.lock().await;
    let mut status_guard = add_party_state.status.write().await;
    let mut error_guard = add_party_state.error.write().await;

    let join_handle = tokio::spawn(async move {
        let _workflow_guard = WorkflowGuard::new(workflows, instance_for_task.clone());

        let invite_result =
            send_add_party_invites(&config, &db, &add_party_config, &invitees_for_invites).await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send add-party invites: {e}");
            let msg = format!("Failed to send invites: {e}");
            {
                let mut status = add_party_state_clone.status.write().await;
                let mut error = add_party_state_clone.error.write().await;
                *status = WorkflowProgress::Failed;
                *error = Some(msg.clone());
            }
            mark_run_failed(&db, kind, &instance_for_task, &msg).await;
            return;
        }

        // Give peers time to start their peer workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let result = workflow::start_coordinator(
            config,
            db.clone(),
            workflow::WorkflowType::AddParty,
            None, // No onboarding config
            None, // No kick config
            None, // No contracts config
            None, // No dars config
            Some(add_party_config),
            None, // No change-threshold config
            None, // No rotate-key config
            workflow_auth,
            last_seen,
            instance_for_coord,
        )
        .await;

        match result {
            Ok(_) => {
                // The run row is marked terminal BEFORE the in-memory status
                // flips. The two are read by different callers — `/…/status`
                // reads this lock, while the "already has a workflow in flight"
                // guard scans the run rows — so flipping memory first opens a
                // window where the API reports a run finished and refuses the
                // next workflow for that party in the same breath. Doing the
                // durable write first can only report a finished run as still
                // running for an instant, which costs a poll rather than a
                // rejected request.
                mark_run_completed(&db, kind, &instance_for_task).await;
                {
                    let mut status = add_party_state_clone.status.write().await;
                    *status = WorkflowProgress::Completed;
                }
                tracing::info!("Add-party workflow completed successfully");
            }
            Err(e) => {
                let msg = format!("{e}");
                // Durable row first, in-memory status second — same ordering as the
                // success path above, and for the same reason: a caller that reads
                // Failed from `/…/status` and retries must not be refused by the
                // in-flight guard, which scans the run rows.
                mark_run_failed(&db, kind, &instance_for_task, &msg).await;
                {
                    let mut status = add_party_state_clone.status.write().await;
                    let mut error = add_party_state_clone.error.write().await;
                    *status = WorkflowProgress::Failed;
                    *error = Some(msg.clone());
                }
                tracing::error!("Add-party workflow failed: {e}");
            }
        }
    });
    *abort_guard = Some(join_handle.abort_handle());
    *status_guard = WorkflowProgress::InProgress;
    *error_guard = None;
    drop(error_guard);
    drop(status_guard);
    drop(abort_guard);

    HttpResponse::Accepted().json(WorkflowResponse {
        status: WorkflowProgress::InProgress,
        message: "Add-party workflow started".to_string(),
        instance_name,
    })
}

/// Get the current status of the add-party workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Add-party workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/add-party/status")]
pub async fn get_add_party_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(kind_status(&data, WorkflowKind::AddParty).await)
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/add-party/cancel")]
pub async fn cancel_add_party(http_req: HttpRequest, data: web::Data<AppState>) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(&data, "Add-party", WorkflowKind::AddParty).await
}

/// Send add-party invites over Noise to the existing members and the new
/// member (`invitees` carries both).
async fn send_add_party_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    add_party_config: &workflow::AddPartyConfig,
    invitees: &[CantonId],
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let current_participant_id = config.participant_id();
    let invitee_set: HashSet<&CantonId> = invitees.iter().collect();
    // The card shows the full post-add member set: invitees + this node.
    let mut participants = invitees.to_vec();
    participants.push(current_participant_id.clone());
    let payload = AddPartyInvitePayload {
        dec_party_id: add_party_config.decentralized_party_id.clone(),
        new_participant: add_party_config.new_participant_id.clone(),
        new_threshold: add_party_config.new_threshold,
        previous_threshold: add_party_config.previous_threshold,
        confirmation_threshold: add_party_config.confirmation_threshold,
        participants,
        workflow_instance: Some(add_party_config.instance_name.clone()),
    };
    let payload_bytes = serde_json::to_vec(&payload).context("encode AddPartyInvitePayload")?;
    let invite_message = Message::new(MessageType::InviteAddParty, payload_bytes);

    for peer in &network_config.peers {
        if !invitee_set.contains(&peer.participant_id)
            || peer.participant_id == *current_participant_id
        {
            continue;
        }

        if peer.public_key.is_empty() {
            tracing::warn!(
                "Skipping add-party invite to {} - no public key configured",
                peer.participant_id
            );
            continue;
        }
        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!(
                    "Skipping add-party invite to {} - invalid public key: {e}",
                    peer.participant_id
                );
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);
        let identity = config.participant_id().to_string();

        tracing::info!(
            "Sending add-party invite to {} at {}:{}",
            peer.participant_id,
            peer.address,
            peer.port
        );

        match send_noise_message(
            &peer.address,
            peer.port,
            &psk,
            identity.as_bytes(),
            &invite_message,
        )
        .await
        {
            Ok(response) => {
                interpret_invite_reply(&peer.participant_id, "add-party", &response)?;
            }
            Err(e) => {
                tracing::error!(
                    "Failed to send add-party invite to {}: {e}",
                    peer.participant_id
                );
            }
        }
    }

    Ok(())
}

// ============================================================================
// Change-Threshold Workflow
// ============================================================================

/// Start a change-threshold workflow to change the namespace and
/// confirmation thresholds of an existing decentralized party, keeping its
/// membership.
#[utoipa::path(
    tag = "Workflows",
    request_body = ChangeThresholdRequest,
    responses(
        (status = 202, description = "Change-threshold workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request, such as a threshold or confirmation threshold out of range", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress for this party", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[post("/change-threshold")]
pub async fn start_change_threshold(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<ChangeThresholdRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }

    tracing::info!(
        "Change-threshold request received: party={}, threshold={}->{}, confirmation={:?}",
        body.decentralized_party_id,
        body.previous_threshold,
        body.new_threshold,
        body.confirmation_threshold
    );

    let decentralized_party_id = body.decentralized_party_id.clone();

    // Scope to the party's cached membership (same rationale as start_kick /
    // start_add_party — the party can be a strict subset of the mesh).
    let party_member_ids: HashSet<CantonId> = match data
        .db
        .get_dec_party_participants(&decentralized_party_id)
        .await
    {
        Ok(rows) => rows
            .iter()
            .filter_map(|r| CantonId::parse(&r.participant_uid).ok())
            .collect(),
        Err(e) => {
            tracing::error!("Failed to load dec party participants for change-threshold: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to load decentralized party members".to_string(),
            });
        }
    };

    if party_member_ids.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "No cached membership for {decentralized_party_id}. Try refreshing \
                 /decentralized-parties first."
            ),
        });
    }
    if !party_member_ids.contains(data.config.participant_id()) {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "This node is not a member of {decentralized_party_id}; only an existing \
                 member can coordinate a threshold change"
            ),
        });
    }
    // Need at least one other member to reach a signing quorum — a solo-owner
    // party has a fixed threshold of 1 and nothing to change.
    if party_member_ids.len() < 2 {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "Cannot change threshold: {decentralized_party_id} has only {n} member(s)",
                n = party_member_ids.len()
            ),
        });
    }

    // Bound the new threshold by the member count. The deeper validation
    // against the live DNS owner set (and the "already current" check) happens
    // in ExportState.
    let member_count = party_member_ids.len() as i32;
    if body.new_threshold < 1 || body.new_threshold > member_count {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "new_threshold must be between 1 and {member_count} (party member count); got {got}",
                got = body.new_threshold,
            ),
        });
    }
    // Every member hosts the party, so the member count bounds the
    // confirmation threshold too.
    if let Some(confirmation) = body.confirmation_threshold
        && let Err(e) = check_confirmation_threshold(confirmation, party_member_ids.len())
    {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: e.to_string(),
        });
    }

    // Invitees: every member except self — they all sign.
    let invitees: Vec<CantonId> = match data.db.get_all_peers().await {
        Ok(peers) => peers
            .into_iter()
            .map(|p| p.participant_id)
            .filter(|p| party_member_ids.contains(p) && p != data.config.participant_id())
            .collect(),
        Err(e) => {
            tracing::error!("Failed to load peers for change-threshold: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to load peers".to_string(),
            });
        }
    };

    let timestamp = now_secs();
    let instance_name = format!(
        "{}-change-threshold-{timestamp}",
        decentralized_party_id.prefix
    );

    let instance = WorkflowInstance::new(
        instance_name.clone(),
        WorkflowKind::ChangeThreshold,
        WorkflowRole::Coordinator,
    );
    let change_state = &instance.http;

    let change_config = workflow::ChangeThresholdConfig::new(
        decentralized_party_id.clone(),
        body.new_threshold,
        body.confirmation_threshold,
        body.previous_threshold,
        instance_name.clone(),
    );

    // Refuse a second workflow targeting the SAME dec party (topology mutations
    // must not interleave — see start_kick).
    if let Some((run, kind)) =
        find_inprogress_run_for_party(&data.db, &decentralized_party_id).await
    {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "Party {decentralized_party_id} already has a {kind} workflow in flight \
                 (run {run}); wait for it to finish or cancel it first"
            ),
        });
    }

    // Refuse to invite any peer that can't speak the concurrent-workflows wire
    // format — NO invites are sent if even one invitee fails the version gate.
    let incompatible = preflight_incompatible_peers(&data.config, &data.db, &invitees).await;
    if !incompatible.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format_incompatible_peers(&incompatible),
        });
    }

    if let Err(resp) = register_and_persist(
        &data,
        &instance,
        ChangeThresholdStep::WaitingForPeers,
        &change_config,
        &invitees,
        Some(decentralized_party_id.clone()),
    )
    .await
    {
        return resp;
    }

    let invitees_for_invites = invitees.clone();
    *change_state.invited_peers.write().await = invitees;

    let config = data.config.clone();
    let db = data.db.clone();
    let change_state_clone = instance.http.clone();
    let instance_for_coord = instance.clone();
    let kind = instance.kind;
    let workflows = data.workflows.clone();
    let last_seen = data.last_seen.clone();
    let instance_for_task = instance_name.clone();

    // abort_handle/status/error are flipped under simultaneously-held locks —
    // see start_dars for the cancel-race rationale.
    let mut abort_guard = change_state.abort_handle.lock().await;
    let mut status_guard = change_state.status.write().await;
    let mut error_guard = change_state.error.write().await;

    let join_handle = tokio::spawn(async move {
        let _workflow_guard = WorkflowGuard::new(workflows, instance_for_task.clone());

        let invite_result =
            send_change_threshold_invites(&config, &db, &change_config, &invitees_for_invites)
                .await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send change-threshold invites: {e}");
            let msg = format!("Failed to send invites: {e}");
            {
                let mut status = change_state_clone.status.write().await;
                let mut error = change_state_clone.error.write().await;
                *status = WorkflowProgress::Failed;
                *error = Some(msg.clone());
            }
            mark_run_failed(&db, kind, &instance_for_task, &msg).await;
            return;
        }

        // Give peers time to start their peer workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let result = workflow::start_coordinator(
            config,
            db.clone(),
            workflow::WorkflowType::ChangeThreshold,
            None, // No onboarding config
            None, // No kick config
            None, // No contracts config
            None, // No dars config
            None, // No add-party config
            Some(change_config),
            None, // No rotate-key config
            None, // No auth registry for change-threshold
            last_seen,
            instance_for_coord,
        )
        .await;

        match result {
            Ok(_) => {
                // The run row is marked terminal BEFORE the in-memory status
                // flips. The two are read by different callers — `/…/status`
                // reads this lock, while the "already has a workflow in flight"
                // guard scans the run rows — so flipping memory first opens a
                // window where the API reports a run finished and refuses the
                // next workflow for that party in the same breath. Doing the
                // durable write first can only report a finished run as still
                // running for an instant, which costs a poll rather than a
                // rejected request.
                mark_run_completed(&db, kind, &instance_for_task).await;
                {
                    let mut status = change_state_clone.status.write().await;
                    *status = WorkflowProgress::Completed;
                }
                tracing::info!("Change-threshold workflow completed successfully");
            }
            Err(e) => {
                let msg = format!("{e}");
                // Durable row first, in-memory status second — same ordering as the
                // success path above, and for the same reason: a caller that reads
                // Failed from `/…/status` and retries must not be refused by the
                // in-flight guard, which scans the run rows.
                mark_run_failed(&db, kind, &instance_for_task, &msg).await;
                {
                    let mut status = change_state_clone.status.write().await;
                    let mut error = change_state_clone.error.write().await;
                    *status = WorkflowProgress::Failed;
                    *error = Some(msg.clone());
                }
                tracing::error!("Change-threshold workflow failed: {e}");
            }
        }
    });
    *abort_guard = Some(join_handle.abort_handle());
    *status_guard = WorkflowProgress::InProgress;
    *error_guard = None;
    drop(error_guard);
    drop(status_guard);
    drop(abort_guard);

    HttpResponse::Accepted().json(WorkflowResponse {
        status: WorkflowProgress::InProgress,
        message: "Change-threshold workflow started".to_string(),
        instance_name,
    })
}

/// Get the current status of the change-threshold workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Change-threshold workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/change-threshold/status")]
pub async fn get_change_threshold_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(kind_status(&data, WorkflowKind::ChangeThreshold).await)
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/change-threshold/cancel")]
pub async fn cancel_change_threshold(
    http_req: HttpRequest,
    data: web::Data<AppState>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(&data, "Change-threshold", WorkflowKind::ChangeThreshold).await
}

/// Send change-threshold invites over Noise to every existing member.
async fn send_change_threshold_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    change_config: &workflow::ChangeThresholdConfig,
    invitees: &[CantonId],
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let current_participant_id = config.participant_id();
    let invitee_set: HashSet<&CantonId> = invitees.iter().collect();
    // The card shows the full member set: invitees + this node.
    let mut participants = invitees.to_vec();
    participants.push(current_participant_id.clone());
    let payload = ChangeThresholdInvitePayload {
        dec_party_id: change_config.decentralized_party_id.clone(),
        new_threshold: change_config.new_threshold,
        previous_threshold: change_config.previous_threshold,
        confirmation_threshold: change_config.confirmation_threshold,
        participants,
        workflow_instance: Some(change_config.instance_name.clone()),
    };
    let payload_bytes =
        serde_json::to_vec(&payload).context("encode ChangeThresholdInvitePayload")?;
    let invite_message = Message::new(MessageType::InviteChangeThreshold, payload_bytes);

    for peer in &network_config.peers {
        if !invitee_set.contains(&peer.participant_id)
            || peer.participant_id == *current_participant_id
        {
            continue;
        }

        if peer.public_key.is_empty() {
            tracing::warn!(
                "Skipping change-threshold invite to {} - no public key configured",
                peer.participant_id
            );
            continue;
        }
        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!(
                    "Skipping change-threshold invite to {} - invalid public key: {e}",
                    peer.participant_id
                );
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);
        let identity = config.participant_id().to_string();

        tracing::info!(
            "Sending change-threshold invite to {} at {}:{}",
            peer.participant_id,
            peer.address,
            peer.port
        );

        match send_noise_message(
            &peer.address,
            peer.port,
            &psk,
            identity.as_bytes(),
            &invite_message,
        )
        .await
        {
            Ok(response) => {
                interpret_invite_reply(&peer.participant_id, "change-threshold", &response)?;
            }
            Err(e) => {
                tracing::error!(
                    "Failed to send change-threshold invite to {}: {e}",
                    peer.participant_id
                );
            }
        }
    }

    Ok(())
}

// ============================================================================
// Rotate-Key Workflow
// ============================================================================

/// Start a rotate-key workflow: replace this node's Daml signing key for a
/// decentralized party with a new key from its vault.
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
    let party = body.decentralized_party_id.clone();
    tracing::info!("Rotate-key request received: party={party}");

    let members: HashSet<CantonId> = match data.db.get_dec_party_participants(&party).await {
        Ok(rows) => rows
            .iter()
            .filter_map(|r| CantonId::parse(&r.participant_uid).ok())
            .collect(),
        Err(e) => {
            tracing::error!("Failed to load dec party participants for rotate-key: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to load decentralized party members".to_string(),
            });
        }
    };
    if members.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "No cached membership for {party}. Try refreshing /decentralized-parties first."
            ),
        });
    }
    if !members.contains(data.config.participant_id()) {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "This node does not host {party}, so it holds no Daml key of it to rotate"
            ),
        });
    }
    if members.len() < 2 {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!("Cannot rotate a key of {party}: it has only one member"),
        });
    }

    if let Err(e) = require_kms_for_rotation(&data.config, &data.db).await {
        let error = format!("{e:#}");
        return if canton_unreachable(&e) {
            HttpResponse::BadGateway().json(ErrorResponse { error })
        } else {
            HttpResponse::BadRequest().json(ErrorResponse { error })
        };
    }
    let rotation = match rotation_baseline(&data.config, &data.db, &party, &members).await {
        Ok(rotation) => rotation,
        Err(e) => {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: format!("{e:#}"),
            });
        }
    };
    #[cfg(feature = "test-mode")]
    let rotation = match injected_rotated_key().await {
        Some(rotated_key) => RotationBaseline {
            rotated_key,
            ..rotation
        },
        None => rotation,
    };

    let invitees: Vec<CantonId> = match data.db.get_all_peers().await {
        Ok(peers) => peers
            .into_iter()
            .map(|p| p.participant_id)
            .filter(|p| members.contains(p) && p != data.config.participant_id())
            .collect(),
        Err(e) => {
            tracing::error!("Failed to load peers for rotate-key: {e}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to load peers".to_string(),
            });
        }
    };
    // Every other member must record the new key. A member that is not a
    // configured peer would keep the old key on record for this node.
    let unreachable = members_missing_from_peers(&members, &invitees, data.config.participant_id());
    if !unreachable.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "Cannot rotate this node's key of {party}: every other member must record the new \
                 key, but these members are not configured peers: {list}. Add them as peers first",
                list = unreachable.join(", ")
            ),
        });
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

    if let Some((run, kind)) = find_inprogress_run_for_party(&data.db, &party).await {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "Party {party} already has a {kind} workflow in flight (run {run}); wait for it \
                 to finish or cancel it first"
            ),
        });
    }
    let health = probe_invitee_health(&data.config, &data.db, &invitees).await;
    let incompatible = incompatible_versions(&health);
    if !incompatible.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format_incompatible_peers(&incompatible),
        });
    }
    // An older peer cannot decode the invitation, so the run would stall
    // after the invites went out.
    let lacking = lacking_feature(&health, ROTATE_KEY);
    if !lacking.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format_peers_lacking_rotate_key(&lacking),
        });
    }
    if let Err(resp) = register_and_persist(
        &data,
        &instance,
        RotateKeyStep::WaitingForPeers,
        &rotate_config,
        &invitees,
        Some(party.clone()),
    )
    .await
    {
        return resp;
    }

    let state = &instance.http;
    *state.invited_peers.write().await = invitees.clone();

    let config = data.config.clone();
    let db = data.db.clone();
    let state_clone = instance.http.clone();
    let instance_for_coord = instance.clone();
    let kind = instance.kind;
    let workflows = data.workflows.clone();
    let last_seen = data.last_seen.clone();
    let instance_for_task = instance_name.clone();

    // abort_handle/status/error are flipped under simultaneously-held locks —
    // see start_dars for the cancel-race rationale.
    let mut abort_guard = state.abort_handle.lock().await;
    let mut status_guard = state.status.write().await;
    let mut error_guard = state.error.write().await;

    let join_handle = tokio::spawn(async move {
        let _workflow_guard = WorkflowGuard::new(workflows, instance_for_task.clone());

        if let Err(e) = send_rotate_key_invites(&config, &db, &rotate_config, &invitees).await {
            tracing::error!("Failed to send rotate-key invites: {e}");
            let msg = format!("Failed to send invites: {e}");
            mark_run_failed(&db, kind, &instance_for_task, &msg).await;
            *state_clone.status.write().await = WorkflowProgress::Failed;
            *state_clone.error.write().await = Some(msg);
            return;
        }

        // Give peers time to start their peer workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let result = workflow::start_coordinator(
            config,
            db.clone(),
            workflow::WorkflowType::RotateKey,
            None, // No onboarding config
            None, // No kick config
            None, // No contracts config
            None, // No dars config
            None, // No add-party config
            None, // No change-threshold config
            Some(rotate_config),
            None, // No auth registry for rotate-key
            last_seen,
            instance_for_coord,
        )
        .await;

        // Durable row first, in-memory status second: see start_change_threshold.
        match result {
            Ok(_) => {
                mark_run_completed(&db, kind, &instance_for_task).await;
                *state_clone.status.write().await = WorkflowProgress::Completed;
                tracing::info!("Rotate-key workflow completed successfully");
            }
            Err(e) => {
                let msg = format!("{e}");
                mark_run_failed(&db, kind, &instance_for_task, &msg).await;
                *state_clone.status.write().await = WorkflowProgress::Failed;
                *state_clone.error.write().await = Some(msg);
                tracing::error!("Rotate-key workflow failed: {e}");
            }
        }
    });
    *abort_guard = Some(join_handle.abort_handle());
    *status_guard = WorkflowProgress::InProgress;
    *error_guard = None;
    drop(error_guard);
    drop(status_guard);
    drop(abort_guard);

    HttpResponse::Accepted().json(WorkflowResponse {
        status: WorkflowProgress::InProgress,
        message: "Rotate-key workflow started".to_string(),
        instance_name,
    })
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
        .any(|cause| cause.is::<tonic::Status>() || cause.is::<tonic::transport::Error>())
}

/// The hosts that differ between the cached members and the chain, or `None`
/// when both name the same hosts.
fn stale_members(cached: &HashSet<CantonId>, on_chain: &[String]) -> Option<String> {
    let cached: std::collections::BTreeSet<String> =
        cached.iter().map(ToString::to_string).collect();
    let on_chain: std::collections::BTreeSet<String> = on_chain.iter().cloned().collect();
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

/// Send rotate-key invites over Noise to every other member.
async fn send_rotate_key_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    rotate_config: &workflow::RotateKeyConfig,
    invitees: &[CantonId],
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let mut participants = invitees.to_vec();
    participants.push(config.participant_id().clone());
    let payload = RotateKeyInvitePayload {
        dec_party_id: rotate_config.decentralized_party_id.clone(),
        rotated_key: rotate_config.rotated_key.to_string(),
        threshold: rotate_config.threshold,
        confirmation_threshold: rotate_config.confirmation_threshold,
        participants,
        workflow_instance: Some(rotate_config.instance_name.clone()),
    };
    let invite_message = Message::new(
        MessageType::InviteRotateKey,
        serde_json::to_vec(&payload).context("encode RotateKeyInvitePayload")?,
    );

    let invitee_set: HashSet<&CantonId> = invitees.iter().collect();
    for peer in &network_config.peers {
        if !invitee_set.contains(&peer.participant_id) {
            continue;
        }
        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!(
                    "Skipping rotate-key invite to {} - invalid public key: {e}",
                    peer.participant_id
                );
                continue;
            }
        };
        let psk = keypair.derive_psk(&peer_pub_key);
        let identity = config.participant_id().to_string();
        tracing::info!("Sending rotate-key invite to {}", peer.participant_id);
        match send_noise_message(
            &peer.address,
            peer.port,
            &psk,
            identity.as_bytes(),
            &invite_message,
        )
        .await
        {
            Ok(response) => interpret_invite_reply(&peer.participant_id, "rotate-key", &response)?,
            Err(e) => tracing::error!(
                "Failed to send rotate-key invite to {}: {e}",
                peer.participant_id
            ),
        }
    }
    Ok(())
}

// ============================================================================
// External-Party Workflow
// ============================================================================

/// Validate the requested confirmation threshold for a decentrally-hosted
/// external party. The hosting set is the local participant plus the hosting
/// peers, and the threshold is capped at `N-1`: Canton requires the threshold to
/// be at most the number of confirming hosts remaining after a change, so a
/// threshold of `N` would block any later unhost. Reject 0 and anything above
/// `N-1` up-front instead of failing deep in a Canton proto error. An unset
/// value passes here and is defaulted to `N-1` by
/// [`prepare_topology`](crate::workflow::external_party::steps::prepare_topology),
/// so it also stays within the cap.
pub(crate) fn validate_confirmation_threshold(
    threshold: Option<u32>,
    num_hosts: usize,
) -> Result<(), String> {
    let Some(t) = threshold else { return Ok(()) };
    let max = num_hosts.saturating_sub(1) as u32;
    if t < 1 || t > max {
        return Err(format!(
            "confirmation_threshold must be between 1 and {max} (one less than the {num_hosts} \
             hosting participants, so a host can still exit); got {t}"
        ));
    }
    Ok(())
}

// ============================================================================
// Onboarding Workflow
// ============================================================================

/// Start an onboarding workflow to create a new decentralized party
#[utoipa::path(
    tag = "Workflows",
    request_body = OnboardingRequest,
    responses(
        (status = 202, description = "Onboarding workflow started", body = WorkflowResponse),
        (status = 400, description = "Invalid party prefix, threshold or confirmation threshold", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress", body = ErrorResponse),
        (status = 422, description = "Selected peers are not mutually meshed", body = OnboardingMeshErrorResponse)
    )
)]
#[post("/onboarding")]
pub async fn start_onboarding(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<OnboardingRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }

    // Reject an invalid party prefix up-front: it becomes the identifier part
    // of the Canton party id (`<prefix>::<namespace>`), and a bad character
    // would otherwise fail deep in the workflow as an opaque Canton proto
    // deserialization error ~90s later. Fail fast with a clear 400 instead.
    if let Err(msg) = validate_party_id_prefix(&body.party_id_prefix) {
        return HttpResponse::BadRequest().json(ErrorResponse { error: msg });
    }

    // Pre-flight: every selected peer must have every other selected peer in
    // its network config, otherwise the coordinator workflow will hang waiting
    // for peer connections that can never be established.
    match verify_peer_mesh(&data.config, &data.db, &body.peer_ids).await {
        Ok(missing) if !missing.is_empty() => {
            // Tag each edge with its failure mode in the human-readable
            // summary; the structured `missing_edges` array carries the same
            // info via `kind` for the frontend to render targeted hints.
            let edge_summary = missing
                .iter()
                .map(|e| match e.kind {
                    MissingEdgeKind::UnreachableFromCoordinator => format!(
                        "[unreachable from coordinator] {from} → {to}",
                        from = e.from,
                        to = e.to
                    ),
                    MissingEdgeKind::MeshHole => {
                        format!("[mesh hole] {from} → {to}", from = e.from, to = e.to)
                    }
                })
                .collect::<Vec<_>>()
                .join("; ");
            tracing::warn!(
                "Onboarding rejected: {n} missing peer mesh edge(s): {edge_summary}",
                n = missing.len()
            );
            return HttpResponse::UnprocessableEntity().json(OnboardingMeshErrorResponse {
                error: format!(
                    "Could not verify a full peer mesh. Two failure modes are folded together: \
                     `unreachable from coordinator` (the coordinator cannot query that peer — \
                     it's missing from network config, has no/invalid public key, didn't \
                     answer, or replied with an unparsable peer list — fix the coordinator's \
                     view of `to`, or `to` itself), and `mesh hole` (peer `from` is reachable \
                     but does not have peer `to` in its peer list — add `to` to `from`'s \
                     network config). Edges: {edge_summary}"
                ),
                missing_edges: missing,
            });
        }
        Ok(_) => {}
        Err(e) => {
            tracing::error!("Failed to run mesh pre-flight: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Failed to verify peer mesh".into(),
            });
        }
    }

    // Build instance + config up-front so the persisted workflow_runs row
    // can carry the same identifier as the spawned coordinator task.
    let party_id_prefix = body.party_id_prefix.clone();
    let peer_ids = body.peer_ids.clone();
    let instance_name = format!("{party_id_prefix}-creation");
    let instance = WorkflowInstance::new(
        instance_name.clone(),
        WorkflowKind::Onboarding,
        WorkflowRole::Coordinator,
    );
    let onboarding_state = &instance.http;

    // If the operator set an explicit initial threshold, bound it by the owner
    // count (invited peers + this coordinator). Omitted => the coordinator uses
    // the default majority algorithm once the owner set is resolved.
    let owner_count = peer_ids.len() as i32 + 1;
    if let Some(t) = body.threshold
        && !(1..=owner_count).contains(&t)
    {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!(
                "threshold must be between 1 and {owner_count} (invited peers + this node); got {t}"
            ),
        });
    }
    // Every owner's node also hosts the party, so the same count bounds the
    // confirmation threshold.
    if let Some(confirmation) = body.confirmation_threshold
        && let Err(e) = check_confirmation_threshold(confirmation, owner_count as usize)
    {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: e.to_string(),
        });
    }

    let onboarding_config = workflow::OnboardingConfig::new(
        party_id_prefix.clone(),
        instance_name.clone(),
        body.threshold,
        body.confirmation_threshold,
    );

    // Refuse onboarding when a party with this prefix already exists. The
    // human-readable prefix is the only piece of the party id the operator
    // chooses; allowing duplicates makes the parties list ambiguous and —
    // when the participant set also matches — the workflow silently
    // converges onto the existing party (Canton's DNS hash is deterministic
    // from owners + threshold). Surface a clear 409 upfront instead.
    match find_party_with_prefix(&data.db, &party_id_prefix).await {
        Ok(Some(existing_party_id)) => {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: format!(
                    "A decentralized party with the prefix '{party_id_prefix}' already exists \
                     ({existing_party_id}). Choose a different prefix."
                ),
            });
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!("Failed to check for duplicate-prefix party: {e:#}");
        }
    }

    // Refuse to invite any peer that can't speak the concurrent-workflows wire
    // format — NO invites are sent if even one invitee fails the version gate.
    let incompatible = preflight_incompatible_peers(&data.config, &data.db, &peer_ids).await;
    if !incompatible.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format_incompatible_peers(&incompatible),
        });
    }

    // Onboarding's instance_name is deterministic (`{prefix}-creation`), so a
    // racing same-prefix request is realistic — register_and_persist's registry
    // insert rejects it before the upsert can clobber the in-flight run's row.
    if let Err(resp) = register_and_persist(
        &data,
        &instance,
        OnboardingStep::WaitingForPeers,
        &onboarding_config,
        &peer_ids,
        None,
    )
    .await
    {
        return resp;
    }

    let config = data.config.clone();
    let db = data.db.clone();
    let onboarding_state_clone = instance.http.clone();
    let instance_for_coord = instance.clone();
    let kind = instance.kind;
    let workflows = data.workflows.clone();
    *onboarding_state.invited_peers.write().await = peer_ids.clone();
    let party_credentials = data.party_credentials.clone();
    let auth_lock = data.auth.clone();
    let discovery_gate = super::DiscoveryGate::of(&data);
    let last_seen = data.last_seen.clone();
    let instance_for_task = instance_name.clone();

    // See start_dars below for the rationale: abort_handle, status, and error
    // are flipped under simultaneously-held locks so a concurrent
    // /onboarding/cancel can never observe "status=InProgress + abort_handle=None"
    // and bail.
    let mut abort_guard = onboarding_state.abort_handle.lock().await;
    let mut status_guard = onboarding_state.status.write().await;
    let mut error_guard = onboarding_state.error.write().await;

    let join_handle = tokio::spawn(async move {
        let _workflow_guard = WorkflowGuard::new(workflows, instance_for_task.clone());

        // Send invites to selected peers before starting coordinator workflow
        let invite_result = send_onboarding_invites(
            &config,
            &db,
            &peer_ids,
            &party_id_prefix,
            onboarding_config.threshold,
            onboarding_config.confirmation_threshold,
            &instance_for_task,
        )
        .await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send onboarding invites: {e}");
            let msg = format!("Failed to send invites: {e}");
            {
                let mut status = onboarding_state_clone.status.write().await;
                let mut error = onboarding_state_clone.error.write().await;
                *status = OnboardingStatus::Failed;
                *error = Some(msg.clone());
            }
            mark_run_failed(&db, kind, &instance_for_task, &msg).await;
            return;
        }

        // Give peers time to start their peer workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let result = workflow::start_coordinator(
            config.clone(),
            db.clone(),
            workflow::WorkflowType::Onboarding,
            Some(onboarding_config),
            None, // No kick config
            None, // No contracts config
            None, // No dars config
            None, // No add-party config
            None, // No change-threshold config
            None, // No rotate-key config
            None, // No auth registry for onboarding
            last_seen,
            instance_for_coord,
        )
        .await;

        // Update in-memory state in tight scopes — never hold the RwLock
        // across a DB await. /onboarding/status acquires a read lock to
        // serve every poll; if a writer holds the lock during the DB write,
        // every concurrent read blocks for that duration on a slow runner.
        match result {
            Ok(_) => {
                {
                    let mut status = onboarding_state_clone.status.write().await;
                    *status = OnboardingStatus::Completed;
                }
                tracing::info!("Onboarding workflow completed successfully");
                mark_run_completed(&db, kind, &instance_for_task).await;
                // Operator configures party credentials via the Party Configuration
                // dialog; auto-saving placeholders pollutes the auth registry.

                // Refresh dec_party cache in background
                let bg_config = config.clone();
                let bg_db = db.clone();
                let bg_auth = auth_lock.clone();
                let bg_creds = party_credentials.clone();
                let bg_gate = discovery_gate.clone();
                tokio::spawn(async move {
                    let auth = bg_auth.read().await.clone();
                    let creds = bg_creds.read().await.clone();
                    // Through the shared gate: a direct discovery slips the
                    // concurrency bound and races the cache write against a
                    // request for the same prefix.
                    match super::discover_and_cache(
                        &bg_gate,
                        &bg_config,
                        &bg_db,
                        "",
                        auth,
                        &creds,
                        Default::default(),
                    )
                    .await
                    {
                        super::Discovery::Done(resp) => {
                            resolve_owner_keys_from_peers(&bg_config, &bg_db, &resp.parties).await;
                            // Audit: report any participants whose owner_key
                            // is still NULL after resolve. Not fatal — Noise
                            // resolution may run again on the next stale
                            // refresh — but unexpected for a freshly
                            // onboarded party.
                            for party in &resp.parties {
                                for p in &party.participants {
                                    let uid = p.participant_uid.to_string();
                                    match bg_db
                                        .get_dec_party_participant_owner_key(&party.party_id, &uid)
                                        .await
                                    {
                                        Ok(Some(_)) => {} // resolved
                                        Ok(None) => tracing::warn!(
                                            party_id = %party.party_id,
                                            participant_uid = %uid,
                                            "Participant owner_key unresolved after onboarding"
                                        ),
                                        Err(e) => tracing::warn!(
                                            party_id = %party.party_id,
                                            participant_uid = %uid,
                                            error = %e,
                                            "Failed to read owner_key from cache during post-onboarding audit"
                                        ),
                                    }
                                }
                            }
                        }
                        super::Discovery::InFlight
                        | super::Discovery::AtCapacity
                        | super::Discovery::Superseded => {
                            tracing::debug!("Post-onboarding refresh skipped: already running");
                        }
                        super::Discovery::Failed(e) => {
                            tracing::warn!("Failed to refresh parties after onboarding: {e}");
                        }
                    }
                });
            }
            Err(e) => {
                let msg = format!("{e}");
                {
                    let mut status = onboarding_state_clone.status.write().await;
                    let mut error = onboarding_state_clone.error.write().await;
                    *status = OnboardingStatus::Failed;
                    *error = Some(msg.clone());
                }
                tracing::error!("Onboarding workflow failed: {e}");
                mark_run_failed(&db, kind, &instance_for_task, &msg).await;
            }
        }
    });
    *abort_guard = Some(join_handle.abort_handle());
    *status_guard = OnboardingStatus::InProgress;
    *error_guard = None;
    drop(error_guard);
    drop(status_guard);
    drop(abort_guard);

    HttpResponse::Accepted().json(OnboardingResponse {
        status: OnboardingStatus::InProgress,
        message: "Onboarding workflow started".to_string(),
        instance_name,
    })
}

/// Get the current status of the onboarding workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Onboarding workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/onboarding/status")]
pub async fn get_onboarding_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(kind_status(&data, WorkflowKind::Onboarding).await)
}

/// Interpret a peer's reply to a workflow invite.
///
/// Current peers accept invites unconditionally (workflows are multi-instance
/// now — see the invite arm in `server::mod`), so a current peer always replies
/// `Ack`, and a pre-`Ack` build is refused by the version gate before it can
/// reach an invite. The `Busy` arm is therefore a defensive safety net no
/// current-version peer triggers: were a peer ever to report `Busy`, treating it
/// as a hard error makes the coordinator fail fast (the spawning task marks the
/// run Failed with a clear reason) instead of waiting on a peer that will never
/// join. Any other non-`Ack` reply, or an unparseable one, is logged but
/// tolerated.
fn interpret_invite_reply(peer_id: &CantonId, kind: &str, response: &[u8]) -> Result {
    match Message::from_bytes(response) {
        Ok(msg) if msg.msg_type == MessageType::Ack => {
            tracing::info!("Peer {peer_id} acknowledged {kind} invite");
            Ok(())
        }
        Ok(msg) if msg.msg_type == MessageType::Busy => {
            anyhow::bail!(
                "Peer {peer_id} is already participating in another workflow; \
                 aborting {kind} before it starts"
            )
        }
        Ok(msg) => {
            tracing::warn!(
                "Peer {peer_id} responded with {msg_type:?} instead of Ack to {kind} invite",
                msg_type = msg.msg_type
            );
            Ok(())
        }
        Err(_) => {
            tracing::warn!("Peer {peer_id} sent an unparseable {kind} invite reply");
            Ok(())
        }
    }
}

/// Send onboarding invites to selected peers using Noise protocol
async fn send_onboarding_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
    party_id_prefix: &str,
    threshold: Option<i32>,
    confirmation_threshold: Option<i32>,
    instance_name: &str,
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    // Show the threshold the coordinator will actually use: the operator's
    // choice, or the same majority default it falls back to (owners = invited
    // peers + this coordinator).
    let owners = peer_ids.len() + 1;
    let effective_threshold = threshold.unwrap_or_else(|| owners.div_ceil(2).max(1) as i32);

    let payload = OnboardingInvitePayload {
        prefix: party_id_prefix.to_string(),
        participants: peer_ids.to_vec(),
        threshold: Some(effective_threshold),
        confirmation_threshold,
        workflow_instance: Some(instance_name.to_string()),
    };
    let payload_bytes = serde_json::to_vec(&payload)?;
    let invite_message = Message::new(MessageType::InviteOnboarding, payload_bytes);

    for peer_id in peer_ids {
        let peer = match network_config
            .peers
            .iter()
            .find(|p| &p.participant_id == peer_id)
        {
            Some(p) => p,
            None => {
                tracing::warn!("Skipping invite to {peer_id} - peer not found in network config");
                continue;
            }
        };

        if peer.public_key.is_empty() {
            tracing::warn!("Skipping invite to {peer_id} - no public key configured");
            continue;
        }

        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!("Skipping invite to {peer_id} - invalid public key: {e}");
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);
        // Use participant_id as identity (must match what server expects in peer_keys lookup)
        let identity = config.participant_id().to_string();

        tracing::info!(
            "Sending onboarding invite to {peer_id} at {addr}:{port}",
            addr = peer.address,
            port = peer.port
        );

        match send_noise_message(
            &peer.address,
            peer.port,
            &psk,
            identity.as_bytes(),
            &invite_message,
        )
        .await
        {
            Ok(response) => {
                interpret_invite_reply(peer_id, "onboarding", &response)?;
            }
            Err(e) => {
                tracing::error!("Failed to send invite to {peer_id}: {e}");
            }
        }
    }

    Ok(())
}

/// Pre-flight check: query each selected peer for its known peer list and
/// verify every pair within `peer_ids` knows each other. Returns the list of
/// missing directed edges (`from` does not have `to` configured). Empty Vec
/// on success.
///
/// Coordinator → peer reachability is implicitly verified: if a peer can
/// answer our Noise call, it already has us in its peer list. We only check
/// peer ↔ peer mesh — that's the case the user can't see otherwise.
async fn verify_peer_mesh(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
) -> Result<Vec<MissingPeerEdge>> {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;
    let request = Message::new_empty(MessageType::ListPeers);
    let identity = config.participant_id().to_string();

    let mut missing_edges = Vec::new();
    let mut peer_views: HashMap<String, HashSet<String>> = HashMap::new();

    for peer_id in peer_ids {
        let peer = match network_config
            .peers
            .iter()
            .find(|p| &p.participant_id == peer_id)
        {
            Some(p) => p,
            None => {
                // Coordinator doesn't know this peer — won't be able to invite them.
                missing_edges.push(MissingPeerEdge {
                    from: identity.clone(),
                    to: peer_id.to_string(),
                    kind: MissingEdgeKind::UnreachableFromCoordinator,
                });
                continue;
            }
        };

        if peer.public_key.is_empty() {
            tracing::warn!("Peer {peer_id} has no public key configured — cannot mesh-check");
            missing_edges.push(MissingPeerEdge {
                from: identity.clone(),
                to: peer_id.to_string(),
                kind: MissingEdgeKind::UnreachableFromCoordinator,
            });
            continue;
        }

        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!("Peer {peer_id} has invalid public key: {e}");
                missing_edges.push(MissingPeerEdge {
                    from: identity.clone(),
                    to: peer_id.to_string(),
                    kind: MissingEdgeKind::UnreachableFromCoordinator,
                });
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);

        let response = match send_noise_message(
            &peer.address,
            peer.port,
            &psk,
            identity.as_bytes(),
            &request,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Failed to query peers from {peer_id}: {e}");
                missing_edges.push(MissingPeerEdge {
                    from: identity.clone(),
                    to: peer_id.to_string(),
                    kind: MissingEdgeKind::UnreachableFromCoordinator,
                });
                continue;
            }
        };

        let msg = match Message::from_bytes(&response) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("Malformed response from {peer_id}: {e}");
                missing_edges.push(MissingPeerEdge {
                    from: identity.clone(),
                    to: peer_id.to_string(),
                    kind: MissingEdgeKind::UnreachableFromCoordinator,
                });
                continue;
            }
        };

        if msg.msg_type != MessageType::PeerList {
            tracing::warn!(
                "Peer {peer_id} responded with {msg_type:?} instead of PeerList",
                msg_type = msg.msg_type
            );
            missing_edges.push(MissingPeerEdge {
                from: identity.clone(),
                to: peer_id.to_string(),
                kind: MissingEdgeKind::UnreachableFromCoordinator,
            });
            continue;
        }

        let view: HashSet<String> = match serde_json::from_slice(&msg.payload) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Could not parse peer list from {peer_id}: {e}");
                missing_edges.push(MissingPeerEdge {
                    from: identity.clone(),
                    to: peer_id.to_string(),
                    kind: MissingEdgeKind::UnreachableFromCoordinator,
                });
                continue;
            }
        };
        peer_views.insert(peer_id.to_string(), view);
    }

    // For every directed pair within selected peers, check that A's peer view
    // includes B. Missing means A and B aren't mutually connected.
    for a in peer_ids {
        let Some(a_view) = peer_views.get(&a.to_string()) else {
            // Already recorded a coordinator→A reachability problem above.
            continue;
        };
        for b in peer_ids {
            if a == b {
                continue;
            }
            if !a_view.contains(&b.to_string()) {
                missing_edges.push(MissingPeerEdge {
                    from: a.to_string(),
                    to: b.to_string(),
                    kind: MissingEdgeKind::MeshHole,
                });
            }
        }
    }

    Ok(missing_edges)
}

// ============================================================================
// Contracts Workflow
// ============================================================================

/// Start a contracts workflow to upload DARs and create contracts
#[utoipa::path(
    tag = "Workflows",
    request_body = ContractsRequest,
    responses(
        (status = 202, description = "Contracts workflow started", body = WorkflowResponse),
        (status = 400, description = "No contracts to deploy, or the deployment cannot be resolved", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress", body = ErrorResponse)
    )
)]
#[post("/contracts")]
pub async fn start_contracts(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<ContractsRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    // Peers sign one prepared create per contract, so an empty run would
    // invite them and then fail on every one of them at signing.
    if body.contracts.is_empty() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "The request lists no contracts to deploy".to_string(),
        });
    }

    // Create contracts config from request
    let timestamp = now_secs();
    let instance_name = format!(
        "{}-contracts-{timestamp}",
        body.decentralized_party_id.prefix
    );
    let mut contracts_config = workflow::ContractsConfig::new(
        body.decentralized_party_id.clone(),
        body.participant_ids.clone(),
        body.participant_parties.clone(),
        body.operator_party.clone(),
        body.contracts.clone(),
        instance_name,
    );

    // Scope to the party's actual signers, not every peer this node knows
    // about. A dec party can be a strict subset of the configured mesh (see
    // start_onboarding), so inviting/waiting for non-member peers would pull
    // outsiders into the deployment or stall WaitingForPeers on a peer that
    // never joins. Keep only configured peers that are this party's
    // participants, minus self.
    let participant_set: HashSet<CantonId> = body.participant_ids.iter().cloned().collect();
    let contracts_invitees: Vec<CantonId> = match data.db.get_all_peers().await {
        Ok(peers) => peers
            .into_iter()
            .map(|p| p.participant_id)
            .filter(|p| p != data.config.participant_id() && participant_set.contains(p))
            .collect(),
        Err(e) => {
            tracing::warn!("Failed to load peers for cancel-invite tracking: {e}");
            Vec::new()
        }
    };

    let instance_name_for_run = contracts_config.instance_name.clone();
    let instance = WorkflowInstance::new(
        instance_name_for_run.clone(),
        WorkflowKind::Contracts,
        WorkflowRole::Coordinator,
    );
    let contracts_state = &instance.http;
    // Refuse a second workflow targeting the SAME decentralized party (see
    // `find_inprogress_run_for_party`).
    if let Some((run, kind)) =
        find_inprogress_run_for_party(&data.db, &body.decentralized_party_id).await
    {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "Party {} already has a {kind} workflow in flight (run {run}); wait for it \
                 to finish or cancel it first",
                body.decentralized_party_id
            ),
        });
    }

    // Refuse to invite any peer that can't speak the concurrent-workflows wire
    // format — NO invites are sent if even one invitee fails the version gate.
    let incompatible =
        preflight_incompatible_peers(&data.config, &data.db, &contracts_invitees).await;
    if !incompatible.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format_incompatible_peers(&incompatible),
        });
    }

    // Resolve aliases once, before persisting or inviting. Preparation and
    // retries must use the same package IDs the operators approved.
    if let Err(error) = workflow::contracts::steps::prepare::resolve_package_ids(
        &data.config,
        &mut contracts_config.contracts,
    )
    .await
    {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!("Cannot resolve deployment packages: {error:#}"),
        });
    }

    // Commit to the exact creates before persisting or inviting. Peers verify
    // the prepared transactions against these, including on a resumed run.
    match workflow::contracts::steps::prepare::commit_deployment(&data.db, &contracts_config).await
    {
        Ok(committed) => contracts_config.committed_deployment = Some(committed),
        Err(error) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("Cannot commit the deployment: {error:#}"),
            });
        }
    }

    // Register + persist atomically w.r.t. duplicates (registry insert dedups
    // before the upsert; a persist failure unregisters so nothing leaks).
    if let Err(resp) = register_and_persist(
        &data,
        &instance,
        ContractsStep::WaitingForPeers,
        &contracts_config,
        &contracts_invitees,
        Some(body.decentralized_party_id.clone()),
    )
    .await
    {
        return resp;
    }

    let config = data.config.clone();
    let db = data.db.clone();
    let workflow_auth = data.auth.read().await.clone();
    let auth_lock = data.auth.clone();
    let discovery_gate = super::DiscoveryGate::of(&data);
    let contracts_state_clone = instance.http.clone();
    let instance_for_coord = instance.clone();
    let kind = instance.kind;
    let workflows = data.workflows.clone();
    let party_credentials = data.party_credentials.clone();
    let last_seen = data.last_seen.clone();
    let contracts_invitees_for_invites = contracts_invitees.clone();
    *contracts_state.invited_peers.write().await = contracts_invitees;
    let instance_for_task = instance_name_for_run.clone();

    // See start_dars below for the rationale: abort_handle, status, and error
    // are flipped under simultaneously-held locks so a concurrent
    // /contracts/cancel can never observe "status=InProgress + abort_handle=None"
    // and bail.
    let mut abort_guard = contracts_state.abort_handle.lock().await;
    let mut status_guard = contracts_state.status.write().await;
    let mut error_guard = contracts_state.error.write().await;

    let join_handle = tokio::spawn(async move {
        let _workflow_guard = WorkflowGuard::new(workflows, instance_for_task.clone());

        // Send invites to this party's members before starting coordinator workflow
        let invite_result = send_contracts_invites(
            &config,
            &db,
            &contracts_config,
            &contracts_invitees_for_invites,
        )
        .await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send contracts invites: {e}");
            let msg = format!("Failed to send invites: {e}");
            {
                let mut status = contracts_state_clone.status.write().await;
                let mut error = contracts_state_clone.error.write().await;
                *status = WorkflowProgress::Failed;
                *error = Some(msg.clone());
            }
            mark_run_failed(&db, kind, &instance_for_task, &msg).await;
            return;
        }

        // Give peers time to start their peer workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let result = workflow::start_coordinator(
            config.clone(),
            db.clone(),
            workflow::WorkflowType::Contracts,
            None, // No onboarding config
            None, // No kick config
            Some(contracts_config.clone()),
            None, // No dars config
            None, // No add-party config
            None, // No change-threshold config
            None, // No rotate-key config
            workflow_auth,
            last_seen,
            instance_for_coord,
        )
        .await;

        // Update in-memory state in tight scopes — never hold the RwLock
        // across a DB await. /contracts/status acquires a read lock to
        // serve every poll; if a writer holds the lock during the DB write,
        // every concurrent read blocks for that duration on a slow runner.
        match result {
            Ok(_) => {
                // The run row is marked terminal BEFORE the in-memory status
                // flips. The two are read by different callers — `/…/status`
                // reads this lock, while the "already has a workflow in flight"
                // guard scans the run rows — so flipping memory first opens a
                // window where the API reports a run finished and refuses the
                // next workflow for that party in the same breath. Doing the
                // durable write first can only report a finished run as still
                // running for an instant, which costs a poll rather than a
                // rejected request.
                mark_run_completed(&db, kind, &instance_for_task).await;
                {
                    let mut status = contracts_state_clone.status.write().await;
                    *status = WorkflowProgress::Completed;
                }
                tracing::info!("Contracts workflow completed successfully");

                // Refresh dec_party cache to pick up new contracts
                let bg_config = config.clone();
                let bg_db = db.clone();
                let bg_auth = auth_lock.clone();
                let bg_creds = party_credentials.clone();
                let bg_gate = discovery_gate.clone();
                tokio::spawn(async move {
                    let auth = bg_auth.read().await.clone();
                    let creds = bg_creds.read().await.clone();
                    // Through the shared gate, as above.
                    match super::discover_and_cache(
                        &bg_gate,
                        &bg_config,
                        &bg_db,
                        "",
                        auth,
                        &creds,
                        Default::default(),
                    )
                    .await
                    {
                        super::Discovery::Done(resp) => {
                            resolve_owner_keys_from_peers(&bg_config, &bg_db, &resp.parties).await;
                        }
                        super::Discovery::InFlight
                        | super::Discovery::AtCapacity
                        | super::Discovery::Superseded => {
                            tracing::debug!("Post-contracts refresh skipped: already running");
                        }
                        super::Discovery::Failed(e) => {
                            tracing::warn!(
                                "Failed to refresh parties after contract deployment: {e}"
                            );
                        }
                    }
                });
            }
            Err(e) => {
                let msg = format!("{e}");
                // Durable row first, in-memory status second — same ordering as the
                // success path above, and for the same reason: a caller that reads
                // Failed from `/…/status` and retries must not be refused by the
                // in-flight guard, which scans the run rows.
                mark_run_failed(&db, kind, &instance_for_task, &msg).await;
                {
                    let mut status = contracts_state_clone.status.write().await;
                    let mut error = contracts_state_clone.error.write().await;
                    *status = WorkflowProgress::Failed;
                    *error = Some(msg.clone());
                }
                tracing::error!("Contracts workflow failed: {e}");
            }
        }
    });
    *abort_guard = Some(join_handle.abort_handle());
    *status_guard = WorkflowProgress::InProgress;
    *error_guard = None;
    drop(error_guard);
    drop(status_guard);
    drop(abort_guard);

    HttpResponse::Accepted().json(WorkflowResponse {
        status: WorkflowProgress::InProgress,
        message: "Contracts workflow started".to_string(),
        instance_name: instance_name_for_run,
    })
}

/// Get the current status of the contracts workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Contracts workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/contracts/status")]
pub async fn get_contracts_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(kind_status(&data, WorkflowKind::Contracts).await)
}

// ============================================================================
// DARs Upload (Local)
// ============================================================================

/// Upload DAR files to the current node only (no distribution to peers)
#[utoipa::path(
    tag = "Workflows",
    request_body = DarsRequest,
    responses(
        (status = 200, description = "DARs uploaded to local node", body = SuccessResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 500, description = "Upload failed", body = ErrorResponse)
    )
)]
#[post("/dars/upload")]
pub async fn upload_dars_local(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<DarsRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    match workflow::contracts::upload_dars(&data.config, &body.dar_files).await {
        Ok(()) => {
            tracing::info!(
                "Uploaded {} DAR file(s) to local node",
                body.dar_files.len()
            );
            HttpResponse::Ok().json(SuccessResponse { success: true })
        }
        Err(e) => {
            tracing::error!("Failed to upload DARs to local node: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to upload DARs: {e}"),
            })
        }
    }
}

// ============================================================================
// DARs Distribution Workflow
// ============================================================================

/// Distribute DARs across all participants via Noise protocol
#[utoipa::path(
    tag = "Workflows",
    request_body = DarsRequest,
    responses(
        (status = 202, description = "DARs distribution workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request (e.g. empty peer_ids)", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress", body = ErrorResponse),
        (status = 413, description = "The DAR set exceeds the size one run can carry", body = ErrorResponse)
    )
)]
#[post("/dars/distribute")]
pub async fn start_dars(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<DarsRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    if body.peer_ids.is_empty() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "peer_ids must contain at least one peer".to_string(),
        });
    }
    let body = body.into_inner();
    launch_dars(&data, body.dar_files, body.peer_ids).await
}

/// Distribute the DAR on this node that holds a package
///
/// The DAR is read from this participant, so the operator does not upload it
/// again. It then runs the same workflow as `/dars/distribute`: each peer
/// accepts or rejects the invitation as before.
#[utoipa::path(
    tag = "Workflows",
    request_body = DistributePackageRequest,
    responses(
        (status = 202, description = "DARs distribution workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request (e.g. empty peer_ids)", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 404, description = "No DAR on this node holds the package", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress", body = ErrorResponse),
        (status = 413, description = "The DAR exceeds the size one run can carry", body = ErrorResponse),
        (status = 500, description = "The DAR could not be read", body = ErrorResponse)
    )
)]
#[post("/dars/distribute-package")]
pub async fn start_dars_for_package(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<DistributePackageRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    if body.peer_ids.is_empty() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "peer_ids must contain at least one peer".to_string(),
        });
    }
    let dar = match fetch_dar_for_package(&data.config, &body.package_id).await {
        Ok(Some(dar)) => dar,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: format!("No DAR on this node holds package {}", body.package_id),
            });
        }
        Err(e) => {
            tracing::error!(
                "Failed to read the DAR for package {}: {e:#}",
                body.package_id
            );
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to read the DAR: {e}"),
            });
        }
    };
    let body = body.into_inner();
    launch_dars(&data, vec![dar], body.peer_ids).await
}

/// Refuse a DAR set whose encoded `UploadDars` bundle exceeds `limit` bytes.
///
/// A peer checks the size only after it accepts the invitation, so the
/// coordinator checks it first. The bundle frames each file with its name and
/// two lengths, and those bytes count toward the limit.
fn dars_bundle_refusal(
    files: &[DarFile],
    limit: usize,
) -> std::result::Result<(), (StatusCode, String)> {
    let mut sizes = Vec::with_capacity(files.len());
    for file in files {
        let data = STANDARD.decode(&file.data).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!(
                    "DAR file {name} is not valid base64: {e}",
                    name = file.filename
                ),
            )
        })?;
        sizes.push((file.filename.as_str(), data.len()));
    }
    let Some(error) = bundle_size_refusal(&sizes, limit) else {
        return Ok(());
    };
    tracing::warn!(
        "Refused to distribute {count} DAR file(s): {error}",
        count = files.len()
    );
    Err((StatusCode::PAYLOAD_TOO_LARGE, error))
}

/// Why the bundle of `files`, each a name and a DAR length, does not fit in
/// `limit` bytes, or `None` when it fits.
///
/// A DAR that is over the limit alone can never be distributed, so the
/// refusal names it and does not suggest several runs.
fn bundle_size_refusal(files: &[(&str, usize)], limit: usize) -> Option<String> {
    // A size too large to count is over any limit.
    let size_of = |files: &[(&str, usize)]| {
        utils::encoded_files_len(files.iter().map(|(name, len)| (name.len(), *len)))
            .unwrap_or(usize::MAX)
    };
    let size = size_of(files);
    if size <= limit {
        return None;
    }
    let mib = |bytes: usize| bytes as f64 / (1024.0 * 1024.0);
    let limit_text = format!("{limit} bytes ({limit_mib:.1} MiB)", limit_mib = mib(limit));
    let over = |size: usize| {
        format!(
            "That is {over} bytes over the limit of {limit_text}.",
            over = size - limit
        )
    };
    let alone: Vec<(&str, usize)> = files
        .iter()
        .map(|file| (file.0, size_of(std::slice::from_ref(file))))
        .filter(|(_, size)| *size > limit)
        .collect();
    Some(match alone.as_slice() {
        [] => format!(
            "The DAR set is too large for one run. The files and their names take {size} bytes \
             ({size_mib:.1} MiB). {over} Send the files in several runs, each within the limit.",
            size_mib = mib(size),
            over = over(size),
        ),
        [(name, alone_size)] => format!(
            "{name} is too large to distribute, even on its own. The file and its name take \
             {alone_size} bytes ({size_mib:.1} MiB). {over} Upload it to each node separately \
             instead.",
            size_mib = mib(*alone_size),
            over = over(*alone_size),
        ),
        several => format!(
            "These DARs are each too large to distribute, even on their own: {names}. The limit \
             is {limit_text}. Upload them to each node separately instead.",
            names = several
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
                .join(", "),
        ),
    })
}

/// Start a DARs distribution run: register it, invite the peers and run the
/// coordinator. The caller has checked the admin role and that `peer_ids` is
/// not empty.
async fn launch_dars(
    data: &web::Data<AppState>,
    dar_files: Vec<DarFile>,
    peer_ids: Vec<CantonId>,
) -> HttpResponse {
    if let Err((status, error)) = dars_bundle_refusal(&dar_files, MAX_CHUNKED_TOTAL_SIZE) {
        return HttpResponse::build(status).json(ErrorResponse { error });
    }

    // Create DARs config from request
    let timestamp = now_secs();
    let instance_name = format!("dars-distribute-{timestamp}");
    let instance = WorkflowInstance::new(
        instance_name.clone(),
        WorkflowKind::Dars,
        WorkflowRole::Coordinator,
    );
    let dars_state = &instance.http;
    let dars_config = workflow::DarsConfig {
        dar_files,
        instance_name: instance_name.clone(),
        peer_ids: peer_ids.clone(),
    };

    // Refuse to invite any peer that can't speak the concurrent-workflows wire
    // format — NO invites are sent if even one invitee fails the version gate.
    let incompatible = preflight_incompatible_peers(&data.config, &data.db, &peer_ids).await;
    if !incompatible.is_empty() {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format_incompatible_peers(&incompatible),
        });
    }

    // Register + persist atomically w.r.t. duplicates (registry insert dedups
    // before the upsert; a persist failure unregisters so nothing leaks).
    if let Err(resp) = register_and_persist(
        data,
        &instance,
        DarsStep::WaitingForPeers,
        &dars_config,
        &peer_ids,
        None,
    )
    .await
    {
        return resp;
    }

    let config = data.config.clone();
    let db = data.db.clone();
    let dars_state_clone = instance.http.clone();
    let instance_for_coord = instance.clone();
    let kind = instance.kind;
    let workflows = data.workflows.clone();
    let last_seen = data.last_seen.clone();
    *dars_state.invited_peers.write().await = peer_ids.clone();
    let instance_for_task = instance_name.clone();

    // Acquire abort_handle, status, and error locks BEFORE spawning, then
    // flip all three together. Held simultaneously around the spawn so a
    // concurrent /dars/cancel either runs entirely before us (status !=
    // InProgress → 409 "no workflow in progress") or entirely after us
    // (status InProgress AND abort_handle Some → cancel proceeds). Without
    // this, a cancel arriving between the status flip and the abort_handle
    // assignment would observe "InProgress + None" and bail with the
    // spurious "still initializing" 409 — which leaves dars_state pinned
    // to InProgress and breaks the next /dars/distribute with a stale
    // 409. Holding `status.write()` across the spawn also blocks the
    // spawned task's terminal status write until our InProgress flip
    // lands, so a fast-failing task can't publish its terminal status
    // before we publish InProgress.
    let mut abort_guard = dars_state.abort_handle.lock().await;
    let mut status_guard = dars_state.status.write().await;
    let mut error_guard = dars_state.error.write().await;

    let join_handle = tokio::spawn(async move {
        let _workflow_guard = WorkflowGuard::new(workflows, instance_for_task.clone());

        // Send invites to selected peers before starting coordinator workflow
        let dar_filenames: Vec<String> = dars_config
            .dar_files
            .iter()
            .map(|f| f.filename.clone())
            .collect();
        // Pin the content, not just the name: the invitation the operator sees
        // carries the hash of the exact bytes the peers will be asked to upload
        // and vet. An empty hash list is the wire signal for "coordinator
        // predates the field", which makes peers fall back to checking
        // filenames only — so a DAR we cannot decode has to fail the run rather
        // than quietly downgrade every peer's check.
        let dar_hashes: std::result::Result<Vec<String>, _> = dars_config
            .dar_files
            .iter()
            .map(|f| {
                STANDARD
                    .decode(&f.data)
                    .map(|bytes| workflow::validation::hash_dar(&bytes))
            })
            .collect();
        let invite_result = match dar_hashes {
            Ok(dar_hashes) => {
                send_dars_invites(
                    &config,
                    &db,
                    &peer_ids,
                    &dar_filenames,
                    &dar_hashes,
                    &dars_config.instance_name,
                )
                .await
            }
            Err(e) => Err(anyhow::anyhow!(
                "Could not hash the DARs for the invitation: {e}"
            )),
        };
        if let Err(e) = invite_result {
            tracing::error!("Failed to send DARs invites: {e}");
            let mut status = dars_state_clone.status.write().await;
            let mut error = dars_state_clone.error.write().await;
            *status = WorkflowProgress::Failed;
            *error = Some(format!("Failed to send invites: {e}"));
            mark_run_failed(
                &db,
                kind,
                &instance_for_task,
                &format!("Failed to send invites: {e}"),
            )
            .await;
            return;
        }

        // Give peers time to start their peer workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let result = workflow::start_coordinator(
            config,
            db.clone(),
            workflow::WorkflowType::Dars,
            None, // No onboarding config
            None, // No kick config
            None, // No contracts config
            Some(dars_config),
            None, // No add-party config
            None, // No change-threshold config
            None, // No rotate-key config
            None, // No auth
            last_seen,
            instance_for_coord,
        )
        .await;

        // Update in-memory state in tight scopes — never hold the RwLock
        // across a DB await (see kick/onboarding/contracts handlers above).
        match result {
            Ok(_) => {
                // The run row is marked terminal BEFORE the in-memory status
                // flips. The two are read by different callers — `/…/status`
                // reads this lock, while the "already has a workflow in flight"
                // guard scans the run rows — so flipping memory first opens a
                // window where the API reports a run finished and refuses the
                // next workflow for that party in the same breath. Doing the
                // durable write first can only report a finished run as still
                // running for an instant, which costs a poll rather than a
                // rejected request.
                mark_run_completed(&db, kind, &instance_for_task).await;
                {
                    let mut status = dars_state_clone.status.write().await;
                    *status = WorkflowProgress::Completed;
                }
                tracing::info!("DARs distribution workflow completed successfully");
            }
            Err(e) => {
                let msg = format!("{e}");
                // Durable row first, in-memory status second — same ordering as the
                // success path above, and for the same reason: a caller that reads
                // Failed from `/…/status` and retries must not be refused by the
                // in-flight guard, which scans the run rows.
                mark_run_failed(&db, kind, &instance_for_task, &msg).await;
                {
                    let mut status = dars_state_clone.status.write().await;
                    let mut error = dars_state_clone.error.write().await;
                    *status = WorkflowProgress::Failed;
                    *error = Some(msg.clone());
                }
                tracing::error!("DARs distribution workflow failed: {e}");
            }
        }
    });
    *abort_guard = Some(join_handle.abort_handle());
    *status_guard = WorkflowProgress::InProgress;
    *error_guard = None;
    drop(error_guard);
    drop(status_guard);
    drop(abort_guard);

    HttpResponse::Accepted().json(WorkflowResponse {
        status: WorkflowProgress::InProgress,
        message: "DARs distribution workflow started".to_string(),
        instance_name,
    })
}

/// Get the current status of the DARs distribution workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "DARs distribution workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/dars/distribute/status")]
pub async fn get_dars_status(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(kind_status(&data, WorkflowKind::Dars).await)
}

/// Send DARs invites to selected peers using Noise protocol
async fn send_dars_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
    dar_filenames: &[String],
    dar_hashes: &[String],
    instance_name: &str,
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let payload = DarsInvitePayload {
        dar_filenames: dar_filenames.to_vec(),
        dar_hashes: dar_hashes.to_vec(),
        // Carry the member set so the peer card shows the same participant
        // list the coordinator shows.
        participants: peer_ids.to_vec(),
        workflow_instance: Some(instance_name.to_string()),
    };
    let payload_bytes = serde_json::to_vec(&payload)?;
    let invite_message = Message::new(MessageType::InviteDars, payload_bytes);

    for peer_id in peer_ids {
        let peer = match network_config
            .peers
            .iter()
            .find(|p| &p.participant_id == peer_id)
        {
            Some(p) => p,
            None => {
                tracing::warn!("Skipping invite to {peer_id} - peer not found in network config");
                continue;
            }
        };

        if peer.public_key.is_empty() {
            tracing::warn!("Skipping invite to {peer_id} - no public key configured");
            continue;
        }

        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!("Skipping invite to {peer_id} - invalid public key: {e}");
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);
        let identity = config.participant_id().to_string();

        tracing::info!(
            "Sending DARs invite to {peer_id} at {addr}:{port}",
            addr = peer.address,
            port = peer.port
        );

        match send_noise_message(
            &peer.address,
            peer.port,
            &psk,
            identity.as_bytes(),
            &invite_message,
        )
        .await
        {
            Ok(response) => {
                interpret_invite_reply(peer_id, "DARs", &response)?;
            }
            Err(e) => {
                tracing::error!("Failed to send DARs invite to {peer_id}: {e}");
            }
        }
    }

    Ok(())
}

/// Shared cancel logic for the legacy per-kind endpoints. Picks the coordinator
/// run of `kind` via [`pick_coordinator_run`] (deterministic lowest
/// instance_name), so `/{kind}/status` and `/{kind}/cancel` always act on the
/// same instance. Callers that know which run they mean should use
/// `POST /workflows/{instance}/cancel`. With no in-memory run, falls back to the
/// persisted row (see [`cancel_persisted_run`]) so a stuck run stays cancellable.
async fn cancel_workflow_state(
    data: &web::Data<AppState>,
    label: &str,
    kind: WorkflowKind,
) -> HttpResponse {
    if let Some(instance) = pick_coordinator_run(data, kind) {
        return cancel_instance(data, &instance, label).await;
    }
    cancel_persisted_run(data, label, kind).await
}

/// Cancel a run with a persisted `inprogress` row but no in-memory instance
/// (nothing to abort): notify the row's peers, flip it to Cancelled, 409 if none.
async fn cancel_persisted_run(
    data: &web::Data<AppState>,
    label: &str,
    kind: WorkflowKind,
) -> HttpResponse {
    let run = match data
        .db
        .get_active_workflow_run(kind, WorkflowRole::Coordinator)
        .await
    {
        Ok(Some(run)) => run,
        Ok(None) => {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: format!("No {label} workflow in progress"),
            });
        }
        Err(e) => {
            tracing::error!("Failed to load persisted {label} run for cancel: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to cancel {label} workflow (database error) — try again"),
            });
        }
    };

    if !run.expected_peers.is_empty()
        && let Err(e) = send_cancel_invites(
            &data.config,
            &data.db,
            &run.expected_peers,
            &run.instance_name,
        )
        .await
    {
        tracing::warn!("send_cancel_invites failed during {label} cancel: {e}");
    }

    // On write failure the row stays InProgress (recovery may resume it) — 500.
    match mark_run_cancelled(&data.db, kind, &run.instance_name).await {
        Ok(true) => {}
        // Another write ended the run after it was loaded, and no task is left
        // to stop, so this cancel did nothing.
        Ok(false) => {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: format!("No {label} workflow in progress"),
            });
        }
        Err(e) => {
            tracing::error!(
                "Failed to persist cancel for stuck {label} run {}: {e:#}",
                run.instance_name
            );
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!(
                    "Failed to cancel {label} workflow (database error); the run is still \
                     active — try again"
                ),
            });
        }
    }

    tracing::info!(
        "{label} workflow {} cancelled off the persisted row",
        run.instance_name
    );
    HttpResponse::Ok().json(MessageResponse {
        message: format!("{label} workflow cancelled"),
    })
}

/// Cancel exactly one registered coordinator run: abort its task, notify its
/// invitees with an instance-stamped CancelInvite (sibling runs on the peers
/// survive), and mirror Cancelled onto the persisted row. A row that already
/// records an outcome keeps it, and the task is still stopped.
async fn cancel_instance(
    data: &web::Data<AppState>,
    instance: &Arc<WorkflowInstance>,
    label: &str,
) -> HttpResponse {
    let state = &instance.http;
    {
        let status = state.status.read().await;
        if *status != WorkflowProgress::InProgress {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: format!("No {label} workflow in progress"),
            });
        }
    }

    // Take the abort handle FIRST. If it's None we're racing with a start path that
    // hasn't finished setting itself up yet; refuse the cancel rather than mark the
    // workflow Cancelled while the spawned task is still alive. Start paths always
    // populate abort_handle before they let any await reach this point.
    let Some(handle) = state.abort_handle.lock().await.take() else {
        tracing::warn!(
            "{label} cancel arrived before the workflow finished initializing — refusing"
        );
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("{label} workflow is still initializing — try again in a moment"),
        });
    };

    // Persist Cancelled BEFORE aborting. Recovery resumes every InProgress
    // coordinator row on restart, so if we aborted first and this status write
    // then failed transiently, the row would stay InProgress and
    // respawn_coordinator would re-launch — re-sending invites, re-submitting
    // topology — for a run the operator explicitly cancelled. So on a write
    // failure we DON'T abort: restore the handle so the run stays cancellable and
    // live, and surface 500 so the operator retries against consistent state.
    // Once the row is durably Cancelled, a crash before the abort is safe —
    // recovery only resumes InProgress rows.
    match mark_run_cancelled(&data.db, instance.kind, &instance.instance_name).await {
        Ok(true) => {}
        // The row already records an outcome, which it keeps, but the task can
        // still be alive: a declined run idles in WaitingForPeers. Operators reap
        // that task with this cancel, so it stops the task and answers like a
        // normal cancel. Its peers either got the decline's CancelInvite or
        // finished, so none is sent.
        Ok(false) => {
            handle.abort();
            tracing::info!(
                "{label} workflow {} had already ended; stopped its task",
                instance.instance_name
            );
            return HttpResponse::Ok().json(MessageResponse {
                message: format!("{label} workflow had already ended; its task is stopped"),
            });
        }
        Err(e) => {
            tracing::error!(
                "Failed to persist cancel for {}: {e:#}; leaving the run InProgress and \
                 active rather than aborting a run we can't durably mark cancelled",
                instance.instance_name
            );
            *state.abort_handle.lock().await = Some(handle);
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!(
                    "Failed to cancel {label} workflow (database error); the run is still \
                     active — try again"
                ),
            });
        }
    }

    // Durably Cancelled — now stop the task and notify invitees.
    // NOTE: an abort landing in the narrow window between the workflow task's
    // Ok(result) and its mark_completed write can leave the row Cancelled for a
    // run that de-facto finished — rare and benign (the work itself is already
    // committed on-ledger; only the feed status differs).
    handle.abort();

    let invitees = state.invited_peers.read().await.clone();
    if !invitees.is_empty()
        && let Err(e) =
            send_cancel_invites(&data.config, &data.db, &invitees, &instance.instance_name).await
    {
        tracing::warn!("send_cancel_invites failed during {label} cancel: {e}");
    }

    {
        let mut status = state.status.write().await;
        *status = WorkflowProgress::Cancelled;
    }
    {
        let mut error = state.error.write().await;
        *error = None;
    }

    tracing::info!("{label} workflow {} cancelled", instance.instance_name);
    HttpResponse::Ok().json(MessageResponse {
        message: format!("{label} workflow cancelled"),
    })
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/onboarding/cancel")]
pub async fn cancel_onboarding(http_req: HttpRequest, data: web::Data<AppState>) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(&data, "Onboarding", WorkflowKind::Onboarding).await
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/kick/cancel")]
pub async fn cancel_kick(http_req: HttpRequest, data: web::Data<AppState>) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(&data, "Kick", WorkflowKind::Kick).await
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/contracts/cancel")]
pub async fn cancel_contracts(http_req: HttpRequest, data: web::Data<AppState>) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(&data, "Contracts", WorkflowKind::Contracts).await
}

#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No workflow in progress", body = ErrorResponse)
    )
)]
#[post("/dars/cancel")]
pub async fn cancel_dars(http_req: HttpRequest, data: web::Data<AppState>) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    cancel_workflow_state(&data, "DARs", WorkflowKind::Dars).await
}

/// Cancel one specific in-flight coordinator run by its `instance_name`. With
/// concurrent runs of the same kind, this is the only unambiguous cancel — the
/// legacy per-kind `/{kind}/cancel` endpoints pick a run deterministically but
/// cannot target a chosen card.
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Workflow cancelled", body = MessageResponse),
        (status = 409, description = "No in-flight run with this instance_name", body = ErrorResponse)
    )
)]
#[post("/workflows/{instance_name}/cancel")]
pub async fn cancel_workflow_instance(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let instance_name = path.into_inner();
    let Some(instance) = data.workflows.get(&instance_name) else {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("No in-flight workflow run named {instance_name}"),
        });
    };
    if instance.role != WorkflowRole::Coordinator {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("Run {instance_name} is not coordinated by this node"),
        });
    }
    let label = instance.kind.as_str();
    cancel_instance(&data, &instance, label).await
}

// ============================================================================
// Generic workflow_runs endpoints (used by the unified notifications feed)
// ============================================================================

/// List every workflow run that should appear in the notifications feed:
/// every InProgress run on this node + any terminal run the operator hasn't
/// dismissed yet. `coordinator_name` is joined from the peers table.
#[utoipa::path(
    tag = "Workflows",
    responses((status = 200, description = "Visible workflow runs", body = WorkflowRunsResponse))
)]
#[get("/workflows")]
pub async fn list_workflows(data: web::Data<AppState>) -> impl Responder {
    let runs = match data.db.get_visible_workflow_runs().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to list workflow runs: {e:#}");
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to list workflow runs: {e}"),
            });
        }
    };

    // Resolve coordinator names from the peers table — same pattern get_invitations uses.
    let pubkey_to_name: HashMap<String, String> = data
        .db
        .get_all_peers()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.public_key, p.name))
        .collect();

    let mut resolved: Vec<WorkflowRun> = Vec::with_capacity(runs.len());
    for mut r in runs {
        if let Some(pk) = r.coordinator_pubkey.as_deref() {
            r.coordinator_name = pubkey_to_name.get(pk).cloned();
        }
        enrich_from_config_json(&mut r);
        // Who has joined lives only in the live coordinator's `WorkflowState`;
        // the row itself carries no such column. Merge it in so a run parked on
        // WaitingForPeers can show real progress instead of a permanent zero.
        if let Some(active) = data.workflows.route(&r.instance_name) {
            let mut joined: Vec<CantonId> = active.connected_peers().await.into_iter().collect();
            joined.sort();
            r.connected_peers = joined;
            // The source's progress is live in the open export session, so read
            // it straight from memory rather than from a sampled artefact.
            r.acs_progress = active.acs_export_progress().await;
        }
        if r.acs_progress.is_none() {
            // The target has no registry entry — its side of the transfer runs
            // in a peer job — so it records samples as an artefact instead.
            r.acs_progress = read_acs_progress(&data.db, &r.instance_name).await;
        }
        resolved.push(r);
    }

    HttpResponse::Ok().json(WorkflowRunsResponse { runs: resolved })
}

/// Read the target's last recorded ACS transfer sample for a run.
///
/// Display only: a missing or unparseable sample means the card shows no
/// transfer line, never an error.
async fn read_acs_progress(db: &SqlitePool, instance_name: &str) -> Option<AcsTransferProgress> {
    let raw = db
        .read_artifact(
            instance_name,
            workflow::storage::artifact_kinds::ADD_PARTY_ACS_PROGRESS,
            None,
        )
        .await
        .ok()
        .flatten()?;
    serde_json::from_slice(&raw).ok()
}

/// List the external parties this participant currently hosts, cached from its own
/// Canton topology (an authorized `PartyToParticipant` naming this participant
/// with Confirmation and a self-owned single-key namespace). Wallet-driven
/// onboarding keeps no local run row, so there is nothing DB-side to read.
///
/// The answer comes from a snapshot, and a stale one starts a background scan.
/// Until the first scan finishes, the response carries no `fetched_at`.
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "External parties", body = ExternalPartiesResponse),
        (status = 503, description = "Discovery failed and no earlier result exists", body = ErrorResponse)
    )
)]
#[get("/external-parties")]
pub async fn list_external_parties(data: web::Data<AppState>) -> impl Responder {
    let (snapshot, refreshing, refresh_error) = match external_parties::read(&data) {
        external_parties::View::Pending => {
            return HttpResponse::Ok().json(ExternalPartiesResponse {
                parties: Vec::new(),
                fetched_at: None,
                refreshing: true,
                refresh_error: None,
            });
        }
        external_parties::View::Failed(error) => {
            return HttpResponse::ServiceUnavailable().json(ErrorResponse { error });
        }
        external_parties::View::Ready {
            snapshot,
            refreshing,
            refresh_error,
        } => (snapshot, refreshing, refresh_error),
    };
    let parties = snapshot
        .parties
        .into_iter()
        .map(|p| ExternalPartyInfo {
            party_id: p.party_id,
            fingerprint: p.fingerprint,
            threshold: p.threshold,
            host_count: p.host_count,
            created_at: p.created_at,
            onboarding: p.onboarding,
            hosts: p
                .hosts
                .into_iter()
                .map(|h| ExternalPartyHost {
                    participant_uid: h.participant_uid,
                    permission: permission_from_proto(h.permission),
                })
                .collect(),
        })
        .collect();
    HttpResponse::Ok().json(ExternalPartiesResponse {
        parties,
        fetched_at: Some(snapshot.fetched_at.to_rfc3339()),
        refreshing,
        refresh_error,
    })
}

/// Pull `prefix` + `participants` out of the run's `config_json` and lift
/// them onto the response struct so the frontend can show them without
/// parsing JSON blobs. Coordinator configs spell the prefix field
/// `party_id_prefix` (e.g. `OnboardingConfig`) while the peer-side payload
/// uses `prefix`; we accept either. For participants we fall back to
/// `expected_peers` when the config doesn't carry a list of its own — that
/// way Kick / Contracts / Dars runs (whose configs don't include a peer
/// list) still surface their participants.
fn enrich_from_config_json(run: &mut WorkflowRun) {
    // A contract entry in a coordinator-side `ContractsConfig`. We only need
    // its human-readable `name` for the card's "Packages" row.
    #[derive(serde::Deserialize)]
    struct ContractNameShape {
        #[serde(default)]
        name: String,
    }
    // A DAR entry in a coordinator-side `DarsConfig`.
    #[derive(serde::Deserialize)]
    struct DarFileShape {
        #[serde(default)]
        filename: String,
    }
    #[derive(serde::Deserialize)]
    struct ConfigShape {
        #[serde(default)]
        prefix: Option<String>,
        #[serde(default)]
        party_id_prefix: Option<String>,
        #[serde(default)]
        participants: Vec<CantonId>,
        // Kick + AddParty configs only.
        #[serde(default)]
        new_threshold: Option<i32>,
        #[serde(default)]
        previous_threshold: Option<i32>,
        // Onboarding config only: the initial threshold (no previous).
        #[serde(default)]
        threshold: Option<i32>,
        // Kick configs only.
        #[serde(default)]
        participant_id: Option<CantonId>,
        #[serde(default)]
        namespace_fingerprint: Option<String>,
        // AddParty configs only.
        #[serde(default)]
        new_participant_id: Option<CantonId>,
        // AddParty + Kick configs: derive a display prefix from the party id.
        #[serde(default)]
        decentralized_party_id: Option<CantonId>,
        // Contracts: `package_names` is the peer's flat list (from the invite);
        // `contracts[].name` is the coordinator's `ContractsConfig`.
        #[serde(default)]
        package_names: Vec<String>,
        #[serde(default)]
        contracts: Vec<ContractNameShape>,
        // Dars: `dar_filenames` is the peer's flat list (from the invite);
        // `dar_files[].filename` is the coordinator's `DarsConfig`.
        #[serde(default)]
        dar_filenames: Vec<String>,
        #[serde(default)]
        dar_files: Vec<DarFileShape>,
    }
    if let Ok(shape) = serde_json::from_str::<ConfigShape>(&run.config_json) {
        let prefix = shape
            .prefix
            .or(shape.party_id_prefix)
            .or_else(|| shape.decentralized_party_id.map(|p| p.prefix));
        if let Some(p) = prefix
            && !p.is_empty()
        {
            run.prefix = Some(p);
        }
        if !shape.participants.is_empty() {
            run.participants = shape.participants;
        }
        // Onboarding stores the initial threshold under `threshold`; kick /
        // add-party use `new_threshold`. Either populates the card's value.
        run.new_threshold = shape.new_threshold.or(shape.threshold);
        // Only surface a previous threshold when an older client actually
        // sent one (it defaults to 0); 0 means "unknown", render as new-only.
        run.previous_threshold = shape.previous_threshold.filter(|t| *t > 0);
        run.kicked_participant = shape.participant_id;
        if run.kind == WorkflowKind::Kick {
            run.kicked_owner_key = shape.namespace_fingerprint;
        }
        run.added_participant = shape.new_participant_id;
        // Package names: peer's flat list wins; otherwise derive from the
        // coordinator's contract definitions. Both converge to the same set.
        if !shape.package_names.is_empty() {
            run.package_names = shape.package_names;
        } else if !shape.contracts.is_empty() {
            run.package_names = shape
                .contracts
                .into_iter()
                .map(|c| c.name)
                .filter(|n| !n.is_empty())
                .collect();
        }
        // DAR filenames: same peer-flat-list-vs-coordinator-config convergence.
        if !shape.dar_filenames.is_empty() {
            run.dar_filenames = shape.dar_filenames;
        } else if !shape.dar_files.is_empty() {
            run.dar_filenames = shape
                .dar_files
                .into_iter()
                .map(|d| d.filename)
                .filter(|n| !n.is_empty())
                .collect();
        }
    }
    // Fallback: if config_json didn't expose a participants list (e.g. Kick /
    // Contracts / Dars), surface the run's `expected_peers` instead so the
    // card still shows who was involved.
    if run.participants.is_empty() && !run.expected_peers.is_empty() {
        run.participants = run.expected_peers.clone();
    }
}

/// Mark a terminal-state workflow run as dismissed so it disappears from the
/// notifications feed. Returns 409 if the run is still InProgress.
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Run dismissed", body = MessageResponse),
        (status = 404, description = "Run not found", body = ErrorResponse),
        (status = 409, description = "Run is still in progress", body = ErrorResponse)
    )
)]
#[post("/workflows/{instance_name}/dismiss")]
pub async fn dismiss_workflow(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let instance_name = path.into_inner();

    let run = match data.db.get_workflow_run(&instance_name).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: format!("workflow run {instance_name} not found"),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to load workflow run: {e}"),
            });
        }
    };

    if run.status == WorkflowProgress::InProgress {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "{} workflow {instance_name} is still in progress — cancel it first",
                run.kind
            ),
        });
    }

    let mut tx = match data.db.begin_transaction().await {
        Ok(t) => t,
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to begin tx: {e}"),
            });
        }
    };
    if let Err(e) = tx.dismiss_workflow_run(&instance_name).await {
        return HttpResponse::InternalServerError().json(ErrorResponse {
            error: format!("Failed to dismiss workflow run: {e}"),
        });
    }
    if let Err(e) = Commitable::commit(tx).await {
        return HttpResponse::InternalServerError().json(ErrorResponse {
            error: format!("Failed to commit dismiss: {e}"),
        });
    }

    HttpResponse::Ok().json(MessageResponse {
        message: format!("workflow {instance_name} dismissed"),
    })
}

/// Retry a Failed coordinator-side workflow run from where it left off.
///
/// Flips `status` back to `inprogress`, clears `error`, and re-spawns the
/// coordinator task. `NoiseServer::new` re-hydrates `WorkflowState` from the
/// persisted `current_step`, so the run picks up at the same step that
/// failed. Only valid on Failed coordinator-side rows; peer retry is not
/// supported (the coordinator may already be past the config-bearing
/// command — operator should dismiss the peer row and re-accept the
/// invite instead).
#[utoipa::path(
    tag = "Workflows",
    params(
        ("instance_name" = String, Path, description = "Workflow run identifier")
    ),
    responses(
        (status = 200, description = "Retry started", body = MessageResponse),
        (status = 404, description = "Run not found", body = ErrorResponse),
        (status = 409, description = "Run is not in a retryable state", body = ErrorResponse)
    )
)]
#[post("/workflows/{instance_name}/retry")]
pub async fn retry_workflow(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    let instance_name = path.into_inner();

    let run = match data.db.get_workflow_run(&instance_name).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return HttpResponse::NotFound().json(ErrorResponse {
                error: format!("workflow run {instance_name} not found"),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to load workflow run: {e}"),
            });
        }
    };

    if run.role != WorkflowRole::Coordinator {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: "Retry must be initiated from the coordinator side. Peer rows flip back \
                    to InProgress automatically when the coordinator retries — wait for that or \
                    dismiss the row."
                .to_string(),
        });
    }
    if run.status != WorkflowProgress::Failed {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!(
                "Cannot retry a workflow in status {:?}; only Failed runs can be retried",
                run.status
            ),
        });
    }

    // Flip the row back to InProgress before re-spawning the task. Concurrent
    // same-kind runs are allowed now (no unique index); the registry insert in
    // respawn_coordinator still rejects a same-instance duplicate.
    let mut tx = match data.db.begin_transaction().await {
        Ok(t) => t,
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to begin tx: {e}"),
            });
        }
    };
    if let Err(e) = tx
        .set_workflow_run_status(
            &instance_name,
            WorkflowProgress::InProgress,
            None,
            now_secs(),
        )
        .await
    {
        return HttpResponse::Conflict().json(ErrorResponse {
            error: format!("Failed to flip status to InProgress: {e}"),
        });
    }
    if let Err(e) = Commitable::commit(tx).await {
        return HttpResponse::InternalServerError().json(ErrorResponse {
            error: format!("Failed to commit retry status: {e}"),
        });
    }
    count_run_started(run.kind);

    // Re-load the row (now InProgress), broadcast RetryWorkflow to the
    // peer cohort, and re-spawn the coordinator task. respawn_coordinator
    // updates the matching HttpWorkflowState, stashes a new abort handle,
    // and finalizes the row to Completed/Failed when the task ends.
    let run = match data.db.get_workflow_run(&instance_name).await {
        Ok(Some(r)) => r,
        _ => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: "Workflow run vanished mid-retry".to_string(),
            });
        }
    };
    // Tell every previously-invited peer to flip their Failed row back to
    // InProgress and re-spin start_peer. Best-effort — peers that are
    // unreachable now will stay Failed; operator can dismiss + re-accept.
    if let Err(e) = send_retry_workflow(
        &data.config,
        &data.db,
        &run.expected_peers,
        &run.instance_name,
    )
    .await
    {
        tracing::warn!("Failed to broadcast RetryWorkflow: {e:#}");
    }
    respawn_coordinator(
        data.db.clone(),
        data.config.clone(),
        &run,
        data.workflows.clone(),
        data.auth.clone(),
        data.last_seen.clone(),
    )
    .await;

    HttpResponse::Ok().json(MessageResponse {
        message: format!(
            "Retrying workflow {instance_name} from step {}",
            run.current_step
        ),
    })
}

/// Look for an existing decentralized party whose human-readable prefix
/// equals `prefix`. Returns the matching `party_id` if found — used by the
/// onboarding pre-flight to refuse duplicate-prefix runs.
/// Probe every invitee's `Health` and return those that cannot POSITIVELY be
/// verified to run dec-party-manager >= [`MIN_PEER_VERSION`].
async fn preflight_incompatible_peers(
    config: &NodeConfig,
    db: &SqlitePool,
    invitees: &[CantonId],
) -> Vec<(String, String)> {
    incompatible_versions(&probe_invitee_health(config, db, invitees).await)
}

/// One invitee's reply to a `Health` probe, or why no reply came.
type InviteeHealth = (String, std::result::Result<HealthReply, String>);

/// Probe every invitee's `Health` over Noise. An invitee missing from the
/// peer table, or with a public key that does not parse, is not probed.
async fn probe_invitee_health(
    config: &NodeConfig,
    db: &SqlitePool,
    invitees: &[CantonId],
) -> Vec<InviteeHealth> {
    let Ok(peers) = db.get_all_peers().await else {
        return Vec::new();
    };
    let Ok(keypair) = NoiseKeypair::from_file(&config.key_file_path()).await else {
        return Vec::new();
    };
    let identity = config.participant_id().to_string();
    let invitee_set: HashSet<String> = invitees.iter().map(CantonId::to_string).collect();

    let mut probes = Vec::new();
    for peer in peers
        .into_iter()
        .filter(|p| invitee_set.contains(&p.participant_id.to_string()))
    {
        let Ok(pubkey) = parse_public_key(&peer.public_key) else {
            continue;
        };
        let psk = keypair.derive_psk(&pubkey);
        let id = peer.participant_id.to_string();
        let address = peer.address.clone();
        let port = peer.port;
        let identity = identity.clone();
        let retry = config.noise_retry.clone();
        probes.push(tokio::spawn(async move {
            let reply = send_noise_message_with_retry(
                &address,
                port,
                &psk,
                identity.as_bytes(),
                &Message::new_empty(MessageType::Health),
                &retry,
            )
            .await
            .map(|response| classify_health_reply(&response))
            .map_err(|e| e.to_string());
            (id, reply)
        }));
    }

    let mut replies = Vec::new();
    for probe in probes {
        if let Ok(reply) = probe.await {
            replies.push(reply);
        }
    }
    replies
}

/// The invitees that cannot POSITIVELY be verified to run at least
/// dec-party-manager [`MIN_PEER_VERSION`]. 0.1.9's Noise wire format is
/// breaking — an older peer cannot parse our frames at all, so inviting it
/// would strand the run. An old build answers the probe with a 503 (it can't
/// decode the request), so any failure to obtain a parseable version report
/// blocks the start: for this gate, unverifiable means incompatible, never
/// "assume OK".
fn incompatible_versions(replies: &[InviteeHealth]) -> Vec<(String, String)> {
    replies
        .iter()
        .filter_map(|(id, reply)| match reply {
            Ok(reply) => match &reply.version {
                Some(v) if utils::version_at_least(v, MIN_PEER_VERSION) => None,
                Some(v) => Some((id.clone(), format!("running version {v}"))),
                None => Some((
                    id.clone(),
                    "reachable but reported no version (pre-0.1.9 build)".to_string(),
                )),
            },
            Err(e) => Some((id.clone(), format!("could not verify version: {e}"))),
        })
        .collect()
}

/// The invitees whose `Health` reply does not list `feature`, each with the
/// version it reported. Unverifiable means lacking, as for the version gate.
fn lacking_feature(replies: &[InviteeHealth], feature: &str) -> Vec<(String, String)> {
    replies
        .iter()
        .filter_map(|(id, reply)| match reply {
            Ok(reply) if reply.features.iter().any(|f| f == feature) => None,
            Ok(reply) => Some((
                id.clone(),
                format!(
                    "running version {}",
                    reply.version.as_deref().unwrap_or("unknown")
                ),
            )),
            Err(e) => Some((id.clone(), format!("could not verify: {e}"))),
        })
        .collect()
}

/// Operator-facing 409 message naming each peer that cannot sign an
/// owner-only kick.
fn format_peers_lacking_owner_only_kick(lacking: &[(String, String)]) -> String {
    let list = lacking
        .iter()
        .map(|(id, reason)| format!("{id} ({reason})"))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "Cannot start the kick: removing an owner that hosts no participant needs every \
         invited peer to support it, and older builds refuse to sign. Blocking peer(s): \
         {list}. Upgrade them and retry."
    )
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

/// Operator-facing 409 message naming each peer that blocks the start and why.
fn format_incompatible_peers(incompatible: &[(String, String)]) -> String {
    let list = incompatible
        .iter()
        .map(|(id, reason)| format!("{id} ({reason})"))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "Cannot start the workflow: every invited peer must run dec-party-manager \
         >= {MIN_PEER_VERSION} (concurrent workflows changed the Noise wire format, \
         older builds cannot participate). Blocking peer(s): {list}. Upgrade or \
         deselect them and retry."
    )
}

/// Any in-progress run on this node (either role) already targeting the given
/// decentralized party. The old global one-at-a-time gate incidentally
/// prevented two workflows from mutating the same party's topology
/// concurrently (e.g. two kicks, or a kick racing a contracts deployment);
/// with concurrent workflows that protection must be explicit. Local-node
/// guard only — two DIFFERENT nodes coordinating conflicting workflows on the
/// same party is a distributed conflict Canton itself surfaces.
async fn find_inprogress_run_for_party(
    db: &SqlitePool,
    party: &CantonId,
) -> Option<(String, WorkflowKind)> {
    SchemaRead::get_in_progress_workflow_runs(db)
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|r| r.dec_party_id.as_ref() == Some(party))
        .map(|r| (r.instance_name, r.kind))
}

async fn find_party_with_prefix(db: &SqlitePool, prefix: &str) -> Result<Option<String>> {
    use crate::db::schema::SchemaRead;

    let parties = db.get_dec_parties_by_prefix(prefix).await?;
    Ok(parties.into_iter().next().map(|p| p.party_id))
}

/// `instance` is the cancelled run's `instance_name`, stamped onto the message
/// so peers cancel only that run's invite/peer-run — not every run they hold
/// from this coordinator (which may have other concurrent runs in flight).
async fn send_cancel_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
    instance: &str,
) -> Result {
    broadcast_simple_message(
        config,
        db,
        peer_ids,
        Message::new_empty(MessageType::CancelInvite).with_instance(instance),
        "CancelInvite",
    )
    .await
}

/// Best-effort: notify previously-invited peers that the coordinator is
/// retrying the workflow so they flip their Failed row back to InProgress
/// and re-spin `start_peer`. `instance` scopes the retry to this run's peer
/// rows (see `send_cancel_invites`).
async fn send_retry_workflow(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
    instance: &str,
) -> Result {
    broadcast_simple_message(
        config,
        db,
        peer_ids,
        Message::new_empty(MessageType::RetryWorkflow).with_instance(instance),
        "RetryWorkflow",
    )
    .await
}

async fn broadcast_simple_message(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
    message: Message,
    label: &str,
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;
    let identity = config.participant_id().to_string();

    for peer_id in peer_ids {
        let Some(peer) = network_config
            .peers
            .iter()
            .find(|p| &p.participant_id == peer_id)
        else {
            continue;
        };
        if peer.public_key.is_empty() {
            continue;
        }
        let Ok(peer_pub_key) = parse_public_key(&peer.public_key) else {
            continue;
        };
        let psk = keypair.derive_psk(&peer_pub_key);

        if let Err(e) = send_noise_message(
            &peer.address,
            peer.port,
            &psk,
            identity.as_bytes(),
            &message,
        )
        .await
        {
            tracing::warn!("Failed to send {label} to {peer_id}: {e}");
        }
    }
    Ok(())
}

/// Send contracts invites to this party's members (`invitees`) over Noise.
async fn send_contracts_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    contracts_config: &workflow::ContractsConfig,
    invitees: &[CantonId],
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let current_participant_id = config.participant_id();
    let invitee_set: HashSet<&CantonId> = invitees.iter().collect();
    // Carry the dec party, member set, and package names so the peer card
    // renders the same rich summary the coordinator shows (mirrors the Kick
    // invite). Skip empty names, then sort+dedup — `dedup` only removes
    // adjacent duplicates, and multiple contracts can share a package name.
    let mut package_names: Vec<String> = contracts_config
        .contracts
        .iter()
        .map(|c| c.name.clone())
        .filter(|n| !n.is_empty())
        .collect();
    package_names.sort();
    package_names.dedup();
    let payload = ContractsInvitePayload {
        dec_party_id: contracts_config.decentralized_party_id.clone(),
        participants: invitees.to_vec(),
        package_names,
        contract_intents: contracts_config.committed()?.intents.clone(),
        workflow_instance: Some(contracts_config.instance_name.clone()),
    };
    let payload_bytes = serde_json::to_vec(&payload).context("encode ContractsInvitePayload")?;
    let invite_message = Message::new(MessageType::InviteContracts, payload_bytes);

    for peer in &network_config.peers {
        if peer.participant_id == *current_participant_id {
            continue;
        }

        // Only this party's members (see start_contracts) — never every
        // configured peer.
        if !invitee_set.contains(&peer.participant_id) {
            continue;
        }

        if peer.public_key.is_empty() {
            tracing::warn!(
                "Skipping invite to {} - no public key configured",
                peer.participant_id
            );
            continue;
        }

        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!(
                    "Skipping invite to {} - invalid public key: {e}",
                    peer.participant_id
                );
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);
        // Use participant_id as identity (must match what server expects in peer_keys lookup)
        let identity = config.participant_id().to_string();

        tracing::info!(
            "Sending contracts invite to {} at {}:{}",
            peer.participant_id,
            peer.address,
            peer.port
        );

        match send_noise_message(
            &peer.address,
            peer.port,
            &psk,
            identity.as_bytes(),
            &invite_message,
        )
        .await
        {
            Ok(response) => {
                interpret_invite_reply(&peer.participant_id, "contracts", &response)?;
            }
            Err(e) => {
                tracing::error!(
                    "Failed to send contracts invite to {}: {e}",
                    peer.participant_id
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use actix_web::{App, HttpMessage, http::StatusCode, test as actix_test};
    use canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::{SigningKeyUsage, SigningKeysWithThreshold, SigningPublicKey},
        protocol::v30::{enums::ParticipantPermission, party_to_participant::HostingParticipant},
    };

    use super::super::invitations::insert_peer_run;
    use super::*;
    use crate::{
        auth::Principal,
        db::MIGRATOR,
        server::{AppState, PendingInvitation},
        workflow::{
            kick::prune_cached_membership, signing_keys::signing_keys_after_kick,
            validation::PeerExpectations,
        },
    };

    /// A minimal coordinator run row for `party`, in progress.
    fn in_progress_run(instance: &str, party: &CantonId) -> WorkflowRun {
        WorkflowRun {
            instance_name: instance.to_string(),
            kind: WorkflowKind::Contracts,
            role: WorkflowRole::Coordinator,
            status: WorkflowProgress::InProgress,
            current_step: "SubmitProposals".to_string(),
            step_index: 0,
            step_total: 1,
            config_json: "{}".to_string(),
            coordinator_pubkey: None,
            coordinator_instance: None,
            coordinator_name: None,
            expected_peers: Vec::new(),
            completed_peers: Vec::new(),
            connected_peers: Vec::new(),
            acs_progress: None,
            dec_party_id: Some(party.clone()),
            prefix: None,
            participants: Vec::new(),
            previous_threshold: None,
            new_threshold: None,
            kicked_participant: None,
            kicked_owner_key: None,
            added_participant: None,
            package_names: Vec::new(),
            dar_filenames: Vec::new(),
            error: None,
            dismissed: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    /// The invariant the completion ordering rests on: once a run row is
    /// terminal, the in-flight guard must not see it.
    ///
    /// This is the half that made the ordering bug reachable. `/…/status` reads
    /// the in-memory status while this guard reads the run rows, so marking the
    /// row terminal *after* flipping memory left a window where the API reported
    /// a run finished and refused the next workflow for the same party in the
    /// same breath. The handlers now write the row first; this pins what that
    /// write has to achieve.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_completed_run_is_not_in_flight_for_its_party(pool: SqlitePool) -> Result {
        let party = pid(7)?;
        let run = in_progress_run("contracts-1", &party);
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_workflow_run(&run).await?;
        Commitable::commit(tx).await?;

        // While it runs, a second workflow for the same party is refused — the
        // guard's whole purpose.
        let found = find_inprogress_run_for_party(&pool, &party).await;
        assert_eq!(
            found.map(|(run, _)| run),
            Some("contracts-1".to_string()),
            "an in-progress run must be reported as in flight"
        );

        mark_run_completed(&pool, WorkflowKind::Contracts, "contracts-1").await;

        assert!(
            find_inprogress_run_for_party(&pool, &party).await.is_none(),
            "a completed run must not block the next workflow for its party"
        );
        Ok(())
    }

    /// A failed run must not block the party either: the operator's next move
    /// after reading Failed is to retry, and a guard that still counted it would
    /// refuse that retry.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_run_is_not_in_flight_for_its_party(pool: SqlitePool) -> Result {
        let party = pid(8)?;
        let run = in_progress_run("contracts-2", &party);
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_workflow_run(&run).await?;
        Commitable::commit(tx).await?;

        mark_run_failed(&pool, WorkflowKind::Contracts, "contracts-2", "boom").await;

        assert!(
            find_inprogress_run_for_party(&pool, &party).await.is_none(),
            "a failed run must not block the retry it invites"
        );
        Ok(())
    }

    fn pid(tag: u8) -> anyhow::Result<CantonId> {
        let ns = format!("1220{:0>64}", format!("{tag:02x}"));
        CantonId::parse(&format!("validator-{tag}::{ns}"))
    }

    /// The coordinator signs the proposals, so it is a member of the party it
    /// is changing — but it is absent from its own `peers` table. A member set
    /// derived from that list must still count it, or every threshold bound
    /// computed from it is one too low.
    #[test]
    fn party_member_count_counts_this_node_when_the_set_omits_it() -> anyhow::Result<()> {
        let me = pid(1)?;
        let others: HashSet<CantonId> = [pid(2)?, pid(3)?, pid(4)?].into_iter().collect();

        // The shape that caused the bug: three other members cached, self absent.
        assert_eq!(party_member_count(&others, &me), 4);
        Ok(())
    }

    /// When the cache does contain this node — the normal case, since the
    /// refresh writes the chain's participant list verbatim — it must not be
    /// counted twice.
    #[test]
    fn party_member_count_does_not_double_count_a_present_self() -> anyhow::Result<()> {
        let me = pid(1)?;
        let all: HashSet<CantonId> = [me.clone(), pid(2)?, pid(3)?, pid(4)?]
            .into_iter()
            .collect();

        assert_eq!(party_member_count(&all, &me), 4);
        Ok(())
    }

    /// A one-member party is this node alone, whichever way the set was built.
    #[test]
    fn party_member_count_handles_a_lone_member() -> anyhow::Result<()> {
        let me = pid(1)?;
        assert_eq!(party_member_count(&HashSet::new(), &me), 1);
        assert_eq!(
            party_member_count(&[me.clone()].into_iter().collect(), &me),
            1
        );
        Ok(())
    }

    fn host_row(uid: &CantonId, owner_key: Option<&str>) -> DecPartyParticipantRow {
        DecPartyParticipantRow {
            dec_party_id: "party".to_string(),
            participant_uid: uid.to_string(),
            permission: "confirmation".to_string(),
            owner_key: owner_key.map(str::to_string),
            signing_key: None,
        }
    }

    fn owner_row(owner_key: &str, claimant: Option<&CantonId>) -> DecPartyOwnerRow {
        DecPartyOwnerRow {
            dec_party_id: "party".to_string(),
            owner_key: owner_key.to_string(),
            participant_uid: claimant.map(ToString::to_string),
        }
    }

    /// Hosts p1 and p2 hold keys "k1" and "k2"; "k3" is owned by p3, which
    /// hosts nothing, and "k4" by nobody that has said so.
    fn owner_only_party() -> anyhow::Result<(Vec<DecPartyParticipantRow>, Vec<DecPartyOwnerRow>)> {
        let (p1, p2, p3) = (pid(1)?, pid(2)?, pid(3)?);
        Ok((
            vec![host_row(&p1, Some("k1")), host_row(&p2, Some("k2"))],
            vec![
                owner_row("k1", Some(&p1)),
                owner_row("k2", None),
                owner_row("k3", Some(&p3)),
                owner_row("k4", None),
            ],
        ))
    }

    #[test]
    fn a_host_is_named_by_its_participant() -> anyhow::Result<()> {
        let (hosts, owners) = owner_only_party()?;
        let target = resolve_kick_target(Some(&pid(2)?), None, &hosts, &owners);
        assert_eq!(
            target,
            Ok(KickTarget {
                owner_key: "k2".to_string(),
                participant: Some(pid(2)?),
                hosts: true,
            })
        );
        Ok(())
    }

    /// An owner key a host holds kicks that host, whoever is named for it.
    #[test]
    fn a_host_key_kicks_its_host() -> anyhow::Result<()> {
        let (hosts, owners) = owner_only_party()?;
        let target = resolve_kick_target(None, Some("k2"), &hosts, &owners);
        assert_eq!(
            target,
            Ok(KickTarget {
                owner_key: "k2".to_string(),
                participant: Some(pid(2)?),
                hosts: true,
            })
        );
        let mismatch = resolve_kick_target(Some(&pid(1)?), Some("k2"), &hosts, &owners);
        assert!(
            matches!(&mismatch, Err(KickTargetError::BadRequest(e)) if e.contains("belongs to")),
            "unexpected result: {mismatch:?}"
        );
        Ok(())
    }

    #[test]
    fn an_owner_that_hosts_nothing_is_named_by_its_claimant_or_its_key() -> anyhow::Result<()> {
        let (hosts, owners) = owner_only_party()?;
        let expected = KickTarget {
            owner_key: "k3".to_string(),
            participant: Some(pid(3)?),
            hosts: false,
        };
        assert_eq!(
            resolve_kick_target(Some(&pid(3)?), None, &hosts, &owners).as_ref(),
            Ok(&expected)
        );
        assert_eq!(
            resolve_kick_target(None, Some("k3"), &hosts, &owners),
            Ok(expected)
        );
        assert_eq!(
            resolve_kick_target(None, Some("k4"), &hosts, &owners),
            Ok(KickTarget {
                owner_key: "k4".to_string(),
                participant: None,
                hosts: false,
            })
        );
        Ok(())
    }

    /// Until every host's key is known, an owner key may be the key of a
    /// host, and kicking it would strip that host of its ownership. A claim
    /// does not settle it, because any peer can claim any key.
    #[test]
    fn an_owner_only_kick_waits_for_every_host_key() -> anyhow::Result<()> {
        let (mut hosts, owners) = owner_only_party()?;
        hosts[1].owner_key = None;
        for (participant, key) in [
            (None, Some("k4")),
            (None, Some("k3")),
            (Some(pid(3)?), None),
        ] {
            let target = resolve_kick_target(participant.as_ref(), key, &hosts, &owners);
            assert!(
                matches!(&target, Err(KickTargetError::Conflict(e)) if e.contains("not been resolved")),
                "{participant:?} / {key:?}: unexpected result: {target:?}"
            );
        }
        Ok(())
    }

    /// Host 2 rotated its owner key to k4, and the cache still holds k2. The
    /// unclaimed k4 may be host 2's new key, so neither k4 nor host 2 can be
    /// kicked until a refresh resolves it.
    #[test]
    fn a_host_key_that_is_no_longer_an_owner_counts_as_unresolved() -> anyhow::Result<()> {
        let (hosts, mut owners) = owner_only_party()?;
        owners.retain(|owner| owner.owner_key != "k2");
        for (participant, key) in [(None, Some("k4")), (Some(pid(2)?), None)] {
            let target = resolve_kick_target(participant.as_ref(), key, &hosts, &owners);
            assert!(
                matches!(&target, Err(KickTargetError::Conflict(e)) if e.contains("not been resolved")),
                "{participant:?} / {key:?}: unexpected result: {target:?}"
            );
        }
        Ok(())
    }

    /// A participant label must come from a claim, or the run card would name
    /// a participant nobody tied to the key.
    #[test]
    fn a_participant_must_match_the_claim_on_the_key() -> anyhow::Result<()> {
        let (hosts, owners) = owner_only_party()?;
        for (participant, key, expected) in [
            (pid(4)?, "k3", "was claimed by"),
            (pid(4)?, "k4", "No peer has claimed"),
            (pid(1)?, "k4", "under another owner key"),
        ] {
            let target = resolve_kick_target(Some(&participant), Some(key), &hosts, &owners);
            assert!(
                matches!(&target, Err(KickTargetError::BadRequest(e)) if e.contains(expected)),
                "{participant} / {key}: unexpected result: {target:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn a_kick_must_name_a_member() -> anyhow::Result<()> {
        let (hosts, owners) = owner_only_party()?;
        for (participant, key) in [(None, None), (Some(pid(5)?), None), (None, Some("k9"))] {
            let target = resolve_kick_target(participant.as_ref(), key, &hosts, &owners);
            assert!(
                matches!(target, Err(KickTargetError::BadRequest(_))),
                "unexpected result: {target:?}"
            );
        }
        Ok(())
    }

    fn health_reply(version: &str, features: &[&str]) -> HealthReply {
        HealthReply {
            status: crate::server::ConnectionStatus::Connected,
            workflow: None,
            version: Some(version.to_string()),
            build_version: None,
            features: features.iter().map(ToString::to_string).collect(),
        }
    }

    /// Every build reports one version until a release bumps it, so only the
    /// feature list tells a peer that can sign an owner-only kick. A peer that
    /// lists no features, or that did not answer, cannot.
    #[test]
    fn an_owner_only_kick_needs_every_peer_to_list_the_feature() {
        let replies: Vec<InviteeHealth> = vec![
            (
                "p2".to_string(),
                Ok(health_reply("1.14.0", &[OWNER_ONLY_KICK])),
            ),
            ("p3".to_string(), Ok(health_reply("1.14.0", &[]))),
            ("p4".to_string(), Err("timed out".to_string())),
        ];
        assert_eq!(
            lacking_feature(&replies, OWNER_ONLY_KICK),
            vec![
                ("p3".to_string(), "running version 1.14.0".to_string()),
                ("p4".to_string(), "could not verify: timed out".to_string()),
            ]
        );
        assert!(incompatible_versions(&replies[..2]).is_empty());
    }

    /// This build lists the rotation, so a peer on it passes the gate. A peer
    /// on an older build, or one that did not answer, blocks the start, and
    /// the message names it.
    #[test]
    fn a_rotation_needs_every_peer_to_list_the_feature() {
        assert!(crate::consts::FEATURES.contains(&ROTATE_KEY));
        let replies: Vec<InviteeHealth> = vec![
            (
                "p2".to_string(),
                Ok(health_reply("1.15.0", crate::consts::FEATURES)),
            ),
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

    /// Three hosts and four owners. A host's kick leaves two keys to sign
    /// with; an owner-only kick leaves all three.
    #[test]
    fn the_kick_bound_counts_the_hosts_and_the_owners_left() {
        assert_eq!(kick_threshold_bound(true, 3, 4), 2);
        assert_eq!(kick_threshold_bound(false, 3, 4), 3);
        // Two owners hosting nothing: the hosts left bound it below the owners.
        assert_eq!(kick_threshold_bound(false, 2, 4), 2);
        // An uncached owner set bounds by the hosts, as before.
        assert_eq!(kick_threshold_bound(true, 4, 0), 3);
    }

    /// The bug in the numbers that produced it: a 4-member party at threshold
    /// 3 could not be kicked, because the bound said 2 while the peers demanded
    /// 3. With self counted the bound is 3 and the kick is expressible.
    #[test]
    fn kick_bound_admits_the_threshold_the_peers_require() -> anyhow::Result<()> {
        let me = pid(1)?;
        let cached_without_self: HashSet<CantonId> =
            [pid(2)?, pid(3)?, pid(4)?].into_iter().collect();

        let bound = party_member_count(&cached_without_self, &me) as i32 - 1;
        assert_eq!(bound, 3, "a 4-member party leaves 3 after a kick");
        assert!(bound >= 3, "threshold 3 must be expressible");
        Ok(())
    }

    fn ids(tags: &[u8]) -> anyhow::Result<HashSet<CantonId>> {
        tags.iter().map(|tag| pid(*tag)).collect()
    }

    fn signers(invitees: &[u8], not_peers: &[u8]) -> anyhow::Result<KickSigners> {
        Ok(KickSigners {
            invitees: invitees
                .iter()
                .map(|tag| pid(*tag))
                .collect::<anyhow::Result<_>>()?,
            not_peers: not_peers
                .iter()
                .map(|tag| pid(*tag))
                .collect::<anyhow::Result<_>>()?,
        })
    }

    #[test]
    fn kick_signers_invites_only_members_that_are_peers() -> anyhow::Result<()> {
        let split = kick_signers(
            &ids(&[1, 2, 3, 4])?,
            &ids(&[1, 2, 3, 9])?,
            &pid(1)?,
            Some(&pid(4)?),
        );
        assert_eq!(split, signers(&[2, 3], &[])?);
        Ok(())
    }

    #[test]
    fn a_kicked_host_that_is_not_a_peer_is_neither_invited_nor_reported() -> anyhow::Result<()> {
        let split = kick_signers(&ids(&[1, 2, 3])?, &ids(&[2])?, &pid(1)?, Some(&pid(3)?));
        assert_eq!(split, signers(&[2], &[])?);
        Ok(())
    }

    #[test]
    fn kick_signers_names_surviving_members_that_are_not_peers() -> anyhow::Result<()> {
        let split = kick_signers(&ids(&[1, 2, 3, 4])?, &ids(&[2])?, &pid(1)?, Some(&pid(4)?));
        assert_eq!(split, signers(&[2], &[3])?);
        Ok(())
    }

    #[test]
    fn a_kick_below_quorum_names_the_members_that_are_not_peers() -> anyhow::Result<()> {
        let refusal = kick_quorum_refusal(Some(3), 2, &signers(&[2], &[3, 5])?)
            .context("a kick two signers short of threshold 3 must be refused")?;
        assert!(
            refusal.contains(
                "the party's current namespace threshold is 3. Only 2 members can sign: this \
                 node and 1 configured peer."
            ),
            "{refusal}"
        );
        for tag in [3, 5] {
            assert!(
                refusal.contains(&format!(
                    "{} is a member of the party but is not a configured peer of this node.",
                    pid(tag)?
                )),
                "{refusal}"
            );
        }
        assert!(
            refusal.ends_with(" Add them as peers, then retry."),
            "{refusal}"
        );
        assert!(!refusal.contains(&pid(2)?.to_string()), "{refusal}");
        Ok(())
    }

    /// Adding the members that are not peers would still leave the kick
    /// short, so the advice is about the threshold instead.
    #[test]
    fn a_kick_that_peers_cannot_save_points_to_the_threshold() -> anyhow::Result<()> {
        let refusal = kick_quorum_refusal(Some(4), 2, &signers(&[2], &[3])?)
            .context("three reachable signers cannot meet threshold 4")?;
        assert!(refusal.contains(&pid(3)?.to_string()), "{refusal}");
        assert!(!refusal.contains("Add it as a peer"), "{refusal}");
        assert!(
            refusal.ends_with(
                " Lower the threshold with Change Threshold first, while every host can still \
                 sign."
            ),
            "{refusal}"
        );
        Ok(())
    }

    #[test]
    fn a_short_kick_says_which_threshold_is_the_limit() -> anyhow::Result<()> {
        let current = kick_quorum_refusal(Some(3), 2, &signers(&[2], &[])?)
            .context("the current threshold 3 is out of reach")?;
        assert!(
            current.starts_with("Cannot kick: the party's current namespace threshold is 3."),
            "{current}"
        );
        assert!(current.contains("Change Threshold"), "{current}");
        assert!(!current.contains("Pick a new_threshold"), "{current}");

        let requested = kick_quorum_refusal(Some(2), 3, &signers(&[2], &[])?)
            .context("the requested threshold 3 is out of reach")?;
        assert!(
            requested.starts_with("Cannot kick: the requested new_threshold is 3."),
            "{requested}"
        );
        assert!(
            requested.ends_with(" Pick a new_threshold of 2 or lower."),
            "{requested}"
        );
        assert!(!requested.contains("Change Threshold"), "{requested}");

        let both = kick_quorum_refusal(Some(4), 3, &signers(&[2], &[])?)
            .context("both thresholds are out of reach")?;
        assert!(both.contains("current namespace threshold is 4."), "{both}");
        assert!(
            both.ends_with(" Then pick a new_threshold of 2 or lower."),
            "{both}"
        );
        Ok(())
    }

    #[test]
    fn the_key_adoption_refusal_names_each_owner_and_the_next_step() -> anyhow::Result<()> {
        assert_eq!(kick_missing_adopters(&[]), None);
        let one = kick_missing_adopters(&[pid(3)?.to_string()])
            .context("one owner that is not a peer must sign")?;
        assert_eq!(
            one,
            format!(
                "Cannot kick: the party still keeps signing keys in the older key mapping. The \
                 kick moves them into the hosting list. The owner of each moved key must sign. \
                 {p3} owns such a key but is not a configured peer of this node. Add it as a \
                 peer, then retry.",
                p3 = pid(3)?
            )
        );
        let two = kick_missing_adopters(&[pid(3)?.to_string(), pid(4)?.to_string()])
            .context("two owners that are not peers must sign")?;
        assert!(
            two.ends_with(
                " own such keys but are not configured peers of this node. Add them as peers, \
                 then retry."
            ),
            "{two}"
        );
        assert!(!two.contains("inline"), "{two}");
        Ok(())
    }

    #[test]
    fn a_kick_at_quorum_passes_with_a_member_that_is_not_a_peer() -> anyhow::Result<()> {
        assert_eq!(kick_quorum_refusal(Some(2), 2, &signers(&[2], &[3])?), None);
        Ok(())
    }

    #[test]
    fn an_uncached_threshold_skips_the_quorum_check() -> anyhow::Result<()> {
        assert_eq!(kick_quorum_refusal(None, 2, &signers(&[2], &[3])?), None);
        Ok(())
    }

    #[test]
    fn a_kick_with_no_invitee_is_refused() -> anyhow::Result<()> {
        let alone = kick_quorum_refusal(None, 1, &signers(&[], &[])?)
            .context("a kick with no co-signer must be refused")?;
        assert_eq!(
            alone,
            "Cannot kick: this node has no other hosting member to co-sign the kick."
        );

        let refusal = kick_quorum_refusal(Some(1), 1, &signers(&[], &[3])?)
            .context("a kick whose only survivor is not a peer must be refused")?;
        assert!(
            refusal.contains(&format!("{} is a member of the party", pid(3)?)),
            "{refusal}"
        );
        assert!(
            refusal.ends_with(" Add it as a peer, then retry."),
            "{refusal}"
        );
        Ok(())
    }

    /// [`state_on`], with `pid(1)` as this node.
    async fn kick_state_on(pool: SqlitePool) -> Result<web::Data<AppState>> {
        let state = Arc::try_unwrap(state_on(pool).await?.into_inner())
            .map_err(|_| anyhow::anyhow!("a fresh test state has one owner"))?;
        let mut config = NodeConfig::default();
        config.node.participant_id = Some(pid(1)?);
        Ok(web::Data::new(AppState { config, ..state }))
    }

    /// A party hosted by this node and `pid(2..=4)` at namespace threshold 3.
    /// Each host holds its own owner key. Only `pid(2)` is a configured peer.
    async fn cache_party_with_one_peer(pool: &SqlitePool) -> Result<CantonId> {
        let party = pid(40)?;
        cache_party(pool, &party, 3, None).await?;
        let keys: Vec<String> = (1..=4).map(|n| format!("k{n}")).collect();
        let hosts = (1..=4)
            .zip(&keys)
            .map(|(tag, key)| Ok(host_row(&pid(tag)?, Some(key))))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut tx = pool.begin_transaction().await?;
        tx.replace_dec_party_participants(&party, &hosts).await?;
        tx.replace_dec_party_owners(&party, &keys).await?;
        tx.insert_peer(&crate::config::Peer {
            participant_id: pid(2)?,
            name: "peer-2".to_string(),
            address: "127.0.0.1".to_string(),
            port: 1,
            public_key: String::new(),
            party: None,
        })
        .await?;
        Commitable::commit(tx).await?;
        Ok(party)
    }

    /// A kick of `target` at threshold 2.
    fn kick_request(party: &CantonId, target: &CantonId) -> actix_test::TestRequest {
        actix_test::TestRequest::post()
            .uri("/kick")
            .set_json(serde_json::json!({
                "decentralized_party_id": party,
                "participant_id": target,
                "new_threshold": 2,
            }))
    }

    async fn status_and_body(response: actix_web::dev::ServiceResponse) -> (StatusCode, String) {
        let status = response.status();
        let body = actix_test::read_body(response).await;
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    /// The kicked host has no peer entry, which a kick does not need. The
    /// refusal is about the survivor that is not a peer, so it names that
    /// survivor. A participant outside the party is still refused as one.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_kick_refused_for_quorum_names_the_survivor_not_the_target(
        pool: SqlitePool,
    ) -> Result {
        let party = cache_party_with_one_peer(&pool).await?;
        let app = actix_test::init_service(
            App::new()
                .app_data(kick_state_on(pool).await?)
                .service(start_kick),
        )
        .await;

        let request = kick_request(&party, &pid(4)?).to_request();
        request.extensions_mut().insert(admin());
        let response = actix_test::call_service(&app, request).await;
        let (status, body) = status_and_body(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.contains(&format!(
                "{} is a member of the party but is not a configured peer of this node",
                pid(3)?
            )),
            "{body}"
        );
        assert!(
            !body.contains("not a member of this decentralized party"),
            "{body}"
        );

        let request = kick_request(&party, &pid(9)?).to_request();
        request.extensions_mut().insert(admin());
        let response = actix_test::call_service(&app, request).await;
        let (status, body) = status_and_body(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.contains("not a member of this decentralized party"),
            "{body}"
        );
        Ok(())
    }

    /// Caches `party` at namespace threshold `threshold`, hosted by `hosts`
    /// (this node is `pid(1)`), each under its own owner key `k{tag}`. The
    /// keys in `owners_only` own the party without hosting it. Only `peers`
    /// are configured peers of this node.
    ///
    /// The party also gets a run in flight. A kick that passes every check
    /// then stops at the in-flight guard with a 409, before it probes or
    /// invites any peer.
    async fn cache_kick_party(
        pool: &SqlitePool,
        party: &CantonId,
        threshold: i64,
        hosts: &[u8],
        owners_only: &[&str],
        peers: &[u8],
    ) -> Result {
        cache_party(pool, party, threshold, None).await?;
        let mut keys: Vec<String> = hosts.iter().map(|tag| format!("k{tag}")).collect();
        let host_rows = hosts
            .iter()
            .zip(&keys)
            .map(|(tag, key)| Ok(host_row(&pid(*tag)?, Some(key))))
            .collect::<anyhow::Result<Vec<_>>>()?;
        keys.extend(owners_only.iter().map(ToString::to_string));
        let mut tx = pool.begin_transaction().await?;
        tx.replace_dec_party_participants(party, &host_rows).await?;
        tx.replace_dec_party_owners(party, &keys).await?;
        for tag in peers {
            tx.insert_peer(&crate::config::Peer {
                participant_id: pid(*tag)?,
                name: format!("peer-{tag}"),
                address: "127.0.0.1".to_string(),
                port: 1,
                public_key: String::new(),
                party: None,
            })
            .await?;
        }
        Commitable::commit(tx).await?;
        insert_run(pool, &in_progress_run("contracts-in-flight", party)).await
    }

    async fn post_kick(pool: SqlitePool, body: serde_json::Value) -> Result<(StatusCode, String)> {
        let app = actix_test::init_service(
            App::new()
                .app_data(kick_state_on(pool).await?)
                .service(start_kick),
        )
        .await;
        let request = actix_test::TestRequest::post()
            .uri("/kick")
            .set_json(body)
            .to_request();
        request.extensions_mut().insert(admin());
        Ok(status_and_body(actix_test::call_service(&app, request).await).await)
    }

    /// Issue #617: the operator removed a lost host from the peer list, then
    /// kicked it. The remaining members still reach the threshold, so the
    /// kick must pass every check. It reaches the in-flight guard, the last
    /// step before invitations.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_host_removed_from_the_peer_list_can_still_be_kicked(pool: SqlitePool) -> Result {
        let party = pid(41)?;
        cache_kick_party(&pool, &party, 2, &[1, 2, 3], &[], &[2]).await?;

        let (status, body) = post_kick(
            pool,
            serde_json::json!({
                "decentralized_party_id": party,
                "participant_id": pid(3)?,
                "new_threshold": 2,
            }),
        )
        .await?;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("already has a"), "{body}");
        Ok(())
    }

    /// The bound includes every surviving host, but a higher threshold
    /// still needs enough configured peers to sign the P2P update.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_raised_threshold_needs_enough_reachable_signers(pool: SqlitePool) -> Result {
        let party = pid(42)?;
        cache_kick_party(&pool, &party, 2, &[1, 2, 3, 4], &[], &[2, 3]).await?;
        let kick = |new_threshold: i64| -> anyhow::Result<serde_json::Value> {
            Ok(serde_json::json!({
                "decentralized_party_id": party,
                "participant_id": pid(3)?,
                "new_threshold": new_threshold,
            }))
        };

        let (status, body) = post_kick(pool.clone(), kick(4)?).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.contains("new_threshold must be between 1 and 3"),
            "{body}"
        );

        let (status, body) = post_kick(pool.clone(), kick(3)?).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.contains("the requested new_threshold is 3. Only 2 members can sign"),
            "{body}"
        );
        assert!(body.contains(&pid(4)?.to_string()), "{body}");
        assert!(body.contains("Add it as a peer, then retry."), "{body}");
        assert!(!body.contains(&pid(3)?.to_string()), "{body}");

        let (status, body) = post_kick(pool, kick(2)?).await?;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("already has a"), "{body}");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_raised_threshold_at_reachable_quorum_passes(pool: SqlitePool) -> Result {
        let party = pid(44)?;
        cache_kick_party(&pool, &party, 2, &[1, 2, 3, 4], &[], &[2, 4]).await?;

        let (status, body) = post_kick(
            pool,
            serde_json::json!({
                "decentralized_party_id": party,
                "participant_id": pid(3)?,
                "new_threshold": 3,
            }),
        )
        .await?;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("already has a"), "{body}");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_lowered_kick_threshold_allows_an_immediate_second_kick(pool: SqlitePool) -> Result {
        let party = pid(45)?;
        cache_kick_party(&pool, &party, 3, &[1, 2, 3, 4], &[], &[2, 3, 4]).await?;

        prune_cached_membership(
            &pool,
            &party,
            Some(&pid(4)?),
            "k4",
            crate::db::rows::DecPartyTopologyVersion {
                namespace_serial: 2,
                p2p_serial: 2,
            },
            PartyThresholds::new(2, None),
        )
        .await?;

        let (status, body) = post_kick(
            pool,
            serde_json::json!({
                "decentralized_party_id": party,
                "participant_id": pid(3)?,
                "new_threshold": 2,
            }),
        )
        .await?;
        // The second kick passes quorum and reaches the no-invitations
        // in-flight guard. A stale cached threshold of 3 refuses it first.
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("already has a"), "{body}");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_quorum_kick_advertises_non_peers_without_waiting_for_them(
        pool: SqlitePool,
    ) -> Result {
        let party = pid(46)?;
        cache_party(&pool, &party, 2, None).await?;
        let signers = kick_signers(
            &ids(&[1, 2, 3, 4])?,
            &ids(&[2, 5])?,
            &pid(1)?,
            Some(&pid(4)?),
        );
        assert_eq!(signers.invitees, vec![pid(2)?]);
        assert_eq!(signers.not_peers, vec![pid(3)?]);
        let config = workflow::KickConfig::new(
            party.clone(),
            Some(pid(4)?),
            "k4".to_string(),
            2,
            None,
            2,
            "quorum-kick".to_string(),
        );
        let payload = kick_invite_payload(&config, &signers, &pid(1)?);
        // Exercise the existing wire schema; no new capability is needed.
        let payload: KickInvitePayload = serde_json::from_slice(&serde_json::to_vec(&payload)?)?;
        assert_eq!(
            payload.participants.iter().cloned().collect::<HashSet<_>>(),
            ids(&[1, 2, 3])?
        );

        insert_coordinator_run(
            &pool,
            &config.instance_name,
            WorkflowKind::Kick,
            KickStep::WaitingForPeers,
            &config,
            &signers.invitees,
            Some(party),
        )
        .await?;
        let run = pool
            .get_workflow_run(&config.instance_name)
            .await?
            .context("coordinator run missing")?;
        assert_eq!(run.expected_peers, vec![pid(2)?]);
        let state = workflow::WorkflowState::new(
            pool.clone(),
            run.instance_name,
            KickStep::WaitingForPeers,
            run.expected_peers,
            Some(1),
        );
        state.peer_connected(pid(2)?).await;
        assert_eq!(state.current_step().await, KickStep::ExportState);

        let mut received = serde_json::to_value(payload)?;
        received["id"] = "kick-invite".into();
        received["invitation_type"] = "Kick".into();
        received["coordinator_pubkey"] = "a1".repeat(32).into();
        received["received_at"] = 1000.into();
        let invitation: PendingInvitation = serde_json::from_value(received)?;
        let peer_instance = insert_peer_run(&pool, &invitation)
            .await
            .context("accepted peer run missing")?;
        let mut node = NodeConfig::default();
        node.node.participant_id = Some(pid(2)?);
        let accepted = PeerExpectations::load(&pool, &peer_instance, &node, &pid(1)?).await?;
        assert_eq!(
            accepted.members.into_iter().collect::<HashSet<_>>(),
            ids(&[1, 2, 3])?
        );
        assert_eq!(accepted.kicked_participant, Some(pid(4)?));
        assert_eq!(accepted.threshold, Some(2));
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_unreadable_key_adoption_preflight_creates_no_kick_run(pool: SqlitePool) -> Result {
        let party = pid(47)?;
        cache_kick_party(&pool, &party, 2, &[1, 2, 3, 4], &[], &[2]).await?;
        record_signing_keys(&pool, &party, &[(2, 2), (3, 3)]).await?;
        let (state, status, body) = post_kick_without_run_in_flight(&pool, &party, 4).await?;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(
            error_text(&body)?,
            "Cannot check the party's signing keys right now. Retry later."
        );
        assert_no_kick_started(&pool, &state).await
    }

    /// Posts a kick of `pid(target)` at threshold 2 once the party's run in
    /// flight is gone. Canton is unreachable, so a ledger read fails at once.
    async fn post_kick_without_run_in_flight(
        pool: &SqlitePool,
        party: &CantonId,
        target: u8,
    ) -> Result<(web::Data<AppState>, StatusCode, String)> {
        sqlx::query("DELETE FROM workflow_runs WHERE instance_name = 'contracts-in-flight'")
            .execute(pool)
            .await?;
        let state = Arc::try_unwrap(kick_state_on(pool.clone()).await?.into_inner())
            .map_err(|_| anyhow::anyhow!("fresh state has one owner"))?;
        let mut config = state.config.clone();
        // Invalid URI fails at the Canton boundary without opening a socket.
        config.canton.admin_api_host = "[".to_string();
        let state = web::Data::new(AppState { config, ..state });
        let app =
            actix_test::init_service(App::new().app_data(state.clone()).service(start_kick)).await;
        let request = kick_request(party, &pid(target)?).to_request();
        request.extensions_mut().insert(admin());
        let (status, body) = status_and_body(actix_test::call_service(&app, request).await).await;
        Ok((state, status, body))
    }

    /// The `error` field of an error response body.
    fn error_text(body: &str) -> Result<String> {
        serde_json::from_str::<serde_json::Value>(body)?["error"]
            .as_str()
            .map(ToString::to_string)
            .with_context(|| format!("no error text in {body}"))
    }

    async fn assert_no_kick_started(pool: &SqlitePool, state: &web::Data<AppState>) -> Result {
        assert!(
            pool.get_workflow_runs_by_kind(WorkflowKind::Kick)
                .await?
                .is_empty()
        );
        assert!(state.workflows.snapshot().is_empty());
        assert!(state.pending_invitations.read().await.is_empty());
        Ok(())
    }

    /// Every remaining host's key must be on record, or the proposal cannot
    /// keep it. The refusal comes before any run exists. It names the member,
    /// and a member that is not a peer is told to become one.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_kick_needs_every_remaining_hosts_key_before_any_run(pool: SqlitePool) -> Result {
        let party = pid(50)?;
        cache_kick_party(&pool, &party, 2, &[1, 2, 3, 4], &[], &[2]).await?;
        record_signing_keys(&pool, &party, &[(2, 2)]).await?;

        let (state, status, body) = post_kick_without_run_in_flight(&pool, &party, 4).await?;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        let error = error_text(&body)?;
        assert_eq!(
            error,
            format!(
                "Cannot kick: this node has no record of the signing key of {p3}. The kick \
                 must keep the key of each member that stays. {p3} is not a configured peer \
                 of this node, so a refresh cannot read its key. Add it as a peer, refresh \
                 /decentralized-parties, then retry.",
                p3 = pid(3)?
            )
        );
        assert_no_kick_started(&pool, &state).await
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_peer_without_a_recorded_key_is_asked_to_refresh(pool: SqlitePool) -> Result {
        let party = pid(51)?;
        cache_kick_party(&pool, &party, 2, &[1, 2, 3], &[], &[2, 3]).await?;
        let (state, status, body) = post_kick_without_run_in_flight(&pool, &party, 3).await?;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        let error = error_text(&body)?;
        assert!(error.contains(&pid(2)?.to_string()), "{error}");
        assert!(
            error.ends_with(
                " Refresh /decentralized-parties so each peer reports its key, then retry."
            ),
            "{error}"
        );
        assert!(!error.contains("Add it as a peer"), "{error}");
        assert_no_kick_started(&pool, &state).await
    }

    /// Two remaining members that claim one key are a conflict no retry can
    /// settle. The refusal is a 409 and comes before any run exists.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn two_survivors_that_claim_one_key_are_refused_with_a_conflict(
        pool: SqlitePool,
    ) -> Result {
        let party = pid(52)?;
        cache_kick_party(&pool, &party, 2, &[1, 2, 3, 4], &[], &[2, 3]).await?;
        record_signing_keys(&pool, &party, &[(2, 2), (3, 2), (4, 4)]).await?;

        let (state, status, body) = post_kick_without_run_in_flight(&pool, &party, 4).await?;

        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        let error = error_text(&body)?;
        assert!(error.contains("both claim signing key"), "{error}");
        assert!(error.contains(&pid(2)?.to_string()), "{error}");
        assert!(error.contains(&pid(3)?.to_string()), "{error}");
        assert_no_kick_started(&pool, &state).await
    }

    /// The kicked member reports a remaining member's key as its own. That
    /// claim is ignored, so it cannot block its own kick.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_target_that_claims_a_survivors_key_passes_the_key_check(pool: SqlitePool) -> Result {
        let party = pid(53)?;
        cache_kick_party(&pool, &party, 2, &[1, 2, 3, 4], &[], &[2, 3]).await?;
        record_signing_keys(&pool, &party, &[(2, 2), (3, 3), (4, 2)]).await?;
        let signers = kick_signers(
            &ids(&[1, 2, 3, 4])?,
            &ids(&[2, 3])?,
            &pid(1)?,
            Some(&pid(4)?),
        );

        check_kick_survivor_keys(&pool, &party, &pid(1)?, &signers, &pid(4)?.to_string()).await
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_kick_with_no_missing_peer_skips_the_extra_topology_read(pool: SqlitePool) -> Result {
        let mut config = NodeConfig::default();
        config.canton.admin_api_host = "[".to_string();
        assert_eq!(
            kick_key_adoption_refusal(&config, &pool, &pid(48)?, &[], "unused", None).await?,
            None
        );
        Ok(())
    }

    fn kick_public_key(tag: u8) -> SigningPublicKey {
        SigningPublicKey {
            public_key: vec![tag; 32],
            usage: vec![SigningKeyUsage::Protocol as i32],
            ..Default::default()
        }
    }

    /// Records `kick_public_key(key)` as the signing key of host `pid(host)`.
    async fn record_signing_keys(pool: &SqlitePool, party: &CantonId, keys: &[(u8, u8)]) -> Result {
        for (host, key) in keys {
            sqlx::query(
                "UPDATE dec_party_participant SET signing_key = ? \
                 WHERE dec_party_id = ? AND participant_uid = ?",
            )
            .bind(utils::compute_fingerprint(&kick_public_key(*key)).to_string())
            .bind(party.to_string())
            .bind(pid(*host)?.to_string())
            .execute(pool)
            .await?;
        }
        Ok(())
    }

    async fn fully_inline_kick_keys(
        pool: &SqlitePool,
    ) -> Result<(NodeConfig, CantonId, PartyToParticipant)> {
        let party = pid(49)?;
        cache_kick_party(pool, &party, 2, &[1, 2, 3, 4], &[], &[2]).await?;
        record_signing_keys(pool, &party, &[(1, 1), (2, 2), (3, 3), (4, 4)]).await?;
        let mut config = NodeConfig::default();
        config.node.participant_id = Some(pid(1)?);
        config.canton.admin_api_host = "[".to_string();
        let mapping = PartyToParticipant {
            party: party.to_string(),
            threshold: 2,
            participants: (1..=4)
                .map(|tag| -> Result<HostingParticipant> {
                    Ok(HostingParticipant {
                        participant_uid: pid(tag)?.to_string(),
                        permission: ParticipantPermission::Confirmation as i32,
                        ..Default::default()
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            party_signing_keys: Some(SigningKeysWithThreshold {
                keys: (1..=4).map(kick_public_key).collect(),
                threshold: 2,
            }),
        };
        Ok((config, party, mapping))
    }

    /// The kicked member claims the key of a remaining member that is not a
    /// peer. The preflight ignores that claim, and the kick removes the
    /// target's own key by elimination. A remaining member that claims the
    /// same key as another one is still refused.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn fully_inline_preflight_ignores_a_target_claiming_a_non_peers_key(
        pool: SqlitePool,
    ) -> Result {
        let (config, party, mapping) = fully_inline_kick_keys(&pool).await?;
        record_signing_keys(&pool, &party, &[(4, 3)]).await?;
        let preflight = |pool: SqlitePool| {
            let (config, party, mapping) = (config.clone(), party.clone(), mapping.clone());
            async move {
                kick_key_adoption_refusal_for_mapping(
                    &config,
                    &pool,
                    &party,
                    &[pid(3)?],
                    &pid(4)?.to_string(),
                    Some(&pid(4)?),
                    &mapping,
                )
                .await
            }
        };

        assert_eq!(preflight(pool.clone()).await?, None);
        let keys = &mapping
            .party_signing_keys
            .as_ref()
            .context("inline keys")?
            .keys;
        let attributed = known_signing_keys_by_member(&config, &pool, &party, keys).await?;
        let kept = signing_keys_after_kick(
            keys,
            &pid(4)?.to_string(),
            &[
                pid(1)?.to_string(),
                pid(2)?.to_string(),
                pid(3)?.to_string(),
            ],
            &attributed,
        )?;
        assert_eq!(kept, keys[..3].to_vec());

        record_signing_keys(&pool, &party, &[(2, 3)]).await?;
        let error = preflight(pool)
            .await
            .expect_err("two remaining members claim one key");
        assert!(
            matches!(
                error.downcast_ref::<KeyClaimError>(),
                Some(KeyClaimError::Conflict(_))
            ),
            "{error:#}"
        );
        let response = kick_key_check_failure(&party, &error);
        assert_eq!(response.status(), StatusCode::CONFLICT);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_non_peer_needs_a_unique_known_key_even_on_a_fully_inline_party(
        pool: SqlitePool,
    ) -> Result {
        let (config, party, mut mapping) = fully_inline_kick_keys(&pool).await?;
        sqlx::query(
            "UPDATE dec_party_participant SET signing_key = NULL WHERE participant_uid = ?",
        )
        .bind(pid(3)?.to_string())
        .execute(&pool)
        .await?;
        let error = kick_key_adoption_refusal_for_mapping(
            &config,
            &pool,
            &party,
            &[pid(3)?],
            &pid(4)?.to_string(),
            Some(&pid(4)?),
            &mapping,
        )
        .await
        .expect_err("unknown non-peer ownership must request a refresh")
        .to_string();
        assert!(error.contains(&pid(3)?.to_string()), "{error}");
        assert!(error.contains("no record of the signing key"), "{error}");
        assert!(
            error.contains("A member that is not a peer must first be added as one."),
            "{error}"
        );
        sqlx::query("UPDATE dec_party_participant SET signing_key = ? WHERE participant_uid = ?")
            .bind(utils::compute_fingerprint(&kick_public_key(3)).to_string())
            .bind(pid(3)?.to_string())
            .execute(&pool)
            .await?;
        assert_eq!(
            kick_key_adoption_refusal_for_mapping(
                &config,
                &pool,
                &party,
                &[pid(3)?],
                &pid(4)?.to_string(),
                Some(&pid(4)?),
                &mapping,
            )
            .await?,
            None
        );
        // A reachable p2 needs adoption, but non-peer p3 is already inline.
        mapping.party_signing_keys = Some(SigningKeysWithThreshold {
            keys: vec![kick_public_key(1), kick_public_key(3)],
            threshold: 2,
        });
        assert_eq!(
            kick_key_adoption_refusal_for_mapping(
                &config,
                &pool,
                &party,
                &[pid(3)?],
                &pid(4)?.to_string(),
                Some(&pid(4)?),
                &mapping,
            )
            .await?,
            None
        );
        Ok(())
    }

    /// A host kick at a threshold equal to the host count can never collect
    /// enough signatures, because the kicked host does not sign. The refusal
    /// comes before any invitation. It is about the signers, so it does not
    /// name the target, whose peer entry the kick does not need.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_kick_short_of_signers_does_not_blame_the_target(pool: SqlitePool) -> Result {
        let party = pid(43)?;
        cache_kick_party(&pool, &party, 3, &[1, 2, 3], &[], &[2]).await?;

        let (status, body) = post_kick(
            pool,
            serde_json::json!({
                "decentralized_party_id": party,
                "participant_id": pid(3)?,
                "new_threshold": 2,
            }),
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.contains("current namespace threshold is 3. Only 2 members can sign"),
            "{body}"
        );
        assert!(body.contains("Change Threshold"), "{body}");
        assert!(!body.contains(&pid(3)?.to_string()), "{body}");
        Ok(())
    }

    /// An owner-only kick removes no host, so every host stays and has to
    /// sign. A host that is not a peer cannot be invited. When that leaves
    /// the run short of the threshold, the refusal names that host.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_owner_only_kick_short_of_signers_names_the_host_that_is_not_a_peer(
        pool: SqlitePool,
    ) -> Result {
        let party = pid(44)?;
        cache_kick_party(&pool, &party, 3, &[1, 2, 3], &["k5"], &[2]).await?;

        let (status, body) = post_kick(
            pool,
            serde_json::json!({
                "decentralized_party_id": party,
                "owner_key": "k5",
                "new_threshold": 2,
            }),
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.contains(&format!(
                "{} is a member of the party but is not a configured peer of this node",
                pid(3)?
            )),
            "{body}"
        );
        assert!(body.contains("namespace threshold is 3"), "{body}");
        Ok(())
    }

    async fn cache_party(
        pool: &SqlitePool,
        party: &CantonId,
        threshold: i64,
        confirmation_threshold: Option<i64>,
    ) -> Result {
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&crate::db::rows::DecPartyRow {
            party_id: party.to_string(),
            prefix: party.prefix.clone(),
            threshold,
            updated_at: 0,
            my_owner_key: None,
            confirmation_threshold,
        })
        .await?;
        Commitable::commit(tx).await
    }

    /// Kick and add-party read the party's two thresholds from the cache.
    /// Matching ones stay matched, so the run asks for no confirmation
    /// threshold of its own and the invite is what an older peer expects.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_cached_party_with_matching_thresholds_carries_none(pool: SqlitePool) -> Result {
        let party = pid(9)?;
        cache_party(&pool, &party, 2, Some(2)).await?;
        assert_eq!(cached_confirmation_threshold(&pool, &party, 4).await?, None);

        // A row cached before the column existed reads the same way.
        cache_party(&pool, &party, 2, None).await?;
        assert_eq!(cached_confirmation_threshold(&pool, &party, 4).await?, None);

        // So does a party that is not cached at all.
        assert_eq!(
            cached_confirmation_threshold(&pool, &pid(10)?, 4).await?,
            None
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_cached_party_with_distinct_thresholds_keeps_its_confirmation_threshold(
        pool: SqlitePool,
    ) -> Result {
        let party = pid(11)?;
        cache_party(&pool, &party, 1, Some(3)).await?;
        // Add-party grows the host set, so the threshold stays as it is.
        assert_eq!(
            cached_confirmation_threshold(&pool, &party, 4).await?,
            Some(3)
        );
        // A kick down to two hosts caps it at two.
        assert_eq!(
            cached_confirmation_threshold(&pool, &party, 2).await?,
            Some(2)
        );
        Ok(())
    }

    #[test]
    fn invite_reply_aborts_on_busy_only() -> anyhow::Result<()> {
        let peer = CantonId::parse(
            "participant::12200ad4539c269a7b13af6806fb2ee326e7c0d7233fa6144004c416502a2c73fb0b",
        )?;

        // Ack is the happy path.
        let ack = Message::new_empty(MessageType::Ack).to_bytes();
        assert!(interpret_invite_reply(&peer, "onboarding", &ack).is_ok());

        // Busy is a defensive net no current-version peer triggers (peers Ack
        // unconditionally and pre-Ack builds are version-gated out); were it ever
        // sent, it still aborts so the coordinator fails fast instead of waiting.
        let busy = Message::new(MessageType::Busy, b"Onboarding".to_vec()).to_bytes();
        assert!(interpret_invite_reply(&peer, "onboarding", &busy).is_err());

        // Any other (parseable) reply is tolerated — older peers may not Ack.
        let other = Message::new_empty(MessageType::Wait).to_bytes();
        assert!(interpret_invite_reply(&peer, "onboarding", &other).is_ok());

        Ok(())
    }

    #[test]
    fn external_party_threshold_bounded_by_host_count() {
        // 3 hosts (local + 2 peers): capped at N-1 = 2 so a host can still exit.
        assert!(validate_confirmation_threshold(None, 3).is_ok());
        assert!(validate_confirmation_threshold(Some(1), 3).is_ok());
        assert!(validate_confirmation_threshold(Some(2), 3).is_ok());
        // 0 can never reach quorum; N and above would block any unhost.
        assert!(validate_confirmation_threshold(Some(0), 3).is_err());
        assert!(validate_confirmation_threshold(Some(3), 3).is_err());
        assert!(validate_confirmation_threshold(Some(4), 3).is_err());
    }

    fn test_cid(prefix: &str) -> anyhow::Result<CantonId> {
        let ns = format!("1220{:0>64}", "a");
        CantonId::parse(&format!("{prefix}::{ns}"))
    }

    fn enrich_run(config_json: &str, expected_peers: Vec<CantonId>) -> WorkflowRun {
        WorkflowRun {
            instance_name: "t".to_string(),
            kind: WorkflowKind::Contracts,
            role: WorkflowRole::Coordinator,
            status: WorkflowProgress::InProgress,
            current_step: "Active".to_string(),
            step_index: 0,
            step_total: 5,
            config_json: config_json.to_string(),
            coordinator_pubkey: None,
            coordinator_instance: None,
            coordinator_name: None,
            expected_peers,
            completed_peers: Vec::new(),
            connected_peers: Vec::new(),
            acs_progress: None,
            dec_party_id: None,
            prefix: None,
            participants: Vec::new(),
            previous_threshold: None,
            new_threshold: None,
            kicked_participant: None,
            kicked_owner_key: None,
            added_participant: None,
            package_names: Vec::new(),
            dar_filenames: Vec::new(),
            error: None,
            dismissed: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn enrich_contracts_coordinator_surfaces_packages() -> anyhow::Result<()> {
        let peers = vec![test_cid("node1")?, test_cid("node2")?];
        let mut run = enrich_run(
            r#"{"contracts":[{"name":"Governance Core"},{"name":"Token Custody"}]}"#,
            peers.clone(),
        );
        enrich_from_config_json(&mut run);
        assert_eq!(run.package_names, vec!["Governance Core", "Token Custody"]);
        // Member set comes from expected_peers when config_json has no list.
        assert_eq!(run.participants, peers);
        Ok(())
    }

    #[test]
    fn enrich_contracts_peer_surfaces_packages_and_participants() -> anyhow::Result<()> {
        let peers = vec![test_cid("node1")?, test_cid("node2")?];
        let peers_json = serde_json::to_string(&peers)?;
        let mut run = enrich_run(
            &format!(r#"{{"package_names":["Governance Core"],"participants":{peers_json}}}"#),
            Vec::new(),
        );
        enrich_from_config_json(&mut run);
        assert_eq!(run.package_names, vec!["Governance Core"]);
        assert_eq!(run.participants, peers);
        Ok(())
    }

    #[test]
    fn enrich_dars_surfaces_filenames_without_dec_party() -> anyhow::Result<()> {
        let mut run = enrich_run(
            r#"{"dar_files":[{"filename":"app.dar"},{"filename":"lib.dar"}]}"#,
            Vec::new(),
        );
        enrich_from_config_json(&mut run);
        assert_eq!(run.dar_filenames, vec!["app.dar", "lib.dar"]);
        assert!(run.dec_party_id.is_none());
        Ok(())
    }

    /// The feed is polled every two seconds, so a run may put nothing
    /// unbounded on the wire. A Dars config carries each DAR's bytes.
    #[test]
    fn dars_run_keeps_its_dar_bytes_off_the_feed() -> anyhow::Result<()> {
        let payload = "A".repeat(4096);
        let mut run = enrich_run(
            &format!(r#"{{"dar_files":[{{"filename":"app.dar","data":"{payload}"}}]}}"#),
            Vec::new(),
        );
        enrich_from_config_json(&mut run);

        let feed = serde_json::to_string(&WorkflowRunsResponse { runs: vec![run] })?;
        assert!(!feed.contains(&payload), "the feed carries the DAR bytes");
        assert!(feed.contains("app.dar"), "the feed lost the DAR filename");

        // The CLI and the IT harness read this response back, so the field has
        // to default in rather than fail as missing.
        let back: WorkflowRunsResponse = serde_json::from_str(&feed)?;
        assert!(back.runs[0].config_json.is_empty());
        Ok(())
    }

    #[test]
    fn contracts_invite_payload_roundtrips_with_defaults() -> anyhow::Result<()> {
        let payload = ContractsInvitePayload {
            dec_party_id: test_cid("dec")?,
            participants: vec![test_cid("node1")?],
            package_names: vec!["Governance Core".to_string()],
            contract_intents: Vec::new(),
            workflow_instance: Some("dec-contracts-1".to_string()),
        };
        let bytes = serde_json::to_vec(&payload)?;
        let back: ContractsInvitePayload = serde_json::from_slice(&bytes)?;
        assert_eq!(back.package_names, vec!["Governance Core"]);
        assert_eq!(back.participants.len(), 1);
        // A minimal payload (only the required dec party) still decodes, so an
        // older coordinator stays compatible.
        let minimal: ContractsInvitePayload =
            serde_json::from_str(&format!(r#"{{"dec_party_id":"{}"}}"#, test_cid("dec")?))?;
        assert!(minimal.participants.is_empty());
        assert!(minimal.package_names.is_empty());
        Ok(())
    }

    /// A run with nothing to create would invite every peer and then fail on
    /// each of them at signing, so it is refused before anything is persisted
    /// or sent.
    #[actix_web::test]
    async fn start_contracts_refuses_a_request_with_no_contracts() -> anyhow::Result<()> {
        let app = actix_test::init_service(
            App::new()
                .app_data(AppState::for_test(None).await?)
                .service(start_contracts),
        )
        .await;
        let party = pid(9)?;
        let request = actix_test::TestRequest::post()
            .uri("/contracts")
            .set_json(serde_json::json!({
                "decentralized_party_id": party.to_string(),
                "participant_ids": [pid(10)?.to_string()],
                "participant_parties": [pid(11)?.to_string()],
                "operator_party": party.to_string(),
                "contracts": [],
            }))
            .to_request();
        request.extensions_mut().insert(Principal {
            sub: "admin".to_string(),
            issuer: "test".to_string(),
            roles: Vec::new(),
            email: None,
        });

        let response = actix_test::call_service(&app, request).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        Ok(())
    }

    fn started(kind: WorkflowKind) -> u64 {
        RUNS_STARTED
            .with_label_values(&[run_kind_label(kind)])
            .get()
    }

    fn finished(kind: WorkflowKind, outcome: RunOutcome) -> u64 {
        RUNS_FINISHED
            .with_label_values(&[run_kind_label(kind), outcome.label()])
            .get()
    }

    async fn insert_run(pool: &SqlitePool, run: &WorkflowRun) -> Result {
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_workflow_run(run).await?;
        Commitable::commit(tx).await
    }

    /// `AppState::for_test` on a migrated database.
    async fn state_on(pool: SqlitePool) -> Result<web::Data<AppState>> {
        let state = Arc::try_unwrap(AppState::for_test(None).await?.into_inner())
            .map_err(|_| anyhow::anyhow!("a fresh test state has one owner"))?;
        Ok(web::Data::new(AppState { db: pool, ..state }))
    }

    // The tests below share one process-wide metrics registry and run in
    // parallel, so each owns the kind and outcome it asserts on.

    #[test]
    fn each_kind_has_a_fixed_snake_case_label() {
        let labels: Vec<&str> = RUN_KINDS.into_iter().map(run_kind_label).collect();
        assert_eq!(
            labels,
            [
                "onboarding",
                "kick",
                "contracts",
                "dars",
                "add_party",
                "change_threshold",
                "rotate_key"
            ]
        );
    }

    #[test]
    fn every_kind_and_outcome_is_exported_before_the_first_run() {
        register_run_metrics();
        let families = prometheus::gather();
        let series = |name: &str| -> HashSet<Vec<String>> {
            families
                .iter()
                .filter(|f| f.name() == name)
                .flat_map(|f| f.get_metric())
                .map(|m| {
                    m.get_label()
                        .iter()
                        .map(|l| l.value().to_string())
                        .collect()
                })
                .collect()
        };
        let started_series = series("decman_workflow_runs_started_total");
        let finished_series = series("decman_workflow_runs_finished_total");

        for kind in RUN_KINDS.map(run_kind_label) {
            assert!(
                started_series.contains(&vec![kind.to_string()]),
                "no started series for {kind}"
            );
            for outcome in RunOutcome::ALL.map(RunOutcome::label) {
                assert!(
                    finished_series.contains(&vec![kind.to_string(), outcome.to_string()]),
                    "no finished series for {kind} {outcome}"
                );
            }
        }
    }

    /// A duplicate instance is refused with 409, so only the first registration
    /// starts a run.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_registered_run_counts_as_started_once(pool: SqlitePool) -> Result {
        let data = state_on(pool).await?;
        let instance = WorkflowInstance::new(
            "onboarding-1".to_string(),
            WorkflowKind::Onboarding,
            WorkflowRole::Coordinator,
        );
        let before = started(WorkflowKind::Onboarding);

        let config = serde_json::json!({});
        let register = || {
            register_and_persist(
                &data,
                &instance,
                OnboardingStep::WaitingForPeers,
                &config,
                &[],
                None,
            )
        };
        let first = register().await;
        let again = register().await;

        assert!(first.is_ok());
        assert!(again.is_err(), "a duplicate instance must be refused");
        assert_eq!(started(WorkflowKind::Onboarding), before + 1);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_completed_run_counts_once(pool: SqlitePool) -> Result {
        insert_run(&pool, &in_progress_run("kick-1", &pid(12)?)).await?;
        let before = finished(WorkflowKind::Kick, RunOutcome::Completed);

        mark_run_completed(&pool, WorkflowKind::Kick, "kick-1").await;

        assert_eq!(
            finished(WorkflowKind::Kick, RunOutcome::Completed),
            before + 1
        );
        Ok(())
    }

    /// A failed write leaves the row in progress, so the outcome is not counted
    /// yet. A database with no tables makes the write fail.
    #[tokio::test]
    async fn a_failed_status_write_counts_nothing() -> Result {
        let pool = SqlitePool::connect("sqlite::memory:").await?;
        let before = finished(WorkflowKind::Dars, RunOutcome::Failed);

        mark_run_failed(&pool, WorkflowKind::Dars, "dars-1", "boom").await;

        assert_eq!(finished(WorkflowKind::Dars, RunOutcome::Failed), before);
        Ok(())
    }

    async fn status_of(pool: &SqlitePool, instance: &str) -> Result<Option<WorkflowProgress>> {
        Ok(pool.get_workflow_run(instance).await?.map(|run| run.status))
    }

    /// A completion that lands after a cancel loses: the row stays Cancelled
    /// and the run counts as cancelled only.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_cancel_that_lands_first_wins(pool: SqlitePool) -> Result {
        let kind = WorkflowKind::Onboarding;
        insert_run(&pool, &in_progress_run("onboarding-2", &pid(15)?)).await?;
        let cancelled = finished(kind, RunOutcome::Cancelled);
        let completed = finished(kind, RunOutcome::Completed);
        let failed = finished(kind, RunOutcome::Failed);

        assert!(mark_run_cancelled(&pool, kind, "onboarding-2").await?);
        mark_run_completed(&pool, kind, "onboarding-2").await;
        mark_run_failed(&pool, kind, "onboarding-2", "late").await;

        assert_eq!(
            status_of(&pool, "onboarding-2").await?,
            Some(WorkflowProgress::Cancelled)
        );
        assert_eq!(finished(kind, RunOutcome::Cancelled), cancelled + 1);
        assert_eq!(finished(kind, RunOutcome::Completed), completed);
        assert_eq!(finished(kind, RunOutcome::Failed), failed);
        Ok(())
    }

    /// A cancel that lands after the run completed loses: the row stays
    /// Completed and the run counts as completed only.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_completion_that_lands_first_wins(pool: SqlitePool) -> Result {
        let kind = WorkflowKind::Dars;
        insert_run(&pool, &in_progress_run("dars-2", &pid(16)?)).await?;
        let completed = finished(kind, RunOutcome::Completed);
        let cancelled = finished(kind, RunOutcome::Cancelled);

        mark_run_completed(&pool, kind, "dars-2").await;
        let cancel_won = mark_run_cancelled(&pool, kind, "dars-2").await?;

        assert!(!cancel_won, "a cancel after completion must not win");
        assert_eq!(
            status_of(&pool, "dars-2").await?,
            Some(WorkflowProgress::Completed)
        );
        assert_eq!(finished(kind, RunOutcome::Completed), completed + 1);
        assert_eq!(finished(kind, RunOutcome::Cancelled), cancelled);
        Ok(())
    }

    /// A failed cancel write leaves the run live, so only a written cancel
    /// counts.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_cancel_counts_only_once_it_is_written(pool: SqlitePool) -> Result {
        let kind = WorkflowKind::ChangeThreshold;
        insert_run(&pool, &in_progress_run("change-threshold-2", &pid(13)?)).await?;
        let before = finished(kind, RunOutcome::Cancelled);
        assert!(mark_run_cancelled(&pool, kind, "change-threshold-2").await?);
        assert_eq!(finished(kind, RunOutcome::Cancelled), before + 1);

        let no_tables = SqlitePool::connect("sqlite::memory:").await?;
        let before = finished(WorkflowKind::Kick, RunOutcome::Cancelled);
        assert!(
            mark_run_cancelled(&no_tables, WorkflowKind::Kick, "kick-2")
                .await
                .is_err()
        );
        assert_eq!(finished(WorkflowKind::Kick, RunOutcome::Cancelled), before);
        Ok(())
    }

    /// A retry puts a Failed run back in progress, so it counts as a new start.
    /// This run's config does not parse, so its resume fails at once, through
    /// the same counted helper as every other coordinator run.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_retry_counts_as_a_new_start(pool: SqlitePool) -> Result {
        let kind = WorkflowKind::ChangeThreshold;
        let run = WorkflowRun {
            kind,
            status: WorkflowProgress::Failed,
            config_json: "not json".to_string(),
            ..in_progress_run("change-threshold-1", &pid(14)?)
        };
        insert_run(&pool, &run).await?;
        let app = actix_test::init_service(
            App::new()
                .app_data(state_on(pool).await?)
                .service(retry_workflow),
        )
        .await;
        let started_before = started(kind);
        let failed_before = finished(kind, RunOutcome::Failed);

        let request = actix_test::TestRequest::post()
            .uri("/workflows/change-threshold-1/retry")
            .to_request();
        request.extensions_mut().insert(admin());
        let response = actix_test::call_service(&app, request).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(started(kind), started_before + 1);
        assert_eq!(finished(kind, RunOutcome::Failed), failed_before + 1);
        Ok(())
    }

    fn admin() -> Principal {
        Principal {
            sub: "admin".to_string(),
            issuer: "test".to_string(),
            roles: Vec::new(),
            email: None,
        }
    }

    /// POSTs the per-instance cancel for a live coordinator run. Its in-memory
    /// status reads InProgress and its task never ends on its own, like a
    /// declined run idling in WaitingForPeers. Returns the status and the task.
    async fn cancel_live_run(
        pool: SqlitePool,
        instance: &str,
        kind: WorkflowKind,
    ) -> Result<(StatusCode, tokio::task::JoinHandle<()>)> {
        let data = state_on(pool).await?;
        let run = WorkflowInstance::new(instance.to_string(), kind, WorkflowRole::Coordinator);
        *run.http.status.write().await = WorkflowProgress::InProgress;
        let task = tokio::spawn(std::future::pending::<()>());
        *run.http.abort_handle.lock().await = Some(task.abort_handle());
        anyhow::ensure!(
            data.workflows.insert(run),
            "{instance} is already registered"
        );
        let app =
            actix_test::init_service(App::new().app_data(data).service(cancel_workflow_instance))
                .await;
        let request = actix_test::TestRequest::post()
            .uri(&format!("/workflows/{instance}/cancel"))
            .to_request();
        request.extensions_mut().insert(admin());
        Ok((actix_test::call_service(&app, request).await.status(), task))
    }

    /// A run whose own outcome landed first keeps it. The cancel still stops
    /// the run's task and answers like a normal cancel.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn cancelling_a_finished_run_stops_its_task_and_keeps_its_outcome(
        pool: SqlitePool,
    ) -> Result {
        let kind = WorkflowKind::Contracts;
        let run = WorkflowRun {
            status: WorkflowProgress::Completed,
            ..in_progress_run("contracts-3", &pid(17)?)
        };
        insert_run(&pool, &run).await?;
        let before = finished(kind, RunOutcome::Cancelled);

        let (status, task) = cancel_live_run(pool.clone(), "contracts-3", kind).await?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            status_of(&pool, "contracts-3").await?,
            Some(WorkflowProgress::Completed)
        );
        assert_eq!(finished(kind, RunOutcome::Cancelled), before);
        assert!(task.await.is_err_and(|e| e.is_cancelled()));
        Ok(())
    }

    /// A peer's decline records Failed while the run's task idles. The
    /// operator's cancel then reaps the task: it answers 200, keeps the row
    /// Failed and counts nothing more.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_cancel_after_a_decline_reaps_the_task_and_keeps_failed(pool: SqlitePool) -> Result {
        let kind = WorkflowKind::AddParty;
        let run = WorkflowRun {
            kind,
            ..in_progress_run("add-party-2", &pid(18)?)
        };
        insert_run(&pool, &run).await?;
        let failed = finished(kind, RunOutcome::Failed);
        let cancelled = finished(kind, RunOutcome::Cancelled);

        let state = workflow::state::WorkflowState::new(
            pool.clone(),
            "add-party-2".to_string(),
            AddPartyStep::WaitingForPeers,
            Vec::new(),
            None,
        );
        state.mark_failed("Peer p3 declined the invitation").await;
        assert_eq!(
            status_of(&pool, "add-party-2").await?,
            Some(WorkflowProgress::Failed)
        );
        assert_eq!(finished(kind, RunOutcome::Failed), failed + 1);

        let (status, task) = cancel_live_run(pool.clone(), "add-party-2", kind).await?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            status_of(&pool, "add-party-2").await?,
            Some(WorkflowProgress::Failed)
        );
        assert_eq!(finished(kind, RunOutcome::Failed), failed + 1);
        assert_eq!(finished(kind, RunOutcome::Cancelled), cancelled);
        assert!(task.await.is_err_and(|e| e.is_cancelled()));
        Ok(())
    }

    fn dar(filename: &str, len: usize) -> DarFile {
        DarFile {
            filename: filename.to_string(),
            data: STANDARD.encode(vec![0u8; len]),
        }
    }

    /// Each file adds its name and 8 bytes of framing, and the set adds 4.
    /// Two DARs that fit alone but not together can go in two runs.
    #[test]
    fn a_set_above_the_limit_states_its_size_and_excess() -> Result {
        let refusal = dars_bundle_refusal(&[dar("a.dar", 60), dar("b.dar", 60)], 100);
        let Err((status, error)) = refusal else {
            anyhow::bail!("a 150-byte bundle must be refused at limit 100");
        };
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert!(error.contains("their names take 150 bytes"), "{error}");
        assert!(
            error.contains("50 bytes over the limit of 100 bytes"),
            "{error}"
        );
        assert!(error.contains("several runs"), "{error}");
        Ok(())
    }

    /// No split helps a DAR that is over the limit alone, so the refusal
    /// names it and does not suggest several runs.
    #[test]
    fn a_dar_over_the_limit_alone_is_named_without_advice_to_split() -> Result {
        let refusal = dars_bundle_refusal(&[dar("a.dar", 10), dar("big.dar", 120)], 100);
        let Err((status, error)) = refusal else {
            anyhow::bail!("a DAR over the limit alone must be refused");
        };
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert!(
            error.starts_with("big.dar is too large to distribute"),
            "{error}"
        );
        assert!(error.contains("its name take 139 bytes"), "{error}");
        assert!(
            error.contains("39 bytes over the limit of 100 bytes"),
            "{error}"
        );
        assert!(!error.contains("several runs"), "{error}");
        Ok(())
    }

    #[test]
    fn every_dar_over_the_limit_alone_is_named() {
        let refusal = bundle_size_refusal(&[("x.dar", 120), ("a.dar", 1), ("y.dar", 130)], 100)
            .unwrap_or_default();
        assert!(refusal.contains(": x.dar, y.dar."), "{refusal}");
        assert!(!refusal.contains("several runs"), "{refusal}");
    }

    /// A size too large to add up is refused, not wrapped to a small one.
    #[test]
    fn a_size_that_overflows_is_refused() {
        assert!(bundle_size_refusal(&[("a.dar", usize::MAX)], 100).is_some());
        assert!(bundle_size_refusal(&[("a.dar", usize::MAX - 20), ("b.dar", 10)], 100).is_some());
    }

    #[test]
    fn framing_bytes_count_toward_the_limit() {
        let refusal = dars_bundle_refusal(&[dar("a.dar", 60), dar("b.dar", 40)], 100);
        assert!(
            matches!(refusal, Err((StatusCode::PAYLOAD_TOO_LARGE, _))),
            "100 DAR bytes plus framing must exceed limit 100: {refusal:?}"
        );
    }

    #[test]
    fn a_bundle_at_the_limit_passes() {
        assert_eq!(dars_bundle_refusal(&[dar("a.dar", 83)], 100), Ok(()));
    }

    #[test]
    fn an_undecodable_dar_is_a_bad_request() -> Result {
        let files = [
            dar("good.dar", 10),
            DarFile {
                filename: "broken.dar".to_string(),
                data: "not base64!".to_string(),
            },
        ];
        let Err((status, error)) = dars_bundle_refusal(&files, 100) else {
            anyhow::bail!("a DAR that is not base64 must be refused");
        };
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error.contains("broken.dar"), "{error}");
        Ok(())
    }

    /// One DAR the size of the limit fails only through its framing. The run
    /// is refused before it is registered, so no peer is invited.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_oversized_distribute_sends_no_invitation(pool: SqlitePool) -> Result {
        let data = state_on(pool.clone()).await?;
        let app = actix_test::init_service(
            App::new()
                .app_data(web::JsonConfig::default().limit(100 * 1024 * 1024))
                .app_data(data.clone())
                .service(start_dars),
        )
        .await;
        let request = actix_test::TestRequest::post()
            .uri("/dars/distribute")
            .set_json(serde_json::json!({
                "dar_files": [dar("big.dar", MAX_CHUNKED_TOTAL_SIZE)],
                "peer_ids": [pid(2)?],
            }))
            .to_request();
        request.extensions_mut().insert(admin());

        let response = actix_test::call_service(&app, request).await;

        let status = response.status();
        let body = actix_test::read_body(response).await;
        let body = String::from_utf8_lossy(&body);
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
        assert!(
            body.contains("big.dar is too large to distribute"),
            "{body}"
        );
        assert!(
            body.contains(&format!(
                "19 bytes over the limit of {MAX_CHUNKED_TOTAL_SIZE} bytes"
            )),
            "{body}"
        );
        assert!(
            pool.get_workflow_runs_by_kind(WorkflowKind::Dars)
                .await?
                .is_empty()
        );
        assert!(data.workflows.snapshot().is_empty());
        Ok(())
    }

    /// Three DARs whose `UploadDars` bundle is `bundle_len` bytes.
    fn three_dars_of_bundle(bundle_len: usize) -> Vec<DarFile> {
        // A 4-byte count, then two 4-byte lengths and a 5-byte name per file.
        let data_len = bundle_len - 4 - 3 * (8 + 5);
        vec![
            dar("a.dar", data_len / 3),
            dar("b.dar", data_len / 3),
            dar("c.dar", data_len - 2 * (data_len / 3)),
        ]
    }

    /// The bundle the peer receives, built the way the coordinator builds it.
    fn bundle_of(files: &[DarFile]) -> anyhow::Result<Vec<u8>> {
        let decoded = files
            .iter()
            .map(|file| Ok((file.filename.clone(), STANDARD.decode(&file.data)?)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(utils::encode_files(&decoded))
    }

    /// The coordinator must refuse exactly the sets a peer would refuse. A
    /// stricter check blocks runs that would work. A looser one lets the peer
    /// fail after it accepts, which is the fault this check prevents.
    #[test]
    fn the_coordinator_refuses_exactly_what_a_peer_refuses() -> anyhow::Result<()> {
        use crate::noise::{CHUNK_SIZE, check_chunked_metadata};

        for bundle_len in [MAX_CHUNKED_TOTAL_SIZE, MAX_CHUNKED_TOTAL_SIZE + 1] {
            let files = three_dars_of_bundle(bundle_len);
            let bundle = bundle_of(&files)?;
            assert_eq!(bundle.len(), bundle_len);
            let peer = check_chunked_metadata(bundle.len(), bundle.len().div_ceil(CHUNK_SIZE));
            let coordinator = dars_bundle_refusal(&files, MAX_CHUNKED_TOTAL_SIZE);
            assert_eq!(
                coordinator.is_ok(),
                peer.is_ok(),
                "bundle of {bundle_len} bytes: coordinator {coordinator:?}, peer {peer:?}"
            );
        }
        Ok(())
    }

    /// Issue #616: many DARs, each well below the limit, that exceed it
    /// together. The refusal states the exact size of the bundle a peer
    /// would have received.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn many_small_dars_above_the_limit_together_are_refused(pool: SqlitePool) -> Result {
        let files: Vec<DarFile> = (0..9)
            .map(|n| dar(&format!("package-{n}.dar"), 2 * 1024 * 1024))
            .collect();
        let size = bundle_of(&files)?.len();
        let data = state_on(pool.clone()).await?;
        let app = actix_test::init_service(
            App::new()
                .app_data(web::JsonConfig::default().limit(100 * 1024 * 1024))
                .app_data(data.clone())
                .service(start_dars),
        )
        .await;
        let request = actix_test::TestRequest::post()
            .uri("/dars/distribute")
            .set_json(serde_json::json!({
                "dar_files": files,
                "peer_ids": [pid(2)?],
            }))
            .to_request();
        request.extensions_mut().insert(admin());

        let response = actix_test::call_service(&app, request).await;

        let status = response.status();
        let body = actix_test::read_body(response).await;
        let body = String::from_utf8_lossy(&body);
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
        assert!(
            body.contains(&format!("their names take {size} bytes")),
            "{body}"
        );
        assert!(
            body.contains(&format!(
                "{over} bytes over the limit of {MAX_CHUNKED_TOTAL_SIZE} bytes",
                over = size - MAX_CHUNKED_TOTAL_SIZE
            )),
            "{body}"
        );
        assert!(body.contains("several runs"), "{body}");
        assert!(
            pool.get_workflow_runs_by_kind(WorkflowKind::Dars)
                .await?
                .is_empty()
        );
        assert!(data.workflows.snapshot().is_empty());
        Ok(())
    }
}
