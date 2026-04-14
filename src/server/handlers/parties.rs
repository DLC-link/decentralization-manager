use std::{
    collections::{HashMap, HashSet},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use actix_web::{HttpResponse, Responder, get, web};
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
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::{
    auth::WorkflowAuth,
    config::{NetworkConfig, NodeConfig, PartyCredentials},
    db::{
        rows::{DecPartyContractRow, DecPartyParticipantRow, DecPartyRow},
        schema::{Commitable, SchemaRead, SchemaWrite},
    },
    error::Result,
    noise::{Message, MessageType, NoiseKeypair, parse_public_key, send_noise_message},
    participant_id::CantonId,
    server::{
        AppState,
        queries::{get_contracts, get_party_metadata},
        types::{
            ConnectionStatus, ContractInfo, DecentralizedPartiesResponse, DecentralizedParty,
            ErrorResponse, PackageInfo, ParticipantInfo, ParticipantStatus,
            ParticipantsStatusResponse, PeerPackageComparison, PeerPackageResult, Permission,
            ResponseSource, VettedPackageInfo,
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
    let prefix = query.prefix.clone().unwrap_or_default();

    // Try to load from DB cache first
    let cached = load_cached_parties(&data.db, &prefix).await;
    if let Ok(Some((mut response, updated_at))) = cached {
        response.source = ResponseSource::Cache;

        // Only refresh if cache is stale (older than 60 seconds)
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let is_stale = (now - updated_at) > 60;

        if is_stale {
            // Atomic check+insert to avoid duplicate spawns
            let spawned = data
                .refreshing_prefixes
                .write()
                .await
                .insert(prefix.clone());
            if spawned {
                let data = data.clone();
                let prefix = prefix.clone();
                tokio::spawn(async move {
                    refresh_and_cache_parties(&data, &prefix).await;
                    data.refreshing_prefixes.write().await.remove(&prefix);
                });
            }
        }

        response.refreshing = is_stale && data.refreshing_prefixes.read().await.contains(&prefix);

        return HttpResponse::Ok().json(response);
    }

    // No cache — do the full Canton query (first request is slow)
    let auth = data.auth.read().await.clone();
    let party_creds = data.party_credentials.read().await.clone();
    match fetch_decentralized_parties(
        &data.config,
        Some(prefix.as_str()).filter(|s| !s.is_empty()),
        auth,
        &party_creds,
    )
    .await
    {
        Ok(response) => {
            // Cache in background
            let db = data.db.clone();
            let parties = response.parties.clone();
            let prefix = prefix.clone();
            tokio::spawn(async move {
                if let Err(e) = store_parties_to_db(&db, &prefix, &parties).await {
                    tracing::warn!("Failed to cache parties: {e}");
                }
            });
            HttpResponse::Ok().json(response)
        }
        Err(e) => {
            tracing::error!("Failed to fetch decentralized parties: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch decentralized parties: {e}"),
            })
        }
    }
}

/// Background task: fetch from Canton, store to DB, then resolve owner keys from peers
async fn refresh_and_cache_parties(data: &web::Data<AppState>, prefix: &str) {
    let auth = data.auth.read().await.clone();
    let party_creds = data.party_credentials.read().await.clone();
    match fetch_decentralized_parties(
        &data.config,
        Some(prefix).filter(|s| !s.is_empty()),
        auth,
        &party_creds,
    )
    .await
    {
        Ok(response) => {
            if let Err(e) = store_parties_to_db(&data.db, prefix, &response.parties).await {
                tracing::warn!("Failed to cache parties: {e}");
                return;
            }
            resolve_owner_keys_from_peers(&data.config, &data.db, &response.parties).await;
        }
        Err(e) => {
            tracing::warn!("Background refresh failed for prefix '{prefix}': {e}");
        }
    }
}

/// Query each peer via Noise for their owner keys, then update the DB
pub async fn resolve_owner_keys_from_peers(
    config: &NodeConfig,
    db: &SqlitePool,
    parties: &[DecentralizedParty],
) {
    tracing::debug!("Resolving owner keys from peers...");

    let peers = match db.get_all_peers().await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Failed to load peers for owner key resolution: {e}");
            return;
        }
    };

    let keypair = match NoiseKeypair::from_file(&config.key_file_path()).await {
        Ok(kp) => kp,
        Err(e) => {
            tracing::warn!("Failed to load keypair for owner key resolution: {e}");
            return;
        }
    };

    let current_participant_id = config.participant_id().to_string();
    let known_party_ids: HashSet<String> = parties.iter().map(|p| p.party_id.to_string()).collect();

    for peer in &peers {
        let peer_uid = peer.participant_id.to_string();
        if peer_uid == current_participant_id || peer.public_key.is_empty() {
            continue;
        }

        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!("Failed to parse public key for {peer_uid}: {e}");
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);
        let identity = keypair.public_key.serialize();
        let msg = Message::new_empty(MessageType::RequestOwnerKeys);

        tracing::debug!("Requesting owner keys from {peer_uid}");
        let response = match tokio::time::timeout(
            Duration::from_secs(10),
            send_noise_message(&peer.address, peer.port, &psk, &identity, &msg),
        )
        .await
        {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(e)) => {
                tracing::warn!("Noise request to {peer_uid} failed: {e}");
                continue;
            }
            Err(_) => {
                tracing::warn!("Noise request to {peer_uid} timed out");
                continue;
            }
        };

        let response_msg = match Message::from_bytes(&response) {
            Ok(m) if m.msg_type == MessageType::OwnerKeys => m,
            Ok(m) => {
                tracing::warn!("Unexpected response type from {peer_uid}: {:?}", m.msg_type);
                continue;
            }
            Err(e) => {
                tracing::warn!("Failed to parse response from {peer_uid}: {e}");
                continue;
            }
        };

        let entries: Vec<serde_json::Value> = match serde_json::from_slice(&response_msg.payload) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Failed to deserialize owner keys from {peer_uid}: {e}");
                continue;
            }
        };

        tracing::debug!(
            "Received {} owner key entries from {peer_uid}",
            entries.len()
        );

        // Update DB with the owner keys
        let peer_uid = peer.participant_id.to_string();
        if let Ok(mut tx) = db.begin_transaction().await {
            for entry in &entries {
                let Some(party_id) = entry["party_id"].as_str() else {
                    continue;
                };
                let Some(owner_key) = entry["owner_key"].as_str() else {
                    continue;
                };

                if known_party_ids.contains(party_id)
                    && let Err(e) = tx
                        .update_participant_owner_key(party_id, &peer_uid, owner_key)
                        .await
                {
                    tracing::debug!("Failed to update owner key for {peer_uid}: {e}");
                }
            }
            if let Err(e) = Commitable::commit(tx).await {
                tracing::debug!("Failed to commit owner key updates: {e}");
            }
        }
    }
}

