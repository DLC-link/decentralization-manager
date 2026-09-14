//! Onboarding driver (design section 6): create a new decentralized party.
//!
//! Stub. The step bodies land with the onboarding agent; every tick logs
//! and returns so the observer loop keeps running.

use anyhow::{Result, bail};
use common::types::{WorkflowKind, WorkflowRun};

use super::{KindDriver, MemberVariant, OnLedger, ProposalExtras, RunMeta, StartRequest, TickCtx};

pub struct Onboarding;

pub const COORDINATOR_STEPS: &[&str] = &[
    "GenerateKeys",
    "WaitingForAcceptances",
    "ProposeNamespace",
    "AwaitNamespace",
    "ProposeParty",
    "AwaitParty",
    "Complete",
];

pub const MEMBER_STEPS: &[&str] = &["GenerateKeys", "CoSignNamespace", "CoSignParty", "Complete"];

impl KindDriver for Onboarding {
    fn kind() -> WorkflowKind {
        WorkflowKind::Onboarding
    }

    fn coordinator_steps() -> &'static [&'static str] {
        COORDINATOR_STEPS
    }

    fn member_steps(_variant: Option<MemberVariant>) -> &'static [&'static str] {
        MEMBER_STEPS
    }

    /// TODO(onboarding agent): generate the dual-usage `{prefix}-key`, publish
    /// its root `NamespaceDelegation`, and return the fingerprint, the
    /// serialized public key as hex, and the Daml key fingerprint (design D4).
    async fn prepare(_ol: &OnLedger, _req: &StartRequest) -> Result<ProposalExtras> {
        bail!("not implemented: onboarding proposer key material (design D4)")
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
