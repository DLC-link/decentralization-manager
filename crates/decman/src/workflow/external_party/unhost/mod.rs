//! Making THIS node leave an external party on its own.
//!
//! Canton lets a host leave without the party. `PartyToParticipant.requiredAuth`
//! has a branch for exactly this shape: the threshold is unchanged, no host is
//! added, and one host is removed. Such a write needs **either** the party's
//! namespace **or** the removed participant's, so this node's own key completes
//! it and no wallet signature is involved.
//!
//! The node also created Splice contracts for the party while it served as the
//! party's validator, and it controls both cleanups alone:
//! `ValidatorRight_ArchiveAsValidator` (controller `validator`) and
//! `TransferPreapproval_Cancel` (controller the receiver or the provider, before
//! `expiresAt`). Both run as the node's validator operator party.
//!
//! The steps run in this order, and each one is safe to repeat:
//!
//! 1. **Splice cleanup.** It runs before any topology write, so a failure
//!    leaves the party fully hosted and the operator free to retry.
//! 2. **Topology.** The serial-N+1 mapping without this node, authorized with
//!    this node's key and checked against head state at submission.
//! 3. **Audit.** [`super::unhost_audit`] holds one row per attempt. The row is
//!    written first and updated at every step, so a crash leaves it open at the
//!    step it reached, and the next attempt resumes from there.

mod attempt;
mod error;
mod preview;
mod splice_cleanup;
#[cfg(test)]
mod test_support;
mod topology;

pub use attempt::{UnhostEnv, UnhostOutcome, UnhostRequest, unhost};
pub use error::UnhostError;
pub use preview::preview;
