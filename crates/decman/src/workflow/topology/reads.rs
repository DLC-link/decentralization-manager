use std::collections::HashSet;

use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToKeyMapping, PartyToParticipant,
        SignedTopologyTransaction, TopologyTransaction, enums, topology_mapping,
    },
    topology::admin::v30::{
        ListAllRequest, ListDecentralizedNamespaceDefinitionRequest, ListPartyToKeyMappingRequest,
        ListPartyToParticipantRequest, StoreId,
        list_party_to_key_mapping_response::result::Item as PartyToKeyItem,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};

use super::requests::{
    head_state_query, head_state_query_in, history_query, p2p_rows, p2p_v30, synchronizer_store_id,
};
use crate::{
    canton_id::{CantonId, Fingerprint},
    config::NodeConfig,
    error::Result,
    utils,
    workflow::topology_version,
};

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

    let mut history: Vec<(i32, prost_types::Timestamp, PartyToParticipant)> =
        p2p_rows(response.results)?
            .into_iter()
            .filter_map(|(context, mapping)| {
                let context = context?;
                Some((context.serial, context.valid_from?, mapping))
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

/// Fetch the party's current `PartyToParticipant` mapping from the
/// synchronizer head state. Errors if the party has no mapping.
///
/// # Errors
///
/// Returns an error if the Admin API channel cannot be opened, or if
/// `ListPartyToParticipant` fails. Returns an error if the head state has no
/// add-or-replace mapping for `party_id`. Returns an error if a returned row
/// holds a `v31` mapping with no `v30` form, or if the serial does not fit a
/// `u32`.
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
    let mut mapping = None;
    for r in response.results {
        let Some(context) = r.context else {
            without_context += 1;
            continue;
        };
        if context.operation != enums::TopologyChangeOp::AddReplace as i32 {
            continue;
        }
        let Some(item) = r.item else {
            continue;
        };
        let candidate = p2p_v30(item)?;
        if candidate.party == party_id.to_string() {
            mapping = Some((context.serial, candidate));
            break;
        }
    }

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
        topology_version::decode_transaction(&transaction.transaction)?;
    let Some(topology_mapping::Mapping::PartyToParticipant(proposed)) =
        topology_transaction.mapping.and_then(|m| m.mapping)
    else {
        anyhow::bail!("P2P transaction does not carry a PartyToParticipant mapping");
    };

    let current: HashSet<Fingerprint> = fetch_p2p_mapping(config, synchronizer_id, party_id)
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
        .map(String::from)
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

/// The signed `PartyToParticipant` transaction in force for `party_id`, as
/// the synchronizer store holds it.
///
/// A kick that leaves the hosting mapping as it is still runs the peers'
/// signing round on a DNS and a P2P transaction. Handing them the one already
/// in force keeps that round unchanged; submit does not send it again.
///
/// # Errors
/// Returns an error if the store holds no such transaction.
pub async fn fetch_signed_party_to_participant(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &CantonId,
) -> Result<SignedTopologyTransaction> {
    let party = party_id.to_string();
    signed_head_for(
        config,
        synchronizer_id,
        &party_id.namespace.to_hex(),
        |mapping| {
            matches!(
                mapping,
                topology_mapping::Mapping::PartyToParticipant(p2p) if p2p.party == party
            )
        },
    )
    .await?
    .map(|(signed, _)| signed)
    .ok_or_else(|| {
        anyhow::anyhow!(
            "No PartyToParticipant transaction for {party} in the synchronizer head state"
        )
    })
}

/// The first add-or-replace transaction in the synchronizer head state under
/// `namespace_hex` whose mapping `wanted` accepts, signed and decoded.
///
/// The typed `List*` reads return the mapping without its signatures or its
/// transaction, and a caller that compares transactions or hands one to the
/// peers needs both.
pub(super) async fn signed_head_for(
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
        let transaction: TopologyTransaction =
            topology_version::decode_transaction(&signed.transaction)?;
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
///
/// # Errors
///
/// Returns an error if the Admin API channel cannot be opened, or if
/// `ListDecentralizedNamespaceDefinition` fails. Returns an error if the
/// response is empty or its first result carries no definition.
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
