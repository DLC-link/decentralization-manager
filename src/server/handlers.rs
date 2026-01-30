use std::{collections::HashMap, sync::Arc, time::Duration};

use actix_web::{HttpResponse, Responder, get, post, web};
use base64::Engine;
use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        Command, Commands, DisclosedContract, ExerciseCommand, Identifier, Record, RecordField,
        SubmitAndWaitRequest, Value,
        admin::{
            ListUserRightsRequest,
            right::{CanActAs, CanReadAs, Kind},
        },
        command,
        command_service_client::CommandServiceClient,
        value,
    },
    digitalasset::canton::{
        crypto::{
            admin::v30::{ListMyKeysRequest, vault_service_client::VaultServiceClient},
            v30::public_key,
        },
        topology::admin::v30::{
            BaseQuery, ListDecentralizedNamespaceDefinitionRequest, ListPartyToParticipantRequest,
            StoreId, Synchronizer, base_query, store_id, synchronizer,
            topology_manager_read_service_client::TopologyManagerReadServiceClient,
        },
    },
};
use serde::Deserialize;

use super::{
    AppState, action_serializer,
    queries::{
        get_contracts, get_governance_confirmations,
        get_governance_state as query_governance_state, get_party_metadata, get_provider_services,
        get_user_services, get_vaults,
    },
    types::{
        AuthStatus, AuthStatusResponse, AuthTestResponse, AuthTestResult, ConfirmActionRequest,
        ConnectionStatus, ContractsRequest, DecentralizedPartiesResponse, DecentralizedParty,
        ExecuteActionRequest, ExpireConfirmationRequest, GovernanceResponse,
        GovernanceStateResponse, HttpWorkflowState, InvitationActionRequest, InvitationType,
        KeyStatusResponse, KickRequest, KickResponse, KickStatus, ListenerPauseGuard,
        OnboardingRequest, OnboardingResponse, OnboardingStatus, ParticipantInfo,
        ParticipantStatus, ParticipantsStatusResponse, PartyAuthStatus, PendingInvitation,
        PendingInvitationsResponse, Permission, ProviderServicesResponse, RightsStatus,
        UserServicesResponse, VaultsResponse, WorkflowProgress, WorkflowResponse,
    },
};
use crate::{
    auth::WorkflowAuth,
    config::{NetworkConfig, NodeConfig, Peer},
    consts::VAULT_GOVERNANCE_PACKAGE_ID,
    error::Result,
    noise::{Message, MessageType, NoiseKeypair, parse_public_key, send_noise_message},
    participant_id::CantonId,
    server::ActionType,
    utils, workflow,
};

/// Get the network configuration
#[get("/network-config")]
pub async fn get_network_config(data: web::Data<AppState>) -> impl Responder {
    match data.config.load_network_config().await {
        Ok(network_config) => HttpResponse::Ok().json(network_config),
        Err(e) => {
            tracing::error!("Failed to load network config: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to load network config: {e}")
            }))
        }
    }
}

/// Save the network configuration (peers list)
#[post("/network-config")]
pub async fn save_network_config(
    data: web::Data<AppState>,
    body: web::Json<Vec<Peer>>,
) -> impl Responder {
    let network_config = NetworkConfig {
        peers: body.into_inner(),
    };

    match data.config.save_network_config(&network_config).await {
        Ok(()) => {
            tracing::info!(
                "Saved network config with {} peers",
                network_config.peers.len()
            );
            HttpResponse::Ok().json(serde_json::json!({ "success": true }))
        }
        Err(e) => {
            tracing::error!("Failed to save network config: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to save network config: {e}")
            }))
        }
    }
}

/// Get the node configuration
#[get("/node-config")]
pub async fn get_node_config(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(&data.config)
}

/// Query parameters for decentralized parties endpoint
#[derive(Debug, Deserialize)]
pub struct PartiesQuery {
    /// Filter parties by prefix (e.g., "cbtc-network")
    #[serde(default)]
    pub prefix: Option<String>,
}

/// Get decentralized parties the current participant is a member of
#[get("/decentralized-parties")]
pub async fn get_decentralized_parties(
    data: web::Data<AppState>,
    query: web::Query<PartiesQuery>,
) -> impl Responder {
    match fetch_decentralized_parties(&data.config, query.prefix.as_deref(), data.auth.clone())
        .await
    {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(e) => {
            tracing::error!("Failed to fetch decentralized parties: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to fetch decentralized parties: {e}")
            }))
        }
    }
}

async fn fetch_decentralized_parties(
    config: &NodeConfig,
    prefix_filter: Option<&str>,
    auth: Option<WorkflowAuth>,
) -> Result<DecentralizedPartiesResponse> {
    let channel = tonic::transport::Channel::from_shared(config.admin_api_url())?
        .connect()
        .await?;

    let mut topology_client = TopologyManagerReadServiceClient::new(channel.clone())
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);
    let mut vault_client =
        VaultServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    // Get all namespace keys from this participant
    let keys_response = vault_client
        .list_my_keys(tonic::Request::new(ListMyKeysRequest { filters: None }))
        .await?
        .into_inner();

    let mut namespace_key_fingerprints = HashMap::new();
    for key_meta in keys_response.private_keys_metadata {
        if let Some(pub_key_with_name) = &key_meta.public_key_with_name
            && let Some(pub_key) = &pub_key_with_name.public_key
            && let Some(public_key::Key::SigningPublicKey(signing_key)) = &pub_key.key
            && signing_key.usage.contains(&1)
        {
            // SigningKeyUsage::Namespace = 1
            let fingerprint = utils::compute_fingerprint(signing_key);
            namespace_key_fingerprints.insert(fingerprint, true);
        }
    }

    // List all decentralized namespaces
    let dns_response = topology_client
        .list_decentralized_namespace_definition(tonic::Request::new(
            ListDecentralizedNamespaceDefinitionRequest {
                base_query: Some(BaseQuery {
                    store: Some(StoreId {
                        store: Some(store_id::Store::Synchronizer(Synchronizer {
                            kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.clone())),
                        })),
                    }),
                    proposals: false,
                    operation: 0,
                    time_query: Some(base_query::TimeQuery::HeadState(())),
                    filter_signed_key: String::new(),
                    protocol_version: None,
                }),
                filter_namespace: String::new(),
            },
        ))
        .await?
        .into_inner();

    // Query P2P mappings with optional party prefix filter
    let p2p_response = topology_client
        .list_party_to_participant(tonic::Request::new(ListPartyToParticipantRequest {
            base_query: Some(BaseQuery {
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
            }),
            filter_party: prefix_filter.unwrap_or_default().to_string(),
            filter_participant: String::new(),
        }))
        .await?
        .into_inner();

    // Build a map of namespace -> P2P item for quick lookup
    let p2p_by_namespace: HashMap<String, _> = p2p_response
        .results
        .into_iter()
        .filter_map(|r| {
            let p = r.item?;
            let ns = p.party.rsplit_once("::")?.1.to_string();
            Some((ns, p))
        })
        .collect();

    // Filter to parties where this participant is a member
    let my_parties: Vec<_> = dns_response
        .results
        .into_iter()
        .filter_map(|result| {
            let item = result.item?;
            let my_owner_key = item
                .owners
                .iter()
                .find(|owner| namespace_key_fingerprints.contains_key(*owner))
                .cloned()?;
            let p2p = p2p_by_namespace.get(&item.decentralized_namespace)?;
            Some((item, my_owner_key, p2p.clone()))
        })
        .collect();

    // Check if we're in test mode (mock auth)
    let test_mode = matches!(auth, Some(WorkflowAuth::Mock(_)));

    // Fetch contracts and metadata in parallel for all parties
    let futures: Vec<_> = my_parties
        .into_iter()
        .map(|(item, my_owner_key, p2p)| {
            let config = config.clone();
            let auth = auth.clone();
            let party_id_str = p2p.party.clone();
            async move {
                // Get token for this party from auth (real or mock)
                let token = match &auth {
                    Some(WorkflowAuth::Keycloak(registry)) => {
                        match registry.get_by_str(&party_id_str) {
                            Some(tm) => tm.get_token().await.ok(),
                            None => None,
                        }
                    }
                    Some(WorkflowAuth::Mock(mock_registry)) => {
                        Some(mock_registry.get_by_str(&party_id_str).get_token())
                    }
                    None => None,
                };

                let token_clone = token.clone();
                let (contracts, local_metadata) = tokio::join!(
                    async {
                        get_contracts(&config, &party_id_str, token, test_mode)
                            .await
                            .unwrap_or_else(|e| {
                                tracing::warn!("Failed to get contracts for {party_id_str}: {e}");
                                Vec::new()
                            })
                    },
                    async {
                        get_party_metadata(&config, &party_id_str, token_clone)
                            .await
                            .ok()
                            .flatten()
                    }
                );

                let party_id = CantonId::parse(&p2p.party)?;
                let participants = p2p
                    .participants
                    .iter()
                    .filter_map(|p| {
                        let participant_uid = CantonId::parse(&p.participant_uid).ok()?;
                        Some(ParticipantInfo {
                            participant_uid,
                            permission: Permission::from(p.permission),
                        })
                    })
                    .collect();

                Ok::<_, anyhow::Error>(DecentralizedParty {
                    party_id,
                    threshold: item.threshold,
                    owners: item.owners,
                    my_owner_key: Some(my_owner_key),
                    participants,
                    contracts,
                    local_metadata,
                })
            }
        })
        .collect();

    let results = futures::future::join_all(futures).await;
    let parties: Vec<_> = results.into_iter().filter_map(|r| r.ok()).collect();

    Ok(DecentralizedPartiesResponse { parties })
}

