mod action_serializer;
mod assets;
mod audit;
mod chain_audit;
mod handlers;
mod middleware;
mod queries;
mod transfer_context;
mod types;

pub mod health;
pub mod peer_status;

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

use actix_cors::Cors;
use actix_web::{App, HttpServer, web};
use canton_proto_rs::com::digitalasset::canton::{
    admin::participant::v30::{ListPackagesRequest, package_service_client::PackageServiceClient},
    crypto::{
        admin::v30::{ListMyKeysRequest, vault_service_client::VaultServiceClient},
        v30::public_key,
    },
    topology::admin::v30::{
        BaseQuery, ListDecentralizedNamespaceDefinitionRequest, StoreId, Synchronizer, base_query,
        store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use hyper::{Body, Response, StatusCode};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, Notify, RwLock};
use tokio_noise::handshakes::nn_psk2::Responder;
use utoipa_actix_web::AppExt;
use utoipa_swagger_ui::SwaggerUi;

#[cfg(not(any(test, feature = "test-mode")))]
use crate::auth::{AuthRegistry, JwtValidator};
#[cfg(any(test, feature = "test-mode"))]
use crate::auth::{MockAuthRegistry, MockValidator};
use crate::{
    auth::{TokenValidator, WorkflowAuth},
    config::{NodeConfig, PartyCredentials},
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
    noise::{
        CHUNK_SIZE, MAX_CHUNKED_TOTAL_SIZE, MAX_PAYLOAD_SIZE, Message, MessageType, NoiseKeypair,
        load_or_generate_keypair, parse_public_key,
    },
    participant_id::CantonId,
    server::middleware::AuthMiddleware,
    server::peer_status::LastSeen,
    utils::{self, compute_fingerprint},
    workflow::{self, WorkflowType},
};

pub use types::*;

/// TTL for cached chunked ListPackages payloads (per peer).
const LIST_PACKAGES_CHUNK_CACHE_TTL: Duration = Duration::from_secs(30);

/// Per-peer entry in the ListPackages chunk cache: `(raw JSON bytes, last-access time)`.
/// The `Instant` is updated on every successful chunk read (see `MessageType::GetChunk`
/// handler), so a slow peer mid-reassembly extends its own TTL window.
type ChunkCacheEntry = (Vec<u8>, Instant);

/// Shared cache of large ListPackages payloads awaiting chunk retrieval by peers.
type ListPackagesChunkCache = Arc<Mutex<HashMap<String, ChunkCacheEntry>>>;

/// Application state shared across all handlers
pub struct AppState {
    pub db: SqlitePool,
    pub config: NodeConfig,
    pub peer_status: Arc<RwLock<HashMap<String, bool>>>,
    pub last_seen: LastSeen,
    pub onboarding_trigger: Arc<Notify>,
    pub kick_trigger: Arc<Notify>,
    pub contracts_trigger: Arc<Notify>,
    pub dars_trigger: Arc<Notify>,
    /// Coordinator's public key (set when invite is received)
    pub coordinator_pubkey: Arc<RwLock<Option<String>>>,
    /// `instance_name` of the peer-side `workflow_runs` row that the
    /// trigger listener should mark Completed/Failed when the workflow ends.
    /// Populated by `accept_invitation` right before it fires the trigger.
    pub peer_run_instance: Arc<RwLock<Option<String>>>,
    /// Pending invitations awaiting user acceptance
    pub pending_invitations: Arc<RwLock<Vec<PendingInvitation>>>,
    /// Authentication registry (real Keycloak or mock for test mode)
    pub auth: Arc<RwLock<Option<WorkflowAuth>>>,
    /// Inbound token validator — authenticates API callers.
    pub token_validator: TokenValidator,
    /// Role name that grants admin access to sensitive endpoints.
    pub admin_role: Option<String>,
    /// Party credentials (mutable, hot-reloadable)
    pub party_credentials: Arc<RwLock<Vec<PartyCredentials>>>,
    /// Serializes unauthenticated `PUT /party-config` bootstrap calls so two
    /// concurrent first-run requests cannot both pass the empty-table auth
    /// exemption and overwrite each other. Held by the auth middleware for
    /// the lifetime of a bootstrap request.
    pub bootstrap_mu: Arc<Mutex<()>>,
    /// Cross-workflow mutex: set while any of kick / onboarding / contracts
    /// / dars is in flight. `start_*` handlers `try_acquire` the
    /// `WorkflowInFlightGuard` before spawning; the guard rides along inside
    /// the spawned task and drops when the task ends.
    pub workflow_in_flight: Arc<AtomicBool>,
    /// The coordinator's in-flight workflow, registered here so the always-on
    /// Noise listener can route its workflow-command messages. `None` when idle.
    pub active_workflow: ActiveWorkflowSlot,
    /// Whether the server is running in test mode
    pub test_mode: bool,
    /// Prefixes currently being refreshed from Canton (deduplication)
    pub refreshing_prefixes: Arc<RwLock<HashSet<String>>>,
    /// Shared `reqwest::Client` for the proxy-style handlers (`/network-info`,
    /// `/operator-info`, `/token-standard-contracts`). Constructed once at
    /// startup so its connection pool / keep-alives are reused across
    /// requests instead of paying TCP+TLS setup on every call.
    pub http_client: reqwest::Client,
}

// The previous `ListenerControl` struct collapsed to a single `Arc<AtomicBool>`
// (see `noise_listener_pause_flag` below). Atomic so `ListenerPauseGuard::Drop`
// can reset it synchronously when a spawned workflow task is aborted or panics.

/// Workflow triggers shared across Noise server handlers
#[derive(Clone)]
struct WorkflowTriggers {
    pending_invitations: Arc<RwLock<Vec<PendingInvitation>>>,
    /// Full node config — read by handler arms that need the participant
    /// identity, admin URL, or synchronizer alias as a bundle (e.g.
    /// `list_my_owner_keys`'s P2P filter, see #149).
    config: NodeConfig,
    admin_api_url: String,
    /// Cache for chunked ListPackages responses, keyed by the requesting
    /// peer's pubkey (hex). Populated when a ListPackages response exceeds
    /// `MAX_PAYLOAD_SIZE`; consumed by subsequent GetChunk requests from the
    /// same peer. TTL: 30 seconds. One entry per peer; replaced on new
    /// ListPackages call from the same peer.
    list_packages_chunk_cache: ListPackagesChunkCache,
    db: SqlitePool,
    /// Read by the `RequestMemberParty` listener arm.
    party_credentials: Arc<RwLock<Vec<PartyCredentials>>>,
    /// Per-kind triggers + AppState slots, used by the `RetryWorkflow`
    /// listener arm to flip a Failed peer row back to InProgress and
    /// re-spin its workflow loop.
    onboarding_trigger: Arc<Notify>,
    kick_trigger: Arc<Notify>,
    contracts_trigger: Arc<Notify>,
    dars_trigger: Arc<Notify>,
    coordinator_pubkey: Arc<RwLock<Option<String>>>,
    peer_run_instance: Arc<RwLock<Option<String>>>,
    /// The node's in-flight workflow, used to route workflow-command messages.
    active_workflow: ActiveWorkflowSlot,
}

enum InvitationMeta {
    None,
    Onboarding(OnboardingInvitePayload),
    Dars(DarsInvitePayload),
    Kick(KickInvitePayload),
}

/// On boot, re-spawn any InProgress workflow runs that were interrupted by the
/// last shutdown. The previous task handle died with the process, but the
/// state machine + artefacts survived in SQLite, so we can pick the run back
/// up at its persisted `current_step`.
///
/// Coordinator-side (this node started the workflow): we deserialize the
/// `config_json` we stored at start and call `workflow::start_coordinator`
/// again. `NoiseServer::new` detects the existing `workflow_runs` row and
/// re-hydrates `WorkflowState` via `from_persisted`, so the coordinator's
/// step-driven loop resumes at `current_step`. The HTTP `<kind>WorkflowState`
/// is set to InProgress and the new abort handle is stashed so
/// `/{kind}/cancel` can stop the resumed task.
///
/// Peer-side (we accepted an invite): we re-fire the per-kind trigger so
/// the existing listener calls `start_peer`, which establishes a fresh
/// Noise client back to the persisted `coordinator_pubkey`. Limitation: the
/// peer pulls its instance_name out of the GenerateKeys / SignSubmissions
/// / SignKick command payload — if the coordinator is past those steps, the
/// peer cannot rebind its instance_name and will fail. Those runs surface
/// as Failed in the feed, with the operator left to dismiss. Lifting this
/// limitation requires coordinator-side protocol changes (sending the config
/// alongside every command) and is tracked separately.
#[allow(clippy::too_many_arguments)]
async fn recover_in_progress_workflows(
    db: SqlitePool,
    config: NodeConfig,
    kick_state: web::Data<Arc<handlers::KickWorkflowState>>,
    onboarding_state: web::Data<Arc<handlers::OnboardingWorkflowState>>,
    contracts_state: web::Data<Arc<handlers::ContractsWorkflowState>>,
    dars_state: web::Data<Arc<handlers::DarsWorkflowState>>,
    active_workflow: ActiveWorkflowSlot,
    auth: Arc<RwLock<Option<WorkflowAuth>>>,
    last_seen: LastSeen,
    onboarding_trigger: Arc<Notify>,
    kick_trigger: Arc<Notify>,
    contracts_trigger: Arc<Notify>,
    dars_trigger: Arc<Notify>,
    coordinator_pubkey: Arc<RwLock<Option<String>>>,
    peer_run_instance: Arc<RwLock<Option<String>>>,
) {
    let runs = match SchemaRead::get_in_progress_workflow_runs(&db).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("recover_in_progress_workflows: load failed: {e}");
            return;
        }
    };
    if runs.is_empty() {
        return;
    }
    tracing::info!(
        "Recovering {} in-progress workflow run(s) interrupted by last shutdown",
        runs.len()
    );

    for run in runs {
        match run.role {
            WorkflowRole::Coordinator => {
                respawn_coordinator(
                    db.clone(),
                    config.clone(),
                    &run,
                    kick_state.clone(),
                    onboarding_state.clone(),
                    contracts_state.clone(),
                    dars_state.clone(),
                    active_workflow.clone(),
                    auth.clone(),
                    last_seen.clone(),
                )
                .await;
            }
            WorkflowRole::Peer => {
                refire_peer(
                    &run,
                    &onboarding_trigger,
                    &kick_trigger,
                    &contracts_trigger,
                    &dars_trigger,
                    &coordinator_pubkey,
                    &peer_run_instance,
                )
                .await;
            }
        }
    }
}

