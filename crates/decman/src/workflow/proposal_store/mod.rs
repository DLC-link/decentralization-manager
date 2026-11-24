//! Party topology proposals, signed without being published (#448).
//!
//! Add-party, kick and change-threshold change a party's
//! `DecentralizedNamespaceDefinition` (DNS) and `PartyToParticipant` (P2P)
//! together. `Authorize` against the synchronizer store proposes *and
//! distributes*, and it authorizes outright when this node alone holds enough
//! keys. At namespace threshold 1 that put the DNS in force the moment the
//! coordinator built it, while the paired P2P still waited for the members.
//!
//! The coordinator now authorizes both proposals in a temporary topology
//! store. Canton keeps such a store in the participant's memory, signs into
//! it without distributing anything, and forgets it when it is dropped. The
//! proposals reach the synchronizer only when submit publishes them. The one
//! exception is add-party's P2P, which its create step publishes as a pending
//! proposal for the new member: adding a host needs that host's signature, so
//! the proposal cannot take effect early.
//!
//! Canton accepts an explicit serial in a store only as the successor of that
//! store's head for the mapping, and a new store has no head. So the store is
//! first seeded with the party's history from the synchronizer, replayed in
//! order, until its DNS and P2P heads are the synchronizer's.
//!
//! A replay can fail to rebuild the heads. `signing_route` then decides by
//! the party's current namespace threshold. At threshold 1 the run stops with
//! nothing signed, because only there can the coordinator alone put the DNS in
//! force. Above it the coordinator signs with `Authorize` against the
//! synchronizer store, as before #448: its signature alone cannot meet the
//! threshold, so nothing takes effect early. A participant that cannot be
//! reached is never a replay failure; that error goes back to the step.
//!
//! Every store is dropped on every way out of [`sign_party_proposals`]: after
//! success, after an error, and from a drop guard when the task is aborted.
//! Store names carry the run's instance name, so a run that resumes after a
//! crash drops what its earlier attempt left. [`drop_leftover_stores`] sweeps
//! the rest at boot.

mod change;
mod route;
mod seed;
mod sign;
mod store;

pub use change::{
    SignedPartyChange, hosts_signing_keys, read_exported_namespace, save_party_proposals,
};
pub use sign::{PartyHead, SignedProposals, fetch_party_head, sign_party_proposals};
pub use store::{STORE_PREFIX, drop_leftover_stores};