/// Check connectivity status of all participants
#[get("/participants-status")]
pub async fn get_participants_status(data: web::Data<AppState>) -> impl Responder {
    match check_participants_status(&data.config).await {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(e) => {
            tracing::error!("Failed to check participants status: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to check participants status: {e}")
            }))
        }
    }
}

async fn check_participants_status(config: &NodeConfig) -> Result<ParticipantsStatusResponse> {
    let network_config = config.load_network_config().await?;
    let current_participant_id = config.participant_id();
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let ping_message = Message::new_empty(MessageType::Ping);

    let mut status_futures = Vec::new();

    for peer in network_config.peers.iter() {
        let peer_id = peer.participant_id.to_string();
        let is_self = peer.participant_id == *current_participant_id;

        if is_self {
            status_futures.push(tokio::spawn(async move {
                ParticipantStatus {
                    id: peer_id,
                    status: ConnectionStatus::CurrentNode,
                }
            }));
            continue;
        }

        let peer_pub_key = parse_public_key(&peer.public_key).ok();
        let psk = peer_pub_key.map(|pk| keypair.derive_psk(&pk));
        let identity = keypair.public_key.serialize();
        let address = peer.address.clone();
        let port = peer.port;
        let ping_msg = ping_message.clone();

        status_futures.push(tokio::spawn(async move {
            // First check if node is reachable via TCP
            let socket_addr = format!("{address}:{port}");
            let tcp_check = tokio::time::timeout(
                Duration::from_secs(3),
                tokio::net::TcpStream::connect(&socket_addr),
            )
            .await;

            match tcp_check {
                Ok(Ok(_)) => {
                    // TCP connection succeeded, now check Noise handshake
                    let (Some(psk), Some(_)) = (psk, peer_pub_key) else {
                        // Invalid public key but node is reachable
                        return ParticipantStatus {
                            id: peer_id,
                            status: ConnectionStatus::HandshakeFailed,
                        };
                    };

                    match send_noise_message(&address, port, &psk, &identity, &ping_msg).await {
                        Ok(response) => {
                            let status = match Message::from_bytes(&response) {
                                Ok(msg) if msg.msg_type == MessageType::Pong => {
                                    ConnectionStatus::Connected
                                }
                                _ => ConnectionStatus::HandshakeFailed,
                            };
                            ParticipantStatus {
                                id: peer_id,
                                status,
                            }
                        }
                        Err(_) => ParticipantStatus {
                            id: peer_id,
                            status: ConnectionStatus::HandshakeFailed,
                        },
                    }
                }
                _ => {
                    // TCP connection failed - node is unreachable
                    ParticipantStatus {
                        id: peer_id,
                        status: ConnectionStatus::Unreachable,
                    }
                }
            }
        }));
    }

    let results = futures::future::join_all(status_futures).await;
    let statuses: Vec<ParticipantStatus> = results.into_iter().filter_map(|r| r.ok()).collect();

    Ok(ParticipantsStatusResponse { statuses })
}

/// Start a kick workflow to remove a participant from a decentralized party
#[post("/kick")]
pub async fn start_kick(
    data: web::Data<AppState>,
    kick_state: web::Data<Arc<KickWorkflowState>>,
    body: web::Json<KickRequest>,
) -> impl Responder {
    tracing::info!(
        "Kick request received: party={}, participant_to_kick={}, threshold={}",
        body.decentralized_party_id,
        body.participant_id,
        body.new_threshold
    );

    // Parse IDs first for validation
    let decentralized_party_id = match CantonId::parse(&body.decentralized_party_id) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("Invalid decentralized_party_id: {e}")
            }));
        }
    };

    let participant_id = match CantonId::parse(&body.participant_id) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("Invalid participant_id: {e}")
            }));
        }
    };

    // Prevent kicking ourselves
    if participant_id == *data.config.participant_id() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "Cannot kick yourself"
        }));
    }

    // Check if a kick is already in progress
    {
        let status = kick_state.status.read().await;
        if *status == KickStatus::InProgress {
            return HttpResponse::Conflict().json(serde_json::json!({
                "error": "A kick workflow is already in progress"
            }));
        }
    }

    // Update status to in progress
    {
        let mut status = kick_state.status.write().await;
        *status = KickStatus::InProgress;
        let mut error = kick_state.error.write().await;
        *error = None;
    }

    // Spawn the kick workflow in the background
    let config = data.config.clone();
    let kick_state_clone = kick_state.get_ref().clone();
    let namespace_fingerprint = body.namespace_fingerprint.clone();
    let new_threshold = body.new_threshold;
    let listener_control = data.noise_listener_control.clone();
    let listener_notify = data.noise_listener_notify.clone();

    tokio::spawn(async move {
        let guard = ListenerPauseGuard::pause(listener_control, listener_notify).await;

        // Send kick invites to all peers before starting coordinator workflow
        let invite_result = send_kick_invites(&config, &participant_id).await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send kick invites: {e}");
            guard.resume().await;
            let mut status = kick_state_clone.status.write().await;
            let mut error = kick_state_clone.error.write().await;
            *status = KickStatus::Failed;
            *error = Some(format!("Failed to send invites: {e}"));
            return;
        }

        // Give peers time to start their attestor workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let instance_name = format!("{}-kick-{timestamp}", decentralized_party_id.prefix);
        let kick_config = workflow::KickConfig::new(
            decentralized_party_id,
            participant_id,
            namespace_fingerprint,
            new_threshold,
            instance_name,
        );

        let result = workflow::start_coordinator(
            config,
            workflow::WorkflowType::Kick,
            None, // No onboarding config
            Some(kick_config),
            None, // No contracts config
            None, // No auth registry for kick
        )
        .await;

        guard.resume().await;

        let mut status = kick_state_clone.status.write().await;
        let mut error = kick_state_clone.error.write().await;

        match result {
            Ok(()) => {
                *status = KickStatus::Completed;
                tracing::info!("Kick workflow completed successfully");
            }
            Err(e) => {
                *status = KickStatus::Failed;
                *error = Some(format!("{e}"));
                tracing::error!("Kick workflow failed: {e}");
            }
        }
    });

    HttpResponse::Accepted().json(KickResponse {
        status: KickStatus::InProgress,
        message: "Kick workflow started".to_string(),
    })
}

/// Get the current status of the kick workflow
#[get("/kick/status")]
pub async fn get_kick_status(kick_state: web::Data<Arc<KickWorkflowState>>) -> impl Responder {
    let status = kick_state.status.read().await;
    let error = kick_state.error.read().await;

    HttpResponse::Ok().json(serde_json::json!({
        "status": *status,
        "error": *error,
    }))
}

/// State for tracking kick workflow
pub type KickWorkflowState = HttpWorkflowState<KickStatus>;

/// Check if Noise keys exist for this node
#[get("/keys/status")]
pub async fn get_key_status(data: web::Data<AppState>) -> impl Responder {
    let key_file = data.config.key_file_path();

    match NoiseKeypair::from_file(&key_file).await {
        Ok(keypair) => HttpResponse::Ok().json(KeyStatusResponse {
            has_keys: true,
            public_key: Some(keypair.public_key_hex()),
        }),
        Err(_) => HttpResponse::Ok().json(KeyStatusResponse {
            has_keys: false,
            public_key: None,
        }),
    }
}

