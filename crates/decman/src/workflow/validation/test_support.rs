//! Builders the validation tests share.

use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{SigningKeysWithThreshold, SigningPublicKey},
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToParticipant, SignedTopologyTransaction,
        TopologyMapping, TopologyTransaction, enums, party_to_participant::HostingParticipant,
        topology_mapping,
    },
    version::v1::{UntypedVersionedMessage, untyped_versioned_message},
};
use prost::Message;

use super::expectations::PeerExpectations;
use crate::{canton_id::CantonId, error::Result, server::WorkflowKind, utils};

pub(super) fn canton_id(prefix: &str, byte: u8) -> Result<CantonId> {
    let mut namespace = vec![0x12, 0x20];
    namespace.extend(std::iter::repeat_n(byte, 32));
    CantonId::parse(&format!("{prefix}::{hex}", hex = hex::encode(namespace)))
}

pub(super) fn expectations(members: Vec<CantonId>, self_id: CantonId) -> PeerExpectations {
    PeerExpectations {
        kind: WorkflowKind::ChangeThreshold,
        contract_intents: Vec::new(),
        members: members.into_iter().collect(),
        dec_party_id: None,
        prefix: None,
        threshold: Some(2),
        confirmation_threshold: None,
        kicked_participant: None,
        kicked_owner_key: None,
        new_participant: None,
        dar_filenames: Vec::new(),
        dar_hashes: Vec::new(),
        self_id,
    }
}

pub(super) fn p2p(party: &str, hosts: &[CantonId], threshold: u32) -> PartyToParticipant {
    PartyToParticipant {
        party: party.to_string(),
        threshold,
        participants: hosts
            .iter()
            .map(|id| HostingParticipant {
                participant_uid: id.to_string(),
                permission: enums::ParticipantPermission::Confirmation as i32,
                onboarding: None,
            })
            .collect(),
        party_signing_keys: None,
    }
}

/// One distinct Daml signing key per member — the shape every mapping
/// this tool builds carries, and what the checks expect to see.
pub(super) fn signing_keys(count: u8, threshold: u32) -> SigningKeysWithThreshold {
    SigningKeysWithThreshold {
        keys: (1..=count)
            .map(|seed| SigningPublicKey {
                public_key: vec![seed; 32],
                ..Default::default()
            })
            .collect(),
        threshold,
    }
}

/// Wrap a mapping the way the coordinator ships it, so the decode path is
/// exercised end to end rather than around.
pub(super) fn encode_proposal(mapping: topology_mapping::Mapping) -> Vec<u8> {
    encode_proposal_with_op(mapping, enums::TopologyChangeOp::AddReplace as i32)
}

pub(super) fn encode_proposal_with_op(
    mapping: topology_mapping::Mapping,
    operation: i32,
) -> Vec<u8> {
    let transaction = TopologyTransaction {
        operation,
        serial: 1,
        mapping: Some(TopologyMapping {
            mapping: Some(mapping),
        }),
    };
    let signed = SignedTopologyTransaction {
        transaction: UntypedVersionedMessage {
            version: 30,
            wrapper: Some(untyped_versioned_message::Wrapper::Data(
                transaction.encode_to_vec(),
            )),
        }
        .encode_to_vec(),
        ..Default::default()
    };
    utils::encode_length_prefixed_message(&signed)
}

pub(super) fn owners(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

pub(super) fn namespace(owners_list: &[&str]) -> DecentralizedNamespaceDefinition {
    DecentralizedNamespaceDefinition {
        decentralized_namespace: "dns".to_string(),
        threshold: 1,
        owners: owners(owners_list),
    }
}
