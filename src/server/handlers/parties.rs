use std::{collections::HashMap, time::Duration};

use actix_web::{HttpResponse, Responder, get, web};
use canton_proto_rs::com::digitalasset::canton::{
    admin::participant::v30::{ListPackagesRequest, package_service_client::PackageServiceClient},
    crypto::{
        admin::v30::{ListMyKeysRequest, vault_service_client::VaultServiceClient},
        v30::public_key,
    },
    topology::admin::v30::{
        BaseQuery, ListDecentralizedNamespaceDefinitionRequest, ListPartyToParticipantRequest,
        ListVettedPackagesRequest, StoreId, Synchronizer, base_query, store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use serde::Deserialize;

use sqlx::SqlitePool;

use crate::{
    auth::WorkflowAuth,
    config::{NetworkConfig, NodeConfig, PartyCredentials},
    db::schema::SchemaRead,
    error::Result,
    noise::{Message, MessageType, NoiseKeypair, parse_public_key, send_noise_message},
    participant_id::CantonId,
    server::{
        AppState,
        queries::{get_contracts, get_party_metadata},
        types::{
            ConnectionStatus, DecentralizedPartiesResponse, DecentralizedParty, ErrorResponse,
            PackageInfo, ParticipantInfo, ParticipantStatus, ParticipantsStatusResponse,
            PeerPackageComparison, PeerPackageResult, Permission, VettedPackageInfo,
        },
    },
    utils,
};

/// Query parameters for decentralized parties endpoint
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct PartiesQuery {
    /// Filter parties by prefix (e.g., "cbtc-network")
    #[serde(default)]
    pub prefix: Option<String>,
}

/// Get decentralized parties the current participant is a member of
#[utoipa::path(
    tag = "Parties",
    params(PartiesQuery),
    responses(
        (status = 200, description = "Decentralized parties", body = DecentralizedPartiesResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/decentralized-parties")]
pub async fn get_decentralized_parties(
    data: web::Data<AppState>,
    query: web::Query<PartiesQuery>,
) -> impl Responder {
    let auth = data.auth.read().await.clone();
    let party_creds = data.party_credentials.read().await.clone();
    match fetch_decentralized_parties(&data.config, query.prefix.as_deref(), auth, &party_creds)
        .await
    {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(e) => {
            tracing::error!("Failed to fetch decentralized parties: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch decentralized parties: {e}"),
            })
        }
    }
}

async fn fetch_decentralized_parties(
    config: &NodeConfig,
    prefix_filter: Option<&str>,
    auth: Option<WorkflowAuth>,
    party_credentials: &[PartyCredentials],
) -> Result<DecentralizedPartiesResponse> {
    let channel = tonic::transport::Channel::from_shared(config.admin_api_url())?
        .connect()
        .await?;

    let mut topology_client = TopologyManagerReadServiceClient::new(channel.clone())
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);
    let mut vault_client = VaultServiceClient::new(channel.clone())
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);
    let mut package_client =
        PackageServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

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
                        kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.clone())),
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

    // Fetch vetted packages for this participant (participant-level, not per-party)
    let vetted_packages = fetch_vetted_packages(
        &mut topology_client,
        &mut package_client,
        &synchronizer_id,
        &namespace_key_fingerprints,
    )
    .await
    .unwrap_or_else(|e| {
        tracing::warn!("Failed to fetch vetted packages: {e:#}");
        Vec::new()
    });

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
                        Some(mock_registry.get_by_str(&party_id_str).await.get_token())
                    }
                    None => None,
                };

                let packages = CantonId::parse(&party_id_str)
                    .ok()
                    .and_then(|id| party_credentials.iter().find(|p| p.dec_party_id == id))
                    .map(|c| c.packages.clone())
                    .unwrap_or_default();
                let token_clone = token.clone();
                let (contracts, local_metadata) = tokio::join!(
                    async {
                        get_contracts(&config, &party_id_str, token, test_mode, &packages)
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

    Ok(DecentralizedPartiesResponse {
        parties,
        vetted_packages,
    })
}

async fn fetch_vetted_packages(
    topology_client: &mut TopologyManagerReadServiceClient<tonic::transport::Channel>,
    package_client: &mut PackageServiceClient<tonic::transport::Channel>,
    synchronizer_id: &str,
    namespace_key_fingerprints: &HashMap<String, bool>,
) -> Result<Vec<VettedPackageInfo>> {
    // Get all vetted packages on this synchronizer
    let vetted_response = topology_client
        .list_vetted_packages(tonic::Request::new(ListVettedPackagesRequest {
            base_query: Some(BaseQuery {
                store: Some(StoreId {
                    store: Some(store_id::Store::Synchronizer(Synchronizer {
                        kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.to_string())),
                    })),
                }),
                proposals: false,
                operation: 0,
                time_query: Some(base_query::TimeQuery::HeadState(())),
                filter_signed_key: String::new(),
                protocol_version: None,
            }),
            filter_participant: String::new(),
        }))
        .await?
        .into_inner();

    // Collect vetted package IDs only for this participant (match by namespace fingerprint)
    let mut vetted_ids: Vec<String> = Vec::new();
    for result in &vetted_response.results {
        let is_ours = result
            .item
            .as_ref()
            .and_then(|item| item.participant_uid.rsplit_once("::"))
            .is_some_and(|(_, ns)| namespace_key_fingerprints.contains_key(ns));

        if !is_ours {
            continue;
        }

        if let Some(item) = &result.item {
            #[allow(deprecated)]
            for id in &item.package_ids {
                vetted_ids.push(id.clone());
            }
            for pkg in &item.packages {
                vetted_ids.push(pkg.package_id.clone());
            }
        }
    }

    // Get all uploaded package descriptions
    let packages_response = package_client
        .list_packages(tonic::Request::new(ListPackagesRequest {
            limit: 0,
            filter_name: String::new(),
        }))
        .await?
        .into_inner();

    // Build lookup map: package_id -> (name, version)
    let package_info: HashMap<String, (String, String)> = packages_response
        .package_descriptions
        .into_iter()
        .map(|p| (p.package_id, (p.name, p.version)))
        .collect();

    // Cross-reference vetted IDs with package metadata
    let vetted_packages = vetted_ids
        .into_iter()
        .map(|package_id| {
            let (package_name, package_version) =
                package_info.get(&package_id).cloned().unwrap_or_default();
            VettedPackageInfo {
                package_id,
                package_name,
                package_version,
            }
        })
        .collect();

    Ok(vetted_packages)
}