/// Start an onboarding workflow to create a new decentralized party
#[post("/onboarding")]
pub async fn start_onboarding(
    data: web::Data<AppState>,
    onboarding_state: web::Data<Arc<OnboardingWorkflowState>>,
    body: web::Json<OnboardingRequest>,
) -> impl Responder {
    // Check if an onboarding is already in progress
    {
        let status = onboarding_state.status.read().await;
        if *status == OnboardingStatus::InProgress {
            return HttpResponse::Conflict().json(serde_json::json!({
                "error": "An onboarding workflow is already in progress"
            }));
        }
    }

    // Update status to in progress
    {
        let mut status = onboarding_state.status.write().await;
        *status = OnboardingStatus::InProgress;
        let mut error = onboarding_state.error.write().await;
        *error = None;
    }

    // Spawn the onboarding workflow in the background
    let config = data.config.clone();
    let onboarding_state_clone = onboarding_state.get_ref().clone();
    let listener_control = data.noise_listener_control.clone();
    let listener_notify = data.noise_listener_notify.clone();
    let party_id_prefix = body.party_id_prefix.clone();
    let peer_ids = body.peer_ids.clone();

    tokio::spawn(async move {
        let guard = ListenerPauseGuard::pause(listener_control, listener_notify).await;

        // Send invites to selected peers before starting coordinator workflow
        let invite_result = send_onboarding_invites(&config, &peer_ids).await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send onboarding invites: {e}");
            guard.resume().await;
            let mut status = onboarding_state_clone.status.write().await;
            let mut error = onboarding_state_clone.error.write().await;
            *status = OnboardingStatus::Failed;
            *error = Some(format!("Failed to send invites: {e}"));
            return;
        }

        // Give peers time to start their attestor workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let instance_name = format!("{party_id_prefix}-creation");
        let onboarding_config = workflow::OnboardingConfig::new(party_id_prefix, instance_name);

        let result = workflow::start_coordinator(
            config,
            workflow::WorkflowType::Onboarding,
            Some(onboarding_config),
            None, // No kick config
            None, // No contracts config
            None, // No auth registry for onboarding
        )
        .await;

        guard.resume().await;

        let mut status = onboarding_state_clone.status.write().await;
        let mut error = onboarding_state_clone.error.write().await;

        match result {
            Ok(()) => {
                *status = OnboardingStatus::Completed;
                tracing::info!("Onboarding workflow completed successfully");
            }
            Err(e) => {
                *status = OnboardingStatus::Failed;
                *error = Some(format!("{e}"));
                tracing::error!("Onboarding workflow failed: {e}");
            }
        }
    });

    HttpResponse::Accepted().json(OnboardingResponse {
        status: OnboardingStatus::InProgress,
        message: "Onboarding workflow started".to_string(),
    })
}

