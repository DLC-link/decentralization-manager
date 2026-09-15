//! Onboarding driver (design D4, D5, D6, section 6): create a new
//! decentralized party from nothing.
//!
//! The coordinator generates its dual-usage key before the proposal exists
//! (`prepare`), waits until every invitee has accepted, then proposes the
//! DND and, once that is effective, the P2P. Every member generates its own
//! key, accepts with its key material, and co-signs each mapping after the
//! section-5 checks pass. Both sides fill the local `dec_party` cache when
//! the P2P is effective, so the parties list and a later kick work without
//! a refresh.
//!
//! Every tick does one bounded unit of work for `current_step` and returns.
//! A wait is a step: a wait that is not over leaves the row unchanged, and
//! the observer ticks again a few seconds later.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::SigningPublicKey, protocol::v30::DecentralizedNamespaceDefinition,
};
use common::{
    canton_id::CantonId,
    types::{Permission, WorkflowKind, WorkflowProgress, WorkflowRun},
};
use sqlx::SqlitePool;

use crate::{
    db::{
        rows::{DecPartyParticipantRow, DecPartyRow},
        schema::{Commitable, SchemaRead, SchemaWrite},
    },
    onledger::{
        CoordinationClient,
        daml::codec::WorkflowProposalRecord,
        identity::verify_hosting,
        keys, now_secs,
        proposals::{self, Acceptance, ActiveProposal},
        topology::{self, CosignOutcome, MappingKey, PendingProposal, WaitBudget},
        validation::{self, Check, Expectations, HeadState},
    },
};

use super::{
    KindDriver, MemberVariant, OnLedger, PreflightRejected, ProposalExtras, RunMeta, StartRequest,
    TickCtx, advance_step, complete_run, fail_run, pin_topology_hash, read_run_meta,
};

pub struct Onboarding;

pub const COORDINATOR_STEPS: &[&str] = &[
    STEP_GENERATE_KEYS,
    STEP_WAITING_FOR_ACCEPTANCES,
    STEP_PROPOSE_NAMESPACE,
    STEP_AWAIT_NAMESPACE,
    STEP_PROPOSE_PARTY,
    STEP_AWAIT_PARTY,
    super::COMPLETE_STEP,
];

pub const MEMBER_STEPS: &[&str] = &[
    STEP_GENERATE_KEYS,
    STEP_COSIGN_NAMESPACE,
    STEP_COSIGN_PARTY,
    super::COMPLETE_STEP,
];

const STEP_GENERATE_KEYS: &str = "GenerateKeys";
const STEP_WAITING_FOR_ACCEPTANCES: &str = super::WAITING_FOR_ACCEPTANCES_STEP;
const STEP_PROPOSE_NAMESPACE: &str = "ProposeNamespace";
const STEP_AWAIT_NAMESPACE: &str = "AwaitNamespace";
const STEP_PROPOSE_PARTY: &str = "ProposeParty";
const STEP_AWAIT_PARTY: &str = "AwaitParty";
const STEP_COSIGN_NAMESPACE: &str = "CoSignNamespace";
const STEP_COSIGN_PARTY: &str = "CoSignParty";

/// `RunMeta::topology_hashes` keys.
const HASH_DND: &str = "dnd";
const HASH_P2P: &str = "p2p";

/// A new mapping starts at serial 1 (design D5 step 2).
const FIRST_SERIAL: u32 = 1;

/// How long an `Await*` step tolerates a pinned proposal that is missing
/// from the synchronizer store before it re-proposes (design D10, retry).
/// A fresh proposal needs a moment to be sequenced, so the check has a
/// grace period.
const REPROPOSE_GRACE_SECS: i64 = 60;

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// The threshold when the operator gives none: a simple majority,
/// `ceil(n / 2)`, and at least 1. Legacy onboarding used this rule, and
/// `engine::resolve_targets` applies it before the proposal is created.
pub fn default_threshold(members: usize) -> u32 {
    u32::try_from(members.div_ceil(2).max(1)).unwrap_or(u32::MAX)
}

/// The threshold the proposal carries, in `1..=participants`. `None` fails
/// closed, as every check in `validation` does.
pub fn proposal_threshold(proposal: &WorkflowProposalRecord) -> Result<u32> {
    let Some(threshold) = proposal.threshold else {
        bail!("the WorkflowProposal carries no threshold");
    };
    let members = i64::try_from(proposal.participants.len()).unwrap_or(i64::MAX);
    if threshold < 1 || threshold > members {
        bail!("threshold {threshold} is outside 1..={members}");
    }
    u32::try_from(threshold).context("threshold does not fit u32")
}

/// One member's key claims, as the proposal (for the proposer) or its
/// counted acceptance (for an invitee) states them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberKeys {
    pub participant: CantonId,
    /// The dual-usage key fingerprint: the DND owner (design D4).
    pub owner_fingerprint: String,
    pub daml_key_fingerprint: Option<String>,
}

/// Every member of the new party: the proposer first, then every counted
/// acceptor. Fails closed on a missing or repeated owner fingerprint, a
/// repeated participant, or a participant id that does not parse.
pub fn member_keys(
    proposal: &WorkflowProposalRecord,
    counted: &[Acceptance],
) -> Result<Vec<MemberKeys>> {
    let proposer = (
        proposal.proposer_participant.as_str(),
        proposal.proposer_namespace_fingerprint.as_deref(),
        proposal.proposer_daml_key_fingerprint.as_deref(),
        format!("the proposer {}", proposal.proposer),
    );
    let acceptors = counted.iter().map(|a| {
        (
            a.record.participant_id.as_str(),
            a.record.namespace_fingerprint.as_deref(),
            a.record.daml_key_fingerprint.as_deref(),
            format!("the acceptance of {}", a.record.acceptor),
        )
    });

    let mut members = Vec::with_capacity(counted.len() + 1);
    let mut owners = BTreeSet::new();
    let mut participants = BTreeSet::new();
    for (participant, owner, daml, who) in std::iter::once(proposer).chain(acceptors) {
        let participant = CantonId::parse(participant)
            .with_context(|| format!("{who} names an invalid participant `{participant}`"))?;
        let Some(owner) = owner else {
            bail!("{who} carries no namespace fingerprint");
        };
        if !owners.insert(owner.to_string()) {
            bail!("owner fingerprint {owner} appears twice ({who})");
        }
        if !participants.insert(participant.clone()) {
            bail!("participant {participant} appears twice ({who})");
        }
        members.push(MemberKeys {
            participant,
            owner_fingerprint: owner.to_string(),
            daml_key_fingerprint: daml.map(str::to_string),
        });
    }
    Ok(members)
}

