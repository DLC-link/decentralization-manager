use canton_proto_rs::com::digitalasset::canton::protocol::v30::{
    DecentralizedNamespaceDefinition, PartyToParticipant, SignedTopologyTransaction,
    TopologyTransaction, enums, topology_mapping,
};

use crate::{error::Result, utils, workflow::topology_version};

/// Unwrap a coordinator-supplied `varint(len)||SignedTopologyTransaction`
/// blob down to the topology mapping it carries.
///
/// Only an add-or-replace is ever signed. A `Remove` carrying the right party
/// and the right members would pass every other check while tearing the
/// mapping down, so the operation is pinned here, before anything else looks
/// at the mapping.
pub(super) fn decode_topology_mapping(payload: &[u8]) -> Result<topology_mapping::Mapping> {
    decode_add_replace(payload)?
        .mapping
        .and_then(|mapping| mapping.mapping)
        .ok_or_else(|| anyhow::anyhow!("topology transaction carries no mapping"))
}

pub(super) fn decode_proposal_serial(payload: &[u8]) -> Result<u32> {
    Ok(decode_add_replace(payload)?.serial)
}

pub(super) fn decode_add_replace(payload: &[u8]) -> Result<TopologyTransaction> {
    let signed: SignedTopologyTransaction = utils::read_first_message_from_bytes(payload)?;
    let transaction = topology_version::decode_transaction(&signed.transaction)?;

    if transaction.operation != enums::TopologyChangeOp::AddReplace as i32 {
        anyhow::bail!(
            "topology transaction is a {operation:?} operation, not an add-or-replace",
            operation = enums::TopologyChangeOp::try_from(transaction.operation)
                .map(|op| op.as_str_name().to_string())
                .unwrap_or_else(|_| format!("unknown ({})", transaction.operation))
        );
    }

    Ok(transaction)
}

pub(super) fn decode_namespace_definition(
    payload: &[u8],
) -> Result<DecentralizedNamespaceDefinition> {
    match decode_topology_mapping(payload)? {
        topology_mapping::Mapping::DecentralizedNamespaceDefinition(def) => Ok(def),
        other => anyhow::bail!(
            "expected a DecentralizedNamespaceDefinition, got {kind}",
            kind = mapping_name(&other)
        ),
    }
}

pub(super) fn decode_party_to_participant(payload: &[u8]) -> Result<PartyToParticipant> {
    match decode_topology_mapping(payload)? {
        topology_mapping::Mapping::PartyToParticipant(mapping) => Ok(mapping),
        other => anyhow::bail!(
            "expected a PartyToParticipant mapping, got {kind}",
            kind = mapping_name(&other)
        ),
    }
}

/// A short name for a mapping the peer refuses to sign, so the error says what
/// arrived without dumping the whole protobuf.
pub(super) fn mapping_name(mapping: &topology_mapping::Mapping) -> &'static str {
    use topology_mapping::Mapping;
    match mapping {
        Mapping::NamespaceDelegation(_) => "NamespaceDelegation",
        Mapping::DecentralizedNamespaceDefinition(_) => "DecentralizedNamespaceDefinition",
        Mapping::OwnerToKeyMapping(_) => "OwnerToKeyMapping",
        Mapping::PartyToParticipant(_) => "PartyToParticipant",
        Mapping::SynchronizerTrustCertificate(_) => "SynchronizerTrustCertificate",
        Mapping::ParticipantPermission(_) => "ParticipantPermission",
        Mapping::PartyHostingLimits(_) => "PartyHostingLimits",
        Mapping::VettedPackages(_) => "VettedPackages",
        Mapping::SynchronizerParametersState(_) => "SynchronizerParametersState",
        Mapping::MediatorSynchronizerState(_) => "MediatorSynchronizerState",
        Mapping::SequencerSynchronizerState(_) => "SequencerSynchronizerState",
        Mapping::SequencingDynamicParametersState(_) => "SequencingDynamicParametersState",
        Mapping::SynchronizerUpgradeAnnouncement(_) => "SynchronizerUpgradeAnnouncement",
        Mapping::SequencerConnectionSuccessor(_) => "SequencerConnectionSuccessor",
        _ => "an unrecognized mapping",
    }
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::enums;

    use super::*;
    use crate::workflow::validation::test_support::*;

    /// A payload that decodes to some other topology mapping must be refused
    /// outright rather than signed because it happened to parse.
    #[test]
    fn rejects_a_mapping_of_the_wrong_kind() -> Result {
        let payload = encode_proposal(topology_mapping::Mapping::PartyToParticipant(p2p(
            "dec::x",
            &[canton_id("p1", 1)?],
            1,
        )));
        assert!(decode_namespace_definition(&payload).is_err());
        Ok(())
    }

    /// A Remove that names the accepted party and members would pass every
    /// content check while tearing the mapping down, so the operation itself
    /// is pinned.
    #[test]
    fn rejects_a_remove_operation() -> Result {
        let payload = encode_proposal_with_op(
            topology_mapping::Mapping::PartyToParticipant(p2p("dec::x", &[canton_id("p1", 1)?], 1)),
            enums::TopologyChangeOp::Remove as i32,
        );
        assert!(decode_party_to_participant(&payload).is_err());
        Ok(())
    }
}