/// Check connectivity status of all participants
#[utoipa::path(
    tag = "Parties",
    responses(
        (status = 200, description = "Participants connection status", body = ParticipantsStatusResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/participants-status")]
pub async fn get_participants_status(data: web::Data<AppState>) -> impl Responder {
    match check_participants_status(&data.config, &data.db).await {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(e) => {
            tracing::error!("Failed to check participants status: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to check participants status: {e}"),
            })
        }
    }
}

async fn check_participants_status(
    config: &NodeConfig,
    db: &SqlitePool,
) -> Result<ParticipantsStatusResponse> {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
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

/// Compare locally uploaded packages with peer nodes via Noise protocol
#[utoipa::path(
    tag = "Packages",
    responses(
        (status = 200, description = "Peer package comparison", body = PeerPackageComparison),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/packages/compare-peers")]
pub async fn compare_peer_packages(data: web::Data<AppState>) -> impl Responder {
    match fetch_peer_packages(&data.config, &data.db).await {
        Ok(comparison) => HttpResponse::Ok().json(comparison),
        Err(e) => {
            tracing::error!("Failed to compare peer packages: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to compare peer packages: {e}"),
            })
        }
    }
}

async fn fetch_peer_packages(
    config: &NodeConfig,
    db: &SqlitePool,
) -> Result<PeerPackageComparison> {
    // Get local packages
    let mut client = PackageServiceClient::connect(config.admin_api_url()).await?;
    let local_response = client
        .list_packages(tonic::Request::new(ListPackagesRequest {
            limit: 0,
            filter_name: String::new(),
        }))
        .await?
        .into_inner();

    let local_packages: Vec<PackageInfo> = local_response
        .package_descriptions
        .into_iter()
        .map(|p| PackageInfo {
            package_id: p.package_id,
            name: p.name,
            version: p.version,
        })
        .collect();

    // Load network config and keypair for Noise communication
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;
    let current_participant_id = config.participant_id();

    let invite_message = Message::new_empty(MessageType::ListPackages);

    // Query each peer in parallel
    let peer_futures: Vec<_> = network_config
        .peers
        .iter()
        .filter(|p| p.participant_id != *current_participant_id && !p.public_key.is_empty())
        .map(|peer| {
            let keypair = keypair.clone();
            let peer = peer.clone();
            let msg = invite_message.clone();
            async move {
                let peer_pub_key = match parse_public_key(&peer.public_key) {
                    Ok(pk) => pk,
                    Err(_) => {
                        return PeerPackageResult {
                            participant_id: peer.participant_id.to_string(),
                            name: peer.name.clone(),
                            reachable: false,
                            packages: vec![],
                        };
                    }
                };

                let psk = keypair.derive_psk(&peer_pub_key);
                let identity = current_participant_id.to_string();

                match tokio::time::timeout(
                    Duration::from_secs(5),
                    send_noise_message(&peer.address, peer.port, &psk, identity.as_bytes(), &msg),
                )
                .await
                {
                    Ok(Ok(response)) => {
                        if let Ok(response_msg) = Message::from_bytes(&response)
                            && response_msg.msg_type == MessageType::Data
                            && let Ok(packages) =
                                serde_json::from_slice::<Vec<PackageInfo>>(&response_msg.payload)
                        {
                            return PeerPackageResult {
                                participant_id: peer.participant_id.to_string(),
                                name: peer.name.clone(),
                                reachable: true,
                                packages,
                            };
                        }
                        PeerPackageResult {
                            participant_id: peer.participant_id.to_string(),
                            name: peer.name.clone(),
                            reachable: true,
                            packages: vec![],
                        }
                    }
                    _ => PeerPackageResult {
                        participant_id: peer.participant_id.to_string(),
                        name: peer.name.clone(),
                        reachable: false,
                        packages: vec![],
                    },
                }
            }
        })
        .collect();

    let peers = futures::future::join_all(peer_futures).await;

    Ok(PeerPackageComparison {
        local_packages,
        peers,
    })
}
