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
        DecentralizedNamespaceDefinition, PartyToKeyMapping, PartyToParticipant,
        SignedTopologyTransaction, TopologyTransaction, enums, topology_mapping,
    },
    topology::admin::v30::{
        AddTransactionsRequest, AuthorizeRequest, AuthorizeResponse, BaseQuery, ForceFlag,
        ListAllRequest, ListDecentralizedNamespaceDefinitionRequest, ListPartyToKeyMappingRequest,
        ListPartyToParticipantRequest, SignTransactionsRequest, SignTransactionsResponse, StoreId,
        Synchronizer, base_query,
        list_party_to_key_mapping_response::result::Item as PartyToKeyItem,
        list_party_to_participant_response::result::Item as P2pItem, store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    consts::{
        topology_propagation_delay_secs, topology_retry_delay_secs, topology_retry_max_attempts,
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
    head_state_query_in(synchronizer_store_id(synchronizer_id))
}

/// A head-state [`BaseQuery`] against any store, such as the temporary store
/// a proposal is signed in.
pub fn head_state_query_in(store: StoreId) -> BaseQuery {
    BaseQuery {
        store: Some(store),
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
///
/// `force_changes` carries the [`ForceFlag`]s the transaction needs to pass
/// Canton's validation. A party proposal is signed in a temporary store (see
/// [`super::proposal_store`]), so this request is the first time the
/// synchronizer store validates it.
pub fn add_transactions_request(
    synchronizer_id: &str,
    transaction: SignedTopologyTransaction,
    force_changes: Vec<i32>,
) -> AddTransactionsRequest {
    AddTransactionsRequest {
        transactions: vec![transaction],
        force_changes,
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
    fetch_p2p_mapping_at_head(config, synchronizer_id, party_id)
        .await
        .map(|(_, mapping)| mapping)
}

/// [`fetch_p2p_mapping`] together with the serial of the head transaction.
///
/// # Errors
///
/// Errors if the party has no mapping or the serial is out of range.
pub async fn fetch_p2p_mapping_at_head(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &CantonId,
) -> Result<(u32, PartyToParticipant)> {
    fetch_p2p_mapping_at_head_in(config, synchronizer_store_id(synchronizer_id), party_id).await
}

/// [`fetch_p2p_mapping_at_head`] against any store.
///
/// # Errors
///
/// Errors if the party has no mapping in that store or the serial is out of
/// range.
pub async fn fetch_p2p_mapping_at_head_in(
    config: &NodeConfig,
    store: StoreId,
    party_id: &CantonId,
) -> Result<(u32, PartyToParticipant)> {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::new(config.admin_channel().await?);

    let request = tonic::Request::new(ListPartyToParticipantRequest {
        base_query: Some(head_state_query_in(store)),
        filter_party: party_id.to_string(),
        filter_participant: String::new(),
    });

    let response = topology_read_client
        .list_party_to_participant(request)
        .await?
        .into_inner();

    // Same pinning as `fetch_party_to_key_mapping`: `filter_party` matches on
    // a prefix, and the head state can hold a `Remove`.
    let mut without_context = 0usize;
    let mapping = response.results.into_iter().find_map(|r| {
        let Some(context) = r.context else {
            without_context += 1;
            return None;
        };
        if context.operation != enums::TopologyChangeOp::AddReplace as i32 {
            return None;
        }
        let P2pItem::V30(mapping) = r.item?;
        (mapping.party == party_id.to_string()).then_some((context.serial, mapping))
    });

    match mapping {
        Some((serial, mapping)) => Ok((u32::try_from(serial)?, mapping)),
        // Saying "no mapping" for a malformed response sends the reader after
        // the wrong problem entirely.
        None if without_context > 0 => anyhow::bail!(
            "Canton returned {without_context} P2P row(s) with no context for {party_id}, \
             so none could be checked for being an add-or-replace of this party"
        ),
        None => anyhow::bail!("No P2P mapping found for party {party_id}"),
    }
}

/// Fetch the deprecated `PartyToKeyMapping` for a party from the
/// synchronizer head state, or `None` when the party has none.
///
/// A party onboarded before Canton 3.4 keeps its protocol signing keys in
/// this separate mapping instead of inline on its `PartyToParticipant`.
/// Canton 3.5 deprecates the mapping but still serves it.
///
/// # Errors
///
/// Errors when the admin API call fails.
pub async fn fetch_party_to_key_mapping(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &CantonId,
) -> Result<Option<PartyToKeyMapping>> {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::new(config.admin_channel().await?);

    let response = topology_read_client
        .list_party_to_key_mapping(tonic::Request::new(ListPartyToKeyMappingRequest {
            base_query: Some(head_state_query(synchronizer_id)),
            filter_party: party_id.to_string(),
        }))
        .await?
        .into_inner();

    // `filter_party` is a prefix filter and a head-state result can carry a
    // `Remove`, so neither is taken on trust: these keys become the party's
    // entire signing authority once a caller moves them inline.
    Ok(response.results.into_iter().find_map(|r| {
        if r.context?.operation != enums::TopologyChangeOp::AddReplace as i32 {
            return None;
        }
        let PartyToKeyItem::V30(mapping) = r.item?;
        (mapping.party == party_id.to_string()).then_some(mapping)
    }))
}

/// Contribute this member's namespace signature to retiring the legacy mapping.
/// Called only after the replacement topology has become effective. Every
/// member reads the same live mapping and signs the same explicit next serial;
/// Canton merges their signatures under the current namespace threshold.
pub async fn retire_legacy_keys(
    config: &NodeConfig,
    synchronizer_id: &str,
    party: &CantonId,
) -> Result {
    let mut reader = TopologyManagerReadServiceClient::new(config.admin_channel().await?);
    let response = reader
        .list_party_to_key_mapping(ListPartyToKeyMappingRequest {
            base_query: Some(head_state_query(synchronizer_id)),
            filter_party: party.to_string(),
        })
        .await?
        .into_inner();
    let Some((serial, mapping)) = response.results.into_iter().find_map(|row| {
        let context = row.context?;
        let PartyToKeyItem::V30(mapping) = row.item?;
        (context.operation == enums::TopologyChangeOp::AddReplace as i32
            && mapping.party == party.to_string())
        .then_some((context.serial, mapping))
    }) else {
        return Ok(());
    };
    let p2p = fetch_p2p_mapping(config, synchronizer_id, party).await?;
    validate_legacy_retirement(&p2p, &mapping)?;
    let request = legacy_retirement_request(synchronizer_id, serial, mapping)?;
    if let Err(error) =
        authorize_with_topology_retry(config, request, "retire legacy party keys").await
    {
        // Another member may have completed this exact removal while we signed.
        if fetch_party_to_key_mapping(config, synchronizer_id, party)
            .await?
            .is_some()
        {
            return Err(error);
        }
    }
    Ok(())
}

#[allow(deprecated)] // Removing the deprecated mapping requires its wire variant.
fn legacy_retirement_request(
    synchronizer_id: &str,
    serial: i32,
    mapping: PartyToKeyMapping,
) -> Result<AuthorizeRequest> {
    use canton_proto_rs::com::digitalasset::canton::{
        protocol::v30::TopologyMapping, topology::admin::v30::authorize_request,
    };
    anyhow::ensure!(serial > 0, "Legacy mapping has an invalid serial");
    Ok(AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            authorize_request::Proposal {
                change: enums::TopologyChangeOp::Remove as i32,
                serial: serial
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("Legacy mapping serial overflow"))?
                    .try_into()?,
                mapping: Some(authorize_request::proposal::Mapping::V30(TopologyMapping {
                    mapping: Some(topology_mapping::Mapping::PartyToKeyMapping(mapping)),
                })),
            },
        )),
        store: Some(synchronizer_store_id(synchronizer_id)),
        must_fully_authorize: false,
        ..Default::default()
    })
}

