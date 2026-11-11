//! Builders the add-hosts tests share.

use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{
        CryptoKeyFormat, SigningKeySpec, SigningKeyUsage, SigningKeysWithThreshold,
        SigningPublicKey,
    },
    protocol::v30::{
        PartyToParticipant, TopologyMapping, TopologyTransaction,
        enums::{ParticipantPermission, TopologyChangeOp},
        party_to_participant::{HostingParticipant, hosting_participant},
        topology_mapping,
    },
    version::v1::{UntypedVersionedMessage, untyped_versioned_message},
};
use common::api::HostPermission;
use prost::Message as _;

use super::{
    head::CurrentPartyTopology,
    mapping::add_hosts_mapping,
    validate::{ExternalPartyAddHostsPayload, validate_add_hosts_topology},
};
use crate::{canton_id::CantonId, config::NodeConfig, error::Result};

pub(super) fn participant(tag: u8) -> CantonId {
    let namespace = format!("1220{}", format!("{tag:02x}").repeat(32));
    match CantonId::parse(&format!("participant-{tag}::{namespace}")) {
        Ok(id) => id,
        Err(e) => panic!("test participant id must parse: {e}"),
    }
}

pub(super) fn test_config(tag: u8) -> NodeConfig {
    let mut config = NodeConfig::default();
    config.node.participant_id = Some(participant(tag));
    config
}

pub(super) fn party_keys() -> Option<SigningKeysWithThreshold> {
    Some(SigningKeysWithThreshold {
        keys: vec![SigningPublicKey {
            format: CryptoKeyFormat::DerX509SubjectPublicKeyInfo as i32,
            public_key: vec![7u8; 44],
            key_spec: SigningKeySpec::EcCurve25519 as i32,
            usage: vec![
                SigningKeyUsage::Namespace as i32,
                SigningKeyUsage::Protocol as i32,
            ],
            ..Default::default()
        }],
        threshold: 1,
    })
}

pub(super) fn host(
    tag: u8,
    permission: ParticipantPermission,
    onboarding: bool,
) -> HostingParticipant {
    HostingParticipant {
        participant_uid: participant(tag).to_string(),
        permission: permission as i32,
        onboarding: onboarding.then_some(hosting_participant::Onboarding {}),
    }
}

/// A party at serial 4, hosted on participants 1 and 2 at threshold 1.
pub(super) fn current() -> CurrentPartyTopology {
    CurrentPartyTopology {
        serial: 4,
        mapping: PartyToParticipant {
            party: "alice::1220aa".to_string(),
            threshold: 1,
            participants: vec![
                host(1, ParticipantPermission::Confirmation, false),
                host(2, ParticipantPermission::Confirmation, false),
            ],
            party_signing_keys: party_keys(),
        },
    }
}

pub(super) fn serialize(mapping: PartyToParticipant, serial: u32) -> Vec<u8> {
    let transaction = TopologyTransaction {
        operation: TopologyChangeOp::AddReplace as i32,
        serial,
        mapping: Some(TopologyMapping {
            mapping: Some(topology_mapping::Mapping::PartyToParticipant(mapping)),
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

pub(super) fn bundle_of(mapping: PartyToParticipant, serial: u32) -> ExternalPartyAddHostsPayload {
    ExternalPartyAddHostsPayload {
        party_id: "alice::1220aa".to_string(),
        base_serial: 4,
        topology_transactions: vec![serialize(mapping, serial)],
        signatures: vec![vec![0u8; 64]],
        signed_by: "1220aa".to_string(),
    }
}

/// Validate as participant 3 — the host being added.
pub(super) fn validate(mapping: PartyToParticipant) -> Result<()> {
    validate_add_hosts_topology(&test_config(3), &current(), &bundle_of(mapping, 5))
}

pub(super) fn built() -> PartyToParticipant {
    match add_hosts_mapping(
        &current().mapping,
        &[participant(3)],
        HostPermission::Confirmation,
    ) {
        Ok(m) => m,
        Err(e) => panic!("building the add-hosts mapping must succeed: {e}"),
    }
}
