//! The application state shared by every handler, and its test constructors.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use sqlx::SqlitePool;
use tokio::sync::{Mutex, RwLock, Semaphore, mpsc};

use crate::server::types::PendingInvitation;
use crate::server::{HealthCache, PeerJob, WorkflowRegistry, external_parties, tenant_rate_limit};
use crate::{
    auth::{TokenValidator, WorkflowAuth},
    config::{NodeConfig, PartyCredentials},
    server::peer_status::LastSeen,
};

/// TTL for a cached chunked response (per peer): a DAR a peer asked to read
/// before accepting its invite.
pub(super) const PEER_CHUNK_CACHE_TTL: Duration = Duration::from_secs(30);

/// Per-peer entry in the chunk cache: `(payload bytes, last-access time)`.
/// The `Instant` is updated on every successful chunk read (see `MessageType::GetChunk`
/// handler), so a slow peer mid-reassembly extends its own TTL window.
pub(super) type ChunkCacheEntry = (Vec<u8>, Instant);

/// Shared cache of large peer responses awaiting chunk retrieval by peers.
pub(super) type PeerChunkCache = Arc<Mutex<HashMap<String, ChunkCacheEntry>>>;

/// Application state shared across all handlers
pub struct AppState {
    pub db: SqlitePool,
    pub config: NodeConfig,
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
    /// Authentication registry (real Keycloak, or mock in insecure mode)
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
    /// Wallet-relayed ACS transfers this node is part of, as source or joiner.
    ///
    /// Held here rather than per-request because both ends keep a Canton stream
    /// open across HTTP calls: the source an `ExportPartyAcs`, the joiner an
    /// `ImportPartyAcs` plus the synchronizer disconnect it requires.
    pub relay_sessions: Arc<crate::workflow::party_replication::relay::RelaySessions>,
    /// Every in-flight workflow this node owns, keyed by `instance_name`.
    /// Replaces the single-tenant per-kind `HttpWorkflowState` singletons, the
    /// global in-flight gate, and the single `active_workflow` routing slot:
    /// any number of runs (even of the same kind) run side-by-side. The
    /// always-on Noise listener routes a peer's command via
    /// `workflows.route(msg.instance)`, and `/workflows/{instance}/cancel`
    /// looks an entry up to abort its spawn.
    pub workflows: WorkflowRegistry,
    /// Whether the server is running in insecure/permissive mode (`--insecure`
    /// or tests): mock auth plus wildcard-token query filtering. Field name
    /// kept as `test_mode` for continuity.
    pub test_mode: bool,
    /// Prefixes currently being refreshed from Canton (deduplication)
    pub refreshing_prefixes: Arc<RwLock<HashSet<String>>>,
    /// Bounds how many Canton discoveries run at once, across every prefix.
    /// `refreshing_prefixes` deduplicates one prefix; this bounds the total,
    /// which matters because the prefix comes from the request.
    pub discovery_permits: Arc<Semaphore>,
    /// How many discoveries have completed for a prefix.
    ///
    /// The signal a waiting request watches. A count rather than a timestamp,
    /// because `dec_parties.updated_at` has one-second resolution and a
    /// discovery finishing inside the same second was indistinguishable from
    /// one that never ran.
    pub discovery_generations: Arc<RwLock<HashMap<String, (u64, i64)>>>,
    /// Unix seconds of the last completed Canton discovery, per prefix.
    ///
    /// A prefix with no parties leaves no rows in `dec_parties`, so the cached
    /// rows alone cannot tell "never fetched" from "fetched, found nothing".
    /// Without this a node with no party re-ran the whole discovery query on
    /// every request.
    pub discovery_completed: Arc<RwLock<HashMap<String, i64>>>,
    /// Shared `reqwest::Client` for the proxy-style handlers (`/network-info`,
    /// `/operator-info`, `/token-standard-contracts`). Constructed once at
    /// startup so its connection pool / keep-alives are reused across
    /// requests instead of paying TCP+TLS setup on every call.
    pub http_client: reqwest::Client,
    /// Cached per-hop health of this node and the participant it drives, with
    /// the warm gRPC channels the probes reuse. Shared so a Config tab open in
    /// many browsers costs one probe per TTL, not one per browser.
    pub health_cache: HealthCache,
    /// Topology snapshot of the external parties this node hosts. A request
    /// for a stale one starts a single background scan.
    pub external_parties: external_parties::ExternalPartiesCache,
    /// Request counts per tenant API key, for the per-minute limit on the
    /// `/v0/tenant/*` endpoints.
    pub tenant_rate_limiter: tenant_rate_limit::TenantRateLimiter,
}

