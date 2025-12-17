use std::{collections::HashMap, sync::Arc, time::Duration};

use actix_web::{HttpResponse, Responder, get, post, web};
use canton_proto_rs::com::digitalasset::canton::{
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

use super::{
    AppState,
    queries::{get_contracts, get_party_metadata},
    types::{
        ContractsRequest, DecentralizedPartiesResponse, DecentralizedParty, HttpWorkflowState,
        KeyStatusResponse, KickRequest, KickResponse, KickStatus, ListenerPauseGuard,
        OnboardingRequest, OnboardingResponse, OnboardingStatus, ParticipantInfo,
        ParticipantStatus, ParticipantsStatusResponse, Permission, WorkflowProgress,
        WorkflowResponse,
    },
};
use crate::{
    config::{NetworkConfig, NodeConfig, Peer},
    error::Result,
    noise::{Message, MessageType, NoiseKeypair, parse_public_key, send_noise_message},
    participant_id::CantonId,
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

/// Get decentralized parties the current participant is a member of
#[get("/decentralized-parties")]
pub async fn get_decentralized_parties(data: web::Data<AppState>) -> impl Responder {
    match fetch_decentralized_parties(&data.config).await {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(e) => {
            tracing::error!("Failed to fetch decentralized parties: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to fetch decentralized parties: {e}")
            }))
        }
    }
}

