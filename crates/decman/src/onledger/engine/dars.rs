//! DARs driver (design sections 6, D8): hash-pinned local uploads.
//!
//! Coordinator: `WaitingForAcceptances` -> `AwaitVetting` -> `Complete`.
//! `prepare` uploads every file to this participant with its main package
//! id pinned and puts the pins on the proposal; the ticks only read. The run
//! completes when the synchronizer topology store shows every pin vetted on
//! every participant.
//!
//! Member: `UploadDars` -> `Complete`. The first tick exercises
//! `WorkflowProposal_Accept`; the operator then uploads the pinned files
//! through `POST /dars/upload` (`onledger::dars::upload_pinned`), and the
//! run completes when the local participant has vetted every pin.
//!
//! No key material and no decentralized party are involved, so there is
//! nothing to record in `dec_party_participant`.

use anyhow::{Result, bail};
use common::{
    canton_id::CantonId,
    types::{WorkflowKind, WorkflowProgress, WorkflowRun},
};

use crate::onledger::{
    daml::codec::WorkflowProposalRecord,
    dars,
    proposals::{self, Acceptance, ActiveProposal},
};

use super::{
    COMPLETE_STEP, KindDriver, MemberVariant, OnLedger, PreflightRejected, ProposalExtras,
    ProposerKeyMaterial, RunMeta, StartRequest, TickCtx, WAITING_FOR_ACCEPTANCES_STEP, accept_args,
    advance_step, complete_run, fail_run,
};

pub struct Dars;

pub const AWAIT_VETTING_STEP: &str = "AwaitVetting";
pub const UPLOAD_DARS_STEP: &str = "UploadDars";

pub const COORDINATOR_STEPS: &[&str] = &[
    WAITING_FOR_ACCEPTANCES_STEP,
    AWAIT_VETTING_STEP,
    COMPLETE_STEP,
];

pub const MEMBER_STEPS: &[&str] = &[UPLOAD_DARS_STEP, COMPLETE_STEP];

// ---------------------------------------------------------------------------
// Pure decision helpers
// ---------------------------------------------------------------------------

/// Whether every invitee has a counted acceptance (section 6: dars needs
/// every invitee).
pub fn all_invitees_accepted(invitees: &[CantonId], counted: &[Acceptance]) -> bool {
    acceptance_gate(invitees, counted) == AcceptanceGate::Ready
}

/// Whether `me` already accepted, so the tick does not exercise `Accept`
/// twice.
pub fn has_my_acceptance(acceptances: &[Acceptance], me: &CantonId) -> bool {
    acceptances.iter().any(|a| a.record.acceptor == *me)
}

/// The proposal's participants as Canton ids. A participant that does not
/// parse is an error: the run cannot know who must vet.
pub fn participants_of(proposal: &WorkflowProposalRecord) -> Result<Vec<CantonId>> {
    proposal
        .participants
        .iter()
        .map(|p| CantonId::parse(p).map_err(|e| anyhow::anyhow!("participant `{p}`: {e}")))
        .collect()
}

/// The `WaitingForAcceptances` decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AcceptanceGate {
    /// Every invitee accepted: move on.
    Ready,
    /// Some invitees have not accepted yet: stay.
    Waiting { missing: Vec<CantonId> },
}

/// Which invitees still have to accept.
pub fn acceptance_gate(invitees: &[CantonId], counted: &[Acceptance]) -> AcceptanceGate {
    let missing: Vec<CantonId> = invitees
        .iter()
        .filter(|i| !counted.iter().any(|a| a.record.acceptor == **i))
        .cloned()
        .collect();
    if missing.is_empty() {
        AcceptanceGate::Ready
    } else {
        AcceptanceGate::Waiting { missing }
    }
}

// ---------------------------------------------------------------------------
// Ledger glue
// ---------------------------------------------------------------------------

/// The proposal of this run in the tick snapshot. `drive` already stopped
/// the run when it vanished, so `None` here only means a race with cancel;
/// the next tick reconciles it.
fn proposal_of<'a>(ctx: &'a TickCtx<'_>, meta: &RunMeta) -> Option<&'a ActiveProposal> {
    ctx.proposals.proposal(&meta.proposal_cid)
}

