//! Cross-workflow Canton topology helpers.
//!
//! Workflows (onboarding, kick, …) that submit signed topology transactions
//! to Canton share the same write path
//! ([`TopologyManagerWriteServiceClient::sign_transactions`]) and the same
//! transient failure mode while a freshly-restarted participant's local
//! topology store is reconciling. This module owns the retry policy so
//! callers don't reach across workflow boundaries to share it.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    time::Duration,
};

use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToParticipant, SignedTopologyTransaction,
    },
    topology::admin::v30::{
        AddTransactionsRequest, AuthorizeRequest, AuthorizeResponse, BaseQuery,
        ListDecentralizedNamespaceDefinitionRequest, ListPartyToParticipantRequest,
        SignTransactionsRequest, SignTransactionsResponse, StoreId, Synchronizer, base_query,
        store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    consts::{
        TOPOLOGY_PROPAGATION_DELAY_SECS, topology_retry_delay_secs, topology_retry_max_attempts,
    },
    error::Result,
    utils,
    workflow::storage::WorkflowStorage,
};

/// Call `sign_transactions` on the participant's TopologyManagerWriteService,
/// retrying only when Canton returns
/// `TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE` — the transient that
/// surfaces while a freshly-restarted participant's local topology store is
/// still reconciling its own signing keys.
///
/// All other gRPC errors bubble up immediately. On a healthy synchronizer
/// the first attempt succeeds, so production code paths pay no retry-loop
/// overhead.
///
/// The retry budget is [`topology_retry_max_attempts`] ×
/// [`topology_retry_delay_secs`] (env-configurable via
/// `DECPM_TOPOLOGY_RETRY_MAX_ATTEMPTS` / `DECPM_TOPOLOGY_RETRY_DELAY_SECS`,
/// defaults 30 × 2s = 60s), shared with the post-write topology-propagation
/// polls in `submit.rs::wait_for_dns_in_topology` /
/// `wait_for_p2p_in_topology`.
///
/// `label` is a short tag included in log lines (e.g. `"DNS"`, `"P2P"`,
/// `"kick"`) so operators can distinguish which sign path is retrying.
pub async fn sign_transactions_with_topology_retry(
    config: &NodeConfig,
    request: SignTransactionsRequest,
    label: &str,
) -> Result<SignTransactionsResponse> {
    let mut topology_client = TopologyManagerWriteServiceClient::new(config.admin_channel().await?);

    let max_attempts = topology_retry_max_attempts();
    let retry_delay = Duration::from_secs(topology_retry_delay_secs());

    let mut attempt = 0usize;
    loop {
        attempt += 1;
        match topology_client
            .sign_transactions(tonic::Request::new(request.clone()))
            .await
        {
            Ok(response) => {
                if attempt > 1 {
                    tracing::info!(
                        "{label}: sign_transactions succeeded on attempt {attempt}/{max_attempts}",
                    );
                }
                return Ok(response.into_inner());
            }
            Err(status) if is_topology_signing_key_not_ready(&status) => {
                if attempt >= max_attempts {
                    anyhow::bail!(
                        "{label}: sign_transactions still returning \
                         TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE after \
                         {max_attempts} attempts: {status}",
                    );
                }
                tracing::warn!(
                    "{label}: TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE \
                     on attempt {attempt}/{max_attempts}, retrying in {retry_delay:?}",
                );
                tokio::time::sleep(retry_delay).await;
            }
            Err(status) => return Err(status.into()),
        }
    }
}

/// `authorize` twin of [`sign_transactions_with_topology_retry`]: proposal
/// creation hits the same `TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE`
/// transient while the local topology store reconciles — observed live on
/// the add-party flag-clearing proposal, which is authorized right after the
/// heaviest topology churn in the codebase (owner-set growth + activation +
/// ACS import on the counterparty).
pub async fn authorize_with_topology_retry(
    config: &NodeConfig,
    request: AuthorizeRequest,
    label: &str,
) -> Result<AuthorizeResponse> {
    let mut topology_client = TopologyManagerWriteServiceClient::new(config.admin_channel().await?);

    let max_attempts = topology_retry_max_attempts();
    let retry_delay = Duration::from_secs(topology_retry_delay_secs());

    let mut attempt = 0usize;
    loop {
        attempt += 1;
        match topology_client
            .authorize(tonic::Request::new(request.clone()))
            .await
        {
            Ok(response) => {
                if attempt > 1 {
                    tracing::info!(
                        "{label}: authorize succeeded on attempt {attempt}/{max_attempts}",
                    );
                }
                return Ok(response.into_inner());
            }
            Err(status) if is_topology_signing_key_not_ready(&status) => {
                if attempt >= max_attempts {
                    anyhow::bail!(
                        "{label}: authorize still returning \
                         TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE after \
                         {max_attempts} attempts: {status}",
                    );
                }
                tracing::warn!(
                    "{label}: TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE \
                     on attempt {attempt}/{max_attempts}, retrying in {retry_delay:?}",
                );
                tokio::time::sleep(retry_delay).await;
            }
            Err(status) => return Err(status.into()),
        }
    }
}

