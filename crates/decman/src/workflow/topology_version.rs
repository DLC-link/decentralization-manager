//! Canton's two formats for a topology transaction.
//!
//! A synchronizer on protocol version 37 writes topology transactions in the
//! `v31` format. One on protocol version 36 or lower writes `v30`. The formats
//! differ only in `PartyToParticipant`. `v31` gives each host a hosting state
//! where `v30` has an onboarding flag, and it moves the party signing keys to
//! another field. So a `v30` decode of a `v31` party mapping fails.
//!
//! DecMan works in `v30`. It reads a `v31` transaction by converting it, and
//! [`ProposalFormat`] converts its own proposals to `v31` on a protocol
//! version 37 synchronizer.

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::{
    protocol::{
        v30::{
            PartyToParticipant, TopologyMapping, TopologyTransaction,
            enums::TopologyChangeOp,
            party_to_participant::{HostingParticipant, hosting_participant::Onboarding},
            topology_mapping::Mapping,
        },
        v31::{
            self, party_to_participant::hosting_participant::HostingState,
            topology_mapping::Mapping as V31Mapping,
        },
    },
    topology::admin::v30::{authorize_request, generate_transactions_request},
    version::v1::{UntypedVersionedMessage, untyped_versioned_message},
};
use prost::Message;

use crate::error::Result;

/// The first protocol version that writes topology transactions in `v31`.
const FIRST_V31_PROTOCOL_VERSION: u32 = 37;

/// The first protocol version that DecMan names in its proposals. It is the
/// oldest version a Canton 3.7 participant runs.
const FIRST_NAMED_PROTOCOL_VERSION: u32 = 36;

/// The proto version that the envelope of a `v30` transaction names.
const V30_PROTO_VERSION: i32 = 30;

/// The proto version that the envelope of a `v31` transaction names.
const V31_PROTO_VERSION: i32 = 31;

/// The protocol version a physical synchronizer id ends with, e.g. `35` from
/// `global-domain::1220ab::35-0`.
///
/// # Errors
///
/// Errors when the id does not end in a protocol version.
pub fn protocol_version_of(synchronizer_id: &str) -> Result<u32> {
    synchronizer_id
        .rsplit_once("::")
        .and_then(|(_, suffix)| suffix.split('-').next())
        .and_then(|version| version.parse::<u32>().ok())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Cannot read a protocol version from physical synchronizer id {synchronizer_id}"
            )
        })
}

/// How DecMan writes a topology proposal for one synchronizer.
///
/// From protocol version 37, the mapping goes to Canton in `v31`, with a
/// hosting state for each host. Below 37 it goes in `v30`.
///
/// From protocol version 36, the proposal also names the synchronizer's
/// protocol version. Without it, the participant's authorized store writes
/// at the participant's newest version. A Canton 3.7 participant would then
/// write `v31` for a synchronizer on protocol version 36. Below 36 the
/// proposal names no version, so an older participant gets the same request
/// as before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProposalFormat {
    protocol_version: Option<u32>,
}

/// A mapping in one of the two formats.
enum Versioned {
    V30(TopologyMapping),
    V31(v31::TopologyMapping),
}

impl ProposalFormat {
    /// The format for `synchronizer_id`. An id without a protocol version
    /// gets `v30` and Canton's default version.
    pub fn for_synchronizer(synchronizer_id: &str) -> Self {
        Self {
            protocol_version: protocol_version_of(synchronizer_id).ok(),
        }
    }

    /// An `AddReplace` proposal of `mapping` at `serial`. Serial 0 lets Canton
    /// pick the next serial.
    pub fn proposal(self, serial: u32, mapping: Mapping) -> authorize_request::Proposal {
        authorize_request::Proposal {
            protocol_version: self
                .protocol_version
                .filter(|version| *version >= FIRST_NAMED_PROTOCOL_VERSION),
            change: TopologyChangeOp::AddReplace as i32,
            serial,
            mapping: Some(match self.versioned(mapping) {
                Versioned::V30(mapping) => authorize_request::proposal::Mapping::V30(mapping),
                Versioned::V31(mapping) => authorize_request::proposal::Mapping::V31(mapping),
            }),
        }
    }

