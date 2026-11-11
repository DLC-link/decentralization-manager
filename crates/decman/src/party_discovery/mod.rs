//! Party discovery: the topology reads that find the parties this node hosts.
//!
//! [`decentralized`] finds the decentralized parties this node owns a
//! namespace key in. [`external`] lists the external parties this participant
//! hosts. Both read the synchronizer's head state through
//! [`crate::workflow::topology::head_state_query`].

mod bounded;
pub mod decentralized;
pub mod external;

pub(crate) use bounded::{bounded_call, bounded_read};
pub(crate) use decentralized::{
    build_decentralized_namespace_request, build_party_to_participant_request,
    discover_hosted_party_ids, known_party_filters, my_namespace_fingerprints,
    owned_decentralized_namespaces, pair_namespaces_with_parties,
};
pub use external::{HostedExternalParty, HostedExternalPartyHost, list_hosted_external_parties};