/// Load cached parties from the dec_party tables.
/// Returns the response and the newest `updated_at` timestamp (unix seconds).
async fn load_cached_parties(
    db: &SqlitePool,
    prefix: &str,
) -> Result<Option<(DecentralizedPartiesResponse, i64)>> {
    let rows = db.get_dec_parties_by_prefix(prefix).await?;
    if rows.is_empty() {
        return Ok(None);
    }

    // Bulk-fetch all related data in 3 queries instead of 3*N
    let all_owners = db.get_all_dec_party_owners(prefix).await?;
    let all_participants = db.get_all_dec_party_participants(prefix).await?;
    let all_contracts = db.get_all_dec_party_contracts(prefix).await?;

    // Group by party_id
    let mut owners_map: HashMap<String, Vec<String>> = HashMap::new();
    for (party_id, owner_key) in all_owners {
        owners_map.entry(party_id).or_default().push(owner_key);
    }

    let mut participants_map: HashMap<String, Vec<ParticipantInfo>> = HashMap::new();
    for p in all_participants {
        if let Ok(uid) = CantonId::parse(&p.participant_uid) {
            participants_map
                .entry(p.dec_party_id.clone())
                .or_default()
                .push(ParticipantInfo {
                    participant_uid: uid,
                    permission: match p.permission.as_str() {
                        "submission" => Permission::Submission,
                        "confirmation" => Permission::Confirmation,
                        "observation" => Permission::Observation,
                        _ => Permission::Unknown,
                    },
                    owner_key: p.owner_key,
                });
        }
    }

    let mut contracts_map: HashMap<String, Vec<ContractInfo>> = HashMap::new();
    for c in all_contracts {
        contracts_map
            .entry(c.dec_party_id.clone())
            .or_default()
            .push(ContractInfo {
                contract_id: c.contract_id,
                template_id: c.template_id,
                package_id: c.package_id,
            });
    }

    let max_updated_at = rows.iter().map(|r| r.updated_at).max().unwrap_or(0);

    let mut parties = Vec::with_capacity(rows.len());
    for row in rows {
        parties.push(DecentralizedParty {
            party_id: CantonId::parse(&row.party_id)?,
            threshold: row.threshold as i32,
            owners: owners_map.remove(&row.party_id).unwrap_or_default(),
            my_owner_key: None,
            participants: participants_map.remove(&row.party_id).unwrap_or_default(),
            contracts: contracts_map.remove(&row.party_id).unwrap_or_default(),
            local_metadata: None,
        });
    }

    Ok(Some((
        DecentralizedPartiesResponse {
            parties,
            source: ResponseSource::Cache,
            refreshing: false,
        },
        max_updated_at,
    )))
}

