//! Topology reads and writes shared by the three changes that move an existing
//! party to its next serial: adding hosts ([`super::add_hosts`]), changing the
//! threshold ([`super::threshold`]) and adopting a key for a local party
//! ([`super::local_party`]).
//!
//! Each change prepares the same way: read the party's mapping, check the
//! wallet's pinned serial, ask Canton to generate the next serial, and hand the
//! hashes to the party to sign. Each submits the same way too: pair every
//! transaction with the party's signature and call `AddTransactions`. What
//! differs is the mapping, the validation, and whether this node co-signs.
//!
//! The validators of those three changes, and the onboarding validator, also
//! share their first checks: the bundle envelope, the signature forms, and the
//! decode of each transaction into its `PartyToParticipant` mapping.

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::Signature,
    protocol::v30::{
        PartyToParticipant, SignedTopologyTransaction, TopologyMapping, enums::TopologyChangeOp,
        topology_mapping,
    },
    topology::admin::v30::{
        AddTransactionsRequest, GenerateTransactionsRequest, StoreId,
        generate_transactions_request,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};

use super::{
    add_hosts::{AddHostsError, CurrentPartyTopology, read_party_to_participant},
    keys::PartySignatureScheme,
};
use crate::{
    config::NodeConfig,
    error::Result,
    workflow::{topology, topology_version},
};

/// The transactions Canton generated for one change, plus the hash the party
/// must sign for each. The two vectors are index-aligned.
pub(super) struct GeneratedChange {
    /// The serial the transactions carry (the current serial plus one).
    pub(super) serial: u32,
    /// Canton's hash for each transaction, for the party to sign.
    pub(super) transaction_hashes: Vec<Vec<u8>>,
    /// The serialized topology transactions to submit.
    pub(super) topology_transactions: Vec<Vec<u8>>,
}

/// Read `party_id`'s authorized mapping from this node's head state.
///
/// # Errors
/// [`AddHostsError::UnknownParty`] when this node holds no authorized mapping,
/// and [`AddHostsError::Canton`] when the read fails.
pub(super) async fn read_current(
    config: &NodeConfig,
    party_id: &str,
) -> Result<CurrentPartyTopology, AddHostsError> {
    let Some(current) = read_party_to_participant(config, party_id)
        .await
        .map_err(AddHostsError::Canton)?
    else {
        return Err(AddHostsError::UnknownParty {
            party: party_id.to_string(),
        });
    };
    Ok(current)
}

/// Read `party_id`'s authorized mapping and check that it still sits at the
/// serial the wallet pinned.
///
/// # Errors
/// The errors of [`read_current`], and [`AddHostsError::StaleSerial`] when
/// head state is not at `base_serial`.
pub(super) async fn read_pinned(
    config: &NodeConfig,
    party_id: &str,
    base_serial: u32,
) -> Result<CurrentPartyTopology, AddHostsError> {
    let current = read_current(config, party_id).await?;
    if current.serial != base_serial {
        return Err(AddHostsError::StaleSerial {
            party: party_id.to_string(),
            pinned: base_serial,
            found: current.serial,
        });
    }
    Ok(current)
}

