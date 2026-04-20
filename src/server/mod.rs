mod action_serializer;
mod assets;
pub mod audit;
mod handlers;
mod queries;
mod types;

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
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
        BaseQuery, ListDecentralizedNamespaceDefinitionRequest, ListPartyToParticipantRequest,
        StoreId, Synchronizer, base_query, store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use hyper::{Body, Response, StatusCode};
use sqlx::SqlitePool;
use tokio::sync::{Notify, RwLock};
use tokio_noise::handshakes::nn_psk2::Responder;
use utoipa_actix_web::AppExt;
use utoipa_swagger_ui::SwaggerUi;

use crate::{
    auth::{AuthRegistry, MockAuthRegistry, WorkflowAuth},
    config::{NodeConfig, PartyCredentials},
    db::schema::SchemaRead,
    error::Result,
    noise::{Message, MessageType, NoiseKeypair, load_or_generate_keypair, parse_public_key},
    utils::{compute_fingerprint, get_synchronizer_id_from_url},
    workflow,
};

pub use types::*;

/// Application state shared across all handlers
pub struct AppState {
    pub db: SqlitePool,
    pub config: NodeConfig,
    pub peer_status: Arc<RwLock<HashMap<String, bool>>>,
    pub noise_listener_control: Arc<RwLock<ListenerControl>>,
    pub noise_listener_notify: Arc<Notify>,
    pub onboarding_trigger: Arc<Notify>,
    pub kick_trigger: Arc<Notify>,
    pub contracts_trigger: Arc<Notify>,
    pub dars_trigger: Arc<Notify>,
    /// Coordinator's public key (set when invite is received)
    pub coordinator_pubkey: Arc<RwLock<Option<String>>>,
    /// Pending invitations awaiting user acceptance
    pub pending_invitations: Arc<RwLock<Vec<PendingInvitation>>>,
    /// Authentication registry (real Keycloak or mock for test mode)
    pub auth: Arc<RwLock<Option<WorkflowAuth>>>,
    /// Party credentials (mutable, hot-reloadable)
    pub party_credentials: Arc<RwLock<Vec<PartyCredentials>>>,
    /// Whether the server is running in test mode
    pub test_mode: bool,
    /// Prefixes currently being refreshed from Canton (deduplication)
    pub refreshing_prefixes: Arc<RwLock<HashSet<String>>>,
}

/// Control mechanism for the Noise port listener
pub struct ListenerControl {
    pub should_pause: bool,
}

/// Workflow triggers shared across Noise server handlers
#[derive(Clone)]
struct WorkflowTriggers {
    pending_invitations: Arc<RwLock<Vec<PendingInvitation>>>,
    admin_api_url: String,
    synchronizer: String,
}

