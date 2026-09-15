//! Change-threshold driver (design section 5 "Change threshold", section 6,
//! D5): re-issue the DND and then the P2P of an existing party with a new
//! threshold, so that Canton authorizes later changes with the new count.
//!
//! The coordinator proposes; the members co-sign. Kick and change-threshold
//! are the quorum kinds: a mapping needs `max(previousThreshold, threshold)`
//! owner signatures, the proposer's included. The coordinator therefore
//! leaves `WaitingForAcceptances` before every invitee answers, and a late
//! acceptor still co-signs while the proposal is pending. The quorum-aware
//! decline rule runs in `engine::reconcile` before every tick.
//!
//! A tick does one bounded piece of work for the row's current step and
//! returns. A wait is a re-tick, not a sleep: a sleeping tick holds the run
//! lock and blocks cancel and retry.

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use canton_proto_rs::com::digitalasset::canton::protocol::v30::{
    DecentralizedNamespaceDefinition, PartyToParticipant,
};
use common::{
    canton_id::CantonId,
    types::{WorkflowKind, WorkflowProgress, WorkflowRole, WorkflowRun},
};
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    db::{
        rows::{DecPartyRow, ProposalDecision},
        schema::{Commitable, SchemaRead, SchemaWrite},
    },
    onledger::{
        daml::codec::WorkflowProposalRecord,
        identity, keys, now_secs,
        proposals::{self, Acceptance, ActiveProposal},
        topology::{self, AcceptedMapping, CosignOutcome, PendingProposal},
        validation::{self, Expectations, HeadState},
    },
    utils,
};

use super::{
    KindDriver, MemberVariant, OnLedger, PreflightRejected, ProposalExtras, ProposerKeyMaterial,
    RunMeta, StartRequest, TickCtx, WAITING_FOR_ACCEPTANCES_STEP, acceptances_needed, advance_step,
    complete_run, fail_run, pin_topology_hash,
};

pub struct ChangeThreshold;

pub const COORDINATOR_STEPS: &[&str] = &[
    "WaitingForAcceptances",
    "ProposeChanges",
    "AwaitChanges",
    "Complete",
];

pub const MEMBER_STEPS: &[&str] = &["CoSignChanges", "Complete"];

const PROPOSE_CHANGES_STEP: &str = "ProposeChanges";
const AWAIT_CHANGES_STEP: &str = "AwaitChanges";
const CO_SIGN_CHANGES_STEP: &str = "CoSignChanges";

/// `RunMeta.topology_hashes` keys, one per mapping the run pins.
const DND_HASH: &str = "dnd";
const P2P_HASH: &str = "p2p";

// ---------------------------------------------------------------------------
// Pure decisions
// ---------------------------------------------------------------------------

/// The proposal fields every step reads. Converted once, and failed closed
/// on a gap: without the party, both thresholds, and both base serials the
/// change cannot be validated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Intent {
    pub party: CantonId,
    pub threshold: u32,
    pub previous_threshold: u32,
    pub dnd_base: u32,
    pub p2p_base: u32,
}

fn positive_field(value: Option<i64>, name: &str) -> Result<u32> {
    let value = value.with_context(|| format!("the proposal carries no {name}"))?;
    u32::try_from(value)
        .ok()
        .filter(|v| *v >= 1)
        .with_context(|| format!("the proposal {name} {value} is not a positive count"))
}

/// Read the [`Intent`] of an accepted change-threshold proposal.
///
/// # Errors
/// Returns an error when a field is missing or not a positive count.
pub fn intent_of(record: &WorkflowProposalRecord) -> Result<Intent> {
    let party = record
        .dec_party_id
        .as_deref()
        .context("the proposal names no decPartyId")?;
    let party = CantonId::parse(party).with_context(|| format!("decPartyId `{party}`"))?;
    Ok(Intent {
        party,
        threshold: positive_field(record.threshold, "threshold")?,
        previous_threshold: positive_field(record.previous_threshold, "previousThreshold")?,
        dnd_base: positive_field(record.dnd_base_serial, "dndBaseSerial")?,
        p2p_base: positive_field(record.p2p_base_serial, "p2pBaseSerial")?,
    })
}

/// Where an accepted mapping stands against the base serial the proposal
/// recorded (design D5 steps 1, 7, and 8).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MappingProgress {
    /// Still at the base serial: the change is not authorized yet.
    Pending,
    /// At `base + 1` with the new threshold, but `valid_from` is still in
    /// the future on this node.
    Authorized,
    /// At `base + 1` with the new threshold, and effective here.
    Effective,
    /// At `base + 1` with another threshold: another change won the serial.
    Foreign(u32),
    /// Neither the base nor `base + 1`: the topology moved under the run.
    Moved(u32),
}

/// Classify an accepted serial. `has_threshold` says whether the mapping
/// carries the proposed threshold; the serial alone cannot tell the run's
/// own change from a concurrent one that took the same serial.
pub fn mapping_progress(
    serial: u32,
    effective: bool,
    base: u32,
    has_threshold: bool,
) -> MappingProgress {
    match serial.checked_sub(base) {
        Some(0) => MappingProgress::Pending,
        Some(1) if !has_threshold => MappingProgress::Foreign(serial),
        Some(1) if effective => MappingProgress::Effective,
        Some(1) => MappingProgress::Authorized,
        _ => MappingProgress::Moved(serial),
    }
}

