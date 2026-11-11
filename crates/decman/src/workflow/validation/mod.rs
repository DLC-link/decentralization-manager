//! Peer-side validation of coordinator payloads against the accepted invitation.
//!
//! A peer authenticates the coordinator over the Noise channel and its
//! operator accepts one invitation per workflow run. Everything after that is
//! automatic, so without this module the peer signs whatever the coordinator
//! sends: a topology transaction for any party, with any owner set and any
//! threshold, or a DAR it never agreed to install.
//!
//! The invitation the operator accepted is the peer's statement of intent, and
//! it is persisted on the peer's `workflow_runs` row. [`PeerExpectations`]
//! reads it back and every signing step checks its payload against it. A
//! mismatch is refused — the peer never signs something the operator did not
//! agree to.
//!
//! Checks that the invitation can already answer are always enforced. Fields
//! added later (the DAR hashes) are enforced when the coordinator sends them
//! and warned about when it does not, so a network mid-upgrade keeps working.

mod decode;
mod delta;
mod expectations;
mod head;
mod onboarding;
mod own_keys;
mod p2p;
mod party_proposals;
mod pins;
#[cfg(test)]
mod test_support;

pub use expectations::PeerExpectations;
pub use pins::hash_dar;
