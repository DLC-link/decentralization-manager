use std::{
    collections::{HashMap, HashSet},
    future::Future,
    time::Duration,
};

use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{SignedTopologyTransaction, TopologyTransaction, topology_mapping},
    topology::admin::v30::{
        ForceFlag, SignTransactionsRequest,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    consts::topology_propagation_delay_secs,
    error::Result,
    utils,
    workflow::{storage::WorkflowStorage, topology_version},
};

use super::{
    reads::signed_head_for,
    requests::{add_transactions_request, synchronizer_store_id},
    retry::sign_transactions_with_topology_retry,
};

/// Where a proposal stands against the transaction the synchronizer holds for
/// the same mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// The proposal carries the next serial, so it has not been published.
    Next,
    /// The proposal is the head transaction itself. An earlier attempt of the
    /// same submit published it, before a restart or a later failure.
    InForce,
}

/// Compare a proposal with the head transaction for its mapping.
///
/// The coordinator pins each serial when it builds a proposal, and the peers
/// sign that serial. A change that lands before submit moves the head. The
/// proposal must then not be published: Canton refuses a stale serial, and a
/// DNS that goes in while its P2P is refused leaves the party half changed.
///
/// A proposal already distributed while pending, by add-party or by the
/// signing fallback above threshold 1, is not in the head state. It still
/// reads as the next serial, and submit re-publishes it with the peers'
/// signatures merged in.
///
/// `head` is `None` when the synchronizer holds no transaction for the
/// mapping, which only a first serial can follow.
///
/// # Errors
///
/// Errors when the head moved, naming both serials.
pub fn proposal_standing(
    what: &str,
    proposal: &TopologyTransaction,
    head: Option<&TopologyTransaction>,
) -> Result<Standing> {
    if head == Some(proposal) {
        return Ok(Standing::InForce);
    }
    let head_serial = head.map_or(0, |h| h.serial);
    if head_serial.checked_add(1) == Some(proposal.serial) {
        return Ok(Standing::Next);
    }
    if head_serial == proposal.serial {
        anyhow::bail!(
            "Another {what} change already holds serial {head_serial}, the serial this \
             proposal was built for. The proposal is not submitted; start the workflow again \
             so it is built on the current state"
        );
    }
    anyhow::bail!(
        "The {what} proposal is at serial {proposal_serial}, but the synchronizer holds \
         serial {head_serial}. Another change landed after the proposals were built, so the \
         proposal is not submitted; start the workflow again so it is built on the current \
         state",
        proposal_serial = proposal.serial
    )
}

/// Read the head transaction for a proposal's mapping and decide whether the
/// proposal may still be published. See [`proposal_standing`].
///
/// # Errors
///
/// Errors when the proposal is neither a DNS nor a P2P, when the head cannot
/// be read, or when the head moved.
pub async fn check_proposal_standing(
    config: &NodeConfig,
    synchronizer_id: &str,
    proposal: &SignedTopologyTransaction,
) -> Result<Standing> {
    let transaction: TopologyTransaction =
        topology_version::decode_transaction(&proposal.transaction)?;
    let (what, head) = match transaction
        .mapping
        .as_ref()
        .and_then(|m| m.mapping.as_ref())
    {
        Some(topology_mapping::Mapping::DecentralizedNamespaceDefinition(def)) => {
            let namespace = def.decentralized_namespace.as_str();
            let head = signed_head_for(config, synchronizer_id, namespace, |mapping| {
                matches!(
                    mapping,
                    topology_mapping::Mapping::DecentralizedNamespaceDefinition(head)
                        if head.decentralized_namespace == namespace
                )
            })
            .await?;
            ("DNS", head)
        }
        Some(topology_mapping::Mapping::PartyToParticipant(p2p)) => {
            let party = p2p.party.as_str();
            let namespace = CantonId::parse(party)?.namespace.to_hex();
            let head = signed_head_for(config, synchronizer_id, &namespace, |mapping| {
                matches!(
                    mapping,
                    topology_mapping::Mapping::PartyToParticipant(head) if head.party == party
                )
            })
            .await?;
            ("P2P", head)
        }
        _ => anyhow::bail!("Only a DNS or a P2P proposal can be checked against the head"),
    };
    proposal_standing(what, &transaction, head.as_ref().map(|(_, head)| head))
}

