//! HTTP server and always-on Noise listener.
//!
//! Builds the actix-web application (REST API + embedded React UI; a Swagger UI
//! is mounted only in test/dev builds, i.e. `cfg!(any(test, feature =
//! "test-mode"))`), wires shared [`AppState`], and runs the long-lived Noise
//! listener that
//! handles inbound peer messages (invites, signing, health, cancellation)
//! independently of any coordinator-driven workflow.

mod action_serializer;
mod assets;
mod audit;
mod chain_audit;
mod handlers;
mod middleware;
mod package_inventory;
mod queries;
mod transfer_context;
mod types;

pub mod health;
pub mod peer_status;

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
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
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio_noise::handshakes::nn_psk2::Responder;
use utoipa_actix_web::AppExt;
use utoipa_swagger_ui::SwaggerUi;

#[cfg(not(any(test, feature = "test-mode")))]
use crate::auth::{AuthRegistry, JwtValidator};
#[cfg(any(test, feature = "test-mode"))]
use crate::auth::{MockAuthRegistry, MockValidator};
use crate::{
    auth::{TokenValidator, WorkflowAuth},
    canton_id::CantonId,
    config::{NodeConfig, PartyCredentials},
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
    noise::{
        CHUNK_SIZE, MAX_CHUNKED_TOTAL_SIZE, MAX_PAYLOAD_SIZE, Message, MessageType,
        NOISE_HANDLER_TIMEOUT, NoiseKeypair, load_or_generate_keypair, parse_public_key,
    },
    server::middleware::AuthMiddleware,
    server::peer_status::LastSeen,
    utils::{self, compute_fingerprint},
    workflow::{self, WorkflowType},
};

pub use handlers::NodeConfigResponse;
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
    /// Single peer-job queue. `accept_invitation` (and the `RetryWorkflow`
    /// listener arm) enqueue a [`PeerJob`] carrying the run's `kind`,
    /// `instance_name`, and `coordinator_pubkey`; the peer listener owns the
    /// receiver and spawns one `workflow::start_peer` per job, so a node can
    /// participate as a peer in many concurrent workflows at once without
    /// racing over a shared slot.
    pub peer_job_sender: mpsc::UnboundedSender<PeerJob>,
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
    /// Every in-flight workflow this node owns, keyed by `instance_name`.
    /// Replaces the single-tenant per-kind `HttpWorkflowState` singletons, the
    /// global in-flight gate, and the single `active_workflow` routing slot:
    /// any number of runs (even of the same kind) run side-by-side. The
    /// always-on Noise listener routes a peer's command via
    /// `workflows.route(msg.instance)`, and `/workflows/{instance}/cancel`
    /// looks an entry up to abort its spawn.
    pub workflows: WorkflowRegistry,
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
    /// Every in-flight workflow this node owns, keyed by `instance_name`. The
    /// listener routes a peer's workflow-command (`Message::instance`) to the
    /// matching coordinator run via `workflows.route(..)`.
    workflows: WorkflowRegistry,
    /// Used by the `RetryWorkflow` listener arm to re-enqueue Failed peer runs
    /// as `PeerJob`s.
    peer_job_sender: mpsc::UnboundedSender<PeerJob>,
}

enum InvitationMeta {
    None,
    Onboarding(OnboardingInvitePayload),
    Dars(DarsInvitePayload),
    Kick(KickInvitePayload),
    Contracts(ContractsInvitePayload),
    AddParty(AddPartyInvitePayload),
    ChangeThreshold(ChangeThresholdInvitePayload),
}

/// On boot, re-spawn any InProgress workflow runs that were interrupted by the
/// last shutdown. The previous task handle died with the process, but the
/// state machine + artefacts survived in SQLite, so we can pick the run back
/// up at its persisted `current_step`.
///
/// Coordinator-side: each run is rebuilt as its own [`WorkflowInstance`] in the
/// registry (keyed by `instance_name`) and its `start_coordinator` task is
/// re-spawned. Because every coordinator run is now routed independently by the
/// always-on listener via `Message::instance`, all of them resume concurrently
/// — not just the newest, as the former single-slot model required.
///
/// Peer-side: a [`PeerJob`] is re-enqueued so the peer listener re-spins
/// `start_peer`. Limitation (unchanged): the peer pulls its instance_name out
/// of the GenerateKeys / SignSubmissions / SignKick command payload — if the
/// coordinator is past those steps the peer cannot rebind and the run surfaces
/// as Failed for the operator to dismiss.
async fn recover_in_progress_workflows(
    db: SqlitePool,
    config: NodeConfig,
    workflows: WorkflowRegistry,
    peer_job_sender: mpsc::UnboundedSender<PeerJob>,
    auth: Arc<RwLock<Option<WorkflowAuth>>>,
    last_seen: LastSeen,
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

    for run in &runs {
        match run.role {
            WorkflowRole::Coordinator => {
                respawn_coordinator(
                    db.clone(),
                    config.clone(),
                    run,
                    workflows.clone(),
                    auth.clone(),
                    last_seen.clone(),
                )
                .await;
            }
            WorkflowRole::Peer => refire_peer(run, &peer_job_sender),
        }
    }
}

