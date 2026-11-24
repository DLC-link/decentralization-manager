//! Canton topology transactions: their two wire formats, their hashes, and
//! the signing key of an external party.
//!
//! A synchronizer on protocol version 37 writes topology transactions in the
//! `v31` format. One on protocol version 36 or lower writes `v30`. The formats
//! differ only in `PartyToParticipant`. `v31` gives each host a hosting state
//! where `v30` has an onboarding flag, and it moves the party signing keys to
//! another field. So a `v30` decode of a `v31` party mapping fails.
//!
//! The DecMan server and the dApp both work in `v30`. They convert at the
//! edge with the functions here, so the two cannot disagree on a byte.

use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{CryptoKeyFormat, SigningKeySpec, SigningKeyUsage, SigningPublicKey},
    protocol::{
        v30::{
            PartyToParticipant, TopologyMapping, TopologyTransaction,
            party_to_participant::{HostingParticipant, hosting_participant::Onboarding},
            topology_mapping::Mapping,
        },
        v31::{
            self, party_to_participant::hosting_participant::HostingState,
            topology_mapping::Mapping as V31Mapping,
        },
    },
    version::v1::{UntypedVersionedMessage, untyped_versioned_message},
};
use prost::Message;
use sha2::{Digest, Sha256};

use crate::Error;

/// The proto version that the envelope of a `v30` transaction names.
const V30_PROTO_VERSION: i32 = 30;

/// The proto version that the envelope of a `v31` transaction names.
const V31_PROTO_VERSION: i32 = 31;

/// Canton's `HashPurpose.TopologyTransactionSignature`.
const TOPOLOGY_TRANSACTION_HASH_PURPOSE: u32 = 11;

/// The multihash prefix of a SHA-256 digest: code 0x12, length 0x20.
const SHA256_MULTIHASH_PREFIX: [u8; 2] = [0x12, 0x20];

/// DER prefix of an Ed25519 `SubjectPublicKeyInfo` (RFC 8410): a 42-byte
/// SEQUENCE with the `id-Ed25519` algorithm and a 33-byte BIT STRING. The
/// 32-byte key follows.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// The wire format of a synchronizer's topology transactions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopologyFormat {
    V30,
    V31,
}

impl TopologyFormat {
    /// The format whose envelope names `version`.
    ///
    /// # Errors
    ///
    /// Errors for any version other than 30 and 31.
    pub fn from_proto_version(version: i32) -> Result<Self, Error> {
        match version {
            V30_PROTO_VERSION => Ok(Self::V30),
            V31_PROTO_VERSION => Ok(Self::V31),
            other => Err(Error::decode(format!(
                "The topology transaction is in proto version {other}. Only 30 and 31 are \
                 supported"
            ))),
        }
    }

    /// The proto version that this format's envelope names.
    pub fn proto_version(self) -> i32 {
        match self {
            Self::V30 => V30_PROTO_VERSION,
            Self::V31 => V31_PROTO_VERSION,
        }
    }
}

/// The format of a serialized topology transaction, read from its envelope.
///
/// # Errors
///
/// Errors when the envelope does not decode or names an unknown version.
pub fn envelope_format(bytes: &[u8]) -> Result<TopologyFormat, Error> {
    let versioned = UntypedVersionedMessage::decode(bytes).map_err(|e| {
        Error::decode_from(
            "Failed to decode the topology transaction's versioned envelope",
            e,
        )
    })?;
    TopologyFormat::from_proto_version(versioned.version)
}

/// `transaction` serialized as Canton serializes it in `format`: the
/// versioned envelope around the transaction.
///
/// Canton hashes and verifies these exact bytes, so a signature covers them
/// and nothing else.
pub fn encode_transaction(format: TopologyFormat, transaction: TopologyTransaction) -> Vec<u8> {
    let data = match format {
        TopologyFormat::V30 => transaction.encode_to_vec(),
        TopologyFormat::V31 => transaction_to_v31(transaction).encode_to_vec(),
    };
    UntypedVersionedMessage {
        wrapper: Some(untyped_versioned_message::Wrapper::Data(data)),
        version: format.proto_version(),
    }
    .encode_to_vec()
}