/// Sign the DNS + P2P topology proposal pair for a topology-changing workflow.
///
/// `proposal_data` is the `[dns, p2p]` length-prefixed pair the coordinator
/// sent (each element a `varint(len)||SignedTopologyTransaction` blob). Signs
/// both with the participant's topology keys — retrying the transient
/// `TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE` via
/// [`sign_transactions_with_topology_retry`] — and persists the signed results
/// as `kinds.signed_dns` / `kinds.signed_p2p`, keyed by this node's
/// participant id. Each artefact is a single `varint(len)||proto` blob, so the
/// coordinator reads them back with `utils::read_first_message_from_bytes` and
/// the on-wire bytes stay byte-identical to the original combined-file format.
///
/// `label` is a short workflow tag (`"kick"`, `"add-party"`,
/// `"change-threshold"`) included in log lines and the sign-retry diagnostics.
pub async fn sign_dns_p2p_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    proposal_data: &[u8],
    label: &str,
    kinds: DnsP2pArtifactKinds<'_>,
) -> Result {
    tracing::info!("Signing {label} proposals...");

    let node_id = config.participant_id().to_string();
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    let items = utils::decode_length_prefixed(proposal_data, 2)?;
    let dns_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&items[0])?;
    let p2p_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&items[1])?;

    let request = SignTransactionsRequest {
        transactions: vec![dns_transaction, p2p_transaction],
        signed_by: vec![],
        store: Some(synchronizer_store_id(&synchronizer_id)),
        force_flags: vec![],
    };

    tracing::debug!("Calling SignTransactions RPC for {label} proposals...");
    let response = sign_transactions_with_topology_retry(config, request, label).await?;

    if response.transactions.len() != 2 {
        anyhow::bail!(
            "Expected 2 signed transactions (DNS and P2P), got {count}",
            count = response.transactions.len()
        );
    }

    // Persist signed DNS + P2P as separate per-peer artefacts, each
    // `varint(len)||proto`, so their concatenation is byte-identical to what
    // `write_messages_to_file(&[dns, p2p], path)` produced before.
    storage
        .write_artifact(
            instance_name,
            kinds.signed_dns,
            Some(&node_id),
            &utils::encode_length_prefixed_message(&response.transactions[0]),
        )
        .await?;
    storage
        .write_artifact(
            instance_name,
            kinds.signed_p2p,
            Some(&node_id),
            &utils::encode_length_prefixed_message(&response.transactions[1]),
        )
        .await?;

    tracing::info!("{label} proposals signed successfully");
    Ok(())
}

/// The four artefact kinds a topology submit joins when aggregating peer
/// signatures: the coordinator's original DNS / P2P proposals and the per-peer
/// signed DNS / P2P blobs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DnsP2pArtifactKinds<'a> {
    pub dns_proposal: &'a str,
    pub p2p_proposal: &'a str,
    pub signed_dns: &'a str,
    pub signed_p2p: &'a str,
}