/// Send onboarding invites to selected peers using Noise protocol
async fn send_onboarding_invites(config: &NodeConfig, peer_ids: &[String]) -> Result {
    let network_config = config.load_network_config().await?;
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let invite_message = Message::new_empty(MessageType::InviteOnboarding);

    for peer_id in peer_ids {
        let peer = match network_config
            .peers
            .iter()
            .find(|p| p.participant_id.to_string() == *peer_id)
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
                if let Ok(msg) = Message::from_bytes(&response) {
                    if msg.msg_type == MessageType::Ack {
                        tracing::info!("Peer {peer_id} acknowledged invite");
                    } else {
                        tracing::warn!(
                            "Peer {peer_id} responded with {msg_type:?} instead of Ack",
                            msg_type = msg.msg_type
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to send invite to {peer_id}: {e}");
            }
        }
    }

    Ok(())
}

/// Send kick invites to all peers using Noise protocol (excluding the peer being kicked)
async fn send_kick_invites(config: &NodeConfig, kicked_participant: &CantonId) -> Result {
    let network_config = config.load_network_config().await?;
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let current_participant_id = config.participant_id();
    let invite_message = Message::new_empty(MessageType::InviteKick);

    tracing::info!(
        "Kick invites: self={}, kicked={}",
        current_participant_id,
        kicked_participant
    );

    for peer in &network_config.peers {
        tracing::debug!(
            "Checking peer {}: self_match={}, kicked_match={}",
            peer.participant_id,
            peer.participant_id == *current_participant_id,
            peer.participant_id == *kicked_participant
        );

        // Skip self
        if peer.participant_id == *current_participant_id {
            tracing::debug!("Skipping {} - this is self", peer.participant_id);
            continue;
        }

        // Skip the peer being kicked (they won't participate in the kick workflow)
        if peer.participant_id == *kicked_participant {
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
                if let Ok(msg) = Message::from_bytes(&response) {
                    if msg.msg_type == MessageType::Ack {
                        tracing::info!("Peer {} acknowledged kick invite", peer.participant_id);
                    } else {
                        tracing::warn!(
                            "Peer {} responded with {msg_type:?} instead of Ack",
                            peer.participant_id,
                            msg_type = msg.msg_type
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to send kick invite to {}: {e}", peer.participant_id);
            }
        }
    }

    Ok(())
}

/// Get the current status of the onboarding workflow
#[get("/onboarding/status")]
pub async fn get_onboarding_status(
    onboarding_state: web::Data<Arc<OnboardingWorkflowState>>,
) -> impl Responder {
    let status = onboarding_state.status.read().await;
    let error = onboarding_state.error.read().await;

    HttpResponse::Ok().json(serde_json::json!({
        "status": *status,
        "error": *error,
    }))
}

/// State for tracking onboarding workflow
pub type OnboardingWorkflowState = HttpWorkflowState<OnboardingStatus>;

/// State for tracking contracts workflow
pub type ContractsWorkflowState = HttpWorkflowState<WorkflowProgress>;

/// Start a contracts workflow to upload DARs and create contracts
#[post("/contracts")]
pub async fn start_contracts(
    data: web::Data<AppState>,
    contracts_state: web::Data<Arc<ContractsWorkflowState>>,
    body: web::Json<ContractsRequest>,
) -> impl Responder {
    // Check if a contracts workflow is already in progress
    {
        let status = contracts_state.status.read().await;
        if *status == WorkflowProgress::InProgress {
            return HttpResponse::Conflict().json(serde_json::json!({
                "error": "A contracts workflow is already in progress"
            }));
        }
    }

    // Update status to in progress
    {
        let mut status = contracts_state.status.write().await;
        *status = WorkflowProgress::InProgress;
        let mut error = contracts_state.error.write().await;
        *error = None;
    }

    // Create contracts config from request
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let instance_name = format!(
        "{}-contracts-{timestamp}",
        body.decentralized_party_id.prefix
    );
    let contracts_config = workflow::ContractsConfig::new(
        body.decentralized_party_id.clone(),
        body.participant_ids.clone(),
        body.participant_parties.clone(),
        body.operator_party.clone(),
        body.dar_files.clone(),
        body.contracts.clone(),
        instance_name,
    );

    // Spawn the contracts workflow in the background
    let config = data.config.clone();
    let workflow_auth = data.auth.clone();
    let contracts_state_clone = contracts_state.get_ref().clone();
    let listener_control = data.noise_listener_control.clone();
    let listener_notify = data.noise_listener_notify.clone();

    tokio::spawn(async move {
        let guard = ListenerPauseGuard::pause(listener_control, listener_notify).await;

        // Send invites to all peers before starting coordinator workflow
        let invite_result = send_contracts_invites(&config).await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send contracts invites: {e}");
            guard.resume().await;
            let mut status = contracts_state_clone.status.write().await;
            let mut error = contracts_state_clone.error.write().await;
            *status = WorkflowProgress::Failed;
            *error = Some(format!("Failed to send invites: {e}"));
            return;
        }

        // Give peers time to start their attestor workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let result = workflow::start_coordinator(
            config,
            workflow::WorkflowType::Contracts,
            None, // No onboarding config
            None, // No kick config
            Some(contracts_config),
            workflow_auth,
        )
        .await;

        guard.resume().await;

        let mut status = contracts_state_clone.status.write().await;
        let mut error = contracts_state_clone.error.write().await;

        match result {
            Ok(()) => {
                *status = WorkflowProgress::Completed;
                tracing::info!("Contracts workflow completed successfully");
            }
            Err(e) => {
                *status = WorkflowProgress::Failed;
                *error = Some(format!("{e}"));
                tracing::error!("Contracts workflow failed: {e}");
            }
        }
    });

    HttpResponse::Accepted().json(WorkflowResponse {
        status: WorkflowProgress::InProgress,
        message: "Contracts workflow started".to_string(),
    })
}

/// Send contracts invites to all peers using Noise protocol
async fn send_contracts_invites(config: &NodeConfig) -> Result {
    let network_config = config.load_network_config().await?;
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let current_participant_id = config.participant_id();
    let invite_message = Message::new_empty(MessageType::InviteContracts);

    for peer in &network_config.peers {
        if peer.participant_id == *current_participant_id {
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
                if let Ok(msg) = Message::from_bytes(&response) {
                    if msg.msg_type == MessageType::Ack {
                        tracing::info!(
                            "Peer {} acknowledged contracts invite",
                            peer.participant_id
                        );
                    } else {
                        tracing::warn!(
                            "Peer {} responded with {msg_type:?} instead of Ack",
                            peer.participant_id,
                            msg_type = msg.msg_type
                        );
                    }
                }
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

/// Get the current status of the contracts workflow
#[get("/contracts/status")]
pub async fn get_contracts_status(
    contracts_state: web::Data<Arc<ContractsWorkflowState>>,
) -> impl Responder {
    let status = contracts_state.status.read().await;
    let error = contracts_state.error.read().await;

    HttpResponse::Ok().json(serde_json::json!({
        "status": *status,
        "error": *error,
    }))
}

/// Get all pending invitations
#[get("/invitations")]
pub async fn get_invitations(data: web::Data<AppState>) -> impl Responder {
    let invitations = data.pending_invitations.read().await;

    // Try to resolve coordinator names from network config
    let network_config = data.config.load_network_config().await.ok();
    let invitations_with_names: Vec<PendingInvitation> = invitations
        .iter()
        .map(|inv| {
            let coordinator_name = network_config.as_ref().and_then(|nc| {
                nc.peers
                    .iter()
                    .find(|p| p.public_key == inv.coordinator_pubkey)
                    .map(|p| p.name.clone())
            });
            PendingInvitation {
                coordinator_name,
                ..inv.clone()
            }
        })
        .collect();

    HttpResponse::Ok().json(PendingInvitationsResponse {
        invitations: invitations_with_names,
    })
}

/// Accept a pending invitation and trigger the workflow
#[post("/invitations/accept")]
pub async fn accept_invitation(
    data: web::Data<AppState>,
    body: web::Json<InvitationActionRequest>,
) -> impl Responder {
    let invitation = {
        let mut invitations = data.pending_invitations.write().await;
        let idx = invitations.iter().position(|i| i.id == body.id);
        match idx {
            Some(i) => invitations.remove(i),
            None => {
                return HttpResponse::NotFound().json(serde_json::json!({
                    "error": "Invitation not found"
                }));
            }
        }
    };

    // Store coordinator's public key and trigger the appropriate workflow
    {
        let mut coordinator_pubkey = data.coordinator_pubkey.write().await;
        *coordinator_pubkey = Some(invitation.coordinator_pubkey.clone());
    }

    match invitation.invitation_type {
        InvitationType::Onboarding => {
            tracing::info!("Accepting onboarding invitation, triggering attestor workflow");
            data.onboarding_trigger.notify_one();
        }
        InvitationType::Kick => {
            tracing::info!("Accepting kick invitation, triggering attestor workflow");
            data.kick_trigger.notify_one();
        }
        InvitationType::Contracts => {
            tracing::info!("Accepting contracts invitation, triggering attestor workflow");
            data.contracts_trigger.notify_one();
        }
    }

    HttpResponse::Ok().json(serde_json::json!({
        "message": "Invitation accepted"
    }))
}

/// Decline a pending invitation
#[post("/invitations/decline")]
pub async fn decline_invitation(
    data: web::Data<AppState>,
    body: web::Json<InvitationActionRequest>,
) -> impl Responder {
    let mut invitations = data.pending_invitations.write().await;
    let idx = invitations.iter().position(|i| i.id == body.id);

    match idx {
        Some(i) => {
            invitations.remove(i);
            tracing::info!("Declined invitation {}", body.id);
            HttpResponse::Ok().json(serde_json::json!({
                "message": "Invitation declined"
            }))
        }
        None => HttpResponse::NotFound().json(serde_json::json!({
            "error": "Invitation not found"
        })),
    }
}

/// Check authentication status for all configured parties
#[get("/auth/status")]
pub async fn get_auth_status(data: web::Data<AppState>) -> impl Responder {
    let mut party_statuses = Vec::new();

    // Handle test mode - return mock status
    if let Some(WorkflowAuth::Mock(ref mock_registry)) = data.auth {
        let manager = mock_registry.get_by_str("");
        party_statuses.push(PartyAuthStatus {
            dec_party_id: "(test mode)".to_string(),
            member_party_id: "(test mode)".to_string(),
            user_id: manager.user_id().to_string(),
            keycloak_url: None,
            keycloak_realm: None,
            status: AuthStatus::Mock,
            rights: None,
        });
        return HttpResponse::Ok().json(AuthStatusResponse {
            parties: party_statuses,
        });
    }

    // Check each configured party
    for party_creds in &data.config.parties {
        let dec_party_id = party_creds.dec_party_id.to_string();
        let member_party_id = party_creds.member_party_id.to_string();
        let user_id = party_creds.user_id.clone();

        // Try to get a token from the auth registry
        let (status, token) = match &data.auth {
            Some(WorkflowAuth::Keycloak(registry)) => {
                match registry.get(&party_creds.dec_party_id) {
                    Some(tm) => match tm.get_token().await {
                        Ok(t) => (AuthStatus::Authenticated, Some(t)),
                        Err(e) => (
                            AuthStatus::Failed {
                                error: e.to_string(),
                            },
                            None,
                        ),
                    },
                    None => (AuthStatus::NotConfigured, None),
                }
            }
            _ => (AuthStatus::NotConfigured, None),
        };

        // Check user rights if we have a valid token
        let rights = if let Some(ref t) = token {
            check_user_rights(&data.config, t, &user_id, &member_party_id, &dec_party_id)
                .await
                .ok()
        } else {
            None
        };

        party_statuses.push(PartyAuthStatus {
            dec_party_id,
            member_party_id,
            user_id,
            keycloak_url: Some(party_creds.keycloak.url.clone()),
            keycloak_realm: Some(party_creds.keycloak.realm.clone()),
            status,
            rights,
        });
    }

    HttpResponse::Ok().json(AuthStatusResponse {
        parties: party_statuses,
    })
}

/// Extract user_id (sub claim) from JWT token
fn extract_user_id_from_jwt(token: &str) -> Option<String> {
    // JWT format: header.payload.signature
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }

    // Decode the payload (second part) - URL-safe base64 without padding
    let payload = parts[1];
    let padding_needed = (4 - (payload.len() % 4)) % 4;
    let padded = if padding_needed > 0 {
        format!("{}{}", payload, "=".repeat(padding_needed))
    } else {
        payload.to_string()
    };

    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&padded)
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    json.get("sub").and_then(|v| v.as_str()).map(String::from)
}

/// Check user rights for both member party and decentralized party
async fn check_user_rights(
    config: &NodeConfig,
    token: &str,
    user_id: &str,
    member_party_id: &str,
    dec_party_id: &str,
) -> Result<RightsStatus> {
    let mut client = utils::create_user_client(config, Some(token.to_string())).await?;

    // For M2M auth, the actual user_id in Canton is from JWT's 'sub' claim
    let effective_user_id = extract_user_id_from_jwt(token).unwrap_or_else(|| user_id.to_string());

    tracing::debug!(
        "Checking rights for user_id={effective_user_id} (configured: {user_id}), member_party={member_party_id}, dec_party={dec_party_id}"
    );

    let response = client
        .list_user_rights(tonic::Request::new(ListUserRightsRequest {
            user_id: effective_user_id.clone(),
            identity_provider_id: String::new(),
        }))
        .await?
        .into_inner();

    tracing::debug!(
        "ListUserRights for {effective_user_id} returned {} rights",
        response.rights.len()
    );

    let mut member_party_act_as = false;
    let mut member_party_read_as = false;
    let mut dec_party_act_as = false;
    let mut dec_party_read_as = false;

    for right in response.rights {
        match right.kind {
            Some(Kind::CanActAs(CanActAs { ref party })) => {
                tracing::debug!("  CanActAs: {party}");
                if party == member_party_id {
                    member_party_act_as = true;
                }
                if party == dec_party_id {
                    dec_party_act_as = true;
                }
            }
            Some(Kind::CanReadAs(CanReadAs { ref party })) => {
                tracing::debug!("  CanReadAs: {party}");
                if party == member_party_id {
                    member_party_read_as = true;
                }
                if party == dec_party_id {
                    dec_party_read_as = true;
                }
            }
            _ => {}
        }
    }

    Ok(RightsStatus {
        member_party_act_as,
        member_party_read_as,
        dec_party_act_as,
        dec_party_read_as,
    })
}

/// Test authentication by attempting to get a fresh token
#[post("/auth/test")]
pub async fn test_auth(data: web::Data<AppState>) -> impl Responder {
    let mut results = Vec::new();

    // Handle test mode - mock auth always succeeds
    if matches!(data.auth, Some(WorkflowAuth::Mock(_))) {
        results.push(AuthTestResult {
            party_id: "(test mode)".to_string(),
            success: true,
            error: None,
        });
        return HttpResponse::Ok().json(AuthTestResponse { results });
    }

    for party_creds in &data.config.parties {
        let dec_party_id = party_creds.dec_party_id.to_string();

        // Attempt fresh authentication
        let result = test_keycloak_auth(&party_creds.keycloak).await;

        results.push(AuthTestResult {
            party_id: dec_party_id,
            success: result.is_ok(),
            error: result.err(),
        });
    }

    HttpResponse::Ok().json(AuthTestResponse { results })
}

async fn test_keycloak_auth(
    config: &crate::config::KeycloakConfig,
) -> std::result::Result<(), String> {
    let url = keycloak::login::password_url(&config.url, &config.realm);

    // Use client_credentials if client_secret is set, otherwise password flow
    if let Some(ref client_secret) = config.client_secret {
        keycloak::login::client_credentials(keycloak::login::ClientCredentialsParams {
            url,
            client_id: config.client_id.clone(),
            client_secret: client_secret.clone(),
        })
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
    } else {
        let username = config
            .username
            .as_ref()
            .ok_or_else(|| "Missing username for password flow".to_string())?;
        let password = config
            .password
            .as_ref()
            .ok_or_else(|| "Missing password for password flow".to_string())?;

        keycloak::login::password(keycloak::login::PasswordParams {
            client_id: config.client_id.clone(),
            username: username.clone(),
            password: password.clone(),
            url,
        })
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
    }
}

// ============================================================================
// Governance Endpoints
// ============================================================================

/// Query parameters for governance confirmations endpoint
#[derive(Debug, Deserialize)]
pub struct GovernanceQuery {
    pub party_id: String,
}

/// Get governance confirmations with parsed actions
#[get("/governance/confirmations")]
pub async fn get_governance(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = match CantonId::parse(&query.party_id) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("Invalid party_id: {e}")
            }));
        }
    };

    // Get token for this party
    let token = get_party_token(&data, &party_id).await;

    // Check if we're in test mode (mock auth)
    let test_mode = matches!(data.auth, Some(WorkflowAuth::Mock(_)));

    // Get threshold for this party (default to 2 if not found)
    let threshold = get_party_threshold(&data, &query.party_id)
        .await
        .unwrap_or(2);

    match get_governance_confirmations(&data.config, &query.party_id, threshold, token, test_mode)
        .await
    {
        Ok(actions) => HttpResponse::Ok().json(GovernanceResponse { actions, threshold }),
        Err(e) => {
            tracing::error!("Failed to fetch governance confirmations: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to fetch governance confirmations: {e}")
            }))
        }
    }
}