/// Decode a topology transaction from its versioned envelope, as `v30`.
///
/// A `v31` transaction is converted with [`transaction_from_v31`].
///
/// # Errors
///
/// Errors when the envelope names a version other than 30 or 31, when the
/// envelope or the transaction inside it fails to decode, or when a `v31`
/// transaction has no `v30` form.
pub fn decode_transaction(bytes: &[u8]) -> Result<TopologyTransaction, Error> {
    let versioned = UntypedVersionedMessage::decode(bytes).map_err(|e| {
        Error::decode_from(
            "Failed to decode the topology transaction's versioned envelope",
            e,
        )
    })?;
    let Some(untyped_versioned_message::Wrapper::Data(inner)) = versioned.wrapper else {
        return Err(Error::decode(
            "The topology transaction's versioned envelope has no data",
        ));
    };
    match TopologyFormat::from_proto_version(versioned.version)? {
        TopologyFormat::V30 => TopologyTransaction::decode(inner.as_slice())
            .map_err(|e| Error::decode_from("Failed to decode the versioned message payload", e)),
        TopologyFormat::V31 => {
            let transaction = v31::TopologyTransaction::decode(inner.as_slice()).map_err(|e| {
                Error::decode_from("Failed to decode the v31 topology transaction", e)
            })?;
            transaction_from_v31(transaction)
        }
    }
}

/// The `v30` form of a `v31` topology transaction.
///
/// Every `v31` mapping except `PartyToParticipant` already is a `v30`
/// message. See [`p2p_from_v31`] for that one.
///
/// # Errors
///
/// Errors when the transaction holds a `PartyToParticipant` with no `v30`
/// form.
pub fn transaction_from_v31(
    transaction: v31::TopologyTransaction,
) -> Result<TopologyTransaction, Error> {
    let mapping = transaction
        .mapping
        .and_then(|mapping| mapping.mapping)
        .map(mapping_from_v31)
        .transpose()?;
    Ok(TopologyTransaction {
        operation: transaction.operation,
        serial: transaction.serial,
        mapping: Some(TopologyMapping { mapping }),
    })
}

/// The `v31` form of a `v30` topology transaction.
pub fn transaction_to_v31(transaction: TopologyTransaction) -> v31::TopologyTransaction {
    v31::TopologyTransaction {
        operation: transaction.operation,
        serial: transaction.serial,
        mapping: transaction.mapping.map(|mapping| v31::TopologyMapping {
            mapping: mapping.mapping.map(mapping_to_v31),
        }),
    }
}

#[allow(deprecated)] // `PartyToKeyMapping` has to be carried over like the rest.
fn mapping_from_v31(mapping: V31Mapping) -> Result<Mapping, Error> {
    Ok(match mapping {
        V31Mapping::NamespaceDelegation(m) => Mapping::NamespaceDelegation(m),
        V31Mapping::DecentralizedNamespaceDefinition(m) => {
            Mapping::DecentralizedNamespaceDefinition(m)
        }
        V31Mapping::OwnerToKeyMapping(m) => Mapping::OwnerToKeyMapping(m),
        V31Mapping::SynchronizerTrustCertificate(m) => Mapping::SynchronizerTrustCertificate(m),
        V31Mapping::ParticipantPermission(m) => Mapping::ParticipantPermission(m),
        V31Mapping::PartyHostingLimits(m) => Mapping::PartyHostingLimits(m),
        V31Mapping::VettedPackages(m) => Mapping::VettedPackages(m),
        V31Mapping::PartyToParticipant(m) => Mapping::PartyToParticipant(p2p_from_v31(m)?),
        V31Mapping::SynchronizerParametersState(m) => Mapping::SynchronizerParametersState(m),
        V31Mapping::MediatorSynchronizerState(m) => Mapping::MediatorSynchronizerState(m),
        V31Mapping::SequencerSynchronizerState(m) => Mapping::SequencerSynchronizerState(m),
        V31Mapping::SequencingDynamicParametersState(m) => {
            Mapping::SequencingDynamicParametersState(m)
        }
        V31Mapping::PartyToKeyMapping(m) => Mapping::PartyToKeyMapping(m),
        V31Mapping::SynchronizerUpgradeAnnouncement(m) => {
            Mapping::SynchronizerUpgradeAnnouncement(m)
        }
        V31Mapping::SequencerConnectionSuccessor(m) => Mapping::SequencerConnectionSuccessor(m),
    })
}