/// Aggregate every peer's signatures onto the coordinator's original DNS and
/// P2P proposals.
///
/// Reads the coordinator's `dns_proposal` / `p2p_proposal` artefacts, then
/// joins the per-peer `signed_dns` / `signed_p2p` artefacts by peer id so the
/// two signatures for each peer stay paired the way the original combined-file
/// format guaranteed. Returns the two proposals with all peer signatures
/// merged in (the coordinator's own signature is already on the originals).
///
/// Callers that may resubmit — where a peer could sign twice or the
/// coordinator's own signature could be re-added — should run
/// [`dedupe_signatures`] on the results before submitting.
pub async fn aggregate_dns_p2p_signatures(
    storage: &SqlitePool,
    instance_name: &str,
    kinds: DnsP2pArtifactKinds<'_>,
) -> Result<(SignedTopologyTransaction, SignedTopologyTransaction)> {
    let dns_bytes = storage
        .read_artifact(instance_name, kinds.dns_proposal, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("{kind} artifact missing", kind = kinds.dns_proposal))?;
    let mut dns_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&dns_bytes)?;

    let p2p_bytes = storage
        .read_artifact(instance_name, kinds.p2p_proposal, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("{kind} artifact missing", kind = kinds.p2p_proposal))?;
    let mut p2p_transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&p2p_bytes)?;

    // Join DNS and P2P signatures by peer id so each peer's pair stays paired.
    let signed_dns = storage
        .list_artifacts(instance_name, kinds.signed_dns)
        .await?;
    let signed_p2p: HashMap<String, Vec<u8>> = storage
        .list_artifacts(instance_name, kinds.signed_p2p)
        .await?
        .into_iter()
        .collect();

    tracing::info!(
        "Found signed proposals from {count} peer(s)",
        count = signed_dns.len()
    );
    if signed_dns.len() != signed_p2p.len() {
        anyhow::bail!(
            "Mismatched signed proposal counts: {dns} DNS vs {p2p} P2P",
            dns = signed_dns.len(),
            p2p = signed_p2p.len()
        );
    }

    for (peer_id, dns_signed_bytes) in &signed_dns {
        tracing::info!("Aggregating signatures from peer {peer_id}");
        let dns_signed: SignedTopologyTransaction =
            utils::read_first_message_from_bytes(dns_signed_bytes)?;
        let p2p_signed_bytes = signed_p2p
            .get(peer_id)
            .ok_or_else(|| anyhow::anyhow!("Peer {peer_id} signed DNS but not P2P"))?;
        let p2p_signed: SignedTopologyTransaction =
            utils::read_first_message_from_bytes(p2p_signed_bytes)?;

        dns_transaction.signatures.extend(dns_signed.signatures);
        p2p_transaction.signatures.extend(p2p_signed.signatures);
    }

    tracing::info!(
        "Final DNS proposal has {dns} signature(s), P2P has {p2p}",
        dns = dns_transaction.signatures.len(),
        p2p = p2p_transaction.signatures.len()
    );

    Ok((dns_transaction, p2p_transaction))
}

/// Drop duplicate signatures, keeping the first per signing fingerprint.
///
/// Canton rejects a submitted transaction carrying the same signature twice —
/// which can happen when the coordinator's own signature is already on a
/// proposal and a retried peer signs again.
pub fn dedupe_signatures(transaction: &mut SignedTopologyTransaction) {
    let mut seen = HashSet::new();
    transaction
        .signatures
        .retain(|sig| seen.insert(sig.signed_by.clone()));
}

/// The force flags a party's DNS and P2P proposals carry, both where they are
/// signed and where they are published.
///
/// Add-party built its proposals with `AllowUnvalidatedSigningKeys`, because a
/// new member's keys may not have reached the synchronizer store yet. The pair
/// is now signed in a temporary store and first validated by the synchronizer
/// store at `AddTransactions`, so the flags travel to both places. Kick and
/// change-threshold share them rather than keeping a second code path.
pub fn party_proposal_force_flags() -> Vec<i32> {
    vec![ForceFlag::AllowUnvalidatedSigningKeys as i32]
}

/// The signed pair a topology workflow publishes, with the force flags their
/// publication needs.
///
/// Bundled because all three travel together from proposal creation to
/// submission, and because the flags only mean anything alongside the
/// transactions they let through.
pub struct DnsP2pSubmission {
    pub dns: SignedTopologyTransaction,
    pub p2p: SignedTopologyTransaction,
    pub force_changes: Vec<i32>,
}