/// Get governance state (VaultGovernanceRules contract state)
#[get("/governance/state")]
pub async fn get_governance_state(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = match CantonId::parse(&query.party_id) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("Invalid party_id: {e}")
            }));
        }
    };

    // Get token for this party
    let token = get_party_token(&data, &party_id).await;

    // Check if we're in test mode (mock auth)
    let test_mode = matches!(data.auth, Some(WorkflowAuth::Mock(_)));

    match query_governance_state(&data.config, &query.party_id, token, test_mode).await {
        Ok(state) => HttpResponse::Ok().json(GovernanceStateResponse { state }),
        Err(e) => {
            tracing::error!("Failed to fetch governance state: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to fetch governance state: {e}")
            }))
        }
    }
}

/// Get deployed Vault contracts
#[get("/vaults")]
pub async fn get_vaults_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = match CantonId::parse(&query.party_id) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("Invalid party_id: {e}")
            }));
        }
    };

    // Get token for this party
    let token = get_party_token(&data, &party_id).await;

    // Check if we're in test mode (mock auth)
    let test_mode = matches!(data.auth, Some(WorkflowAuth::Mock(_)));

    match get_vaults(&data.config, &query.party_id, token, test_mode).await {
        Ok(vaults) => HttpResponse::Ok().json(VaultsResponse { vaults }),
        Err(e) => {
            tracing::error!("Failed to fetch vaults: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to fetch vaults: {e}")
            }))
        }
    }
}

/// Get ProviderService contracts
#[get("/services/provider")]
pub async fn get_provider_services_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = match CantonId::parse(&query.party_id) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("Invalid party_id: {e}")
            }));
        }
    };

    let token = get_party_token(&data, &party_id).await;
    let test_mode = matches!(data.auth, Some(WorkflowAuth::Mock(_)));

    match get_provider_services(&data.config, &query.party_id, token, test_mode).await {
        Ok(services) => HttpResponse::Ok().json(ProviderServicesResponse { services }),
        Err(e) => {
            tracing::error!("Failed to fetch provider services: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to fetch provider services: {e}")
            }))
        }
    }
}

/// Get UserService contracts
#[get("/services/user")]
pub async fn get_user_services_handler(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = match CantonId::parse(&query.party_id) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("Invalid party_id: {e}")
            }));
        }
    };

    let token = get_party_token(&data, &party_id).await;
    let test_mode = matches!(data.auth, Some(WorkflowAuth::Mock(_)));

    match get_user_services(&data.config, &query.party_id, token, test_mode).await {
        Ok(services) => HttpResponse::Ok().json(UserServicesResponse { services }),
        Err(e) => {
            tracing::error!("Failed to fetch user services: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to fetch user services: {e}")
            }))
        }
    }
}

/// Get token for a party from auth registry
async fn get_party_token(data: &web::Data<AppState>, party_id: &CantonId) -> Option<String> {
    match &data.auth {
        Some(WorkflowAuth::Keycloak(registry)) => registry.get(party_id)?.get_token().await.ok(),
        Some(WorkflowAuth::Mock(mock_registry)) => Some(mock_registry.get(party_id).get_token()),
        None => None,
    }
}

/// Get threshold for a decentralized party
async fn get_party_threshold(data: &web::Data<AppState>, party_id: &str) -> Option<usize> {
    // Extract namespace from party_id
    let namespace = party_id.rsplit_once("::")?.1;

    let channel = tonic::transport::Channel::from_shared(data.config.admin_api_url())
        .ok()?
        .connect()
        .await
        .ok()?;

    let mut topology_client = TopologyManagerReadServiceClient::new(channel)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let synchronizer_id = utils::get_synchronizer_id(&data.config).await.ok()?;

    let response = topology_client
        .list_decentralized_namespace_definition(tonic::Request::new(
            ListDecentralizedNamespaceDefinitionRequest {
                base_query: Some(BaseQuery {
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
                }),
                filter_namespace: namespace.to_string(),
            },
        ))
        .await
        .ok()?
        .into_inner();

    response
        .results
        .first()
        .and_then(|r| r.item.as_ref())
        .map(|item| item.threshold as usize)
}

// ============================================================================
// Governance Endpoints (Structured Actions)
// ============================================================================

/// Submit a confirmation for a governance action using structured ActionType
#[post("/governance/confirm")]
pub async fn confirm_action(
    data: web::Data<AppState>,
    body: web::Json<ConfirmActionRequest>,
) -> impl Responder {
    let party_id = &body.party_id;

    // Get token and credentials for this party
    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Some(creds) => creds,
        None => {
            return HttpResponse::Unauthorized().json(serde_json::json!({
                "error": "No credentials configured for party"
            }));
        }
    };

    match execute_confirm_action(&data.config, &body, &token, &member_party_id).await {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({
            "message": "Confirmation submitted successfully"
        })),
        Err(e) => {
            tracing::error!("Failed to submit confirmation: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to submit confirmation: {e}")
            }))
        }
    }
}

/// Execute a confirmed governance action using structured ActionType
#[post("/governance/execute")]
pub async fn execute_action(
    data: web::Data<AppState>,
    body: web::Json<ExecuteActionRequest>,
) -> impl Responder {
    let party_id = &body.party_id;

    // Get token and credentials for this party
    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Some(creds) => creds,
        None => {
            return HttpResponse::Unauthorized().json(serde_json::json!({
                "error": "No credentials configured for party"
            }));
        }
    };

    match execute_confirmed_action(&data.config, &body, &token, &member_party_id).await {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({
            "message": "Action executed successfully"
        })),
        Err(e) => {
            tracing::error!("Failed to execute action: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to execute action: {e}")
            }))
        }
    }
}

/// Expire a stale governance confirmation
#[post("/governance/expire")]
pub async fn expire_confirmation(
    data: web::Data<AppState>,
    body: web::Json<ExpireConfirmationRequest>,
) -> impl Responder {
    let party_id = &body.party_id;

    // Get token and credentials for this party
    let (token, member_party_id) = match get_party_credentials(&data, party_id).await {
        Some(creds) => creds,
        None => {
            return HttpResponse::Unauthorized().json(serde_json::json!({
                "error": "No credentials configured for party"
            }));
        }
    };

    match execute_expire_confirmation(&data.config, &body, &token, &member_party_id).await {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({
            "message": "Confirmation expired successfully"
        })),
        Err(e) => {
            tracing::error!("Failed to expire confirmation: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to expire confirmation: {e}")
            }))
        }
    }
}