/// Store parties into the dec_party tables
pub async fn store_parties_to_db(
    db: &SqlitePool,
    prefix: &str,
    parties: &[DecentralizedParty],
) -> Result {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let mut tx = db.begin_transaction().await?;
    tx.delete_dec_parties_by_prefix(prefix).await?;

    for party in parties {
        // Extract the real prefix from party_id (everything before "::")
        let party_id_str = party.party_id.to_string();
        let real_prefix = party_id_str
            .split_once("::")
            .map(|(p, _)| p)
            .unwrap_or(&party_id_str);

        let row = DecPartyRow {
            party_id: party_id_str.clone(),
            prefix: real_prefix.to_string(),
            threshold: party.threshold as i64,
            updated_at: now,
        };
        tx.upsert_dec_party(&row).await?;

        tx.replace_dec_party_owners(&row.party_id, &party.owners)
            .await?;

        let participants: Vec<DecPartyParticipantRow> = party
            .participants
            .iter()
            .map(|p| DecPartyParticipantRow {
                dec_party_id: row.party_id.clone(),
                participant_uid: p.participant_uid.to_string(),
                permission: match p.permission {
                    Permission::Submission => "submission",
                    Permission::Confirmation => "confirmation",
                    Permission::Observation => "observation",
                    Permission::Unknown => "unknown",
                }
                .to_string(),
                owner_key: p.owner_key.clone(),
            })
            .collect();
        tx.replace_dec_party_participants(&row.party_id, &participants)
            .await?;

        let contracts: Vec<DecPartyContractRow> = party
            .contracts
            .iter()
            .map(|c| DecPartyContractRow {
                dec_party_id: row.party_id.clone(),
                contract_id: c.contract_id.clone(),
                template_id: c.template_id.clone(),
                package_id: c.package_id.clone(),
            })
            .collect();
        tx.replace_dec_party_contracts(&row.party_id, &contracts)
            .await?;
    }

    Commitable::commit(tx).await
}

/// Fetch decentralized parties from Canton topology and ledger APIs
pub async fn fetch_decentralized_parties(
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
                            owner_key: None, // Populated during onboarding
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
        source: ResponseSource::Live,
        refreshing: false,
    })
}

/// Get vetted packages for this participant
#[utoipa::path(
    tag = "Packages",
    responses(
        (status = 200, description = "Vetted packages", body = Vec<VettedPackageInfo>),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/packages/vetted")]
pub async fn get_vetted_packages(data: web::Data<AppState>) -> impl Responder {
    let mut client = match PackageServiceClient::connect(data.config.admin_api_url()).await {
        Ok(c) => c,
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to connect to Canton: {e}"),
            });
        }
    };

    let response = match client
        .list_packages(tonic::Request::new(ListPackagesRequest {
            limit: 0,
            filter_name: String::new(),
        }))
        .await
    {
        Ok(r) => r.into_inner(),
        Err(e) => {
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to list packages: {e}"),
            });
        }
    };

    let packages: Vec<VettedPackageInfo> = response
        .package_descriptions
        .into_iter()
        .map(|p| VettedPackageInfo {
            package_id: p.package_id,
            package_name: p.name,
            package_version: p.version,
        })
        .collect();

    HttpResponse::Ok().json(packages)
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
