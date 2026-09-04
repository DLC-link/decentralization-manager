use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use actix_web::{HttpResponse, Responder, get, web};
use canton_proto_rs::com::digitalasset::canton::{
    admin::participant::v30::{ListPackagesRequest, package_service_client::PackageServiceClient},
    crypto::{
        admin::v30::{
            ListMyKeysRequest, private_key_metadata, vault_service_client::VaultServiceClient,
        },
        v30::public_key,
    },
    topology::admin::v30::{
        BaseQuery, ListDecentralizedNamespaceDefinitionRequest, ListNamespaceDelegationRequest,
        ListPartyToParticipantRequest, StoreId, Synchronizer, base_query,
        list_namespace_delegation_response::result::Item as NsDelegationItem,
        list_party_to_participant_response::result::Item as P2pItem, store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::{
    auth::WorkflowAuth,
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig, PartyCredentials, default_package_config},
    db::{
        rows::{DecPartyContractRow, DecPartyParticipantRow, DecPartyRow},
        schema::{Commitable, SchemaRead, SchemaWrite},
    },
    error::Result,
    noise::{
        Message, MessageType, NoiseError, NoiseKeypair, parse_public_key, send_noise_message,
        send_noise_message_with_chunked_response, send_noise_message_with_retry,
    },
    server::{
        AppState,
        health::classify_health_reply,
        package_inventory::fetch_vetted_packages,
        queries::{get_contracts, get_party_metadata, sort_contracts},
        types::{
            ConnectionStatus, ContractInfo, DecentralizedPartiesResponse, DecentralizedParty,
            ErrorResponse, PackageInfo, ParticipantInfo, ParticipantStatus,
            ParticipantsStatusResponse, PeerErrorKind, PeerPackageComparison, PeerPackageResult,
            Permission, ResponseSource, VettedPackageInfo, permission_from_proto,
        },
    },
    utils,
};

/// How long a completed party discovery answers requests before another one
/// runs, whether it found parties or not.
const PARTIES_CACHE_TTL_SECS: i64 = 60;

/// How long a request waits on another request's in-flight discovery before it
/// answers without data.
const SINGLE_FLIGHT_WAIT: Duration = Duration::from_secs(3);

/// Deadline for a single topology read.
///
/// Canton applies no deadline of its own, so a read the participant cannot
/// answer quickly holds the request open indefinitely, well past any gateway
/// timeout in front of it, and every retry stacks another one on the
/// participant.
const TOPOLOGY_READ_TIMEOUT: Duration = Duration::from_secs(60);

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

/// Longest accepted `prefix`. A prefix narrows a Canton party id, so a longer
/// one filters nothing, and the value keys an in-memory map.
const MAX_PREFIX_LEN: usize = 128;

/// Cap on distinct prefixes tracked in `AppState::discovery_completed`.
const MAX_TRACKED_PREFIXES: usize = 1024;

/// Whether a completed discovery still answers for a prefix.
///
/// `None` means no discovery has completed for it in this process. A timestamp
/// ahead of `now` is not fresh: the clock moved backwards after it was written,
/// and treating a negative age as fresh would pin the answer indefinitely.
fn discovery_is_fresh(completed_at: Option<i64>, now: i64) -> bool {
    completed_at.is_some_and(|at| (0..=PARTIES_CACHE_TTL_SECS).contains(&(now - at)))
}

/// Record that a discovery completed for `prefix`.
///
/// Only an empty result is recorded. That is the one case the `dec_parties`
/// cache cannot represent, and recording a non-empty result would let a
/// request arriving before the cache write answer empty for the whole TTL.
///
/// Expired entries go on the way in and the map is capped, because `prefix`
/// comes from the request.
pub(crate) async fn record_discovery(
    completed: &Arc<tokio::sync::RwLock<HashMap<String, i64>>>,
    prefix: &str,
    parties: &[DecentralizedParty],
) {
    if !parties.is_empty() {
        return;
    }

    let now = now_secs();
    let mut completed = completed.write().await;
    completed.retain(|_, at| discovery_is_fresh(Some(*at), now));
    if completed.len() >= MAX_TRACKED_PREFIXES {
        completed.clear();
    }
    completed.insert(prefix.to_string(), now);
}

/// Run one topology read under [`TOPOLOGY_READ_TIMEOUT`].
async fn bounded_read<T>(
    what: &str,
    read: impl std::future::Future<Output = std::result::Result<T, tonic::Status>>,
) -> Result<T> {
    match tokio::time::timeout(TOPOLOGY_READ_TIMEOUT, read).await {
        Ok(result) => Ok(result?),
        Err(_) => anyhow::bail!(
            "{what} did not answer within {secs}s",
            secs = TOPOLOGY_READ_TIMEOUT.as_secs()
        ),
    }
}

/// Query parameters for decentralized parties endpoint
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct PartiesQuery {
    /// Filter parties by prefix (e.g., "cbtc-network")
    #[serde(default)]
    pub prefix: Option<String>,
    /// Force a synchronous Canton fetch, bypassing the cache. Used right after
    /// mutating workflows (kick / contracts / dars) so the UI sees fresh data
    /// instead of the up-to-60s-stale cached snapshot.
    #[serde(default)]
    pub refresh: Option<bool>,
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
    let force_refresh = query.refresh.unwrap_or(false);

    if prefix.len() > MAX_PREFIX_LEN {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: format!("prefix must be at most {MAX_PREFIX_LEN} characters"),
        });
    }

    // Try to load from DB cache first (unless caller explicitly demanded fresh)
    let cached = if force_refresh {
        Ok(None)
    } else {
        load_cached_parties(&data.db, &prefix).await
    };
    if let Ok(Some((mut response, updated_at))) = cached {
        response.source = ResponseSource::Cache;

        // Only refresh if the cache is stale
        let is_stale = (now_secs() - updated_at) > PARTIES_CACHE_TTL_SECS;

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

        // Resolve my_owner_key for parties where it's missing (e.g. old cache)
        if response.parties.iter().any(|p| p.my_owner_key.is_none())
            && let Ok(fingerprints) = get_local_namespace_fingerprints(&data.config).await
        {
            for party in &mut response.parties {
                if party.my_owner_key.is_none() {
                    party.my_owner_key = party
                        .owners
                        .iter()
                        .find(|o| fingerprints.contains(o.as_str()))
                        .cloned();
                }
            }
        }

        return HttpResponse::Ok().json(response);
    }

    // No cached rows. A party this node is not a member of and a party that
    // does not exist leave the same empty table, so the cache alone cannot
    // tell "never fetched" from "fetched, found nothing". Answering from a
    // recent completed discovery is what stops a node with no party from
    // re-running that query on every single request.
    let completed_at = data.discovery_completed.read().await.get(&prefix).copied();
    if !force_refresh && discovery_is_fresh(completed_at, now_secs()) {
        return HttpResponse::Ok().json(DecentralizedPartiesResponse {
            parties: Vec::new(),
            source: ResponseSource::Cache,
            refreshing: data.refreshing_prefixes.read().await.contains(&prefix),
        });
    }

    // Single-flight. A cold cache used to let every concurrent request start
    // its own discovery, so one page load could put several full topology
    // reads on the participant at once.
    if !data
        .refreshing_prefixes
        .write()
        .await
        .insert(prefix.clone())
    {
        return await_in_flight_discovery(&data, &prefix).await;
    }

    let auth = data.auth.read().await.clone();
    let party_creds = data.party_credentials.read().await.clone();
    let fetched = fetch_decentralized_parties(
        &data.config,
        &data.db,
        Some(prefix.as_str()).filter(|s| !s.is_empty()),
        auth,
        &party_creds,
    )
    .await;

    // The claim covered the expensive part and is released here whatever the
    // outcome: a failure has to be retryable, and the background pass below
    // takes its own claim. Nothing downstream of this point can strand it.
    data.refreshing_prefixes.write().await.remove(&prefix);

    match fetched {
        Ok(response) => {
            record_discovery(&data.discovery_completed, &prefix, &response.parties).await;

            // Cache + resolve owner keys in background. Mirrors
            // `refresh_and_cache_parties` so a cold cache reaches the same
            // post-resolved state on the next request. Dedup against
            // `refreshing_prefixes` so concurrent cold-cache requests don't
            // each fan out their own Noise resolution pass.
            let spawned = data
                .refreshing_prefixes
                .write()
                .await
                .insert(prefix.clone());
            if spawned {
                let data = data.clone();
                let parties = response.parties.clone();
                tokio::spawn(async move {
                    if let Err(e) = store_parties_to_db(&data.db, &prefix, &parties).await {
                        tracing::warn!("Failed to cache parties: {e}");
                    } else {
                        resolve_owner_keys_from_peers(&data.config, &data.db, &parties).await;
                    }
                    data.refreshing_prefixes.write().await.remove(&prefix);
                });
            }
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

/// Answer a request whose prefix another request is already discovering.
///
/// Waits [`SINGLE_FLIGHT_WAIT`] for that discovery to land, which covers an
/// ordinary scoped query, and stops early once the cache holds rows or a
/// discovery has completed and found nothing. Answering an empty list straight
/// away reads as "no parties" in a client that does not act on `refreshing`.
async fn await_in_flight_discovery(data: &web::Data<AppState>, prefix: &str) -> HttpResponse {
    let deadline = tokio::time::Instant::now() + SINGLE_FLIGHT_WAIT;

    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;

        if let Ok(Some((mut response, _))) = load_cached_parties(&data.db, prefix).await {
            response.source = ResponseSource::Cache;
            response.refreshing = data.refreshing_prefixes.read().await.contains(prefix);
            return HttpResponse::Ok().json(response);
        }

        let completed_at = data.discovery_completed.read().await.get(prefix).copied();
        if discovery_is_fresh(completed_at, now_secs()) {
            break;
        }
    }

    HttpResponse::Ok().json(DecentralizedPartiesResponse {
        parties: Vec::new(),
        source: ResponseSource::Cache,
        refreshing: data.refreshing_prefixes.read().await.contains(prefix),
    })
}

/// Background task: fetch from Canton, store to DB, then resolve owner keys from peers
async fn refresh_and_cache_parties(data: &web::Data<AppState>, prefix: &str) {
    let auth = data.auth.read().await.clone();
    let party_creds = data.party_credentials.read().await.clone();
    match fetch_decentralized_parties(
        &data.config,
        &data.db,
        Some(prefix).filter(|s| !s.is_empty()),
        auth,
        &party_creds,
    )
    .await
    {
        Ok(response) => {
            record_discovery(&data.discovery_completed, prefix, &response.parties).await;

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
        // Tell the peer which parties we want owner_keys for. See #149: peer
        // used to enumerate the whole synchronizer to build a namespace→party
        // map; we now pass the namespaces (via the full party_ids) directly so
        // the peer can skip that scan.
        let request_payload = match serde_json::to_vec(
            &parties
                .iter()
                .map(|p| p.party_id.to_string())
                .collect::<Vec<_>>(),
        ) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("Failed to serialise RequestOwnerKeys payload: {e}");
                continue;
            }
        };
        let msg = Message::new(MessageType::RequestOwnerKeys, request_payload);

        tracing::debug!("Requesting owner keys from {peer_uid}");
        let response = match tokio::time::timeout(
            Duration::from_secs(10),
            send_noise_message(
                &peer.address,
                peer.port,
                &psk,
                current_participant_id.as_bytes(),
                &msg,
            ),
        )
        .await
        {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(e)) => {
                tracing::warn!(
                    peer = %peer_uid,
                    endpoint = %format!("{}:{}", peer.address, peer.port),
                    "RequestOwnerKeys failed: {e} — {hint}",
                    hint = peer_failure_hint(&e)
                );
                continue;
            }
            Err(_) => {
                tracing::warn!(
                    peer = %peer_uid,
                    endpoint = %format!("{}:{}", peer.address, peer.port),
                    "RequestOwnerKeys timed out after 10s — {hint}",
                    hint = peer_failure_hint(&NoiseError::RequestTimeout)
                );
                continue;
            }
        };

        // A 200 with an empty body. The peer accepted the request and then
        // said nothing, which used to surface as "message too short: got 0" and
        // read like a protocol bug in the peer.
        //
        // A denied request is NOT this case: it arrives as a 503, so
        // `send_noise_message` returns `BadStatusCode` above and never reaches
        // here. This arm is a peer that answered 200 with no frame at all, or a
        // proxy that terminated the request and returned an empty 200.
        if response.is_empty() {
            tracing::warn!(
                peer = %peer_uid,
                endpoint = %format!("{}:{}", peer.address, peer.port),
                "RequestOwnerKeys got an empty reply — {hint}",
                hint = peer_failure_hint(&NoiseError::InvalidMessage)
            );
            continue;
        }

        let response_msg = match Message::from_bytes(&response) {
            Ok(m) if m.msg_type == MessageType::OwnerKeys => m,
            Ok(m) if m.msg_type == MessageType::Error => {
                // An Error frame under 200 OK. The listener's own deny paths
                // send that frame with a 503, so those surface as
                // `BadStatusCode(_, Some(reason))` above rather than here. This
                // arm catches a peer that reports the error in the body while
                // still answering 200. Either way, prefer its words to a guess.
                tracing::warn!(
                    peer = %peer_uid,
                    endpoint = %format!("{}:{}", peer.address, peer.port),
                    "RequestOwnerKeys refused by the peer: {reason}",
                    reason = String::from_utf8_lossy(&m.payload)
                );
                continue;
            }
            Ok(m) => {
                tracing::warn!(
                    peer = %peer_uid,
                    endpoint = %format!("{}:{}", peer.address, peer.port),
                    "RequestOwnerKeys got an unexpected response type {:?} — the peer may be on a \
                     different wire format",
                    m.msg_type
                );
                continue;
            }
            Err(e) => {
                tracing::warn!(
                    peer = %peer_uid,
                    endpoint = %format!("{}:{}", peer.address, peer.port),
                    "RequestOwnerKeys reply did not parse: {e} — {hint}",
                    hint = peer_failure_hint(&NoiseError::InvalidMessage)
                );
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

                if !known_party_ids.contains(party_id) {
                    continue;
                }
                let Ok(party_id_canton) = CantonId::parse(party_id) else {
                    tracing::debug!(
                        "Skipping owner-key update from {peer_uid}: bad party_id {party_id}"
                    );
                    continue;
                };
                if let Err(e) = tx
                    .update_participant_owner_key(&party_id_canton, &peer_uid, owner_key)
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

    // Topology-driven fallback: covers the case where the participant whose
    // owner_key we need is offline / unreachable via Noise. The mapping
    // (participant_uid → owner_key in a party) is recoverable from public
    // synchronizer state — each participant publishes `NamespaceDelegation`
    // entries listing the signing keys delegated under its namespace, and
    // one of those fingerprints is what appears in the party's `owners`
    // list. This is independent of peer reachability.
    if let Err(e) = supplement_owner_keys_from_topology(config, db, parties).await {
        tracing::debug!("Topology-based owner-key fallback skipped: {e:#}");
    }
}

/// Fill in missing `dec_party_participant.owner_key` rows by reading public
/// Canton topology state. For each (party, participant) where the local
/// cache hasn't learned the owner_key yet, we query the participant's
/// `NamespaceDelegation` entries, fingerprint their target keys, and
/// intersect with the party's `owners` list. Whatever matches is the
/// participant's contribution to the decentralized namespace.
async fn supplement_owner_keys_from_topology(
    config: &NodeConfig,
    db: &SqlitePool,
    parties: &[DecentralizedParty],
) -> Result {
    let channel = config.admin_channel().await?;
    let mut topology_client = TopologyManagerReadServiceClient::new(channel)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);
    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    let base_query = || BaseQuery {
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
        client_version: None,
    };

    // Cache per-namespace fingerprints so a participant who appears in many
    // parties is only queried once.
    let mut delegated_fingerprints: HashMap<String, HashSet<String>> = HashMap::new();

    for party in parties {
        for participant in &party.participants {
            // Already known — nothing to derive.
            if participant.owner_key.is_some() {
                continue;
            }
            let uid = participant.participant_uid.to_string();
            let Some((_, namespace)) = uid.rsplit_once("::") else {
                continue;
            };
            let namespace = namespace.to_string();

            if !delegated_fingerprints.contains_key(&namespace) {
                let resp = match topology_client
                    .list_namespace_delegation(tonic::Request::new(
                        ListNamespaceDelegationRequest {
                            base_query: Some(base_query()),
                            filter_namespace: namespace.clone(),
                            filter_target_key_fingerprint: String::new(),
                        },
                    ))
                    .await
                {
                    Ok(r) => r.into_inner(),
                    Err(e) => {
                        tracing::debug!("ListNamespaceDelegation for {namespace} failed: {e}");
                        // Cache empty set so we don't retry on every party.
                        delegated_fingerprints.insert(namespace.clone(), HashSet::new());
                        continue;
                    }
                };
                let mut fingerprints: HashSet<String> = HashSet::new();
                for result in resp.results {
                    if let Some(NsDelegationItem::V30(item)) = result.item
                        && let Some(target_key) = item.target_key
                    {
                        fingerprints.insert(utils::compute_fingerprint(&target_key));
                    }
                }
                delegated_fingerprints.insert(namespace.clone(), fingerprints);
            }

            let fingerprints = match delegated_fingerprints.get(&namespace) {
                Some(s) if !s.is_empty() => s,
                _ => continue,
            };
            let Some(owner_key) = party.owners.iter().find(|o| fingerprints.contains(*o)) else {
                continue;
            };

            let mut tx = match db.begin_transaction().await {
                Ok(t) => t,
                Err(e) => {
                    tracing::debug!("Topology fallback: begin_transaction failed: {e}");
                    continue;
                }
            };
            if let Err(e) = tx
                .update_participant_owner_key(&party.party_id, &uid, owner_key)
                .await
            {
                tracing::debug!("Topology fallback: update_participant_owner_key for {uid}: {e}");
                continue;
            }
            if let Err(e) = Commitable::commit(tx).await {
                tracing::debug!("Topology fallback: commit failed: {e}");
            }
        }
    }
    Ok(())
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
                package_name: c.package_name,
                package_version: c.package_version,
                created_at: c.created_at,
            });
    }
    for list in contracts_map.values_mut() {
        sort_contracts(list);
    }

    let max_updated_at = rows.iter().map(|r| r.updated_at).max().unwrap_or(0);

    let mut parties = Vec::with_capacity(rows.len());
    for row in rows {
        parties.push(DecentralizedParty {
            party_id: CantonId::parse(&row.party_id)?,
            threshold: row.threshold as i32,
            owners: owners_map.remove(&row.party_id).unwrap_or_default(),
            my_owner_key: row.my_owner_key,
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
    let fresh_party_ids: Vec<String> = parties.iter().map(|p| p.party_id.to_string()).collect();

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
            my_owner_key: party.my_owner_key.clone(),
        };
        tx.upsert_dec_party(&row).await?;

        tx.replace_dec_party_owners(&party.party_id, &party.owners)
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
        tx.replace_dec_party_participants(&party.party_id, &participants)
            .await?;

        let contracts: Vec<DecPartyContractRow> = party
            .contracts
            .iter()
            .map(|c| DecPartyContractRow {
                dec_party_id: row.party_id.clone(),
                contract_id: c.contract_id.clone(),
                template_id: c.template_id.clone(),
                package_id: c.package_id.clone(),
                package_name: c.package_name.clone(),
                package_version: c.package_version.clone(),
                created_at: c.created_at.clone(),
            })
            .collect();
        tx.replace_dec_party_contracts(&party.party_id, &contracts)
            .await?;
    }

    // Remove stale parties no longer returned by Canton
    tx.delete_stale_dec_parties(prefix, &fresh_party_ids)
        .await?;

    Commitable::commit(tx).await
}