/// Get token and member_party_id for a party
async fn get_party_credentials(
    data: &web::Data<AppState>,
    party_id: &CantonId,
) -> Option<(String, String)> {
    match &data.auth {
        Some(WorkflowAuth::Keycloak(registry)) => {
            let tm = registry.get(party_id)?;
            let token = tm.get_token().await.ok()?;
            Some((token, tm.member_party_id().to_string()))
        }
        Some(WorkflowAuth::Mock(mock_registry)) => {
            let mm = mock_registry.get(party_id);
            Some((mm.get_token(), mm.member_party_id().to_string()))
        }
        None => None,
    }
}

/// Execute ConfirmAction choice on VaultGovernanceRules contract with structured action
async fn execute_confirm_action(
    config: &NodeConfig,
    request: &ConfirmActionRequest,
    token: &str,
    member_party_id: &str,
) -> Result {
    let channel = tonic::transport::Channel::from_shared(config.ledger_api_url())?
        .connect()
        .await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let template_id = Identifier {
        package_id: VAULT_GOVERNANCE_PACKAGE_ID.to_string(),
        module_name: "BitsafeVault.VaultGovernance".to_string(),
        entity_name: "VaultGovernanceRules".to_string(),
    };

    // Build choice argument using action_serializer
    let choice_argument =
        action_serializer::build_confirm_action_argument(member_party_id, &request.action);

    let cmd = Command {
        command: Some(command::Command::Exercise(ExerciseCommand {
            template_id: Some(template_id),
            contract_id: request.rules_contract_id.clone(),
            choice: "VaultGovernanceRules_ConfirmAction".to_string(),
            choice_argument: Some(choice_argument),
        })),
    };

    let commands = Commands {
        workflow_id: String::new(),
        user_id: String::new(),
        command_id: uuid::Uuid::new_v4().to_string(),
        commands: vec![cmd],
        deduplication_period: None,
        min_ledger_time_abs: None,
        min_ledger_time_rel: None,
        act_as: vec![member_party_id.to_string()],
        read_as: vec![request.party_id.to_string()],
        submission_id: String::new(),
        disclosed_contracts: vec![],
        synchronizer_id: String::new(),
        package_id_selection_preference: vec![],
        prefetch_contract_keys: vec![],
    };

    let mut req = tonic::Request::new(SubmitAndWaitRequest {
        commands: Some(commands),
    });
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());

    client.submit_and_wait(req).await?;

    Ok(())
}

/*
macro_rules! disclose {
    ($contract_id:expr, $disclosed_contracts:expr) => DisclosedContract {
        template_id: None,
        contract_id: vault_rules_cid.clone(),
        created_event_blob: base64::engine::general_purpose::STANDARD
            .decode("CgMyLjES5gQKRQAZlM04PpOCb1MKGx+pYtIWrHhZ5I2jc8fHky5FNSAikcoSEiA7G+6/yN41XjcZEgveRn9rzMg+z2letvss/k9lgddSPhIUYml0c2FmZS12YXVsdC12MC1yYzIaaApAODkyZjdiNjRkMzgxNDI5N2ZhNTMwYjBhMDgzZDMwZjRiMDY0OWFlOTEzMzY0NTk3NDBlN2M2M2RjMGYyNmYyZBIMQml0c2FmZVZhdWx0EgpWYXVsdFJ1bGVzGgpWYXVsdFJ1bGVzIrsBargBClcKVTpTYml0c2FmZS1hZG1pbjo6MTIyMDk5OTUzOTM0ZDlmZTE2M2ZlZDA3ZGQzNzFmYTEzOTgyYjJiMzA3NDlkNmRmNTZlY2RiYTM4NWY4Yzc4YTg2N2EKXQpbWlkKVzpVdmF1bHQtbWFuYWdlci0wOjoxMjIwOTk5NTM5MzRkOWZlMTYzZmVkMDdkZDM3MWZhMTM5ODJiMmIzMDc0OWQ2ZGY1NmVjZGJhMzg1ZjhjNzhhODY3YSpTYml0c2FmZS1hZG1pbjo6MTIyMDk5OTUzOTM0ZDlmZTE2M2ZlZDA3ZGQzNzFmYTEzOTgyYjJiMzA3NDlkNmRmNTZlY2RiYTM4NWY4Yzc4YTg2N2EyVXZhdWx0LW1hbmFnZXItMDo6MTIyMDk5OTUzOTM0ZDlmZTE2M2ZlZDA3ZGQzNzFmYTEzOTgyYjJiMzA3NDlkNmRmNTZlY2RiYTM4NWY4Yzc4YTg2N2E5w/1sZ/xIBgBCKgomCiQIARIgTmYuGZgxVotyEsMCqSiw44/3agOtvJKn9BHynYzXZWoQHg==").expect("Invalid base64 blob"),
        synchronizer_id: String::new(),
    }
}*/