/// Whether a DND carries `threshold`.
pub fn dnd_has_threshold(dnd: &DecentralizedNamespaceDefinition, threshold: u32) -> bool {
    u32::try_from(dnd.threshold).ok() == Some(threshold)
}

/// Whether a P2P carries `threshold` as both its hosting threshold and its
/// party signing-key threshold (design section 5).
pub fn p2p_has_threshold(p2p: &PartyToParticipant, threshold: u32) -> bool {
    p2p.threshold == threshold
        && p2p
            .party_signing_keys
            .as_ref()
            .is_some_and(|keys| keys.threshold == threshold)
}

fn dnd_progress(
    dnd: &AcceptedMapping<DecentralizedNamespaceDefinition>,
    intent: &Intent,
    now: i64,
) -> MappingProgress {
    mapping_progress(
        dnd.serial,
        dnd.is_effective(now),
        intent.dnd_base,
        dnd_has_threshold(&dnd.mapping, intent.threshold),
    )
}

fn p2p_progress(
    p2p: &AcceptedMapping<PartyToParticipant>,
    intent: &Intent,
    now: i64,
) -> MappingProgress {
    mapping_progress(
        p2p.serial,
        p2p.is_effective(now),
        intent.p2p_base,
        p2p_has_threshold(&p2p.mapping, intent.threshold),
    )
}

fn describe_moved(label: &str, progress: MappingProgress, base: u32) -> String {
    match progress {
        MappingProgress::Foreign(serial) => format!(
            "topology moved: the {label} at serial {serial} carries another threshold; a \
             concurrent change took serial {base} + 1"
        ),
        MappingProgress::Moved(serial) => format!(
            "topology moved: the {label} is at serial {serial}, but the proposal recorded base \
             serial {base}"
        ),
        other => format!("the {label} is {other:?}"),
    }
}

/// Section 6 preflight: the new threshold fits `1..=owners` and differs
/// from the one in force.
///
/// # Errors
/// Returns the 409 text when a gate fails.
pub fn check_new_threshold(
    new: i32,
    previous: u32,
    owners: usize,
) -> Result<u32, PreflightRejected> {
    let max = u32::try_from(owners).unwrap_or(u32::MAX);
    let Some(new) = u32::try_from(new).ok().filter(|n| (1..=max).contains(n)) else {
        return Err(PreflightRejected::new(format!(
            "threshold {new} is outside 1..={owners} for the {owners} owner(s) of the party"
        )));
    };
    if new == previous {
        return Err(PreflightRejected::new(format!(
            "the party threshold is already {previous}"
        )));
    }
    Ok(new)
}

/// Section 6 quorum: the coordinator proposes once the counted acceptances
/// reach [`acceptances_needed`], because its own signature is the last one.
pub fn quorum_reached(counted: usize, previous_threshold: u32, new_threshold: u32) -> bool {
    u32::try_from(counted).unwrap_or(u32::MAX)
        >= acceptances_needed(previous_threshold, new_threshold)
}

/// Design D5 steps 2 and 4: the pending proposals a member may consider.
/// `ADD_REPLACE`, signed by the proposer's owner key, not yet by this node,
/// and at `accepted + 1`. A proposal this node signed is not a match, so a
/// signed proposal that waits for other owners reads as "nothing to do",
/// not as a mismatch.
pub fn matching_proposals<'a, M>(
    pending: &'a [PendingProposal<M>],
    proposer_fingerprint: &str,
    own_fingerprints: &BTreeSet<String>,
    accepted_serial: u32,
) -> Vec<&'a PendingProposal<M>> {
    pending
        .iter()
        .filter(|p| {
            p.is_add_replace()
                && p.is_signed_by(proposer_fingerprint)
                && !own_fingerprints.iter().any(|fp| p.is_signed_by(fp))
                && Some(p.serial) == accepted_serial.checked_add(1)
        })
        .collect()
}

/// Design D5 step 5: a mapping pinned to one hash is never signed for a
/// second one. Returns the refusal, or `None` when the pin agrees or is
/// absent.
pub fn pin_conflict(pinned: Option<&str>, hash_hex: &str) -> Option<String> {
    match pinned {
        Some(p) if p != hash_hex => Some(format!(
            "proposal {hash_hex} differs from the hash {p} this run pinned earlier"
        )),
        _ => None,
    }
}

/// The one owner key this node signs with. Zero or several keys fail
/// closed, because `signed_by = []` lets Canton pick and the proposal must
/// name the key the members check for.
///
/// # Errors
/// Returns an error unless exactly one fingerprint is present.
pub fn single_owner(owners: &BTreeSet<String>, party: &CantonId) -> Result<String> {
    let mut it = owners.iter();
    match (it.next(), it.next()) {
        (Some(fp), None) => Ok(fp.clone()),
        (None, _) => {
            bail!("this node holds no owner key of {party}; only an owner can change its threshold")
        }
        (Some(_), Some(_)) => bail!(
            "this node holds {} owner keys of {party} ({owners:?}); cannot tell which one signs \
             the proposal",
            owners.len()
        ),
    }
}

// ---------------------------------------------------------------------------
// Shared glue
// ---------------------------------------------------------------------------

/// One tick of one run, with the proposal fields already converted.
struct Step<'a> {
    ctx: &'a TickCtx<'a>,
    run: &'a WorkflowRun,
    meta: &'a RunMeta,
    proposal: &'a ActiveProposal,
    intent: Intent,
}