/// The `filter_party` value that selects every party in `namespace`.
///
/// Canton splits `filter_party` on `::`, drops the empty identifier half, and
/// compiles the namespace half into the store query as `namespace LIKE 'ns%'`.
/// That makes a namespace lookup as cheap as an exact party id, and it is what
/// lets a node find a party it holds no local record of without reading every
/// party on the synchronizer.
fn namespace_party_filter(namespace: &str) -> String {
    format!("::{namespace}")
}

/// This node's namespace signing-key fingerprints, from the local vault.
async fn my_namespace_fingerprints(
    vault_client: &mut VaultServiceClient<tonic::transport::Channel>,
) -> Result<HashSet<String>> {
    let response = bounded_read(
        "list_my_keys",
        vault_client.list_my_keys(tonic::Request::new(ListMyKeysRequest {
            filters: None,
            base_request: None,
        })),
    )
    .await?
    .into_inner();

    let mut fingerprints = HashSet::new();
    for key_meta in response.private_keys_metadata {
        if let Some(private_key_metadata::PublicKeyWithName::V30(pub_key_with_name)) =
            &key_meta.public_key_with_name
            && let Some(pub_key) = &pub_key_with_name.public_key
            && let Some(public_key::Key::SigningPublicKey(signing_key)) = &pub_key.key
            && signing_key.usage.contains(&1)
        {
            // SigningKeyUsage::Namespace = 1
            fingerprints.insert(utils::compute_fingerprint(signing_key));
        }
    }

    Ok(fingerprints)
}