async fn fetch_decentralized_parties(config: &NodeConfig) -> Result<DecentralizedPartiesResponse> {
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

    // Query all P2P mappings once (instead of per-party)
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
            filter_party: String::new(),
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

    // Fetch contracts and metadata in parallel for all parties
    let futures: Vec<_> = my_parties
        .into_iter()
        .map(|(item, my_owner_key, p2p)| {
            let config = config.clone();
            let party_id_str = p2p.party.clone();
            async move {
                let (contracts, local_metadata) = tokio::join!(
                    async {
                        get_contracts(&config, &party_id_str)
                            .await
                            .unwrap_or_else(|e| {
                                tracing::warn!("Failed to get contracts for {party_id_str}: {e}");
                                Vec::new()
                            })
                    },
                    async {
                        get_party_metadata(&config, &party_id_str)
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
    match check_participants_status(&data.config, &data.peer_status).await {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(e) => {
            tracing::error!("Failed to check participants status: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to check participants status: {e}")
            }))
        }
    }
}

async fn check_participants_status(
    config: &NodeConfig,
    peer_status_cache: &tokio::sync::RwLock<std::collections::HashMap<String, bool>>,
) -> Result<ParticipantsStatusResponse> {
    let network_config = config.load_network_config().await?;
    let current_node_id = &config.node.node_id;
    let cache = peer_status_cache.read().await;

    let statuses: Vec<ParticipantStatus> = network_config
        .peers
        .iter()
        .map(|peer| {
            let id = peer.id.clone();
            let is_self = id == *current_node_id;

            let active = if is_self {
                // Current node is always active
                true
            } else {
                // Get status from cache
                *cache.get(&id).unwrap_or(&false)
            };

            ParticipantStatus { id, active }
        })
        .collect();

    Ok(ParticipantsStatusResponse { statuses })
}

/// Start a kick workflow to remove a participant from a decentralized party
#[post("/kick")]
pub async fn start_kick(
    data: web::Data<AppState>,
    kick_state: web::Data<Arc<KickWorkflowState>>,
    body: web::Json<KickRequest>,
) -> impl Responder {
    // Check if a kick is already in progress
    {
        let status = kick_state.status.read().await;
        if *status == KickStatus::InProgress {
            return HttpResponse::Conflict().json(serde_json::json!({
                "error": "A kick workflow is already in progress"
            }));
        }
    }

    // Parse IDs
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

        let kick_config = workflow::KickConfig::new(
            decentralized_party_id,
            participant_id,
            namespace_fingerprint,
        );

        let result = workflow::start_coordinator(
            config,
            workflow::WorkflowType::Kick,
            None, // No onboarding config
            Some(kick_config),
            None, // No contracts config
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

    tokio::spawn(async move {
        let guard = ListenerPauseGuard::pause(listener_control, listener_notify).await;

        // Send invites to all peers before starting coordinator workflow
        let invite_result = send_onboarding_invites(&config).await;
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

        let onboarding_config = workflow::OnboardingConfig::new(party_id_prefix);

        let result = workflow::start_coordinator(
            config,
            workflow::WorkflowType::Onboarding,
            Some(onboarding_config),
            None, // No kick config
            None, // No contracts config
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

/// Send onboarding invites to all peers using Noise protocol
async fn send_onboarding_invites(config: &NodeConfig) -> Result {
    let network_config = config.load_network_config().await?;
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let current_node_id = &config.node.node_id;
    let invite_message = Message::new_empty(MessageType::InviteOnboarding);

    for peer in &network_config.peers {
        if peer.id == *current_node_id {
            continue;
        }

        if peer.public_key.is_empty() {
            tracing::warn!("Skipping invite to {} - no public key configured", peer.id);
            continue;
        }

        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!("Skipping invite to {} - invalid public key: {e}", peer.id);
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);
        let identity = keypair.public_key.serialize();

        tracing::info!(
            "Sending onboarding invite to {} at {}:{}",
            peer.id,
            peer.address,
            peer.port
        );

        match send_noise_message(&peer.address, peer.port, &psk, &identity, &invite_message).await {
            Ok(response) => {
                if let Ok(msg) = Message::from_bytes(&response) {
                    if msg.msg_type == MessageType::Ack {
                        tracing::info!("Peer {id} acknowledged invite", id = peer.id);
                    } else {
                        tracing::warn!(
                            "Peer {id} responded with {msg_type:?} instead of Ack",
                            id = peer.id,
                            msg_type = msg.msg_type
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to send invite to {id}: {e}", id = peer.id);
            }
        }
    }

    Ok(())
}

/// Send kick invites to all peers using Noise protocol (excluding the peer being kicked)
async fn send_kick_invites(config: &NodeConfig, kicked_participant: &CantonId) -> Result {
    let network_config = config.load_network_config().await?;
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let current_node_id = &config.node.node_id;
    let invite_message = Message::new_empty(MessageType::InviteKick);

    for peer in &network_config.peers {
        // Skip self and the peer being kicked
        if peer.id == *current_node_id {
            continue;
        }

        // Skip the peer being kicked (they won't participate in the kick workflow)
        // Compare the peer's Canton party ID with the kicked participant ID
        if let Some(party) = &peer.party
            && let Ok(party_id) = CantonId::parse(party)
            && party_id == *kicked_participant
        {
            tracing::info!("Skipping invite to {} - they are being kicked", peer.id);
            continue;
        }

        if peer.public_key.is_empty() {
            tracing::warn!("Skipping invite to {} - no public key configured", peer.id);
            continue;
        }

        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!("Skipping invite to {} - invalid public key: {e}", peer.id);
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);
        let identity = keypair.public_key.serialize();

        tracing::info!(
            "Sending kick invite to {} at {}:{}",
            peer.id,
            peer.address,
            peer.port
        );

        match send_noise_message(&peer.address, peer.port, &psk, &identity, &invite_message).await {
            Ok(response) => {
                if let Ok(msg) = Message::from_bytes(&response) {
                    if msg.msg_type == MessageType::Ack {
                        tracing::info!("Peer {id} acknowledged kick invite", id = peer.id);
                    } else {
                        tracing::warn!(
                            "Peer {id} responded with {msg_type:?} instead of Ack",
                            id = peer.id,
                            msg_type = msg.msg_type
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to send kick invite to {id}: {e}", id = peer.id);
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
    let contracts_config = workflow::ContractsConfig::new(body.decentralized_party_id.clone());

    // Spawn the contracts workflow in the background
    let config = data.config.clone();
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

    let current_node_id = &config.node.node_id;
    let invite_message = Message::new_empty(MessageType::InviteContracts);

    for peer in &network_config.peers {
        if peer.id == *current_node_id {
            continue;
        }

        if peer.public_key.is_empty() {
            tracing::warn!("Skipping invite to {} - no public key configured", peer.id);
            continue;
        }

        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!("Skipping invite to {} - invalid public key: {e}", peer.id);
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);
        let identity = keypair.public_key.serialize();

        tracing::info!(
            "Sending contracts invite to {} at {}:{}",
            peer.id,
            peer.address,
            peer.port
        );

        match send_noise_message(&peer.address, peer.port, &psk, &identity, &invite_message).await {
            Ok(response) => {
                if let Ok(msg) = Message::from_bytes(&response) {
                    if msg.msg_type == MessageType::Ack {
                        tracing::info!("Peer {id} acknowledged contracts invite", id = peer.id);
                    } else {
                        tracing::warn!(
                            "Peer {id} responded with {msg_type:?} instead of Ack",
                            id = peer.id,
                            msg_type = msg.msg_type
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to send contracts invite to {id}: {e}", id = peer.id);
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