/// Design D5 step 5 before the one ledger write a member makes: the row is
/// still in progress and the proposal is still active and unexpired, read
/// fresh rather than from the tick snapshot.
async fn recheck_before_accept(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
) -> Result<bool> {
    use crate::db::schema::SchemaRead;
    let Some(fresh) = ctx.db().get_workflow_run(&run.instance_name).await? else {
        return Ok(false);
    };
    if fresh.status != WorkflowProgress::InProgress {
        return Ok(false);
    }
    let Some(proposal) = proposals::read_proposal(ctx.client, &meta.proposal_cid).await? else {
        return Ok(false);
    };
    Ok(!ctx.is_expired(&proposal))
}

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

    /// Refuse a request without files, with undecodable files, or with a
    /// file that is not a DAR, before anything is uploaded.
    async fn preflight(_ol: &OnLedger, req: &StartRequest) -> Result<()> {
        let StartRequest::Dars { dar_files, .. } = req else {
            return Ok(());
        };
        if dar_files.is_empty() {
            return Err(PreflightRejected::new("no DAR files to distribute").into());
        }
        let files = dars::decode_dar_files(dar_files)
            .map_err(|e| PreflightRejected::new(format!("{e:#}")))?;
        dars::main_package_ids(&files).map_err(|e| PreflightRejected::new(format!("{e:#}")))?;
        Ok(())
    }

    /// Upload every file to this participant with its main package id
    /// pinned, then pin the files on the proposal (design D8). The bytes
    /// exist only here, so the local upload cannot wait for a tick.
    async fn prepare(ol: &OnLedger, req: &StartRequest) -> Result<ProposalExtras> {
        let StartRequest::Dars { dar_files, .. } = req else {
            bail!("Dars::prepare called with a {} request", req.kind());
        };
        let files = dars::decode_dar_files(dar_files)?;
        let ids = dars::main_package_ids(&files)?;
        for (filename, bytes) in &files {
            let expected = ids.get(filename).map(String::as_str);
            dars::upload_and_vet_locally(ol.config(), filename, bytes, expected).await?;
        }
        Ok(ProposalExtras {
            dar_pins: dars::pin_dar_files(&files, &ids)?,
            ..ProposalExtras::default()
        })
    }

    async fn tick_coordinator(ctx: &TickCtx<'_>, run: &WorkflowRun, meta: &RunMeta) -> Result<()> {
        let Some(proposal) = proposal_of(ctx, meta) else {
            return Ok(());
        };
        let db = ctx.db();
        match run.current_step.as_str() {
            WAITING_FOR_ACCEPTANCES_STEP => {
                let raw = ctx.proposals.acceptances_for(&proposal.contract_id);
                let counted =
                    match proposals::counted_acceptances_verified(ctx.ol.config(), proposal, &raw)
                        .await
                    {
                        Ok(counted) => counted,
                        // Only a conflicting acceptance set reaches here; a
                        // hosting read failure is logged and not counted.
                        Err(e) => {
                            let msg = format!("acceptances conflict: {e:#}");
                            fail_run(db, run, &msg).await?;
                            finish_quietly(ctx, meta, false, Some(msg)).await;
                            return Ok(());
                        }
                    };
                match acceptance_gate(&proposal.record.invitees, &counted) {
                    AcceptanceGate::Ready => advance_step(db, run, AWAIT_VETTING_STEP).await,
                    AcceptanceGate::Waiting { missing } => {
                        tracing::debug!(
                            instance = %run.instance_name,
                            missing = ?missing,
                            "waiting for acceptances"
                        );
                        Ok(())
                    }
                }
            }
            AWAIT_VETTING_STEP => {
                let participants = participants_of(&proposal.record)?;
                let report = dars::unvetted_pins_by_participant(
                    ctx.ol.config(),
                    &participants,
                    &proposal.record.dar_pins,
                )
                .await?;
                if !dars::all_vetted(&report) {
                    tracing::debug!(
                        instance = %run.instance_name,
                        pending = %dars::describe_unvetted(&report),
                        "waiting for vetting"
                    );
                    return Ok(());
                }
                // Complete the row first, then finish, like every other
                // driver. `reconcile` fails a coordinator whose proposal is
                // gone, so a crash after a successful finish but before the
                // row is written would turn this success into a failure.
                // A finish that fails instead leaves the proposal to expire,
                // and members complete from the vetting they can already see.
                complete_run(db, run).await?;
                if let Err(e) = proposals::finish(ctx.client, &meta.proposal_cid, true, None).await
                {
                    tracing::warn!(
                        instance = %run.instance_name,
                        error = %e,
                        "Dars run completed but WorkflowProposal_Finish failed; it expires on its own"
                    );
                }
                Ok(())
            }
            COMPLETE_STEP => Ok(()),
            other => bail!("unknown Dars coordinator step {other}"),
        }
    }

    async fn tick_member(ctx: &TickCtx<'_>, run: &WorkflowRun, meta: &RunMeta) -> Result<()> {
        let Some(proposal) = proposal_of(ctx, meta) else {
            return Ok(());
        };
        let db = ctx.db();
        match run.current_step.as_str() {
            UPLOAD_DARS_STEP => {
                let me = &ctx.identity.node_party;
                if !has_my_acceptance(&ctx.proposals.acceptances_for(&proposal.contract_id), me) {
                    if !recheck_before_accept(ctx, run, meta).await? {
                        tracing::debug!(
                            instance = %run.instance_name,
                            "run or proposal changed under the tick; not accepting"
                        );
                        return Ok(());
                    }
                    // Dars carries no key material: every field is None.
                    let args = accept_args(ctx.identity, &ProposerKeyMaterial::default(), None);
                    let cid = proposals::accept(ctx.client, &meta.proposal_cid, &args).await?;
                    tracing::info!(
                        instance = %run.instance_name,
                        acceptance = %cid,
                        pins = proposal.record.dar_pins.len(),
                        "DAR pins accepted; upload the files through POST /dars/upload"
                    );
                    return Ok(());
                }
                let missing =
                    dars::unvetted_pins_locally(ctx.ol.config(), &proposal.record.dar_pins).await?;
                if missing.is_empty() {
                    return complete_run(db, run).await;
                }
                tracing::debug!(
                    instance = %run.instance_name,
                    missing = ?missing,
                    "waiting for the operator to upload the pinned DARs"
                );
                Ok(())
            }
            COMPLETE_STEP => Ok(()),
            other => bail!("unknown Dars member step {other}"),
        }
    }
}