/// Re-spawn a coordinator-side workflow that was running when the node
/// stopped. The original `workflow_runs` row stays in place; the spawned task
/// uses `WorkflowState::from_persisted` (via `NoiseServer::new`) to resume at
/// `current_step` instead of restarting from `WaitingForPeers`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn respawn_coordinator(
    db: SqlitePool,
    config: NodeConfig,
    run: &WorkflowRun,
    kick_state: web::Data<Arc<handlers::KickWorkflowState>>,
    onboarding_state: web::Data<Arc<handlers::OnboardingWorkflowState>>,
    contracts_state: web::Data<Arc<handlers::ContractsWorkflowState>>,
    dars_state: web::Data<Arc<handlers::DarsWorkflowState>>,
    active_workflow: ActiveWorkflowSlot,
    auth: Arc<RwLock<Option<WorkflowAuth>>>,
    last_seen: LastSeen,
) {
    let instance = run.instance_name.clone();
    let kind = run.kind;
    let current_step = run.current_step.clone();
    tracing::info!(
        "Resuming {kind:?} coordinator run {instance} at step {current_step} \
         ({completed} of {total} peers completed)",
        completed = run.completed_peers.len(),
        total = run.expected_peers.len()
    );

    match kind {
        WorkflowKind::Onboarding => {
            let onboarding_config: workflow::OnboardingConfig =
                match serde_json::from_str(&run.config_json) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(
                            "respawn_coordinator: bad onboarding config_json for {instance}: {e}"
                        );
                        mark_failed_via_pool(&db, &instance, "Resume failed: invalid config").await;
                        return;
                    }
                };
            *onboarding_state.invited_peers.write().await = run.expected_peers.clone();
            let state_ref = onboarding_state.get_ref().clone();
            // Hold abort_handle, status, and error locks across the spawn so a
            // concurrent /onboarding/cancel can't observe "status=InProgress
            // + abort_handle=None" — see start_dars in handlers/workflows.rs.
            let mut abort_guard = onboarding_state.abort_handle.lock().await;
            let mut status_guard = onboarding_state.status.write().await;
            let mut error_guard = onboarding_state.error.write().await;
            let join_handle = tokio::spawn(spawn_onboarding_resume(
                config,
                db.clone(),
                onboarding_config,
                instance.clone(),
                state_ref,
                active_workflow,
                last_seen,
            ));
            *abort_guard = Some(join_handle.abort_handle());
            *status_guard = OnboardingStatus::InProgress;
            *error_guard = None;
        }
        WorkflowKind::Kick => {
            let kick_config: workflow::KickConfig = match serde_json::from_str(&run.config_json) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("respawn_coordinator: bad kick config_json for {instance}: {e}");
                    mark_failed_via_pool(&db, &instance, "Resume failed: invalid config").await;
                    return;
                }
            };
            *kick_state.invited_peers.write().await = run.expected_peers.clone();
            let state_ref = kick_state.get_ref().clone();
            let mut abort_guard = kick_state.abort_handle.lock().await;
            let mut status_guard = kick_state.status.write().await;
            let mut error_guard = kick_state.error.write().await;
            let join_handle = tokio::spawn(spawn_kick_resume(
                config,
                db.clone(),
                kick_config,
                instance.clone(),
                state_ref,
                active_workflow,
                last_seen,
            ));
            *abort_guard = Some(join_handle.abort_handle());
            *status_guard = KickStatus::InProgress;
            *error_guard = None;
        }
        WorkflowKind::Contracts => {
            let contracts_config: workflow::ContractsConfig =
                match serde_json::from_str(&run.config_json) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(
                            "respawn_coordinator: bad contracts config_json for {instance}: {e}"
                        );
                        mark_failed_via_pool(&db, &instance, "Resume failed: invalid config").await;
                        return;
                    }
                };
            *contracts_state.invited_peers.write().await = run.expected_peers.clone();
            let state_ref = contracts_state.get_ref().clone();
            let auth_snapshot = auth.read().await.clone();
            let mut abort_guard = contracts_state.abort_handle.lock().await;
            let mut status_guard = contracts_state.status.write().await;
            let mut error_guard = contracts_state.error.write().await;
            let join_handle = tokio::spawn(spawn_contracts_resume(
                config,
                db.clone(),
                contracts_config,
                instance.clone(),
                state_ref,
                active_workflow,
                auth_snapshot,
                last_seen,
            ));
            *abort_guard = Some(join_handle.abort_handle());
            *status_guard = WorkflowProgress::InProgress;
            *error_guard = None;
        }
        WorkflowKind::Dars => {
            let dars_config: workflow::DarsConfig = match serde_json::from_str(&run.config_json) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("respawn_coordinator: bad dars config_json for {instance}: {e}");
                    mark_failed_via_pool(&db, &instance, "Resume failed: invalid config").await;
                    return;
                }
            };
            *dars_state.invited_peers.write().await = run.expected_peers.clone();
            let state_ref = dars_state.get_ref().clone();
            let mut abort_guard = dars_state.abort_handle.lock().await;
            let mut status_guard = dars_state.status.write().await;
            let mut error_guard = dars_state.error.write().await;
            let join_handle = tokio::spawn(spawn_dars_resume(
                config,
                db.clone(),
                dars_config,
                instance.clone(),
                state_ref,
                active_workflow,
                last_seen,
            ));
            *abort_guard = Some(join_handle.abort_handle());
            *status_guard = WorkflowProgress::InProgress;
            *error_guard = None;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn spawn_onboarding_resume(
    config: NodeConfig,
    db: SqlitePool,
    onboarding_config: workflow::OnboardingConfig,
    instance: String,
    state: Arc<HttpWorkflowState<OnboardingStatus>>,
    active_workflow: ActiveWorkflowSlot,
    last_seen: LastSeen,
) {
    let result = workflow::start_coordinator(
        config,
        db.clone(),
        WorkflowType::Onboarding,
        Some(onboarding_config),
        None,
        None,
        None,
        None,
        last_seen,
        active_workflow,
    )
    .await;

    // Update in-memory state in tight scopes — never hold the RwLock across
    // a DB await. /onboarding/status acquires a read lock to serve every
    // poll; if a writer holds the lock during the DB write, every concurrent
    // read blocks for that duration on a slow runner.
    match result {
        Ok(_) => {
            {
                let mut status = state.status.write().await;
                *status = OnboardingStatus::Completed;
            }
            tracing::info!("Resumed onboarding workflow {instance} completed");
            mark_completed_via_pool(&db, &instance).await;
        }
        Err(e) => {
            let msg = format!("{e}");
            {
                let mut status = state.status.write().await;
                let mut error = state.error.write().await;
                *status = OnboardingStatus::Failed;
                *error = Some(msg.clone());
            }
            tracing::error!("Resumed onboarding workflow {instance} failed: {e:#}");
            mark_failed_via_pool(&db, &instance, &msg).await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn spawn_kick_resume(
    config: NodeConfig,
    db: SqlitePool,
    kick_config: workflow::KickConfig,
    instance: String,
    state: Arc<HttpWorkflowState<KickStatus>>,
    active_workflow: ActiveWorkflowSlot,
    last_seen: LastSeen,
) {
    let result = workflow::start_coordinator(
        config,
        db.clone(),
        WorkflowType::Kick,
        None,
        Some(kick_config),
        None,
        None,
        None,
        last_seen,
        active_workflow,
    )
    .await;

    match result {
        Ok(_) => {
            {
                let mut status = state.status.write().await;
                *status = KickStatus::Completed;
            }
            tracing::info!("Resumed kick workflow {instance} completed");
            mark_completed_via_pool(&db, &instance).await;
        }
        Err(e) => {
            let msg = format!("{e}");
            {
                let mut status = state.status.write().await;
                let mut error = state.error.write().await;
                *status = KickStatus::Failed;
                *error = Some(msg.clone());
            }
            tracing::error!("Resumed kick workflow {instance} failed: {e:#}");
            mark_failed_via_pool(&db, &instance, &msg).await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn spawn_contracts_resume(
    config: NodeConfig,
    db: SqlitePool,
    contracts_config: workflow::ContractsConfig,
    instance: String,
    state: Arc<HttpWorkflowState<WorkflowProgress>>,
    active_workflow: ActiveWorkflowSlot,
    auth: Option<WorkflowAuth>,
    last_seen: LastSeen,
) {
    let result = workflow::start_coordinator(
        config,
        db.clone(),
        WorkflowType::Contracts,
        None,
        None,
        Some(contracts_config),
        None,
        auth,
        last_seen,
        active_workflow,
    )
    .await;

    match result {
        Ok(_) => {
            {
                let mut status = state.status.write().await;
                *status = WorkflowProgress::Completed;
            }
            tracing::info!("Resumed contracts workflow {instance} completed");
            mark_completed_via_pool(&db, &instance).await;
        }
        Err(e) => {
            let msg = format!("{e}");
            {
                let mut status = state.status.write().await;
                let mut error = state.error.write().await;
                *status = WorkflowProgress::Failed;
                *error = Some(msg.clone());
            }
            tracing::error!("Resumed contracts workflow {instance} failed: {e:#}");
            mark_failed_via_pool(&db, &instance, &msg).await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn spawn_dars_resume(
    config: NodeConfig,
    db: SqlitePool,
    dars_config: workflow::DarsConfig,
    instance: String,
    state: Arc<HttpWorkflowState<WorkflowProgress>>,
    active_workflow: ActiveWorkflowSlot,
    last_seen: LastSeen,
) {
    let result = workflow::start_coordinator(
        config,
        db.clone(),
        WorkflowType::Dars,
        None,
        None,
        None,
        Some(dars_config),
        None,
        last_seen,
        active_workflow,
    )
    .await;

    match result {
        Ok(_) => {
            {
                let mut status = state.status.write().await;
                *status = WorkflowProgress::Completed;
            }
            tracing::info!("Resumed dars workflow {instance} completed");
            mark_completed_via_pool(&db, &instance).await;
        }
        Err(e) => {
            let msg = format!("{e}");
            {
                let mut status = state.status.write().await;
                let mut error = state.error.write().await;
                *status = WorkflowProgress::Failed;
                *error = Some(msg.clone());
            }
            tracing::error!("Resumed dars workflow {instance} failed: {e:#}");
            mark_failed_via_pool(&db, &instance, &msg).await;
        }
    }
}

pub(crate) async fn refire_peer(
    run: &WorkflowRun,
    onboarding_trigger: &Arc<Notify>,
    kick_trigger: &Arc<Notify>,
    contracts_trigger: &Arc<Notify>,
    dars_trigger: &Arc<Notify>,
    coordinator_pubkey: &Arc<RwLock<Option<String>>>,
    peer_run_instance: &Arc<RwLock<Option<String>>>,
) {
    let Some(pk) = run.coordinator_pubkey.clone() else {
        tracing::warn!(
            "Skipping peer recover for {}: no coordinator_pubkey persisted",
            run.instance_name
        );
        return;
    };
    *coordinator_pubkey.write().await = Some(pk);
    *peer_run_instance.write().await = Some(run.instance_name.clone());
    let trigger = match run.kind {
        WorkflowKind::Onboarding => onboarding_trigger,
        WorkflowKind::Kick => kick_trigger,
        WorkflowKind::Contracts => contracts_trigger,
        WorkflowKind::Dars => dars_trigger,
    };
    trigger.notify_one();
    tracing::info!(
        "Re-fired {:?} peer trigger for resumed run {} (coordinator may be past the \
         config-bearing command — run will fail if so; remediation: dismiss and re-accept)",
        run.kind,
        run.instance_name
    );
}

async fn mark_completed_via_pool(db: &SqlitePool, instance_name: &str) {
    if let Err(e) = set_run_status(db, instance_name, WorkflowProgress::Completed, None).await {
        tracing::warn!("Failed to mark resumed run {instance_name} completed: {e:#}");
    }
}

async fn mark_failed_via_pool(db: &SqlitePool, instance_name: &str, error: &str) {
    if let Err(e) = set_run_status(
        db,
        instance_name,
        WorkflowProgress::Failed,
        Some(error.to_string()),
    )
    .await
    {
        tracing::warn!("Failed to mark resumed run {instance_name} failed: {e:#}");
    }
}

async fn set_run_status(
    db: &SqlitePool,
    instance_name: &str,
    status: WorkflowProgress,
    error: Option<String>,
) -> Result {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut tx = db.begin_transaction().await?;
    tx.set_workflow_run_status(instance_name, status, error.as_deref(), now)
        .await?;
    Commitable::commit(tx).await
}

impl WorkflowTriggers {
    async fn record_invitation(
        &self,
        invitation_type: InvitationType,
        coordinator_pubkey: &str,
        meta: InvitationMeta,
    ) {
        let mut prefix = None;
        let mut participants = Vec::new();
        let mut dar_filenames = Vec::new();
        let mut kicked_participant = None;
        let mut new_threshold = None;
        let mut previous_threshold = None;
        let mut dec_party_id = None;
        match meta {
            InvitationMeta::None => {}
            InvitationMeta::Onboarding(p) => {
                prefix = Some(p.prefix);
                participants = p.participants;
            }
            InvitationMeta::Dars(p) => {
                dar_filenames = p.dar_filenames;
            }
            InvitationMeta::Kick(p) => {
                kicked_participant = Some(p.kicked_participant);
                new_threshold = Some(p.new_threshold);
                previous_threshold = Some(p.previous_threshold);
                dec_party_id = Some(p.dec_party_id);
            }
        }
        let invitation = PendingInvitation {
            id: format!(
                "{}-{}",
                invitation_type.as_str().to_lowercase(),
                &coordinator_pubkey[..16]
            ),
            invitation_type,
            coordinator_pubkey: coordinator_pubkey.to_string(),
            coordinator_name: None,
            received_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            prefix,
            participants,
            dar_filenames,
            kicked_participant,
            new_threshold,
            previous_threshold,
            dec_party_id,
        };

        match self.db.begin_transaction().await {
            Ok(mut tx) => {
                if let Err(e) = tx.upsert_pending_invitation(&invitation).await {
                    tracing::warn!("Failed to persist pending invitation: {e}");
                } else if let Err(e) = Commitable::commit(tx).await {
                    tracing::warn!("Failed to commit pending invitation: {e}");
                }
            }
            Err(e) => tracing::warn!("Failed to begin tx for pending invitation: {e}"),
        }

        let mut invitations = self.pending_invitations.write().await;
        invitations.retain(|i| i.id != invitation.id);
        invitations.push(invitation);
    }

    async fn drop_invitations_from(&self, coordinator_pubkey: &str) {
        match self.db.begin_transaction().await {
            Ok(mut tx) => {
                if let Err(e) = tx
                    .delete_pending_invitations_by_coordinator(coordinator_pubkey)
                    .await
                {
                    tracing::warn!("Failed to delete persisted invitations: {e}");
                } else if let Err(e) = Commitable::commit(tx).await {
                    tracing::warn!("Failed to commit invitation deletion: {e}");
                }
            }
            Err(e) => tracing::warn!("Failed to begin tx for invitation deletion: {e}"),
        }

        let mut invitations = self.pending_invitations.write().await;
        invitations.retain(|i| i.coordinator_pubkey != coordinator_pubkey);
    }

    /// Cancel any peer-side workflow_runs we have InProgress whose
    /// coordinator matches the sender of a CancelInvite. Same authority — the
    /// coordinator who started the workflow is also the one who's allowed to
    /// abort it. Used by the CancelInvite listener arm so a single message
    /// covers both un-accepted invites AND accepted-but-running runs.
    async fn cancel_peer_runs_from(&self, coordinator_pubkey: &str) {
        let runs = match SchemaRead::get_in_progress_workflow_runs(&self.db).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("cancel_peer_runs_from: load failed: {e}");
                return;
            }
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        for run in runs.into_iter().filter(|r| {
            r.role == WorkflowRole::Peer
                && r.coordinator_pubkey.as_deref() == Some(coordinator_pubkey)
        }) {
            let mut tx = match self.db.begin_transaction().await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("cancel_peer_runs_from: begin_transaction: {e}");
                    continue;
                }
            };
            if let Err(e) = tx
                .set_workflow_run_status(
                    &run.instance_name,
                    WorkflowProgress::Cancelled,
                    Some("Coordinator cancelled the workflow"),
                    now,
                )
                .await
            {
                tracing::warn!(
                    "cancel_peer_runs_from: update failed for {}: {e}",
                    run.instance_name
                );
                continue;
            }
            if let Err(e) = Commitable::commit(tx).await {
                tracing::warn!(
                    "cancel_peer_runs_from: commit failed for {}: {e}",
                    run.instance_name
                );
            } else {
                tracing::info!(
                    "Cancelled peer workflow run {} (coordinator cancelled)",
                    run.instance_name
                );
            }
        }
    }

    /// Coordinator-initiated retry: find any Failed peer rows whose
    /// `coordinator_pubkey` matches the sender, flip them back to InProgress,
    /// and fire the per-kind trigger so `start_peer` re-spins. Same
    /// authority model as `cancel_peer_runs_from` — the coordinator who
    /// started the run is also the one allowed to retry it.
    async fn retry_peer_runs_from(&self, coordinator_pubkey: &str) {
        let runs = match SchemaRead::get_visible_workflow_runs(&self.db).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("retry_peer_runs_from: load failed: {e}");
                return;
            }
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        for run in runs.into_iter().filter(|r| {
            r.role == WorkflowRole::Peer
                && r.status == WorkflowProgress::Failed
                && r.coordinator_pubkey.as_deref() == Some(coordinator_pubkey)
        }) {
            let mut tx = match self.db.begin_transaction().await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("retry_peer_runs_from: begin_transaction: {e}");
                    continue;
                }
            };
            if let Err(e) = tx
                .set_workflow_run_status(
                    &run.instance_name,
                    WorkflowProgress::InProgress,
                    None,
                    now,
                )
                .await
            {
                tracing::warn!(
                    "retry_peer_runs_from: status flip failed for {}: {e}",
                    run.instance_name
                );
                continue;
            }
            if let Err(e) = Commitable::commit(tx).await {
                tracing::warn!(
                    "retry_peer_runs_from: commit failed for {}: {e}",
                    run.instance_name
                );
                continue;
            }
            refire_peer(
                &run,
                &self.onboarding_trigger,
                &self.kick_trigger,
                &self.contracts_trigger,
                &self.dars_trigger,
                &self.coordinator_pubkey,
                &self.peer_run_instance,
            )
            .await;
            tracing::info!(
                "Re-fired peer workflow run {} (coordinator retried)",
                run.instance_name
            );
        }
    }
}

/// Start the HTTP server and a heartbeat system for peer status tracking
pub async fn start_server(
    host: &str,
    port: u16,
    config: NodeConfig,
    db: SqlitePool,
    admin_role: Option<String>,
    allowed_origin: Option<String>,
) -> Result {
    // Test-mode is a compile-time decision: it's `true` iff the binary was
    // built with `--features test-mode` (or under `cargo test`). Production
    // binaries cannot run with mock auth — the `MockValidator` and
    // `MockAuthRegistry` selections below are gated by the same cfg.
    let test_mode = cfg!(any(test, feature = "test-mode"));

    if !test_mode {
        tracing::info!(
            "Running production build (no `test-mode` feature). Swagger UI disabled, \
             real JWT validation in effect."
        );
    }

    // Make the admin-role policy explicit at boot so a single-user deployment
    // doesn't quietly lose authorization on multi-user upgrade. With
    // `admin_role = None` (the default since the gating became opt-in),
    // every authenticated caller passes `require_admin`.
    match admin_role.as_deref() {
        Some(role) if !role.is_empty() => {
            tracing::info!("Admin gate active: requests must carry role '{role}'");
        }
        _ => {
            tracing::warn!(
                "DECPM_ADMIN_ROLE not set: every authenticated caller is treated as admin. \
                 Set DECPM_ADMIN_ROLE=<role> to require a specific Keycloak role on \
                 PUT /party-config, POST /kick, POST /auth/grant-rights, and other \
                 admin-gated endpoints."
            );
        }
    }

    let db_party_creds = db.get_all_party_credentials().await.unwrap_or_else(|e| {
        tracing::warn!("Failed to load party credentials from DB: {e}");
        Vec::new()
    });
    let party_credentials = Arc::new(RwLock::new(db_party_creds.clone()));

    // Initialize auth based on mode
    #[cfg(any(test, feature = "test-mode"))]
    let auth = {
        tracing::info!("Running in TEST MODE - using mock authentication");
        Some(WorkflowAuth::Mock(Arc::new(MockAuthRegistry::new(
            party_credentials.clone(),
        ))))
    };
    #[cfg(not(any(test, feature = "test-mode")))]
    let auth = if db_party_creds.is_empty() {
        tracing::info!("No party credentials configured, auth disabled");
        None
    } else {
        tracing::info!(
            "Initializing auth registry for {} parties",
            db_party_creds.len()
        );
        Some(WorkflowAuth::Keycloak(Arc::new(
            AuthRegistry::new(&db_party_creds).await?,
        )))
    };

    let auth = Arc::new(RwLock::new(auth));

    // Inbound token validator. Production verifies JWT signatures locally
    // against the JWKS of any trusted issuer derived from the top-level
    // Single process-wide `reqwest::Client`. Shared by `AppState.http_client`
    // (proxy-style handlers) and the JWT/OIDC validators so all outbound
    // HTTPS traffic goes through the same connection pool / TLS session
    // cache and inherits the same 10s timeout.
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("reqwest client build");

    // keycloak config plus any `party_credentials` rows. The permissive
    // `MockValidator` is compiled in only behind `cfg(any(test, feature =
    // "test-mode"))`, so a release binary cannot select it.
    #[cfg(any(test, feature = "test-mode"))]
    let token_validator = TokenValidator::Mock(Arc::new(MockValidator::new(
        admin_role.clone().unwrap_or_default(),
    )));
    #[cfg(not(any(test, feature = "test-mode")))]
    let token_validator = {
        let no_top_level_config = config.keycloak.is_none();
        let no_party_creds = party_credentials.read().await.is_empty();
        if no_top_level_config && no_party_creds {
            tracing::warn!(
                "No top-level Keycloak config (--keycloak-url/realm/client-id) and no \
                 party credentials yet. Inbound auth will reject every request except \
                 the first-run PUT /party-config bootstrap. Configure the IdP and \
                 provision a party to make the node usable."
            );
        } else if no_top_level_config {
            tracing::info!(
                "No top-level Keycloak config; trusting only issuers from \
                 party_credentials ({} configured).",
                party_credentials.read().await.len()
            );
        }
        TokenValidator::Jwt(Arc::new(JwtValidator::new(
            config.keycloak.clone(),
            config.auth0.clone(),
            party_credentials.clone(),
            http_client.clone(),
        )))
    };

    let peer_status = Arc::new(RwLock::new(HashMap::new()));
    let last_seen: LastSeen = Arc::new(RwLock::new(HashMap::new()));
    let onboarding_trigger = Arc::new(Notify::new());
    let kick_trigger = Arc::new(Notify::new());
    let contracts_trigger = Arc::new(Notify::new());
    let dars_trigger = Arc::new(Notify::new());
    let coordinator_pubkey = Arc::new(RwLock::new(None));
    let peer_run_instance = Arc::new(RwLock::new(None));
    let persisted_invitations = db.get_all_pending_invitations().await.unwrap_or_else(|e| {
        tracing::warn!("Failed to load persisted pending invitations: {e}");
        Vec::new()
    });
    if !persisted_invitations.is_empty() {
        tracing::info!(
            "Loaded {} persisted pending invitation(s) from DB",
            persisted_invitations.len()
        );
    }
    let pending_invitations = Arc::new(RwLock::new(persisted_invitations));

    let active_workflow: ActiveWorkflowSlot = Arc::new(std::sync::RwLock::new(None));
    let app_state = web::Data::new(AppState {
        db: db.clone(),
        config: config.clone(),
        peer_status: peer_status.clone(),
        last_seen: last_seen.clone(),
        onboarding_trigger: onboarding_trigger.clone(),
        kick_trigger: kick_trigger.clone(),
        contracts_trigger: contracts_trigger.clone(),
        dars_trigger: dars_trigger.clone(),
        coordinator_pubkey: coordinator_pubkey.clone(),
        peer_run_instance: peer_run_instance.clone(),
        pending_invitations: pending_invitations.clone(),
        auth,
        token_validator,
        admin_role,
        party_credentials: party_credentials.clone(),
        bootstrap_mu: Arc::new(Mutex::new(())),
        workflow_in_flight: Arc::new(AtomicBool::new(false)),
        active_workflow: active_workflow.clone(),
        test_mode,
        refreshing_prefixes: Arc::new(RwLock::new(HashSet::new())),
        http_client,
    });
    let kick_state = web::Data::new(Arc::new(handlers::KickWorkflowState::new()));
    let onboarding_state = web::Data::new(Arc::new(handlers::OnboardingWorkflowState::new()));
    let contracts_state = web::Data::new(Arc::new(handlers::ContractsWorkflowState::new()));
    let dars_state = web::Data::new(Arc::new(handlers::DarsWorkflowState::new()));

    // Boot-time workflow recovery. For any `workflow_runs` row that was
    // InProgress when we shut down, re-spawn the coordinator task (which
    // resumes at the persisted `current_step` via `WorkflowState::from_persisted`)
    // or re-fire the peer trigger so its listener picks the run back up.
    recover_in_progress_workflows(
        db.clone(),
        config.clone(),
        kick_state.clone(),
        onboarding_state.clone(),
        contracts_state.clone(),
        dars_state.clone(),
        active_workflow.clone(),
        app_state.auth.clone(),
        last_seen.clone(),
        onboarding_trigger.clone(),
        kick_trigger.clone(),
        contracts_trigger.clone(),
        dars_trigger.clone(),
        coordinator_pubkey.clone(),
        peer_run_instance.clone(),
    )
    .await;

    // Start heartbeat background task (pings peers and listens for invites)
    let heartbeat_config = config.clone();
    let heartbeat_db = db.clone();
    let heartbeat_status = peer_status.clone();
    let heartbeat_last_seen = last_seen.clone();
    let heartbeat_triggers = WorkflowTriggers {
        pending_invitations: pending_invitations.clone(),
        config: config.clone(),
        admin_api_url: config.admin_api_url(),
        list_packages_chunk_cache: Arc::new(Mutex::new(HashMap::new())),
        db: db.clone(),
        party_credentials: party_credentials.clone(),
        onboarding_trigger: onboarding_trigger.clone(),
        kick_trigger: kick_trigger.clone(),
        contracts_trigger: contracts_trigger.clone(),
        dars_trigger: dars_trigger.clone(),
        coordinator_pubkey: coordinator_pubkey.clone(),
        peer_run_instance: peer_run_instance.clone(),
        active_workflow: active_workflow.clone(),
    };
    tokio::spawn(async move {
        run_heartbeat(
            heartbeat_config,
            heartbeat_db,
            heartbeat_status,
            heartbeat_last_seen,
            heartbeat_triggers,
        )
        .await;
    });

    // Start peer trigger listener for onboarding (starts peer workflow when invite received)
    let onboarding_peer_config = config.clone();
    let onboarding_peer_db = db.clone();
    let onboarding_peer_state = onboarding_state.clone();
    let onboarding_coordinator_pubkey = coordinator_pubkey.clone();
    let onboarding_peer_run_instance = peer_run_instance.clone();
    tokio::spawn(async move {
        run_onboarding_peer_listener(
            onboarding_peer_config,
            onboarding_peer_db,
            onboarding_peer_state,
            onboarding_trigger,
            onboarding_coordinator_pubkey,
            onboarding_peer_run_instance,
        )
        .await;
    });

    // Start peer trigger listener for kick (starts peer workflow when kick invite received)
    let kick_peer_config = config.clone();
    let kick_peer_db = db.clone();
    let kick_peer_state = kick_state.clone();
    let kick_coordinator_pubkey = coordinator_pubkey.clone();
    let kick_peer_run_instance = peer_run_instance.clone();
    tokio::spawn(async move {
        run_kick_peer_listener(
            kick_peer_config,
            kick_peer_db,
            kick_peer_state,
            kick_trigger,
            kick_coordinator_pubkey,
            kick_peer_run_instance,
        )
        .await;
    });

    // Start peer trigger listener for contracts (starts peer workflow when contracts invite received)
    let contracts_peer_config = config.clone();
    let contracts_peer_db = db.clone();
    let contracts_peer_state = contracts_state.clone();
    let contracts_coordinator_pubkey = coordinator_pubkey.clone();
    let contracts_peer_run_instance = peer_run_instance.clone();
    tokio::spawn(async move {
        run_contracts_peer_listener(
            contracts_peer_config,
            contracts_peer_db,
            contracts_peer_state,
            contracts_trigger,
            contracts_coordinator_pubkey,
            contracts_peer_run_instance,
        )
        .await;
    });

    // Start peer trigger listener for DARs (starts peer workflow when DARs invite received)
    let dars_peer_config = config.clone();
    let dars_peer_db = db.clone();
    let dars_peer_state = dars_state.clone();
    let dars_coordinator_pubkey = coordinator_pubkey.clone();
    let dars_peer_run_instance = peer_run_instance.clone();
    tokio::spawn(async move {
        run_dars_peer_listener(
            dars_peer_config,
            dars_peer_db,
            dars_peer_state,
            dars_trigger,
            dars_coordinator_pubkey,
            dars_peer_run_instance,
        )
        .await;
    });

    // Background task: sync decentralized parties from Canton on startup
    let sync_config = config.clone();
    let sync_db = db.clone();
    let sync_auth = app_state.auth.clone();
    let sync_party_creds = app_state.party_credentials.clone();
    tokio::spawn(async move {
        // Delay to let Canton stabilize after startup
        tokio::time::sleep(Duration::from_secs(5)).await;
        tracing::info!("Starting background sync of decentralized parties from Canton...");

        let auth_snapshot = sync_auth.read().await.clone();
        let creds_snapshot = sync_party_creds.read().await.clone();

        match handlers::fetch_decentralized_parties(
            &sync_config,
            None,
            auth_snapshot,
            &creds_snapshot,
        )
        .await
        {
            Ok(response) => {
                if let Err(e) = handlers::store_parties_to_db(&sync_db, "", &response.parties).await
                {
                    tracing::warn!("Failed to cache parties on startup: {e}");
                } else {
                    tracing::info!(
                        "Cached {} decentralized parties from Canton",
                        response.parties.len()
                    );
                    handlers::resolve_owner_keys_from_peers(
                        &sync_config,
                        &sync_db,
                        &response.parties,
                    )
                    .await;
                }
            }
            Err(e) => {
                tracing::warn!("Background Canton sync failed on startup: {e}");
            }
        }
    });

    tracing::info!("Starting HTTP server on {host}:{port}");
    tracing::info!("Frontend available at http://{host}:{port}/");

    HttpServer::new(move || {
        // Frontend is embedded and served from this same origin, so no
        // cross-origin access is required by default. `Cors::default()` is
        // same-origin only — tightening the previous `Cors::permissive()`.
        //
        // For split-origin deployments (reverse proxy, separate dev server,
        // etc.) the operator can set `--allowed-origin` to permit one
        // additional origin with credentials.
        let cors = match allowed_origin.as_deref() {
            Some(origin) => Cors::default()
                .allowed_origin(origin)
                .allow_any_method()
                .allow_any_header()
                .supports_credentials(),
            None => Cors::default(),
        };

        // Increase payload limit to 100MB for DAR file uploads
        let json_config = web::JsonConfig::default().limit(100 * 1024 * 1024);
        let payload_config = web::PayloadConfig::default().limit(100 * 1024 * 1024);

        // Build app with utoipa-actix-web: each .service() call both registers
        // the actix route AND collects its OpenAPI path automatically.
        // No separate path list to maintain.
        let (app, api) = App::new()
            .into_utoipa_app()
            .app_data(json_config)
            .app_data(payload_config)
            .app_data(app_state.clone())
            .app_data(kick_state.clone())
            .app_data(onboarding_state.clone())
            .app_data(contracts_state.clone())
            .app_data(dars_state.clone())
            .service(handlers::get_network_config)
            .service(handlers::save_network_config)
            .service(handlers::get_node_config)
            .service(handlers::get_decentralized_parties)
            .service(handlers::get_participants_status)
            .service(handlers::compare_peer_packages)
            .service(handlers::get_vetted_packages)
            .service(handlers::start_kick)
            .service(handlers::get_kick_status)
            .service(handlers::cancel_kick)
            .service(handlers::start_onboarding)
            .service(handlers::get_onboarding_status)
            .service(handlers::cancel_onboarding)
            .service(handlers::start_contracts)
            .service(handlers::get_contracts_status)
            .service(handlers::cancel_contracts)
            .service(handlers::upload_dars_local)
            .service(handlers::start_dars)
            .service(handlers::get_dars_status)
            .service(handlers::cancel_dars)
            .service(handlers::list_workflows)
            .service(handlers::dismiss_workflow)
            .service(handlers::retry_workflow)
            .service(handlers::get_key_status)
            .service(handlers::get_invitations)
            .service(handlers::accept_invitation)
            .service(handlers::decline_invitation)
            .service(handlers::get_auth_config)
            .service(handlers::get_auth_status)
            .service(handlers::test_auth)
            .service(handlers::grant_rights)
            .service(handlers::get_governance)
            .service(handlers::get_governance_state)
            .service(handlers::get_known_members)
            .service(handlers::get_vaults_handler)
            .service(handlers::get_provider_services_handler)
            .service(handlers::get_user_services_handler)
            .service(handlers::get_registrar_services_handler)
            .service(handlers::get_instruments_handler)
            .service(handlers::get_transfer_instructions_handler)
            .service(handlers::get_mint_requests_handler)
            .service(handlers::get_burn_requests_handler)
            .service(handlers::get_transfer_preapprovals_handler)
            .service(handlers::get_transfer_factories_handler)
            .service(handlers::get_holdings_handler)
            .service(handlers::query_contracts_handler)
            .service(handlers::get_packages)
            .service(handlers::propose_action)
            .service(handlers::confirm_action)
            .service(handlers::execute_action)
            .service(handlers::expire_confirmation)
            .service(handlers::cancel_confirmation)
            .service(handlers::get_governance_audit)
            .service(handlers::get_governance_chain_audit)
            .service(handlers::get_token_standard_contracts)
            .service(handlers::get_network_info)
            .service(handlers::get_operator_info)
            .service(handlers::get_party_config)
            .service(handlers::save_party_config)
            .service(handlers::discover_member_party)
            .split_for_parts();

        let mut app = app.wrap(AuthMiddleware).wrap(cors);
        if test_mode {
            app = app
                .service(SwaggerUi::new("/swagger-ui/{_:.*}").url("/api-docs/openapi.json", api));
        }
        app.service(assets::serve_frontend)
    })
    .bind((host, port))?
    .run()
    .await?;

    Ok(())
}

/// Background task that runs a Noise server for handling pings and invites
async fn run_heartbeat(
    config: NodeConfig,
    db: SqlitePool,
    peer_status: Arc<RwLock<HashMap<String, bool>>>,
    last_seen: LastSeen,
    triggers: WorkflowTriggers,
) {
    use tokio::net::TcpListener;

    let listen_addr = format!(
        "{addr}:{port}",
        addr = config.node.listen_address,
        port = config.node.port
    );

    // Load or generate keypair for Noise handshakes
    let keypair = match load_or_generate_keypair(&config.key_file_path()).await {
        Ok(kp) => Arc::new(kp),
        Err(e) => {
            tracing::error!("Failed to load or generate keypair: {e}");
            return;
        }
    };

    // Load peers from database for peer key authentication
    let peers = match db.get_all_peers().await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Failed to load peers from database: {e}");
            return;
        }
    };

    // Build peer key map for Noise authentication
    let mut peer_keys = HashMap::new();
    for peer in &peers {
        if peer.participant_id == *config.participant_id() || peer.public_key.is_empty() {
            continue;
        }
        if let Ok(pub_key) = parse_public_key(&peer.public_key) {
            peer_keys.insert(peer.participant_id.to_string(), pub_key);
        }
    }
    let peer_keys = Arc::new(peer_keys);

    // Listener loop: bind the always-on Noise listener and accept forever. It is
    // never paused — workflow traffic is routed in-process via the
    // active-workflow slot, so the listener stays up (and keeps answering
    // Health / Ping) even while this node is participating in a workflow.
    let keypair_spawn = keypair.clone();
    let last_seen_spawn = last_seen.clone();
    let peer_keys_spawn = peer_keys.clone();
    let triggers_spawn = triggers.clone();

    tokio::spawn(async move {
        loop {
            match TcpListener::bind(&listen_addr).await {
                Ok(listener) => {
                    tracing::info!("Noise listener started on {listen_addr}");

                    loop {
                        if let Ok((socket, peer_addr)) = listener.accept().await {
                            let keypair = keypair_spawn.clone();
                            let last_seen = last_seen_spawn.clone();
                            let peer_keys = peer_keys_spawn.clone();
                            let triggers = triggers_spawn.clone();

                            tokio::spawn(async move {
                                handle_incoming_connection(
                                    socket, peer_addr, keypair, peer_keys, triggers, last_seen,
                                )
                                .await;
                            });
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to bind Noise listener on {listen_addr}: {e}, retrying in 5s"
                    );
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });

    // Ping peers every 5 seconds
    run_peer_ping_loop(config, db, peer_status, last_seen, keypair).await;
}

/// Handle an incoming Noise connection (either ping or invite)
async fn handle_incoming_connection(
    socket: tokio::net::TcpStream,
    peer_addr: std::net::SocketAddr,
    keypair: Arc<NoiseKeypair>,
    peer_keys: Arc<HashMap<String, secp256k1::PublicKey>>,
    triggers: WorkflowTriggers,
    last_seen: LastSeen,
) {
    let keypair_for_closure = keypair.clone();
    let peer_keys_clone = peer_keys.clone();
    let our_public_key_hex = keypair.public_key_hex();

    // Create PSK derivation responder. Identity MUST be the sender's
    // `participant_id` string — looked up in `peer_keys_clone` (the
    // allowlist) before any ECDH happens. Returns the raw [u8; 32] by
    // deref-copy from the Zeroizing wrapper; the wrapper drops here so
    // the in-keypair secret material is the only long-lived copy.
    //
    // The earlier "raw 33-byte compressed public key" fallback was
    // removed: it bypassed the allowlist, letting any keypair-holder
    // complete the handshake and inject Invite* messages (audit finding).
    let responder = Responder::new(move |identity: &[u8]| -> Option<[u8; 32]> {
        let peer_id = std::str::from_utf8(identity).ok()?;
        let peer_pub_key = peer_keys_clone.get(peer_id)?;
        Some(*keypair_for_closure.derive_psk(peer_pub_key))
    });

    let result = hyper_noise::server::serve_http(
        socket,
        responder,
        move |peer_id: &[u8], req: hyper::Request<Body>| {
            let triggers = triggers.clone();
            let our_pubkey = our_public_key_hex.clone();
            let peer_keys = peer_keys.clone();
            let last_seen = last_seen.clone();

            // The responder above only authenticates identities that are
            // valid utf-8 participant_id strings present in `peer_keys` (the
            // raw 33-byte fallback was removed in #136 as an audit finding).
            // We extract `peer_id_str` once so both `peer_pubkey_hex` and the
            // `last_seen` bump below can reuse it; the conservative
            // `peer_keys` re-check on the bump path is defensive against any
            // future responder change.
            let peer_id_str = std::str::from_utf8(peer_id).ok().map(str::to_owned);
            let peer_pubkey_hex = peer_id_str
                .as_deref()
                .and_then(|id| peer_keys.get(id))
                .map(|pk| hex::encode(pk.serialize()));

            async move {
                // Bump last_seen for known peers.
                if let Some(id) = peer_id_str.as_deref()
                    && peer_keys.contains_key(id)
                {
                    let now = std::time::Instant::now();
                    let mut map = last_seen.write().await;
                    peer_status::bump(&mut map, id.to_string(), now);
                }

                let body_bytes = hyper::body::to_bytes(req.into_body()).await?;

                if body_bytes.len() < 6 {
                    return Ok::<_, hyper::Error>(Response::new(Body::empty()));
                }

                if let Ok(msg) = Message::from_bytes(&body_bytes) {
                    tracing::debug!("Received message type {:?}", msg.msg_type);

                    match msg.msg_type {
                        MessageType::Ping => {
                            tracing::debug!("Received ping, responding with pong");
                            let pong = Message::new(MessageType::Pong, our_pubkey.into_bytes());
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(pong.to_bytes()))
                                .unwrap());
                        }
                        MessageType::Health => {
                            // Always answer health — even mid-workflow. Reports
                            // liveness plus this node's in-flight workflow (if
                            // any) so peers don't infer "offline" from a busy node.
                            let resp = health::build_health_response(
                                &triggers.db,
                                &triggers.config.participant_id().to_string(),
                            )
                            .await;
                            let msg = Message::new(MessageType::HealthResponse, resp.to_payload());
                            // `Response::new` defaults to 200 OK and is infallible,
                            // so there is no builder error to unwrap.
                            return Ok(Response::new(Body::from(msg.to_bytes())));
                        }
                        MessageType::ListPackages => {
                            tracing::debug!("Received ListPackages request");
                            let admin_url = triggers.admin_api_url.clone();
                            let payload = match list_local_packages(&admin_url).await {
                                Ok(data) => data,
                                Err(e) => {
                                    tracing::error!("Failed to list packages: {e}");
                                    b"[]".to_vec()
                                }
                            };

                            if payload.len() <= MAX_PAYLOAD_SIZE {
                                // Small enough to ship in one un-chunked Data response.
                                let response_msg = Message::new(MessageType::Data, payload);
                                return Ok(Response::builder()
                                    .status(StatusCode::OK)
                                    .body(Body::from(response_msg.to_bytes()))
                                    .unwrap());
                            }

                            // Too large for one Noise frame — cache the payload and send
                            // ChunkedCommand metadata. Subsequent GetChunk requests from the same
                            // peer will pull chunks from the cache.
                            let Some(ref pk) = peer_pubkey_hex else {
                                // Without a peer pubkey we have nowhere to key the cache. Fall
                                // through to sending the full payload anyway — it'll fail at the
                                // transport layer, but logs will show what happened.
                                tracing::warn!(
                                    "ListPackages response is {} bytes (> {} chunk threshold) but \
                                     no peer pubkey available; cannot chunk. Sending unchunked \
                                     anyway, may fail at transport.",
                                    payload.len(),
                                    MAX_PAYLOAD_SIZE,
                                );
                                let response_msg = Message::new(MessageType::Data, payload);
                                return Ok(Response::builder()
                                    .status(StatusCode::OK)
                                    .body(Body::from(response_msg.to_bytes()))
                                    .unwrap());
                            };

                            // Server-side cap symmetric with the client's `MAX_CHUNKED_TOTAL_SIZE`.
                            // Without this, a very large package listing could (a) eat unbounded
                            // memory in the per-peer cache and (b) truncate silently on the
                            // `usize → u32` casts below. 16 MiB is well above any plausible Canton
                            // package listing.
                            if payload.len() > MAX_CHUNKED_TOTAL_SIZE {
                                tracing::error!(
                                    "ListPackages response is {} bytes; exceeds chunked cap {} — \
                                     refusing to chunk",
                                    payload.len(),
                                    MAX_CHUNKED_TOTAL_SIZE,
                                );
                                return Ok(Response::builder()
                                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                                    .body(Body::empty())
                                    .unwrap());
                            }

                            // Casts are infallible because we just verified `payload.len() <=
                            // MAX_CHUNKED_TOTAL_SIZE` (16 MiB), which fits in u32.
                            let total_size =
                                u32::try_from(payload.len()).expect("checked against cap");
                            let chunk_count = u32::try_from(payload.len().div_ceil(CHUNK_SIZE))
                                .expect("checked against cap");
                            tracing::info!(
                                "ListPackages response too large ({total_size} bytes), chunking \
                                 into {chunk_count} chunks for peer {pk}",
                            );

                            // Cache the payload (evict expired entries first; replace this peer's
                            // existing entry if any).
                            {
                                let mut cache = triggers.list_packages_chunk_cache.lock().await;
                                cache.retain(|_, (_, t)| {
                                    t.elapsed() < LIST_PACKAGES_CHUNK_CACHE_TTL
                                });
                                cache.insert(pk.clone(), (payload, Instant::now()));
                            }

                            // Build ChunkedCommand metadata: [Data:2][total_size:4][chunk_count:4]
                            // The first 2 bytes record the type the client should reconstitute the
                            // assembled payload as — `Data` for ListPackages responses.
                            let mut meta = Vec::with_capacity(10);
                            meta.extend_from_slice(&MessageType::Data.to_u16().to_be_bytes());
                            meta.extend_from_slice(&total_size.to_be_bytes());
                            meta.extend_from_slice(&chunk_count.to_be_bytes());

                            let response_msg = Message::new(MessageType::ChunkedCommand, meta);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(response_msg.to_bytes()))
                                .unwrap());
                        }
                        MessageType::GetChunk => {
                            // During a workflow, GetChunk fetches the workflow's
                            // chunked command payload via the registered slot;
                            // otherwise it serves a chunked ListPackages response
                            // from the cache below.
                            let active = triggers
                                .active_workflow
                                .read()
                                .unwrap_or_else(|e| e.into_inner())
                                .clone();
                            if let Some(wf) = active
                                && let Some(pid) =
                                    peer_id_str.as_deref().and_then(|s| CantonId::parse(s).ok())
                            {
                                let resp = wf.handle_command(pid, msg).await.unwrap_or_else(|e| {
                                    Message::new(MessageType::Error, format!("{e}").into_bytes())
                                });
                                return Ok(Response::new(Body::from(resp.to_bytes())));
                            }
                            if msg.payload.len() < 4 {
                                tracing::warn!("Received GetChunk request with payload < 4 bytes");
                                return Ok(Response::builder()
                                    .status(StatusCode::BAD_REQUEST)
                                    .body(Body::empty())
                                    .unwrap());
                            }
                            let chunk_index = u32::from_be_bytes([
                                msg.payload[0],
                                msg.payload[1],
                                msg.payload[2],
                                msg.payload[3],
                            ]) as usize;

                            let Some(ref pk) = peer_pubkey_hex else {
                                tracing::warn!(
                                    "Received GetChunk request without identifiable peer pubkey"
                                );
                                return Ok(Response::builder()
                                    .status(StatusCode::BAD_REQUEST)
                                    .body(Body::empty())
                                    .unwrap());
                            };

                            let chunk_bytes = {
                                let mut cache = triggers.list_packages_chunk_cache.lock().await;

                                // Pre-check expiry without holding a borrow into the entry,
                                // so we can `remove` the expired entry without borrowck pain.
                                let entry_state = cache
                                    .get(pk)
                                    .map(|(_, t)| t.elapsed() >= LIST_PACKAGES_CHUNK_CACHE_TTL);
                                match entry_state {
                                    None => {
                                        tracing::warn!(
                                            "GetChunk request from {pk} for chunk {chunk_index} \
                                             but no cached payload"
                                        );
                                        return Ok(Response::builder()
                                            .status(StatusCode::NOT_FOUND)
                                            .body(Body::empty())
                                            .unwrap());
                                    }
                                    Some(true) => {
                                        // Expired — drop the stale entry now so it doesn't
                                        // linger until the next ListPackages-driven `retain`.
                                        cache.remove(pk);
                                        tracing::warn!(
                                            "GetChunk for {pk} chunk {chunk_index}: cache entry \
                                             expired (removed)"
                                        );
                                        return Ok(Response::builder()
                                            .status(StatusCode::NOT_FOUND)
                                            .body(Body::empty())
                                            .unwrap());
                                    }
                                    Some(false) => {}
                                }

                                let (payload, t) = cache.get_mut(pk).expect("checked above");
                                let start = chunk_index * CHUNK_SIZE;
                                if start >= payload.len() {
                                    tracing::warn!(
                                        "GetChunk for {pk} chunk {chunk_index}: out of range \
                                         (start={start}, payload_len={})",
                                        payload.len(),
                                    );
                                    return Ok(Response::builder()
                                        .status(StatusCode::BAD_REQUEST)
                                        .body(Body::empty())
                                        .unwrap());
                                }
                                let end = (start + CHUNK_SIZE).min(payload.len());
                                let bytes = payload[start..end].to_vec();
                                // Extend TTL on successful read so a slow peer
                                // mid-reassembly can't have entries evicted out from
                                // under it just because the original 30s window from
                                // insertion ran out.
                                *t = Instant::now();
                                bytes
                            };

                            // Build Chunk response: [chunk_index:4][chunk_data]
                            let mut response_payload = Vec::with_capacity(4 + chunk_bytes.len());
                            response_payload.extend_from_slice(&(chunk_index as u32).to_be_bytes());
                            response_payload.extend_from_slice(&chunk_bytes);

                            let response_msg = Message::new(MessageType::Chunk, response_payload);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(response_msg.to_bytes()))
                                .unwrap());
                        }
                        MessageType::RequestOwnerKeys => {
                            tracing::debug!("Received RequestOwnerKeys request");
                            // Payload is a JSON array of decentralized party_ids
                            // the caller wants this node's owner_keys for (see
                            // #149: peer no longer enumerates the synchronizer).
                            // A malformed or absent payload is treated as
                            // "nothing requested" — return an empty list rather
                            // than fall back to a slow whole-synchronizer scan.
                            let requested_party_ids: Vec<String> =
                                serde_json::from_slice(&msg.payload).unwrap_or_default();
                            let payload =
                                match list_my_owner_keys(&triggers.config, &requested_party_ids)
                                    .await
                                {
                                    Ok(data) => data,
                                    Err(e) => {
                                        tracing::error!("Failed to list owner keys: {e}");
                                        b"[]".to_vec()
                                    }
                                };
                            let response_msg = Message::new(MessageType::OwnerKeys, payload);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(response_msg.to_bytes()))
                                .unwrap());
                        }
                        MessageType::ListPeers => {
                            tracing::debug!("Received ListPeers request");
                            let payload = match triggers.db.get_all_peers().await {
                                Ok(peers) => {
                                    let ids: Vec<String> = peers
                                        .into_iter()
                                        .map(|p| p.participant_id.to_string())
                                        .collect();
                                    serde_json::to_vec(&ids).unwrap_or_else(|_| b"[]".to_vec())
                                }
                                Err(e) => {
                                    tracing::error!("Failed to list peers: {e}");
                                    b"[]".to_vec()
                                }
                            };
                            let response_msg = Message::new(MessageType::PeerList, payload);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(response_msg.to_bytes()))
                                .unwrap());
                        }
                        MessageType::RequestMemberParty => {
                            let dec_party_id =
                                std::str::from_utf8(&msg.payload).unwrap_or("").to_string();
                            tracing::debug!("Received RequestMemberParty for {dec_party_id}",);
                            let payload = {
                                let creds = triggers.party_credentials.read().await;
                                creds
                                    .iter()
                                    .find(|c| c.dec_party_id.to_string() == dec_party_id)
                                    .map(|c| c.member_party_id.to_string().into_bytes())
                                    .unwrap_or_default()
                            };
                            let response_msg =
                                Message::new(MessageType::MemberPartyResponse, payload);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(response_msg.to_bytes()))
                                .unwrap());
                        }
                        MessageType::GetNextCommand
                        | MessageType::KeysUpload
                        | MessageType::DnsSignature
                        | MessageType::P2pSignatures
                        | MessageType::SubmissionSignatures
                        | MessageType::KickSignatures
                        | MessageType::StatusUpdate
                        | MessageType::DeclineInvitation => {
                            // Route workflow-command traffic to the coordinator's
                            // in-flight workflow (registered in the slot). Hold the
                            // lock only long enough to clone the handle out — never
                            // across the await.
                            let active = triggers
                                .active_workflow
                                .read()
                                .unwrap_or_else(|e| e.into_inner())
                                .clone();
                            let peer = peer_id_str.as_deref().and_then(|s| CantonId::parse(s).ok());
                            let resp = match (active, peer) {
                                (Some(wf), Some(pid)) => {
                                    wf.handle_command(pid, msg).await.unwrap_or_else(|e| {
                                        Message::new(
                                            MessageType::Error,
                                            format!("{e}").into_bytes(),
                                        )
                                    })
                                }
                                _ => Message::new_empty(MessageType::Wait),
                            };
                            return Ok(Response::new(Body::from(resp.to_bytes())));
                        }
                        MessageType::InviteOnboarding
                        | MessageType::InviteKick
                        | MessageType::InviteContracts
                        | MessageType::InviteDars => {
                            // Refuse a new invite while already in a workflow, so a
                            // busy node doesn't silently queue a second one. The DB
                            // is the single source of truth for both coordinator and
                            // peer roles.
                            let health = health::build_health_response(
                                &triggers.db,
                                &triggers.config.participant_id().to_string(),
                            )
                            .await;
                            if let Some(wf) = health.workflow {
                                let kind = wf.kind;
                                tracing::info!(
                                    "Refusing invite — node is already in a {kind} workflow"
                                );
                                return Ok(Response::new(Body::from(
                                    Message::new(MessageType::Busy, health::busy_payload(kind))
                                        .to_bytes(),
                                )));
                            }
                            let invitation_type = match msg.msg_type {
                                MessageType::InviteOnboarding => InvitationType::Onboarding,
                                MessageType::InviteKick => InvitationType::Kick,
                                MessageType::InviteContracts => InvitationType::Contracts,
                                MessageType::InviteDars => InvitationType::Dars,
                                _ => unreachable!(),
                            };
                            tracing::info!(
                                "Received {invitation_type} invite, storing as pending invitation"
                            );
                            let meta = if msg.payload.is_empty() {
                                InvitationMeta::None
                            } else {
                                match invitation_type {
                                    InvitationType::Onboarding => {
                                        match serde_json::from_slice::<OnboardingInvitePayload>(
                                            &msg.payload,
                                        ) {
                                            Ok(p) => InvitationMeta::Onboarding(p),
                                            Err(e) => {
                                                tracing::warn!(
                                                    "Onboarding invite payload was unparseable: {e}"
                                                );
                                                InvitationMeta::None
                                            }
                                        }
                                    }
                                    InvitationType::Dars => {
                                        match serde_json::from_slice::<DarsInvitePayload>(
                                            &msg.payload,
                                        ) {
                                            Ok(p) => InvitationMeta::Dars(p),
                                            Err(e) => {
                                                tracing::warn!(
                                                    "Dars invite payload was unparseable: {e}"
                                                );
                                                InvitationMeta::None
                                            }
                                        }
                                    }
                                    InvitationType::Kick => {
                                        match serde_json::from_slice::<KickInvitePayload>(
                                            &msg.payload,
                                        ) {
                                            Ok(p) => InvitationMeta::Kick(p),
                                            Err(e) => {
                                                tracing::warn!(
                                                    "Kick invite payload was unparseable: {e}"
                                                );
                                                InvitationMeta::None
                                            }
                                        }
                                    }
                                    _ => InvitationMeta::None,
                                }
                            };
                            if let Some(ref pubkey) = peer_pubkey_hex {
                                triggers
                                    .record_invitation(invitation_type, pubkey, meta)
                                    .await;
                            }

                            let ack = Message::new_empty(MessageType::Ack);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(ack.to_bytes()))
                                .unwrap());
                        }
                        MessageType::CancelInvite => {
                            tracing::info!(
                                "Received CancelInvite, dropping pending invites + cancelling \
                                 any in-flight peer runs from sender"
                            );
                            if let Some(ref pubkey) = peer_pubkey_hex {
                                triggers.drop_invitations_from(pubkey).await;
                                triggers.cancel_peer_runs_from(pubkey).await;
                            }
                            let ack = Message::new_empty(MessageType::Ack);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(ack.to_bytes()))
                                .unwrap());
                        }
                        MessageType::RetryWorkflow => {
                            tracing::info!(
                                "Received RetryWorkflow, retrying any Failed peer runs from \
                                 sender"
                            );
                            if let Some(ref pubkey) = peer_pubkey_hex {
                                triggers.retry_peer_runs_from(pubkey).await;
                            }
                            let ack = Message::new_empty(MessageType::Ack);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(ack.to_bytes()))
                                .unwrap());
                        }
                        _ => {
                            tracing::debug!("Ignoring message type {:?}", msg.msg_type);
                        }
                    }
                }

                Ok(Response::new(Body::empty()))
            }
        },
        Some(Duration::from_secs(5)),
    )
    .await;

    match result {
        Ok(()) => {
            tracing::debug!("Connection from {peer_addr} handled successfully");
        }
        Err(e) => {
            tracing::debug!("Noise connection from {peer_addr} failed: {e}");
        }
    }
}