/// Returns true iff the gRPC status is Canton's signal that a participant's
/// local topology store doesn't yet have a usable signing key for the
/// transaction it was asked to sign. This is a transient that resolves once
/// Canton finishes reconciling the participant's `OwnerToKeyMapping` /
/// `NamespaceDelegation` — typically within seconds of participant startup
/// (longer on slow/tunneled deployments).
///
/// Matches on the Canton error name in the status message rather than the
/// gRPC code, because Canton surfaces this error as different gRPC codes in
/// different paths — observed as `NOT_FOUND` from `sign_transactions`
/// (devnet run 2026-05-21, four occurrences on P2 with code
/// `'Some requested entity was not found'`), but historically documented
/// as `FAILED_PRECONDITION` elsewhere. The error-name string is the stable
/// semantic identifier; the gRPC code is implementation detail that varies
/// across Canton versions and call paths.
fn is_topology_signing_key_not_ready(status: &tonic::Status) -> bool {
    status
        .message()
        .contains("TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE")
}

// ---------------------------------------------------------------------------
// Shared topology-transaction request builders
// ---------------------------------------------------------------------------

/// A [`StoreId`] targeting the physical synchronizer store — the target every
/// topology read and write in these workflows uses.
pub fn synchronizer_store_id(synchronizer_id: &str) -> StoreId {
    StoreId {
        store: Some(store_id::Store::Synchronizer(Synchronizer {
            kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.to_string())),
        })),
    }
}

/// A head-state [`BaseQuery`] against the synchronizer store — the boilerplate
/// every topology read in these workflows shares.
pub fn head_state_query(synchronizer_id: &str) -> BaseQuery {
    BaseQuery {
        store: Some(synchronizer_store_id(synchronizer_id)),
        proposals: false,
        operation: 0,
        time_query: Some(base_query::TimeQuery::HeadState(())),
        filter_signed_key: String::new(),
        protocol_version: None,
    }
}

/// An [`AddTransactionsRequest`] submitting a single signed transaction to the
/// synchronizer store.
pub fn add_transactions_request(
    synchronizer_id: &str,
    transaction: SignedTopologyTransaction,
) -> AddTransactionsRequest {
    AddTransactionsRequest {
        transactions: vec![transaction],
        force_changes: vec![],
        store: Some(synchronizer_store_id(synchronizer_id)),
        wait_to_become_effective: None,
    }
}

// ---------------------------------------------------------------------------
// Shared head-state topology reads
// ---------------------------------------------------------------------------

/// Fetch the party's current `PartyToParticipant` mapping from the
/// synchronizer head state. Errors if the party has no mapping.
pub async fn fetch_p2p_mapping(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &CantonId,
) -> Result<PartyToParticipant> {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::new(config.admin_channel().await?);

    let request = tonic::Request::new(ListPartyToParticipantRequest {
        base_query: Some(head_state_query(synchronizer_id)),
        filter_party: party_id.to_string(),
        filter_participant: String::new(),
    });

    let response = topology_read_client
        .list_party_to_participant(request)
        .await?
        .into_inner();

    response
        .results
        .first()
        .and_then(|r| r.item.as_ref())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("No P2P mapping found for party {party_id}"))
}

/// Fetch the current `DecentralizedNamespaceDefinition` from the synchronizer
/// head state. Errors if the namespace is not present.
pub async fn fetch_namespace_definition(
    config: &NodeConfig,
    synchronizer_id: &str,
    namespace_hex: &str,
) -> Result<DecentralizedNamespaceDefinition> {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::new(config.admin_channel().await?);

    let request = tonic::Request::new(ListDecentralizedNamespaceDefinitionRequest {
        base_query: Some(head_state_query(synchronizer_id)),
        filter_namespace: namespace_hex.to_string(),
    });

    let response = topology_read_client
        .list_decentralized_namespace_definition(request)
        .await?
        .into_inner();

    response
        .results
        .first()
        .and_then(|r| r.item.as_ref())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Namespace {namespace_hex} not found in topology"))
}