#[cfg(test)]
impl AppState {
    /// An `AppState` on an in-memory database, for tests that need one to reach
    /// the code under test. Every field a test cares about is `auth`; the rest
    /// are the cheapest value that satisfies the type.
    pub(crate) async fn for_test(
        auth: Option<crate::auth::WorkflowAuth>,
    ) -> anyhow::Result<actix_web::web::Data<Self>> {
        Ok(Self::for_test_on(
            auth,
            SqlitePool::connect("sqlite::memory:").await?,
        ))
    }

    /// As [`Self::for_test`], on a database the test prepared, such as an
    /// `#[sqlx::test]` pool that already has the migrations applied. Nothing
    /// listens for peer jobs, so an accept fails at the enqueue.
    pub(crate) fn for_test_on(
        auth: Option<crate::auth::WorkflowAuth>,
        db: SqlitePool,
    ) -> actix_web::web::Data<Self> {
        Self::for_test_with_peer_jobs(auth, db).0
    }

    /// As [`Self::for_test_on`], also returning the peer-job queue, so an
    /// accept succeeds the way it does with the peer listener running.
    pub(crate) fn for_test_with_peer_jobs(
        auth: Option<crate::auth::WorkflowAuth>,
        db: SqlitePool,
    ) -> (actix_web::web::Data<Self>, mpsc::UnboundedReceiver<PeerJob>) {
        let (state, peer_jobs) = Self::test_state(auth, db);
        (actix_web::web::Data::new(state), peer_jobs)
    }

    /// An `AppState` whose admin gate is on while every accepted token carries
    /// only a non-admin role, for tests that prove a handler turns such a
    /// caller away.
    pub(crate) async fn for_test_as_non_admin() -> anyhow::Result<actix_web::web::Data<Self>> {
        let (state, _) = Self::test_state(None, SqlitePool::connect("sqlite::memory:").await?);
        Ok(actix_web::web::Data::new(Self {
            token_validator: crate::auth::TokenValidator::Mock(Arc::new(
                crate::auth::MockValidator::new("viewer".to_string()),
            )),
            admin_role: Some("decman-admin".to_string()),
            ..state
        }))
    }

    fn test_state(
        auth: Option<crate::auth::WorkflowAuth>,
        db: SqlitePool,
    ) -> (Self, mpsc::UnboundedReceiver<PeerJob>) {
        let (peer_job_sender, peer_jobs) = mpsc::unbounded_channel();
        let state = Self {
            db,
            config: NodeConfig::default(),
            last_seen: Arc::new(RwLock::new(HashMap::new())),
            peer_job_sender,
            workflows: WorkflowRegistry::new(),
            pending_invitations: Arc::new(RwLock::new(Vec::new())),
            auth: Arc::new(RwLock::new(auth)),
            token_validator: crate::auth::TokenValidator::Mock(Arc::new(
                crate::auth::MockValidator::new("decman-admin".to_string()),
            )),
            admin_role: None,
            party_credentials: Arc::new(RwLock::new(Vec::new())),
            bootstrap_mu: Arc::new(Mutex::new(())),
            relay_sessions: Arc::new(
                crate::workflow::party_replication::relay::RelaySessions::new(),
            ),
            test_mode: true,
            refreshing_prefixes: Arc::new(RwLock::new(HashSet::new())),
            discovery_permits: Arc::new(Semaphore::new(
                crate::server::handlers::MAX_CONCURRENT_DISCOVERIES,
            )),
            discovery_generations: Arc::new(RwLock::new(HashMap::new())),
            discovery_completed: Arc::new(RwLock::new(HashMap::new())),
            http_client: reqwest::Client::new(),
            health_cache: HealthCache::new(),
            external_parties: Default::default(),
            tenant_rate_limiter: Default::default(),
        };
        (state, peer_jobs)
    }
}

// The previous `ListenerControl` struct collapsed to a single `Arc<AtomicBool>`
// (see `noise_listener_pause_flag` below). Atomic so `ListenerPauseGuard::Drop`
// can reset it synchronously when a spawned workflow task is aborted or panics.