/// Ping peers every 5 seconds
async fn run_peer_ping_loop(
    config: NodeConfig,
    db: SqlitePool,
    peer_status: Arc<RwLock<HashMap<String, bool>>>,
    last_seen: LastSeen,
    keypair: Arc<NoiseKeypair>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        let peers = match db.get_all_peers().await {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!("Failed to load peers from database for heartbeat: {e}");
                continue;
            }
        };

        let our_canton_id = config.participant_id();
        let our_id = our_canton_id.to_string();
        let futures: Vec<_> = peers
            .iter()
            .filter(|p| &p.participant_id != our_canton_id)
            .map(|peer| {
                let id = peer.participant_id.to_string();
                let address = peer.address.clone();
                let port = peer.port;
                let public_key_hex = peer.public_key.clone();
                let last_seen = last_seen.clone();
                let keypair = keypair.clone();
                let our_id = our_id.clone();

                async move {
                    // Parse the peer's pubkey from the freshly-loaded DB row.
                    // (Cannot rely on a startup-only cache: new peers must be probed.)
                    let peer_pubkey = match parse_public_key(&public_key_hex) {
                        Ok(pk) => pk,
                        Err(e) => {
                            tracing::debug!("Skipping probe of {id}: malformed public_key: {e}");
                            return (id, false);
                        }
                    };

                    let now = std::time::Instant::now();
                    let stale = {
                        let map = last_seen.read().await;
                        peer_status::should_probe(&map, &id, now)
                    };

                    if !stale {
                        return (id, true);
                    }

                    let psk = keypair.derive_psk(&peer_pubkey);
                    let result = crate::noise::send_noise_message(
                        &address,
                        port,
                        &psk,
                        our_id.as_bytes(),
                        &Message::new_empty(MessageType::Ping),
                    )
                    .await;

                    let active = matches!(
                        result,
                        Ok(ref bytes) if Message::from_bytes(bytes)
                            .map(|m| m.msg_type == MessageType::Pong)
                            .unwrap_or(false)
                    );

                    if active {
                        let now = std::time::Instant::now();
                        let mut map = last_seen.write().await;
                        peer_status::bump(&mut map, id.clone(), now);
                    }

                    (id, active)
                }
            })
            .collect();

        let results = futures::future::join_all(futures).await;

        let mut status_map = peer_status.write().await;
        for (id, active) in results {
            status_map.insert(id, active);
        }
    }
}

