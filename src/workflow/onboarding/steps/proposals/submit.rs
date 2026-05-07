use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{DecentralizedNamespaceDefinition, SignedTopologyTransaction},
    topology::admin::v30::{
        AddTransactionsRequest, BaseQuery, ListDecentralizedNamespaceDefinitionRequest,
        ListPartyToParticipantRequest, StoreId, Synchronizer, base_query, store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use sqlx::SqlitePool;
use tokio::time;

use crate::{
    config::NodeConfig,
    consts::{
        TOPOLOGY_PROPAGATION_DELAY_SECS, TOPOLOGY_RETRY_DELAY_SECS, TOPOLOGY_RETRY_MAX_ATTEMPTS,
    },
    error::Result,
    participant_id::CantonId,
    utils,
    workflow::{
        onboarding::OnboardingConfig,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// Aggregate and submit DNS proposals
///
/// This step must be run once by the coordinator after all attestors have signed the DNS proposal.
/// It aggregates all signatures and submits the fully-signed proposal to Canton.
pub async fn submit_dns_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
) -> Result {
    tracing::info!("Submitting DNS proposals...");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    let dns_bytes = storage
        .read_artifact(instance_name, artifact_kinds::DNS_PROTO, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("DNS_PROTO artifact missing — did CreateProposals run?"))?;
    let mut dns_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&dns_bytes)?;

    let signed_dns = storage
        .list_artifacts(instance_name, artifact_kinds::SIGNED_DNS_PROPOSAL)
        .await?;
    tracing::info!(
        "Found {count} signed DNS proposal artefacts",
        count = signed_dns.len()
    );

    for (attestor_id, signed_payload) in &signed_dns {
        tracing::info!("Reading signatures from attestor {attestor_id}");
        let signed_transactions: Vec<SignedTopologyTransaction> =
            decode_messages_from_bytes(signed_payload)?;

        for signed_tx in signed_transactions {
            dns_transaction
                .signatures
                .extend(signed_tx.signatures.clone());
        }
    }

    tracing::info!(
        "Aggregated DNS proposal has {count} signature(s)",
        count = dns_transaction.signatures.len()
    );

    tracing::info!("Submitting aggregated DNS proposal...");
    let mut topology_write_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(AddTransactionsRequest {
        transactions: vec![dns_transaction],
        force_changes: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.clone())),
            })),
        }),
        wait_to_become_effective: None,
    });

    topology_write_client.add_transactions(request).await?;
    tracing::info!("DNS proposal submitted to topology");

    let namespace_bytes = storage
        .read_artifact(instance_name, artifact_kinds::NAMESPACE_DEF, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("NAMESPACE_DEF artifact missing"))?;
    let namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_bytes(&namespace_bytes)?;

    tracing::info!(
        "Waiting for DNS to appear in topology for namespace {namespace}...",
        namespace = namespace_def.decentralized_namespace
    );
    wait_for_dns_in_topology(
        config,
        &synchronizer_id,
        &namespace_def.decentralized_namespace,
    )
    .await?;

    tracing::info!("DNS proposal submitted and confirmed in topology successfully");
    Ok(())
}

/// Wait for DNS to appear in topology by polling
async fn wait_for_dns_in_topology(
    config: &NodeConfig,
    synchronizer_id: &str,
    namespace: &str,
) -> Result {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::connect(config.admin_api_url()).await?;

    let max_attempts = TOPOLOGY_RETRY_MAX_ATTEMPTS;
    let retry_delay = time::Duration::from_secs(TOPOLOGY_RETRY_DELAY_SECS);

    for attempt in 1..=max_attempts {
        let request = tonic::Request::new(ListDecentralizedNamespaceDefinitionRequest {
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
            filter_namespace: namespace.to_string(),
        });

        let response = topology_read_client
            .list_decentralized_namespace_definition(request)
            .await?
            .into_inner();

        if !response.results.is_empty() {
            tracing::info!("DNS found in topology after {attempt} attempt(s)");
            return Ok(());
        }

        if attempt < max_attempts {
            tracing::debug!(
                "DNS not yet in topology, attempt {attempt}/{max_attempts}, retrying in {retry_delay:?}..."
            );
            time::sleep(retry_delay).await;
        }
    }

    anyhow::bail!("DNS did not appear in topology after {max_attempts} attempts")
}

