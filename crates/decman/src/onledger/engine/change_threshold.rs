//! Change-threshold driver (design section 6): re-issue the DND and P2P
//! with a new threshold.
//!
//! Stub. The step bodies land with the change-threshold agent; every tick
//! logs and returns so the observer loop keeps running.

use anyhow::{Result, bail};
use common::types::{WorkflowKind, WorkflowRun};

use super::{KindDriver, MemberVariant, OnLedger, ProposalExtras, RunMeta, StartRequest, TickCtx};

pub struct ChangeThreshold;

pub const COORDINATOR_STEPS: &[&str] = &[
    "WaitingForAcceptances",
    "ProposeChanges",
    "AwaitChanges",
    "Complete",
];

pub const MEMBER_STEPS: &[&str] = &["CoSignChanges", "Complete"];

impl KindDriver for ChangeThreshold {
    fn kind() -> WorkflowKind {
        WorkflowKind::ChangeThreshold
    }

    fn coordinator_steps() -> &'static [&'static str] {
        COORDINATOR_STEPS
    }

    fn member_steps(_variant: Option<MemberVariant>) -> &'static [&'static str] {
        MEMBER_STEPS
    }

    /// TODO(change-threshold agent): refuse when the new threshold equals
    /// the current one.
    async fn preflight(_ol: &OnLedger, _req: &StartRequest) -> Result<()> {
        Ok(())
    }

    /// TODO(change-threshold agent): the coordinator's own owner fingerprint
    /// for the party (design D6).
    async fn prepare(_ol: &OnLedger, _req: &StartRequest) -> Result<ProposalExtras> {
        bail!("not implemented: change-threshold proposer key material (design D6)")
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