fn validate_legacy_retirement(p2p: &PartyToParticipant, legacy: &PartyToKeyMapping) -> Result {
    anyhow::ensure!(
        p2p.party == legacy.party,
        "Legacy mapping belongs to another party"
    );
    let keys = p2p.party_signing_keys.as_ref().ok_or_else(|| {
        anyhow::anyhow!("Cannot retire legacy keys before inline keys are effective")
    })?;
    let distinct: HashSet<_> = keys.keys.iter().map(utils::compute_fingerprint).collect();
    anyhow::ensure!(
        keys.threshold > 0 && keys.threshold as usize <= distinct.len(),
        "Cannot retire legacy keys without a usable inline signing threshold"
    );
    anyhow::ensure!(
        distinct.len() == keys.keys.len(),
        "Inline signing keys contain duplicates"
    );
    Ok(())
}

/// Coordinator side of the retirement round. Peers contribute while this
/// waits; completing the workflow requires observing the removal on-chain.
pub async fn retire_legacy_keys_and_wait(config: &NodeConfig, party: &CantonId) -> Result {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    retire_legacy_keys(config, &synchronizer_id, party).await?;
    for _ in 0..topology_retry_max_attempts() {
        if fetch_party_to_key_mapping(config, &synchronizer_id, party)
            .await?
            .is_none()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(topology_retry_delay_secs())).await;
    }
    anyhow::bail!(
        "Legacy PartyToKeyMapping removal for {party} did not reach the namespace threshold; retry with enough upgraded members online"
    )
}

