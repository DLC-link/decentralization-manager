use std::{sync::Arc, time::Duration};

use actix_web::{HttpResponse, Responder, get, post, web};
use canton_proto_rs::com::digitalasset::canton::topology::admin::v30::{
    BaseQuery, ListPartyToParticipantRequest, StoreId, Synchronizer, base_query, store_id,
    synchronizer, topology_manager_read_service_client::TopologyManagerReadServiceClient,
};
use tokio::sync::RwLock;

use crate::{
    auth::{AuthRegistry, WorkflowAuth},
    config::{KeycloakConfig, NodeConfig, PartyCredentials, default_package_config},
    error::Result,
    noise::{Message, MessageType, NoiseKeypair, parse_public_key, send_noise_message},
    participant_id::CantonId,
    server::{
        AppState,
        types::{
            ContractsRequest, DarsRequest, ErrorResponse, HttpWorkflowState, KickRequest,
            KickResponse, KickStatus, ListenerPauseGuard, OnboardingRequest, OnboardingResponse,
            OnboardingStatus, WorkflowProgress, WorkflowResponse, WorkflowStatusResponse,
        },
    },
    utils,
    workflow::{self, ContractsConfig},
};

// ============================================================================
// Workflow State Types
// ============================================================================

/// State for tracking kick workflow
pub type KickWorkflowState = HttpWorkflowState<KickStatus>;

/// State for tracking onboarding workflow
pub type OnboardingWorkflowState = HttpWorkflowState<OnboardingStatus>;

/// State for tracking contracts workflow
pub type ContractsWorkflowState = HttpWorkflowState<WorkflowProgress>;

/// State for tracking DARs upload workflow
pub type DarsWorkflowState = HttpWorkflowState<WorkflowProgress>;

// ============================================================================
// Kick Workflow
// ============================================================================

