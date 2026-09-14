//! Canton-native coordination: nodes coordinate only through the
//! synchronizer topology store, Daml contracts, and the Ledger API.
//!
//! This module is the on-ledger foundation (design section 4):
//!
//! * [`identity`]: the node party from `party_credentials` (`kind = 'node'`)
//!   and the topology hosting check every trust decision uses.
//! * [`daml`]: template ids, the record codec, and the client that submits
//!   and reads as the node party.
//! * [`registry`]: the `DecmanNode` entry, peer entries keyed by signatory,
//!   the health snapshot, and the version gate.
//! * [`proposals`]: `WorkflowProposal` create/accept/decline/cancel/finish,
//!   the D6 counting predicates, and the `pending_invitations` projection.
//! * [`topology`]: proposal discovery, propose, co-sign by hash, waits, root
//!   delegations, and the canonical mapping builders.
//! * [`keys`]: the dual-usage party key, its root delegation, the local
//!   identity, and the member key caches (design D4).
//! * [`validation`]: what a member checks before it co-signs.
//! * [`engine`]: start, accept, decline, cancel, retry, and the per-kind
//!   [`engine::KindDriver`] contract the observer dispatches to.
//! * [`observer`]: the one polling loop that drives every run.
//! * [`submission`], [`dars`], [`acs`]: the contracts, DAR, and ACS
//!   contracts (signatures final, bodies pending).
//!
//! [`OnLedger`] is the facade `AppState` holds. See `README.md` in this
//! directory for every public signature.

pub mod acs;
pub mod daml;
pub mod dars;
pub mod engine;
pub mod identity;
pub mod keys;
pub mod observer;
pub mod proposals;
pub mod registry;
pub mod submission;
pub mod topology;
pub mod validation;

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use common::{coordination::RegistryResponse, types::PendingInvitation};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, RwLock};

use crate::{
    auth::WorkflowAuth,
    config::{NodeConfig, PartyCredentials},
    consts,
    db::schema::SchemaRead,
};

pub use daml::{CoordinationClient, CoordinationPackage, CoordinationTemplate};
pub use engine::{
    AcceptedInvitation, KindDriver, MemberVariant, PreflightRejected, RunMeta, StartRequest,
    StartedRun, TickCtx, accept_invitation, cancel_run, decline_invitation, retry_run, start_run,
};
pub use identity::{HostingCheck, NodeIdentity, require_node_identity, verify_hosting};
pub use keys::{PartyKey, ensure_party_key, party_key_name, proposer_key_material};
pub use observer::spawn_observer;
pub use registry::{PeerHealth, PeerHealthSnapshot, PublishOutcome};
pub use topology::UnsolicitedProposal;

/// Micros since the epoch, the Ledger API `Timestamp` unit.
pub fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_micros()).ok())
        .unwrap_or(0)
}

/// Unix seconds.
pub fn now_secs() -> i64 {
    now_micros().div_euclid(1_000_000)
}

