use std::{collections::HashMap, time::Duration};

use actix_web::{HttpResponse, Responder, get, web};
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
use serde::Deserialize;

use crate::{
    auth::WorkflowAuth,
    config::NodeConfig,
    error::Result,
    noise::{Message, MessageType, NoiseKeypair, parse_public_key, send_noise_message},
    participant_id::CantonId,
    server::{
        AppState,
        queries::{get_contracts, get_party_metadata},
        types::{
            ConnectionStatus, DecentralizedPartiesResponse, DecentralizedParty, ErrorResponse,
            ParticipantInfo, ParticipantStatus, ParticipantsStatusResponse, Permission,
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
    match fetch_decentralized_parties(&data.config, query.prefix.as_deref(), data.auth.clone())
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

                let packages = config.get_packages(&party_id_str);
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

    Ok(DecentralizedPartiesResponse { parties })
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
    match check_participants_status(&data.config).await {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(e) => {
            tracing::error!("Failed to check participants status: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to check participants status: {e}"),
            })
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
