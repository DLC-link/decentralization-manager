use canton_proto_rs::com::digitalasset::canton::protocol::v30::topology_mapping;
use common::api::HostPermission;
use sqlx::SqlitePool;

use super::{
    error::AddHostsError, mapping::add_hosts_mapping, replication::capture_replication_offsets,
};
use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    utils,
    workflow::{
        external_party::topology_bundle::{generate_p2p_change, read_pinned},
        topology_version::ProposalFormat,
    },
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
) -> Result<PreparedAddHosts, AddHostsError> {
    let synchronizer_id = utils::get_synchronizer_id(config)
        .await
        .map_err(AddHostsError::Canton)?;

    let current = read_pinned(config, party_id, base_serial).await?;

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
    let generated = generate_p2p_change(
        config,
        party_id,
        current.serial,
        &synchronizer_id,
        ProposalFormat::for_synchronizer(&synchronizer_id)
            .generated_mapping(topology_mapping::Mapping::PartyToParticipant(mapping)),
    )
    .await?;
    let next_serial = generated.serial;

    tracing::info!(
        %party_id,
        base_serial,
        next_serial,
        new_hosts = new_hosts.len(),
        "external-party: generated add-hosts topology"
    );

    Ok(PreparedAddHosts {
        party_id: party_id.to_string(),
        serial: next_serial,
        transaction_hashes: generated.transaction_hashes,
        topology_transactions: generated.topology_transactions,
    })
}