/// Start the HTTP server and a heartbeat system for peer status tracking
pub async fn start_server(
    host: &str,
    port: u16,
    config: NodeConfig,
    test_mode: bool,
    db: SqlitePool,
) -> Result {
    if !test_mode {
        tracing::warn!(
            "Running without --test flag. Swagger UI is disabled. \
             Use `serve --test` to enable mock auth and Swagger UI."
        );
    }

    let db_party_creds = db.get_all_party_credentials().await.unwrap_or_else(|e| {
        tracing::warn!("Failed to load party credentials from DB: {e}");
        Vec::new()
    });
    let party_credentials = Arc::new(RwLock::new(db_party_creds.clone()));

    // Initialize auth based on mode
    let auth = if test_mode {
        tracing::info!("Running in TEST MODE - using mock authentication");
        Some(WorkflowAuth::Mock(Arc::new(MockAuthRegistry::new(
            party_credentials.clone(),
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

    let peer_status = Arc::new(RwLock::new(HashMap::new()));
    let listener_control = Arc::new(RwLock::new(ListenerControl {
        should_pause: false,
    }));
    let listener_notify = Arc::new(Notify::new());
    let onboarding_trigger = Arc::new(Notify::new());
    let kick_trigger = Arc::new(Notify::new());
    let contracts_trigger = Arc::new(Notify::new());
    let dars_trigger = Arc::new(Notify::new());
    let coordinator_pubkey = Arc::new(RwLock::new(None));
    let pending_invitations = Arc::new(RwLock::new(Vec::new()));

    let app_state = web::Data::new(AppState {
        db: db.clone(),
        config: config.clone(),
        peer_status: peer_status.clone(),
        noise_listener_control: listener_control.clone(),
        noise_listener_notify: listener_notify.clone(),
        onboarding_trigger: onboarding_trigger.clone(),
        kick_trigger: kick_trigger.clone(),
        contracts_trigger: contracts_trigger.clone(),
        dars_trigger: dars_trigger.clone(),
        coordinator_pubkey: coordinator_pubkey.clone(),
        pending_invitations: pending_invitations.clone(),
        auth,
        party_credentials,
        test_mode,
        refreshing_prefixes: Arc::new(RwLock::new(HashSet::new())),
    });
    let kick_state = web::Data::new(Arc::new(handlers::KickWorkflowState::new()));
    let onboarding_state = web::Data::new(Arc::new(handlers::OnboardingWorkflowState::new()));
    let contracts_state = web::Data::new(Arc::new(handlers::ContractsWorkflowState::new()));
    let dars_state = web::Data::new(Arc::new(handlers::DarsWorkflowState::new()));

    // Start heartbeat background task (pings peers and listens for invites)
    let heartbeat_config = config.clone();
    let heartbeat_db = db.clone();
    let heartbeat_status = peer_status.clone();
    let heartbeat_control = listener_control.clone();
    let heartbeat_notify = listener_notify.clone();
    let heartbeat_triggers = WorkflowTriggers {
        pending_invitations: pending_invitations.clone(),
        admin_api_url: config.admin_api_url(),
        synchronizer: config.synchronizer().to_string(),
    };
    tokio::spawn(async move {
        run_heartbeat(
            heartbeat_config,
            heartbeat_db,
            heartbeat_status,
            heartbeat_control,
            heartbeat_notify,
            heartbeat_triggers,
        )
        .await;
    });

    // Start attestor trigger listener for onboarding (starts attestor workflow when invite received)
    let onboarding_attestor_config = config.clone();
    let onboarding_attestor_db = db.clone();
    let onboarding_attestor_control = listener_control.clone();
    let onboarding_attestor_notify = listener_notify.clone();
    let onboarding_attestor_state = onboarding_state.clone();
    let onboarding_coordinator_pubkey = coordinator_pubkey.clone();
    tokio::spawn(async move {
        run_onboarding_attestor_listener(
            onboarding_attestor_config,
            onboarding_attestor_db,
            onboarding_attestor_control,
            onboarding_attestor_notify,
            onboarding_attestor_state,
            onboarding_trigger,
            onboarding_coordinator_pubkey,
        )
        .await;
    });

    // Start attestor trigger listener for kick (starts attestor workflow when kick invite received)
    let kick_attestor_config = config.clone();
    let kick_attestor_db = db.clone();
    let kick_attestor_control = listener_control.clone();
    let kick_attestor_notify = listener_notify.clone();
    let kick_attestor_state = kick_state.clone();
    let kick_coordinator_pubkey = coordinator_pubkey.clone();
    tokio::spawn(async move {
        run_kick_attestor_listener(
            kick_attestor_config,
            kick_attestor_db,
            kick_attestor_control,
            kick_attestor_notify,
            kick_attestor_state,
            kick_trigger,
            kick_coordinator_pubkey,
        )
        .await;
    });

    // Start attestor trigger listener for contracts (starts attestor workflow when contracts invite received)
    let contracts_attestor_config = config.clone();
    let contracts_attestor_db = db.clone();
    let contracts_attestor_control = listener_control.clone();
    let contracts_attestor_notify = listener_notify.clone();
    let contracts_attestor_state = contracts_state.clone();
    let contracts_coordinator_pubkey = coordinator_pubkey.clone();
    tokio::spawn(async move {
        run_contracts_attestor_listener(
            contracts_attestor_config,
            contracts_attestor_db,
            contracts_attestor_control,
            contracts_attestor_notify,
            contracts_attestor_state,
            contracts_trigger,
            contracts_coordinator_pubkey,
        )
        .await;
    });

    // Start attestor trigger listener for DARs (starts attestor workflow when DARs invite received)
    let dars_attestor_config = config.clone();
    let dars_attestor_db = db.clone();
    let dars_attestor_control = listener_control.clone();
    let dars_attestor_notify = listener_notify.clone();
    let dars_attestor_state = dars_state.clone();
    let dars_coordinator_pubkey = coordinator_pubkey.clone();
    tokio::spawn(async move {
        run_dars_attestor_listener(
            dars_attestor_config,
            dars_attestor_db,
            dars_attestor_control,
            dars_attestor_notify,
            dars_attestor_state,
            dars_trigger,
            dars_coordinator_pubkey,
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
        let cors = Cors::permissive();

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
            .service(handlers::start_onboarding)
            .service(handlers::get_onboarding_status)
            .service(handlers::start_contracts)
            .service(handlers::get_contracts_status)
            .service(handlers::upload_dars_local)
            .service(handlers::start_dars)
            .service(handlers::get_dars_status)
            .service(handlers::get_key_status)
            .service(handlers::get_invitations)
            .service(handlers::accept_invitation)
            .service(handlers::decline_invitation)
            .service(handlers::get_auth_config)
            .service(handlers::get_auth_status)
            .service(handlers::test_auth)
            .service(handlers::get_governance)
            .service(handlers::get_governance_state)
            .service(handlers::get_vaults_handler)
            .service(handlers::get_provider_services_handler)
            .service(handlers::get_user_services_handler)
            .service(handlers::get_registrar_services_handler)
            .service(handlers::query_contracts_handler)
            .service(handlers::get_packages)
            .service(handlers::propose_action)
            .service(handlers::confirm_action)
            .service(handlers::execute_action)
            .service(handlers::expire_confirmation)
            .service(handlers::cancel_confirmation)
            .service(handlers::get_governance_audit)
            .service(handlers::get_token_standard_contracts)
            .service(handlers::get_network_info)
            .service(handlers::get_party_config)
            .service(handlers::save_party_config)
            .split_for_parts();

        let mut app = app.wrap(cors);
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
    listener_control: Arc<RwLock<ListenerControl>>,
    listener_notify: Arc<Notify>,
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

    // Listener management loop
    let listener_control_spawn = listener_control.clone();
    let listener_notify_spawn = listener_notify.clone();
    let keypair_spawn = keypair.clone();
    let peer_keys_spawn = peer_keys.clone();
    let triggers_spawn = triggers.clone();

    tokio::spawn(async move {
        loop {
            // Wait for permission to bind
            let should_pause = {
                let control = listener_control_spawn.read().await;
                control.should_pause
            };

            if should_pause {
                tracing::info!("Noise listener paused for workflow");
                listener_notify_spawn.notified().await;
                tracing::info!("Resuming Noise listener");
                continue;
            }

            // Try to bind listener
            match TcpListener::bind(&listen_addr).await {
                Ok(listener) => {
                    tracing::info!("Noise invite listener started on {listen_addr}");

                    loop {
                        tokio::select! {
                            result = listener.accept() => {
                                if let Ok((socket, peer_addr)) = result {
                                    let keypair = keypair_spawn.clone();
                                    let peer_keys = peer_keys_spawn.clone();
                                    let triggers = triggers_spawn.clone();

                                    tokio::spawn(async move {
                                        handle_incoming_connection(socket, peer_addr, keypair, peer_keys, triggers).await;
                                    });
                                }
                            }
                            _ = async {
                                loop {
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                    let control = listener_control_spawn.read().await;
                                    if control.should_pause {
                                        break;
                                    }
                                }
                            } => {
                                tracing::info!("Stopping listener for workflow");
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to bind invite listener on {listen_addr}: {e}, retrying in 5s"
                    );
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });

    // Ping peers every 5 seconds
    run_peer_ping_loop(config, db, peer_status).await;
}

/// Handle an incoming Noise connection (either ping or invite)
async fn handle_incoming_connection(
    socket: tokio::net::TcpStream,
    peer_addr: std::net::SocketAddr,
    keypair: Arc<NoiseKeypair>,
    peer_keys: Arc<HashMap<String, secp256k1::PublicKey>>,
    triggers: WorkflowTriggers,
) {
    let secret_key = keypair.secret_key;
    let peer_keys_clone = peer_keys.clone();
    let our_public_key_hex = keypair.public_key_hex();

    // Create PSK derivation responder
    let responder = Responder::new(move |identity: &[u8]| -> Option<[u8; 32]> {
        // Identity contains peer's public key
        if identity.len() == 33 {
            // Compressed public key
            if let Ok(peer_pub_key) = secp256k1::PublicKey::from_slice(identity) {
                let psk = secp256k1::ecdh::SharedSecret::new(&peer_pub_key, &secret_key);
                return Some(psk.secret_bytes());
            }
        }
        // Fallback: try to find peer by ID string
        let peer_id = std::str::from_utf8(identity).ok()?;
        let peer_pub_key = peer_keys_clone.get(peer_id)?;
        let psk = secp256k1::ecdh::SharedSecret::new(peer_pub_key, &secret_key);
        Some(psk.secret_bytes())
    });

    let result = hyper_noise::server::serve_http(
        socket,
        responder,
        move |peer_id: &[u8], req: hyper::Request<Body>| {
            let triggers = triggers.clone();
            let our_pubkey = our_public_key_hex.clone();
            let peer_keys = peer_keys.clone();
            // Convert peer_id to hex public key for storage
            // peer_id can be either raw 33-byte public key or participant_id string
            let peer_pubkey_hex = if peer_id.len() == 33 {
                Some(hex::encode(peer_id))
            } else if let Ok(peer_id_str) = std::str::from_utf8(peer_id) {
                // Look up public key by participant_id
                peer_keys
                    .get(peer_id_str)
                    .map(|pk| hex::encode(pk.serialize()))
            } else {
                None
            };
            async move {
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
                            let response_msg = Message::new(MessageType::Data, payload);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(response_msg.to_bytes()))
                                .unwrap());
                        }
                        MessageType::RequestOwnerKeys => {
                            tracing::debug!("Received RequestOwnerKeys request");
                            let admin_url = triggers.admin_api_url.clone();
                            let synchronizer = triggers.synchronizer.clone();
                            let payload = match list_my_owner_keys(&admin_url, &synchronizer).await
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
                        MessageType::InviteOnboarding => {
                            tracing::info!(
                                "Received onboarding invite, storing as pending invitation"
                            );
                            if let Some(ref pubkey) = peer_pubkey_hex {
                                let invitation = PendingInvitation {
                                    id: format!("onboarding-{}", &pubkey[..16]),
                                    invitation_type: InvitationType::Onboarding,
                                    coordinator_pubkey: pubkey.clone(),
                                    coordinator_name: None,
                                    received_at: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_secs() as i64)
                                        .unwrap_or(0),
                                };
                                let mut invitations = triggers.pending_invitations.write().await;
                                // Remove any existing invitation of the same type from the same coordinator
                                invitations.retain(|i| i.id != invitation.id);
                                invitations.push(invitation);
                            }

                            let ack = Message::new_empty(MessageType::Ack);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(ack.to_bytes()))
                                .unwrap());
                        }
                        MessageType::InviteKick => {
                            tracing::info!("Received kick invite, storing as pending invitation");
                            if let Some(ref pubkey) = peer_pubkey_hex {
                                let invitation = PendingInvitation {
                                    id: format!("kick-{}", &pubkey[..16]),
                                    invitation_type: InvitationType::Kick,
                                    coordinator_pubkey: pubkey.clone(),
                                    coordinator_name: None,
                                    received_at: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_secs() as i64)
                                        .unwrap_or(0),
                                };
                                let mut invitations = triggers.pending_invitations.write().await;
                                invitations.retain(|i| i.id != invitation.id);
                                invitations.push(invitation);
                            }

                            let ack = Message::new_empty(MessageType::Ack);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(ack.to_bytes()))
                                .unwrap());
                        }
                        MessageType::InviteContracts => {
                            tracing::info!(
                                "Received contracts invite, storing as pending invitation"
                            );
                            if let Some(ref pubkey) = peer_pubkey_hex {
                                let invitation = PendingInvitation {
                                    id: format!("contracts-{}", &pubkey[..16]),
                                    invitation_type: InvitationType::Contracts,
                                    coordinator_pubkey: pubkey.clone(),
                                    coordinator_name: None,
                                    received_at: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_secs() as i64)
                                        .unwrap_or(0),
                                };
                                let mut invitations = triggers.pending_invitations.write().await;
                                invitations.retain(|i| i.id != invitation.id);
                                invitations.push(invitation);
                            }

                            let ack = Message::new_empty(MessageType::Ack);
                            return Ok(Response::builder()
                                .status(StatusCode::OK)
                                .body(Body::from(ack.to_bytes()))
                                .unwrap());
                        }
                        MessageType::InviteDars => {
                            tracing::info!("Received DARs invite, storing as pending invitation");
                            if let Some(ref pubkey) = peer_pubkey_hex {
                                let invitation = PendingInvitation {
                                    id: format!("dars-{}", &pubkey[..16]),
                                    invitation_type: InvitationType::Dars,
                                    coordinator_pubkey: pubkey.clone(),
                                    coordinator_name: None,
                                    received_at: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_secs() as i64)
                                        .unwrap_or(0),
                                };
                                let mut invitations = triggers.pending_invitations.write().await;
                                invitations.retain(|i| i.id != invitation.id);
                                invitations.push(invitation);
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

        let current_participant_id = config.participant_id();
        let futures: Vec<_> = peers
            .iter()
            .filter(|p| p.participant_id != *current_participant_id)
            .map(|peer| {
                let id = peer.participant_id.to_string();
                let address = peer.address.clone();
                let port = peer.port;

                async move {
                    let addr = format!("{address}:{port}");
                    let active = tokio::time::timeout(
                        Duration::from_secs(2),
                        tokio::net::TcpStream::connect(&addr),
                    )
                    .await
                    .map(|r| r.is_ok())
                    .unwrap_or(false);

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

/// Background task that starts onboarding attestor workflow when triggered by an invite
async fn run_onboarding_attestor_listener(
    config: NodeConfig,
    db: SqlitePool,
    listener_control: Arc<RwLock<ListenerControl>>,
    listener_notify: Arc<Notify>,
    onboarding_state: web::Data<Arc<handlers::OnboardingWorkflowState>>,
    onboarding_trigger: Arc<Notify>,
    coordinator_pubkey: Arc<RwLock<Option<String>>>,
) {
    loop {
        // Wait for trigger
        onboarding_trigger.notified().await;

        tracing::info!("Received onboarding invite, starting attestor workflow...");

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
                    tracing::error!("No coordinator public key stored, cannot start attestor");
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

        // Update status
        {
            let mut status = onboarding_state.status.write().await;
            *status = types::OnboardingStatus::InProgress;
            let mut error = onboarding_state.error.write().await;
            *error = None;
        }

        let guard =
            types::ListenerPauseGuard::pause(listener_control.clone(), listener_notify.clone())
                .await;

        // Start attestor workflow
        let workflow_config = config.clone();
        let result = workflow::start_attestor(workflow_config, coordinator).await;

        guard.resume().await;

        // Update status
        let mut status = onboarding_state.status.write().await;
        let mut error = onboarding_state.error.write().await;

        match result {
            Ok(()) => {
                *status = types::OnboardingStatus::Completed;
                tracing::info!("Onboarding attestor workflow completed successfully");
            }
            Err(e) => {
                *status = types::OnboardingStatus::Failed;
                *error = Some(format!("{e}"));
                tracing::error!("Onboarding attestor workflow failed: {e}");
            }
        }
    }
}

/// Background task that starts kick attestor workflow when triggered by an invite
async fn run_kick_attestor_listener(
    config: NodeConfig,
    db: SqlitePool,
    listener_control: Arc<RwLock<ListenerControl>>,
    listener_notify: Arc<Notify>,
    kick_state: web::Data<Arc<handlers::KickWorkflowState>>,
    kick_trigger: Arc<Notify>,
    coordinator_pubkey: Arc<RwLock<Option<String>>>,
) {
    loop {
        // Wait for trigger
        kick_trigger.notified().await;

        tracing::info!("Received kick invite, starting kick attestor workflow...");

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
                    tracing::error!("No coordinator public key stored, cannot start attestor");
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

        // Update status
        {
            let mut status = kick_state.status.write().await;
            *status = types::KickStatus::InProgress;
            let mut error = kick_state.error.write().await;
            *error = None;
        }

        let guard =
            types::ListenerPauseGuard::pause(listener_control.clone(), listener_notify.clone())
                .await;

        // Start kick attestor workflow
        let workflow_config = config.clone();
        let result = workflow::start_attestor(workflow_config, coordinator).await;

        guard.resume().await;

        // Update status
        let mut status = kick_state.status.write().await;
        let mut error = kick_state.error.write().await;

        match result {
            Ok(()) => {
                *status = types::KickStatus::Completed;
                tracing::info!("Kick attestor workflow completed successfully");
            }
            Err(e) => {
                *status = types::KickStatus::Failed;
                *error = Some(format!("{e}"));
                tracing::error!("Kick attestor workflow failed: {e}");
            }
        }
    }
}

/// Background task that starts contracts attestor workflow when triggered by an invite
async fn run_contracts_attestor_listener(
    config: NodeConfig,
    db: SqlitePool,
    listener_control: Arc<RwLock<ListenerControl>>,
    listener_notify: Arc<Notify>,
    contracts_state: web::Data<Arc<handlers::ContractsWorkflowState>>,
    contracts_trigger: Arc<Notify>,
    coordinator_pubkey: Arc<RwLock<Option<String>>>,
) {
    loop {
        // Wait for trigger
        contracts_trigger.notified().await;

        tracing::info!("Received contracts invite, starting contracts attestor workflow...");

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
                    tracing::error!("No coordinator public key stored, cannot start attestor");
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

        // Update status
        {
            let mut status = contracts_state.status.write().await;
            *status = types::WorkflowProgress::InProgress;
            let mut error = contracts_state.error.write().await;
            *error = None;
        }

        let guard =
            types::ListenerPauseGuard::pause(listener_control.clone(), listener_notify.clone())
                .await;

        // Start contracts attestor workflow
        let workflow_config = config.clone();
        let result = workflow::start_attestor(workflow_config, coordinator).await;

        guard.resume().await;

        // Update status
        let mut status = contracts_state.status.write().await;
        let mut error = contracts_state.error.write().await;

        match result {
            Ok(()) => {
                *status = types::WorkflowProgress::Completed;
                tracing::info!("Contracts attestor workflow completed successfully");
            }
            Err(e) => {
                *status = types::WorkflowProgress::Failed;
                *error = Some(format!("{e}"));
                tracing::error!("Contracts attestor workflow failed: {e}");
            }
        }
    }
}

/// Background task that starts DARs attestor workflow when triggered by an invite
async fn run_dars_attestor_listener(
    config: NodeConfig,
    db: SqlitePool,
    listener_control: Arc<RwLock<ListenerControl>>,
    listener_notify: Arc<Notify>,
    dars_state: web::Data<Arc<handlers::DarsWorkflowState>>,
    dars_trigger: Arc<Notify>,
    coordinator_pubkey: Arc<RwLock<Option<String>>>,
) {
    loop {
        // Wait for trigger
        dars_trigger.notified().await;

        tracing::info!("Received DARs invite, starting DARs attestor workflow...");

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
                    tracing::error!("No coordinator public key stored, cannot start attestor");
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

        // Update status
        {
            let mut status = dars_state.status.write().await;
            *status = types::WorkflowProgress::InProgress;
            let mut error = dars_state.error.write().await;
            *error = None;
        }

        let guard =
            types::ListenerPauseGuard::pause(listener_control.clone(), listener_notify.clone())
                .await;

        // Start DARs attestor workflow
        let workflow_config = config.clone();
        let result = workflow::start_attestor(workflow_config, coordinator).await;

        guard.resume().await;

        // Update status
        let mut status = dars_state.status.write().await;
        let mut error = dars_state.error.write().await;

        match result {
            Ok(()) => {
                *status = types::WorkflowProgress::Completed;
                tracing::info!("DARs attestor workflow completed successfully");
            }
            Err(e) => {
                *status = types::WorkflowProgress::Failed;
                *error = Some(format!("{e}"));
                tracing::error!("DARs attestor workflow failed: {e}");
            }
        }
    }
}

/// Query Canton for this node's owner keys across all decentralized parties.
/// Returns JSON: `[{"party_id": "prefix::namespace", "owner_key": "fingerprint"}, ...]`
async fn list_my_owner_keys(admin_api_url: &str, synchronizer_alias: &str) -> Result<Vec<u8>> {
    let channel = tonic::transport::Channel::from_shared(admin_api_url.to_string())?
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

    let synchronizer_id = get_synchronizer_id_from_url(admin_api_url, synchronizer_alias).await?;

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

    // Get all decentralized namespace definitions
    let dns_response = topology_client
        .list_decentralized_namespace_definition(tonic::Request::new(
            ListDecentralizedNamespaceDefinitionRequest {
                base_query: Some(base_query.clone()),
                filter_namespace: String::new(),
            },
        ))
        .await?
        .into_inner();

    // Get P2P mappings to resolve full party_id (prefix::namespace)
    let p2p_response = topology_client
        .list_party_to_participant(tonic::Request::new(ListPartyToParticipantRequest {
            base_query: Some(base_query),
            filter_party: String::new(),
            filter_participant: String::new(),
        }))
        .await?
        .into_inner();

    // Build namespace → full party_id map from P2P data
    let namespace_to_party: HashMap<String, String> = p2p_response
        .results
        .into_iter()
        .filter_map(|r| {
            let p = r.item?;
            let ns = p.party.rsplit_once("::")?.1.to_string();
            Some((ns, p.party))
        })
        .collect();

    // Match this node's fingerprints against each party's owners list
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