/// The DND owners, sorted.
pub fn owner_fingerprints(members: &[MemberKeys]) -> Vec<String> {
    members
        .iter()
        .map(|m| m.owner_fingerprint.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The P2P hosts, sorted.
pub fn host_participants(members: &[MemberKeys]) -> Vec<CantonId> {
    members
        .iter()
        .map(|m| m.participant.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// `prefix::computeNamespace(owners)` (design D5, member step 1).
pub fn party_id_for(prefix: &str, owners: &[String]) -> Result<CantonId> {
    let namespace = topology::compute_namespace(owners.iter());
    CantonId::parse(&format!("{prefix}::{namespace}")).context("derived party id")
}

/// Invitees without a counted acceptance yet.
pub fn missing_invitees(
    proposal: &WorkflowProposalRecord,
    counted: &[Acceptance],
) -> Vec<CantonId> {
    let accepted: BTreeSet<&CantonId> = counted.iter().map(|a| &a.record.acceptor).collect();
    proposal
        .invitees
        .iter()
        .filter(|i| !accepted.contains(i))
        .cloned()
        .collect()
}

/// The participants behind the counted acceptances, sorted: the
/// `connected_peers` projection ("invitees that accepted").
pub fn accepted_participants(counted: &[Acceptance]) -> Vec<CantonId> {
    counted
        .iter()
        .filter_map(|a| CantonId::parse(&a.record.participant_id).ok())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// A DND that already exists for the derived namespace must be the one the
/// proposal describes; otherwise a second party under an older namespace
/// would inherit a threshold nobody accepted.
pub fn check_existing_dnd(
    dnd: &DecentralizedNamespaceDefinition,
    owners: &[String],
    threshold: u32,
) -> Result<()> {
    let existing: BTreeSet<&String> = dnd.owners.iter().collect();
    let expected: BTreeSet<&String> = owners.iter().collect();
    if existing != expected {
        bail!(
            "namespace {} already exists with owners {:?}, not the accepted {:?}",
            dnd.decentralized_namespace,
            dnd.owners,
            owners
        );
    }
    if u32::try_from(dnd.threshold).ok() != Some(threshold) {
        bail!(
            "namespace {} already exists with threshold {}, not the accepted {threshold}",
            dnd.decentralized_namespace,
            dnd.threshold
        );
    }
    Ok(())
}

/// What a member found among the pending proposals of one mapping.
#[derive(Debug)]
pub enum Selection<'a, M> {
    /// No candidate signed by the proposer yet.
    Waiting,
    /// This node already signed a candidate.
    AlreadySigned(&'a PendingProposal<M>),
    /// The first candidate that passes validation.
    Match(&'a PendingProposal<M>),
    /// Every candidate failed validation, with the reasons.
    Mismatch(Vec<String>),
}

/// Pick the proposal a member co-signs. A candidate is an `ADD_REPLACE`
/// signed by the proposer's owner key (design D5, member step 2). A
/// candidate this node already signed wins over every other one, because
/// one mapping gets one transaction (design D5 step 8). A stale candidate
/// from an older attempt fails validation without blocking a valid one; the
/// run fails only when no candidate passes.
pub fn select_pending<'a, M>(
    pending: &'a [PendingProposal<M>],
    proposer_fingerprint: &str,
    own_fingerprints: &BTreeSet<String>,
    check: impl Fn(&PendingProposal<M>) -> Check,
) -> Selection<'a, M> {
    let candidates: Vec<&'a PendingProposal<M>> = pending
        .iter()
        .filter(|p| p.is_add_replace() && p.is_signed_by(proposer_fingerprint))
        .collect();
    if let Some(signed) = candidates
        .iter()
        .copied()
        .find(|p| own_fingerprints.iter().any(|fp| p.is_signed_by(fp)))
    {
        return Selection::AlreadySigned(signed);
    }
    let mut reasons = Vec::new();
    for candidate in candidates {
        match check(candidate) {
            Ok(()) => return Selection::Match(candidate),
            Err(e) => reasons.push(format!("{}: {e}", candidate.hash_hex)),
        }
    }
    if reasons.is_empty() {
        Selection::Waiting
    } else {
        Selection::Mismatch(reasons)
    }
}

/// Whether an `Await*` step may look for its pinned proposal at all: a pin
/// exists and the grace period since the last row change is over. Without
/// a pin there is nothing to compare, so the step waits.
pub fn repropose_due(pinned: Option<&str>, updated_at: i64, now: i64) -> bool {
    pinned.is_some() && now - updated_at >= REPROPOSE_GRACE_SECS
}

/// Whether an `Await*` step goes back and re-proposes: [`repropose_due`]
/// and the pinned hash has left the synchronizer store.
pub fn should_repropose(
    pinned: Option<&str>,
    pending_hashes: &[String],
    updated_at: i64,
    now: i64,
) -> bool {
    repropose_due(pinned, updated_at, now)
        && pinned.is_some_and(|pin| !pending_hashes.iter().any(|h| h == pin))
}

/// One read and no sleep: a wait that is not over stays in its step, and
/// the observer ticks again in a few seconds.
fn check_once() -> WaitBudget {
    WaitBudget {
        max_attempts: 1,
        delay: Duration::ZERO,
    }
}

fn required_prefix(proposal: &WorkflowProposalRecord) -> Result<&str> {
    proposal
        .prefix
        .as_deref()
        .context("the onboarding WorkflowProposal carries no prefix")
}

fn required_proposer_fingerprint(proposal: &WorkflowProposalRecord) -> Result<&str> {
    proposal
        .proposer_namespace_fingerprint
        .as_deref()
        .context("the onboarding WorkflowProposal carries no proposer namespace fingerprint")
}

// ---------------------------------------------------------------------------
// Shared async glue
// ---------------------------------------------------------------------------

/// The party the proposal and its counted acceptances describe.
struct PartyPlan {
    members: Vec<MemberKeys>,
    owners: Vec<String>,
    namespace: String,
    party: CantonId,
    threshold: u32,
}

fn party_plan(proposal: &WorkflowProposalRecord, counted: &[Acceptance]) -> Result<PartyPlan> {
    let members = member_keys(proposal, counted)?;
    let owners = owner_fingerprints(&members);
    let party = party_id_for(required_prefix(proposal)?, &owners)?;
    Ok(PartyPlan {
        namespace: party.namespace.to_hex(),
        threshold: proposal_threshold(proposal)?,
        members,
        owners,
        party,
    })
}

/// The proposal of a run, from this tick's snapshot. `reconcile` fails or
/// cancels the run when it is gone, so `None` only means "not this tick".
fn proposal_of<'a>(ctx: &'a TickCtx<'_>, meta: &RunMeta) -> Option<&'a ActiveProposal> {
    let proposal = ctx.proposals.proposal(&meta.proposal_cid);
    if proposal.is_none() {
        tracing::debug!(proposal = %meta.proposal_cid, "proposal not in this tick's snapshot");
    }
    proposal
}

/// The counted acceptances (design D6). `Err` is a refusal, not a transport
/// error: `counted_acceptances_verified` treats an unreachable hosting check
/// as "not counted this tick", so what remains is conflicting key material,
/// which fails the run closed.
async fn count_acceptances(
    ctx: &TickCtx<'_>,
    proposal: &ActiveProposal,
) -> std::result::Result<Vec<Acceptance>, String> {
    let raw = ctx.proposals.acceptances_for(&proposal.contract_id);
    proposals::counted_acceptances_verified(ctx.ol.config(), proposal, &raw)
        .await
        .map_err(|e| format!("acceptances cannot be counted: {e:#}"))
}

/// The party plan for a coordinator tick, or `None` after the run failed.
async fn coordinator_plan(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &ActiveProposal,
) -> Result<Option<PartyPlan>> {
    let counted = match count_acceptances(ctx, proposal).await {
        Ok(counted) => counted,
        Err(reason) => {
            fail_coordinator(ctx, run, meta, reason).await?;
            return Ok(None);
        }
    };
    match party_plan(&proposal.record, &counted) {
        Ok(plan) => Ok(Some(plan)),
        Err(e) => {
            fail_coordinator(ctx, run, meta, format!("{e:#}")).await?;
            Ok(None)
        }
    }
}

/// Exercise `WorkflowProposal_Finish`. A failure is logged, not returned:
/// the row is already terminal and the proposal expires on its own, the
/// same policy `engine::cancel_run` applies to `Cancel`.
///
/// TODO(engine/mod.rs): `finish_best_effort` there is private; share it.
async fn finish_best_effort(
    client: &CoordinationClient,
    cid: &str,
    succeeded: bool,
    error: Option<String>,
) {
    if let Err(e) = proposals::finish(client, cid, succeeded, error).await {
        tracing::warn!(
            proposal = %cid,
            succeeded,
            error = %e,
            "WorkflowProposal_Finish failed; the proposal expires on its own"
        );
    }
}

/// Fail the coordinator row and finish the proposal with the same reason.
async fn fail_coordinator(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    reason: String,
) -> Result<()> {
    fail_run(ctx.db(), run, &reason).await?;
    finish_best_effort(ctx.client, &meta.proposal_cid, false, Some(reason)).await;
    Ok(())
}

/// Fill the local `dec_party` cache the way a `/decentralized-parties`
/// refresh would, then record every member's keys through the shared
/// helper so a later kick can attribute them (design section 5).
async fn persist_party_cache(db: &SqlitePool, plan: &PartyPlan, me: &CantonId) -> Result<()> {
    let party_id = plan.party.to_string();
    let my_owner_key = plan
        .members
        .iter()
        .find(|m| m.participant == *me)
        .map(|m| m.owner_fingerprint.clone());
    let rows: Vec<DecPartyParticipantRow> = plan
        .members
        .iter()
        .map(|m| DecPartyParticipantRow {
            dec_party_id: party_id.clone(),
            participant_uid: m.participant.to_string(),
            permission: Permission::Confirmation.as_str().to_string(),
            owner_key: Some(m.owner_fingerprint.clone()),
            // `record_member_keys` writes the Daml key below.
            signing_key: None,
        })
        .collect();

    let mut tx = db.begin_transaction().await?;
    tx.upsert_dec_party(&DecPartyRow {
        party_id: party_id.clone(),
        prefix: plan.party.prefix.clone(),
        threshold: i64::from(plan.threshold),
        updated_at: now_secs(),
        my_owner_key,
    })
    .await?;
    tx.replace_dec_party_owners(&plan.party, &plan.owners)
        .await?;
    tx.replace_dec_party_participants(&plan.party, &rows)
        .await?;
    Commitable::commit(tx).await?;

    for m in &plan.members {
        keys::record_member_keys(
            db,
            &plan.party,
            &m.participant,
            Some(&m.owner_fingerprint),
            m.daml_key_fingerprint.as_deref(),
        )
        .await?;
    }
    tracing::info!(party = %plan.party, members = plan.members.len(), "dec_party cache written");
    Ok(())
}

/// Carry the new party id on the run row, so the card and later lookups
/// find it after the run is dismissed.
async fn set_run_party(db: &SqlitePool, run: &WorkflowRun, party: &CantonId) -> Result<()> {
    let mut tx = db.begin_transaction().await?;
    tx.set_workflow_run_dec_party_id(&run.instance_name, party)
        .await?;
    Commitable::commit(tx).await
}

/// Project "invitees that accepted" into `connected_peers`. The row is
/// re-read first so a concurrent cancel or dismiss is not overwritten.
async fn record_connected_peers(
    db: &SqlitePool,
    run: &WorkflowRun,
    accepted: Vec<CantonId>,
) -> Result<()> {
    let current: BTreeSet<&CantonId> = run.connected_peers.iter().collect();
    if current == accepted.iter().collect::<BTreeSet<_>>() {
        return Ok(());
    }
    let Some(mut fresh) = db.get_workflow_run(&run.instance_name).await? else {
        return Ok(());
    };
    if fresh.status != WorkflowProgress::InProgress {
        return Ok(());
    }
    fresh.connected_peers = accepted;
    fresh.updated_at = now_secs();
    let mut tx = db.begin_transaction().await?;
    tx.upsert_workflow_run(&fresh).await?;
    Commitable::commit(tx).await
}

/// Design D5 step 5, immediately before a co-sign: the proposal is still
/// active and unexpired, the row is still in progress at this step, and the
/// hash is pinned on the row and in `proposal_decisions`. A pin for a
/// different hash of the same mapping fails the run closed: one mapping,
/// one transaction. Returns `false` when the node must not sign this tick.
async fn recheck_and_pin(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    key: &str,
    hash_hex: &str,
) -> Result<bool> {
    let db = ctx.db();
    let Some(proposal) = proposals::read_proposal(ctx.client, &meta.proposal_cid).await? else {
        tracing::debug!(proposal = %meta.proposal_cid, "proposal no longer active; not signing");
        return Ok(false);
    };
    if ctx.is_expired(&proposal) {
        tracing::debug!(proposal = %meta.proposal_cid, "proposal expired; not signing");
        return Ok(false);
    }
    let Some(fresh) = db.get_workflow_run(&run.instance_name).await? else {
        return Ok(false);
    };
    if fresh.status != WorkflowProgress::InProgress || fresh.current_step != run.current_step {
        tracing::debug!(
            instance = %run.instance_name,
            status = %fresh.status,
            step = %fresh.current_step,
            "row moved under the tick; not signing"
        );
        return Ok(false);
    }
    let pinned = read_run_meta(&fresh).and_then(|m| m.topology_hashes.get(key).cloned());
    if let Some(pinned) = pinned
        && pinned != hash_hex
    {
        fail_run(
            db,
            run,
            &format!("a different {key} transaction {pinned} was pinned earlier; refusing to sign {hash_hex}"),
        )
        .await?;
        return Ok(false);
    }
    pin_topology_hash(db, &run.instance_name, key, hash_hex).await?;
    pin_decision_hash(db, &meta.proposal_cid, hash_hex).await?;
    Ok(true)
}

/// Add `hash_hex` to the `proposal_decisions` pins of `cid` when this node
/// made a decision on it (a member did; a proposer did not).
///
/// TODO(engine/mod.rs): this belongs next to `pin_topology_hash`.
async fn pin_decision_hash(db: &SqlitePool, cid: &str, hash_hex: &str) -> Result<()> {
    let Some(decision) = db.get_proposal_decision(cid).await? else {
        return Ok(());
    };
    if decision.pinned_hashes.iter().any(|h| h == hash_hex) {
        return Ok(());
    }
    let mut pinned = decision.pinned_hashes;
    pinned.push(hash_hex.to_string());
    let mut tx = db.begin_transaction().await?;
    tx.set_proposal_pinned_hashes(cid, &pinned).await?;
    Commitable::commit(tx).await
}

/// Co-sign `pending` after the step-5 re-read and move to `next_step`.
/// `NotFound` stays in the step for the next poll (design D5 step 6).
async fn cosign_and_advance<M>(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    key: &str,
    pending: &PendingProposal<M>,
    next_step: &str,
) -> Result<()> {
    if !recheck_and_pin(ctx, run, meta, key, &pending.hash_hex).await? {
        return Ok(());
    }
    let outcome = topology::cosign_by_hash(
        ctx.ol.config(),
        &ctx.sync_id,
        &pending.hash_hex,
        &pending.signed_by,
    )
    .await?;
    match outcome {
        CosignOutcome::NotFound => {
            tracing::debug!(hash = %pending.hash_hex, "proposal not in the store yet; retry next tick");
            Ok(())
        }
        CosignOutcome::Signed | CosignOutcome::AlreadySigned => {
            tracing::info!(instance = %run.instance_name, hash = %pending.hash_hex, ?outcome, "{key} co-signed");
            advance_step(ctx.db(), run, next_step).await
        }
    }
}

/// The section-5 reference set for this node: the accepted proposal, its
/// counted acceptances, no head state (the party does not exist yet), this
/// node's vault identity, the owners' root keys, and the proposer hosting
/// check (design D2).
async fn member_expectations(
    ctx: &TickCtx<'_>,
    proposal: &ActiveProposal,
    counted: &[Acceptance],
    owner_keys: BTreeMap<String, SigningPublicKey>,
) -> Result<Expectations> {
    let record = &proposal.record;
    let identity =
        keys::local_identity_for_party(ctx.ol.config(), ctx.db(), None, record.prefix.as_deref())
            .await?;
    let proposer_participant = CantonId::parse(&record.proposer_participant)
        .with_context(|| format!("proposerParticipant `{}`", record.proposer_participant))?;
    let hosting_ok = verify_hosting(ctx.ol.config(), &record.proposer, &proposer_participant)
        .await?
        .has_submission();
    Ok(
        Expectations::new(record, counted, HeadState::default(), identity)
            .with_owner_keys(owner_keys)
            .with_proposer_hosting(hosting_ok),
    )
}

/// The root-NSD key of every owner, or `None` while one is still missing
/// from the synchronizer store.
async fn owner_keys_once(
    ctx: &TickCtx<'_>,
    owners: &[String],
) -> Result<Option<BTreeMap<String, SigningPublicKey>>> {
    match topology::wait_owner_root_delegations(ctx.ol.config(), &ctx.sync_id, owners, check_once())
        .await
    {
        Ok(keys) => Ok(Some(keys)),
        Err(e) => {
            tracing::debug!(error = %e, "waiting for the owners' root NamespaceDelegations");
            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------------
// Coordinator steps
// ---------------------------------------------------------------------------

/// `GenerateKeys`: the key exists since `prepare`; make sure the vault still
/// holds the key the proposal advertises, then move on.
async fn coordinator_generate_keys(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &WorkflowProposalRecord,
) -> Result<()> {
    let prefix = required_prefix(proposal)?;
    let advertised = required_proposer_fingerprint(proposal)?;
    let key = keys::ensure_party_key(ctx.ol.config(), prefix).await?;
    if key.fingerprint != advertised {
        return fail_coordinator(
            ctx,
            run,
            meta,
            format!(
                "the vault key {} now fingerprints to {}, not the advertised {advertised}",
                keys::party_key_name(prefix),
                key.fingerprint
            ),
        )
        .await;
    }
    advance_step(ctx.db(), run, STEP_WAITING_FOR_ACCEPTANCES).await
}

/// `WaitingForAcceptances`: every invitee must have a counted acceptance
/// (section 6 quorum for onboarding).
async fn coordinator_wait_acceptances(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &ActiveProposal,
) -> Result<()> {
    let counted = match count_acceptances(ctx, proposal).await {
        Ok(counted) => counted,
        Err(reason) => return fail_coordinator(ctx, run, meta, reason).await,
    };
    record_connected_peers(ctx.db(), run, accepted_participants(&counted)).await?;
    let missing = missing_invitees(&proposal.record, &counted);
    if !missing.is_empty() {
        tracing::debug!(instance = %run.instance_name, ?missing, "waiting for acceptances");
        return Ok(());
    }
    advance_step(ctx.db(), run, STEP_PROPOSE_NAMESPACE).await
}

/// `ProposeNamespace`: wait until every owner's root delegation is
/// effective, then propose the DND at serial 1 and pin its hash. Re-entrant:
/// an existing DND or a proposal already signed by this node moves on.
async fn coordinator_propose_namespace(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &ActiveProposal,
) -> Result<()> {
    let Some(plan) = coordinator_plan(ctx, run, meta, proposal).await? else {
        return Ok(());
    };
    let config = ctx.ol.config();
    let sync_id = &ctx.sync_id;

    if let Some(existing) = topology::read_accepted_dnd(config, sync_id, &plan.namespace).await? {
        if let Err(e) = check_existing_dnd(&existing.mapping, &plan.owners, plan.threshold) {
            return fail_coordinator(ctx, run, meta, format!("{e:#}")).await;
        }
        tracing::info!(namespace = %plan.namespace, serial = existing.serial, "DND already exists");
        return advance_step(ctx.db(), run, STEP_AWAIT_NAMESPACE).await;
    }

    let my_fp = required_proposer_fingerprint(&proposal.record)?;
    let pending = topology::list_pending_dnd(config, sync_id, &plan.namespace).await?;
    if let Some(mine) = pending
        .iter()
        .find(|p| p.is_add_replace() && p.serial == FIRST_SERIAL && p.is_signed_by(my_fp))
    {
        pin_topology_hash(ctx.db(), &run.instance_name, HASH_DND, &mine.hash_hex).await?;
        return advance_step(ctx.db(), run, STEP_AWAIT_NAMESPACE).await;
    }

    if owner_keys_once(ctx, &plan.owners).await?.is_none() {
        return Ok(());
    }
    let proposed = topology::propose_mapping(
        config,
        sync_id,
        topology::build_dnd(&plan.owners, plan.threshold),
        FIRST_SERIAL,
    )
    .await?;
    pin_topology_hash(ctx.db(), &run.instance_name, HASH_DND, &proposed.hash_hex).await?;
    advance_step(ctx.db(), run, STEP_AWAIT_NAMESPACE).await
}

/// The hashes of every pending proposal for `key`.
async fn pending_hashes(ctx: &TickCtx<'_>, key: &MappingKey) -> Result<Vec<String>> {
    let config = ctx.ol.config();
    let sync_id = &ctx.sync_id;
    Ok(match key {
        MappingKey::Dnd(namespace) => topology::list_pending_dnd(config, sync_id, namespace)
            .await?
            .into_iter()
            .map(|p| p.hash_hex)
            .collect(),
        MappingKey::P2p(party) => topology::list_pending_p2p(config, sync_id, party)
            .await?
            .into_iter()
            .map(|p| p.hash_hex)
            .collect(),
    })
}

/// `AwaitNamespace` and `AwaitParty`: one effectiveness check per tick.
/// When the pinned proposal has left the store, go back one step so the
/// proposer re-issues it (design D10, retry as "ensure"). Returns whether
/// the mapping is effective.
async fn await_or_repropose(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    key: &MappingKey,
    hash_key: &str,
    propose_step: &str,
) -> Result<bool> {
    let waited = topology::wait_effective(
        ctx.ol.config(),
        &ctx.sync_id,
        key,
        FIRST_SERIAL,
        check_once(),
    )
    .await;
    let Err(e) = waited else {
        return Ok(true);
    };
    tracing::debug!(%key, error = %e, "not effective yet");
    let pinned = meta.topology_hashes.get(hash_key).map(String::as_str);
    let now = now_secs();
    if !repropose_due(pinned, run.updated_at, now) {
        return Ok(false);
    }
    let pending = pending_hashes(ctx, key).await?;
    if should_repropose(pinned, &pending, run.updated_at, now) {
        tracing::warn!(%key, pinned, "pinned proposal left the store; re-proposing");
        advance_step(ctx.db(), run, propose_step).await?;
    }
    Ok(false)
}

async fn coordinator_await_namespace(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &ActiveProposal,
) -> Result<()> {
    let Some(plan) = coordinator_plan(ctx, run, meta, proposal).await? else {
        return Ok(());
    };
    let key = MappingKey::Dnd(plan.namespace.clone());
    if await_or_repropose(ctx, run, meta, &key, HASH_DND, STEP_PROPOSE_NAMESPACE).await? {
        advance_step(ctx.db(), run, STEP_PROPOSE_PARTY).await?;
    }
    Ok(())
}

/// `ProposeParty`: read every owner's root key, build the bootstrap P2P,
/// propose it at serial 1, pin its hash. Re-entrant like the DND step.
async fn coordinator_propose_party(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &ActiveProposal,
) -> Result<()> {
    let Some(plan) = coordinator_plan(ctx, run, meta, proposal).await? else {
        return Ok(());
    };
    let config = ctx.ol.config();
    let sync_id = &ctx.sync_id;

    if let Some(existing) = topology::read_accepted_p2p(config, sync_id, &plan.party).await? {
        tracing::info!(party = %plan.party, serial = existing.serial, "P2P already exists");
        return advance_step(ctx.db(), run, STEP_AWAIT_PARTY).await;
    }

    let my_fp = required_proposer_fingerprint(&proposal.record)?;
    let pending = topology::list_pending_p2p(config, sync_id, &plan.party).await?;
    if let Some(mine) = pending
        .iter()
        .find(|p| p.is_add_replace() && p.serial == FIRST_SERIAL && p.is_signed_by(my_fp))
    {
        pin_topology_hash(ctx.db(), &run.instance_name, HASH_P2P, &mine.hash_hex).await?;
        return advance_step(ctx.db(), run, STEP_AWAIT_PARTY).await;
    }

    let Some(owner_keys) = owner_keys_once(ctx, &plan.owners).await? else {
        return Ok(());
    };
    let keys: Vec<SigningPublicKey> = owner_keys.into_values().collect();
    let mapping = topology::build_bootstrap_p2p(
        &plan.party.prefix,
        &plan.namespace,
        &host_participants(&plan.members),
        &keys,
        plan.threshold,
    );
    let proposed = topology::propose_mapping(config, sync_id, mapping, FIRST_SERIAL).await?;
    pin_topology_hash(ctx.db(), &run.instance_name, HASH_P2P, &proposed.hash_hex).await?;
    advance_step(ctx.db(), run, STEP_AWAIT_PARTY).await
}

/// `AwaitParty`: when the P2P is effective, fill the local cache, complete
/// the row, and finish the proposal. The row goes first so a failed
/// `Finish` cannot leave a created party marked failed; the proposal then
/// expires on its own.
async fn coordinator_await_party(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &ActiveProposal,
) -> Result<()> {
    let Some(plan) = coordinator_plan(ctx, run, meta, proposal).await? else {
        return Ok(());
    };
    let key = MappingKey::P2p(plan.party.clone());
    if !await_or_repropose(ctx, run, meta, &key, HASH_P2P, STEP_PROPOSE_PARTY).await? {
        return Ok(());
    }
    persist_party_cache(ctx.db(), &plan, &ctx.participant_id).await?;
    set_run_party(ctx.db(), run, &plan.party).await?;
    complete_run(ctx.db(), run).await?;
    finish_best_effort(ctx.client, &meta.proposal_cid, true, None).await;
    tracing::info!(instance = %run.instance_name, party = %plan.party, "decentralized party created");
    Ok(())
}

// ---------------------------------------------------------------------------
// Member steps
// ---------------------------------------------------------------------------

/// `GenerateKeys`: mint the dual key, wait until its root delegation is in
/// the synchronizer store, accept with the key material (design D6).
/// Re-entrant: an acceptance already on the ledger is not repeated.
async fn member_generate_keys(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &ActiveProposal,
) -> Result<()> {
    let prefix = match required_prefix(&proposal.record) {
        Ok(prefix) => prefix,
        Err(e) => return fail_run(ctx.db(), run, &format!("{e:#}")).await,
    };
    let key = keys::ensure_party_key(ctx.ol.config(), prefix).await?;
    if let Err(e) = keys::wait_own_root_delegation(
        ctx.ol.config(),
        &ctx.sync_id,
        &key.fingerprint,
        check_once(),
    )
    .await
    {
        tracing::debug!(error = %e, "waiting for this node's root NamespaceDelegation");
        return Ok(());
    }
    let me = ctx.client.node_party();
    let already = ctx
        .proposals
        .acceptances_for(&meta.proposal_cid)
        .iter()
        .any(|a| a.record.acceptor == *me);
    if !already {
        let args = super::accept_args(ctx.identity, &keys::proposer_key_material(&key), None);
        let cid = proposals::accept(ctx.client, &meta.proposal_cid, &args).await?;
        tracing::info!(instance = %run.instance_name, acceptance = %cid, "invitation accepted on the ledger");
    }
    advance_step(ctx.db(), run, STEP_COSIGN_NAMESPACE).await
}

/// The inputs both co-sign steps share, or `None` while the member waits.
async fn member_plan(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    proposal: &ActiveProposal,
) -> Result<Option<(Vec<Acceptance>, PartyPlan)>> {
    let counted = match count_acceptances(ctx, proposal).await {
        Ok(counted) => counted,
        Err(reason) => {
            fail_run(ctx.db(), run, &reason).await?;
            return Ok(None);
        }
    };
    // A DND with fewer owners than participants never validates, and an
    // acceptance can reach this node after the topology proposal does. Wait
    // for the full set before judging anything.
    let missing = missing_invitees(&proposal.record, &counted);
    if !missing.is_empty() {
        tracing::debug!(instance = %run.instance_name, ?missing, "waiting for the other acceptances");
        return Ok(None);
    }
    match party_plan(&proposal.record, &counted) {
        Ok(plan) => Ok(Some((counted, plan))),
        Err(e) => {
            fail_run(ctx.db(), run, &format!("{e:#}")).await?;
            Ok(None)
        }
    }
}

/// `CoSignNamespace`: validate and co-sign the DND (section 5), or move on
/// when it is already accepted.
async fn member_cosign_namespace(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &ActiveProposal,
) -> Result<()> {
    let Some((counted, plan)) = member_plan(ctx, run, proposal).await? else {
        return Ok(());
    };
    let config = ctx.ol.config();
    let sync_id = &ctx.sync_id;

    if let Some(existing) = topology::read_accepted_dnd(config, sync_id, &plan.namespace).await? {
        if let Err(e) = check_existing_dnd(&existing.mapping, &plan.owners, plan.threshold) {
            return fail_run(ctx.db(), run, &format!("{e:#}")).await;
        }
        return advance_step(ctx.db(), run, STEP_COSIGN_PARTY).await;
    }

    let exp = member_expectations(ctx, proposal, &counted, BTreeMap::new()).await?;
    let proposer_fp = match exp.required_proposer_fingerprint() {
        Ok(fp) => fp.to_string(),
        Err(e) => return fail_run(ctx.db(), run, &e.0).await,
    };
    let pending = topology::list_pending_dnd(config, sync_id, &plan.namespace).await?;
    match select_pending(
        &pending,
        &proposer_fp,
        &exp.identity.owner_fingerprints,
        |p| validation::validate_dnd(p, &exp, None),
    ) {
        Selection::Waiting => Ok(()),
        Selection::AlreadySigned(_) => advance_step(ctx.db(), run, STEP_COSIGN_PARTY).await,
        Selection::Mismatch(reasons) => {
            fail_run(
                ctx.db(),
                run,
                &format!("DND proposal rejected: {}", reasons.join("; ")),
            )
            .await
        }
        Selection::Match(p) => {
            cosign_and_advance(ctx, run, meta, HASH_DND, p, STEP_COSIGN_PARTY).await
        }
    }
}

/// `CoSignParty`: validate and co-sign the P2P (section 5), or move on when
/// it is already accepted.
async fn member_cosign_party(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &ActiveProposal,
) -> Result<()> {
    let Some((counted, plan)) = member_plan(ctx, run, proposal).await? else {
        return Ok(());
    };
    let config = ctx.ol.config();
    let sync_id = &ctx.sync_id;

    // Write the cache here, not only in `Complete`. The proposer finishes
    // the WorkflowProposal as soon as the party is effective, and a member
    // that reads the outcome first completes its row through `reconcile`
    // without ever running `Complete`. It would then hold no owner key for
    // its peers, and the next kick on this party would fail on this node.
    // The member has validated this membership by now and is about to sign
    // it, so recording it early records what it endorsed.
    persist_party_cache(ctx.db(), &plan, &ctx.participant_id).await?;

    if topology::read_accepted_p2p(config, sync_id, &plan.party)
        .await?
        .is_some()
    {
        return advance_step(ctx.db(), run, super::COMPLETE_STEP).await;
    }

    let Some(owner_keys) = owner_keys_once(ctx, &plan.owners).await? else {
        return Ok(());
    };
    let exp = member_expectations(ctx, proposal, &counted, owner_keys).await?;
    let proposer_fp = match exp.required_proposer_fingerprint() {
        Ok(fp) => fp.to_string(),
        Err(e) => return fail_run(ctx.db(), run, &e.0).await,
    };
    let pending = topology::list_pending_p2p(config, sync_id, &plan.party).await?;
    match select_pending(
        &pending,
        &proposer_fp,
        &exp.identity.owner_fingerprints,
        |p| validation::validate_p2p(p, &exp, None),
    ) {
        Selection::Waiting => Ok(()),
        Selection::AlreadySigned(_) => advance_step(ctx.db(), run, super::COMPLETE_STEP).await,
        Selection::Mismatch(reasons) => {
            fail_run(
                ctx.db(),
                run,
                &format!("P2P proposal rejected: {}", reasons.join("; ")),
            )
            .await
        }
        Selection::Match(p) => {
            cosign_and_advance(ctx, run, meta, HASH_P2P, p, super::COMPLETE_STEP).await
        }
    }
}

/// `Complete`: when the P2P is effective, fill the local cache and complete
/// the row.
async fn member_complete(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    proposal: &ActiveProposal,
) -> Result<()> {
    let Some((_, plan)) = member_plan(ctx, run, proposal).await? else {
        return Ok(());
    };
    let accepted = topology::read_accepted_p2p(ctx.ol.config(), &ctx.sync_id, &plan.party).await?;
    if !accepted.is_some_and(|a| a.is_effective(ctx.now_micros)) {
        tracing::debug!(party = %plan.party, "P2P not effective yet");
        return Ok(());
    }
    persist_party_cache(ctx.db(), &plan, &ctx.participant_id).await?;
    set_run_party(ctx.db(), run, &plan.party).await?;
    complete_run(ctx.db(), run).await?;
    tracing::info!(instance = %run.instance_name, party = %plan.party, "joined the decentralized party");
    Ok(())
}

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

impl KindDriver for Onboarding {
    fn kind() -> WorkflowKind {
        WorkflowKind::Onboarding
    }

    /// This kind's member attaches key material a member step generates,
    /// so it exercises `Accept` itself rather than through [`super::drive`].
    fn member_publishes_own_acceptance() -> bool {
        true
    }

    fn coordinator_steps() -> &'static [&'static str] {
        COORDINATOR_STEPS
    }

    fn member_steps(_variant: Option<MemberVariant>) -> &'static [&'static str] {
        MEMBER_STEPS
    }

    /// Section 6 preflight: the prefix is free, and the threshold (the
    /// majority default when absent) fits the member count. The HTTP
    /// handler checks the prefix too; this gate holds when it does not.
    async fn preflight(ol: &OnLedger, req: &StartRequest) -> Result<()> {
        let StartRequest::Onboarding {
            party_id_prefix,
            peer_ids,
            threshold,
            ..
        } = req
        else {
            return Ok(());
        };
        if let Some(existing) = ol
            .db()
            .get_dec_parties_by_prefix(party_id_prefix)
            .await?
            .into_iter()
            .next()
        {
            return Err(PreflightRejected::new(format!(
                "A decentralized party with the prefix '{party_id_prefix}' already exists \
                 ({}). Choose a different prefix.",
                existing.party_id
            ))
            .into());
        }
        let me = ol.require_identity().await?.participant_id;
        let members = peer_ids
            .iter()
            .filter(|p| **p != me)
            .collect::<BTreeSet<_>>()
            .len()
            + 1;
        let resolved = match threshold {
            None => default_threshold(members),
            Some(t) => u32::try_from(*t).unwrap_or(0),
        };
        if resolved < 1 || usize::try_from(resolved).unwrap_or(usize::MAX) > members {
            return Err(PreflightRejected::new(format!(
                "threshold must be between 1 and {members} (invited peers + this node); got {resolved}"
            ))
            .into());
        }
        Ok(())
    }

    /// Design D4/D6: the proposer's dual-usage key exists before the
    /// proposal, so the proposal can carry its fingerprint and key bytes.
    async fn prepare(ol: &OnLedger, req: &StartRequest) -> Result<ProposalExtras> {
        let StartRequest::Onboarding {
            party_id_prefix, ..
        } = req
        else {
            bail!("onboarding prepare called with a {} request", req.kind());
        };
        let key = keys::ensure_party_key(ol.config(), party_id_prefix).await?;
        Ok(ProposalExtras {
            keys: keys::proposer_key_material(&key),
            ..ProposalExtras::default()
        })
    }

    async fn tick_coordinator(ctx: &TickCtx<'_>, run: &WorkflowRun, meta: &RunMeta) -> Result<()> {
        let Some(proposal) = proposal_of(ctx, meta) else {
            return Ok(());
        };
        match run.current_step.as_str() {
            STEP_GENERATE_KEYS => coordinator_generate_keys(ctx, run, meta, &proposal.record).await,
            STEP_WAITING_FOR_ACCEPTANCES => {
                coordinator_wait_acceptances(ctx, run, meta, proposal).await
            }
            STEP_PROPOSE_NAMESPACE => coordinator_propose_namespace(ctx, run, meta, proposal).await,
            STEP_AWAIT_NAMESPACE => coordinator_await_namespace(ctx, run, meta, proposal).await,
            STEP_PROPOSE_PARTY => coordinator_propose_party(ctx, run, meta, proposal).await,
            STEP_AWAIT_PARTY => coordinator_await_party(ctx, run, meta, proposal).await,
            // A row at `Complete` that is still in progress lost its status
            // write; finish it.
            super::COMPLETE_STEP => complete_run(ctx.db(), run).await,
            other => {
                fail_coordinator(
                    ctx,
                    run,
                    meta,
                    format!("unknown onboarding coordinator step `{other}`"),
                )
                .await
            }
        }
    }

    async fn tick_member(ctx: &TickCtx<'_>, run: &WorkflowRun, meta: &RunMeta) -> Result<()> {
        let Some(proposal) = proposal_of(ctx, meta) else {
            return Ok(());
        };
        match run.current_step.as_str() {
            STEP_GENERATE_KEYS => member_generate_keys(ctx, run, meta, proposal).await,
            STEP_COSIGN_NAMESPACE => member_cosign_namespace(ctx, run, meta, proposal).await,
            STEP_COSIGN_PARTY => member_cosign_party(ctx, run, meta, proposal).await,
            super::COMPLETE_STEP => member_complete(ctx, run, proposal).await,
            other => {
                fail_run(
                    ctx.db(),
                    run,
                    &format!("unknown onboarding member step `{other}`"),
                )
                .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::enums::TopologyChangeOp;

    use super::*;
    use crate::{
        db::MIGRATOR,
        onledger::{
            daml::codec::tests::{acceptance_full, party, proposal_full},
            validation::ValidationError,
        },
    };

    const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

    fn participant(n: u8) -> CantonId {
        CantonId::parse(&format!("participant{n}::{NS}")).expect("id")
    }

    /// The onboarding proposal: proposer on participant 1, invitees node-b
    /// (participant 2) and node-c (participant 3), threshold 2.
    fn proposal() -> WorkflowProposalRecord {
        WorkflowProposalRecord {
            dec_party_id: None,
            previous_threshold: None,
            dnd_base_serial: None,
            p2p_base_serial: None,
            new_participant: None,
            kicked_participant: None,
            dar_pins: vec![],
            package_names: vec![],
            ..proposal_full()
        }
    }

    fn acceptance(
        acceptor: &str,
        participant_n: u8,
        owner: &str,
        daml: Option<&str>,
    ) -> Acceptance {
        let mut record = acceptance_full("00proposal", acceptor);
        record.participant_id = participant(participant_n).to_string();
        record.namespace_fingerprint = Some(owner.into());
        record.daml_key_fingerprint = daml.map(str::to_string);
        Acceptance {
            contract_id: format!("00acc-{acceptor}"),
            offset: 1,
            record,
        }
    }

    fn both_accepted() -> Vec<Acceptance> {
        vec![
            acceptance("node-b", 2, "1220cc", Some("1220cc")),
            acceptance("node-c", 3, "1220ee", None),
        ]
    }

    fn pending(
        hash: &str,
        serial: u32,
        signed_by: &[&str],
        op: TopologyChangeOp,
    ) -> PendingProposal<()> {
        PendingProposal {
            hash_hex: hash.into(),
            serial,
            signed_by: signed_by.iter().map(|s| s.to_string()).collect(),
            operation: op as i32,
            mapping: (),
            sequenced: None,
            valid_from: None,
        }
    }

    const ADD_REPLACE: TopologyChangeOp = TopologyChangeOp::AddReplace;
    const REMOVE: TopologyChangeOp = TopologyChangeOp::Remove;

    #[test]
    fn default_threshold_is_a_simple_majority() {
        assert_eq!(default_threshold(0), 1);
        assert_eq!(default_threshold(1), 1);
        assert_eq!(default_threshold(2), 1);
        assert_eq!(default_threshold(3), 2);
        assert_eq!(default_threshold(4), 2);
        assert_eq!(default_threshold(5), 3);
    }

    #[test]
    fn proposal_threshold_fails_closed() {
        assert_eq!(proposal_threshold(&proposal()).expect("ok"), 2);
        let mut p = proposal();
        p.threshold = None;
        assert!(proposal_threshold(&p).is_err());
        p.threshold = Some(0);
        assert!(proposal_threshold(&p).is_err());
        p.threshold = Some(4);
        assert!(proposal_threshold(&p).is_err(), "3 participants");
    }

    #[test]
    fn member_keys_lists_the_proposer_first_then_the_acceptors() {
        let members = member_keys(&proposal(), &both_accepted()).expect("members");
        assert_eq!(
            members,
            vec![
                MemberKeys {
                    participant: participant(1),
                    owner_fingerprint: "1220aa".into(),
                    daml_key_fingerprint: Some("1220bb".into()),
                },
                MemberKeys {
                    participant: participant(2),
                    owner_fingerprint: "1220cc".into(),
                    daml_key_fingerprint: Some("1220cc".into()),
                },
                MemberKeys {
                    participant: participant(3),
                    owner_fingerprint: "1220ee".into(),
                    daml_key_fingerprint: None,
                },
            ]
        );
        assert_eq!(
            owner_fingerprints(&members),
            vec!["1220aa", "1220cc", "1220ee"]
        );
        assert_eq!(
            host_participants(&members),
            vec![participant(1), participant(2), participant(3)]
        );
    }

    #[test]
    fn member_keys_fails_closed_on_bad_key_material() {
        let mut no_fp = both_accepted();
        no_fp[1].record.namespace_fingerprint = None;
        assert!(member_keys(&proposal(), &no_fp).is_err(), "missing owner");

        let mut dup = both_accepted();
        dup[1].record.namespace_fingerprint = Some("1220cc".into());
        assert!(member_keys(&proposal(), &dup).is_err(), "repeated owner");

        let mut same_participant = both_accepted();
        same_participant[1].record.participant_id = participant(2).to_string();
        assert!(
            member_keys(&proposal(), &same_participant).is_err(),
            "repeated participant"
        );

        let mut bad_id = both_accepted();
        bad_id[0].record.participant_id = "not-a-canton-id".into();
        assert!(member_keys(&proposal(), &bad_id).is_err());

        let mut no_proposer_fp = proposal();
        no_proposer_fp.proposer_namespace_fingerprint = None;
        assert!(member_keys(&no_proposer_fp, &both_accepted()).is_err());
    }

    #[test]
    fn party_id_is_the_prefix_and_the_derived_namespace() {
        let owners = vec!["1220aa".to_string(), "1220cc".to_string()];
        let party = party_id_for("cbtc", &owners).expect("party");
        assert_eq!(party.prefix, "cbtc");
        assert_eq!(
            party.namespace.to_hex(),
            topology::compute_namespace(owners.iter())
        );
        // Owner order does not change the namespace.
        let reversed = vec!["1220cc".to_string(), "1220aa".to_string()];
        assert_eq!(party_id_for("cbtc", &reversed).expect("party"), party);
    }

    #[test]
    fn missing_invitees_are_those_without_a_counted_acceptance() {
        let p = proposal();
        assert_eq!(
            missing_invitees(&p, &[]),
            vec![party("node-b"), party("node-c")]
        );
        assert_eq!(
            missing_invitees(&p, &both_accepted()[..1]),
            vec![party("node-c")]
        );
        assert!(missing_invitees(&p, &both_accepted()).is_empty());
        assert_eq!(
            accepted_participants(&both_accepted()),
            vec![participant(2), participant(3)]
        );
    }

    #[test]
    fn existing_dnd_must_match_the_accepted_owners_and_threshold() {
        let owners = vec!["1220aa".to_string(), "1220cc".to_string()];
        let dnd = topology::dnd_of(&topology::build_dnd(&owners, 2))
            .expect("dnd")
            .clone();
        assert!(check_existing_dnd(&dnd, &owners, 2).is_ok());
        assert!(check_existing_dnd(&dnd, &owners, 1).is_err(), "threshold");
        assert!(
            check_existing_dnd(&dnd, &["1220aa".to_string()], 2).is_err(),
            "owners"
        );
    }

    #[test]
    fn select_pending_waits_without_a_proposer_signed_candidate() {
        let own = BTreeSet::from(["1220me".to_string()]);
        let list = vec![
            pending("h1", 1, &["1220other"], ADD_REPLACE),
            pending("h2", 1, &["1220proposer"], REMOVE),
        ];
        assert!(matches!(
            select_pending(&list, "1220proposer", &own, |_| Ok(())),
            Selection::Waiting
        ));
        let empty: [PendingProposal<()>; 0] = [];
        assert!(matches!(
            select_pending(&empty, "1220proposer", &own, |_| Ok(())),
            Selection::Waiting
        ));
    }

    #[test]
    fn select_pending_prefers_an_already_signed_candidate() {
        let own = BTreeSet::from(["1220me".to_string()]);
        let list = vec![
            pending("h1", 1, &["1220proposer"], ADD_REPLACE),
            pending("h2", 1, &["1220proposer", "1220me"], ADD_REPLACE),
        ];
        let selection = select_pending(&list, "1220proposer", &own, |_| Ok(()));
        assert!(
            matches!(selection, Selection::AlreadySigned(p) if p.hash_hex == "h2"),
            "{selection:?}"
        );
    }

    #[test]
    fn select_pending_skips_a_stale_candidate_and_matches_the_valid_one() {
        let own = BTreeSet::from(["1220me".to_string()]);
        let list = vec![
            pending("stale", 1, &["1220proposer"], ADD_REPLACE),
            pending("good", 1, &["1220proposer"], ADD_REPLACE),
        ];
        let check = |p: &PendingProposal<()>| -> Check {
            if p.hash_hex == "stale" {
                Err(ValidationError("threshold differs".into()))
            } else {
                Ok(())
            }
        };
        let selection = select_pending(&list, "1220proposer", &own, check);
        assert!(
            matches!(selection, Selection::Match(p) if p.hash_hex == "good"),
            "{selection:?}"
        );
    }

    #[test]
    fn select_pending_reports_every_reason_when_nothing_passes() {
        let own = BTreeSet::new();
        let list = vec![
            pending("h1", 1, &["1220proposer"], ADD_REPLACE),
            pending("h2", 1, &["1220proposer"], ADD_REPLACE),
        ];
        let selection = select_pending(&list, "1220proposer", &own, |p| {
            Err(ValidationError(format!("bad {}", p.hash_hex)))
        });
        match selection {
            Selection::Mismatch(reasons) => {
                assert_eq!(reasons.len(), 2);
                assert!(reasons[0].contains("h1") && reasons[0].contains("bad h1"));
                assert!(reasons[1].contains("h2"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn repropose_only_after_the_grace_when_the_pin_is_gone() {
        let present = vec!["h1".to_string(), "h2".to_string()];
        let now = 1_000_000;
        let old = now - REPROPOSE_GRACE_SECS;
        let fresh = now - 1;
        assert!(!repropose_due(None, old, now), "no pin");
        assert!(!repropose_due(Some("h9"), fresh, now), "in grace");
        assert!(repropose_due(Some("h9"), old, now), "grace is inclusive");
        assert!(!should_repropose(None, &[], old, now), "no pin");
        assert!(
            !should_repropose(Some("h1"), &present, old, now),
            "still there"
        );
        assert!(
            !should_repropose(Some("h9"), &present, fresh, now),
            "in grace"
        );
        assert!(should_repropose(Some("h9"), &present, old, now));
        assert!(should_repropose(Some("h9"), &[], old, now));
    }

    #[test]
    fn check_once_reads_once_without_sleeping() {
        let budget = check_once();
        assert_eq!(budget.max_attempts, 1);
        assert_eq!(budget.delay, Duration::ZERO);
    }

    #[test]
    fn step_lists_match_design_section_6() {
        assert_eq!(
            COORDINATOR_STEPS,
            [
                "GenerateKeys",
                "WaitingForAcceptances",
                "ProposeNamespace",
                "AwaitNamespace",
                "ProposeParty",
                "AwaitParty",
                "Complete"
            ]
        );
        assert_eq!(
            MEMBER_STEPS,
            ["GenerateKeys", "CoSignNamespace", "CoSignParty", "Complete"]
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn persist_party_cache_writes_rows_owners_and_member_keys(
        pool: SqlitePool,
    ) -> Result<()> {
        let plan = party_plan(&proposal(), &both_accepted())?;
        persist_party_cache(&pool, &plan, &participant(2)).await?;

        let parties = pool.get_dec_parties_by_prefix("cbtc").await?;
        assert_eq!(parties.len(), 1);
        assert_eq!(parties[0].party_id, plan.party.to_string());
        assert_eq!(parties[0].threshold, 2);
        assert_eq!(parties[0].my_owner_key.as_deref(), Some("1220cc"));

        let mut owners = pool.get_dec_party_owners(&plan.party).await?;
        owners.sort();
        assert_eq!(owners, plan.owners);

        let rows = pool.get_dec_party_participants(&plan.party).await?;
        assert_eq!(rows.len(), 3);
        let by_uid = |uid: &CantonId| {
            rows.iter()
                .find(|r| r.participant_uid == uid.to_string())
                .expect("row")
        };
        let p1 = by_uid(&participant(1));
        assert_eq!(p1.permission, "confirmation");
        assert_eq!(p1.owner_key.as_deref(), Some("1220aa"));
        assert_eq!(p1.signing_key.as_deref(), Some("1220bb"));
        let p3 = by_uid(&participant(3));
        assert_eq!(p3.owner_key.as_deref(), Some("1220ee"));
        assert_eq!(p3.signing_key, None, "no Daml key claimed");

        // A second write is idempotent.
        persist_party_cache(&pool, &plan, &participant(2)).await?;
        assert_eq!(pool.get_dec_party_participants(&plan.party).await?.len(), 3);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn pin_decision_hash_appends_once_and_skips_unknown_proposals(
        pool: SqlitePool,
    ) -> Result<()> {
        use crate::db::rows::{ProposalDecision, ProposalDecisionEntry};

        pin_decision_hash(&pool, "00unknown", "1220ab").await?;
        assert!(pool.get_proposal_decision("00unknown").await?.is_none());

        let mut tx = pool.begin_transaction().await?;
        tx.insert_proposal_decision(&ProposalDecisionEntry {
            proposal_cid: "00proposal".into(),
            decision: ProposalDecision::Accepted,
            decided_at: 1,
            pinned_hashes: vec![],
        })
        .await?;
        Commitable::commit(tx).await?;

        pin_decision_hash(&pool, "00proposal", "1220ab").await?;
        pin_decision_hash(&pool, "00proposal", "1220ab").await?;
        pin_decision_hash(&pool, "00proposal", "1220cd").await?;
        let entry = pool
            .get_proposal_decision("00proposal")
            .await?
            .expect("decision");
        assert_eq!(entry.pinned_hashes, vec!["1220ab", "1220cd"]);
        Ok(())
    }
}
