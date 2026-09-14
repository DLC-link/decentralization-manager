//! Kick driver (design section 6): remove a member from a party.
//!
//! Stub. The step bodies land with the kick agent; every tick logs and
//! returns so the observer loop keeps running. The kicked node is not an
//! invitee and sees the topology proposal as unsolicited.

use anyhow::{Result, bail};
use common::types::{WorkflowKind, WorkflowRun};

use super::{KindDriver, MemberVariant, OnLedger, ProposalExtras, RunMeta, StartRequest, TickCtx};

pub struct Kick;

pub const COORDINATOR_STEPS: &[&str] = &[
    "WaitingForAcceptances",
    "ProposeChanges",
    "AwaitChanges",
    "Complete",
];

pub const MEMBER_STEPS: &[&str] = &["CoSignChanges", "Complete"];

impl KindDriver for Kick {
    fn kind() -> WorkflowKind {
        WorkflowKind::Kick
    }

    fn coordinator_steps() -> &'static [&'static str] {
        COORDINATOR_STEPS
    }

    fn member_steps(_variant: Option<MemberVariant>) -> &'static [&'static str] {
        MEMBER_STEPS
    }

    /// TODO(kick agent): refuse when this node cannot attribute the kicked
    /// member's owner key (`dec_party_participant.owner_key`) or, on a legacy
    /// party, its Daml key (design section 5, legacy branch).
    async fn preflight(_ol: &OnLedger, _req: &StartRequest) -> Result<()> {
        Ok(())
    }

    /// TODO(kick agent): the coordinator's own owner fingerprint for the
    /// party, from the cache or the on-chain lookup (design D4).
    async fn prepare(_ol: &OnLedger, _req: &StartRequest) -> Result<ProposalExtras> {
        bail!("not implemented: kick proposer key material (design D6)")
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
