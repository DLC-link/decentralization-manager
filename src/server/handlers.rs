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
use tokio::net::TcpStream;

use super::{
    AppState,
    queries::{get_contracts, get_party_metadata},
    types::{
        DecentralizedPartiesResponse, DecentralizedParty, KeyStatusResponse, KeygenResponse,
        KickRequest, KickResponse, KickStatus, ParticipantInfo, ParticipantStatus,
        ParticipantsStatusResponse, Permission,
    },
};
use crate::{
    config::NodeConfig, error::Result, noise::NoiseKeypair, participant_id::CantonId, utils,
    workflow,
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
    let admin_url = config.admin_api_url();

    let mut topology_client = TopologyManagerReadServiceClient::connect(admin_url.clone()).await?;
    let mut vault_client = VaultServiceClient::connect(admin_url).await?;

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
    let current_node_id = &config.node.node_id;

    let futures: Vec<_> = network_config
        .participants
        .iter()
        .map(|participant| {
            let id = participant.id.clone();
            let address = participant.address.clone();
            let port = participant.port;
            let is_self = id == *current_node_id;

            async move {
                let active = if is_self {
                    // Current node is always active
                    true
                } else {
                    // Try to connect with a short timeout
                    let addr = format!("{address}:{port}");
                    tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(&addr))
                        .await
                        .map(|r| r.is_ok())
                        .unwrap_or(false)
                };

                ParticipantStatus { id, active }
            }
        })
        .collect();

    let statuses = futures::future::join_all(futures).await;

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

    tokio::spawn(async move {
        let kick_config = workflow::KickConfig::new(
            decentralized_party_id,
            participant_id,
            namespace_fingerprint,
        );

        let result =
            workflow::start_node(config, workflow::WorkflowType::Kick, Some(kick_config)).await;

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
pub struct KickWorkflowState {
    pub status: tokio::sync::RwLock<KickStatus>,
    pub error: tokio::sync::RwLock<Option<String>>,
}

impl KickWorkflowState {
    pub fn new() -> Self {
        Self {
            status: tokio::sync::RwLock::new(KickStatus::Idle),
            error: tokio::sync::RwLock::new(None),
        }
    }
}

/// Check if Noise keys exist for this node
#[get("/keys/status")]
pub async fn get_key_status(data: web::Data<AppState>) -> impl Responder {
    let key_file = &data.config.node.static_key_file;

    match NoiseKeypair::from_file(key_file).await {
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

/// Generate new Noise keypair for this node
#[post("/keys/generate")]
pub async fn generate_keys(data: web::Data<AppState>) -> impl Responder {
    let key_file = &data.config.node.static_key_file;

    // Check if keys already exist
    if NoiseKeypair::from_file(key_file).await.is_ok() {
        return HttpResponse::Conflict().json(serde_json::json!({
            "error": "Keys already exist. Delete the existing key file first if you want to regenerate."
        }));
    }

    // Generate new keypair
    match NoiseKeypair::generate().save_to_file(key_file).await {
        Ok(()) => {
            // Read back to get public key
            match NoiseKeypair::from_file(key_file).await {
                Ok(keypair) => HttpResponse::Ok().json(KeygenResponse {
                    success: true,
                    public_key: keypair.public_key_hex(),
                    message: "Keypair generated successfully".to_string(),
                }),
                Err(e) => HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": format!("Keys generated but failed to read back: {e}")
                })),
            }
        }
        Err(e) => {
            tracing::error!("Failed to generate keys: {e}");
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to generate keys: {e}")
            }))
        }
    }
}
