//! Cross-workflow Canton topology helpers.
//!
//! Workflows (onboarding, kick, …) that submit signed topology transactions
//! to Canton share the same write path
//! ([`TopologyManagerWriteServiceClient::sign_transactions`]) and the same
//! transient failure mode while a freshly-restarted participant's local
//! topology store is reconciling. This module owns the retry policy so
//! callers don't reach across workflow boundaries to share it.

use std::time::Duration;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    consts::{topology_retry_delay_secs, topology_retry_max_attempts},
    error::Result,
};
use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToParticipant, SignedTopologyTransaction,
    },
    topology::admin::v30::{
        AddTransactionsRequest, AuthorizeRequest, AuthorizeResponse, BaseQuery,
        ListDecentralizedNamespaceDefinitionRequest, ListPartyToParticipantRequest,
        SignTransactionsRequest, SignTransactionsResponse, StoreId, Synchronizer, base_query,
        list_party_to_participant_response::result::Item as P2pItem, store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
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
        client_version: None,
    }
}

/// A [`BaseQuery`] over the synchronizer store's whole history, so a caller
/// can see superseded serials rather than only what is currently in force.
///
/// `until: None` means "up to now"; `from: None` means "from the beginning".
pub fn history_query(synchronizer_id: &str) -> BaseQuery {
    BaseQuery {
        store: Some(synchronizer_store_id(synchronizer_id)),
        proposals: false,
        operation: 0,
        time_query: Some(base_query::TimeQuery::Range(base_query::TimeRange {
            from: None,
            until: None,
        })),
        filter_signed_key: String::new(),
        protocol_version: None,
        client_version: None,
    }
}

/// Every `PartyToParticipant` serial ever in force for `party_id`, paired with
/// the time each became effective.
///
/// The head state answers "who hosts this party now"; this answers "when did
/// that become true", which is what a replication needs when it has to find an
/// offset from before a participant was activated.
///
/// # Errors
/// Returns an error if the topology read fails.
pub async fn fetch_p2p_history(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &CantonId,
) -> Result<Vec<(prost_types::Timestamp, PartyToParticipant)>> {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::new(config.admin_channel().await?);

    let request = tonic::Request::new(ListPartyToParticipantRequest {
        base_query: Some(history_query(synchronizer_id)),
        filter_party: party_id.to_string(),
        filter_participant: String::new(),
    });

    let response = topology_read_client
        .list_party_to_participant(request)
        .await?
        .into_inner();

    let mut history: Vec<(i32, prost_types::Timestamp, PartyToParticipant)> = response
        .results
        .into_iter()
        .filter_map(|r| {
            let context = r.context?;
            let valid_from = context.valid_from?;
            let P2pItem::V30(mapping) = r.item?;
            Some((context.serial, valid_from, mapping))
        })
        .collect();

    // Canton does not promise an order, and the caller wants the EARLIEST
    // serial that matches, so sort rather than trusting the response.
    history.sort_by_key(|(serial, _, _)| *serial);
    Ok(history
        .into_iter()
        .map(|(_, valid_from, mapping)| (valid_from, mapping))
        .collect())
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
        .and_then(|r| r.item.as_ref().map(|P2pItem::V30(mapping)| mapping))
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
}