/// Take the peer-side `instance_name` slot and flip the matching
/// `workflow_runs` row to a terminal status. No-op if no slot was set (the
/// peer row creation may have failed; we don't want to over-write
/// unrelated state).
async fn finalize_peer_run(
    db: &SqlitePool,
    instance_slot: &Arc<RwLock<Option<String>>>,
    success: bool,
    error_msg: Option<String>,
) {
    let instance = {
        let mut slot = instance_slot.write().await;
        slot.take()
    };
    let Some(instance) = instance else { return };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut tx = match db.begin_transaction().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("finalize_peer_run: begin_transaction failed: {e}");
            return;
        }
    };
    let status = if success {
        WorkflowProgress::Completed
    } else {
        WorkflowProgress::Failed
    };
    if let Err(e) = tx
        .set_workflow_run_status(&instance, status, error_msg.as_deref(), now)
        .await
    {
        tracing::warn!("finalize_peer_run: update failed: {e}");
        return;
    }
    if let Err(e) = Commitable::commit(tx).await {
        tracing::warn!("finalize_peer_run: commit failed: {e}");
    }
}

/// Background task that starts onboarding peer workflow when triggered by an invite
#[allow(clippy::too_many_arguments)]
async fn run_onboarding_peer_listener(
    config: NodeConfig,
    db: SqlitePool,
    onboarding_state: web::Data<Arc<handlers::OnboardingWorkflowState>>,
    onboarding_trigger: Arc<Notify>,
    coordinator_pubkey: Arc<RwLock<Option<String>>>,
    peer_run_instance: Arc<RwLock<Option<String>>>,
) {
    loop {
        // Wait for trigger
        onboarding_trigger.notified().await;

        tracing::info!("Received onboarding invite, starting peer workflow...");

        // Check if already in progress
        {
            let status = onboarding_state.status.read().await;
            if *status == types::OnboardingStatus::InProgress {
                tracing::warn!("Already in onboarding workflow, ignoring invite");
                continue;
            }
        }

        // Get coordinator from stored public key
        let coordinator = {
            let pubkey_guard = coordinator_pubkey.read().await;
            let pubkey = match pubkey_guard.as_ref() {
                Some(pk) => pk.clone(),
                None => {
                    tracing::error!("No coordinator public key stored, cannot start peer");
                    continue;
                }
            };
            drop(pubkey_guard);

            // Look up coordinator in database by public key
            match db.get_peer_by_public_key(&pubkey).await {
                Ok(Some(peer)) => peer,
                Ok(None) => {
                    tracing::error!("Coordinator with pubkey {pubkey} not found in database");
                    continue;
                }
                Err(e) => {
                    tracing::error!("Failed to look up coordinator peer: {e}");
                    continue;
                }
            }
        };

        tracing::info!("Coordinator identified: {}", coordinator.participant_id);

        // Resolve peer instance BEFORE flipping status (see DARs handler
        // below for the rationale: peer_run_instance is shared across
        // kinds and a race can leave us with None, which used to leak
        // status=InProgress).
        let local_instance = match peer_run_instance.read().await.clone() {
            Some(inst) => inst,
            None => {
                tracing::error!("Peer trigger fired without an peer_run_instance; skipping run");
                continue;
            }
        };

        // Update status
        {
            let mut status = onboarding_state.status.write().await;
            *status = types::OnboardingStatus::InProgress;
            let mut error = onboarding_state.error.write().await;
            *error = None;
        }

        let workflow_config = config.clone();
        let result =
            workflow::start_peer(workflow_config, coordinator, db.clone(), local_instance).await;

        // Update status
        let mut status = onboarding_state.status.write().await;
        let mut error = onboarding_state.error.write().await;

        let success = result.is_ok();
        let err_msg = result.as_ref().err().map(|e| format!("{e}"));
        match result {
            Ok(()) => {
                *status = types::OnboardingStatus::Completed;
                tracing::info!("Onboarding peer workflow completed successfully");
            }
            Err(e) => {
                *status = types::OnboardingStatus::Failed;
                *error = Some(format!("{e}"));
                tracing::error!("Onboarding peer workflow failed: {e}");
            }
        }
        drop(status);
        drop(error);
        finalize_peer_run(&db, &peer_run_instance, success, err_msg).await;
    }
}