/// Submit the aggregated DNS mapping, await its workflow-specific
/// confirmation, then submit the P2P mapping and await its confirmation,
/// finishing with the shared topology-propagation delay.
///
/// The submission order (DNS before P2P) and the trailing propagation delay
/// are identical across the topology workflows; only the post-submit
/// head-state checks differ — kick polls for mere existence, change-threshold
/// for the new threshold value, add-party for owner / participant membership —
/// so each caller supplies those as `confirm_dns` / `confirm_p2p`.
///
/// Each proposal carries the serial the coordinator pinned when it built it.
/// A serial that moved since then stops the submit before anything is
/// published (see [`proposal_standing`]); one already in force, from an
/// earlier attempt of the same submit, is not sent again.
///
/// `label` is a short workflow tag included in the log lines.
pub async fn submit_dns_then_p2p<DnsFut, P2pFut>(
    config: &NodeConfig,
    synchronizer_id: &str,
    label: &str,
    submission: DnsP2pSubmission,
    confirm_dns: impl FnOnce() -> DnsFut,
    confirm_p2p: impl FnOnce() -> P2pFut,
) -> Result
where
    DnsFut: Future<Output = Result>,
    P2pFut: Future<Output = Result>,
{
    let DnsP2pSubmission {
        dns: dns_transaction,
        p2p: p2p_transaction,
        force_changes,
    } = submission;

    // Both serials are checked before either transaction goes out. The DNS is
    // submitted first, so a P2P whose serial moved would otherwise be refused
    // only after the namespace change was already in force.
    let dns_standing = check_proposal_standing(config, synchronizer_id, &dns_transaction).await?;
    check_proposal_standing(config, synchronizer_id, &p2p_transaction).await?;

    if dns_standing == Standing::InForce {
        tracing::info!(
            label,
            "DNS proposal is already in force; not submitting it again"
        );
    } else {
        tracing::info!("Submitting DNS {label} proposal...");
        TopologyManagerWriteServiceClient::new(config.admin_channel().await?)
            .add_transactions(tonic::Request::new(add_transactions_request(
                synchronizer_id,
                dns_transaction,
                force_changes.clone(),
            )))
            .await?;
    }
    confirm_dns().await?;
    tracing::info!("DNS {label} confirmed in topology");

    submit_p2p(
        config,
        synchronizer_id,
        label,
        p2p_transaction,
        force_changes,
        confirm_p2p,
    )
    .await
}

