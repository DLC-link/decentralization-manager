//! Builders the validation tests share.

use std::collections::BTreeMap;

use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{SigningKeyUsage, SigningKeysWithThreshold, SigningPublicKey},
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToKeyMapping, PartyToParticipant,
        SignedTopologyTransaction, TopologyMapping, TopologyTransaction, enums,
        party_to_participant::HostingParticipant, topology_mapping,
    },
    version::v1::{UntypedVersionedMessage, untyped_versioned_message},
};
use prost::Message;

use super::{expectations::PeerExpectations, head::TopologyHead};
use crate::{
    canton_id::{CantonId, Fingerprint},
    error::Result,
    server::WorkflowKind,
    utils,
};

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
        rotated_key: None,
        coordinator_id: self_id.clone(),
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
                usage: vec![SigningKeyUsage::Protocol as i32],
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

pub(super) struct StubHead {
    pub(super) dns: Option<(u32, DecentralizedNamespaceDefinition)>,
    pub(super) p2p: Option<(u32, PartyToParticipant)>,
    pub(super) legacy_keys: Option<Vec<SigningPublicKey>>,
}

impl TopologyHead for StubHead {
    async fn namespace_definition(
        &self,
        _namespace: &str,
    ) -> Result<(u32, DecentralizedNamespaceDefinition)> {
        self.dns
            .clone()
            .ok_or_else(|| anyhow::anyhow!("topology read unavailable"))
    }

    async fn p2p_mapping(&self, _party: &CantonId) -> Result<(u32, PartyToParticipant)> {
        self.p2p
            .clone()
            .ok_or_else(|| anyhow::anyhow!("topology read unavailable"))
    }

    async fn party_to_key_mapping(&self, _party: &CantonId) -> Result<Option<PartyToKeyMapping>> {
        Ok(self
            .legacy_keys
            .clone()
            .map(|signing_keys| PartyToKeyMapping {
                signing_keys,
                threshold: 2,
                ..Default::default()
            }))
    }
}

pub(super) fn keyed_p2p(keys: Option<SigningKeysWithThreshold>) -> Result<PartyToParticipant> {
    let hosts = [
        canton_id("p1", 1)?,
        canton_id("p2", 2)?,
        canton_id("p3", 3)?,
    ];
    Ok(PartyToParticipant {
        party_signing_keys: keys,
        ..p2p("party", &hosts, 2)
    })
}

pub(super) fn key_of(seed: u8) -> SigningPublicKey {
    SigningPublicKey {
        public_key: vec![seed; 32],
        usage: vec![SigningKeyUsage::Protocol as i32],
        ..Default::default()
    }
}

pub(super) fn member_claims(entries: &[(&str, u8)]) -> BTreeMap<String, Fingerprint> {
    entries
        .iter()
        .map(|(member, seed)| {
            (
                (*member).to_string(),
                utils::compute_fingerprint(&key_of(*seed)),
            )
        })
        .collect()
}

pub(super) fn fingerprints(seeds: &[u8]) -> Vec<Fingerprint> {
    seeds
        .iter()
        .map(|seed| utils::compute_fingerprint(&key_of(*seed)))
        .collect()
}