/// The decentralized namespaces this node owns a namespace key in.
///
/// This is the discovery step for a node that holds no local record of its
/// parties. It goes through the namespaces rather than the parties because a
/// party is only selectable by an in-memory participant filter, while a
/// namespace is a store-level predicate.
async fn owned_decentralized_namespaces(
    topology_client: &mut TopologyManagerReadServiceClient<tonic::transport::Channel>,
    synchronizer_id: &str,
    my_fingerprints: &HashSet<String>,
) -> Result<Vec<String>> {
    let response = bounded_read(
        "list_decentralized_namespace_definition",
        topology_client.list_decentralized_namespace_definition(tonic::Request::new(
            build_decentralized_namespace_request(synchronizer_id, ""),
        )),
    )
    .await?
    .into_inner();

    let mut namespaces: Vec<_> = response
        .results
        .into_iter()
        .filter_map(|result| {
            let item = result.item?;
            item.owners
                .iter()
                .any(|owner| my_fingerprints.contains(owner))
                .then_some(item.decentralized_namespace)
        })
        .collect();
    namespaces.sort();
    namespaces.dedup();

    Ok(namespaces)
}

/// Build the `list_party_to_participant` request used to discover this node's
/// decentralized parties.
///
/// `filter_party` is the only scalable filter here. Canton splits it on `::`
/// and compiles both halves into the store query, while `filter_participant`
/// and `filter_signed_key` are applied in memory after every party has been
/// loaded. So every caller must pass a party filter: an exact id, or
/// [`namespace_party_filter`] for a whole namespace.
fn build_party_to_participant_request(
    synchronizer_id: &str,
    party_filter: Option<&str>,
    participant_id: &str,
) -> ListPartyToParticipantRequest {
    ListPartyToParticipantRequest {
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
            client_version: None,
        }),
        filter_party: party_filter.unwrap_or_default().to_string(),
        filter_participant: participant_id.to_string(),
    }
}