/// Background task that starts kick peer workflow when triggered by an invite
#[allow(clippy::too_many_arguments)]
async fn run_kick_peer_listener(
    config: NodeConfig,
    db: SqlitePool,
    kick_state: web::Data<Arc<handlers::KickWorkflowState>>,
    kick_trigger: Arc<Notify>,
    coordinator_pubkey: Arc<RwLock<Option<String>>>,
    peer_run_instance: Arc<RwLock<Option<String>>>,
) {
    loop {
        // Wait for trigger
        kick_trigger.notified().await;

        tracing::info!("Received kick invite, starting kick peer workflow...");

        // Check if already in progress
        {
            let status = kick_state.status.read().await;
            if *status == types::KickStatus::InProgress {
                tracing::warn!("Already in kick workflow, ignoring invite");
                continue;
            }
        }

        // Get coordinator from stored public key
        let coordinator = {
            let pubkey_guard = coordinator_pubkey.read().await;
            let pubkey = match pubkey_guard.as_ref() {
                Some(pk) => pk.clone(),
                None => {
                    tracing::error!("No coordinator public key stored, cannot start peer");
                    continue;
                }
            };
            drop(pubkey_guard);

            match db.get_peer_by_public_key(&pubkey).await {
                Ok(Some(peer)) => peer,
                Ok(None) => {
                    tracing::error!("Coordinator with pubkey {pubkey} not found in database");
                    continue;
                }
                Err(e) => {
                    tracing::error!("Failed to look up coordinator peer: {e}");
                    continue;
                }
            }
        };

        tracing::info!("Coordinator identified: {}", coordinator.participant_id);

        // Resolve peer instance BEFORE flipping status (see DARs handler
        // below for rationale).
        let local_instance = match peer_run_instance.read().await.clone() {
            Some(inst) => inst,
            None => {
                tracing::error!(
                    "Kick peer trigger fired without an peer_run_instance; skipping run"
                );
                continue;
            }
        };

        // Update status
        {
            let mut status = kick_state.status.write().await;
            *status = types::KickStatus::InProgress;
            let mut error = kick_state.error.write().await;
            *error = None;
        }

        let workflow_config = config.clone();
        let result =
            workflow::start_peer(workflow_config, coordinator, db.clone(), local_instance).await;

        // Update status
        let mut status = kick_state.status.write().await;
        let mut error = kick_state.error.write().await;

        let success = result.is_ok();
        let err_msg = result.as_ref().err().map(|e| format!("{e}"));
        match result {
            Ok(()) => {
                *status = types::KickStatus::Completed;
                tracing::info!("Kick peer workflow completed successfully");
            }
            Err(e) => {
                *status = types::KickStatus::Failed;
                *error = Some(format!("{e}"));
                tracing::error!("Kick peer workflow failed: {e}");
            }
        }
        drop(status);
        drop(error);
        finalize_peer_run(&db, &peer_run_instance, success, err_msg).await;
    }
}

