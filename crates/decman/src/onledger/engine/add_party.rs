//! Add-party driver (design section 6): add a member to a party.
//!
//! Stub. The step bodies land with the add-party agent; every tick logs and
//! returns so the observer loop keeps running.

use anyhow::{Result, bail};
use common::types::{WorkflowKind, WorkflowRun};

use super::{KindDriver, MemberVariant, OnLedger, ProposalExtras, RunMeta, StartRequest, TickCtx};

pub struct AddParty;

pub const COORDINATOR_STEPS: &[&str] = &[
    "GenerateKeys",
    "WaitingForAcceptances",
    "ProposeChanges",
    "AwaitChanges",
    "AwaitReplication",
    "Complete",
];

/// The participant being added.
pub const JOINER_STEPS: &[&str] = &[
    "GenerateKeys",
    "CoSignChanges",
    "SyncAcs",
    "ClearOnboarding",
    "Complete",
];

/// Every current host. `CoSignChanges` captures the export offset first.
pub const MEMBER_STEPS: &[&str] = &["CoSignChanges", "PublishManifest", "Complete"];

impl KindDriver for AddParty {
    fn kind() -> WorkflowKind {
        WorkflowKind::AddParty
    }

    fn coordinator_steps() -> &'static [&'static str] {
        COORDINATOR_STEPS
    }

    fn member_steps(variant: Option<MemberVariant>) -> &'static [&'static str] {
        match variant {
            Some(MemberVariant::Joiner) => JOINER_STEPS,
            Some(MemberVariant::Member) | None => MEMBER_STEPS,
        }
    }

    /// TODO(add-party agent): refuse when the new participant already hosts
    /// the party or is quarantined for it (design section 6 preflight).
    async fn preflight(_ol: &OnLedger, _req: &StartRequest) -> Result<()> {
        Ok(())
    }

    /// TODO(add-party agent): the coordinator's own key material for the
    /// party (`proposerNamespaceFingerprint`, `proposerDamlKeyFingerprint`).
    async fn prepare(_ol: &OnLedger, _req: &StartRequest) -> Result<ProposalExtras> {
        bail!("not implemented: add-party proposer key material (design D6)")
    }

    async fn tick_coordinator(
        _ctx: &TickCtx<'_>,
        run: &WorkflowRun,
        _meta: &RunMeta,
    ) -> Result<()> {
        super::not_implemented(run);
        Ok(())
    }

    async fn tick_member(_ctx: &TickCtx<'_>, run: &WorkflowRun, _meta: &RunMeta) -> Result<()> {
        super::not_implemented(run);
        Ok(())
    }
}