/// Start a kick workflow to remove a participant from a decentralized party
#[utoipa::path(
    tag = "Workflows",
    request_body = KickRequest,
    responses(
        (status = 202, description = "Kick workflow started", body = WorkflowResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 409, description = "Workflow already in progress", body = ErrorResponse)
    )
)]
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
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("Invalid decentralized_party_id: {e}"),
            });
        }
    };

    let participant_id = match CantonId::parse(&body.participant_id) {
        Ok(id) => id,
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("Invalid participant_id: {e}"),
            });
        }
    };

    // Prevent kicking ourselves
    if participant_id == *data.config.participant_id() {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: "Cannot kick yourself".to_string(),
        });
    }

    // Check if a kick is already in progress
    {
        let status = kick_state.status.read().await;
        if *status == KickStatus::InProgress {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: "A kick workflow is already in progress".to_string(),
            });
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
            None, // No dars config
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
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Kick workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/kick/status")]
pub async fn get_kick_status(kick_state: web::Data<Arc<KickWorkflowState>>) -> impl Responder {
    let status = kick_state.status.read().await;
    let error = kick_state.error.read().await;

    HttpResponse::Ok().json(WorkflowStatusResponse {
        status: *status,
        error: error.clone(),
    })
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

// ============================================================================
// Onboarding Workflow
// ============================================================================

/// Start an onboarding workflow to create a new decentralized party
#[utoipa::path(
    tag = "Workflows",
    request_body = OnboardingRequest,
    responses(
        (status = 202, description = "Onboarding workflow started", body = WorkflowResponse),
        (status = 409, description = "Workflow already in progress", body = ErrorResponse)
    )
)]
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
            return HttpResponse::Conflict().json(ErrorResponse {
                error: "An onboarding workflow is already in progress".to_string(),
            });
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
    let party_credentials = data.party_credentials.clone();
    let auth_lock = data.auth.clone();
    let is_test_mode = data.test_mode;

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
        let onboarding_config =
            workflow::OnboardingConfig::new(party_id_prefix.clone(), instance_name);

        let result = workflow::start_coordinator(
            config.clone(),
            workflow::WorkflowType::Onboarding,
            Some(onboarding_config),
            None, // No kick config
            None, // No contracts config
            None, // No dars config
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

                // Auto-save default party config for the new dec party
                if !is_test_mode
                    && let Err(e) = save_default_party_config(
                        &config,
                        &party_id_prefix,
                        &party_credentials,
                        &auth_lock,
                    )
                    .await
                {
                    tracing::warn!("Failed to auto-save party config: {e}");
                }
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

/// Get the current status of the onboarding workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Onboarding workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/onboarding/status")]
pub async fn get_onboarding_status(
    onboarding_state: web::Data<Arc<OnboardingWorkflowState>>,
) -> impl Responder {
    let status = onboarding_state.status.read().await;
    let error = onboarding_state.error.read().await;

    HttpResponse::Ok().json(WorkflowStatusResponse {
        status: *status,
        error: error.clone(),
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

// ============================================================================
// Contracts Workflow
// ============================================================================

/// Start a contracts workflow to upload DARs and create contracts
#[utoipa::path(
    tag = "Workflows",
    request_body = ContractsRequest,
    responses(
        (status = 202, description = "Contracts workflow started", body = WorkflowResponse),
        (status = 409, description = "Workflow already in progress", body = ErrorResponse)
    )
)]
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
            return HttpResponse::Conflict().json(ErrorResponse {
                error: "A contracts workflow is already in progress".to_string(),
            });
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
        body.contracts.clone(),
        instance_name,
    );

    // Spawn the contracts workflow in the background
    let config = data.config.clone();
    let workflow_auth = data.auth.read().await.clone();
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
            config.clone(),
            workflow::WorkflowType::Contracts,
            None, // No onboarding config
            None, // No kick config
            Some(contracts_config.clone()),
            None, // No dars config
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
                // Save package IDs from deployed contracts to party config
                if let Err(e) = save_deployed_packages(&config, &contracts_config).await {
                    tracing::warn!("Failed to save package config: {e}");
                }
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

/// Get the current status of the contracts workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "Contracts workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/contracts/status")]
pub async fn get_contracts_status(
    contracts_state: web::Data<Arc<ContractsWorkflowState>>,
) -> impl Responder {
    let status = contracts_state.status.read().await;
    let error = contracts_state.error.read().await;

    HttpResponse::Ok().json(WorkflowStatusResponse {
        status: *status,
        error: error.clone(),
    })
}

/// Save deployed package IDs to party config after successful contracts workflow
async fn save_deployed_packages(config: &NodeConfig, contracts_config: &ContractsConfig) -> Result {
    let mut fresh_config = NodeConfig::from_dir(config.root_dir()).await?;
    let creds = fresh_config
        .parties
        .iter_mut()
        .find(|p| p.dec_party_id == contracts_config.decentralized_party_id);
    if let Some(creds) = creds {
        for contract in &contracts_config.contracts {
            match (contract.module_name.as_str(), contract.entity_name.as_str()) {
                ("BitsafeVault.VaultGovernance", "VaultGovernanceRules") => {
                    creds.packages.vault_governance = Some(contract.package_id.clone());
                }
                ("BitsafeVault.Vault", "Vault") => {
                    creds.packages.vault = Some(contract.package_id.clone());
                }
                (m, _) if m.starts_with("Utility.Registry.App") => {
                    creds.packages.utility_registry = Some(contract.package_id.clone());
                }
                (m, _) if m.starts_with("Utility.Credential.App") => {
                    creds.packages.utility_credential = Some(contract.package_id.clone());
                }
                _ => {}
            }
        }
        fresh_config.save_config().await?;
        tracing::info!("Saved package IDs to party config");
    }
    Ok(())
}

// ============================================================================
// DARs Upload Workflow
// ============================================================================

/// Start a DARs upload workflow to distribute DARs across all participants
#[utoipa::path(
    tag = "Workflows",
    request_body = DarsRequest,
    responses(
        (status = 202, description = "DARs upload workflow started", body = WorkflowResponse),
        (status = 409, description = "Workflow already in progress", body = ErrorResponse)
    )
)]
#[post("/dars")]
pub async fn start_dars(
    data: web::Data<AppState>,
    dars_state: web::Data<Arc<DarsWorkflowState>>,
    body: web::Json<DarsRequest>,
) -> impl Responder {
    // Check if a DARs workflow is already in progress
    {
        let status = dars_state.status.read().await;
        if *status == WorkflowProgress::InProgress {
            return HttpResponse::Conflict().json(ErrorResponse {
                error: "A DARs upload workflow is already in progress".to_string(),
            });
        }
    }

    // Update status to in progress
    {
        let mut status = dars_state.status.write().await;
        *status = WorkflowProgress::InProgress;
        let mut error = dars_state.error.write().await;
        *error = None;
    }

    // Create DARs config from request
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let instance_name = format!("dars-upload-{timestamp}");
    let dars_config = workflow::DarsConfig {
        dar_files: body.dar_files.clone(),
        instance_name,
    };

    // Spawn the DARs workflow in the background
    let config = data.config.clone();
    let dars_state_clone = dars_state.get_ref().clone();
    let listener_control = data.noise_listener_control.clone();
    let listener_notify = data.noise_listener_notify.clone();

    tokio::spawn(async move {
        let guard = ListenerPauseGuard::pause(listener_control, listener_notify).await;

        // Send invites to all peers before starting coordinator workflow
        let invite_result = send_dars_invites(&config).await;
        if let Err(e) = invite_result {
            tracing::error!("Failed to send DARs invites: {e}");
            guard.resume().await;
            let mut status = dars_state_clone.status.write().await;
            let mut error = dars_state_clone.error.write().await;
            *status = WorkflowProgress::Failed;
            *error = Some(format!("Failed to send invites: {e}"));
            return;
        }

        // Give peers time to start their attestor workflows
        tokio::time::sleep(Duration::from_secs(2)).await;

        let result = workflow::start_coordinator(
            config,
            workflow::WorkflowType::Dars,
            None, // No onboarding config
            None, // No kick config
            None, // No contracts config
            Some(dars_config),
            None, // No auth
        )
        .await;

        guard.resume().await;

        let mut status = dars_state_clone.status.write().await;
        let mut error = dars_state_clone.error.write().await;

        match result {
            Ok(()) => {
                *status = WorkflowProgress::Completed;
                tracing::info!("DARs upload workflow completed successfully");
            }
            Err(e) => {
                *status = WorkflowProgress::Failed;
                *error = Some(format!("{e}"));
                tracing::error!("DARs upload workflow failed: {e}");
            }
        }
    });

    HttpResponse::Accepted().json(WorkflowResponse {
        status: WorkflowProgress::InProgress,
        message: "DARs upload workflow started".to_string(),
    })
}