// ---------------------------------------------------------------------------
// Shared DNS + P2P proposal steps (kick / add-party / change-threshold)
// ---------------------------------------------------------------------------

/// Sign the DNS + P2P topology proposal pair for a topology-changing workflow.
///
/// `proposal_data` is the `[dns, p2p]` length-prefixed pair the coordinator
/// sent (each element a `varint(len)||SignedTopologyTransaction` blob). Signs
/// both with the participant's topology keys — retrying the transient
/// `TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE` via
/// [`sign_transactions_with_topology_retry`] — and persists the signed results
/// as `dns_artifact_kind` / `p2p_artifact_kind`, keyed by this node's
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
    dns_artifact_kind: &str,
    p2p_artifact_kind: &str,
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
            dns_artifact_kind,
            Some(&node_id),
            &utils::encode_length_prefixed_message(&response.transactions[0]),
        )
        .await?;
    storage
        .write_artifact(
            instance_name,
            p2p_artifact_kind,
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
/// `label` is a short workflow tag included in the log lines.
pub async fn submit_dns_then_p2p<DnsFut, P2pFut>(
    config: &NodeConfig,
    synchronizer_id: &str,
    label: &str,
    dns_transaction: SignedTopologyTransaction,
    p2p_transaction: SignedTopologyTransaction,
    confirm_dns: impl FnOnce() -> DnsFut,
    confirm_p2p: impl FnOnce() -> P2pFut,
) -> Result
where
    DnsFut: Future<Output = Result>,
    P2pFut: Future<Output = Result>,
{
    let mut topology_write_client =
        TopologyManagerWriteServiceClient::new(config.admin_channel().await?);

    tracing::info!("Submitting DNS {label} proposal...");
    topology_write_client
        .add_transactions(tonic::Request::new(add_transactions_request(
            synchronizer_id,
            dns_transaction,
        )))
        .await?;
    confirm_dns().await?;
    tracing::info!("DNS {label} confirmed in topology");

    tracing::info!("Submitting P2P {label} proposal...");
    topology_write_client
        .add_transactions(tonic::Request::new(add_transactions_request(
            synchronizer_id,
            p2p_transaction,
        )))
        .await?;
    confirm_p2p().await?;
    tracing::info!("P2P {label} confirmed in topology");

    let propagation_delay = Duration::from_secs(TOPOLOGY_PROPAGATION_DELAY_SECS);
    tracing::info!("Waiting {propagation_delay:?} for Canton to propagate topology updates...");
    tokio::time::sleep(propagation_delay).await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real status text Canton returned from `sign_transactions` on devnet
    /// (2026-05-21 IT run). Code is `NOT_FOUND`, not `FAILED_PRECONDITION` —
    /// this is the exact case the original predicate missed.
    #[test]
    fn detects_canton_not_found_form() {
        let status = tonic::Status::not_found(
            "TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE(11,0): \
             Could not find an appropriate signing key to issue the topology transaction",
        );
        assert!(is_topology_signing_key_not_ready(&status));
    }

    /// Canton has historically surfaced the same error via FAILED_PRECONDITION
    /// in other paths. Match this too so future Canton-version changes don't
    /// reintroduce the flake.
    #[test]
    fn detects_failed_precondition_form() {
        let status = tonic::Status::failed_precondition(
            "TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE(9,abc): \
             No appropriate signing key for namespace …",
        );
        assert!(is_topology_signing_key_not_ready(&status));
    }

    #[test]
    fn rejects_other_canton_errors() {
        let status =
            tonic::Status::failed_precondition("SOME_OTHER_TOPOLOGY_ERROR: irrelevant detail");
        assert!(!is_topology_signing_key_not_ready(&status));
    }

    #[test]
    fn rejects_empty_message() {
        let status = tonic::Status::internal("");
        assert!(!is_topology_signing_key_not_ready(&status));
    }

    #[test]
    fn head_state_query_targets_synchronizer_head_state() {
        let query = head_state_query("global::1220abcd::34-0");
        assert!(!query.proposals);
        assert!(matches!(
            query.time_query,
            Some(base_query::TimeQuery::HeadState(()))
        ));
        match query.store.and_then(|s| s.store) {
            Some(store_id::Store::Synchronizer(s)) => assert_eq!(
                s.kind,
                Some(synchronizer::Kind::PhysicalId(
                    "global::1220abcd::34-0".to_string()
                ))
            ),
            other => panic!("expected synchronizer store, got {other:?}"),
        }
    }

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
}