/// Execute ExecuteConfirmedAction choice on VaultGovernanceRules contract with structured action
async fn execute_confirmed_action(
    config: &NodeConfig,
    request: &ExecuteActionRequest,
    token: &str,
    member_party_id: &str,
) -> Result {
    let channel = tonic::transport::Channel::from_shared(config.ledger_api_url())?
        .connect()
        .await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let template_id = Identifier {
        package_id: VAULT_GOVERNANCE_PACKAGE_ID.to_string(),
        module_name: "BitsafeVault.VaultGovernance".to_string(),
        entity_name: "VaultGovernanceRules".to_string(),
    };

    // Build choice argument using action_serializer
    let choice_argument = action_serializer::build_execute_action_argument(
        member_party_id,
        &request.action,
        &request.confirmation_cids,
        None, // contractCid is optional, typically None for execute
    );

    let cmd = Command {
        command: Some(command::Command::Exercise(ExerciseCommand {
            template_id: Some(template_id),
            contract_id: request.rules_contract_id.clone(),
            choice: "VaultGovernanceRules_ExecuteConfirmedAction".to_string(),
            choice_argument: Some(choice_argument),
        })),
    };

    let commands = Commands {
        workflow_id: String::new(),
        user_id: String::new(),
        command_id: uuid::Uuid::new_v4().to_string(),
        commands: vec![cmd],
        deduplication_period: None,
        min_ledger_time_abs: None,
        min_ledger_time_rel: None,
        act_as: vec![member_party_id.to_string()],
        read_as: vec![request.party_id.to_string()],
        submission_id: String::new(),
        disclosed_contracts: match &request.action {
            ActionType::DevNetFeatureApp { amulet_rules_cid } => vec![DisclosedContract {
                template_id: None,
                contract_id: amulet_rules_cid.clone(),
                created_event_blob: base64::engine::general_purpose::STANDARD
                    .decode("CgMyLjESsQ4KRQCeWuQZiFi479pjMwp/p+/f0LM0lXW6CP/BNOhgF02MX8oSEiD6tnb4BtBzehw1XkegzSaSyNKmuNlguE/Jog0G0eM9VRINc3BsaWNlLWFtdWxldBpkCkAzY2ExMzQzYWIyNmI0NTNkMzhjOGFkYjcwZGNhNWYxZWFkODQ0MGM0MmI1OWI2OGYwNzA3ODY5NTVjYmY5ZWMxEgZTcGxpY2USC0FtdWxldFJ1bGVzGgtBbXVsZXRSdWxlcyLyC2rvCwpNCks6SURTTzo6MTIyMGJlNThjMjllNjVkZTQwYmYyNzNiZTFkYzJiMjY2ZDQzYTlhMDAyZWE1YjE4OTU1YWVlZjdhYWM4ODFiYjQ3MWEKlwsKlAtqkQsKiAsKhQtqggsKkgEKjwFqjAEKFgoUahIKEAoOMgwwLjAwMDAwMDAwMDAKFgoUahIKEAoOMgwwLjAwMDAxOTAyNTkKHAoaahgKEAoOMgwwLjAwMDAwMDAwMDAKBAoCWgAKFgoUahIKEAoOMgwwLjAwMDAwMDAwMDAKEAoOMgwxLjAwMDAwMDAwMDAKBQoDGMgBCgUKAxjIAQoECgIYZArhBgreBmrbBgqYAQqVAWqSAQoaChgyFjQwMDAwMDAwMDAwLjAwMDAwMDAwMDAKEAoOMgwwLjA1MDAwMDAwMDAKEAoOMgwwLjE1MDAwMDAwMDAKEAoOMgwwLjIwMDAwMDAwMDAKFAoSMhAyMDAwMC4wMDAwMDAwMDAwChAKDjIMMC42MDAwMDAwMDAwChYKFFISChAyDjU3MC4wMDAwMDAwMDAwCr0FCroFWrcFCq4BaqsBChAKDmoMCgoKCBiAwM/g6JUHCpYBCpMBapABChoKGDIWMjAwMDAwMDAwMDAuMDAwMDAwMDAwMAoQCg4yDDAuMTIwMDAwMDAwMAoQCg4yDDAuNDAwMDAwMDAwMAoQCg4yDDAuMjAwMDAwMDAwMAoUChIyEDIwMDAwLjAwMDAwMDAwMDAKEAoOMgwwLjYwMDAwMDAwMDAKFAoSUhAKDjIMMy4zMzAwMDAwMDAwCqoBaqcBChAKDmoMCgoKCBiAwO6husEVCpIBCo8BaowBChoKGDIWMTAwMDAwMDAwMDAuMDAwMDAwMDAwMAoQCg4yDDAuMTgwMDAwMDAwMAoQCg4yDDAuNjIwMDAwMDAwMAoQCg4yDDAuMjAwMDAwMDAwMAoQCg4yDDEuNTAwMDAwMDAwMAoQCg4yDDAuNjAwMDAwMDAwMAoUChJSEAoOMgwzLjMzMDAwMDAwMDAKqQFqpgEKEAoOagwKCgoIGICAm8aX2kcKkQEKjgFqiwEKGQoXMhU1MDAwMDAwMDAwLjAwMDAwMDAwMDAKEAoOMgwwLjIxMDAwMDAwMDAKEAoOMgwwLjY5MDAwMDAwMDAKEAoOMgwwLjIwMDAwMDAwMDAKEAoOMgwxLjUwMDAwMDAwMDAKEAoOMgwwLjYwMDAwMDAwMDAKFAoSUhAKDjIMMy4zMzAwMDAwMDAwCqoBaqcBChEKD2oNCgsKCRiAgLaMr7SPAQqRAQqOAWqLAQoZChcyFTI1MDAwMDAwMDAuMDAwMDAwMDAwMAoQCg4yDDAuMjAwMDAwMDAwMAoQCg4yDDAuNzUwMDAwMDAwMAoQCg4yDDAuMjAwMDAwMDAwMAoQCg4yDDEuNTAwMDAwMDAwMAoQCg4yDDAuNjAwMDAwMDAwMAoUChJSEAoOMgwzLjMzMDAwMDAwMDAKjQIKigJqhwIKZwplamMKYQpfYl0KWwpVQlNnbG9iYWwtZG9tYWluOjoxMjIwYmU1OGMyOWU2NWRlNDBiZjI3M2JlMWRjMmIyNjZkNDNhOWEwMDJlYTViMTg5NTVhZWVmN2FhYzg4MWJiNDcxYRICCgAKVwpVQlNnbG9iYWwtZG9tYWluOjoxMjIwYmU1OGMyOWU2NWRlNDBiZjI3M2JlMWRjMmIyNjZkNDNhOWEwMDJlYTViMTg5NTVhZWVmN2FhYzg4MWJiNDcxYQpDCkFqPwocChpqGAoGCgQYgOowCg4KDGoKCggKBhiAsLT4CAoRCg8yDTYwLjAwMDAwMDAwMDAKBAoCGAgKBgoEGIC1GAoOCgxqCgoICgYYgJiavAQKSwpJakcKCgoIQgYwLjEuMTQKCgoIQgYwLjEuMTUKCgoIQgYwLjEuMjAKCQoHQgUwLjEuNQoKCghCBjAuMS4xNAoKCghCBjAuMS4xNAoECgJSAAoUChJSEAoOMgwxLjAwMDAwMDAwMDAKBAoCWgAKBAoCEAEqSURTTzo6MTIyMGJlNThjMjllNjVkZTQwYmYyNzNiZTFkYzJiMjY2ZDQzYTlhMDAyZWE1YjE4OTU1YWVlZjdhYWM4ODFiYjQ3MWE5tgHghutIBgBCKgomCiQIARIg2Md4zgUbR/RJCvIxvewOt7EiYUX/d9m6BxFNwoaw4CYQHg==")
                    .expect("Invalid base64 blob"),
                synchronizer_id: String::new(),
            }],
            ActionType::VaultDeployment { vault_rules_cid, .. } => vec![DisclosedContract {
                template_id: None,
                contract_id: vault_rules_cid.clone(),
                created_event_blob: base64::engine::general_purpose::STANDARD
                    .decode("CgMyLjESjwYKRQCMHFjbebg9gs894ojdp4mvHetxBGZJlb4bsG31+th3w8oSEiCYneqOgmkJLin8CWoocGruD6KiiDGIKysaY7nbvnC2LBIUYml0c2FmZS12YXVsdC12MC1yYzIaaApAODkyZjdiNjRkMzgxNDI5N2ZhNTMwYjBhMDgzZDMwZjRiMDY0OWFlOTEzMzY0NTk3NDBlN2M2M2RjMGYyNmYyZBIMQml0c2FmZVZhdWx0EgpWYXVsdFJ1bGVzGgpWYXVsdFJ1bGVzIpICao8CClcKVTpTYml0c2FmZS1hZG1pbjo6MTIyMDk5OTUzOTM0ZDlmZTE2M2ZlZDA3ZGQzNzFmYTEzOTgyYjJiMzA3NDlkNmRmNTZlY2RiYTM4NWY4Yzc4YTg2N2EKswEKsAFarQEKUjpQdmF1bHQtdGVzdDo6MTIyMGQxMmI4YTRlMmY0NDBkODZmNDY3NjRhZmNmYzhkOTkwOGRlMzVmOTBiMzZiYzE5ZWZjZDhlMTk4NjA2Yzk0ZDIKVzpVdmF1bHQtbWFuYWdlci0wOjoxMjIwOTk5NTM5MzRkOWZlMTYzZmVkMDdkZDM3MWZhMTM5ODJiMmIzMDc0OWQ2ZGY1NmVjZGJhMzg1ZjhjNzhhODY3YSpTYml0c2FmZS1hZG1pbjo6MTIyMDk5OTUzOTM0ZDlmZTE2M2ZlZDA3ZGQzNzFmYTEzOTgyYjJiMzA3NDlkNmRmNTZlY2RiYTM4NWY4Yzc4YTg2N2EyVXZhdWx0LW1hbmFnZXItMDo6MTIyMDk5OTUzOTM0ZDlmZTE2M2ZlZDA3ZGQzNzFmYTEzOTgyYjJiMzA3NDlkNmRmNTZlY2RiYTM4NWY4Yzc4YTg2N2EyUHZhdWx0LXRlc3Q6OjEyMjBkMTJiOGE0ZTJmNDQwZDg2ZjQ2NzY0YWZjZmM4ZDk5MDhkZTM1ZjkwYjM2YmMxOWVmY2Q4ZTE5ODYwNmM5NGQyOdjHx9KSSQYAQioKJgokCAESIG1VKCv3bAsT+BjxtNifG0ZEndE86Q7i1nMXyUktduQSEB4=").expect("Invalid base64 blob"),
                synchronizer_id: String::new(),
            }],
            ActionType::ProcessorDeploymentRequest { vault_processor_rules_cid, allocation_factory_cid, .. } => vec![DisclosedContract {
                template_id: None,
                contract_id: vault_processor_rules_cid.clone(),
                created_event_blob: base64::engine::general_purpose::STANDARD
                    .decode("CgMyLjESgAUKRQADHdZlCEAiG5xShFOFTfCSepv/7WjbeNMgvLwKJrb9RcoSEiC8H2AVna89+k+JxAm3r6A92wsYLM7a15KpZOI81rjSihIUYml0c2FmZS12YXVsdC12MC1yYzIaegpAODkyZjdiNjRkMzgxNDI5N2ZhNTMwYjBhMDgzZDMwZjRiMDY0OWFlOTEzMzY0NTk3NDBlN2M2M2RjMGYyNmYyZBIMQml0c2FmZVZhdWx0EhNWYXVsdFByb2Nlc3NvclJ1bGVzGhNWYXVsdFByb2Nlc3NvclJ1bGVzIr8BarwBClcKVTpTYml0c2FmZS1hZG1pbjo6MTIyMDk5OTUzOTM0ZDlmZTE2M2ZlZDA3ZGQzNzFmYTEzOTgyYjJiMzA3NDlkNmRmNTZlY2RiYTM4NWY4Yzc4YTg2N2EKYQpfWl0KWzpZYmFja2VuZC1zaWduYXRvcnktMDo6MTIyMDk5OTUzOTM0ZDlmZTE2M2ZlZDA3ZGQzNzFmYTEzOTgyYjJiMzA3NDlkNmRmNTZlY2RiYTM4NWY4Yzc4YTg2N2EqU2JpdHNhZmUtYWRtaW46OjEyMjA5OTk1MzkzNGQ5ZmUxNjNmZWQwN2RkMzcxZmExMzk4MmIyYjMwNzQ5ZDZkZjU2ZWNkYmEzODVmOGM3OGE4NjdhMlliYWNrZW5kLXNpZ25hdG9yeS0wOjoxMjIwOTk5NTM5MzRkOWZlMTYzZmVkMDdkZDM3MWZhMTM5ODJiMmIzMDc0OWQ2ZGY1NmVjZGJhMzg1ZjhjNzhhODY3YTl7TANp/EgGAEIqCiYKJAgBEiBvGY3UmUCK4oP4XdA6mUK7vNQUtI/KXjSgwk0UK1icBBAe").expect("Invalid base64 blob"),
                synchronizer_id: String::new()
            },DisclosedContract {
                template_id: None,
                contract_id: allocation_factory_cid.clone(),
                created_event_blob: base64::engine::general_purpose::STANDARD
                    .decode("CgMyLjEShwYKRQDVil8GHwhrPEtAW1fKCPTO/LfzonomCJvmOI62LuYZqMoREiAJkdVl2qBMppGQG8BCOKhlXiwGgDkTDkwAAn6sJnXZ9BIXdXRpbGl0eS1yZWdpc3RyeS1hcHAtdjAajQEKQDgyNzk4ZGYwMTgzMDE4NTI3MDRmMjEwYjk3YWRhYWJmNzZkM2VjZDM3ZDg4OWUxYmY5NmI1ZjMxYTIwZWVhMzQSB1V0aWxpdHkSCFJlZ2lzdHJ5EgNBcHASAlYwEgdTZXJ2aWNlEhFBbGxvY2F0aW9uRmFjdG9yeRoRQWxsb2NhdGlvbkZhY3RvcnkioQJqngIKVgpUOlJjYnRjLW5ldHdvcms6OjEyMjAyYTgzYzZmNDA4MjIxN2MxNzVlMjliYzUzZGE1ZjI3MDNiYTI2NzU3NzhhYjk5MjE3YTVhODgxYTk0OTIwM2ZmClYKVDpSY2J0Yy1uZXR3b3JrOjoxMjIwMmE4M2M2ZjQwODIyMTdjMTc1ZTI5YmM1M2RhNWYyNzAzYmEyNjc1Nzc4YWI5OTIxN2E1YTg4MWE5NDkyMDNmZgpsCmo6aGF1dGgwXzAwN2M2NWY4NTdmMWMzZDU5OWNiNmRmNzM3NzU6OjEyMjBkMmQ3MzJkMDQyYzI4MWNlZTgwZjQ4M2FiODBmM2NiYWE0NzgyODYwZWQ1ZjRkYzIyOGFiMDNkZWRkMmVlOGY5KlJjYnRjLW5ldHdvcms6OjEyMjAyYTgzYzZmNDA4MjIxN2MxNzVlMjliYzUzZGE1ZjI3MDNiYTI2NzU3NzhhYjk5MjE3YTVhODgxYTk0OTIwM2ZmMmhhdXRoMF8wMDdjNjVmODU3ZjFjM2Q1OTljYjZkZjczNzc1OjoxMjIwZDJkNzMyZDA0MmMyODFjZWU4MGY0ODNhYjgwZjNjYmFhNDc4Mjg2MGVkNWY0ZGMyMjhhYjAzZGVkZDJlZThmOTlubOVh5DkGAEIqCiYKJAgBEiCdDhxHJbSFz7Snbvg8xLkPDPvaP3wl+HzTfq2LxHAGmRAe").expect("Invalid base64 blob"),
                synchronizer_id: String::new()
            },
            // This is for the FAR config
            DisclosedContract {
                template_id: None,
                contract_id: "009b9fcd0ec3e6340d7fd1d75c192f6d7056c237465d94995fd87b6b3bc9bd091bca12122029b8d78f42969f04b90508b8cb1574d7c7142d2027c39c65717ff37b5667ed6c".to_string(),
                created_event_blob: base64::engine::general_purpose::STANDARD
                    .decode("CgMyLjESywQKRQCbn80Ow+Y0DX/R11wZL21wVsI3Rl2UmV/Ye2s7yb0JG8oSEiApuNePQpafBLkFCLjLFXTXxxQtICfDnGVxf/N7VmftbBINc3BsaWNlLWFtdWxldBpkCkAzY2ExMzQzYWIyNmI0NTNkMzhjOGFkYjcwZGNhNWYxZWFkODQ0MGM0MmI1OWI2OGYwNzA3ODY5NTVjYmY5ZWMxEgZTcGxpY2USBkFtdWxldBoQRmVhdHVyZWRBcHBSaWdodCKxAWquAQpNCks6SURTTzo6MTIyMGJlNThjMjllNjVkZTQwYmYyNzNiZTFkYzJiMjY2ZDQzYTlhMDAyZWE1YjE4OTU1YWVlZjdhYWM4ODFiYjQ3MWEKXQpbOlliYWNrZW5kLXNpZ25hdG9yeS0wOjoxMjIwOTk5NTM5MzRkOWZlMTYzZmVkMDdkZDM3MWZhMTM5ODJiMmIzMDc0OWQ2ZGY1NmVjZGJhMzg1ZjhjNzhhODY3YSpJRFNPOjoxMjIwYmU1OGMyOWU2NWRlNDBiZjI3M2JlMWRjMmIyNjZkNDNhOWEwMDJlYTViMTg5NTVhZWVmN2FhYzg4MWJiNDcxYTJZYmFja2VuZC1zaWduYXRvcnktMDo6MTIyMDk5OTUzOTM0ZDlmZTE2M2ZlZDA3ZGQzNzFmYTEzOTgyYjJiMzA3NDlkNmRmNTZlY2RiYTM4NWY4Yzc4YTg2N2E5StpTtehIBgBCKgomCiQIARIgte09TNtngfU2IfZ5PIabI5+a9HM9ZLsg728k5xjF0GMQHg==").expect("Invalid base64 blob"),
                synchronizer_id: String::new()
            }],
            _ => vec![],
        },
        synchronizer_id: String::new(),
        package_id_selection_preference: vec![],
        prefetch_contract_keys: vec![],
    };

    let mut req = tonic::Request::new(SubmitAndWaitRequest {
        commands: Some(commands),
    });
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());

    client.submit_and_wait(req).await?;

    Ok(())
}