/// Submit the aggregated P2P mapping alone, await its confirmation, and finish
/// with the shared topology-propagation delay. The second half of
/// [`submit_dns_then_p2p`], for a change that leaves the namespace as it is.
///
/// The P2P serial is checked against the synchronizer again right before it
/// is published, which narrows the window a concurrent change can use.
pub async fn submit_p2p<P2pFut>(
    config: &NodeConfig,
    synchronizer_id: &str,
    label: &str,
    p2p_transaction: SignedTopologyTransaction,
    force_changes: Vec<i32>,
    confirm_p2p: impl FnOnce() -> P2pFut,
) -> Result
where
    P2pFut: Future<Output = Result>,
{
    if check_proposal_standing(config, synchronizer_id, &p2p_transaction).await?
        == Standing::InForce
    {
        tracing::info!(
            label,
            "P2P proposal is already in force; not submitting it again"
        );
    } else {
        tracing::info!("Submitting P2P {label} proposal...");
        TopologyManagerWriteServiceClient::new(config.admin_channel().await?)
            .add_transactions(tonic::Request::new(add_transactions_request(
                synchronizer_id,
                p2p_transaction,
                force_changes,
            )))
            .await?;
    }
    confirm_p2p().await?;
    tracing::info!("P2P {label} confirmed in topology");

    let propagation_delay = Duration::from_secs(topology_propagation_delay_secs());
    tracing::info!("Waiting {propagation_delay:?} for Canton to propagate topology updates...");
    tokio::time::sleep(propagation_delay).await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::{
        DecentralizedNamespaceDefinition, enums,
    };

    use super::*;

    #[test]
    fn dedupe_signatures_keeps_first_per_fingerprint() {
        use canton_proto_rs::com::digitalasset::canton::crypto::v30::Signature;

        let sig = |signed_by: &str| Signature {
            signed_by: signed_by.to_string(),
            ..Default::default()
        };
        // Fingerprints a and b repeat; dedupe must keep the first of each,
        // preserving order — Canton rejects a duplicate signature outright.
        let mut transaction = SignedTopologyTransaction {
            signatures: vec![sig("a"), sig("b"), sig("a"), sig("c"), sig("b")],
            ..Default::default()
        };

        dedupe_signatures(&mut transaction);

        let kept: Vec<&str> = transaction
            .signatures
            .iter()
            .map(|s| s.signed_by.as_str())
            .collect();
        assert_eq!(kept, ["a", "b", "c"]);
    }

    fn namespace_change(serial: u32, threshold: i32) -> TopologyTransaction {
        use canton_proto_rs::com::digitalasset::canton::protocol::v30::TopologyMapping;
        TopologyTransaction {
            operation: enums::TopologyChangeOp::AddReplace as i32,
            serial,
            mapping: Some(TopologyMapping {
                mapping: Some(topology_mapping::Mapping::DecentralizedNamespaceDefinition(
                    DecentralizedNamespaceDefinition {
                        decentralized_namespace: "ns".into(),
                        threshold,
                        owners: vec!["a".into(), "b".into()],
                    },
                )),
            }),
        }
    }

    fn standing_error(
        proposal: &TopologyTransaction,
        head: Option<&TopologyTransaction>,
    ) -> Result<String> {
        match proposal_standing("DNS", proposal, head) {
            Ok(standing) => anyhow::bail!("expected a refusal, got {standing:?}"),
            Err(e) => Ok(format!("{e:#}")),
        }
    }

    #[test]
    fn a_proposal_at_the_next_serial_may_be_published() -> Result {
        let proposal = namespace_change(5, 2);
        assert_eq!(
            proposal_standing("DNS", &proposal, Some(&namespace_change(4, 1)))?,
            Standing::Next
        );
        assert_eq!(
            proposal_standing("DNS", &namespace_change(1, 2), None)?,
            Standing::Next
        );
        Ok(())
    }

    /// A resumed submit finds what it published before the restart. That is
    /// not a moved serial, and it must not be sent again.
    #[test]
    fn a_proposal_that_is_the_head_is_in_force() -> Result {
        let proposal = namespace_change(5, 2);
        assert_eq!(
            proposal_standing("DNS", &proposal, Some(&proposal.clone()))?,
            Standing::InForce
        );
        Ok(())
    }

    /// The head moved past the serial the proposal was built for. Publishing
    /// it would replace a change nobody signed against.
    #[test]
    fn a_proposal_behind_the_head_is_refused() -> Result {
        let error = standing_error(&namespace_change(5, 2), Some(&namespace_change(6, 3)))?;
        assert!(
            error.contains("at serial 5") && error.contains("holds serial 6"),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn another_change_at_the_same_serial_is_refused() -> Result {
        let error = standing_error(&namespace_change(5, 2), Some(&namespace_change(5, 3)))?;
        assert!(
            error.contains("Another DNS change already holds serial 5"),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn a_proposal_that_skips_a_serial_is_refused() -> Result {
        let error = standing_error(&namespace_change(5, 2), Some(&namespace_change(3, 1)))?;
        assert!(error.contains("holds serial 3"), "{error}");
        standing_error(&namespace_change(2, 2), None)?;
        Ok(())
    }
}