/// `WorkflowProposal_Finish` after the row is already terminal: a failure
/// here is logged, because the proposal expires on its own.
async fn finish_quietly(ctx: &TickCtx<'_>, meta: &RunMeta, succeeded: bool, error: Option<String>) {
    if let Err(e) = proposals::finish(ctx.client, &meta.proposal_cid, succeeded, error).await {
        tracing::warn!(
            proposal = %meta.proposal_cid,
            error = %e,
            "WorkflowProposal_Finish failed; the proposal expires on its own"
        );
    }
}

#[cfg(test)]
mod tests {
    use common::api::DarFile;

    use super::*;
    use crate::onledger::{
        daml::codec::{WorkflowAcceptanceRecord, tests::party},
        engine::steps_for,
    };

    fn acceptance(who: &str) -> Acceptance {
        Acceptance {
            contract_id: format!("00acc-{who}"),
            offset: 1,
            record: WorkflowAcceptanceRecord {
                proposal: "00p".into(),
                proposer: party("node-a"),
                acceptor: party(who),
                observers: vec![],
                run_id: "dars-1".into(),
                participant_id: "p".into(),
                namespace_fingerprint: None,
                signing_public_key_hex: None,
                daml_key_fingerprint: None,
                member_party: None,
                accepted_at: 1,
            },
        }
    }

