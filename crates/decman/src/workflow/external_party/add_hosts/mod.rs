//! Adding hosting participants to an **existing** external party.
//!
//! Onboarding ([`super::steps::prepare_topology`]) writes serial 1 and creates a
//! party. This module writes serial N+1 and changes one that already exists: it
//! reads the current `PartyToParticipant` from head state, keeps every current
//! host exactly as it is, and appends the new hosts at Confirmation with Canton's
//! `Onboarding` marker so the party stays suspended on them until their ACS
//! import lands.
//!
//! Two rules make this a read-modify-write rather than a rewrite, and both exist
//! because the wallet compares what every host prepared byte-for-byte before it
//! signs (see `execute_co_validated` in the wallet crate):
//!
//! * The wallet pins the base serial in the request. Without it, two hosts that
//!   read head state a moment apart would build different transactions and the
//!   comparison would fail for a reason that is not an attack.
//! * The threshold does not move here. Not because it cannot: the decparty
//!   add-party flow writes a marked new member and a new threshold in one serial
//!   bump, and Canton takes it. The only hard rule is that the threshold must
//!   not exceed the hosts that can actually confirm, and a marked host cannot
//!   (`party_replication.proto` defines the flag being cleared as the point the
//!   party starts participating in transactions there). The known
//!   full-threshold bug is that rule broken from the other side.
//!
//!   Splitting it is about rollback rather than safety: if the raise lands with
//!   the add and the ACS replication then fails, the party sits at a threshold
//!   its live hosts may not meet until someone does another bump. Kept separate,
//!   raising is a cheap last step once the marker has cleared.

//!
//! ## Why there is no `PartyHostingLimits` check
//!
//! A synchronizer operator was once able to cap how many participants may host a
//! given party, and a preflight against that cap belonged here. It no longer
//! exists: `PartyHostingLimits` in `protocol/v30/topology.proto` carries only
//! `synchronizer_id` and `party`, with `reserved 3; // was quota = 3;` where the
//! cap used to be, and `protocol/v31` reuses that same message. Nothing is left
//! to read, so a check could only confirm the mapping exists, which constrains
//! nothing.
//!
//! If Canton reintroduces a cap, the preflight goes here, beside the
//! active-hosts invariant.

mod authorize;
mod error;
mod head;
mod mapping;
mod prepare;
mod replication;
mod submit;
#[cfg(test)]
mod test_support;
mod validate;

pub use authorize::{authorize_add_hosts, owns_party_namespace};
pub use error::AddHostsError;
pub use head::{CurrentPartyTopology, read_party_to_participant};
pub use prepare::{PreparedAddHosts, prepare_add_hosts};
pub use replication::{
    TENANT_ADD_HOSTS_ARTIFACTS, instance_is_for_party, replication_instance, replication_target,
};
pub use submit::submit_add_hosts;
pub use validate::{ExternalPartyAddHostsPayload, validate_add_hosts_topology};