/// Ask Canton to generate `mapping` as an `ADD_REPLACE` at the serial after
/// `current_serial`, in the synchronizer store.
///
/// # Errors
/// [`AddHostsError::Invalid`] when `current_serial` cannot be advanced, and
/// [`AddHostsError::Canton`] when the RPC fails or returns no transactions.
pub(super) async fn generate_p2p_change(
    config: &NodeConfig,
    party_id: &str,
    current_serial: u32,
    synchronizer_id: &str,
    mapping: generate_transactions_request::proposal::Mapping,
) -> Result<GeneratedChange, AddHostsError> {
    let next_serial = current_serial.checked_add(1).ok_or_else(|| {
        AddHostsError::Invalid(anyhow::anyhow!(
            "{party_id} is at serial {s}, which cannot be advanced",
            s = current_serial
        ))
    })?;

    let mut client = TopologyManagerWriteServiceClient::new(
        config
            .admin_channel()
            .await
            .map_err(AddHostsError::Canton)?,
    );
    let generated = client
        .generate_transactions(tonic::Request::new(GenerateTransactionsRequest {
            proposals: vec![generate_transactions_request::Proposal {
                operation: TopologyChangeOp::AddReplace as i32,
                serial: next_serial,
                mapping: Some(mapping),
                store: Some(topology::synchronizer_store_id(synchronizer_id)),
            }],
            base_request: None,
        }))
        .await
        .map_err(|e| {
            AddHostsError::Canton(anyhow::Error::new(e).context("GenerateTransactions RPC failed"))
        })?
        .into_inner()
        .generated_transactions;

    if generated.is_empty() {
        return Err(AddHostsError::Canton(anyhow::anyhow!(
            "GenerateTransactions returned no transactions for {party_id}"
        )));
    }

    let mut transaction_hashes = Vec::with_capacity(generated.len());
    let mut topology_transactions = Vec::with_capacity(generated.len());
    for tx in generated {
        transaction_hashes.push(tx.transaction_hash);
        topology_transactions.push(tx.serialized_transaction);
    }

    Ok(GeneratedChange {
        serial: next_serial,
        transaction_hashes,
        topology_transactions,
    })
}

/// Pair each transaction with the party's signature over it. `signature`
/// turns one raw signature into Canton's form.
pub(super) fn party_signed(
    transactions: &[Vec<u8>],
    signatures: &[Vec<u8>],
    proposal: bool,
    signature: impl Fn(&Vec<u8>) -> Signature,
) -> Vec<SignedTopologyTransaction> {
    transactions
        .iter()
        .zip(signatures)
        .map(|(transaction, raw)| SignedTopologyTransaction {
            transaction: transaction.clone(),
            signatures: vec![signature(raw)],
            proposal,
            multi_transaction_signatures: vec![],
        })
        .collect()
}

/// Submit `transactions` to `store` with `AddTransactions`. `what` names the
/// change in the error.
///
/// # Errors
/// [`AddHostsError::Canton`] when the admin channel or the RPC fails.
pub(super) async fn submit_signed(
    config: &NodeConfig,
    transactions: Vec<SignedTopologyTransaction>,
    force_changes: Vec<i32>,
    store: StoreId,
    what: &str,
) -> Result<(), AddHostsError> {
    let mut client = TopologyManagerWriteServiceClient::new(
        config
            .admin_channel()
            .await
            .map_err(AddHostsError::Canton)?,
    );
    client
        .add_transactions(tonic::Request::new(AddTransactionsRequest {
            transactions,
            force_changes,
            store: Some(store),
            wait_to_become_effective: None,
        }))
        .await
        .map_err(|e| {
            AddHostsError::Canton(
                anyhow::Error::new(e).context(format!("AddTransactions RPC failed for {what}")),
            )
        })?;
    Ok(())
}

/// The serial a bundle's transactions must carry.
#[derive(Clone, Copy, Debug)]
pub(super) enum BundleSerial {
    /// Onboarding creates the party, so its mapping is at serial 1.
    Onboarding,
    /// A change moves an existing party from `current` to `next`, one past it.
    Next { current: u32, next: u32 },
}

/// Check that a bundle carries at least one transaction. `what` names the
/// change in the error.
///
/// # Errors
/// Returns an error when `transactions` is empty.
pub(super) fn check_has_transactions(what: &str, transactions: &[Vec<u8>]) -> Result<()> {
    if transactions.is_empty() {
        anyhow::bail!("{what} submitted no topology transactions");
    }
    Ok(())
}

/// Check that a bundle carries at least one transaction and exactly one
/// signature per transaction. `what` names the change in the error.
///
/// # Errors
/// Returns an error when `transactions` is empty, or when the two lists differ
/// in length.
pub(super) fn check_bundle_envelope(
    what: &str,
    transactions: &[Vec<u8>],
    signatures: &[Vec<u8>],
) -> Result<()> {
    check_has_transactions(what, transactions)?;
    if signatures.len() != transactions.len() {
        anyhow::bail!(
            "{what} submitted {t} transaction(s) and {s} signature(s); they must be \
             index-aligned",
            t = transactions.len(),
            s = signatures.len()
        );
    }
    Ok(())
}

