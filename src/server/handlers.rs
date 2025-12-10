use std::collections::HashMap;

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

use super::{
    AppState,
    queries::{get_contracts, get_party_metadata},
    types::{DecentralizedPartiesResponse, DecentralizedParty, ParticipantInfo, Permission},
};
use crate::{config::NodeConfig, error::Result, participant_id::CantonId, utils};

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
                    .map(|p| ParticipantInfo {
                        participant_uid: p.participant_uid.clone(),
                        permission: Permission::from(p.permission),
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