/// Background task that starts contracts peer workflow when triggered by an invite
#[allow(clippy::too_many_arguments)]
async fn run_contracts_peer_listener(
    config: NodeConfig,
    db: SqlitePool,
    contracts_state: web::Data<Arc<handlers::ContractsWorkflowState>>,
    contracts_trigger: Arc<Notify>,
    coordinator_pubkey: Arc<RwLock<Option<String>>>,
    peer_run_instance: Arc<RwLock<Option<String>>>,
) {
    loop {
        // Wait for trigger
        contracts_trigger.notified().await;

        tracing::info!("Received contracts invite, starting contracts peer workflow...");

        // Check if already in progress
        {
            let status = contracts_state.status.read().await;
            if *status == types::WorkflowProgress::InProgress {
                tracing::warn!("Already in contracts workflow, ignoring invite");
                continue;
            }
        }

        // Get coordinator from stored public key
        let coordinator = {
            let pubkey_guard = coordinator_pubkey.read().await;
            let pubkey = match pubkey_guard.as_ref() {
                Some(pk) => pk.clone(),
                None => {
                    tracing::error!("No coordinator public key stored, cannot start peer");
                    continue;
                }
            };
            drop(pubkey_guard);

            match db.get_peer_by_public_key(&pubkey).await {
                Ok(Some(peer)) => peer,
                Ok(None) => {
                    tracing::error!("Coordinator with pubkey {pubkey} not found in database");
                    continue;
                }
                Err(e) => {
                    tracing::error!("Failed to look up coordinator peer: {e}");
                    continue;
                }
            }
        };

        tracing::info!("Coordinator identified: {}", coordinator.participant_id);

        // Resolve peer instance BEFORE flipping status (see DARs handler
        // below for rationale).
        let local_instance = match peer_run_instance.read().await.clone() {
            Some(inst) => inst,
            None => {
                tracing::error!(
                    "Contracts peer trigger fired without an peer_run_instance; skipping run"
                );
                continue;
            }
        };

        // Update status
        {
            let mut status = contracts_state.status.write().await;
            *status = types::WorkflowProgress::InProgress;
            let mut error = contracts_state.error.write().await;
            *error = None;
        }

        let workflow_config = config.clone();
        let result =
            workflow::start_peer(workflow_config, coordinator, db.clone(), local_instance).await;

        // Update status
        let mut status = contracts_state.status.write().await;
        let mut error = contracts_state.error.write().await;

        let success = result.is_ok();
        let err_msg = result.as_ref().err().map(|e| format!("{e}"));
        match result {
            Ok(()) => {
                *status = types::WorkflowProgress::Completed;
                tracing::info!("Contracts peer workflow completed successfully");
            }
            Err(e) => {
                *status = types::WorkflowProgress::Failed;
                *error = Some(format!("{e}"));
                tracing::error!("Contracts peer workflow failed: {e}");
            }
        }
        drop(status);
        drop(error);
        finalize_peer_run(&db, &peer_run_instance, success, err_msg).await;
    }
}