/// Re-spawn a coordinator-side workflow that was running when the node stopped.
/// The original `workflow_runs` row stays in place; the spawned task uses
/// `WorkflowState::from_persisted` (via `NoiseServer::new`) to resume at
/// `current_step` instead of restarting from `WaitingForPeers`. A fresh
/// [`WorkflowInstance`] is registered so the always-on listener can route this
/// run's commands and `/workflows/{instance}/cancel` can abort it.
pub(crate) async fn respawn_coordinator(
    db: SqlitePool,
    config: NodeConfig,
    run: &WorkflowRun,
    workflows: WorkflowRegistry,
    auth: Arc<RwLock<Option<WorkflowAuth>>>,
    last_seen: LastSeen,
) {
    let instance_name = run.instance_name.clone();
    let kind = run.kind;
    tracing::info!(
        "Resuming {kind:?} coordinator run {instance_name} at step {} \
         ({} of {} peers completed)",
        run.current_step,
        run.completed_peers.len(),
        run.expected_peers.len()
    );

    // Parse the persisted config for this kind into the matching typed option.
    let mut onboarding_config = None;
    let mut kick_config = None;
    let mut contracts_config = None;
    let mut dars_config = None;
    let mut add_party_config = None;
    let mut change_threshold_config = None;
    let parsed = match kind {
        WorkflowKind::Onboarding => {
            serde_json::from_str::<workflow::OnboardingConfig>(&run.config_json)
                .map(|c| onboarding_config = Some(c))
        }
        WorkflowKind::Kick => serde_json::from_str::<workflow::KickConfig>(&run.config_json)
            .map(|c| kick_config = Some(c)),
        WorkflowKind::Contracts => {
            serde_json::from_str::<workflow::ContractsConfig>(&run.config_json)
                .map(|c| contracts_config = Some(c))
        }
        WorkflowKind::Dars => serde_json::from_str::<workflow::DarsConfig>(&run.config_json)
            .map(|c| dars_config = Some(c)),
        WorkflowKind::AddParty => {
            serde_json::from_str::<workflow::AddPartyConfig>(&run.config_json)
                .map(|c| add_party_config = Some(c))
        }
        WorkflowKind::ChangeThreshold => {
            serde_json::from_str::<workflow::ChangeThresholdConfig>(&run.config_json)
                .map(|c| change_threshold_config = Some(c))
        }
    };
    if let Err(e) = parsed {
        tracing::warn!("respawn_coordinator: bad {kind:?} config_json for {instance_name}: {e}");
        mark_failed_via_pool(&db, &instance_name, "Resume failed: invalid config").await;
        return;
    }

    let instance = WorkflowInstance::new(instance_name.clone(), kind, WorkflowRole::Coordinator);
    *instance.http.invited_peers.write().await = run.expected_peers.clone();
    *instance.http.status.write().await = WorkflowProgress::InProgress;
    if !workflows.insert(instance.clone()) {
        tracing::warn!("respawn_coordinator: {instance_name} already registered; skipping");
        return;
    }

    let auth_snapshot = auth.read().await.clone();
    let handle = spawn_coordinator_run(
        db,
        config,
        kind,
        instance.clone(),
        workflows,
        onboarding_config,
        kick_config,
        contracts_config,
        dars_config,
        add_party_config,
        change_threshold_config,
        auth_snapshot,
        last_seen,
    );
    *instance.http.abort_handle.lock().await = Some(handle.abort_handle());
}