impl Step<'_> {
    fn db(&self) -> &SqlitePool {
        self.ctx.db()
    }

    fn config(&self) -> &NodeConfig {
        self.ctx.ol.config()
    }

    fn sync_id(&self) -> &str {
        &self.ctx.sync_id
    }

    fn namespace(&self) -> String {
        self.intent.party.namespace.to_hex()
    }

    async fn read_dnd(&self) -> Result<Option<AcceptedMapping<DecentralizedNamespaceDefinition>>> {
        topology::read_accepted_dnd(self.config(), self.sync_id(), &self.namespace()).await
    }

    async fn read_p2p(&self) -> Result<Option<AcceptedMapping<PartyToParticipant>>> {
        topology::read_accepted_p2p(self.config(), self.sync_id(), &self.intent.party).await
    }

    /// Design D5 step 5: never write from a cached match. The proposal must
    /// still be active and the row in progress; a member also needs its
    /// accepted decision. `false` skips the write until the next tick.
    async fn is_live(&self) -> Result<bool> {
        let cid = &self.meta.proposal_cid;
        if proposals::read_proposal(self.ctx.client, cid)
            .await?
            .is_none()
        {
            tracing::info!(instance = %self.run.instance_name, proposal = %cid, "proposal is no longer active; no write this tick");
            return Ok(false);
        }
        let Some(row) = self.db().get_workflow_run(&self.run.instance_name).await? else {
            return Ok(false);
        };
        if row.status != WorkflowProgress::InProgress {
            tracing::info!(instance = %self.run.instance_name, status = %row.status, "run is not in progress; no write this tick");
            return Ok(false);
        }
        if self.run.role == WorkflowRole::Peer {
            let accepted = self
                .db()
                .get_proposal_decision(cid)
                .await?
                .is_some_and(|d| matches!(d.decision, ProposalDecision::Accepted));
            if !accepted {
                tracing::warn!(instance = %self.run.instance_name, proposal = %cid, "no accepted decision for the proposal; not signing");
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Counted acceptances (design D6), with the acceptors' key material
    /// recorded so a later kick finds it in the local cache (design M6).
    ///
    /// # Errors
    /// Returns an error on a duplicate acceptor, which fails closed.
    async fn counted_acceptances(&self) -> Result<Vec<Acceptance>> {
        let raw = self
            .ctx
            .proposals
            .acceptances_for(&self.proposal.contract_id);
        let counted =
            proposals::counted_acceptances_verified(self.config(), self.proposal, &raw).await?;
        for a in &counted {
            let Ok(participant) = CantonId::parse(&a.record.participant_id) else {
                continue;
            };
            // A failed cache write must not stall the run: the cache heals on
            // the next tick or the next parties refresh.
            if let Err(e) = keys::record_member_keys(
                self.db(),
                &self.intent.party,
                &participant,
                a.record.namespace_fingerprint.as_deref(),
                a.record.daml_key_fingerprint.as_deref(),
            )
            .await
            {
                tracing::warn!(participant = %participant, error = %e, "member keys not recorded");
            }
        }
        Ok(counted)
    }
}

/// Write the new threshold into the `dec_party` cache so the UI shows it
/// before the next parties refresh. A party without a cached row is skipped:
/// the refresh creates the row with the on-chain value.
async fn cache_threshold(db: &SqlitePool, party: &CantonId, threshold: u32) -> Result<()> {
    let party_id = party.to_string();
    let Some(row) = db
        .get_dec_parties_by_prefix(&party.prefix)
        .await?
        .into_iter()
        .find(|r| r.party_id == party_id)
    else {
        tracing::warn!(party = %party, "no cached party row; threshold not cached");
        return Ok(());
    };
    if row.threshold == i64::from(threshold) {
        return Ok(());
    }
    let mut tx = db.begin_transaction().await?;
    tx.upsert_dec_party(&DecPartyRow {
        threshold: i64::from(threshold),
        updated_at: now_secs(),
        ..row
    })
    .await?;
    Commitable::commit(tx).await?;
    tracing::info!(party = %party, threshold, "party threshold cached");
    Ok(())
}

/// Append a pinned hash to the `proposal_decisions` row (design D11). A
/// hash already present is left alone.
///
/// TODO(engine): move next to `engine::pin_topology_hash` in `engine/mod.rs`
/// once that file is open for edits.
async fn pin_decision_hash(db: &SqlitePool, proposal_cid: &str, hash_hex: &str) -> Result<()> {
    let Some(entry) = db.get_proposal_decision(proposal_cid).await? else {
        bail!("no proposal_decisions row for {proposal_cid}");
    };
    if entry.pinned_hashes.iter().any(|h| h == hash_hex) {
        return Ok(());
    }
    let mut pinned = entry.pinned_hashes;
    pinned.push(hash_hex.to_string());
    let mut tx = db.begin_transaction().await?;
    tx.set_proposal_pinned_hashes(proposal_cid, &pinned).await?;
    Commitable::commit(tx).await
}

// ---------------------------------------------------------------------------
// Coordinator
// ---------------------------------------------------------------------------

/// Fail the coordinator row and tell the members why through the outcome.
/// A failed `Finish` is logged: nobody ticks a failed run again, and the
/// proposal then expires on its own.
async fn fail_coordinator(step: &Step<'_>, reason: &str) -> Result<()> {
    fail_run(step.db(), step.run, reason).await?;
    if let Err(e) = proposals::finish(
        step.ctx.client,
        &step.meta.proposal_cid,
        false,
        Some(reason.to_string()),
    )
    .await
    {
        tracing::warn!(proposal = %step.meta.proposal_cid, error = %e, "WorkflowProposal_Finish failed");
    }
    Ok(())
}

async fn coordinator_wait_for_acceptances(step: &Step<'_>) -> Result<()> {
    let counted = match step.counted_acceptances().await {
        Ok(counted) => counted,
        Err(e) => return fail_coordinator(step, &format!("{e:#}")).await,
    };
    let intent = &step.intent;
    if quorum_reached(counted.len(), intent.previous_threshold, intent.threshold) {
        return advance_step(step.db(), step.run, PROPOSE_CHANGES_STEP).await;
    }
    tracing::debug!(
        instance = %step.run.instance_name,
        counted = counted.len(),
        needed = acceptances_needed(intent.previous_threshold, intent.threshold),
        "waiting for acceptances"
    );
    Ok(())
}

async fn coordinator_propose_dnd(step: &Step<'_>) -> Result<()> {
    // The pin landed but the step advance did not: nothing to propose again.
    if step.meta.topology_hashes.contains_key(DND_HASH) {
        return advance_step(step.db(), step.run, AWAIT_CHANGES_STEP).await;
    }
    let Some(dnd) = step.read_dnd().await? else {
        return fail_coordinator(
            step,
            &format!(
                "{} has no accepted DecentralizedNamespaceDefinition",
                step.intent.party
            ),
        )
        .await;
    };
    let progress = dnd_progress(&dnd, &step.intent, step.ctx.now_micros);
    match progress {
        MappingProgress::Pending => {
            if !step.is_live().await? {
                return Ok(());
            }
            let mapping = topology::build_change_threshold_dnd(&dnd.mapping, step.intent.threshold);
            let proposed = topology::propose_mapping(
                step.config(),
                step.sync_id(),
                mapping,
                step.intent.dnd_base + 1,
            )
            .await?;
            pin_topology_hash(
                step.db(),
                &step.run.instance_name,
                DND_HASH,
                &proposed.hash_hex,
            )
            .await?;
            advance_step(step.db(), step.run, AWAIT_CHANGES_STEP).await
        }
        // A restart between `propose_mapping` and the pin: the members
        // carried the DND through without this node's record of it.
        MappingProgress::Authorized | MappingProgress::Effective => {
            advance_step(step.db(), step.run, AWAIT_CHANGES_STEP).await
        }
        MappingProgress::Foreign(_) | MappingProgress::Moved(_) => {
            fail_coordinator(step, &describe_moved("DND", progress, step.intent.dnd_base)).await
        }
    }
}

async fn coordinator_await_changes(step: &Step<'_>) -> Result<()> {
    let intent = &step.intent;
    let now = step.ctx.now_micros;
    let Some(dnd) = step.read_dnd().await? else {
        return fail_coordinator(
            step,
            &format!(
                "{} has no accepted DecentralizedNamespaceDefinition",
                intent.party
            ),
        )
        .await;
    };
    let progress = dnd_progress(&dnd, intent, now);
    match progress {
        MappingProgress::Pending | MappingProgress::Authorized => {
            tracing::debug!(instance = %step.run.instance_name, ?progress, "DND not effective yet");
            return Ok(());
        }
        MappingProgress::Effective => {}
        MappingProgress::Foreign(_) | MappingProgress::Moved(_) => {
            return fail_coordinator(step, &describe_moved("DND", progress, intent.dnd_base)).await;
        }
    }

    // The P2P is proposed only after the DND is effective: Canton
    // authorizes a P2P under a decentralized namespace with the stored DND.
    let Some(p2p) = step.read_p2p().await? else {
        return fail_coordinator(
            step,
            &format!("{} has no accepted PartyToParticipant", intent.party),
        )
        .await;
    };
    let progress = p2p_progress(&p2p, intent, now);
    match progress {
        MappingProgress::Pending if step.meta.topology_hashes.contains_key(P2P_HASH) => {
            tracing::debug!(instance = %step.run.instance_name, "P2P proposed; waiting for the owners");
            Ok(())
        }
        MappingProgress::Pending => {
            if !step.is_live().await? {
                return Ok(());
            }
            let mapping = topology::build_change_threshold_p2p(&p2p.mapping, intent.threshold);
            let proposed = topology::propose_mapping(
                step.config(),
                step.sync_id(),
                mapping,
                intent.p2p_base + 1,
            )
            .await?;
            pin_topology_hash(
                step.db(),
                &step.run.instance_name,
                P2P_HASH,
                &proposed.hash_hex,
            )
            .await
        }
        MappingProgress::Authorized => {
            tracing::debug!(instance = %step.run.instance_name, "P2P authorized, not effective yet");
            Ok(())
        }
        MappingProgress::Effective => {
            cache_threshold(step.db(), &intent.party, intent.threshold).await?;
            // The row first: it is the operator's view. The outcome is for
            // members that have not observed the P2P themselves; they
            // complete from the P2P anyway, so a failed Finish only leaves
            // the proposal to expire.
            complete_run(step.db(), step.run).await?;
            if let Err(e) =
                proposals::finish(step.ctx.client, &step.meta.proposal_cid, true, None).await
            {
                tracing::warn!(proposal = %step.meta.proposal_cid, error = %e, "WorkflowProposal_Finish failed");
            }
            Ok(())
        }
        MappingProgress::Foreign(_) | MappingProgress::Moved(_) => {
            fail_coordinator(step, &describe_moved("P2P", progress, intent.p2p_base)).await
        }
    }
}

// ---------------------------------------------------------------------------
// Member
// ---------------------------------------------------------------------------

/// One mapping a member co-signs: which pending proposals to consider and
/// which section-5 rule applies.
struct CosignTarget<'a, M> {
    label: &'static str,
    hash_key: &'static str,
    head: HeadState,
    pending: &'a [PendingProposal<M>],
    accepted_serial: u32,
    validate: fn(&PendingProposal<M>, &Expectations, Option<u32>) -> validation::Check,
}

/// The reference set for validation (design section 5 inputs), given the
/// local identity read earlier and the counted acceptances.
async fn expectations(
    step: &Step<'_>,
    counted: &[Acceptance],
    head: HeadState,
    local: validation::LocalIdentity,
) -> Result<Expectations> {
    let record = &step.proposal.record;
    let proposer_participant = CantonId::parse(&record.proposer_participant)
        .with_context(|| format!("proposerParticipant `{}`", record.proposer_participant))?;
    let hosting =
        identity::verify_hosting(step.config(), &record.proposer, &proposer_participant).await?;
    Ok(Expectations::new(record, counted, head, local)
        .with_proposer_hosting(hosting.has_submission()))
}

async fn member_cosign<M>(step: &Step<'_>, target: CosignTarget<'_, M>) -> Result<()> {
    let db = step.db();
    let instance = &step.run.instance_name;
    let label = target.label;
    if target.pending.is_empty() {
        tracing::debug!(instance = %instance, "no pending {label} proposal yet");
        return Ok(());
    }
    // The pre-filter needs only this node's keys and the proposer's
    // fingerprint. The acceptance counting and the hosting checks run once
    // a candidate exists, so a waiting tick stays cheap.
    let record = &step.proposal.record;
    let Some(proposer) = record.proposer_namespace_fingerprint.as_deref() else {
        return fail_run(
            db,
            step.run,
            "the accepted proposal carries no proposer namespace fingerprint",
        )
        .await;
    };
    let local = keys::local_identity_for_party(
        step.config(),
        db,
        Some(&step.intent.party),
        record.prefix.as_deref(),
    )
    .await?;
    let matches = matching_proposals(
        target.pending,
        proposer,
        &local.owner_fingerprints,
        target.accepted_serial,
    );
    let [candidate] = matches.as_slice() else {
        if matches.is_empty() {
            tracing::debug!(instance = %instance, "no matching pending {label} proposal yet");
            return Ok(());
        }
        return fail_run(
            db,
            step.run,
            &format!(
                "{} pending {label} proposals match the accepted proposal; refusing to pick one",
                matches.len()
            ),
        )
        .await;
    };
    // Counting fails closed on a duplicate acceptor (design D6). The other
    // inputs are reads, and a read error retries next tick.
    let counted = match step.counted_acceptances().await {
        Ok(counted) => counted,
        Err(e) => return fail_run(db, step.run, &format!("{e:#}")).await,
    };
    let exp = expectations(step, &counted, target.head, local).await?;
    if let Err(e) = (target.validate)(candidate, &exp, Some(target.accepted_serial)) {
        return fail_run(
            db,
            step.run,
            &format!(
                "refusing to co-sign the {label} proposal {}: {e}",
                candidate.hash_hex
            ),
        )
        .await;
    }
    if let Some(reason) = pin_conflict(
        step.meta
            .topology_hashes
            .get(target.hash_key)
            .map(String::as_str),
        &candidate.hash_hex,
    ) {
        return fail_run(db, step.run, &format!("{label}: {reason}")).await;
    }
    if !step.is_live().await? {
        return Ok(());
    }
    pin_topology_hash(db, instance, target.hash_key, &candidate.hash_hex).await?;
    pin_decision_hash(db, &step.meta.proposal_cid, &candidate.hash_hex).await?;
    match topology::cosign_by_hash(
        step.config(),
        step.sync_id(),
        &candidate.hash_hex,
        &candidate.signed_by,
    )
    .await?
    {
        CosignOutcome::NotFound => {
            tracing::info!(instance = %instance, hash = %candidate.hash_hex, "{label} proposal not in this store yet; retry next tick");
        }
        outcome => {
            tracing::info!(instance = %instance, hash = %candidate.hash_hex, ?outcome, "{label} proposal co-signed");
        }
    }
    Ok(())
}

async fn member_cosign_changes(step: &Step<'_>) -> Result<()> {
    let db = step.db();
    let intent = &step.intent;
    let now = step.ctx.now_micros;
    let Some(dnd) = step.read_dnd().await? else {
        return fail_run(
            db,
            step.run,
            &format!(
                "{} has no accepted DecentralizedNamespaceDefinition",
                intent.party
            ),
        )
        .await;
    };
    let Some(p2p) = step.read_p2p().await? else {
        return fail_run(
            db,
            step.run,
            &format!("{} has no accepted PartyToParticipant", intent.party),
        )
        .await;
    };
    let head = HeadState {
        dnd: Some(dnd.mapping.clone()),
        p2p: Some(p2p.mapping.clone()),
    };

    let progress = dnd_progress(&dnd, intent, now);
    match progress {
        MappingProgress::Pending => {
            let pending =
                topology::list_pending_dnd(step.config(), step.sync_id(), &step.namespace())
                    .await?;
            return member_cosign(
                step,
                CosignTarget {
                    label: "DND",
                    hash_key: DND_HASH,
                    head,
                    pending: &pending,
                    accepted_serial: dnd.serial,
                    validate: validation::validate_dnd,
                },
            )
            .await;
        }
        MappingProgress::Authorized => {
            tracing::debug!(instance = %step.run.instance_name, "DND authorized, not effective yet");
            return Ok(());
        }
        MappingProgress::Effective => {}
        MappingProgress::Foreign(_) | MappingProgress::Moved(_) => {
            return fail_run(
                db,
                step.run,
                &describe_moved("DND", progress, intent.dnd_base),
            )
            .await;
        }
    }

    let progress = p2p_progress(&p2p, intent, now);
    match progress {
        MappingProgress::Pending => {
            let pending =
                topology::list_pending_p2p(step.config(), step.sync_id(), &intent.party).await?;
            member_cosign(
                step,
                CosignTarget {
                    label: "P2P",
                    hash_key: P2P_HASH,
                    head,
                    pending: &pending,
                    accepted_serial: p2p.serial,
                    validate: validation::validate_p2p,
                },
            )
            .await
        }
        MappingProgress::Authorized => {
            tracing::debug!(instance = %step.run.instance_name, "P2P authorized, not effective yet");
            Ok(())
        }
        MappingProgress::Effective => {
            cache_threshold(db, &intent.party, intent.threshold).await?;
            complete_run(db, step.run).await
        }
        MappingProgress::Foreign(_) | MappingProgress::Moved(_) => {
            fail_run(
                db,
                step.run,
                &describe_moved("P2P", progress, intent.p2p_base),
            )
            .await
        }
    }
}

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

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

    /// Section 6: refuse a threshold outside `1..=|owners|` or equal to the
    /// one in force. The head DND is the authority, not the request's
    /// `previous_threshold`.
    async fn preflight(ol: &OnLedger, req: &StartRequest) -> Result<()> {
        let StartRequest::ChangeThreshold {
            dec_party_id,
            new_threshold,
            ..
        } = req
        else {
            return Ok(());
        };
        let sync_id = utils::get_synchronizer_id(ol.config()).await?;
        let Some(head) =
            topology::read_accepted_dnd(ol.config(), &sync_id, &dec_party_id.namespace.to_hex())
                .await?
        else {
            bail!(
                "{dec_party_id} has no accepted DecentralizedNamespaceDefinition in the \
                 synchronizer store"
            );
        };
        let previous = u32::try_from(head.mapping.threshold).unwrap_or(0);
        check_new_threshold(*new_threshold, previous, head.mapping.owners.len())?;
        Ok(())
    }

    /// Design D6: the proposal names the owner key the members expect in
    /// `signed_by_fingerprints`. No key is generated: the party exists.
    async fn prepare(ol: &OnLedger, req: &StartRequest) -> Result<ProposalExtras> {
        let StartRequest::ChangeThreshold { dec_party_id, .. } = req else {
            bail!("ChangeThreshold::prepare received a {} request", req.kind());
        };
        let local =
            keys::local_identity_for_party(ol.config(), ol.db(), Some(dec_party_id), None).await?;
        let owner = single_owner(&local.owner_fingerprints, dec_party_id)?;
        // The key bytes are informational for this kind (design D4): the
        // members read the root delegation in the synchronizer store.
        let key_hex = keys::list_vault_keys(ol.config())
            .await?
            .into_iter()
            .find(|k| k.fingerprint == owner)
            .map(|k| keys::PartyKey::from_key(k.key).key_hex);
        Ok(ProposalExtras {
            keys: ProposerKeyMaterial {
                namespace_fingerprint: Some(owner),
                signing_public_key_hex: key_hex,
                daml_key_fingerprint: local.daml_key_fingerprint,
            },
            ..ProposalExtras::default()
        })
    }

    async fn tick_coordinator(ctx: &TickCtx<'_>, run: &WorkflowRun, meta: &RunMeta) -> Result<()> {
        // `engine::reconcile` already ended the run when the proposal
        // vanished, expired, or lost its quorum to declines.
        let Some(proposal) = ctx.proposals.proposal(&meta.proposal_cid) else {
            return Ok(());
        };
        let intent = match intent_of(&proposal.record) {
            Ok(intent) => intent,
            Err(e) => {
                let reason = format!("malformed WorkflowProposal: {e:#}");
                fail_run(ctx.db(), run, &reason).await?;
                if let Err(e) =
                    proposals::finish(ctx.client, &meta.proposal_cid, false, Some(reason)).await
                {
                    tracing::warn!(proposal = %meta.proposal_cid, error = %e, "WorkflowProposal_Finish failed");
                }
                return Ok(());
            }
        };
        let step = Step {
            ctx,
            run,
            meta,
            proposal,
            intent,
        };
        match run.current_step.as_str() {
            WAITING_FOR_ACCEPTANCES_STEP => coordinator_wait_for_acceptances(&step).await,
            PROPOSE_CHANGES_STEP => coordinator_propose_dnd(&step).await,
            AWAIT_CHANGES_STEP => coordinator_await_changes(&step).await,
            other => {
                tracing::debug!(instance = %run.instance_name, step = other, "nothing to do");
                Ok(())
            }
        }
    }

    async fn tick_member(ctx: &TickCtx<'_>, run: &WorkflowRun, meta: &RunMeta) -> Result<()> {
        if run.current_step != CO_SIGN_CHANGES_STEP {
            return Ok(());
        }
        let Some(proposal) = ctx.proposals.proposal(&meta.proposal_cid) else {
            return Ok(());
        };
        let intent = match intent_of(&proposal.record) {
            Ok(intent) => intent,
            Err(e) => {
                return fail_run(ctx.db(), run, &format!("malformed WorkflowProposal: {e:#}"))
                    .await;
            }
        };
        let step = Step {
            ctx,
            run,
            meta,
            proposal,
            intent,
        };
        member_cosign_changes(&step).await
    }
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::enums::TopologyChangeOp;

    use super::*;
    use crate::onledger::{
        daml::codec::tests::proposal_full,
        topology::{
            build_bootstrap_p2p, build_change_threshold_dnd, build_change_threshold_p2p, build_dnd,
            dnd_of, p2p_of,
            tests::{NS, key, participant},
        },
    };

    fn pending(
        hash: &str,
        serial: u32,
        signed_by: &[&str],
        op: TopologyChangeOp,
    ) -> PendingProposal<()> {
        PendingProposal {
            hash_hex: hash.to_string(),
            serial,
            signed_by: signed_by.iter().map(|s| s.to_string()).collect(),
            operation: op as i32,
            mapping: (),
            sequenced: None,
            valid_from: None,
        }
    }

    fn cbtc() -> CantonId {
        CantonId::parse(&format!("cbtc::{NS}")).expect("id")
    }

    #[test]
    fn intent_reads_the_proposal_and_fails_closed_on_gaps() {
        let record = proposal_full();
        let intent = intent_of(&record).expect("intent");
        assert_eq!(intent.party, cbtc());
        assert_eq!(intent.threshold, 2);
        assert_eq!(intent.previous_threshold, 3);
        assert_eq!(intent.dnd_base, 4);
        assert_eq!(intent.p2p_base, 5);

        let mut no_party = proposal_full();
        no_party.dec_party_id = None;
        assert!(intent_of(&no_party).is_err());

        let mut no_threshold = proposal_full();
        no_threshold.threshold = None;
        assert!(intent_of(&no_threshold).is_err());

        let mut zero_threshold = proposal_full();
        zero_threshold.threshold = Some(0);
        assert!(intent_of(&zero_threshold).is_err());

        let mut no_previous = proposal_full();
        no_previous.previous_threshold = None;
        assert!(intent_of(&no_previous).is_err());

        let mut no_base = proposal_full();
        no_base.dnd_base_serial = None;
        assert!(intent_of(&no_base).is_err());

        let mut negative_base = proposal_full();
        negative_base.p2p_base_serial = Some(-1);
        assert!(intent_of(&negative_base).is_err());
    }

    #[test]
    fn progress_follows_the_serial_and_the_content() {
        assert_eq!(
            mapping_progress(4, true, 4, false),
            MappingProgress::Pending
        );
        assert_eq!(
            mapping_progress(5, false, 4, true),
            MappingProgress::Authorized
        );
        assert_eq!(
            mapping_progress(5, true, 4, true),
            MappingProgress::Effective
        );
        // Serial 5 with another threshold: a concurrent change won the serial.
        assert_eq!(
            mapping_progress(5, true, 4, false),
            MappingProgress::Foreign(5)
        );
        assert_eq!(
            mapping_progress(6, true, 4, true),
            MappingProgress::Moved(6)
        );
        // A serial below the base is "moved" too: the store went backwards.
        assert_eq!(
            mapping_progress(3, true, 4, true),
            MappingProgress::Moved(3)
        );
        assert_eq!(
            mapping_progress(u32::MAX, true, u32::MAX - 1, true),
            MappingProgress::Effective
        );
    }

    #[test]
    fn threshold_checks_read_the_dnd_and_both_p2p_thresholds() {
        let owners: Vec<String> = ["a", "b", "c"].iter().map(|s| format!("1220{s}")).collect();
        let head_dnd = dnd_of(&build_dnd(&owners, 2)).expect("dnd").clone();
        assert!(dnd_has_threshold(&head_dnd, 2));
        assert!(!dnd_has_threshold(&head_dnd, 3));
        let changed = build_change_threshold_dnd(&head_dnd, 3);
        assert!(dnd_has_threshold(dnd_of(&changed).expect("dnd"), 3));

        let hosts = [participant(1), participant(2)];
        let keys = [key(1), key(2)];
        let head_p2p = p2p_of(&build_bootstrap_p2p("cbtc", NS, &hosts, &keys, 2))
            .expect("p2p")
            .clone();
        assert!(p2p_has_threshold(&head_p2p, 2));
        assert!(!p2p_has_threshold(&head_p2p, 1));
        let changed = build_change_threshold_p2p(&head_p2p, 1);
        assert!(p2p_has_threshold(p2p_of(&changed).expect("p2p"), 1));

        // A hosting threshold alone is not enough: both thresholds must move.
        let mut half = head_p2p.clone();
        half.threshold = 1;
        assert!(!p2p_has_threshold(&half, 1));
        let mut no_keys = head_p2p;
        no_keys.party_signing_keys = None;
        assert!(!p2p_has_threshold(&no_keys, 2));
    }

    #[test]
    fn new_threshold_must_fit_the_owners_and_differ() {
        assert_eq!(check_new_threshold(3, 2, 3), Ok(3));
        assert_eq!(check_new_threshold(1, 2, 3), Ok(1));

        let same = check_new_threshold(2, 2, 3).expect_err("same value");
        assert!(same.message.contains("already 2"), "{same}");

        let too_high = check_new_threshold(4, 2, 3).expect_err("above the owners");
        assert!(too_high.message.contains("outside 1..=3"), "{too_high}");

        assert!(check_new_threshold(0, 2, 3).is_err());
        assert!(check_new_threshold(-1, 2, 3).is_err());
        assert!(check_new_threshold(1, 2, 0).is_err(), "no owners at all");
    }

    /// Section 10: 4 owners, previous 3, new 2: three signatures per mapping,
    /// so the coordinator waits for two acceptances.
    #[test]
    fn quorum_counts_the_larger_threshold_minus_the_proposer() {
        assert!(!quorum_reached(0, 3, 2));
        assert!(!quorum_reached(1, 3, 2));
        assert!(quorum_reached(2, 3, 2));
        assert!(quorum_reached(3, 3, 2));
        // Raising the threshold: the new value rules.
        assert!(!quorum_reached(1, 2, 3));
        assert!(quorum_reached(2, 2, 3));
        // A one-of-one party with a threshold of 1 needs nobody else.
        assert!(quorum_reached(0, 1, 1));
    }

    #[test]
    fn matching_proposals_apply_the_d5_filters() {
        let proposer = "1220aa";
        let mine: BTreeSet<String> = ["1220bb".to_string()].into_iter().collect();
        let add = TopologyChangeOp::AddReplace;
        let list = vec![
            pending("h-ok", 5, &[proposer], add),
            pending("h-remove", 5, &[proposer], TopologyChangeOp::Remove),
            pending("h-unsigned", 5, &["1220cc"], add),
            pending("h-signed-by-me", 5, &[proposer, "1220bb"], add),
            pending("h-old-serial", 4, &[proposer], add),
            pending("h-far-serial", 6, &[proposer], add),
        ];

        let matches = matching_proposals(&list, proposer, &mine, 4);
        let hashes: Vec<&str> = matches.iter().map(|p| p.hash_hex.as_str()).collect();
        assert_eq!(hashes, ["h-ok"]);

        // Once the accepted serial moves, only the next serial matches.
        let later: Vec<&str> = matching_proposals(&list, proposer, &mine, 5)
            .iter()
            .map(|p| p.hash_hex.as_str())
            .collect();
        assert_eq!(later, ["h-far-serial"]);
        assert!(matching_proposals(&list, proposer, &mine, u32::MAX).is_empty());
    }

    #[test]
    fn pin_conflict_refuses_a_second_hash_only() {
        assert_eq!(pin_conflict(None, "1220ab"), None);
        assert_eq!(pin_conflict(Some("1220ab"), "1220ab"), None);
        let reason = pin_conflict(Some("1220ab"), "1220cd").expect("conflict");
        assert!(
            reason.contains("1220ab") && reason.contains("1220cd"),
            "{reason}"
        );
    }

    #[test]
    fn single_owner_fails_closed_on_none_or_many() {
        let party = cbtc();
        let one: BTreeSet<String> = ["1220aa".to_string()].into_iter().collect();
        assert_eq!(single_owner(&one, &party).expect("one"), "1220aa");

        let none = BTreeSet::new();
        assert!(single_owner(&none, &party).is_err());

        let two: BTreeSet<String> = ["1220aa".to_string(), "1220bb".to_string()]
            .into_iter()
            .collect();
        let err = single_owner(&two, &party).expect_err("two");
        assert!(err.to_string().contains("2 owner keys"), "{err}");
    }

    #[test]
    fn moved_descriptions_name_the_serials() {
        let text = describe_moved("DND", MappingProgress::Moved(7), 4);
        assert!(
            text.contains("serial 7") && text.contains("base serial 4"),
            "{text}"
        );
        let text = describe_moved("P2P", MappingProgress::Foreign(5), 4);
        assert!(text.contains("another threshold"), "{text}");
    }

    #[test]
    fn step_lists_match_design_section_6() {
        assert_eq!(
            ChangeThreshold::coordinator_steps(),
            [
                "WaitingForAcceptances",
                "ProposeChanges",
                "AwaitChanges",
                "Complete"
            ]
        );
        assert_eq!(
            ChangeThreshold::member_steps(None),
            ["CoSignChanges", "Complete"]
        );
        assert_eq!(COORDINATOR_STEPS[0], WAITING_FOR_ACCEPTANCES_STEP);
        assert!(COORDINATOR_STEPS.contains(&PROPOSE_CHANGES_STEP));
        assert!(COORDINATOR_STEPS.contains(&AWAIT_CHANGES_STEP));
        assert!(MEMBER_STEPS.contains(&CO_SIGN_CHANGES_STEP));
    }
}
