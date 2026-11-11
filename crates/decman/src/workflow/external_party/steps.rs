//! Canton calls behind the wallet-driven external-party tenant API.
//!
//! Everything here goes over the **tokenless Canton Admin API**, the same path the
//! decentralized-party workflows use. [`prepare_topology`] builds the onboarding
//! transaction and asks Canton for its hash, [`allocate_party`] attaches the
//! party's signature, has this node co-sign, and submits it, and
//! [`host_onboarding_status`] / [`list_hosted_external_parties`] read topology
//! state. There is no coordinator and no inter-DPM coordination: the wallet calls
//! each host itself.
//!
//! Deliberately NOT the Ledger API's `AllocateExternalParty`. That RPC is a
//! convenience wrapper around exactly this topology write, and the wrapper is where
//! the authorization check and the party-allocation quota live — it demands either
//! `ParticipantAdmin` or a `user_id` matching the caller, and naming a user turns on
//! a quota that defaults to zero. Writing the topology directly needs no ledger
//! credential at all, so onboarding no longer depends on how a node's ledger users
//! happen to be provisioned.

pub use super::{
    onboarding::{
        ExternalPartyAllocatePayload, PreparedTopology, allocate_party, party_signing_key,
        prepare_topology,
    },
    status::{
        HostOnboardingStatus, HostedExternalParty, HostedExternalPartyHost, host_onboarding_status,
        list_hosted_external_parties,
    },
};
