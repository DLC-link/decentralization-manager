//! HTTP server.
//!
//! Builds the actix-web application (REST API + embedded React UI; a Swagger UI
//! is mounted only when the node runs in insecure mode, i.e. `--insecure`),
//! wires shared [`AppState`], and spawns the background tasks: the on-ledger
//! observer loop that drives every workflow run, the coordination-DAR upload,
//! reward automation, metrics, and the node-health refresh. Nodes never open
//! a connection to one another; every exchange goes through Canton.

mod assets;
mod audit;
mod chain_audit;
pub(crate) mod event_filters;
mod handlers;
pub(crate) mod ledger_paging;
mod middleware;
mod node_health;
pub(crate) mod package_inventory;
mod queries;
mod record;
pub(crate) mod reward_automation;
mod transfer_context;
mod types;

#[cfg(test)]
mod serde_snapshots;

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use actix_cors::Cors;
use actix_web::{App, HttpServer, web};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, RwLock, Semaphore};
use utoipa_actix_web::AppExt;
use utoipa_swagger_ui::SwaggerUi;

use crate::{
    auth::{
        AuthRegistry, JwtValidator, MockAuthRegistry, MockValidator, TokenValidator, WorkflowAuth,
    },
    config::{Network, NodeConfig, PartyCredentials},
    db::schema::SchemaRead,
    error::Result,
    server::middleware::AuthMiddleware,
};

// Reached externally as `dec_party_manager::server::NodeConfigResponse` by the
// `gen-types` binary (a separate crate from this lib), so it must stay `pub`.
pub use handlers::NodeConfigResponse;
pub(crate) use types::*;
// These wire DTOs are likewise reached externally as `dec_party_manager::server::…`,
// by `gen-types` (TS generation) and, for `GovernanceResponse`, by the integration
// tests under `tests/` — both are separate crates that can only see `pub` items.
pub(crate) use node_health::HealthCache;
pub use node_health::{
    ComponentHealth, ComponentState, LinkHealth, NodeHealthResponse, NodeHealthStatus,
    ParticipantHealth, SynchronizerHealth, TopologyQueues,
};
pub use types::{
    AcceptTransferDetails, ActionType, AppRewardBeneficiary, BillingParams, BurnRequestsResponse,
    ConfirmActionRequest, DomainConfirmation, DomainGovernanceAction, ExecuteActionRequest,
    GovernanceAction, GovernanceConfirmation, GovernanceResponse, HoldingInfo, HoldingsResponse,
    MintRequestsResponse, PendingAction, ProposalType, ProposeActionRequest, ServiceRequestDetails,
    TokenRequestInfo, TransferInstructionInfo, TransferInstructionStatus,
    TransferInstructionsResponse, TransferProposalDetails,
};

/// Application state shared across all handlers
pub struct AppState {
    pub db: SqlitePool,
    pub config: NodeConfig,
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
    /// Canton-native coordination: node identity, registry snapshot, the
    /// projected invitation cards, and the client that submits as the node
    /// party. Shares `auth` and `party_credentials` with this state.
    pub onledger: Arc<crate::onledger::OnLedger>,
}

#[cfg(test)]
impl AppState {
    /// An `AppState` on an in-memory database, for tests that need one to reach
    /// the code under test. Every field a test cares about is `auth`; the rest
    /// are the cheapest value that satisfies the type.
    pub(crate) async fn for_test(
        auth: Option<crate::auth::WorkflowAuth>,
    ) -> anyhow::Result<actix_web::web::Data<Self>> {
        Ok(actix_web::web::Data::new(Self {
            db: SqlitePool::connect("sqlite::memory:").await?,
            config: NodeConfig::default(),
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
            onledger: crate::onledger::OnLedger::placeholder(),
        }))
    }
}

/// Refuse to boot with insecure mode enabled on anything but devnet.
///
/// Insecure mode disables authentication, so a stray `DECPM_INSECURE=true` on a
/// testnet/mainnet node would silently turn off all auth with only a log line.
/// This turns that footgun into a hard boot failure. Guards the runtime flag
/// only — `cfg!(test)` / `feature = "test-mode"` builds force insecure on
/// regardless of network and are expected to run off-devnet.
fn ensure_insecure_allowed(insecure: bool, network: Network) -> Result {
    if insecure && network != Network::Devnet {
        anyhow::bail!(
            "--insecure / DECPM_INSECURE is only permitted on the devnet network; \
             refusing to start on {network:?}"
        );
    }
    Ok(())
}

