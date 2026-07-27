use std::collections::HashSet;

use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{DecentralizedNamespaceDefinition, SignedTopologyTransaction},
    topology::admin::v30::{
        AddTransactionsRequest, BaseQuery, ListDecentralizedNamespaceDefinitionRequest,
        ListPartyToParticipantRequest, SignTransactionsRequest, StoreId, Synchronizer, base_query,
        store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use sqlx::SqlitePool;
use tokio::time;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    consts::{
        TOPOLOGY_PROPAGATION_DELAY_SECS, topology_retry_delay_secs, topology_retry_max_attempts,
    },
    error::Result,
    utils,
    workflow::{
        onboarding::OnboardingConfig,
        storage::{WorkflowStorage, artifact_kinds},
        topology::{
            dedupe_signatures, sign_transactions_with_topology_retry, synchronizer_store_id,
        },
    },
};

/// Aggregate and submit DNS proposals
///
/// This step must be run once by the coordinator after all peers have signed the DNS proposal.
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

    for (peer_id, signed_payload) in &signed_dns {
        tracing::info!("Reading signatures from peer {peer_id}");
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
        TopologyManagerWriteServiceClient::new(config.admin_channel().await?);

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
        TopologyManagerReadServiceClient::new(config.admin_channel().await?);

    let max_attempts = topology_retry_max_attempts();
    let retry_delay = time::Duration::from_secs(topology_retry_delay_secs());

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
/// This step must be run once by the coordinator after all peers have signed the P2P proposals.
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

    for (peer_id, signed_payload) in &signed_p2p {
        tracing::info!("Reading signatures from peer {peer_id}");
        let signed_transactions: Vec<SignedTopologyTransaction> =
            decode_messages_from_bytes(signed_payload)?;

        if signed_transactions.len() != 1 {
            anyhow::bail!(
                "Expected 1 transaction from peer {peer_id}, got {count}",
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

    // Only worth a round trip when an owner signature is genuinely absent —
    // which, thanks to the Authorized-store gap described on
    // `add_coordinator_signatures`, is the coordinator's own on every run that
    // reaches this point.
    let missing_owners = owners_missing_signatures(&namespace_def, &p2p_transaction);
    let p2p_transaction = if missing_owners.is_empty() {
        p2p_transaction
    } else {
        tracing::info!(
            "P2P aggregate is missing {count} owner-namespace signature(s); \
             re-signing against the synchronizer store",
            count = missing_owners.len()
        );
        add_coordinator_signatures(config, &synchronizer_id, p2p_transaction).await?
    };
    ensure_owner_threshold_signed(&namespace_def, &p2p_transaction)?;

    let party_id_str = format!(
        "{party_id_prefix}::{namespace}",
        namespace = namespace_def.decentralized_namespace
    );
    let party_id = CantonId::parse(&party_id_str)?;
    tracing::info!("Constructed party ID: {party_id}");

    tracing::info!("Submitting aggregated P2P proposal...");
    let mut topology_write_client =
        TopologyManagerWriteServiceClient::new(config.admin_channel().await?);

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

/// Re-sign the aggregated P2P proposal with every key this node can still
/// contribute, against the **synchronizer** store.
///
/// `create_proposals` builds the P2P in the coordinator's *Authorized* store,
/// where the decentralized namespace is at that point nothing but a
/// partially-signed proposal. Canton's key auto-selection cannot chain the
/// party's namespace through a not-yet-authorized DNS, so the coordinator's
/// signature set comes back as participant + party signing key only — its
/// owner-namespace signature is missing. Peers don't hit this: they sign
/// against the synchronizer store after `submit_dns_proposals` activated the
/// DNS, so their namespace key resolves.
///
/// With the default majority threshold the peers' namespace signatures alone
/// clear the bar and the gap is invisible. With a unanimous threshold
/// (`threshold == owners`) every owner must sign, the aggregate stays one
/// signature short, and the proposal sits pending on the synchronizer forever
/// — retry re-submits the identical aggregate and can never recover (#261).
///
/// By this point the DNS *is* active, so making the same `SignTransactions`
/// call the peers make picks up the namespace key. Keys already on the
/// transaction re-sign to identical bytes (Ed25519 is deterministic), hence
/// the dedupe — Canton rejects a transaction carrying the same signature
/// twice.
async fn add_coordinator_signatures(
    config: &NodeConfig,
    synchronizer_id: &str,
    transaction: SignedTopologyTransaction,
) -> Result<SignedTopologyTransaction> {
    let before = transaction.signatures.len();

    let request = SignTransactionsRequest {
        transactions: vec![transaction],
        signed_by: vec![],
        store: Some(synchronizer_store_id(synchronizer_id)),
        force_flags: vec![],
    };
    let response = sign_transactions_with_topology_retry(config, request, "P2P").await?;
    let mut signed = response
        .transactions
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No signed transaction returned for coordinator P2P"))?;

    dedupe_signatures(&mut signed);
    tracing::info!(
        "Coordinator re-signed P2P against the synchronizer store: {before} -> {after} signature(s)",
        after = signed.signatures.len()
    );
    Ok(signed)
}

/// The namespace owners that have not signed `transaction`.
///
/// Each owner of a decentralized namespace is the fingerprint of that node's
/// namespace root key, and `generate_keys` delegates each namespace to that
/// same key, so an owner's signature carries `signed_by == <owner>`.
fn owners_missing_signatures<'a>(
    namespace_def: &'a DecentralizedNamespaceDefinition,
    transaction: &SignedTopologyTransaction,
) -> Vec<&'a str> {
    let signers: HashSet<&str> = transaction
        .signatures
        .iter()
        .map(|sig| sig.signed_by.as_str())
        .collect();

    namespace_def
        .owners
        .iter()
        .map(String::as_str)
        .filter(|owner| !signers.contains(owner))
        .collect()
}

/// Fail before submitting when fewer than `threshold` of the namespace owners
/// have signed the P2P proposal.
///
/// Canton accepts such a proposal, stores it as pending, and never activates
/// it; the only symptom is `wait_for_p2p_in_topology` timing out with no clue
/// as to which node's signature is absent. Name the missing owners instead.
fn ensure_owner_threshold_signed(
    namespace_def: &DecentralizedNamespaceDefinition,
    transaction: &SignedTopologyTransaction,
) -> Result {
    let missing = owners_missing_signatures(namespace_def, transaction);
    let signed = namespace_def.owners.len() - missing.len();
    if (signed as i32) < namespace_def.threshold {
        anyhow::bail!(
            "P2P proposal carries {signed} of the {threshold} required owner-namespace \
             signature(s) ({total} owner(s) total); submitting it would leave the mapping \
             pending on the synchronizer forever. Owners that have not signed: {missing}",
            threshold = namespace_def.threshold,
            total = namespace_def.owners.len(),
            missing = missing.join(", "),
        );
    }

    tracing::info!(
        "P2P proposal has {signed} of {total} owner-namespace signature(s) (threshold {threshold})",
        total = namespace_def.owners.len(),
        threshold = namespace_def.threshold,
    );
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
        TopologyManagerReadServiceClient::new(config.admin_channel().await?);

    let max_attempts = topology_retry_max_attempts();
    let retry_delay = time::Duration::from_secs(topology_retry_delay_secs());

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

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::crypto::v30::Signature;

    use super::*;

    fn namespace_def(threshold: i32, owners: &[&str]) -> DecentralizedNamespaceDefinition {
        DecentralizedNamespaceDefinition {
            decentralized_namespace: "1220dec".to_string(),
            threshold,
            owners: owners.iter().map(|o| (*o).to_string()).collect(),
        }
    }

    fn signed_by(fingerprints: &[&str]) -> SignedTopologyTransaction {
        SignedTopologyTransaction {
            signatures: fingerprints
                .iter()
                .map(|fp| Signature {
                    signed_by: (*fp).to_string(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    /// The predicate that decides whether the coordinator re-signs: after
    /// `create_proposals` its own owner-namespace signature is the one absent.
    #[test]
    fn reports_the_owner_whose_namespace_signature_is_absent() {
        let def = namespace_def(1, &["1220aaa", "1220bbb"]);
        let tx = signed_by(&["1220bbb", "1220part1", "1220daml1"]);

        assert_eq!(owners_missing_signatures(&def, &tx), ["1220aaa"]);
    }

    /// The one case that skips the extra `SignTransactions` round trip: every
    /// owner has already signed, so there is nothing left to contribute.
    #[test]
    fn reports_nothing_missing_when_every_owner_signed() {
        let def = namespace_def(1, &["1220aaa", "1220bbb"]);
        let tx = signed_by(&["1220aaa", "1220bbb", "1220part1"]);

        assert!(owners_missing_signatures(&def, &tx).is_empty());
    }

    /// The #261 shape: two owners, unanimous threshold, and the coordinator's
    /// own namespace signature missing. Canton would park this as a pending
    /// proposal; we reject it up-front and name the owner that didn't sign.
    #[test]
    fn rejects_p2p_short_of_a_unanimous_threshold() {
        let def = namespace_def(2, &["1220aaa", "1220bbb"]);
        // participant + party signing keys are present, only the coordinator's
        // owner-namespace key (1220aaa) is absent.
        let tx = signed_by(&["1220bbb", "1220part1", "1220daml1", "1220daml2"]);

        let message = match ensure_owner_threshold_signed(&def, &tx) {
            Ok(()) => panic!("expected the missing owner-namespace signature to be rejected"),
            Err(e) => e.to_string(),
        };
        assert!(message.contains("1220aaa"), "unhelpful error: {message}");
        assert!(message.contains("1 of the 2"), "unhelpful error: {message}");
    }

    /// Why the bug survived every integration test and testnet run: a majority
    /// threshold is satisfied by the peers alone, so the coordinator's missing
    /// signature is invisible. This must stay accepted.
    #[test]
    fn accepts_majority_threshold_without_every_owner() {
        let def = namespace_def(2, &["1220aaa", "1220bbb", "1220ccc"]);
        let tx = signed_by(&["1220bbb", "1220ccc", "1220part1"]);

        assert!(ensure_owner_threshold_signed(&def, &tx).is_ok());
    }

    #[test]
    fn accepts_p2p_signed_by_every_owner() {
        let def = namespace_def(2, &["1220aaa", "1220bbb"]);
        let tx = signed_by(&["1220aaa", "1220bbb", "1220part1", "1220part2"]);

        assert!(ensure_owner_threshold_signed(&def, &tx).is_ok());
    }

    /// Signatures from keys outside the owner set (hosting participants, party
    /// signing keys) must not be counted towards the owner threshold.
    #[test]
    fn does_not_count_non_owner_signatures() {
        let def = namespace_def(2, &["1220aaa", "1220bbb"]);
        let tx = signed_by(&["1220bbb", "1220part1", "1220part2", "1220daml1"]);

        assert!(ensure_owner_threshold_signed(&def, &tx).is_err());
    }
}