/// Check that every party signing key a `PartyToParticipant` transaction adds
/// is among the transaction's signers.
///
/// Canton requires a newly added signing key to authorize the transaction
/// that adds it (`topology.proto`: "adding a signing key: party namespace +
/// all the new signing key"). A party that already carries its keys inline
/// adds exactly one — the joining member's, whose node signs in the same
/// round — but a party whose keys still sit in a legacy `PartyToKeyMapping`
/// adds every member's key at once. A member whose node can no longer produce
/// that signature would leave the namespace change applied and the
/// participant change rejected, so the run stops before submitting rather
/// than half-way through.
///
/// # Errors
///
/// Errors when the transaction will not decode, when it carries no
/// `PartyToParticipant`, or when a key it adds has not signed it.
pub async fn check_added_signing_keys_signed(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &CantonId,
    transaction: &SignedTopologyTransaction,
) -> Result {
    let topology_transaction: TopologyTransaction =
        utils::decode_versioned(&transaction.transaction)?;
    let Some(topology_mapping::Mapping::PartyToParticipant(proposed)) =
        topology_transaction.mapping.and_then(|m| m.mapping)
    else {
        anyhow::bail!("P2P transaction does not carry a PartyToParticipant mapping");
    };

    let current: HashSet<String> = fetch_p2p_mapping(config, synchronizer_id, party_id)
        .await?
        .party_signing_keys
        .map(|k| k.keys)
        .unwrap_or_default()
        .iter()
        .map(utils::compute_fingerprint)
        .collect();
    // A signature can arrive in either field: `topology.proto` requires one of
    // the two, and Canton merges both when it parses the transaction. Reading
    // only `signatures` would name a key that did in fact sign.
    let signers: HashSet<&str> = transaction
        .signatures
        .iter()
        .chain(
            transaction
                .multi_transaction_signatures
                .iter()
                .flat_map(|multi| multi.signatures.iter()),
        )
        .map(|s| s.signed_by.as_str())
        .collect();

    let missing: Vec<String> = proposed
        .party_signing_keys
        .map(|k| k.keys)
        .unwrap_or_default()
        .iter()
        .map(utils::compute_fingerprint)
        .filter(|f| !current.contains(f) && !signers.contains(f.as_str()))
        .collect();

    if !missing.is_empty() {
        anyhow::bail!(
            "The P2P proposal adds {count} party signing key(s) that did not sign it: \
             {missing}. Canton refuses a new signing key that does not authorize its own \
             addition, and the namespace change is submitted first, so this would leave the \
             party half-migrated",
            count = missing.len(),
            missing = missing.join(", ")
        );
    }

    Ok(())
}