/// Background task that starts DARs peer workflow when triggered by an invite
#[allow(clippy::too_many_arguments)]
async fn run_dars_peer_listener(
    config: NodeConfig,
    db: SqlitePool,
    dars_state: web::Data<Arc<handlers::DarsWorkflowState>>,
    dars_trigger: Arc<Notify>,
    coordinator_pubkey: Arc<RwLock<Option<String>>>,
    peer_run_instance: Arc<RwLock<Option<String>>>,
) {
    loop {
        // Wait for trigger
        dars_trigger.notified().await;

        tracing::info!("Received DARs invite, starting DARs peer workflow...");

        // Check if already in progress
        {
            let status = dars_state.status.read().await;
            if *status == types::WorkflowProgress::InProgress {
                tracing::warn!("Already in DARs workflow, ignoring invite");
                continue;
            }
        }

        // Get coordinator from stored public key
        let coordinator = {
            let pubkey_guard = coordinator_pubkey.read().await;
            let pubkey = match pubkey_guard.as_ref() {
                Some(pk) => pk.clone(),
                None => {
                    tracing::error!("No coordinator public key stored, cannot start peer");
                    continue;
                }
            };
            drop(pubkey_guard);

            match db.get_peer_by_public_key(&pubkey).await {
                Ok(Some(peer)) => peer,
                Ok(None) => {
                    tracing::error!("Coordinator with pubkey {pubkey} not found in database");
                    continue;
                }
                Err(e) => {
                    tracing::error!("Failed to look up coordinator peer: {e}");
                    continue;
                }
            }
        };

        tracing::info!("Coordinator identified: {}", coordinator.participant_id);

        // Resolve the peer instance BEFORE flipping status to InProgress.
        // peer_run_instance is shared across all four workflow kinds, so a
        // race with another kind's accept_invitation can leave it None when
        // our trigger fires (or pointing at the wrong kind's instance, in
        // which case start_peer will return an error and we set Failed
        // — that's recoverable). Setting status=InProgress and *then*
        // bailing on a missing instance leaks status pinned to InProgress
        // until something else flips it: the next /dars/distribute observes
        // it and 409s.
        let local_instance = match peer_run_instance.read().await.clone() {
            Some(inst) => inst,
            None => {
                tracing::error!(
                    "DARs peer trigger fired without an peer_run_instance; skipping run"
                );
                continue;
            }
        };

        // Update status
        {
            let mut status = dars_state.status.write().await;
            *status = types::WorkflowProgress::InProgress;
            let mut error = dars_state.error.write().await;
            *error = None;
        }

        let workflow_config = config.clone();
        let result =
            workflow::start_peer(workflow_config, coordinator, db.clone(), local_instance).await;

        // Update status
        let mut status = dars_state.status.write().await;
        let mut error = dars_state.error.write().await;

        let success = result.is_ok();
        let err_msg = result.as_ref().err().map(|e| format!("{e}"));
        match result {
            Ok(()) => {
                *status = types::WorkflowProgress::Completed;
                tracing::info!("DARs peer workflow completed successfully");
            }
            Err(e) => {
                *status = types::WorkflowProgress::Failed;
                *error = Some(format!("{e}"));
                tracing::error!("DARs peer workflow failed: {e}");
            }
        }
        drop(status);
        drop(error);
        finalize_peer_run(&db, &peer_run_instance, success, err_msg).await;
    }
}