/// Execute ExpireConfirmation choice on VaultGovernanceRules contract
async fn execute_expire_confirmation(
    config: &NodeConfig,
    request: &ExpireConfirmationRequest,
    token: &str,
    member_party_id: &str,
) -> Result {
    let channel = tonic::transport::Channel::from_shared(config.ledger_api_url())?
        .connect()
        .await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let template_id = Identifier {
        package_id: VAULT_GOVERNANCE_PACKAGE_ID.to_string(),
        module_name: "BitsafeVault.VaultGovernance".to_string(),
        entity_name: "VaultGovernanceRules".to_string(),
    };

    // Build choice argument: ExpireConfirmation { confirmationCid : ContractId VaultGovernanceConfirmation }
    let choice_argument = Value {
        sum: Some(value::Sum::Record(Record {
            record_id: None,
            fields: vec![RecordField {
                label: "confirmationCid".to_string(),
                value: Some(Value {
                    sum: Some(value::Sum::ContractId(request.confirmation_cid.clone())),
                }),
            }],
        })),
    };

    let cmd = Command {
        command: Some(command::Command::Exercise(ExerciseCommand {
            template_id: Some(template_id),
            contract_id: request.rules_contract_id.clone(),
            choice: "VaultGovernanceRules_ExpireConfirmation".to_string(),
            choice_argument: Some(choice_argument),
        })),
    };

    let commands = Commands {
        workflow_id: String::new(),
        user_id: String::new(),
        command_id: uuid::Uuid::new_v4().to_string(),
        commands: vec![cmd],
        deduplication_period: None,
        min_ledger_time_abs: None,
        min_ledger_time_rel: None,
        act_as: vec![member_party_id.to_string()],
        read_as: vec![request.party_id.to_string()],
        submission_id: String::new(),
        disclosed_contracts: vec![],
        synchronizer_id: String::new(),
        package_id_selection_preference: vec![],
        prefetch_contract_keys: vec![],
    };

    let mut req = tonic::Request::new(SubmitAndWaitRequest {
        commands: Some(commands),
    });
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());

    client.submit_and_wait(req).await?;

    Ok(())
}