/// Spawn the coordinator workflow task for `instance`: drive `start_coordinator`
/// to completion, reflect the terminal status onto the instance's HTTP state and
/// the persisted row, and remove the instance from the registry on return
/// (success, failure, or abort) via a [`WorkflowGuard`].
#[allow(clippy::too_many_arguments)]
fn spawn_coordinator_run(
    db: SqlitePool,
    config: NodeConfig,
    kind: WorkflowKind,
    instance: Arc<WorkflowInstance>,
    workflows: WorkflowRegistry,
    onboarding_config: Option<workflow::OnboardingConfig>,
    kick_config: Option<workflow::KickConfig>,
    contracts_config: Option<workflow::ContractsConfig>,
    dars_config: Option<workflow::DarsConfig>,
    add_party_config: Option<workflow::AddPartyConfig>,
    change_threshold_config: Option<workflow::ChangeThresholdConfig>,
    auth: Option<WorkflowAuth>,
    last_seen: LastSeen,
) -> tokio::task::JoinHandle<()> {
    let workflow_type = match kind {
        WorkflowKind::Onboarding => WorkflowType::Onboarding,
        WorkflowKind::Kick => WorkflowType::Kick,
        WorkflowKind::Contracts => WorkflowType::Contracts,
        WorkflowKind::Dars => WorkflowType::Dars,
        WorkflowKind::AddParty => WorkflowType::AddParty,
        WorkflowKind::ChangeThreshold => WorkflowType::ChangeThreshold,
    };
    tokio::spawn(async move {
        let instance_name = instance.instance_name.clone();
        // Removes the registry entry on return — including panic/abort — so a
        // finished run stops being routed to.
        let _guard = WorkflowGuard::new(workflows, instance_name.clone());
        let result = workflow::start_coordinator(
            config,
            db.clone(),
            workflow_type,
            onboarding_config,
            kick_config,
            contracts_config,
            dars_config,
            add_party_config,
            change_threshold_config,
            auth,
            last_seen,
            instance.clone(),
        )
        .await;

        match result {
            Ok(_) => {
                *instance.http.status.write().await = WorkflowProgress::Completed;
                tracing::info!("{kind:?} workflow {instance_name} completed");
                mark_completed_via_pool(&db, &instance_name).await;
            }
            Err(e) => {
                let msg = format!("{e}");
                {
                    let mut status = instance.http.status.write().await;
                    let mut error = instance.http.error.write().await;
                    *status = WorkflowProgress::Failed;
                    *error = Some(msg.clone());
                }
                tracing::error!("{kind:?} workflow {instance_name} failed: {e:#}");
                mark_failed_via_pool(&db, &instance_name, &msg).await;
            }
        }
    })
}

/// Re-enqueue a peer-side run as a [`PeerJob`] so the peer listener re-spins
/// `start_peer` against the persisted coordinator pubkey.
pub(crate) fn refire_peer(run: &WorkflowRun, peer_job_sender: &mpsc::UnboundedSender<PeerJob>) {
    let Some(pk) = run.coordinator_pubkey.clone() else {
        tracing::warn!(
            "Skipping peer recover for {}: no coordinator_pubkey persisted",
            run.instance_name
        );
        return;
    };
    let job = PeerJob {
        kind: run.kind,
        instance_name: run.instance_name.clone(),
        // Persisted at accept time (the invite's `workflow_instance`), so a
        // resumed peer routes its commands to the exact coordinator run instead
        // of relying on the sole-active fallback. Empty only for rows that
        // predate instance routing.
        coordinator_instance: run.coordinator_instance.clone().unwrap_or_default(),
        coordinator_pubkey: pk,
    };
    if peer_job_sender.send(job).is_err() {
        tracing::warn!(
            "Failed to re-enqueue {:?} peer job for resumed run {}: receiver dropped",
            run.kind,
            run.instance_name
        );
    } else {
        tracing::info!(
            "Re-enqueued {:?} peer job for resumed run {} (coordinator may be past the \
             config-bearing command — run will fail if so; remediation: dismiss and re-accept)",
            run.kind,
            run.instance_name
        );
    }
}

async fn mark_completed_via_pool(db: &SqlitePool, instance_name: &str) {
    if let Err(e) = set_run_status(db, instance_name, WorkflowProgress::Completed, None).await {
        tracing::warn!("Failed to mark resumed run {instance_name} completed: {e:#}");
    }
}