/// Query Canton for this node's owner keys across a caller-supplied set of
/// decentralized parties. Returns JSON:
/// `[{"party_id": "prefix::namespace", "owner_key": "fingerprint"}, ...]`.
///
/// `requested_party_ids` is the list of parties the caller cares about, sent
/// in the Noise `RequestOwnerKeys` payload by `resolve_owner_keys_from_peers`.
/// Building the `namespace → party_id` map directly from this list lets us
/// skip the unfiltered `list_party_to_participant` call that previously
/// scanned every party on the synchronizer — that scan does not complete
/// within the Noise budget against a kubectl-tunneled Canton admin API on
/// devnet (~170 parties, never returns within 60s; see #149).
async fn list_my_owner_keys(
    config: &NodeConfig,
    requested_party_ids: &[String],
) -> Result<Vec<u8>> {
    if requested_party_ids.is_empty() {
        return Ok(b"[]".to_vec());
    }

    // Caller provides full party_ids ("prefix::namespace_fingerprint"). The
    // namespace is the suffix after the final "::"; we use it to look up the
    // matching `DecentralizedNamespaceDefinition` entry. Party IDs without
    // "::" are dropped (malformed; nothing we could match against).
    let namespace_to_party: HashMap<String, &str> = requested_party_ids
        .iter()
        .filter_map(|pid| {
            pid.rsplit_once("::")
                .map(|(_, ns)| (ns.to_string(), pid.as_str()))
        })
        .collect();
    if namespace_to_party.is_empty() {
        return Ok(b"[]".to_vec());
    }

    let channel = tonic::transport::Channel::from_shared(config.admin_api_url())?
        .connect()
        .await?;

    let mut vault_client = VaultServiceClient::new(channel.clone());
    let mut topology_client = TopologyManagerReadServiceClient::new(channel);

    // Get this node's namespace key fingerprints
    let keys_response = vault_client
        .list_my_keys(tonic::Request::new(ListMyKeysRequest { filters: None }))
        .await?
        .into_inner();

    let mut my_fingerprints = Vec::new();
    for key_meta in keys_response.private_keys_metadata {
        if let Some(pub_key_with_name) = &key_meta.public_key_with_name
            && let Some(pub_key) = &pub_key_with_name.public_key
            && let Some(public_key::Key::SigningPublicKey(signing_key)) = &pub_key.key
            && signing_key.usage.contains(&1)
        {
            my_fingerprints.push(compute_fingerprint(signing_key));
        }
    }

    // Cached after first call (`get_synchronizer_id` memoises via OnceCell).
    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    let base_query = BaseQuery {
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id)),
            })),
        }),
        proposals: false,
        operation: 0,
        time_query: Some(base_query::TimeQuery::HeadState(())),
        filter_signed_key: String::new(),
        protocol_version: None,
    };

    // Get all decentralized namespace definitions. The response is bounded by
    // the number of decentralized parties allocated on the synchronizer
    // (49 entries observed on devnet — completes in <1s).
    let dns_response = topology_client
        .list_decentralized_namespace_definition(tonic::Request::new(
            ListDecentralizedNamespaceDefinitionRequest {
                base_query: Some(base_query),
                filter_namespace: String::new(),
            },
        ))
        .await?
        .into_inner();

    // Match this node's fingerprints against each party's owners list, but
    // only for namespaces the caller asked about.
    let mut entries = Vec::new();
    for result in dns_response.results {
        let Some(item) = result.item else { continue };
        let Some(full_party_id) = namespace_to_party.get(&item.decentralized_namespace) else {
            continue;
        };
        for owner in &item.owners {
            if my_fingerprints.contains(owner) {
                entries.push(serde_json::json!({
                    "party_id": full_party_id,
                    "owner_key": owner,
                }));
            }
        }
    }

    Ok(serde_json::to_vec(&entries)?)
}

async fn list_local_packages(admin_api_url: &str) -> Result<Vec<u8>> {
    let mut client = PackageServiceClient::connect(admin_api_url.to_string()).await?;
    let response = client
        .list_packages(tonic::Request::new(ListPackagesRequest {
            limit: 0,
            filter_name: String::new(),
        }))
        .await?
        .into_inner();

    let packages: Vec<serde_json::Value> = response
        .package_descriptions
        .into_iter()
        .map(|p| {
            serde_json::json!({
                "package_id": p.package_id,
                "name": p.name,
                "version": p.version,
            })
        })
        .collect();

    Ok(serde_json::to_vec(&packages)?)
}
