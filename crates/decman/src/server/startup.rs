//! Server startup: auth, state, background tasks and routes, and the metrics server.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use actix_cors::Cors;
use actix_web::{App, HttpServer, web};
use anyhow::Context;
use sqlx::SqlitePool;
use tokio::sync::{Mutex, RwLock, Semaphore, mpsc};
use utoipa_actix_web::AppExt;
use utoipa_swagger_ui::SwaggerUi;

use super::{recovery::recover_in_progress_workflows, state::AppState};
use crate::server::{
    HealthCache, PeerJob, WorkflowRegistry, WorkflowTriggers, assets, audit, former_parties,
    governance_poller, handlers, node_health, peer_status, reward_automation, run_heartbeat,
    run_peer_listener,
};
use crate::{
    auth::{
        AuthRegistry, JwtValidator, MockAuthRegistry, MockValidator, TokenValidator, WorkflowAuth,
    },
    canton_id::CantonId,
    config::{Network, NodeConfig},
    db::schema::{SchemaRead, SchemaWrite},
    error::Result,
    server::middleware::AuthMiddleware,
    server::peer_status::LastSeen,
    workflow,
};

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

/// Start the HTTP server and a heartbeat system for peer status tracking
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

    let relay_sessions = Arc::new(crate::workflow::party_replication::relay::RelaySessions::new());

    let app_state = web::Data::new(AppState {
        db: db.clone(),
        config: config.clone(),
        last_seen: last_seen.clone(),
        peer_job_sender: peer_job_sender.clone(),
        pending_invitations: pending_invitations.clone(),
        auth: auth.clone(),
        token_validator,
        admin_role,
        party_credentials: party_credentials.clone(),
        bootstrap_mu: Arc::new(Mutex::new(())),
        relay_sessions: relay_sessions.clone(),
        workflows: workflows.clone(),
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
        external_parties: Default::default(),
        tenant_rate_limiter: Default::default(),
    });

    // A coordinator killed while it signed a party proposal leaves that
    // proposal's temporary topology store on the participant. Sweep them
    // before any run resumes, because a resumed run creates its own and a
    // sweep running alongside would drop it (#448). Bounded, so a participant
    // that is down does not hold up the boot.
    match tokio::time::timeout(
        Duration::from_secs(15),
        workflow::proposal_store::drop_leftover_stores(&config),
    )
    .await
    {
        Ok(Ok(0)) => {}
        Ok(Ok(dropped)) => tracing::info!(
            dropped,
            "Dropped temporary topology stores an earlier process left behind"
        ),
        Ok(Err(e)) => tracing::warn!(
            error = format!("{e:#}"),
            "Could not sweep leftover temporary topology stores"
        ),
        Err(_) => tracing::warn!("Timed out sweeping leftover temporary topology stores"),
    }

    // A purge runs inside this process, so one still marked running was cut
    // off by the restart and will never record its own outcome. Its row would
    // block every later purge and workflow start, so decman does not start
    // until the row is repaired.
    let interrupted = db
        .fail_interrupted_purges(former_parties::INTERRUPTED_PURGE_ERROR)
        .await
        .context("Could not mark the purges a restart interrupted as failed")?;
    if !interrupted.is_empty() {
        tracing::error!(
            "{} purge(s) of former-party contracts were interrupted by a restart; \
             the participant may still be disconnected from its synchronizer",
            interrupted.len()
        );
    }
    for party_id in &interrupted {
        match CantonId::parse(party_id) {
            Ok(party) => {
                audit::log_purge_audit(
                    &db,
                    audit::PurgeAuditEvent::End(Err(
                        former_parties::INTERRUPTED_PURGE_ERROR.to_string()
                    )),
                    &party,
                    None,
                )
                .await;
            }
            Err(e) => tracing::warn!("Skipping the audit row of an interrupted purge: {e}"),
        }
    }

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
        peer_chunk_cache: Arc::new(Mutex::new(HashMap::new())),
        db: db.clone(),
        party_credentials: party_credentials.clone(),
        workflows: workflows.clone(),
        peer_job_sender: peer_job_sender.clone(),
    };
    spawn_supervised(
        "heartbeat",
        "peer liveness stops updating until this node restarts",
        async move {
            run_heartbeat(
                heartbeat_config,
                heartbeat_db,
                heartbeat_status,
                heartbeat_last_seen,
                heartbeat_triggers,
            )
            .await;
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

    let key_migration_config = config.clone();
    let key_migration_db = db.clone();
    spawn_supervised(
        "party key migration",
        "keys not yet in DecMan's key store stay uncopied until this node restarts",
        async move {
            crate::signing::migration::run_key_migration(key_migration_config, key_migration_db)
                .await;
        },
    );

    let governance_poller_state = app_state.clone();
    spawn_supervised(
        "governance poller",
        "the pending-governance gauges stop updating until this node restarts",
        async move {
            governance_poller::run_governance_poller(governance_poller_state).await;
        },
    );

    // Single peer-job listener: drains the queue and spawns one
    // `workflow::start_peer` per accepted / retried / resumed invite, so this
    // node can be a peer in many concurrent workflows at once.
    let peer_listener_config = config.clone();
    let peer_listener_db = db.clone();
    let peer_listener_auth = auth.clone();
    spawn_supervised(
        "peer listener",
        "this node accepts no further peer jobs until it restarts",
        async move {
            run_peer_listener(
                peer_listener_config,
                peer_listener_db,
                peer_listener_auth,
                peer_job_receiver,
            )
            .await;
        },
    );

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
                handlers::resolve_owner_keys_from_peers(&sync_config, &sync_db, &response.parties)
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
    handlers::register_run_metrics();
    peer_status::register_metrics();
    handlers::register_governance_metrics();
    governance_poller::register_metrics();

    // Separate from the API server, whose ingress forwards every path. 0 disables it.
    let metrics_port = config.metrics_port;
    if metrics_port == 0 {
        tracing::info!("Metrics endpoint disabled (metrics_port = 0)");
    } else if metrics_port == port || metrics_port == config.node.port {
        // The metrics listener binds first, so racing it against the API or the
        // noise server would let a signal take down governance. It yields the
        // port instead, which is the same rule the bind-failure arm below keeps.
        tracing::error!(
            metrics_port,
            api_port = port,
            noise_port = config.node.port,
            "metrics_port collides with another listener; this node reports no metrics"
        );
    } else {
        match metrics_server(host, metrics_port) {
            Ok(running) => {
                tracing::info!("Serving metrics on {host}:{metrics_port}/metrics");
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
            .service(handlers::get_expected_versions)
            .service(handlers::clear_acs_import_quarantine)
            .service(handlers::list_former_parties)
            .service(handlers::preview_former_party_purge)
            .service(handlers::purge_former_party)
            .service(handlers::reconnect_after_purge)
            .service(handlers::dismiss_former_party)
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
            .service(handlers::unhost_external_party)
            .service(handlers::preview_unhost_external_party)
            .service(handlers::get_unhost_audit)
            .service(handlers::get_tenant_call_audit)
            .service(handlers::tenant_prepare)
            .service(handlers::tenant_execute)
            .service(handlers::tenant_add_hosts_prepare)
            .service(handlers::tenant_add_hosts_authorize)
            .service(handlers::tenant_add_hosts_execute)
            .service(handlers::tenant_acs_snapshot)
            .service(handlers::tenant_acs_import)
            .service(handlers::tenant_threshold_prepare)
            .service(handlers::tenant_threshold_authorize)
            .service(handlers::tenant_threshold_execute)
            .service(handlers::tenant_local_party_adopt_prepare)
            .service(handlers::tenant_local_party_adopt_execute)
            .service(handlers::tenant_party_state)
            .service(handlers::tenant_unhost)
            .service(handlers::tenant_status)
            .service(handlers::start_onboarding)
            .service(handlers::get_onboarding_status)
            .service(handlers::cancel_onboarding)
            .service(handlers::start_contracts)
            .service(handlers::get_contracts_status)
            .service(handlers::cancel_contracts)
            .service(handlers::upload_dars_local)
            .service(handlers::start_dars)
            .service(handlers::start_dars_for_package)
            .service(handlers::get_dars_status)
            .service(handlers::cancel_dars)
            .service(handlers::list_workflows)
            .service(handlers::dismiss_workflow)
            .service(handlers::retry_workflow)
            .service(handlers::cancel_workflow_instance)
            .service(handlers::get_key_status)
            .service(handlers::get_invitations)
            .service(handlers::list_handled_invitations)
            .service(handlers::get_invitation_dar)
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
            .service(handlers::get_block_list_shards_handler)
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
            .service(handlers::get_reward_status)
            .service(handlers::get_network_info)
            .service(handlers::get_open_mining_round)
            .service(handlers::get_operator_info)
            .service(handlers::get_party_config)
            .service(handlers::save_party_config)
            .service(handlers::discover_member_party)
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

/// Binds and starts the server that answers `/metrics`.
///
/// It ignores SIGTERM and SIGINT, so it ends only with the process. The API server
/// handles shutdown. A stop on the signal would end the supervised task, which then
/// logs a normal shutdown at `error`.
pub fn metrics_server(host: &str, port: u16) -> std::io::Result<actix_web::dev::Server> {
    Ok(
        HttpServer::new(|| App::new().route("/metrics", web::get().to(handlers::metrics)))
            // One worker: the default is one per logical CPU.
            .workers(1)
            .disable_signals()
            .bind((host, port))?
            .run(),
    )
}

/// Spawns a task that must live as long as the process, and reports its death at
/// `error` with what the operator loses. `consequence` completes the sentence
/// "… returned; " and "… died; ".
///
/// The outer task is what catches a clean return, which never panics and so never
/// reaches the panic hook in `main`. Nothing respawns: a task that returned left
/// state nobody has inspected, and restarting it would hide that.
pub(super) fn spawn_supervised<F>(name: &'static str, consequence: &'static str, task: F)
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