/// Aggregate and submit P2P proposals
///
/// **Canton 3.4+**: Submits P2P proposals with embedded signing keys
/// (replaces the separate PartyToKeyMapping transactions from Canton 3.3).
///
/// This step must be run once by the coordinator after all attestors have signed the P2P proposals.
/// It aggregates all signatures and submits the fully-signed proposal to Canton.
pub async fn submit_final_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    onboarding_config: &OnboardingConfig,
) -> Result {
    tracing::info!("Submitting P2P proposal with embedded signing keys (Canton 3.4+)...");

    // Use party_id_prefix from onboarding config (provided via UI)
    let party_id_prefix = &onboarding_config.party_id_prefix;

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    let p2p_bytes = storage
        .read_artifact(instance_name, artifact_kinds::P2P_PROTO, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("P2P_PROTO artifact missing — did CreateProposals run?"))?;
    let mut p2p_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&p2p_bytes)?;

    let signed_p2p = storage
        .list_artifacts(instance_name, artifact_kinds::SIGNED_P2P_PROPOSAL)
        .await?;
    tracing::info!(
        "Found {count} signed P2P proposal artefacts",
        count = signed_p2p.len()
    );

    for (attestor_id, signed_payload) in &signed_p2p {
        tracing::info!("Reading signatures from attestor {attestor_id}");
        let signed_transactions: Vec<SignedTopologyTransaction> =
            decode_messages_from_bytes(signed_payload)?;

        if signed_transactions.len() != 1 {
            anyhow::bail!(
                "Expected 1 transaction from attestor {attestor_id}, got {count}",
                count = signed_transactions.len()
            );
        }

        p2p_transaction
            .signatures
            .extend(signed_transactions[0].signatures.clone());
    }

    tracing::info!(
        "Aggregated P2P proposal has {count} signature(s)",
        count = p2p_transaction.signatures.len()
    );

    let namespace_bytes = storage
        .read_artifact(instance_name, artifact_kinds::NAMESPACE_DEF, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("NAMESPACE_DEF artifact missing"))?;
    let namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_bytes(&namespace_bytes)?;

    let party_id_str = format!(
        "{party_id_prefix}::{namespace}",
        namespace = namespace_def.decentralized_namespace
    );
    let party_id = CantonId::parse(&party_id_str)?;
    tracing::info!("Constructed party ID: {party_id}");

    tracing::info!("Submitting aggregated P2P proposal...");
    let mut topology_write_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(AddTransactionsRequest {
        transactions: vec![p2p_transaction.clone()],
        force_changes: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.clone())),
            })),
        }),
        wait_to_become_effective: None,
    });

    topology_write_client.add_transactions(request).await?;
    tracing::info!("P2P proposal submitted to topology");

    tracing::info!("Waiting for P2P to appear in topology...");
    let effective_time = wait_for_p2p_in_topology(config, &synchronizer_id, &party_id).await?;

    tracing::info!("P2P proposal submitted and confirmed in topology successfully");

    let now = std::time::SystemTime::now();
    let effective_system_time = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(effective_time.seconds as u64)
        + std::time::Duration::from_nanos(effective_time.nanos as u64);

    if let Ok(wait_duration) = effective_system_time.duration_since(now) {
        tracing::info!(
            "P2P mapping will become effective in {wait_duration:?}. Waiting for topology effective time..."
        );
        tokio::time::sleep(wait_duration).await;
        tracing::info!("Topology is now effective");
    } else {
        tracing::info!("P2P mapping is already effective");
    }

    let propagation_delay = time::Duration::from_secs(TOPOLOGY_PROPAGATION_DELAY_SECS);
    tracing::info!("Waiting {propagation_delay:?} for Canton to propagate topology updates...");
    time::sleep(propagation_delay).await;
    tracing::info!("Topology propagation wait complete");

    Ok(())
}

/// Decode multiple consecutive `varint(len)||proto` messages from a single
/// payload. Mirrors `utils::read_all_messages_from_file` but operates on
/// in-memory bytes instead of a file path.
fn decode_messages_from_bytes<M: prost::Message + Default>(payload: &[u8]) -> Result<Vec<M>> {
    let mut cursor: &[u8] = payload;
    let mut out = Vec::new();
    while !cursor.is_empty() {
        let len = prost::encoding::decode_varint(&mut cursor)? as usize;
        if cursor.len() < len {
            anyhow::bail!(
                "Truncated message stream: expected {len} bytes, only {remaining} remain",
                remaining = cursor.len()
            );
        }
        let (msg_bytes, rest) = cursor.split_at(len);
        let msg = M::decode(msg_bytes)?;
        out.push(msg);
        cursor = rest;
    }
    Ok(out)
}

/// Wait for P2P (PartyToParticipant) to appear in topology by polling
/// Returns the effective time (valid_from) when the P2P mapping becomes active
async fn wait_for_p2p_in_topology(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &CantonId,
) -> Result<prost_types::Timestamp> {
    let party_id_str = party_id.to_string();
    let mut topology_read_client =
        TopologyManagerReadServiceClient::connect(config.admin_api_url()).await?;

    let max_attempts = TOPOLOGY_RETRY_MAX_ATTEMPTS;
    let retry_delay = time::Duration::from_secs(TOPOLOGY_RETRY_DELAY_SECS);

    for attempt in 1..=max_attempts {
        let request = tonic::Request::new(ListPartyToParticipantRequest {
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
            filter_party: party_id_str.clone(),
            filter_participant: String::new(),
        });

        let response = topology_read_client
            .list_party_to_participant(request)
            .await?
            .into_inner();

        if let Some(result) = response.results.first() {
            tracing::info!("P2P found in topology after {attempt} attempt(s)");

            if let Some(context) = &result.context {
                if let Some(valid_from) = &context.valid_from {
                    tracing::debug!(
                        "P2P mapping effective time: {seconds}.{nanos:09}s",
                        seconds = valid_from.seconds,
                        nanos = valid_from.nanos
                    );
                    return Ok(*valid_from);
                } else {
                    anyhow::bail!("P2P mapping found but has no valid_from timestamp");
                }
            } else {
                anyhow::bail!("P2P mapping found but has no context");
            }
        }

        if attempt < max_attempts {
            tracing::debug!(
                "P2P not yet in topology, attempt {attempt}/{max_attempts}, retrying in {retry_delay:?}..."
            );
            time::sleep(retry_delay).await;
        }
    }

    anyhow::bail!("P2P did not appear in topology after {max_attempts} attempts")
}