    #[test]
    fn steps_match_design_section_6() {
        assert_eq!(
            steps_for(
                WorkflowKind::Dars,
                common::types::WorkflowRole::Coordinator,
                None
            ),
            COORDINATOR_STEPS
        );
        assert_eq!(
            steps_for(WorkflowKind::Dars, common::types::WorkflowRole::Peer, None),
            MEMBER_STEPS
        );
        assert_eq!(COORDINATOR_STEPS.last(), Some(&COMPLETE_STEP));
        assert_eq!(MEMBER_STEPS.last(), Some(&COMPLETE_STEP));
        assert_eq!(COORDINATOR_STEPS[0], WAITING_FOR_ACCEPTANCES_STEP);
        assert_eq!(COORDINATOR_STEPS[1], AWAIT_VETTING_STEP);
        assert_eq!(MEMBER_STEPS[0], UPLOAD_DARS_STEP);
    }

    #[test]
    fn every_invitee_must_accept() {
        let invitees = vec![party("node-b"), party("node-c")];
        let both = vec![acceptance("node-b"), acceptance("node-c")];
        assert!(all_invitees_accepted(&invitees, &both));
        assert_eq!(acceptance_gate(&invitees, &both), AcceptanceGate::Ready);

        let one = vec![acceptance("node-b")];
        assert!(!all_invitees_accepted(&invitees, &one));
        assert_eq!(
            acceptance_gate(&invitees, &one),
            AcceptanceGate::Waiting {
                missing: vec![party("node-c")]
            }
        );

        // A stranger's acceptance does not stand in for an invitee.
        let stranger = vec![acceptance("node-b"), acceptance("node-z")];
        assert!(!all_invitees_accepted(&invitees, &stranger));

        assert!(all_invitees_accepted(&[], &[]));
    }

    #[test]
    fn my_acceptance_is_found_by_acceptor() {
        let list = vec![acceptance("node-b")];
        assert!(has_my_acceptance(&list, &party("node-b")));
        assert!(!has_my_acceptance(&list, &party("node-c")));
        assert!(!has_my_acceptance(&[], &party("node-b")));
    }

    #[test]
    fn participants_must_parse() {
        let mut record = crate::onledger::daml::codec::tests::proposal_full();
        assert_eq!(participants_of(&record).expect("ids").len(), 3);
        record.participants.push("not-a-canton-id".into());
        let err = participants_of(&record).expect_err("garbage");
        assert!(err.to_string().contains("not-a-canton-id"), "{err}");
    }

    #[tokio::test]
    async fn preflight_refuses_empty_undecodable_and_non_dar_files() {
        let ol = OnLedger::placeholder();
        let req = |files: Vec<DarFile>| StartRequest::Dars {
            dar_files: files,
            peer_ids: vec![],
            instance_name: "dars-1".into(),
        };

        let err = Dars::preflight(&ol, &req(vec![])).await.expect_err("empty");
        assert!(err.downcast_ref::<PreflightRejected>().is_some(), "{err}");

        let err = Dars::preflight(
            &ol,
            &req(vec![DarFile {
                filename: "a.dar".into(),
                data: "!!".into(),
            }]),
        )
        .await
        .expect_err("bad base64");
        assert!(err.downcast_ref::<PreflightRejected>().is_some(), "{err}");
        assert!(err.to_string().contains("a.dar"), "{err}");

        let err = Dars::preflight(
            &ol,
            &req(vec![DarFile {
                filename: "junk.dar".into(),
                data: base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    b"PK not a dar",
                ),
            }]),
        )
        .await
        .expect_err("not a DAR");
        assert!(err.downcast_ref::<PreflightRejected>().is_some(), "{err}");
        assert!(err.to_string().contains("junk.dar"), "{err}");

        Dars::preflight(
            &ol,
            &req(vec![DarFile {
                filename: dars::COORDINATION_DAR_FILENAME.into(),
                data: base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    dars::embedded_coordination_dar(),
                ),
            }]),
        )
        .await
        .expect("a real DAR passes");

        // Other kinds are not this driver's business.
        Dars::preflight(
            &ol,
            &StartRequest::Onboarding {
                party_id_prefix: "x".into(),
                peer_ids: vec![],
                threshold: None,
                instance_name: "o".into(),
            },
        )
        .await
        .expect("ignored");
    }
}