    /// `mapping` in the form a `GenerateTransactions` proposal takes.
    pub fn generated_mapping(
        self,
        mapping: Mapping,
    ) -> generate_transactions_request::proposal::Mapping {
        match self.versioned(mapping) {
            Versioned::V30(mapping) => {
                generate_transactions_request::proposal::Mapping::V30(mapping)
            }
            Versioned::V31(mapping) => {
                generate_transactions_request::proposal::Mapping::V31(mapping)
            }
        }
    }

    fn versioned(self, mapping: Mapping) -> Versioned {
        if self
            .protocol_version
            .is_some_and(|version| version >= FIRST_V31_PROTOCOL_VERSION)
        {
            Versioned::V31(v31::TopologyMapping {
                mapping: Some(mapping_to_v31(mapping)),
            })
        } else {
            Versioned::V30(TopologyMapping {
                mapping: Some(mapping),
            })
        }
    }
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
pub fn decode_transaction(bytes: &[u8]) -> Result<TopologyTransaction> {
    let versioned = UntypedVersionedMessage::decode(bytes)
        .context("Failed to decode the topology transaction's versioned envelope")?;
    let Some(untyped_versioned_message::Wrapper::Data(inner)) = versioned.wrapper else {
        anyhow::bail!("The topology transaction's versioned envelope has no data");
    };
    match versioned.version {
        V30_PROTO_VERSION => TopologyTransaction::decode(inner.as_slice())
            .context("Failed to decode the versioned message payload"),
        V31_PROTO_VERSION => {
            let transaction = v31::TopologyTransaction::decode(inner.as_slice())
                .context("Failed to decode the v31 topology transaction")?;
            transaction_from_v31(transaction)
        }
        other => anyhow::bail!(
            "The topology transaction is in proto version {other}, and DecMan reads only 30 \
             and 31"
        ),
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
pub fn transaction_from_v31(transaction: v31::TopologyTransaction) -> Result<TopologyTransaction> {
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

#[allow(deprecated)] // `PartyToKeyMapping` has to be carried over like the rest.
fn mapping_from_v31(mapping: V31Mapping) -> Result<Mapping> {
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
pub fn p2p_from_v31(mapping: v31::PartyToParticipant) -> Result<PartyToParticipant> {
    let participants = mapping
        .participants
        .into_iter()
        .map(|host| {
            let onboarding = match HostingState::try_from(host.state) {
                Ok(HostingState::Active) => None,
                Ok(HostingState::Onboarding) => Some(Onboarding {}),
                Ok(state @ (HostingState::Nominated | HostingState::Freezing)) => anyhow::bail!(
                    "Participant {participant} hosts party {party} in the {state} state, which \
                     DecMan does not support",
                    participant = host.participant_uid,
                    party = mapping.party,
                    state = state.as_str_name(),
                ),
                Ok(HostingState::Unspecified) | Err(_) => anyhow::bail!(
                    "Participant {participant} hosts party {party} in an unknown hosting state {state}",
                    participant = host.participant_uid,
                    party = mapping.party,
                    state = host.state,
                ),
            };
            Ok(HostingParticipant {
                participant_uid: host.participant_uid,
                permission: host.permission,
                onboarding,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(PartyToParticipant {
        party: mapping.party,
        threshold: mapping.threshold,
        participants,
        party_signing_keys: mapping.party_signing_keys,
    })
}

#[allow(deprecated)] // `PartyToKeyMapping` has to be carried over like the rest.
fn mapping_to_v31(mapping: Mapping) -> V31Mapping {
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

    #[test]
    fn a_v30_transaction_decodes_unchanged() -> Result {
        let transaction = TopologyTransaction {
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
        };
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
    fn a_v31_party_mapping_converts_to_its_v30_form() -> Result {
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

    /// The failure this module exists for: a `v30` decode of `v31` party
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
    fn every_other_v31_mapping_carries_over() -> Result {
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

    #[test]
    fn reads_the_protocol_version_off_a_physical_synchronizer_id() -> Result {
        assert_eq!(protocol_version_of("global-domain::1220ab::35-0")?, 35);
        assert_eq!(protocol_version_of("global-domain::1220ab::34-12")?, 34);
        assert!(protocol_version_of("global-domain::1220ab").is_err());
        assert!(protocol_version_of("no-separator").is_err());
        Ok(())
    }

    fn p2p_mapping() -> Mapping {
        Mapping::PartyToParticipant(PartyToParticipant {
            party: PARTY.to_string(),
            threshold: 2,
            participants: vec![v30_host("p1::1220bb", false), v30_host("p2::1220cc", true)],
            party_signing_keys: Some(SigningKeysWithThreshold {
                keys: vec![],
                threshold: 1,
            }),
        })
    }

    fn proposal_for(synchronizer_id: &str) -> authorize_request::Proposal {
        ProposalFormat::for_synchronizer(synchronizer_id).proposal(5, p2p_mapping())
    }

    #[test]
    fn a_proposal_is_v31_from_protocol_version_37() {
        let proposal = proposal_for("global-domain::1220ab::37-0");

        assert_eq!(proposal.protocol_version, Some(37));
        assert_eq!(proposal.serial, 5);
        assert_eq!(proposal.change, TopologyChangeOp::AddReplace as i32);
        assert_eq!(
            proposal.mapping,
            Some(authorize_request::proposal::Mapping::V31(
                v31::TopologyMapping {
                    mapping: Some(V31Mapping::PartyToParticipant(v31_p2p(vec![
                        v31_host("p1::1220bb", HostingState::Active),
                        v31_host("p2::1220cc", HostingState::Onboarding),
                    ]))),
                }
            ))
        );
    }

    /// A Canton 3.7 participant's authorized store writes at protocol
    /// version 37 unless the proposal names 36.
    #[test]
    fn a_proposal_names_protocol_version_36_and_stays_v30() {
        let proposal = proposal_for("global-domain::1220ab::36-7");

        assert_eq!(proposal.protocol_version, Some(36));
        assert_eq!(
            proposal.mapping,
            Some(authorize_request::proposal::Mapping::V30(TopologyMapping {
                mapping: Some(p2p_mapping()),
            }))
        );
    }

    #[test]
    fn an_older_or_unknown_protocol_version_keeps_the_old_request() {
        for synchronizer_id in ["global-domain::1220ab::35-6", "global-domain::1220ab"] {
            let proposal = proposal_for(synchronizer_id);
            assert_eq!(proposal.protocol_version, None, "{synchronizer_id}");
            assert_eq!(
                proposal.mapping,
                Some(authorize_request::proposal::Mapping::V30(TopologyMapping {
                    mapping: Some(p2p_mapping()),
                })),
                "{synchronizer_id}"
            );
        }
    }

    #[test]
    fn a_generated_proposal_takes_the_same_format() {
        let format = ProposalFormat::for_synchronizer("global-domain::1220ab::37-0");
        assert!(matches!(
            format.generated_mapping(p2p_mapping()),
            generate_transactions_request::proposal::Mapping::V31(_)
        ));
        let format = ProposalFormat::for_synchronizer("global-domain::1220ab::36-0");
        assert!(matches!(
            format.generated_mapping(p2p_mapping()),
            generate_transactions_request::proposal::Mapping::V30(_)
        ));
    }

    /// Peers read DecMan's `v31` proposals through the `v30` conversion, so
    /// the two conversions must undo each other.
    #[test]
    fn a_party_mapping_survives_the_trip_through_v31() -> Result {
        let Mapping::PartyToParticipant(p2p) = p2p_mapping() else {
            unreachable!("p2p_mapping builds a party mapping");
        };
        assert_eq!(p2p_from_v31(p2p_to_v31(p2p.clone()))?, p2p);
        Ok(())
    }

    #[test]
    fn every_other_mapping_converts_to_v31_unchanged() -> Result {
        let dns = Mapping::DecentralizedNamespaceDefinition(DecentralizedNamespaceDefinition {
            decentralized_namespace: "1220dd".to_string(),
            threshold: 2,
            owners: vec!["1220ee".to_string()],
        });
        assert_eq!(mapping_from_v31(mapping_to_v31(dns.clone()))?, dns);
        Ok(())
    }
}
