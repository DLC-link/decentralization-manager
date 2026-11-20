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

use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::Signature,
    protocol::v30::{SignedTopologyTransaction, enums::TopologyChangeOp},
    topology::admin::v30::{
        AddTransactionsRequest, GenerateTransactionsRequest, StoreId,
        generate_transactions_request,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};

use crate::{config::NodeConfig, workflow::topology};

use super::add_hosts::{AddHostsError, CurrentPartyTopology, read_party_to_participant};

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