/// Start the HTTP server and the background tasks.
pub async fn start_server(
    host: &str,
    port: u16,
    config: NodeConfig,
    db: SqlitePool,
    admin_role: Option<String>,
    jwt_role_claim: Option<String>,
    allowed_origin: Option<String>,
) -> Result {
    // Fail fast before any setup: the runtime `--insecure` flag must never be
    // honored off devnet (see `ensure_insecure_allowed`).
    ensure_insecure_allowed(config.insecure, config.canton.network)?;

    // Insecure mode is a runtime decision: `--insecure` / `DECPM_INSECURE`
    // selects mock auth (accept any inbound token, present an unsafe HMAC token
    // to Canton) in any build. It is also forced on under `cargo test` and in
    // `--features test-mode` builds so those keep working without the flag.
    // Downstream (`AppState.test_mode`, query filtering) this means "using the
    // permissive wildcard token".
    let insecure = config.insecure || cfg!(any(test, feature = "test-mode"));

    if insecure {
        tracing::warn!(
            "INSECURE MODE ENABLED: inbound auth accepts ANY token and Canton auth uses an \
             unsafe HMAC token. Authentication is effectively disabled — never use in production."
        );
    } else {
        tracing::info!("Running with real JWT validation.");
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

    // Initialize outbound auth (the token DecMan presents to Canton) based on mode
    let auth = if insecure {
        Some(WorkflowAuth::Mock(Arc::new(MockAuthRegistry::with_config(
            party_credentials.clone(),
            &config.insecure_auth,
        ))))
    } else if db_party_creds.is_empty() {
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
    // `MockValidator` is selected only in insecure mode; production verifies
    // real JWTs.
    let token_validator = if insecure {
        TokenValidator::Mock(Arc::new(MockValidator::new(
            admin_role.clone().unwrap_or_default(),
        )))
    } else {
        let no_top_level_config = !config.has_top_level_idp();
        let no_party_creds = party_credentials.read().await.is_empty();
        if no_top_level_config && no_party_creds {
            tracing::warn!(
                "No top-level IdP config (--keycloak-url/realm/client-id or \
                 DECPM_AUTH0_DOMAIN/CLIENT_ID) and no party credentials yet. Inbound \
                 auth will reject every request except the first-run PUT /party-config \
                 bootstrap. Configure the IdP and provision a party to make the node \
                 usable."
            );
        } else if no_top_level_config {
            tracing::info!(
                "No top-level IdP config; trusting only issuers from \
                 party_credentials ({} configured).",
                party_credentials.read().await.len()
            );
        }
        TokenValidator::Jwt(Arc::new(JwtValidator::new(
            config.keycloak.clone(),
            config.auth0.clone(),
            jwt_role_claim,
            party_credentials.clone(),
            http_client.clone(),
        )))
    };

    let relay_sessions = Arc::new(crate::workflow::party_replication::relay::RelaySessions::new());

    // On-ledger coordination facade. Loads the node identity once; a node
    // without one starts normally and waits for `PUT /node-identity`.
    let onledger = crate::onledger::OnLedger::new(
        config.clone(),
        db.clone(),
        auth.clone(),
        party_credentials.clone(),
        insecure,
    )
    .await;

    let app_state = web::Data::new(AppState {
        db: db.clone(),
        config: config.clone(),
        auth: auth.clone(),
        token_validator,
        admin_role,
        party_credentials: party_credentials.clone(),
        bootstrap_mu: Arc::new(Mutex::new(())),
        relay_sessions: relay_sessions.clone(),
        // "test_mode" here means the permissive/wildcard-token mode; driven by
        // `--insecure` (or tests). See the `insecure` binding above.
        test_mode: insecure,
        refreshing_prefixes: Arc::new(RwLock::new(HashSet::new())),
        discovery_permits: Arc::new(Semaphore::new(
            crate::server::handlers::MAX_CONCURRENT_DISCOVERIES,
        )),
        discovery_generations: Arc::new(RwLock::new(HashMap::new())),
        discovery_completed: Arc::new(RwLock::new(HashMap::new())),
        http_client,
        health_cache: HealthCache::new(),
        onledger,
    });

    // Runs interrupted by the last shutdown need no recovery task: the
    // observer re-reads every in-progress `workflow_runs` row on its first
    // tick and continues from the persisted step (design D11).

    // Background task: the coordination DAR (design D8). The package must be
    // vetted on this participant before any registry entry or proposal can be
    // created, so the upload starts at once and retries until the participant
    // answers. `/node-health` reports its state.
    let dar_state = app_state.onledger.clone();
    spawn_supervised(
        "coordination DAR upload",
        "the coordination package may stay unvetted; upload it through POST /dars/upload",
        async move {
            match crate::onledger::dars::startup_upload_coordination_dar(&dar_state).await {
                Ok(state) => tracing::info!(?state.phase, "coordination DAR task finished"),
                Err(e) => tracing::error!(error = %e, "coordination DAR task failed"),
            }
        },
    );

    // Background task: CIP-104 Mode A reward-assignment automation. Clone the
    // existing `web::Data<AppState>` (an Arc) so the loop shares the SAME state —
    // live party credentials, auth, config — never a fresh AppState.
    let reward_automation_state = app_state.clone();
    spawn_supervised(
        "reward automation",
        "coupons will expire unassigned until this node restarts",
        async move {
            reward_automation::run_reward_automation_loop(reward_automation_state).await;
        },
    );

    // Background task: the on-ledger observer loop (design D5, D11). It idles
    // until a node identity exists, so it is safe on a fresh node, and it
    // never panics, so it needs no supervisor.
    let _observer = crate::onledger::spawn_observer(app_state.onledger.clone());

    // Background task: sync decentralized parties from Canton on startup
    let sync_config = config.clone();
    let sync_db = db.clone();
    let sync_auth = app_state.auth.clone();
    let sync_party_creds = app_state.party_credentials.clone();
    let sync_gate = handlers::DiscoveryGate::of(&app_state);
    tokio::spawn(async move {
        // Delay to let Canton stabilize after startup
        tokio::time::sleep(Duration::from_secs(5)).await;
        tracing::info!("Starting background sync of decentralized parties from Canton...");

        let auth_snapshot = sync_auth.read().await.clone();
        let creds_snapshot = sync_party_creds.read().await.clone();

        // Through the same gate as the request paths: a direct call would
        // duplicate an in-flight discovery for the empty prefix and put both
        // results into the cache in an undefined order.
        match handlers::discover_and_cache(
            &sync_gate,
            &sync_config,
            &sync_db,
            "",
            auth_snapshot,
            &creds_snapshot,
            Default::default(),
        )
        .await
        {
            handlers::Discovery::Done(response) => {
                tracing::info!(
                    "Cached {} decentralized parties from Canton",
                    response.parties.len()
                );
                handlers::resolve_owner_keys_from_topology(
                    &sync_config,
                    &sync_db,
                    &response.parties,
                )
                .await;
            }
            handlers::Discovery::InFlight
            | handlers::Discovery::AtCapacity
            | handlers::Discovery::Superseded => {
                tracing::info!("Startup sync skipped: a discovery is already running");
            }
            handlers::Discovery::Failed(e) => {
                tracing::warn!("Background Canton sync failed on startup: {e}");
            }
        }
    });

    reward_automation::register_metrics();
    node_health::register_metrics();

    // Separate from the API server, whose ingress forwards every path. 0 disables it.
    let metrics_port = config.metrics_port;
    if metrics_port == 0 {
        tracing::info!("Metrics endpoint disabled (metrics_port = 0)");
    } else if metrics_port == port {
        // The metrics listener binds first, so racing it against the API server
        // would let a signal take down governance. It yields the port instead,
        // which is the same rule the bind-failure arm below keeps.
        tracing::error!(
            metrics_port,
            api_port = port,
            "metrics_port collides with the API port; this node reports no metrics"
        );
    } else {
        let metrics_host = host.to_string();
        match HttpServer::new(|| App::new().route("/metrics", web::get().to(handlers::metrics)))
            // One worker: the default is one per logical CPU.
            .workers(1)
            .bind((metrics_host.clone(), metrics_port))
        {
            Ok(server) => {
                tracing::info!("Serving metrics on {metrics_host}:{metrics_port}/metrics");
                // `run()` before the async block: `HttpServer` holds an `Rc` and is
                // not `Send`, while the `Server` it returns is.
                let running = server.run();
                spawn_supervised(
                    "metrics server",
                    "every alert rule reads an empty series while this node looks healthy",
                    async move {
                        if let Err(e) = running.await {
                            tracing::error!(error = %e, "the metrics server stopped");
                        }
                    },
                );
            }
            Err(e) => tracing::error!(
                error = %e,
                port = metrics_port,
                "binding the metrics port failed; this node reports no metrics"
            ),
        }

        // Keeps the node-health gauges live while no browser is polling the
        // Config tab, so an alert on them does not silently depend on someone
        // having the tab open. Shares the snapshot cache with the handler, so a
        // tick that a watching browser already paid for costs nothing.
        let health_cache = app_state.health_cache.clone();
        let health_config = config.clone();
        spawn_supervised(
            "node health refresh",
            "the node-health gauges freeze at their last value",
            async move {
                let mut ticker = tokio::time::interval(node_health::BACKGROUND_REFRESH);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    ticker.tick().await;
                    health_cache
                        .refresh_if_older_than(&health_config, node_health::BACKGROUND_REFRESH)
                        .await;
                }
            },
        );
    }

    // A wallet-relayed transfer holds a Canton export stream open on the source
    // and keeps the joiner off the synchronizer for the duration. A wallet that
    // vanishes mid-transfer would leave both that way, so idle sessions are
    // reaped — which fails the import and reconnects the participant.
    {
        let sessions = relay_sessions.clone();
        spawn_supervised(
            "ACS relay session reaper",
            "a wallet that abandons a transfer leaves its joiner off the synchronizer",
            async move {
                crate::workflow::party_replication::relay::reap_forever(sessions).await;
            },
        );
    }

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
            .service(handlers::get_node_health)
            .service(handlers::compare_peer_packages)
            .service(handlers::get_vetted_packages)
            .service(handlers::clear_acs_import_quarantine)
            .service(handlers::start_kick)
            .service(handlers::get_kick_status)
            .service(handlers::cancel_kick)
            .service(handlers::start_add_party)
            .service(handlers::get_add_party_status)
            .service(handlers::cancel_add_party)
            .service(handlers::start_change_threshold)
            .service(handlers::get_change_threshold_status)
            .service(handlers::cancel_change_threshold)
            .service(handlers::list_external_parties)
            .service(handlers::tenant_prepare)
            .service(handlers::tenant_onboard)
            .service(handlers::tenant_add_hosts_prepare)
            .service(handlers::tenant_add_hosts_onboard)
            .service(handlers::tenant_acs_snapshot)
            .service(handlers::tenant_acs_import)
            .service(handlers::tenant_threshold_prepare)
            .service(handlers::tenant_threshold_onboard)
            .service(handlers::tenant_local_party_adopt_prepare)
            .service(handlers::tenant_local_party_adopt_onboard)
            .service(handlers::tenant_party_state)
            .service(handlers::tenant_status)
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
            .service(handlers::get_invitations)
            .service(handlers::accept_invitation)
            .service(handlers::decline_invitation)
            .service(handlers::get_auth_config)
            .service(handlers::get_auth_status)
            .service(handlers::test_auth)
            .service(handlers::grant_rights)
            .service(handlers::get_governance)
            .service(handlers::get_governance_state)
            .service(handlers::get_proposals_page)
            .service(handlers::get_known_members)
            .service(handlers::get_provider_services_handler)
            .service(handlers::get_user_services_handler)
            .service(handlers::get_credential_offers_handler)
            .service(handlers::get_credentials_handler)
            .service(handlers::get_registrar_service_requests_handler)
            .service(handlers::get_provider_configurations_handler)
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
            .service(handlers::cancel_proposal)
            .service(handlers::get_governance_audit)
            .service(handlers::get_governance_chain_audit)
            .service(handlers::get_token_standard_contracts)
            .service(handlers::get_coupon_reassignment_delegation)
            .service(handlers::get_network_info)
            .service(handlers::get_operator_info)
            .service(handlers::get_party_config)
            .service(handlers::save_party_config)
            .service(handlers::discover_member_party)
            .service(handlers::get_node_identity)
            .service(handlers::save_node_identity)
            .service(handlers::get_registry)
            .service(handlers::get_unsolicited_proposals)
            .service(handlers::get_acs_manifests)
            .service(handlers::export_acs_snapshot)
            .service(handlers::import_acs_snapshot)
            .split_for_parts();

        let mut app = app.wrap(AuthMiddleware).wrap(cors);
        if insecure {
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

/// Spawns a task that must live as long as the process, and reports its death at
/// `error` with what the operator loses. `consequence` completes the sentence
/// "… returned; " and "… died; ".
///
/// The outer task is what catches a clean return, which never panics and so never
/// reaches the panic hook in `main`. Nothing respawns: a task that returned left
/// state nobody has inspected, and restarting it would hide that.
fn spawn_supervised<F>(name: &'static str, consequence: &'static str, task: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        match tokio::spawn(task).await {
            Ok(()) => tracing::error!(task = name, "{name} loop returned; {consequence}"),
            Err(e) => tracing::error!(task = name, error = %e, "{name} task died; {consequence}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{Network, ensure_insecure_allowed};

    #[test]
    fn insecure_allowed_on_devnet() {
        assert!(ensure_insecure_allowed(true, Network::Devnet).is_ok());
    }

    #[test]
    fn insecure_refused_off_devnet() {
        assert!(ensure_insecure_allowed(true, Network::Testnet).is_err());
        assert!(ensure_insecure_allowed(true, Network::Mainnet).is_err());
    }

    #[test]
    fn secure_allowed_on_any_network() {
        assert!(ensure_insecure_allowed(false, Network::Devnet).is_ok());
        assert!(ensure_insecure_allowed(false, Network::Testnet).is_ok());
        assert!(ensure_insecure_allowed(false, Network::Mainnet).is_ok());
    }
}
