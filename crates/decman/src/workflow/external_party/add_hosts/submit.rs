use canton_proto_rs::com::digitalasset::canton::protocol::v30::{
    PartyToParticipant, TopologyMapping, topology_mapping,
};

use crate::{
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{
        external_party::{
            keys::PartySignatureScheme,
            topology_bundle::{party_signed, read_current, submit_signed},
        },
        topology,
        topology_version::{self},
    },
};

use super::{
    error::AddHostsError,
    head::CurrentPartyTopology,
    relay,
    validate::{ExternalPartyAddHostsPayload, validate_add_hosts_topology},
};

/// Whether head state is already exactly what `bundle` asked for.
///
/// Used only to recognise a retry of a submission that landed. It compares the
/// authorized mapping against the one inside the submitted transaction rather
/// than assuming an advanced serial means *this* bundle caused it.
fn submitted_mapping_matches(
    bundle: &ExternalPartyAddHostsPayload,
    authorized: &PartyToParticipant,
) -> bool {
    bundle.topology_transactions.iter().any(|serialized| {
        let Ok(transaction) = topology_version::decode_transaction(serialized) else {
            return false;
        };
        matches!(
            transaction.mapping,
            Some(TopologyMapping {
                mapping: Some(topology_mapping::Mapping::PartyToParticipant(ref p2p)),
            }) if p2p == authorized
        )
    })
}

pub(super) fn require_live_source(
    config: &NodeConfig,
    current: &CurrentPartyTopology,
) -> Result<()> {
    let self_id = config.participant_id().to_string();
    anyhow::ensure!(
        current
            .mapping
            .participants
            .iter()
            .any(|host| host.participant_uid == self_id && host.onboarding.is_none()),
        "only a current live host may publish add-hosts topology; a joiner must stage its \
         signature while disconnected and relay it through a current host"
    );
    Ok(())
}

/// Publish the wallet's partial authorization from a current live source host.
///
/// This step contributes no joining signature. Each joiner reads the proposal
/// before it disconnects and stages its signature for publication by the source.
///
/// The source validates the topology against its own head-state read before
/// publishing it. Canton verifies the wallet's signature.
///
/// # Errors
/// [`AddHostsError::UnknownParty`] when this node holds no mapping,
/// [`AddHostsError::StaleSerial`] when the pin has moved,
/// [`AddHostsError::Invalid`] when the bundle fails validation, and
/// [`AddHostsError::Canton`] when an RPC fails — so the caller can answer each
/// differently.
pub async fn submit_add_hosts(
    config: &NodeConfig,
    bundle: &ExternalPartyAddHostsPayload,
) -> std::result::Result<u32, AddHostsError> {
    let current = read_current(config, &bundle.party_id).await?;
    require_live_source(config, &current).map_err(AddHostsError::Invalid)?;
    // Split out from the rest of validation: a pin that has moved is a race the
    // caller resolves by re-reading, not malformed input.
    if bundle.base_serial != current.serial {
        // A retry whose first attempt actually landed must not look like a
        // conflict. Canton treats a re-submitted identical transaction as a
        // no-op, but this host reads the advanced serial first and would answer
        // 409 before Canton ever saw it — so a wallet that lost the response to
        // a successful call could never learn it succeeded.
        //
        // Only when head state is exactly one past the pin AND carries what this
        // bundle asked for. Anything else is a genuine stale pin: another write
        // could have moved the party, and calling that success would tell the
        // wallet its change is live when it is not.
        if current.serial == bundle.base_serial.saturating_add(1)
            && submitted_mapping_matches(bundle, &current.mapping)
        {
            tracing::info!(
                party_id = %bundle.party_id,
                serial = current.serial,
                "add-hosts already applied; treating the retry as success"
            );
            return Ok(bundle.base_serial);
        }
        return Err(AddHostsError::StaleSerial {
            party: bundle.party_id.clone(),
            pinned: bundle.base_serial,
            found: current.serial,
        });
    }

    // Before this node's key goes anywhere near these bytes.
    validate_add_hosts_topology(config, &current, bundle).map_err(AddHostsError::Invalid)?;

    let synchronizer_id = utils::get_synchronizer_id(config)
        .await
        .map_err(AddHostsError::Canton)?;
    let store = topology::synchronizer_store_id(&synchronizer_id);

    let scheme = PartySignatureScheme::for_party(&current.mapping, &bundle.signed_by);
    let signed = party_signed(
        &bundle.topology_transactions,
        &bundle.signatures,
        true,
        |signature| scheme.canton_signature(signature, &bundle.signed_by),
    );

    relay::ensure_connected(config).await?;
    submit_signed(config, signed, vec![], store, "external-party add-hosts").await?;

    tracing::info!(
        party_id = %bundle.party_id,
        base_serial = current.serial,
        "external-party: add-hosts topology submitted on this host"
    );

    Ok(current.serial)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::external_party::add_hosts::test_support::*;

    #[test]
    fn only_a_live_existing_host_can_publish_topology() {
        assert!(require_live_source(&test_config(1), &current()).is_ok());
        assert!(require_live_source(&test_config(3), &current()).is_err());
        let proposed = CurrentPartyTopology {
            serial: 5,
            mapping: built(),
        };
        assert!(require_live_source(&test_config(3), &proposed).is_err());
    }
}
