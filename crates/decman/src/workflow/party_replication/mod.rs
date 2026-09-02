//! Party replication, independent of what kind of party is being replicated.
//!
//! Moving a party onto a participant that does not yet hold it is the same
//! sequence of Canton calls whoever owns the party: capture a ledger offset
//! before the topology moves, export the ACS scoped to the target, import it on
//! the target across a synchronizer disconnect, then clear Canton's
//! `HostingParticipant.Onboarding` marker so the party goes live there.
//!
//! None of that depends on a `DecentralizedNamespaceDefinition`. It was written
//! inside the decentralized-party add-party workflow and took an
//! [`AddPartyConfig`](crate::workflow::add_party::AddPartyConfig), which tied it
//! to decparties for no reason other than where it happened to live. This module
//! is the same code addressed by a [`ReplicationTarget`] instead — a party, a
//! participant, and the artifact keys one run's durable markers live under.
//!
//! What stays outside: deciding the topology and getting it authorized. That is
//! genuinely party-type specific — a decparty needs owner-threshold signatures
//! over a DNS, an external party needs its own key — and it lives with each
//! workflow.

pub mod acs;
pub mod offset;
pub mod onboarding_flag;
pub mod staging;

use crate::canton_id::CantonId;

pub use acs::{collect_party_package_ids, export_party_acs, import_party_acs};
pub use offset::{capture_offset_once, current_ledger_offset};
pub use onboarding_flag::{
    ClearOutcome, clear_onboarding_flag, has_onboarding_marker, wait_for_flag_cleared,
};

/// The artifact keys one replication run reads and writes.
///
/// Passed in rather than fixed so each workflow keeps its own key namespace.
/// The add-party run's keys are unchanged by the extraction, so no persisted
/// run is disturbed and a workflow interrupted before it still resumes after.
#[derive(Clone, Copy, Debug)]
pub struct ReplicationArtifacts {
    /// Unscoped. The source's ledger offset, captured before the topology
    /// change so `ExportPartyAcs` can find the party's activation on the target
    /// after it.
    pub export_offset: &'static str,
    /// Scoped to the target participant. Its own pre-activation offset, which
    /// `ClearPartyOnboardingFlag` searches forward from.
    pub pre_activation_offset: &'static str,
    /// Unscoped, durable, never cleared. Written before the synchronizer
    /// disconnect so a retry after a crash knows the participant was left
    /// mid-window and recovers it before touching anything.
    pub import_inflight: &'static str,
}

/// One replication: which party moves onto which participant, and where this
/// run's durable markers live.
#[derive(Clone, Debug)]
pub struct ReplicationTarget {
    /// The party being replicated. Any party — decentralized, external, or
    /// local; the ACS path does not care who owns it.
    pub party_id: CantonId,
    /// The participant gaining the party. Carries Canton's `Onboarding` marker
    /// until the import lands.
    pub target_participant_id: CantonId,
    /// The workflow run these artifacts belong to.
    pub instance_name: String,
    /// The artifact keys above.
    pub artifacts: ReplicationArtifacts,
}

impl ReplicationTarget {
    /// Build a target for `party_id` moving onto `target_participant_id`.
    pub fn new(
        party_id: CantonId,
        target_participant_id: CantonId,
        instance_name: String,
        artifacts: ReplicationArtifacts,
    ) -> Self {
        Self {
            party_id,
            target_participant_id,
            instance_name,
            artifacts,
        }
    }
}