/// The `v31` form of a `v30` mapping. Only `PartyToParticipant` changes.
#[allow(deprecated)] // `PartyToKeyMapping` has to be carried over like the rest.
pub fn mapping_to_v31(mapping: Mapping) -> V31Mapping {
    match mapping {
        Mapping::NamespaceDelegation(m) => V31Mapping::NamespaceDelegation(m),
        Mapping::DecentralizedNamespaceDefinition(m) => {
            V31Mapping::DecentralizedNamespaceDefinition(m)
        }
        Mapping::OwnerToKeyMapping(m) => V31Mapping::OwnerToKeyMapping(m),
        Mapping::SynchronizerTrustCertificate(m) => V31Mapping::SynchronizerTrustCertificate(m),
        Mapping::ParticipantPermission(m) => V31Mapping::ParticipantPermission(m),
        Mapping::PartyHostingLimits(m) => V31Mapping::PartyHostingLimits(m),
        Mapping::VettedPackages(m) => V31Mapping::VettedPackages(m),
        Mapping::PartyToParticipant(m) => V31Mapping::PartyToParticipant(p2p_to_v31(m)),
        Mapping::SynchronizerParametersState(m) => V31Mapping::SynchronizerParametersState(m),
        Mapping::MediatorSynchronizerState(m) => V31Mapping::MediatorSynchronizerState(m),
        Mapping::SequencerSynchronizerState(m) => V31Mapping::SequencerSynchronizerState(m),
        Mapping::SequencingDynamicParametersState(m) => {
            V31Mapping::SequencingDynamicParametersState(m)
        }
        Mapping::PartyToKeyMapping(m) => V31Mapping::PartyToKeyMapping(m),
        Mapping::SynchronizerUpgradeAnnouncement(m) => {
            V31Mapping::SynchronizerUpgradeAnnouncement(m)
        }
        Mapping::SequencerConnectionSuccessor(m) => V31Mapping::SequencerConnectionSuccessor(m),
    }
}