/// Get the current status of the DARs upload workflow
#[utoipa::path(
    tag = "Workflows",
    responses(
        (status = 200, description = "DARs upload workflow status", body = WorkflowStatusResponse)
    )
)]
#[get("/dars/status")]
pub async fn get_dars_status(dars_state: web::Data<Arc<DarsWorkflowState>>) -> impl Responder {
    let status = dars_state.status.read().await;
    let error = dars_state.error.read().await;

    HttpResponse::Ok().json(WorkflowStatusResponse {
        status: *status,
        error: error.clone(),
    })
}

/// Send DARs invites to all peers using Noise protocol
async fn send_dars_invites(config: &NodeConfig) -> Result {
    let network_config = config.load_network_config().await?;
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;

    let current_participant_id = config.participant_id();
    let invite_message = Message::new_empty(MessageType::InviteDars);

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
        let identity = config.participant_id().to_string();

        tracing::info!(
            "Sending DARs invite to {} at {}:{}",
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
                        tracing::info!("Peer {} acknowledged DARs invite", peer.participant_id);
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
                tracing::error!("Failed to send DARs invite to {}: {e}", peer.participant_id);
            }
        }
    }

    Ok(())
}

/// Auto-save default party config after successful onboarding
async fn save_default_party_config(
    config: &NodeConfig,
    party_id_prefix: &str,
    party_credentials: &Arc<RwLock<Vec<PartyCredentials>>>,
    auth_lock: &Arc<RwLock<Option<WorkflowAuth>>>,
) -> Result {
    let dec_party_id = resolve_dec_party_id(config, party_id_prefix).await?;
    tracing::info!("Resolved new dec party: {dec_party_id}");

    let kc_defaults = config.canton.network.keycloak_defaults();
    let creds = PartyCredentials {
        dec_party_id: dec_party_id.clone(),
        // Placeholder — user must set the real member party ID via the config dialog
        member_party_id: dec_party_id.clone(),
        user_id: "CoordinatorUser".to_string(),
        keycloak: KeycloakConfig {
            url: kc_defaults.url,
            realm: kc_defaults.realm,
            client_id: String::new(),
            client_secret: None,
            username: None,
            password: None,
        },
        packages: default_package_config(),
    };

    let mut fresh_config = NodeConfig::from_dir(config.root_dir()).await?;
    fresh_config.upsert_party_credentials(creds.clone()).await?;

    // Update in-memory state
    {
        let mut pc = party_credentials.write().await;
        if let Some(existing) = pc.iter_mut().find(|p| p.dec_party_id == dec_party_id) {
            *existing = creds;
        } else {
            pc.push(creds);
        }
    }

    let party_creds = party_credentials.read().await;
    if !party_creds.is_empty() {
        match AuthRegistry::new(&party_creds).await {
            Ok(registry) => {
                let mut auth = auth_lock.write().await;
                *auth = Some(WorkflowAuth::Keycloak(Arc::new(registry)));
                tracing::info!("Auth registry reinitialized after onboarding");
            }
            Err(e) => {
                tracing::warn!("Failed to reinitialize auth registry: {e}");
            }
        }
    }

    tracing::info!("Default party config saved for {}", dec_party_id);
    Ok(())
}

/// Resolve the full dec party ID by querying Canton with the prefix
async fn resolve_dec_party_id(config: &NodeConfig, prefix: &str) -> Result<CantonId> {
    let channel = tonic::transport::Channel::from_shared(config.admin_api_url())?
        .connect()
        .await?;

    let mut client = TopologyManagerReadServiceClient::new(channel)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    let response = client
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
            filter_party: prefix.to_string(),
            filter_participant: String::new(),
        }))
        .await?
        .into_inner();

    // Find the party with the matching prefix that has multiple participants (dec party)
    for result in &response.results {
        if let Some(item) = &result.item
            && item.party.starts_with(prefix)
            && item.participants.len() > 1
        {
            return CantonId::parse(&item.party);
        }
    }

    anyhow::bail!("Could not find decentralized party with prefix '{prefix}'")
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