/// The signed transaction currently in force for the decentralized namespace,
/// as the synchronizer store holds it.
///
/// Re-hosting a former member changes nothing about the namespace, but the
/// peers' signing round takes a DNS transaction alongside the P2P one. Handing
/// them the transaction already in force keeps that round unchanged; their
/// signatures on it are never submitted.
///
/// # Errors
/// Returns an error if the store holds no such transaction.
pub async fn fetch_signed_namespace_definition(
    config: &NodeConfig,
    synchronizer_id: &str,
    namespace_hex: &str,
) -> Result<SignedTopologyTransaction> {
    signed_head_for(config, synchronizer_id, namespace_hex, |mapping| {
        matches!(
            mapping,
            topology_mapping::Mapping::DecentralizedNamespaceDefinition(def)
                if def.decentralized_namespace == namespace_hex
        )
    })
    .await?
    .map(|(signed, _)| signed)
    .ok_or_else(|| {
        anyhow::anyhow!(
            "No DecentralizedNamespaceDefinition transaction for {namespace_hex} in the \
             synchronizer head state"
        )
    })
}

/// The first add-or-replace transaction in the synchronizer head state under
/// `namespace_hex` whose mapping `wanted` accepts, signed and decoded.
///
/// The typed `List*` reads return the mapping without its signatures or its
/// transaction, and a caller that compares transactions or hands one to the
/// peers needs both.
async fn signed_head_for(
    config: &NodeConfig,
    synchronizer_id: &str,
    namespace_hex: &str,
    wanted: impl Fn(&topology_mapping::Mapping) -> bool,
) -> Result<Option<(SignedTopologyTransaction, TopologyTransaction)>> {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::new(config.admin_channel().await?);
    // `ListAll` is the variant Canton 3.5 serves; its successor is 3.6-only.
    #[allow(deprecated)]
    let response = topology_read_client
        .list_all(tonic::Request::new(ListAllRequest {
            base_query: Some(head_state_query(synchronizer_id)),
            exclude_mappings: vec![],
            filter_namespace: namespace_hex.to_string(),
        }))
        .await?
        .into_inner();
    for item in response.result.map(|r| r.items).unwrap_or_default() {
        let signed: SignedTopologyTransaction = utils::decode_versioned(&item.transaction)?;
        let transaction: TopologyTransaction = utils::decode_versioned(&signed.transaction)?;
        if transaction.operation != enums::TopologyChangeOp::AddReplace as i32 {
            continue;
        }
        if transaction
            .mapping
            .as_ref()
            .and_then(|m| m.mapping.as_ref())
            .is_some_and(&wanted)
        {
            return Ok(Some((signed, transaction)));
        }
    }
    Ok(None)
}

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
    let transaction: TopologyTransaction = utils::decode_versioned(&proposal.transaction)?;
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

/// Fetch the current `DecentralizedNamespaceDefinition` from the synchronizer
/// head state together with the serial of its transaction.
///
/// # Errors
///
/// Errors if the namespace is not present as an add-or-replace, or the serial
/// is out of range.
pub async fn fetch_namespace_definition_at_head(
    config: &NodeConfig,
    synchronizer_id: &str,
    namespace_hex: &str,
) -> Result<(u32, DecentralizedNamespaceDefinition)> {
    fetch_namespace_definition_at_head_in(
        config,
        synchronizer_store_id(synchronizer_id),
        namespace_hex,
    )
    .await
}