/// The `v30` form of a `v31` `PartyToParticipant`.
///
/// An `ONBOARDING` host gets the `v30` onboarding flag, and an `ACTIVE` host
/// gets none. The party signing keys carry over unchanged.
///
/// # Errors
///
/// Errors when a host is `NOMINATED` or `FREEZING`. `v30` cannot express
/// either state, and DecMan never proposes them. Treating such a host as
/// active or onboarding would misreport who can confirm for the party.
pub fn p2p_from_v31(mapping: v31::PartyToParticipant) -> Result<PartyToParticipant, Error> {
    let participants = mapping
        .participants
        .into_iter()
        .map(|host| {
            let onboarding = match HostingState::try_from(host.state) {
                Ok(HostingState::Active) => None,
                Ok(HostingState::Onboarding) => Some(Onboarding {}),
                Ok(state @ (HostingState::Nominated | HostingState::Freezing)) => {
                    return Err(Error::decode(format!(
                        "Participant {participant} hosts party {party} in the {state} state, \
                         which DecMan does not support. DecMan reads the party again once that \
                         host is onboarding or active",
                        participant = host.participant_uid,
                        party = mapping.party,
                        state = state.as_str_name(),
                    )));
                }
                Ok(HostingState::Unspecified) | Err(_) => {
                    return Err(Error::decode(format!(
                        "Participant {participant} hosts party {party} in an unknown hosting \
                         state {state}",
                        participant = host.participant_uid,
                        party = mapping.party,
                        state = host.state,
                    )));
                }
            };
            Ok(HostingParticipant {
                participant_uid: host.participant_uid,
                permission: host.permission,
                onboarding,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(PartyToParticipant {
        party: mapping.party,
        threshold: mapping.threshold,
        participants,
        party_signing_keys: mapping.party_signing_keys,
    })
}

/// The `v31` form of a `v30` `PartyToParticipant`.
///
/// A host with the onboarding flag is `ONBOARDING`, and any other host is
/// `ACTIVE`. So DecMan only ever moves a host forward: it adds the host as
/// onboarding and later clears the flag.
pub fn p2p_to_v31(mapping: PartyToParticipant) -> v31::PartyToParticipant {
    let participants = mapping
        .participants
        .into_iter()
        .map(|host| v31::party_to_participant::HostingParticipant {
            participant_uid: host.participant_uid,
            permission: host.permission,
            state: if host.onboarding.is_some() {
                HostingState::Onboarding
            } else {
                HostingState::Active
            } as i32,
        })
        .collect();
    v31::PartyToParticipant {
        party: mapping.party,
        threshold: mapping.threshold,
        participants,
        party_signing_keys: mapping.party_signing_keys,
    }
}

/// Canton's hash of a serialized versioned topology transaction, which each
/// signature on the transaction covers: a SHA-256 multihash over the 4-byte
/// hash purpose and the transaction bytes.
pub fn topology_transaction_hash(transaction: &[u8]) -> Vec<u8> {
    let digest = Sha256::new()
        .chain_update(TOPOLOGY_TRANSACTION_HASH_PURPOSE.to_be_bytes())
        .chain_update(transaction)
        .finalize();
    [SHA256_MULTIHASH_PREFIX.as_slice(), digest.as_slice()].concat()
}

/// An external party's signing key, as it appears in `PartyToParticipant`.
///
/// The key does two jobs, so it declares both usages. `Namespace` lets it
/// authorize the party's topology. `Protocol` lets it authorize Daml
/// transactions: Canton filters on that usage when it executes an externally
/// signed submission. A key without it onboards, and then every submission
/// fails with "registered signing keys (0) with protocol usage".
///
/// Canton parses the key as X.509 `SubjectPublicKeyInfo` and rejects a bare
/// 32-byte key, even when the format says raw. The fingerprint still covers
/// only the raw key, so the wrapping does not change the party id.
pub fn party_signing_key(public_key: &[u8; 32]) -> SigningPublicKey {
    SigningPublicKey {
        format: CryptoKeyFormat::DerX509SubjectPublicKeyInfo as i32,
        public_key: [ED25519_SPKI_PREFIX.as_slice(), public_key.as_slice()].concat(),
        key_spec: SigningKeySpec::EcCurve25519 as i32,
        usage: vec![
            SigningKeyUsage::Namespace as i32,
            SigningKeyUsage::Protocol as i32,
        ],
        // `scheme` is deprecated in favour of `key_spec`.
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::SigningKeysWithThreshold,
        protocol::v30::{DecentralizedNamespaceDefinition, enums},
    };

    use super::*;

    const PARTY: &str = "dec::1220aa";

    fn envelope(version: i32, transaction: impl Message) -> Vec<u8> {
        UntypedVersionedMessage {
            wrapper: Some(untyped_versioned_message::Wrapper::Data(
                transaction.encode_to_vec(),
            )),
            version,
        }
        .encode_to_vec()
    }

    fn v31_host(
        participant: &str,
        state: HostingState,
    ) -> v31::party_to_participant::HostingParticipant {
        v31::party_to_participant::HostingParticipant {
            participant_uid: participant.to_string(),
            permission: enums::ParticipantPermission::Confirmation as i32,
            state: state as i32,
        }
    }

    fn v31_p2p(
        hosts: Vec<v31::party_to_participant::HostingParticipant>,
    ) -> v31::PartyToParticipant {
        v31::PartyToParticipant {
            party: PARTY.to_string(),
            threshold: 2,
            participants: hosts,
            party_signing_keys: Some(SigningKeysWithThreshold {
                keys: vec![],
                threshold: 1,
            }),
        }
    }

    fn v31_transaction(mapping: V31Mapping) -> v31::TopologyTransaction {
        v31::TopologyTransaction {
            operation: enums::TopologyChangeOp::AddReplace as i32,
            serial: 4,
            mapping: Some(v31::TopologyMapping {
                mapping: Some(mapping),
            }),
        }
    }

    fn v30_host(participant: &str, onboarding: bool) -> HostingParticipant {
        HostingParticipant {
            participant_uid: participant.to_string(),
            permission: enums::ParticipantPermission::Confirmation as i32,
            onboarding: onboarding.then_some(Onboarding {}),
        }
    }

    fn v30_p2p_transaction() -> TopologyTransaction {
        TopologyTransaction {
            operation: enums::TopologyChangeOp::AddReplace as i32,
            serial: 3,
            mapping: Some(TopologyMapping {
                mapping: Some(Mapping::PartyToParticipant(PartyToParticipant {
                    party: PARTY.to_string(),
                    threshold: 1,
                    participants: vec![v30_host("p1::1220bb", false), v30_host("p2::1220cc", true)],
                    party_signing_keys: None,
                })),
            }),
        }
    }

    #[test]
    fn a_v30_transaction_decodes_unchanged() -> Result<(), Error> {
        let transaction = v30_p2p_transaction();
        assert_eq!(
            decode_transaction(&envelope(30, transaction.clone()))?,
            transaction
        );
        Ok(())
    }

    #[test]
    fn an_unknown_envelope_version_is_refused() {
        let transaction = TopologyTransaction {
            operation: enums::TopologyChangeOp::AddReplace as i32,
            serial: 1,
            mapping: None,
        };
        for version in [0, 29, 32] {
            let error = decode_transaction(&envelope(version, transaction.clone()))
                .err()
                .map(|e| e.to_string());
            assert!(
                error
                    .as_deref()
                    .is_some_and(|e| e.contains(&format!("proto version {version}"))),
                "version {version} gave {error:?}"
            );
        }
    }

    #[test]
    fn a_v31_party_mapping_converts_to_its_v30_form() -> Result<(), Error> {
        let transaction = v31_transaction(V31Mapping::PartyToParticipant(v31_p2p(vec![
            v31_host("p1::1220bb", HostingState::Active),
            v31_host("p2::1220cc", HostingState::Onboarding),
        ])));

        let decoded = decode_transaction(&envelope(31, transaction))?;

        assert_eq!(
            decoded,
            TopologyTransaction {
                operation: enums::TopologyChangeOp::AddReplace as i32,
                serial: 4,
                mapping: Some(TopologyMapping {
                    mapping: Some(Mapping::PartyToParticipant(PartyToParticipant {
                        party: PARTY.to_string(),
                        threshold: 2,
                        participants: vec![
                            v30_host("p1::1220bb", false),
                            v30_host("p2::1220cc", true),
                        ],
                        party_signing_keys: Some(SigningKeysWithThreshold {
                            keys: vec![],
                            threshold: 1,
                        }),
                    })),
                }),
            }
        );
        Ok(())
    }

    /// The failure the two formats exist for: a `v30` decode of `v31` party
    /// bytes is a wire error, because the hosting state reuses the field
    /// number of the onboarding flag with another wire type.
    #[test]
    fn a_v30_decode_of_a_v31_party_mapping_fails() {
        let transaction = v31_transaction(V31Mapping::PartyToParticipant(v31_p2p(vec![v31_host(
            "p1::1220bb",
            HostingState::Active,
        )])));
        assert!(TopologyTransaction::decode(transaction.encode_to_vec().as_slice()).is_err());
    }

    #[test]
    fn a_host_v30_cannot_express_is_refused() {
        for state in [
            HostingState::Nominated,
            HostingState::Freezing,
            HostingState::Unspecified,
        ] {
            let mapping = v31_p2p(vec![
                v31_host("p1::1220bb", HostingState::Active),
                v31_host("p2::1220cc", state),
            ]);
            let error = p2p_from_v31(mapping).expect_err("the state has no v30 form");
            assert!(error.to_string().contains("p2::1220cc"), "{error}");
        }
    }

    #[test]
    fn every_other_v31_mapping_carries_over() -> Result<(), Error> {
        let dns = DecentralizedNamespaceDefinition {
            decentralized_namespace: "1220dd".to_string(),
            threshold: 2,
            owners: vec!["1220ee".to_string(), "1220ff".to_string()],
        };
        let transaction =
            v31_transaction(V31Mapping::DecentralizedNamespaceDefinition(dns.clone()));

        let decoded = decode_transaction(&envelope(31, transaction))?;

        assert_eq!(
            decoded.mapping.and_then(|m| m.mapping),
            Some(Mapping::DecentralizedNamespaceDefinition(dns))
        );
        Ok(())
    }

    /// Readers decode a `v31` proposal through the `v30` conversion, so the
    /// two conversions must undo each other.
    #[test]
    fn a_party_mapping_survives_the_trip_through_v31() -> Result<(), Error> {
        let Some(TopologyMapping {
            mapping: Some(Mapping::PartyToParticipant(p2p)),
        }) = v30_p2p_transaction().mapping
        else {
            unreachable!("v30_p2p_transaction builds a party mapping");
        };
        assert_eq!(p2p_from_v31(p2p_to_v31(p2p.clone()))?, p2p);
        Ok(())
    }

    #[test]
    fn every_other_mapping_converts_to_v31_unchanged() -> Result<(), Error> {
        let dns = Mapping::DecentralizedNamespaceDefinition(DecentralizedNamespaceDefinition {
            decentralized_namespace: "1220dd".to_string(),
            threshold: 2,
            owners: vec!["1220ee".to_string()],
        });
        assert_eq!(mapping_from_v31(mapping_to_v31(dns.clone()))?, dns);
        Ok(())
    }

    #[test]
    fn an_encoded_transaction_names_its_format_and_decodes_back() -> Result<(), Error> {
        for format in [TopologyFormat::V30, TopologyFormat::V31] {
            let bytes = encode_transaction(format, v30_p2p_transaction());
            assert_eq!(envelope_format(&bytes)?, format);
            assert_eq!(decode_transaction(&bytes)?, v30_p2p_transaction());
        }
        Ok(())
    }

    /// A `v31` envelope must hold `v31` bytes, or Canton cannot read the
    /// hosting states.
    #[test]
    fn a_v31_encoding_carries_hosting_states() -> Result<(), Error> {
        let bytes = encode_transaction(TopologyFormat::V31, v30_p2p_transaction());
        let versioned = UntypedVersionedMessage::decode(bytes.as_slice())
            .map_err(|e| Error::decode_from("envelope", e))?;
        let Some(untyped_versioned_message::Wrapper::Data(data)) = versioned.wrapper else {
            return Err(Error::decode("no data"));
        };
        let transaction = v31::TopologyTransaction::decode(data.as_slice())
            .map_err(|e| Error::decode_from("v31", e))?;
        let Some(V31Mapping::PartyToParticipant(p2p)) = transaction.mapping.and_then(|m| m.mapping)
        else {
            return Err(Error::decode("not a party mapping"));
        };
        let states: Vec<i32> = p2p.participants.iter().map(|host| host.state).collect();
        assert_eq!(
            states,
            [HostingState::Active as i32, HostingState::Onboarding as i32]
        );
        Ok(())
    }

    /// The vector comes from an independent computation of Canton's rule:
    /// SHA-256 over the big-endian purpose 11 and the bytes, behind `1220`.
    #[test]
    fn the_topology_hash_follows_cantons_rule() {
        assert_eq!(
            hex::encode(topology_transaction_hash(
                b"a serialized topology transaction"
            )),
            "1220ddc8a584bebf95ead3cf2ce22f591ed98c41643025e3ad5ae99399ad6a4a46fa"
        );
    }

    #[test]
    fn a_party_signing_key_wraps_the_raw_key_and_declares_both_usages() {
        let key = party_signing_key(&[7u8; 32]);
        assert_eq!(key.public_key.len(), 44);
        assert_eq!(&key.public_key[..12], ED25519_SPKI_PREFIX.as_slice());
        assert_eq!(&key.public_key[12..], [7u8; 32].as_slice());
        assert_eq!(
            key.usage,
            [
                SigningKeyUsage::Namespace as i32,
                SigningKeyUsage::Protocol as i32
            ]
        );
    }
}