/// Build a decentralized-namespace query, for one namespace or for all of them.
///
/// An empty namespace enumerates every decentralized namespace on the
/// synchronizer. Canton pushes the mapping type into the store query, so that
/// reads one small slice of the topology store rather than all of it: a
/// synchronizer holds a handful of decentralized namespaces against a
/// `PartyToParticipant` mapping for every party on it.
fn build_decentralized_namespace_request(
    synchronizer_id: &str,
    namespace: &str,
) -> ListDecentralizedNamespaceDefinitionRequest {
    ListDecentralizedNamespaceDefinitionRequest {
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
            client_version: None,
        }),
        filter_namespace: namespace.to_string(),
    }
}

/// Union every decentralized party ID the node already knows locally.
///
/// Credentials cover configured parties, workflow runs cover a party while it
/// is being onboarded (before credentials exist), and cached rows preserve
/// discovery after the originating workflow has been dismissed. Keeping all
/// three sources prevents exact-filter discovery from hiding a newly added
/// party on a node that already has credentials for another party.
fn known_party_filters(
    party_credentials: &[PartyCredentials],
    workflow_party_ids: impl IntoIterator<Item = String>,
    cached_party_ids: impl IntoIterator<Item = String>,
    prefix_filter: Option<&str>,
) -> Vec<String> {
    let mut parties: Vec<_> = party_credentials
        .iter()
        .map(|credentials| credentials.dec_party_id.to_string())
        .chain(workflow_party_ids)
        .chain(cached_party_ids)
        .filter(|party_id| prefix_filter.is_none_or(|prefix| party_id.starts_with(prefix)))
        .collect();
    parties.sort();
    parties.dedup();
    parties
}