/// [`fetch_namespace_definition_at_head`] against any store.
///
/// # Errors
///
/// Errors if the namespace has no add-or-replace definition in that store, or
/// the serial is out of range.
pub async fn fetch_namespace_definition_at_head_in(
    config: &NodeConfig,
    store: StoreId,
    namespace_hex: &str,
) -> Result<(u32, DecentralizedNamespaceDefinition)> {
    let mut topology_read_client =
        TopologyManagerReadServiceClient::new(config.admin_channel().await?);

    let request = tonic::Request::new(ListDecentralizedNamespaceDefinitionRequest {
        base_query: Some(head_state_query_in(store)),
        filter_namespace: namespace_hex.to_string(),
    });

    let response = topology_read_client
        .list_decentralized_namespace_definition(request)
        .await?
        .into_inner();

    let (serial, definition) = response
        .results
        .into_iter()
        .find_map(|r| {
            let context = r.context?;
            if context.operation != enums::TopologyChangeOp::AddReplace as i32 {
                return None;
            }
            let definition = r.item?;
            (definition.decentralized_namespace == namespace_hex)
                .then_some((context.serial, definition))
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Namespace {namespace_hex} has no add-or-replace definition in the head state"
            )
        })?;
    Ok((u32::try_from(serial)?, definition))
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
    use super::*;

    #[test]
    #[allow(deprecated)]
    fn legacy_retirement_removes_the_exact_mapping_at_the_next_serial() -> Result {
        use canton_proto_rs::com::digitalasset::canton::topology::admin::v30::authorize_request;
        let mapping = PartyToKeyMapping {
            party: "party".into(),
            ..Default::default()
        };
        let request = legacy_retirement_request("sync", 4, mapping.clone())?;
        assert!(!request.must_fully_authorize);
        assert!(request.signed_by.is_empty());
        assert!(request.force_changes.is_empty());
        assert_eq!(request.store, Some(synchronizer_store_id("sync")));
        let Some(authorize_request::Type::Proposal(proposal)) = request.r#type else {
            anyhow::bail!("expected removal proposal");
        };
        assert_eq!(proposal.serial, 5);
        assert_eq!(proposal.change, enums::TopologyChangeOp::Remove as i32);
        let Some(authorize_request::proposal::Mapping::V30(topology)) = proposal.mapping else {
            anyhow::bail!("expected topology mapping");
        };
        assert_eq!(
            topology.mapping,
            Some(topology_mapping::Mapping::PartyToKeyMapping(
                mapping.clone()
            ))
        );
        for serial in [-1, 0, i32::MAX] {
            assert!(legacy_retirement_request("sync", serial, mapping.clone()).is_err());
        }
        Ok(())
    }

    #[test]
    fn legacy_retirement_requires_usable_inline_keys() -> Result {
        use canton_proto_rs::com::digitalasset::canton::crypto::v30::{
            SigningKeysWithThreshold, SigningPublicKey,
        };
        let legacy = PartyToKeyMapping {
            party: "party".into(),
            ..Default::default()
        };
        let mut p2p = PartyToParticipant {
            party: legacy.party.clone(),
            ..Default::default()
        };
        assert!(validate_legacy_retirement(&p2p, &legacy).is_err());
        let key = SigningPublicKey {
            public_key: vec![1; 32],
            ..Default::default()
        };
        p2p.party_signing_keys = Some(SigningKeysWithThreshold {
            keys: vec![key.clone()],
            threshold: 1,
        });
        validate_legacy_retirement(&p2p, &legacy)?;
        for (keys, threshold) in [
            (vec![key.clone()], 0),
            (vec![key.clone()], 2),
            (vec![key.clone(), key], 1),
        ] {
            p2p.party_signing_keys = Some(SigningKeysWithThreshold { keys, threshold });
            assert!(validate_legacy_retirement(&p2p, &legacy).is_err());
        }
        p2p.party = "another-party".into();
        assert!(validate_legacy_retirement(&p2p, &legacy).is_err());
        Ok(())
    }

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