pub(crate) async fn mark_failed_via_pool(db: &SqlitePool, instance_name: &str, error: &str) {
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
        let mut new_participant = None;
        let mut new_threshold = None;
        let mut previous_threshold = None;
        let mut dec_party_id = None;
        let mut package_names = Vec::new();
        let mut workflow_instance = None;
        match meta {
            InvitationMeta::None => {}
            InvitationMeta::Onboarding(p) => {
                prefix = Some(p.prefix);
                participants = p.participants;
                new_threshold = p.threshold;
                workflow_instance = p.workflow_instance;
            }
            InvitationMeta::Dars(p) => {
                dar_filenames = p.dar_filenames;
                participants = p.participants;
                workflow_instance = p.workflow_instance;
            }
            InvitationMeta::Kick(p) => {
                kicked_participant = Some(p.kicked_participant);
                new_threshold = Some(p.new_threshold);
                previous_threshold = Some(p.previous_threshold);
                dec_party_id = Some(p.dec_party_id);
                participants = p.participants;
                workflow_instance = p.workflow_instance;
            }
            InvitationMeta::Contracts(p) => {
                dec_party_id = Some(p.dec_party_id);
                participants = p.participants;
                package_names = p.package_names;
                workflow_instance = p.workflow_instance;
            }
            InvitationMeta::AddParty(p) => {
                new_participant = Some(p.new_participant);
                new_threshold = Some(p.new_threshold);
                previous_threshold = Some(p.previous_threshold);
                dec_party_id = Some(p.dec_party_id);
                participants = p.participants;
                workflow_instance = p.workflow_instance;
            }
            InvitationMeta::ChangeThreshold(p) => {
                new_threshold = Some(p.new_threshold);
                previous_threshold = Some(p.previous_threshold);
                dec_party_id = Some(p.dec_party_id);
                participants = p.participants;
                workflow_instance = p.workflow_instance;
            }
        }
        // Key the id on the coordinator's run instance when available so a
        // NEW run's invite never silently morphs an older card in place — an
        // accept/decline racing the replacement then misses (404) instead of
        // acting on an invitation the user never saw. Re-sends of the SAME
        // run still dedup (same instance → same id). Invites from old
        // coordinators (no instance) keep the legacy type+pubkey id.
        let type_str = invitation_type.as_str().to_lowercase();
        let pubkey_short = &coordinator_pubkey[..16];
        let id = match &workflow_instance {
            Some(instance) => format!("{type_str}-{pubkey_short}-{instance}"),
            None => format!("{type_str}-{pubkey_short}"),
        };
        let invitation = PendingInvitation {
            id,
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
            new_participant,
            new_threshold,
            previous_threshold,
            dec_party_id,
            package_names,
            workflow_instance,
        };

        // Dedup by `id` (which includes the coordinator's run `instance`), not
        // by (type, coordinator): a coordinator can now run several workflows
        // of the same kind concurrently, so each distinct run must surface as
        // its own card. Only a re-send of the SAME run (same id) replaces the
        // existing card; `upsert_pending_invitation` is keyed on `id`, so the
        // upsert alone dedups same-run re-sends without dropping sibling runs.
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

        // Bound the per-coordinator backlog: invites are recorded
        // unconditionally now (no busy-gating), so a buggy or hostile —
        // though authenticated — peer could otherwise grow
        // pending_invitations without limit by inventing fresh instances.
        // Keep the newest MAX_PENDING_INVITES_PER_COORDINATOR per sender,
        // evicting the oldest.
        const MAX_PENDING_INVITES_PER_COORDINATOR: usize = 16;
        let coordinator = invitations
            .last()
            .map(|i| i.coordinator_pubkey.clone())
            .unwrap_or_default();
        let mut from_sender: Vec<(i64, String)> = invitations
            .iter()
            .filter(|i| i.coordinator_pubkey == coordinator)
            .map(|i| (i.received_at, i.id.clone()))
            .collect();
        if from_sender.len() > MAX_PENDING_INVITES_PER_COORDINATOR {
            from_sender.sort_by_key(|(at, _)| *at);
            let evict: Vec<String> = from_sender
                [..from_sender.len() - MAX_PENDING_INVITES_PER_COORDINATOR]
                .iter()
                .map(|(_, id)| id.clone())
                .collect();
            tracing::warn!(
                "Pending-invitation cap hit for coordinator {coordinator}: evicting {} \
                 oldest invite(s)",
                evict.len()
            );
            invitations.retain(|i| !evict.contains(&i.id));
            drop(invitations);
            match self.db.begin_transaction().await {
                Ok(mut tx) => {
                    let mut ok = true;
                    for id in &evict {
                        if let Err(e) = tx.delete_pending_invitation(id).await {
                            tracing::warn!("Failed to delete evicted invitation {id}: {e}");
                            ok = false;
                            break;
                        }
                    }
                    if ok && let Err(e) = Commitable::commit(tx).await {
                        tracing::warn!("Failed to commit invitation eviction: {e}");
                    }
                }
                Err(e) => tracing::warn!("Failed to begin tx for invitation eviction: {e}"),
            }
        }
    }

    /// Drop pending invitations from `coordinator_pubkey`. When `instance` is
    /// non-empty (the CancelInvite was stamped with the cancelled run's
    /// `instance_name`), drop only the invitation(s) for that run so a sibling
    /// concurrent run's invite survives. An empty `instance` (legacy
    /// coordinator) keeps the old drop-everything-from-sender behaviour.
    async fn drop_invitations_from(&self, coordinator_pubkey: &str, instance: &str) {
        let mut invitations = self.pending_invitations.write().await;
        let matches = |i: &PendingInvitation| {
            i.coordinator_pubkey == coordinator_pubkey
                && (instance.is_empty() || i.workflow_instance.as_deref() == Some(instance))
        };
        let dropped_ids: Vec<String> = invitations
            .iter()
            .filter(|i| matches(i))
            .map(|i| i.id.clone())
            .collect();
        invitations.retain(|i| !matches(i));
        drop(invitations);

        match self.db.begin_transaction().await {
            Ok(mut tx) => {
                let result = if instance.is_empty() {
                    tx.delete_pending_invitations_by_coordinator(coordinator_pubkey)
                        .await
                } else {
                    let mut res = Ok(());
                    for id in &dropped_ids {
                        if let Err(e) = tx.delete_pending_invitation(id).await {
                            res = Err(e);
                            break;
                        }
                    }
                    res
                };
                if let Err(e) = result {
                    tracing::warn!("Failed to delete persisted invitations: {e}");
                } else if let Err(e) = Commitable::commit(tx).await {
                    tracing::warn!("Failed to commit invitation deletion: {e}");
                }
            }
            Err(e) => tracing::warn!("Failed to begin tx for invitation deletion: {e}"),
        }
    }

    /// Cancel peer-side workflow_runs whose coordinator matches the sender of
    /// a CancelInvite. Same authority — the coordinator who started the
    /// workflow is also the one who's allowed to abort it. Used by the
    /// CancelInvite listener arm so a single message covers both un-accepted
    /// invites AND accepted-but-running runs. When `instance` is non-empty,
    /// only the run(s) belonging to that coordinator run are cancelled —
    /// sibling concurrent runs from the same coordinator keep going; empty
    /// (legacy sender) cancels everything from the sender as before.
    async fn cancel_peer_runs_from(&self, coordinator_pubkey: &str, instance: &str) {
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
                && (instance.is_empty() || r.coordinator_instance.as_deref() == Some(instance))
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

    /// Coordinator-initiated retry: find Failed peer rows whose
    /// `coordinator_pubkey` matches the sender, flip them back to InProgress,
    /// and re-enqueue their peer jobs. Same authority model as
    /// `cancel_peer_runs_from` — the coordinator who started the run is also
    /// the one allowed to retry it. When `instance` is non-empty, only the
    /// run(s) belonging to that coordinator run are retried; empty (legacy
    /// sender) retries every Failed run from the sender as before.
    async fn retry_peer_runs_from(&self, coordinator_pubkey: &str, instance: &str) {
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
                && (instance.is_empty() || r.coordinator_instance.as_deref() == Some(instance))
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
            refire_peer(&run, &self.peer_job_sender);
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
    let (peer_job_sender, peer_job_receiver) = mpsc::unbounded_channel::<PeerJob>();
    let workflows = WorkflowRegistry::new();
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

    let app_state = web::Data::new(AppState {
        db: db.clone(),
        config: config.clone(),
        peer_status: peer_status.clone(),
        last_seen: last_seen.clone(),
        peer_job_sender: peer_job_sender.clone(),
        pending_invitations: pending_invitations.clone(),
        auth: auth.clone(),
        token_validator,
        admin_role,
        party_credentials: party_credentials.clone(),
        bootstrap_mu: Arc::new(Mutex::new(())),
        workflows: workflows.clone(),
        test_mode,
        refreshing_prefixes: Arc::new(RwLock::new(HashSet::new())),
        http_client,
    });

    // Boot-time workflow recovery. For any `workflow_runs` row that was
    // InProgress when we shut down, re-spawn the coordinator task (which
    // resumes at the persisted `current_step` via `WorkflowState::from_persisted`)
    // or re-enqueue the peer job so the peer listener picks the run back up.
    recover_in_progress_workflows(
        db.clone(),
        config.clone(),
        workflows.clone(),
        peer_job_sender.clone(),
        app_state.auth.clone(),
        last_seen.clone(),
    )
    .await;

    // Start heartbeat background task (pings peers, listens for invites, and
    // routes workflow commands to the matching coordinator run).
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
        workflows: workflows.clone(),
        peer_job_sender: peer_job_sender.clone(),
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

    // Single peer-job listener: drains the queue and spawns one
    // `workflow::start_peer` per accepted / retried / resumed invite, so this
    // node can be a peer in many concurrent workflows at once.
    let peer_listener_config = config.clone();
    let peer_listener_db = db.clone();
    let peer_listener_auth = auth.clone();
    tokio::spawn(async move {
        run_peer_listener(
            peer_listener_config,
            peer_listener_db,
            peer_listener_auth,
            peer_job_receiver,
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
            .service(handlers::healthz)
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
            .service(handlers::start_add_party)
            .service(handlers::get_add_party_status)
            .service(handlers::cancel_add_party)
            .service(handlers::start_change_threshold)
            .service(handlers::get_change_threshold_status)
            .service(handlers::cancel_change_threshold)
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
            .service(handlers::cancel_workflow_instance)
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
            .service(handlers::get_credential_offers_handler)
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
    // Surface our own Noise public key at startup so operators can confirm the
    // running node is using the key it published to peers. A mismatch fails
    // every handshake symmetrically and is otherwise invisible in the logs.
    tracing::info!("Noise public key: {key}", key = keypair.public_key_hex());

    // Peer keys for inbound authentication are resolved LIVE from the DB on
    // each incoming connection (see `handle_incoming_connection`) rather than
    // snapshotted here. A frozen snapshot meant that after a `/network-config`
    // key correction the node would accept a peer *outbound* (which reads the
    // live DB) but keep rejecting it *inbound* until a process restart —
    // producing asymmetric red/green across the mesh. Resolving live makes a
    // key fix take effect for both directions immediately.
    let self_id = config.participant_id().clone();

    // Listener loop: bind the always-on Noise listener and accept forever. It is
    // never paused — workflow traffic is routed in-process via the
    // active-workflow slot, so the listener stays up (and keeps answering
    // Health / Ping) even while this node is participating in a workflow.
    let keypair_spawn = keypair.clone();
    let last_seen_spawn = last_seen.clone();
    let db_spawn = db.clone();
    let self_id_spawn = self_id.clone();
    let triggers_spawn = triggers.clone();

    tokio::spawn(async move {
        loop {
            match TcpListener::bind(&listen_addr).await {
                Ok(listener) => {
                    tracing::info!("Noise listener started on {listen_addr}");

                    loop {
                        match listener.accept().await {
                            Ok((socket, peer_addr)) => {
                                let keypair = keypair_spawn.clone();
                                let last_seen = last_seen_spawn.clone();
                                let db = db_spawn.clone();
                                let self_id = self_id_spawn.clone();
                                let triggers = triggers_spawn.clone();

                                tokio::spawn(async move {
                                    handle_incoming_connection(
                                        socket, peer_addr, keypair, db, self_id, triggers,
                                        last_seen,
                                    )
                                    .await;
                                });
                            }
                            Err(e) => {
                                // Don't tight-loop on persistent accept errors (e.g. FD
                                // exhaustion): log and back off briefly before retrying.
                                tracing::warn!(
                                    "Noise listener accept error on {listen_addr}: {e}; \
                                     backing off 100ms"
                                );
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
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

/// Build the identity → public-key allowlist used to authenticate inbound
/// Noise handshakes, from the current `peers` table. Skips self and any peer
/// whose public key is empty or unparseable (the latter is logged so a bad
/// key is visible rather than silently dropped). Read fresh per connection so
/// `/network-config` updates take effect without a restart; a DB error yields
/// an empty allowlist (all inbound handshakes fail closed) rather than a panic.
async fn build_peer_key_map(
    db: &SqlitePool,
    self_id: &CantonId,
) -> HashMap<String, secp256k1::PublicKey> {
    let peers = match db.get_all_peers().await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Failed to load peers for inbound Noise auth: {e}");
            return HashMap::new();
        }
    };
    let mut map = HashMap::new();
    for peer in &peers {
        if peer.participant_id == *self_id || peer.public_key.is_empty() {
            continue;
        }
        match parse_public_key(&peer.public_key) {
            Ok(pub_key) => {
                map.insert(peer.participant_id.to_string(), pub_key);
            }
            Err(e) => tracing::warn!(
                "Skipping inbound auth key for {} — unparseable public key: {e}",
                peer.participant_id
            ),
        }
    }
    map
}

/// Handle an incoming Noise connection (either ping or invite)
async fn handle_incoming_connection(
    socket: tokio::net::TcpStream,
    peer_addr: std::net::SocketAddr,
    keypair: Arc<NoiseKeypair>,
    db: SqlitePool,
    self_id: CantonId,
    triggers: WorkflowTriggers,
    last_seen: LastSeen,
) {
    // Resolve the identity→public-key allowlist LIVE from the DB for this
    // connection so a `/network-config` key correction authenticates inbound
    // handshakes immediately, without a process restart (see `run_heartbeat`).
    let peer_keys = Arc::new(build_peer_key_map(&db, &self_id).await);
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

                // Parse the frame exactly once: this is the always-on listener
                // hot path (5s heartbeats per peer + all workflow/chunk traffic),
                // so re-decoding would needlessly re-allocate the payload Vec and
                // the instance String on every request. Deny anything we can't
                // parse EXPLICITLY — most importantly frames from pre-0.1.9
                // builds, whose wire format predates the version byte. An old
                // coordinator inviting this node lands in the Err arm: warn with
                // the sender's identity so the operator can see who needs
                // upgrading, and answer 503 so the old sender's call fails fast
                // instead of silently vanishing.
                let msg = match Message::from_bytes(&body_bytes) {
                    Ok(msg) => msg,
                    Err(e) => {
                        tracing::warn!(
                            "Denied Noise request from {sender}: {e}",
                            sender = peer_id_str.as_deref().unwrap_or("<unidentified peer>")
                        );
                        let mut resp = Response::new(Body::empty());
                        *resp.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                        return Ok(resp);
                    }
                };

                {
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
                            // GetChunk serves BOTH the chunked ListPackages transfer and a
                            // workflow's chunked command. The routing key disambiguates:
                            // workflow chunk requests are ALWAYS stamped with their
                            // coordinator run's instance (NoiseClient stamps every command),
                            // so a keyed GetChunk routes to exactly that run — and an
                            // EMPTY key means a ListPackages transfer and goes straight to
                            // the chunk cache below. No sole-active fallback here: with
                            // concurrent workflows a node is routinely mid-run while a peer
                            // fetches packages, and falling back would feed the
                            // ListPackages transfer INTO a live workflow's chunk server.
                            if !msg.instance.is_empty() {
                                let active = triggers.workflows.route(&msg.instance);
                                let peer =
                                    peer_id_str.as_deref().and_then(|s| CantonId::parse(s).ok());
                                let resp = match (active, peer) {
                                    (Some(wf), Some(pid)) => {
                                        wf.handle_command(pid, msg).await.unwrap_or_else(|e| {
                                            Message::new(
                                                MessageType::Error,
                                                format!("{e}").into_bytes(),
                                            )
                                        })
                                    }
                                    (None, _) => {
                                        // The named run is gone (cancelled/finished) —
                                        // 503 so the peer's bounded retry gives up.
                                        let mut resp = Response::new(Body::empty());
                                        *resp.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                                        return Ok(resp);
                                    }
                                    (Some(_), None) => Message::new_empty(MessageType::Wait),
                                };
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
                        | MessageType::AddPartyKeysUpload
                        | MessageType::AddPartySignatures
                        | MessageType::AddPartyClearSignatures
                        | MessageType::AddPartyClearProposal
                        | MessageType::ChangeThresholdSignatures
                        | MessageType::StatusUpdate
                        | MessageType::DeclineInvitation => {
                            // Route workflow-command traffic to the coordinator
                            // run identified by `msg.instance` (the peer stamps
                            // its coordinator's run name). The registry lock is
                            // held only long enough to clone the handle out —
                            // never across the await.
                            let active = triggers.workflows.route(&msg.instance);
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
                                (None, _) => {
                                    // No matching coordinator run on this node. The peer is
                                    // resuming a run whose coordinator workflow is gone
                                    // (cancelled, dismissed, never resumed), or its routing
                                    // key matched nothing. Replying Wait would make it poll
                                    // forever, leaving its run InProgress and the node
                                    // perpetually "busy" to invite/pre-flight checks. Return
                                    // a non-success status so the peer's bounded retry (3
                                    // strikes) gives up and finalizes the run. A coordinator
                                    // merely busy on a slow step keeps its registry entry and
                                    // still returns Wait via handle_command above, so this
                                    // fires only when no matching run exists.
                                    let mut resp = Response::new(Body::empty());
                                    *resp.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                                    return Ok(resp);
                                }
                                (Some(_), None) => Message::new_empty(MessageType::Wait),
                            };
                            return Ok(Response::new(Body::from(resp.to_bytes())));
                        }
                        MessageType::InviteOnboarding
                        | MessageType::InviteKick
                        | MessageType::InviteContracts
                        | MessageType::InviteDars
                        | MessageType::InviteAddParty
                        | MessageType::InviteChangeThreshold => {
                            // Invites are accepted unconditionally: workflows are
                            // multi-instance now, so a node already coordinating or
                            // participating in runs can take part in more
                            // concurrently. Each invite surfaces as its own pending
                            // card (deduped by id, which carries the coordinator's
                            // run instance); the old refuse-while-busy gate was a
                            // one-workflow-at-a-time leftover.
                            let invitation_type = match msg.msg_type {
                                MessageType::InviteOnboarding => InvitationType::Onboarding,
                                MessageType::InviteKick => InvitationType::Kick,
                                MessageType::InviteContracts => InvitationType::Contracts,
                                MessageType::InviteDars => InvitationType::Dars,
                                MessageType::InviteAddParty => InvitationType::AddParty,
                                MessageType::InviteChangeThreshold => {
                                    InvitationType::ChangeThreshold
                                }
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
                                    InvitationType::Contracts => {
                                        match serde_json::from_slice::<ContractsInvitePayload>(
                                            &msg.payload,
                                        ) {
                                            Ok(p) => InvitationMeta::Contracts(p),
                                            Err(e) => {
                                                tracing::warn!(
                                                    "Contracts invite payload was unparseable: {e}"
                                                );
                                                InvitationMeta::None
                                            }
                                        }
                                    }
                                    InvitationType::AddParty => {
                                        match serde_json::from_slice::<AddPartyInvitePayload>(
                                            &msg.payload,
                                        ) {
                                            Ok(p) => InvitationMeta::AddParty(p),
                                            Err(e) => {
                                                tracing::warn!(
                                                    "Add-party invite payload was unparseable: {e}"
                                                );
                                                InvitationMeta::None
                                            }
                                        }
                                    }
                                    InvitationType::ChangeThreshold => {
                                        match serde_json::from_slice::<ChangeThresholdInvitePayload>(
                                            &msg.payload,
                                        ) {
                                            Ok(p) => InvitationMeta::ChangeThreshold(p),
                                            Err(e) => {
                                                tracing::warn!(
                                                    "Change-threshold invite payload was unparseable: {e}"
                                                );
                                                InvitationMeta::None
                                            }
                                        }
                                    }
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
                            // `msg.instance` scopes the cancel to one coordinator
                            // run; empty (legacy sender) cancels everything from
                            // the sender.
                            tracing::info!(
                                "Received CancelInvite (instance: {:?}), dropping pending \
                                 invites + cancelling matching in-flight peer runs from sender",
                                msg.instance
                            );
                            if let Some(ref pubkey) = peer_pubkey_hex {
                                triggers.drop_invitations_from(pubkey, &msg.instance).await;
                                triggers.cancel_peer_runs_from(pubkey, &msg.instance).await;
                            }
                            let ack = Message::new_empty(MessageType::Ack);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(ack.to_bytes()))
                                .unwrap());
                        }
                        MessageType::RetryWorkflow => {
                            // `msg.instance` scopes the retry to one coordinator
                            // run; empty (legacy sender) retries every Failed run
                            // from the sender.
                            tracing::info!(
                                "Received RetryWorkflow (instance: {:?}), retrying matching \
                                 Failed peer runs from sender",
                                msg.instance
                            );
                            if let Some(ref pubkey) = peer_pubkey_hex {
                                triggers.retry_peer_runs_from(pubkey, &msg.instance).await;
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
        // Must exceed the client's per-chunk timeout (NOISE_CHUNK_TIMEOUT) — this
        // bounds the whole response write, and a 1 MiB chunk can take well over
        // the old 5s on a CPU-constrained node, which truncated DAR transfers
        // mid-body ("end of file before message length reached").
        Some(NOISE_HANDLER_TIMEOUT),
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
/// Mark a finished peer-side run terminal in the DB (the notification feed is
/// driven off `workflow_runs`, so peer status flows through here rather than an
/// in-memory singleton). `instance_name` is the peer-side row's primary key.
async fn finalize_peer_run(
    db: &SqlitePool,
    instance_name: &str,
    success: bool,
    error_msg: Option<String>,
) {
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
        .set_workflow_run_status(instance_name, status, error_msg.as_deref(), now)
        .await
    {
        tracing::warn!("finalize_peer_run: update failed: {e}");
        return;
    }
    if let Err(e) = Commitable::commit(tx).await {
        tracing::warn!("finalize_peer_run: commit failed: {e}");
    }
}

/// Single peer-job listener. Drains the `PeerJob` queue and spawns one
/// `workflow::start_peer` task per job, so a node can participate as a peer in
/// any number of concurrent workflows at once (the cross-acceptance scenario).
/// Each job carries its own peer-side `instance_name`, the coordinator's run
/// `instance_name` (for command routing), and the coordinator pubkey, so jobs
/// never race over a shared slot. Replaces the four per-kind trigger loops.
async fn run_peer_listener(
    config: NodeConfig,
    db: SqlitePool,
    auth: Arc<RwLock<Option<WorkflowAuth>>>,
    mut peer_jobs: mpsc::UnboundedReceiver<PeerJob>,
) {
    while let Some(job) = peer_jobs.recv().await {
        let config = config.clone();
        let db = db.clone();
        let auth = auth.clone();
        tokio::spawn(async move {
            tracing::info!(
                "Starting {:?} peer workflow {} (coordinator run {:?})",
                job.kind,
                job.instance_name,
                job.coordinator_instance
            );

            // Resolve the coordinator peer from its stored public key.
            let coordinator = match db.get_peer_by_public_key(&job.coordinator_pubkey).await {
                Ok(Some(peer)) => peer,
                Ok(None) => {
                    tracing::error!(
                        "Coordinator with pubkey {} not found; failing peer run {}",
                        job.coordinator_pubkey,
                        job.instance_name
                    );
                    finalize_peer_run(
                        &db,
                        &job.instance_name,
                        false,
                        Some("Coordinator not found in peer database".to_string()),
                    )
                    .await;
                    return;
                }
                Err(e) => {
                    tracing::error!("Failed to look up coordinator peer: {e}");
                    finalize_peer_run(&db, &job.instance_name, false, Some(format!("{e}"))).await;
                    return;
                }
            };

            let auth_snapshot = auth.read().await.clone();
            let result = workflow::start_peer(
                config,
                coordinator,
                db.clone(),
                job.instance_name.clone(),
                job.coordinator_instance.clone(),
                auth_snapshot,
            )
            .await;

            let success = result.is_ok();
            let err_msg = result.as_ref().err().map(|e| format!("{e}"));
            match &result {
                Ok(()) => tracing::info!(
                    "{:?} peer workflow {} completed",
                    job.kind,
                    job.instance_name
                ),
                Err(e) => tracing::error!(
                    "{:?} peer workflow {} failed: {e}",
                    job.kind,
                    job.instance_name
                ),
            }
            finalize_peer_run(&db, &job.instance_name, success, err_msg).await;
        });
    }
    tracing::warn!("peer-job listener exited: all senders dropped");
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