/// Check that each signature has a form the party's key can produce. The forms
/// depend on the party key's type, read off `mapping`.
///
/// # Errors
/// Returns an error naming the first signature of the wrong form.
pub(super) fn check_signature_forms(
    mapping: &PartyToParticipant,
    signed_by: &str,
    signatures: &[Vec<u8>],
) -> Result<()> {
    let scheme = PartySignatureScheme::for_party(mapping, signed_by);
    for (index, signature) in signatures.iter().enumerate() {
        if !scheme.accepts(signature) {
            anyhow::bail!(
                "signature {index} is {len} byte(s); {expected}",
                len = signature.len(),
                expected = scheme.expected_form()
            );
        }
    }
    Ok(())
}

/// The serial after the one `current` sits at.
///
/// # Errors
/// Returns an error when the serial cannot be advanced.
pub(super) fn serial_after(current: &CurrentPartyTopology) -> Result<u32> {
    current.serial.checked_add(1).with_context(|| {
        format!(
            "{party} is at serial {s}, which cannot be advanced",
            party = current.mapping.party,
            s = current.serial
        )
    })
}

/// Decode transaction `index` of a bundle into its `PartyToParticipant`
/// mapping. The transaction must be an `ADD_REPLACE` at the serial `serial`
/// names. `endpoint` says what the endpoint does, for the error when the
/// transaction carries another mapping.
///
/// # Errors
/// Returns an error naming the first check that fails.
pub(super) fn decode_p2p_change(
    index: usize,
    serialized: &[u8],
    serial: BundleSerial,
    endpoint: &str,
) -> Result<PartyToParticipant> {
    // Two layers: Canton wraps a stable-versioned message in
    // `UntypedVersionedMessage`, whose `data` is the `TopologyTransaction`.
    let transaction = topology_version::decode_transaction(serialized)
        .with_context(|| format!("topology transaction {index} is not a TopologyTransaction"))?;

    if transaction.operation != TopologyChangeOp::AddReplace as i32 {
        anyhow::bail!(
            "topology transaction {index} must be an ADD_REPLACE, got operation {op}",
            op = transaction.operation
        );
    }
    match serial {
        // Onboarding creates a party; it never replaces an existing mapping. A
        // higher serial here would be an attempt to rewrite topology that
        // already exists — including some other party's.
        BundleSerial::Onboarding => {
            if transaction.serial != 1 {
                anyhow::bail!(
                    "topology transaction {index} must have serial 1 for onboarding, got {serial}",
                    serial = transaction.serial
                );
            }
        }
        BundleSerial::Next { current, next } => {
            if transaction.serial != next {
                anyhow::bail!(
                    "topology transaction {index} has serial {serial}, which must be exactly one \
                     past the current {current}",
                    serial = transaction.serial
                );
            }
        }
    }

    let Some(TopologyMapping {
        mapping: Some(topology_mapping::Mapping::PartyToParticipant(p2p)),
    }) = transaction.mapping
    else {
        anyhow::bail!(
            "topology transaction {index} must carry a PartyToParticipant mapping; this endpoint \
             {endpoint} and authorizes nothing else"
        );
    };
    Ok(p2p)
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use canton_proto_rs::com::digitalasset::canton::{
        protocol::v30::{NamespaceDelegation, TopologyTransaction},
        version::v1::{UntypedVersionedMessage, untyped_versioned_message},
    };
    use prost::Message as _;

    use super::*;

    const PARTY: &str = "alice::1220aa";

    fn p2p() -> PartyToParticipant {
        PartyToParticipant {
            party: PARTY.to_string(),
            threshold: 1,
            participants: vec![],
            party_signing_keys: None,
        }
    }

    fn serialize(
        operation: TopologyChangeOp,
        serial: u32,
        mapping: topology_mapping::Mapping,
    ) -> Vec<u8> {
        let transaction = TopologyTransaction {
            operation: operation as i32,
            serial,
            mapping: Some(TopologyMapping {
                mapping: Some(mapping),
            }),
        };
        UntypedVersionedMessage {
            wrapper: Some(untyped_versioned_message::Wrapper::Data(
                transaction.encode_to_vec(),
            )),
            version: 30,
        }
        .encode_to_vec()
    }

    fn change(serial: u32) -> Vec<u8> {
        serialize(
            TopologyChangeOp::AddReplace,
            serial,
            topology_mapping::Mapping::PartyToParticipant(p2p()),
        )
    }

    const NEXT: BundleSerial = BundleSerial::Next {
        current: 4,
        next: 5,
    };

    fn error_text(result: Result<impl Debug>) -> String {
        match result {
            Ok(value) => panic!("expected an error, got {value:?}"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn envelope_rejects_an_empty_bundle_with_the_callers_name() {
        let e = error_text(check_bundle_envelope("external-party add-hosts", &[], &[]));
        assert_eq!(
            e,
            "external-party add-hosts submitted no topology transactions"
        );

        let e = error_text(check_has_transactions("external-party onboarding", &[]));
        assert_eq!(
            e,
            "external-party onboarding submitted no topology transactions"
        );
    }

    #[test]
    fn envelope_rejects_signatures_that_are_not_index_aligned() {
        let e = error_text(check_bundle_envelope(
            "local-party adoption",
            &[vec![1], vec![2]],
            &[vec![3]],
        ));
        assert_eq!(
            e,
            "local-party adoption submitted 2 transaction(s) and 1 signature(s); they must be \
             index-aligned"
        );
    }

    #[test]
    fn envelope_accepts_one_signature_per_transaction() -> Result<()> {
        check_bundle_envelope("external-party threshold change", &[vec![1]], &[vec![2]])
    }

    #[test]
    fn serial_after_refuses_the_last_serial() {
        let current = CurrentPartyTopology {
            serial: u32::MAX,
            mapping: p2p(),
        };
        let e = error_text(serial_after(&current));
        assert_eq!(
            e,
            "alice::1220aa is at serial 4294967295, which cannot be advanced"
        );
    }

    #[test]
    fn decode_returns_the_mapping_of_a_well_formed_change() -> Result<()> {
        assert_eq!(decode_p2p_change(0, &change(5), NEXT, "adds hosts")?, p2p());
        assert_eq!(
            decode_p2p_change(0, &change(1), BundleSerial::Onboarding, "onboards")?,
            p2p()
        );
        Ok(())
    }

    #[test]
    fn decode_rejects_bytes_that_are_not_a_transaction() {
        let e = error_text(decode_p2p_change(2, b"junk", NEXT, "adds hosts"));
        assert_eq!(e, "topology transaction 2 is not a TopologyTransaction");
    }

    #[test]
    fn decode_rejects_an_operation_other_than_add_replace() {
        let bytes = serialize(
            TopologyChangeOp::Remove,
            5,
            topology_mapping::Mapping::PartyToParticipant(p2p()),
        );
        let e = error_text(decode_p2p_change(0, &bytes, NEXT, "adds hosts"));
        assert_eq!(
            e,
            format!(
                "topology transaction 0 must be an ADD_REPLACE, got operation {op}",
                op = TopologyChangeOp::Remove as i32
            )
        );
    }

    #[test]
    fn decode_rejects_a_serial_that_is_not_the_next_one() {
        let e = error_text(decode_p2p_change(1, &change(7), NEXT, "adds hosts"));
        assert_eq!(
            e,
            "topology transaction 1 has serial 7, which must be exactly one past the current 4"
        );
    }

    #[test]
    fn decode_rejects_an_onboarding_serial_other_than_one() {
        let e = error_text(decode_p2p_change(
            0,
            &change(2),
            BundleSerial::Onboarding,
            "onboards external parties",
        ));
        assert_eq!(
            e,
            "topology transaction 0 must have serial 1 for onboarding, got 2"
        );
    }

    #[test]
    fn decode_rejects_another_mapping_with_the_endpoints_purpose() {
        let bytes = serialize(
            TopologyChangeOp::AddReplace,
            5,
            topology_mapping::Mapping::NamespaceDelegation(NamespaceDelegation::default()),
        );
        let e = error_text(decode_p2p_change(0, &bytes, NEXT, "changes a threshold"));
        assert_eq!(
            e,
            "topology transaction 0 must carry a PartyToParticipant mapping; this endpoint \
             changes a threshold and authorizes nothing else"
        );
    }
}
