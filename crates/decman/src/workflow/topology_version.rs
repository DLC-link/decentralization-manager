//! Canton's two formats for a topology transaction.
//!
//! A synchronizer on protocol version 37 writes topology transactions in the
//! `v31` format. One on protocol version 36 or lower writes `v30`. The
//! conversions live in [`decman_lib::topology`], which the dApp shares.
//!
//! DecMan works in `v30`. It reads a `v31` transaction by converting it, and
//! [`ProposalFormat`] converts its own proposals to `v31` on a protocol
//! version 37 synchronizer.

use canton_proto_rs::com::digitalasset::canton::{
    protocol::{
        v30::{TopologyMapping, enums::TopologyChangeOp, topology_mapping::Mapping},
        v31,
    },
    topology::admin::v30::{authorize_request, generate_transactions_request},
};
use decman_lib::topology::mapping_to_v31;
pub use decman_lib::topology::{decode_transaction, p2p_from_v31};

use crate::error::Result;

/// The first protocol version that writes topology transactions in `v31`.
const FIRST_V31_PROTOCOL_VERSION: u32 = 37;

/// The first protocol version that DecMan names in its proposals. It is the
/// oldest version a Canton 3.7 participant runs.
const FIRST_NAMED_PROTOCOL_VERSION: u32 = 36;

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

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::SigningKeysWithThreshold,
        protocol::v30::{
            PartyToParticipant, enums,
            party_to_participant::{HostingParticipant, hosting_participant::Onboarding},
        },
    };
    use v31::{
        party_to_participant::hosting_participant::HostingState,
        topology_mapping::Mapping as V31Mapping,
    };

    use super::*;

    const PARTY: &str = "dec::1220aa";

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

    fn v30_host(participant: &str, onboarding: bool) -> HostingParticipant {
        HostingParticipant {
            participant_uid: participant.to_string(),
            permission: enums::ParticipantPermission::Confirmation as i32,
            onboarding: onboarding.then_some(Onboarding {}),
        }
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
}
