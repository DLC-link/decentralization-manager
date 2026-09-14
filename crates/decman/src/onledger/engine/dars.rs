//! DARs driver (design sections 6, D8): hash-pinned local uploads.
//!
//! Stub. The step bodies land with the DARs agent; every tick logs and
//! returns so the observer loop keeps running.

use anyhow::{Result, bail};
use common::types::{WorkflowKind, WorkflowRun};

use super::{KindDriver, MemberVariant, OnLedger, ProposalExtras, RunMeta, StartRequest, TickCtx};

pub struct Dars;

pub const COORDINATOR_STEPS: &[&str] = &["WaitingForAcceptances", "AwaitVetting", "Complete"];

pub const MEMBER_STEPS: &[&str] = &["UploadDars", "Complete"];

impl KindDriver for Dars {
    fn kind() -> WorkflowKind {
        WorkflowKind::Dars
    }

    fn coordinator_steps() -> &'static [&'static str] {
        COORDINATOR_STEPS
    }

    fn member_steps(_variant: Option<MemberVariant>) -> &'static [&'static str] {
        MEMBER_STEPS
    }

    /// TODO(DARs agent): upload every file locally, read its main package id
    /// from the participant, and pin it with `onledger::dars::pin_dar_files`.
    async fn prepare(_ol: &OnLedger, _req: &StartRequest) -> Result<ProposalExtras> {
        bail!("not implemented: DAR pins need the main package ids (design D8)")
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