/// The facade `AppState` holds: config, database, live auth, the node
/// identity, and the caches the observer loop refreshes (registry snapshot,
/// pending invitations, unsolicited proposals).
///
/// `auth` and `party_credentials` are the same `Arc`s `AppState` holds, so a
/// `PUT /party-config` or `PUT /node-identity` followed by
/// [`OnLedger::reload_identity`] picks up the new token manager.
pub struct OnLedger {
    config: NodeConfig,
    db: SqlitePool,
    auth: Arc<RwLock<Option<WorkflowAuth>>>,
    party_credentials: Arc<RwLock<Vec<PartyCredentials>>>,
    test_mode: bool,
    package: CoordinationPackage,
    identity: RwLock<Option<NodeIdentity>>,
    registry: Arc<RwLock<PeerHealthSnapshot>>,
    /// The projected invitation cards, replaced every observer tick.
    pending_invitations: RwLock<Vec<PendingInvitation>>,
    /// Pending topology proposals nobody asked this node about; UI only.
    unsolicited: RwLock<Vec<UnsolicitedProposal>>,
    /// One lock per in-progress run, so one driver at a time touches it.
    run_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl OnLedger {
    /// Build the facade and load the node identity once. A missing identity
    /// is normal on a fresh node; a broken one is logged and left unset so
    /// the server still starts.
    pub async fn new(
        config: NodeConfig,
        db: SqlitePool,
        auth: Arc<RwLock<Option<WorkflowAuth>>>,
        party_credentials: Arc<RwLock<Vec<PartyCredentials>>>,
        test_mode: bool,
    ) -> Arc<Self> {
        let this = Self::detached(config, db, auth, party_credentials, test_mode);
        match this.reload_identity().await {
            Ok(Some(identity)) => tracing::info!(
                node_party = %identity.node_party,
                participant = %identity.participant_id,
                "node identity loaded"
            ),
            Ok(None) => tracing::info!(
                "no node identity configured; on-ledger coordination waits for PUT /node-identity"
            ),
            Err(e) => tracing::error!(error = %e, "node identity failed to load"),
        }
        this
    }

    /// Build the facade without touching the identity. For tests and for
    /// `AppState` literals that never coordinate on-ledger.
    pub fn detached(
        config: NodeConfig,
        db: SqlitePool,
        auth: Arc<RwLock<Option<WorkflowAuth>>>,
        party_credentials: Arc<RwLock<Vec<PartyCredentials>>>,
        test_mode: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            db,
            auth,
            party_credentials,
            test_mode,
            package: CoordinationPackage::from_env(),
            identity: RwLock::new(None),
            registry: Arc::new(RwLock::new(PeerHealthSnapshot::default())),
            pending_invitations: RwLock::new(Vec::new()),
            unsolicited: RwLock::new(Vec::new()),
            run_locks: Mutex::new(HashMap::new()),
        })
    }

    /// A facade on an in-memory database with no identity, for `AppState`
    /// test literals.
    #[cfg(test)]
    pub(crate) fn placeholder() -> Arc<Self> {
        Self::detached(
            NodeConfig::default(),
            SqlitePool::connect_lazy("sqlite::memory:").expect("lazy in-memory pool"),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(Vec::new())),
            true,
        )
    }

    pub fn config(&self) -> &NodeConfig {
        &self.config
    }

    pub fn db(&self) -> &SqlitePool {
        &self.db
    }

    pub fn package(&self) -> &CoordinationPackage {
        &self.package
    }

    pub fn test_mode(&self) -> bool {
        self.test_mode
    }

    /// Re-read the `kind = 'node'` row and the live auth registry, and
    /// replace the cached identity. Call after `reload_auth`.
    ///
    /// # Errors
    /// Returns an error when a node row exists but no token manager backs it;
    /// the cached identity is cleared in that case.
    pub async fn reload_identity(&self) -> Result<Option<NodeIdentity>> {
        let rows = self.party_credentials.read().await.clone();
        let auth = self.auth.read().await.clone();
        let loaded = identity::load_node_identity(&self.config, &rows, auth.as_ref()).await;
        let mut slot = self.identity.write().await;
        match loaded {
            Ok(identity) => {
                *slot = identity.clone();
                Ok(identity)
            }
            Err(e) => {
                *slot = None;
                Err(e)
            }
        }
    }

    /// The cached identity, if configured and loaded.
    pub async fn identity(&self) -> Option<NodeIdentity> {
        self.identity.read().await.clone()
    }

    /// The cached identity, or the standard "configure it first" error.
    ///
    /// # Errors
    /// Returns an error when no identity is loaded.
    pub async fn require_identity(&self) -> Result<NodeIdentity> {
        let slot = self.identity.read().await;
        require_node_identity(slot.as_ref()).cloned()
    }

    /// A client bound to the current identity.
    ///
    /// # Errors
    /// As [`OnLedger::require_identity`].
    pub async fn client(&self) -> Result<CoordinationClient> {
        let identity = self.require_identity().await?;
        Ok(CoordinationClient::new(
            self.config.clone(),
            identity,
            self.package.clone(),
            self.test_mode,
        ))
    }

    /// The shared snapshot handle, for the observer loop and the status
    /// handlers.
    pub fn registry(&self) -> Arc<RwLock<PeerHealthSnapshot>> {
        self.registry.clone()
    }

    /// A copy of the last snapshot.
    pub async fn registry_snapshot(&self) -> PeerHealthSnapshot {
        self.registry.read().await.clone()
    }

    /// The invitation cards the observer projected last (`GET /invitations`).
    pub async fn pending_invitations(&self) -> Vec<PendingInvitation> {
        self.pending_invitations.read().await.clone()
    }

    /// Replace the projected cards. The observer calls this every tick.
    pub async fn set_pending_invitations(&self, list: Vec<PendingInvitation>) {
        *self.pending_invitations.write().await = list;
    }

    /// Drop one card at once, so an accept or decline is visible before the
    /// next tick re-projects.
    pub async fn remove_pending_invitation(&self, proposal_cid: &str) {
        self.pending_invitations
            .write()
            .await
            .retain(|i| i.id != proposal_cid);
    }

    /// The unsolicited proposals the observer scanned last
    /// (`GET /proposals/unsolicited`).
    pub async fn unsolicited_proposals(&self) -> Vec<UnsolicitedProposal> {
        self.unsolicited.read().await.clone()
    }

    /// Replace the unsolicited list. The observer calls this every minute.
    pub async fn set_unsolicited(&self, list: Vec<UnsolicitedProposal>) {
        *self.unsolicited.write().await = list;
    }

    /// The lock of one run. Callers `try_lock` it and skip the run when it
    /// is held (design D11).
    pub async fn run_lock(&self, instance_name: &str) -> Arc<Mutex<()>> {
        self.run_locks
            .lock()
            .await
            .entry(instance_name.to_string())
            .or_default()
            .clone()
    }

    /// Forget the locks of runs that are no longer in progress.
    pub async fn prune_run_locks(&self, live: &HashSet<String>) {
        self.run_locks
            .lock()
            .await
            .retain(|name, _| live.contains(name));
    }

    /// The configured peers whose participant has vetted the coordination
    /// package.
    ///
    /// # Errors
    /// Returns an error when the package inventory read fails.
    pub async fn vetted_peers(&self) -> Result<HashSet<common::canton_id::CantonId>> {
        let peers = self.db.get_all_peers().await?;
        registry::vetted_peers(
            &self.config,
            self.package.package_name(),
            self.config.participant_id(),
            &peers,
        )
        .await
    }

    /// Read every peer entry, cross-check hosting, rebuild the snapshot, and
    /// store it. The observer loop calls this every tick.
    ///
    /// # Errors
    /// Returns an error when no identity is loaded or a read fails.
    pub async fn refresh_registry(&self) -> Result<PeerHealthSnapshot> {
        let client = self.client().await?;
        let peers = self.db.get_all_peers().await?;
        let vetted = registry::vetted_peers(
            &self.config,
            self.package.package_name(),
            client.participant_id(),
            &peers,
        )
        .await?;
        let entries = registry::read_peer_entries(&client, &self.config).await?;
        let snapshot = registry::build_snapshot(
            &peers,
            client.participant_id(),
            &entries,
            &vetted,
            now_micros(),
            consts::peer_stale_factor(),
            now_secs(),
        );
        *self.registry.write().await = snapshot.clone();
        Ok(snapshot)
    }

    /// Publish this node's `DecmanNode`, or update it when the desired
    /// fields differ (design D3).
    ///
    /// # Errors
    /// Returns an error when no identity is loaded or a ledger call fails.
    pub async fn publish_registry_entry(&self) -> Result<PublishOutcome> {
        let client = self.client().await?;
        let peers = self.db.get_all_peers().await?;
        let vetted = registry::vetted_peers(
            &self.config,
            self.package.package_name(),
            client.participant_id(),
            &peers,
        )
        .await?;
        let desired = registry::desired_node_record(
            &self.config,
            client.identity(),
            &peers,
            &vetted,
            now_micros(),
        );
        registry::publish_or_update(&client, &desired).await
    }

    /// Send a heartbeat when the entry's interval has elapsed.
    ///
    /// # Errors
    /// Returns an error when no identity is loaded or a ledger call fails.
    pub async fn heartbeat_if_due(&self) -> Result<Option<String>> {
        let client = self.client().await?;
        let Some(current) = registry::read_own_entry(&client).await? else {
            return Ok(None);
        };
        registry::heartbeat_if_due(&client, &current, now_micros()).await
    }

    /// The `GET /registry` body: own entry, peers bucket, inbound bucket.
    ///
    /// # Errors
    /// Returns an error when no identity is loaded or a read fails.
    pub async fn read_registry(&self) -> Result<RegistryResponse> {
        let client = self.client().await?;
        let peers = self.db.get_all_peers().await?;
        let own = registry::read_own_entry(&client).await?;
        let entries = registry::read_peer_entries(&client, &self.config).await?;
        Ok(registry::registry_response(
            own.as_ref(),
            &entries,
            &peers,
            now_micros(),
            consts::peer_stale_factor(),
        ))
    }
}
