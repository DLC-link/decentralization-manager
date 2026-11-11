use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{enums::TopologyChangeOp, topology_mapping},
    topology::admin::v30::{
        GenerateTransactionsRequest, generate_transactions_request,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use common::api::HostPermission;

use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    utils,
    workflow::{topology, topology_version::ProposalFormat},
};

use super::{
    error::AddHostsError, head::read_party_to_participant, mapping::add_hosts_mapping,
    replication::capture_replication_offsets,
};

/// The unsigned add-hosts topology, plus the hash the party must sign for each
/// transaction. The two vectors are index-aligned.
///
/// Deliberately not [`super::super::steps::PreparedTopology`]: that carries the party's
/// public-key fingerprint because onboarding derives the party id from a key the
/// wallet just supplied. Here the party already exists and no key is supplied, so
/// there is nothing to report.
pub struct PreparedAddHosts {
    /// The party gaining hosts.
    pub party_id: String,
    /// The serial the submitted transactions carry (`base_serial + 1`).
    pub serial: u32,
    /// Canton's hash for each transaction, for the party to sign.
    pub transaction_hashes: Vec<Vec<u8>>,
    /// The serialized (versioned) topology transactions to submit.
    pub topology_transactions: Vec<Vec<u8>>,
}

/// Ask Canton to build the add-hosts topology and the hash the party must sign.
///
/// `base_serial` is the serial the wallet expects the party to sit at. It is
/// checked against head state rather than trusted: a host whose view has moved on
/// must fail loudly instead of quietly preparing a different transaction from its
/// peers.
///
/// # Errors
/// Returns an error if the party has no authorized mapping on this node, its
/// serial differs from `base_serial`, the host set is invalid, or
/// `GenerateTransactions` fails.
pub async fn prepare_add_hosts(
    config: &NodeConfig,
    storage: &SqlitePool,
    party_id: &str,
    new_hosts: &[CantonId],
    permission: HostPermission,
    base_serial: u32,
    ledger_token: Option<&str>,
) -> std::result::Result<PreparedAddHosts, AddHostsError> {
    let synchronizer_id = utils::get_synchronizer_id(config)
        .await
        .map_err(AddHostsError::Canton)?;

    let Some(current) = read_party_to_participant(config, party_id)
        .await
        .map_err(AddHostsError::Canton)?
    else {
        return Err(AddHostsError::UnknownParty {
            party: party_id.to_string(),
        });
    };
    if current.serial != base_serial {
        return Err(AddHostsError::StaleSerial {
            party: party_id.to_string(),
            pinned: base_serial,
            found: current.serial,
        });
    }

    let mapping = add_hosts_mapping(&current.mapping, new_hosts, permission)
        .map_err(AddHostsError::Invalid)?;

    capture_replication_offsets(
        config,
        storage,
        party_id,
        new_hosts,
        base_serial,
        ledger_token,
    )
    .await?;
    let next_serial = current.serial.checked_add(1).ok_or_else(|| {
        AddHostsError::Invalid(anyhow::anyhow!(
            "{party_id} is at serial {s}, which cannot be advanced",
            s = current.serial
        ))
    })?;

    let mut client = TopologyManagerWriteServiceClient::new(
        config
            .admin_channel()
            .await
            .map_err(AddHostsError::Canton)?,
    );
    let response = client
        .generate_transactions(tonic::Request::new(GenerateTransactionsRequest {
            proposals: vec![generate_transactions_request::Proposal {
                operation: TopologyChangeOp::AddReplace as i32,
                serial: next_serial,
                mapping: Some(
                    ProposalFormat::for_synchronizer(&synchronizer_id)
                        .generated_mapping(topology_mapping::Mapping::PartyToParticipant(mapping)),
                ),
                store: Some(topology::synchronizer_store_id(&synchronizer_id)),
            }],
            base_request: None,
        }))
        .await
        .map_err(|e| {
            AddHostsError::Canton(anyhow::Error::new(e).context("GenerateTransactions RPC failed"))
        })?
        .into_inner();

    if response.generated_transactions.is_empty() {
        return Err(AddHostsError::Canton(anyhow::anyhow!(
            "GenerateTransactions returned no transactions for {party_id}"
        )));
    }

    tracing::info!(
        %party_id,
        base_serial,
        next_serial,
        new_hosts = new_hosts.len(),
        "external-party: generated add-hosts topology"
    );

    let mut transaction_hashes = Vec::with_capacity(response.generated_transactions.len());
    let mut topology_transactions = Vec::with_capacity(response.generated_transactions.len());
    for tx in response.generated_transactions {
        transaction_hashes.push(tx.transaction_hash);
        topology_transactions.push(tx.serialized_transaction);
    }

    Ok(PreparedAddHosts {
        party_id: party_id.to_string(),
        serial: next_serial,
        transaction_hashes,
        topology_transactions,
    })
}