/// Fetch decentralized parties from Canton topology and ledger APIs
pub async fn fetch_decentralized_parties(
    config: &NodeConfig,
    db: &SqlitePool,
    prefix_filter: Option<&str>,
    auth: Option<WorkflowAuth>,
    party_credentials: &[PartyCredentials],
) -> Result<DecentralizedPartiesResponse> {
    let workflow_runs = db.get_visible_workflow_runs().await?;
    let cached_parties = db.get_dec_parties_by_prefix("").await?;
    let known_party_ids = known_party_filters(
        party_credentials,
        workflow_runs
            .into_iter()
            .filter_map(|run| run.dec_party_id.map(|party_id| party_id.to_string())),
        cached_parties.into_iter().map(|party| party.party_id),
        None,
    );
    let exact_party_filters: Vec<_> = known_party_ids
        .into_iter()
        .filter(|party_id| prefix_filter.is_none_or(|prefix| party_id.starts_with(prefix)))
        .collect();

    let channel = config.admin_channel().await?;

    let mut topology_client = TopologyManagerReadServiceClient::new(channel.clone())
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);
    let mut vault_client =
        VaultServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let participant_id = config.participant_id().to_string();

    // Wanted twice: to discover which namespaces this node owns a key in, and
    // to pick each party's `my_owner_key` below.
    let namespace_key_fingerprints = my_namespace_fingerprints(&mut vault_client).await?;

    // Prefer exact IDs from every local source. A node with no exact ID for
    // what was asked has to discover, and discovery goes through the
    // decentralized namespaces rather than the parties: a namespace is a
    // store-level predicate, while a participant-scoped party query is filtered
    // in memory only after every party on the synchronizer has been loaded.
    //
    // Keying this on the filters rather than on "knows any party at all"
    // matters for a node that holds one party and is asked about another
    // prefix: it has to discover instead of issuing no query and reporting
    // nothing.
    let party_filters: Vec<String> = if !exact_party_filters.is_empty() {
        exact_party_filters
    } else {
        owned_decentralized_namespaces(
            &mut topology_client,
            &synchronizer_id,
            &namespace_key_fingerprints,
        )
        .await?
        .iter()
        .map(|namespace| namespace_party_filter(namespace))
        .collect()
    };
    let mut p2p_by_namespace = HashMap::new();
    for party_filter in party_filters {
        let response = bounded_read(
            "list_party_to_participant",
            topology_client.list_party_to_participant(tonic::Request::new(
                build_party_to_participant_request(
                    &synchronizer_id,
                    Some(party_filter.as_str()),
                    &participant_id,
                ),
            )),
        )
        .await?
        .into_inner();

        for result in response.results {
            let Some(P2pItem::V30(party_mapping)) = result.item else {
                continue;
            };
            let Some((_, namespace)) = party_mapping.party.rsplit_once("::") else {
                continue;
            };
            if namespace.is_empty() {
                continue;
            }
            p2p_by_namespace.insert(namespace.to_string(), party_mapping);
        }
    }

    // Query only the exact decentralized namespaces belonging to locally
    // hosted parties. Never issue an empty namespace filter on this path.
    let mut namespaces: Vec<_> = p2p_by_namespace.keys().cloned().collect();
    namespaces.sort();
    let mut dns_results = Vec::new();
    for namespace in namespaces {
        let response = bounded_read(
            "list_decentralized_namespace_definition",
            topology_client.list_decentralized_namespace_definition(tonic::Request::new(
                build_decentralized_namespace_request(&synchronizer_id, &namespace),
            )),
        )
        .await?
        .into_inner();
        dns_results.extend(response.results);
    }

    // Filter to parties where this participant is a member
    let my_parties: Vec<_> = dns_results
        .into_iter()
        .filter_map(|result| {
            let item = result.item?;
            let my_owner_key = item
                .owners
                .iter()
                .find(|owner| namespace_key_fingerprints.contains(*owner))
                .cloned()?;
            let p2p = p2p_by_namespace.get(&item.decentralized_namespace)?;
            // Namespace-scoped discovery does not know about the requested
            // prefix, so apply it to the party the namespace resolved to.
            if !prefix_filter.is_none_or(|prefix| p2p.party.starts_with(prefix)) {
                return None;
            }
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
                let party_id = CantonId::parse(&p2p.party)?;
                // Get token for this party from auth (real or mock).
                // Registry uses raw string keys (`_by_str`) so we still
                // need party_id_str for lookup.
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

                let packages = default_package_config();
                let token_clone = token.clone();
                let (contracts, local_metadata) = if token.is_some() || test_mode {
                    tokio::join!(
                        async {
                            get_contracts(&config, &party_id, token, &packages)
                                .await
                                .unwrap_or_else(|e| {
                                    tracing::warn!(
                                        "Failed to get contracts for {party_id_str}: {e}"
                                    );
                                    Vec::new()
                                })
                        },
                        async {
                            get_party_metadata(&config, &party_id, token_clone)
                                .await
                                .ok()
                                .flatten()
                        }
                    )
                } else {
                    (Vec::new(), None)
                };

                let self_uid = config.participant_id().to_string();
                let participants = p2p
                    .participants
                    .iter()
                    .filter_map(|p| {
                        let participant_uid = CantonId::parse(&p.participant_uid).ok()?;
                        let owner_key = if participant_uid.to_string() == self_uid {
                            Some(my_owner_key.clone())
                        } else {
                            None // resolved later via Noise polling of peers
                        };
                        Some(ParticipantInfo {
                            participant_uid,
                            permission: permission_from_proto(p.permission),
                            owner_key,
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
    // Reads topology vetting state. Neither list contains the other: a DAR can
    // be uploaded without being vetted, and a vetting can outlive its DAR
    // (e.g. after a restore from backup).
    match fetch_vetted_packages(&data.config).await {
        Ok(packages) => HttpResponse::Ok().json(packages),
        Err(e) => {
            tracing::error!("Failed to list vetted packages: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to list vetted packages: {e}"),
            })
        }
    }
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

    let mut status_futures = Vec::new();

    for peer in network_config.peers.iter() {
        let peer_id = peer.participant_id.to_string();
        let is_self = peer.participant_id == *current_participant_id;

        if is_self {
            status_futures.push(tokio::spawn(async move {
                ParticipantStatus {
                    id: peer_id,
                    status: ConnectionStatus::CurrentNode,
                    latency_ms: None,
                    workflow: None,
                    version: Some(crate::build_info::SEMVER.to_string()),
                    build_version: Some(crate::build_info::build_version().to_string()),
                }
            }));
            continue;
        }

        let peer_pub_key = parse_public_key(&peer.public_key).ok();
        let psk = peer_pub_key.map(|pk| keypair.derive_psk(&pk));
        let identity = current_participant_id.to_string();
        let address = peer.address.clone();
        let port = peer.port;
        let noise_retry_cfg = config.noise_retry.clone();

        status_futures.push(tokio::spawn(async move {
            let (Some(psk), Some(_)) = (psk, peer_pub_key) else {
                // Public key parse failed — no PSK available; classify as handshake-side.
                return ParticipantStatus {
                    id: peer_id,
                    status: ConnectionStatus::HandshakeFailed,
                    latency_ms: None,
                    workflow: None,
                    version: None,
                    build_version: None,
                };
            };

            let started = std::time::Instant::now();
            match send_noise_message_with_retry(
                &address,
                port,
                &psk,
                identity.as_bytes(),
                &Message::new_empty(MessageType::Health),
                &noise_retry_cfg,
            )
            .await
            {
                Ok(response) => {
                    // A successful Noise round-trip means the peer is reachable;
                    // classify_health_reply extracts its workflow state (or None
                    // if the peer is on older code that doesn't answer Health).
                    let latency_ms = u64::try_from(started.elapsed().as_millis()).ok();
                    let reply = classify_health_reply(&response);
                    ParticipantStatus {
                        id: peer_id,
                        status: reply.status,
                        latency_ms,
                        workflow: reply.workflow,
                        version: reply.version,
                        build_version: reply.build_version,
                    }
                }
                Err(e) => {
                    // Map NoiseError -> ConnectionStatus (binary semantics — Unreachable
                    // covers transport-side failures; HandshakeFailed covers everything
                    // else, matching prior behavior of this endpoint).
                    let status = match &e {
                        NoiseError::TcpConnectionTimeout(_)
                        | NoiseError::TcpConnectionFailed(_)
                        | NoiseError::Io(_)
                        | NoiseError::Hyper(_)
                        | NoiseError::RequestTimeout => ConnectionStatus::Unreachable,
                        _ => ConnectionStatus::HandshakeFailed,
                    };
                    ParticipantStatus {
                        id: peer_id,
                        status,
                        latency_ms: None,
                        workflow: None,
                        version: None,
                        build_version: None,
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

/// Pure mapping from `NoiseError` to the wire-stable `PeerErrorKind`.
///
/// What an operator should check, for a peer request that failed.
///
/// The raw error names a symptom — `snow error: input error`,
/// `Message too short: got 0` — and an operator reading it has no way to get
/// from there to an action. Each arm below names the thing to go and look at.
///
/// Exhaustive for the same reason as [`peer_error_kind_from_noise_err`]: a new
/// `NoiseError` variant must be given a hint rather than silently inheriting a
/// vague one.
fn peer_failure_hint(err: &NoiseError) -> &'static str {
    match err {
        NoiseError::TcpConnectionTimeout(_) | NoiseError::TcpConnectionFailed(_) => {
            "nothing is accepting connections on that address — check the peer is up and that \
             its advertised host/port reach it from here"
        }
        NoiseError::RequestTimeout => {
            "connected, but the peer never answered — it may be overloaded, or wedged mid-request"
        }
        NoiseError::Noise(_) | NoiseError::HandshakeFailed | NoiseError::DecryptionError => {
            "the Noise handshake failed — this node's key or the derived PSK does not match what \
             the peer expects, so check each side has the other's current public key"
        }
        NoiseError::BadStatusCode(..) => {
            "the peer answered but refused the request — it may not have this node registered as \
             a peer, or a load balancer in front of it has no healthy backend"
        }
        NoiseError::InvalidMessage => {
            "the peer answered with something this build cannot parse — most often an empty body \
             from a denied request or an unhealthy proxy, or a peer on an older wire format"
        }
        NoiseError::JsonSerialization(_) => {
            "the peer's payload did not deserialize — likely a version skew between the two builds"
        }
        NoiseError::Io(_) | NoiseError::Hyper(_) => {
            "the connection broke mid-request — check for a proxy or firewall closing idle \
             connections between the two nodes"
        }
        NoiseError::Http(_)
        | NoiseError::InvalidUri(_)
        | NoiseError::UriParsingError(_)
        | NoiseError::UnknownPeer(_)
        | NoiseError::Anyhow(_) => {
            "check this peer's address, port and public key in the peers table"
        }
    }
}

/// Exhaustive match (no wildcard) — adding a new `NoiseError` variant will
/// fail to compile here until it's explicitly classified.
fn peer_error_kind_from_noise_err(err: &NoiseError) -> PeerErrorKind {
    match err {
        NoiseError::TcpConnectionTimeout(_) => PeerErrorKind::TcpConnectTimeout,
        NoiseError::TcpConnectionFailed(_) => PeerErrorKind::TcpConnectFailed,
        NoiseError::RequestTimeout => PeerErrorKind::RequestTimeout,
        NoiseError::Io(_) | NoiseError::Hyper(_) => PeerErrorKind::Transport,
        NoiseError::Noise(_) | NoiseError::HandshakeFailed | NoiseError::DecryptionError => {
            PeerErrorKind::HandshakeFailed
        }
        NoiseError::BadStatusCode(..) => PeerErrorKind::BadStatus,
        NoiseError::InvalidMessage | NoiseError::JsonSerialization(_) => {
            PeerErrorKind::DecodeFailed
        }
        NoiseError::Http(_)
        | NoiseError::InvalidUri(_)
        | NoiseError::UriParsingError(_)
        | NoiseError::UnknownPeer(_)
        | NoiseError::Anyhow(_) => PeerErrorKind::Other,
    }
}

async fn fetch_peer_packages(
    config: &NodeConfig,
    db: &SqlitePool,
) -> Result<PeerPackageComparison> {
    let mut client = PackageServiceClient::new(config.admin_channel().await?);
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

    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = Arc::new(NoiseKeypair::from_file(&config.key_file_path()).await?);
    let current_participant_id = config.participant_id();

    let invite_message = Message::new_empty(MessageType::ListPackages);
    let noise_retry_cfg = config.noise_retry.clone();

    let peer_futures: Vec<_> = network_config
        .peers
        .iter()
        .filter(|p| p.participant_id != *current_participant_id && !p.public_key.is_empty())
        .map(|peer| {
            let keypair = Arc::clone(&keypair);
            let peer = peer.clone();
            let msg = invite_message.clone();
            let noise_retry_cfg = noise_retry_cfg.clone();
            async move {
                let peer_pub_key = match parse_public_key(&peer.public_key) {
                    Ok(pk) => pk,
                    Err(_) => {
                        return PeerPackageResult {
                            participant_id: peer.participant_id.to_string(),
                            name: peer.name.clone(),
                            reachable: false,
                            error_kind: Some(PeerErrorKind::InvalidPublicKey),
                            packages: vec![],
                        };
                    }
                };

                let psk = keypair.derive_psk(&peer_pub_key);
                let identity = current_participant_id.to_string();

                match send_noise_message_with_chunked_response(
                    &peer.address,
                    peer.port,
                    &psk,
                    identity.as_bytes(),
                    &msg,
                    &noise_retry_cfg,
                )
                .await
                {
                    Ok(response) => {
                        if let Ok(response_msg) = Message::from_bytes(&response)
                            && response_msg.msg_type == MessageType::Data
                            && let Ok(packages) =
                                serde_json::from_slice::<Vec<PackageInfo>>(&response_msg.payload)
                        {
                            return PeerPackageResult {
                                participant_id: peer.participant_id.to_string(),
                                name: peer.name.clone(),
                                reachable: true,
                                error_kind: None,
                                packages,
                            };
                        }
                        // 200 OK but unexpected message shape — `error_kind` stays
                        // None per the documented invariant; widening this case is
                        // tracked as Future work item 5 in the spec.
                        PeerPackageResult {
                            participant_id: peer.participant_id.to_string(),
                            name: peer.name.clone(),
                            reachable: true,
                            error_kind: None,
                            packages: vec![],
                        }
                    }
                    Err(e) => PeerPackageResult {
                        participant_id: peer.participant_id.to_string(),
                        name: peer.name.clone(),
                        reachable: false,
                        error_kind: Some(peer_error_kind_from_noise_err(&e)),
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

/// Query the local participant's vault for namespace key fingerprints.
/// Returns a set of fingerprints that identify this node as an owner.
async fn get_local_namespace_fingerprints(config: &NodeConfig) -> Result<HashSet<String>> {
    let mut vault_client = VaultServiceClient::new(config.admin_channel().await?)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    my_namespace_fingerprints(&mut vault_client).await
}

#[cfg(test)]
mod tests {
    use http::StatusCode;

    use super::*;
    use crate::{config::Network, db::MIGRATOR};

    /// Discovery is the same query on every network, because it reads the
    /// decentralized-namespace mappings rather than every party. MainNet used
    /// to be the size problem, so it must not be treated as a special case
    /// again: with nothing listening, every network has to fail rather than
    /// quietly answer empty.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn every_network_discovers_the_same_way(pool: SqlitePool) {
        for network in [Network::Devnet, Network::Testnet, Network::Mainnet] {
            let config = closed_admin_api(network);

            let result = fetch_decentralized_parties(&config, &pool, None, None, &[]).await;

            assert!(result.is_err(), "{network:?} must attempt discovery");
        }
    }

    /// A node that holds one party and is asked about a different prefix has
    /// no exact ID to use, so it must discover under that prefix rather than
    /// issue no query at all and report nothing.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_known_party_does_not_mask_another_prefix(pool: SqlitePool) -> anyhow::Result<()> {
        let config = closed_admin_api(Network::Mainnet);
        store_parties_to_db(&pool, "cbtc", &[a_party()?]).await?;

        let result = fetch_decentralized_parties(&config, &pool, Some("other"), None, &[]).await;

        assert!(
            result.is_err(),
            "an unmatched prefix must still be queried against Canton"
        );
        Ok(())
    }

    /// The whole fix rests on this string. Canton splits `filter_party` on
    /// `::` and compiles the namespace half into the store query, so an empty
    /// identifier half asks for every party in one namespace and nothing else.
    /// Drop the separator and it becomes an identifier prefix that matches no
    /// party at all.
    #[test]
    fn a_namespace_filter_leaves_the_identifier_half_empty() {
        let namespace = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

        let filter = namespace_party_filter(namespace);

        assert_eq!(filter, format!("::{namespace}"));
        let (identifier, matched_namespace) = filter
            .split_once("::")
            .expect("the filter must carry the separator Canton splits on");
        assert!(identifier.is_empty(), "identifier half must not narrow");
        assert_eq!(matched_namespace, namespace);
    }

    /// Nothing can listen on port 1, so any attempt to reach Canton fails
    /// instead of finding a participant a developer happens to be running.
    fn closed_admin_api(network: Network) -> NodeConfig {
        let mut config = NodeConfig::default();
        config.canton.network = network;
        config.canton.admin_api_host = "127.0.0.1".to_string();
        config.canton.admin_api_port = 1;
        config
    }

    /// A party this node does not belong to leaves no cached rows, so without
    /// a record of the completed discovery every request re-ran it. The
    /// boundary matters: the TTL second itself still answers from the record.
    #[test]
    fn a_completed_discovery_answers_until_the_ttl_passes() {
        let now = 1_000_000;

        assert!(!discovery_is_fresh(None, now));
        assert!(discovery_is_fresh(Some(now), now));
        assert!(discovery_is_fresh(Some(now - PARTIES_CACHE_TTL_SECS), now));
        assert!(!discovery_is_fresh(
            Some(now - PARTIES_CACHE_TTL_SECS - 1),
            now
        ));
    }

    /// A backwards clock jump must not pin the answer. A negative age would
    /// satisfy a plain `<= TTL` and hold the record forever.
    #[test]
    fn a_timestamp_from_the_future_is_not_fresh() {
        let now = 1_000_000;

        assert!(!discovery_is_fresh(Some(now + 1), now));
        assert!(!discovery_is_fresh(Some(now + 86_400), now));
    }

    /// Only an empty result is recorded. A non-empty one is served from
    /// `dec_parties`, and recording it would make a request that arrives
    /// before that write answer empty for the whole TTL.
    #[tokio::test]
    async fn only_an_empty_discovery_is_recorded() -> anyhow::Result<()> {
        let completed = Arc::new(tokio::sync::RwLock::new(HashMap::new()));

        record_discovery(&completed, "cbtc", &[a_party()?]).await;
        assert!(
            completed.read().await.is_empty(),
            "a non-empty discovery must not be recorded"
        );

        record_discovery(&completed, "cbtc", &[]).await;
        assert!(completed.read().await.contains_key("cbtc"));
        Ok(())
    }

    /// `prefix` comes from the request, so the map has to stay bounded whoever
    /// is asking.
    #[tokio::test]
    async fn recording_drops_expired_entries_and_stays_bounded() {
        let completed = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let stale = now_secs() - PARTIES_CACHE_TTL_SECS - 1;
        completed.write().await.insert("expired".to_string(), stale);

        record_discovery(&completed, "fresh", &[]).await;

        let seen = completed.read().await;
        assert!(!seen.contains_key("expired"), "expired entry survived");
        assert!(seen.contains_key("fresh"));
        drop(seen);

        for index in 0..MAX_TRACKED_PREFIXES + 1 {
            record_discovery(&completed, &format!("prefix-{index}"), &[]).await;
        }
        assert!(
            completed.read().await.len() <= MAX_TRACKED_PREFIXES,
            "the map grew past its cap"
        );
    }

    fn a_party() -> anyhow::Result<DecentralizedParty> {
        Ok(DecentralizedParty {
            party_id: CantonId::parse(
                "cbtc::1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892",
            )?,
            threshold: 1,
            owners: Vec::new(),
            my_owner_key: None,
            participants: Vec::new(),
            contracts: Vec::new(),
            local_metadata: None,
        })
    }

    #[test]
    fn peer_error_kind_mapping_known_variants() {
        // Construct one easily-instantiable example of each PeerErrorKind
        // category and assert the mapping. Hard-to-construct NoiseError
        // variants (Hyper, Noise, JsonSerialization, Http, InvalidUri) are
        // not exercised here — the helper's exhaustive match is what
        // guarantees they're classified. This test catches accidental
        // arm-swap regressions in the easy variants.
        let pairs: Vec<(NoiseError, PeerErrorKind)> = vec![
            (
                NoiseError::TcpConnectionTimeout("x".into()),
                PeerErrorKind::TcpConnectTimeout,
            ),
            (NoiseError::RequestTimeout, PeerErrorKind::RequestTimeout),
            (
                NoiseError::TcpConnectionFailed("x".into()),
                PeerErrorKind::TcpConnectFailed,
            ),
            (
                NoiseError::Io(std::io::Error::other("x")),
                PeerErrorKind::Transport,
            ),
            (NoiseError::HandshakeFailed, PeerErrorKind::HandshakeFailed),
            (NoiseError::DecryptionError, PeerErrorKind::HandshakeFailed),
            (
                NoiseError::BadStatusCode(StatusCode::INTERNAL_SERVER_ERROR, None),
                PeerErrorKind::BadStatus,
            ),
            (NoiseError::InvalidMessage, PeerErrorKind::DecodeFailed),
            (
                NoiseError::UriParsingError("x".into()),
                PeerErrorKind::Other,
            ),
            (NoiseError::UnknownPeer("x".into()), PeerErrorKind::Other),
        ];
        for (err, expected) in &pairs {
            let got = peer_error_kind_from_noise_err(err);
            assert_eq!(got, *expected, "for variant {err:?}");
        }
    }

    #[test]
    fn anyhow_variant_falls_through_to_other() {
        let err = NoiseError::Anyhow(anyhow::anyhow!("anything"));
        assert!(matches!(
            peer_error_kind_from_noise_err(&err),
            PeerErrorKind::Other
        ));
    }

    #[test]
    fn party_to_participant_fallback_scopes_to_local_participant() {
        // The no-local-knowledge onboarding fallback has to use an empty party
        // filter, but still post-filters the result to this participant.
        let request =
            build_party_to_participant_request("sync::physical", None, "participant::abc123");

        assert_eq!(request.filter_participant, "participant::abc123");
        assert_eq!(request.filter_party, "");
    }

    #[test]
    fn party_to_participant_request_uses_exact_party_and_participant() {
        let request = build_party_to_participant_request(
            "sync::physical",
            Some("alice::namespace"),
            "participant::abc123",
        );

        assert_eq!(request.filter_participant, "participant::abc123");
        assert_eq!(request.filter_party, "alice::namespace");
    }

    #[test]
    fn decentralized_namespace_request_uses_exact_namespace_filter() {
        let namespace = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        let request = build_decentralized_namespace_request("sync::physical", namespace);

        assert_eq!(request.filter_namespace, namespace);
    }

    #[test]
    fn known_party_filters_union_all_sources_deduplicate_and_scope() -> anyhow::Result<()> {
        let namespace_a = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        let namespace_b = "1220d5010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5893";
        let namespace_c = "1220e6010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5894";
        let credentials = |party: &str, namespace: &str| -> anyhow::Result<PartyCredentials> {
            Ok(PartyCredentials {
                dec_party_id: CantonId::parse(&format!("{party}::{namespace}"))?,
                member_party_id: CantonId::parse(&format!("member::{namespace}"))?,
                user_id: "user".to_string(),
                keycloak: Default::default(),
                auth0: None,
                packages: Default::default(),
            })
        };
        let configured = credentials("cbtc-configured", namespace_a)?;
        let parties = vec![configured.clone(), configured];
        let workflow_ids = vec![
            format!("cbtc-workflow::{namespace_b}"),
            format!("cbtc-configured::{namespace_a}"),
        ];
        let cached_ids = vec![
            format!("cbtc-cached::{namespace_c}"),
            format!("other-network::{namespace_b}"),
        ];

        assert_eq!(
            known_party_filters(
                &parties,
                workflow_ids.clone(),
                cached_ids.clone(),
                Some("cbtc-")
            ),
            vec![
                format!("cbtc-cached::{namespace_c}"),
                format!("cbtc-configured::{namespace_a}"),
                format!("cbtc-workflow::{namespace_b}"),
            ]
        );
        assert_eq!(
            known_party_filters(&parties, workflow_ids, cached_ids, Some("missing")),
            Vec::<String>::new()
        );
        Ok(())
    }

    /// Every hint must name something to go and look at. The failure this
    /// guards is a new `NoiseError` variant being handed a vague catch-all,
    /// which is how the logs got unactionable in the first place (#332).
    #[test]
    fn every_peer_failure_hint_is_actionable() {
        let cases = [
            NoiseError::TcpConnectionFailed("x".into()),
            NoiseError::TcpConnectionTimeout("x".into()),
            NoiseError::RequestTimeout,
            NoiseError::HandshakeFailed,
            NoiseError::DecryptionError,
            NoiseError::BadStatusCode(StatusCode::SERVICE_UNAVAILABLE, None),
            NoiseError::InvalidMessage,
            NoiseError::UnknownPeer("x".into()),
        ];
        for e in &cases {
            let hint = peer_failure_hint(e);
            assert!(!hint.is_empty(), "no hint for {e:?}");
            // "check", "the peer", an instruction of some kind — not a restatement.
            assert!(
                hint.len() > 30,
                "hint for {e:?} is too terse to act on: {hint}"
            );
        }

        // The two that used to be indistinguishable must not read alike.
        assert_ne!(
            peer_failure_hint(&NoiseError::InvalidMessage),
            peer_failure_hint(&NoiseError::TcpConnectionFailed("x".into())),
        );
    }

    /// The peer's own reason reaches the operator through Display, which is
    /// what `{e}` in the fan-out warnings renders.
    #[test]
    fn bad_status_code_surfaces_the_peers_reason() {
        let bare = NoiseError::BadStatusCode(StatusCode::SERVICE_UNAVAILABLE, None);
        let bare_rendered = format!("{bare}");
        assert!(
            bare_rendered.starts_with("Bad status code: 503"),
            "unexpected Display: {bare_rendered}"
        );
        assert!(!bare_rendered.contains("peer said"));

        let with_reason = NoiseError::BadStatusCode(
            StatusCode::SERVICE_UNAVAILABLE,
            Some("request rejected: unknown sender".to_string()),
        );
        let rendered = format!("{with_reason}");
        assert!(
            rendered.contains("unknown sender"),
            "reason missing from Display: {rendered}"
        );
    }
}
