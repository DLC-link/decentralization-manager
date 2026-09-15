//! Kick driver (design D5, D6, D10, sections 5 and 6): remove a member
//! from a decentralized party.
//!
//! The invitees are the remaining members only; the kicked node sees the
//! topology proposal as unsolicited. Kick is a quorum kind: the coordinator
//! proposes once `acceptances_needed(previousThreshold, threshold)` members
//! accepted, and later acceptances still co-sign. A decline fails the run
//! only when the remaining invitees cannot reach that quorum;
//! `engine::drive` applies that rule before this driver runs.
//!
//! Every tick does one bounded piece of work for the run's current step and
//! returns. Waiting states are steps: a tick reads the accepted mapping
//! once and never sleeps for it. The DND goes first; the P2P is proposed
//! and co-signed only after the DND is effective on this node.
//!
//! The pure decision helpers are public and unit-tested; the async glue is
//! thin and reuses the foundation for every read, write, and check.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use canton_proto_rs::com::digitalasset::canton::protocol::v30::{
    DecentralizedNamespaceDefinition, PartyToParticipant,
};
use common::{
    canton_id::CantonId,
    types::{WorkflowKind, WorkflowProgress, WorkflowRun},
};
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    db::{
        rows::ProposalDecision,
        schema::{Commitable, SchemaRead, SchemaWrite},
    },
    onledger::{
        daml::codec::WorkflowProposalRecord,
        keys,
        proposals::{self, Acceptance, ActiveProposal},
        topology::{self, CosignOutcome, MappingKey, PendingProposal},
        validation::{self, Expectations, HeadState, KickedMember, LocalIdentity},
    },
    utils,
    workflow::signing_keys,
};

use super::{
    COMPLETE_STEP, KindDriver, MemberVariant, OnLedger, PreflightRejected, ProposalExtras,
    ProposerKeyMaterial, RunMeta, StartRequest, TickCtx, WAITING_FOR_ACCEPTANCES_STEP,
    acceptances_needed, advance_step, complete_run, fail_run, pin_topology_hash, read_run_meta,
};

pub struct Kick;

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

/// `RunMeta.topology_hashes` keys.
const DND_HASH: &str = "dnd";
const P2P_HASH: &str = "p2p";

// ---------------------------------------------------------------------------
// Pure decision helpers
// ---------------------------------------------------------------------------

/// The kick a `WorkflowProposal` describes, parsed once. A malformed
/// proposal is a permanent failure, so `parse` returns the reason as text
/// for `fail_run` instead of an `Err` the observer would retry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KickPlan {
    pub party: CantonId,
    /// `party.namespace` as hex: the DND key.
    pub namespace: String,
    pub kicked: CantonId,
    pub threshold: u32,
    pub previous_threshold: Option<i64>,
    pub dnd_base: u32,
    pub p2p_base: u32,
    /// The proposer's owner fingerprint; members require it in `signed_by`.
    pub proposer_fingerprint: String,
}

impl KickPlan {
    pub fn parse(record: &WorkflowProposalRecord) -> Result<Self, String> {
        if record.kind != WorkflowKind::Kick {
            return Err(format!("proposal kind is {}, not Kick", record.kind));
        }
        let party = record
            .dec_party_id
            .as_deref()
            .ok_or_else(|| "the proposal names no decentralized party".to_string())
            .and_then(|s| CantonId::parse(s).map_err(|e| format!("decPartyId `{s}`: {e}")))?;
        let kicked = record
            .kicked_participant
            .as_deref()
            .ok_or_else(|| "the proposal names no kicked participant".to_string())
            .and_then(|s| {
                CantonId::parse(s).map_err(|e| format!("kickedParticipant `{s}`: {e}"))
            })?;
        let threshold = record
            .threshold
            .and_then(|t| u32::try_from(t).ok())
            .filter(|t| *t >= 1)
            .ok_or_else(|| format!("threshold {:?} is not a positive integer", record.threshold))?;
        let serial = |name: &str, v: Option<i64>| {
            v.and_then(|s| u32::try_from(s).ok())
                .filter(|s| *s >= 1)
                .ok_or_else(|| format!("{name} {v:?} is not a positive serial"))
        };
        let dnd_base = serial("dndBaseSerial", record.dnd_base_serial)?;
        let p2p_base = serial("p2pBaseSerial", record.p2p_base_serial)?;
        let proposer_fingerprint = record
            .proposer_namespace_fingerprint
            .clone()
            .filter(|fp| !fp.is_empty())
            .ok_or("the proposal carries no proposer namespace fingerprint")?;
        Ok(Self {
            namespace: party.namespace.to_hex(),
            party,
            kicked,
            threshold,
            previous_threshold: record.previous_threshold,
            dnd_base,
            p2p_base,
            proposer_fingerprint,
        })
    }
}

/// Where the accepted mapping stands relative to the base serial the
/// proposal recorded. `accepted` is `(serial, effective on the local clock)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MappingProgress {
    /// The accepted serial equals the base: the change is not in yet.
    AtBase,
    /// Serial base + 1 is accepted but `valid_from` is still in the future.
    Pending,
    /// Serial base + 1 is accepted and effective.
    Landed,
    /// The accepted serial is neither the base nor base + 1, or the mapping
    /// is gone. The run fails: the topology moved under the proposal.
    Moved { accepted: Option<u32> },
}

pub fn mapping_progress(accepted: Option<(u32, bool)>, base: u32) -> MappingProgress {
    match accepted {
        Some((serial, _)) if serial == base => MappingProgress::AtBase,
        Some((serial, true)) if serial == base + 1 => MappingProgress::Landed,
        Some((serial, false)) if serial == base + 1 => MappingProgress::Pending,
        Some((serial, _)) => MappingProgress::Moved {
            accepted: Some(serial),
        },
        None => MappingProgress::Moved { accepted: None },
    }
}

fn moved_message(key: &MappingKey, base: u32, accepted: Option<u32>) -> String {
    match accepted {
        Some(serial) => format!(
            "topology moved: {key} is accepted at serial {serial}, but the proposal recorded base \
             serial {base}"
        ),
        None => format!("topology moved: {key} has no accepted mapping any more"),
    }
}

/// Section 6 quorum: the coordinator leaves `WaitingForAcceptances` when
/// the counted acceptances reach `max(previous, new) - 1`.
pub fn quorum_reached(counted: usize, previous_threshold: Option<i64>, threshold: u32) -> bool {
    let previous = previous_threshold
        .and_then(|t| u32::try_from(t).ok())
        .unwrap_or(0);
    let needed = acceptances_needed(previous, threshold);
    u32::try_from(counted).unwrap_or(u32::MAX) >= needed
}

/// This node's owner fingerprint for the party: exactly one of its owner
/// keys must own the head DND, or the proposal cannot name a proposer key.
pub fn own_owner_fingerprint(
    identity: &LocalIdentity,
    head: &DecentralizedNamespaceDefinition,
) -> Result<String> {
    let owners: BTreeSet<&String> = head.owners.iter().collect();
    let mine: Vec<&String> = identity
        .owner_fingerprints
        .iter()
        .filter(|fp| owners.contains(fp))
        .collect();
    match mine.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!(
            "this node holds no owner key of namespace {}",
            head.decentralized_namespace
        ),
        many => bail!(
            "this node holds {} owner keys of namespace {}; cannot pick the proposer key",
            many.len(),
            head.decentralized_namespace
        ),
    }
}

/// The cached owner key of the kicked member must still own the head DND.
/// A miss means the cache is stale, or the kick already landed.
pub fn check_kicked_is_owner(
    kicked: &KickedMember,
    head: &DecentralizedNamespaceDefinition,
) -> Result<(), String> {
    if head.owners.contains(&kicked.owner_fingerprint) {
        return Ok(());
    }
    Err(format!(
        "cached owner key {} of {} is not an owner of namespace {} at threshold {}: the cache is \
         stale, or the kick already landed. Try refreshing /decentralized-parties first.",
        kicked.owner_fingerprint,
        kicked.participant_id,
        head.decentralized_namespace,
        head.threshold
    ))
}

/// The party signing key a kick removes (section 5).
///
/// On a dual-usage member the owner fingerprint is also the party signing
/// key, so it is removed directly. On a legacy member the cached Daml key
/// wins when it is on the P2P; otherwise `signing_keys_without_member`
/// attributes by elimination. The result must be exactly one key: the
/// members refuse a P2P that removes none or two.
///
/// # Errors
/// Returns an error when the key cannot be attributed.
pub fn kicked_key_fingerprint(
    head: &PartyToParticipant,
    kicked: &KickedMember,
    claims: &BTreeMap<String, String>,
) -> Result<String> {
    let keys = head
        .party_signing_keys
        .as_ref()
        .map(|k| k.keys.clone())
        .unwrap_or_default();
    let fingerprints: BTreeSet<String> = keys.iter().map(utils::compute_fingerprint).collect();
    if fingerprints.contains(&kicked.owner_fingerprint) {
        return Ok(kicked.owner_fingerprint.clone());
    }
    if let Some(fp) = &kicked.signing_key_fingerprint
        && fingerprints.contains(fp)
    {
        return Ok(fp.clone());
    }

    // The cached row of the kicked member is one more claim for elimination.
    let mut claims = claims.clone();
    if let Some(fp) = &kicked.signing_key_fingerprint {
        claims
            .entry(kicked.participant_id.clone())
            .or_insert(fp.clone());
    }
    let survivors: Vec<String> = head
        .participants
        .iter()
        .map(|h| h.participant_uid.clone())
        .filter(|uid| *uid != kicked.participant_id)
        .collect();
    let remaining = signing_keys::signing_keys_without_member(
        &keys,
        &kicked.participant_id,
        &survivors,
        &claims,
    )?;
    let remaining: BTreeSet<String> = remaining.iter().map(utils::compute_fingerprint).collect();
    let removed: Vec<&String> = fingerprints.difference(&remaining).collect();
    match removed.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!(
            "every party signing key of {} is claimed by a remaining member, so no key can be \
             attributed to {}. Refresh /decentralized-parties so every member reports its signing \
             key, then retry",
            head.party,
            kicked.participant_id
        ),
        many => bail!(
            "{} party signing keys of {} would be removed for {}; a kick removes exactly one",
            many.len(),
            head.party,
            kicked.participant_id
        ),
    }
}

/// Design D5 step 2 plus the spend rule: the pending proposals a member may
/// validate. `ADD_REPLACE`, signed by the proposer, not signed by this
/// node, and equal to the pinned hash when one exists.
pub fn candidates<M>(
    pending: Vec<PendingProposal<M>>,
    proposer_fingerprint: &str,
    own_fingerprints: &BTreeSet<String>,
    pinned: Option<&String>,
) -> Vec<PendingProposal<M>> {
    pending
        .into_iter()
        .filter(|p| p.is_add_replace())
        .filter(|p| p.is_signed_by(proposer_fingerprint))
        .filter(|p| !own_fingerprints.iter().any(|fp| p.is_signed_by(fp)))
        .filter(|p| pinned.is_none_or(|h| *h == p.hash_hex))
        .collect()
}

/// The first candidate that passes `check`. `Ok(None)` when there is no
/// candidate; `Err(reasons)` when candidates exist and none pass, so the
/// member fails its run with the reason (section 5).
pub fn select_valid<M>(
    candidates: &[PendingProposal<M>],
    check: impl Fn(&PendingProposal<M>) -> validation::Check,
) -> Result<Option<&PendingProposal<M>>, String> {
    let mut reasons = Vec::new();
    for candidate in candidates {
        match check(candidate) {
            Ok(()) => return Ok(Some(candidate)),
            Err(e) => reasons.push(format!("{}: {e}", candidate.hash_hex)),
        }
    }
    if reasons.is_empty() {
        Ok(None)
    } else {
        Err(format!(
            "refusing to co-sign the pending topology proposal(s): {}",
            reasons.join("; ")
        ))
    }
}

/// The proposer's own proposal already in the store: signed by its owner
/// key, at `serial`, with exactly the expected mapping. The retry rule
/// (design D10) re-proposes only when this is absent.
pub fn find_own_pending<'a, M: PartialEq>(
    pending: &'a [PendingProposal<M>],
    own_fingerprint: &str,
    serial: u32,
    expected: &M,
) -> Option<&'a PendingProposal<M>> {
    pending.iter().find(|p| {
        p.is_add_replace()
            && p.serial == serial
            && p.is_signed_by(own_fingerprint)
            && p.mapping == *expected
    })
}

/// Whether `me` already accepted `proposal_cid` on the ledger.
pub fn accepted_by(acceptances: &[Acceptance], proposal_cid: &str, me: &CantonId) -> bool {
    acceptances
        .iter()
        .any(|a| a.record.proposal == proposal_cid && a.record.acceptor == *me)
}

/// The one fingerprint of a set, or `None` when the set does not have
/// exactly one. An acceptance carries a fingerprint only when it is
/// unambiguous; the field is informational for a kick.
pub fn sole_fingerprint(fingerprints: &BTreeSet<String>) -> Option<String> {
    match fingerprints.iter().collect::<Vec<_>>().as_slice() {
        [one] => Some((*one).clone()),
        _ => None,
    }
}

fn no_owner_key_message(party: &CantonId, kicked: &CantonId) -> String {
    format!(
        "Participant {kicked} is not present in cached decentralized party {party}, or its owner \
         key has not yet been resolved. Try refreshing /decentralized-parties first."
    )
}

// ---------------------------------------------------------------------------
// Async glue shared by both sides
// ---------------------------------------------------------------------------

/// The kicked member from this node's own cache, without the head-DND
/// cross-check. The P2P phase runs after the kick DND landed, when the
/// kicked fingerprint is no longer an owner, so `keys::kicked_member` would
/// refuse there; the DND phase adds [`check_kicked_is_owner`] itself.
async fn cached_kicked_member(
    db: &SqlitePool,
    party: &CantonId,
    kicked: &CantonId,
) -> Result<Option<KickedMember>> {
    let rows = db.get_dec_party_participants(party).await?;
    Ok(keys::kicked_member_from_rows(&rows, &kicked.to_string()))
}

async fn progress_of(
    config: &NodeConfig,
    sync_id: &str,
    key: &MappingKey,
    base: u32,
    now: i64,
) -> Result<MappingProgress> {
    let state = topology::read_accepted_state(config, sync_id, key).await?;
    Ok(mapping_progress(
        state.map(|s| (s.serial, topology::is_effective(s.valid_from.as_ref(), now))),
        base,
    ))
}

/// Design D5 step 5 and D11: re-read the run row and the `WorkflowProposal`
/// immediately before a ledger or topology write. `None` means cancel,
/// dismiss, failure, or expiry won the race; the caller returns without
/// writing.
async fn ensure_live(ctx: &TickCtx<'_>, run: &WorkflowRun) -> Result<Option<RunMeta>> {
    let Some(fresh) = ctx.db().get_workflow_run(&run.instance_name).await? else {
        return Ok(None);
    };
    if fresh.status != WorkflowProgress::InProgress {
        tracing::debug!(instance = %run.instance_name, status = %fresh.status, "run is no longer in progress");
        return Ok(None);
    }
    let Some(meta) = read_run_meta(&fresh) else {
        return Ok(None);
    };
    match proposals::read_proposal(ctx.client, &meta.proposal_cid).await? {
        Some(p) if !ctx.is_expired(&p) => Ok(Some(meta)),
        _ => {
            tracing::debug!(instance = %run.instance_name, proposal = %meta.proposal_cid, "WorkflowProposal is gone or expired");
            Ok(None)
        }
    }
}

/// Drop the kicked participant from the cached membership so an immediate
/// re-add is not refused with "already a member". Best effort: a stale
/// cache heals with the next `/decentralized-parties` refresh, while a run
/// rewound after an effective kick would re-propose the same change.
async fn prune_cached_member(db: &SqlitePool, party: &CantonId, kicked: &CantonId) {
    let result: Result<()> = async {
        let mut tx = db.begin_transaction().await?;
        tx.delete_dec_party_participant(party, &kicked.to_string())
            .await?;
        Commitable::commit(tx).await
    }
    .await;
    match result {
        Ok(()) => {
            tracing::info!(%party, %kicked, "kicked participant removed from the cached membership")
        }
        Err(e) => {
            tracing::warn!(%party, %kicked, error = %e, "cached membership not pruned; refresh /decentralized-parties")
        }
    }
}

// ---------------------------------------------------------------------------
// Coordinator
// ---------------------------------------------------------------------------

/// `WaitingForAcceptances`: count the acceptances, record what each member
/// said about its keys (design M6), and move on at the quorum.
async fn wait_for_acceptances(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    proposal: &ActiveProposal,
    plan: &KickPlan,
) -> Result<()> {
    let raw = ctx.proposals.acceptances_for(&proposal.contract_id);
    let counted =
        match proposals::counted_acceptances_verified(ctx.ol.config(), proposal, &raw).await {
            Ok(counted) => counted,
            // A duplicate acceptor is a conflict in key material: fail closed.
            Err(e) => return fail_run(ctx.db(), run, &format!("{e:#}")).await,
        };
    for a in &counted {
        if let Ok(participant) = CantonId::parse(&a.record.participant_id) {
            keys::record_member_keys(
                ctx.db(),
                &plan.party,
                &participant,
                a.record.namespace_fingerprint.as_deref(),
                a.record.daml_key_fingerprint.as_deref(),
            )
            .await?;
        }
    }
    if quorum_reached(counted.len(), plan.previous_threshold, plan.threshold) {
        tracing::info!(instance = %run.instance_name, counted = counted.len(), "kick quorum reached");
        return advance_step(ctx.db(), run, PROPOSE_CHANGES_STEP).await;
    }
    tracing::debug!(instance = %run.instance_name, counted = counted.len(), "waiting for kick acceptances");
    Ok(())
}

/// `ProposeChanges`: propose the kick DND at `dndBaseSerial + 1` once, pin
/// its hash, and move to `AwaitChanges`.
async fn propose_dnd(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    plan: &KickPlan,
) -> Result<()> {
    let config = ctx.ol.config();
    let db = ctx.db();
    if meta.topology_hashes.contains_key(DND_HASH) {
        return advance_step(db, run, AWAIT_CHANGES_STEP).await;
    }
    let key = MappingKey::Dnd(plan.namespace.clone());
    let Some(accepted) = topology::read_accepted_dnd(config, &ctx.sync_id, &plan.namespace).await?
    else {
        return fail_run(db, run, &moved_message(&key, plan.dnd_base, None)).await;
    };
    match mapping_progress(
        Some((accepted.serial, accepted.is_effective(ctx.now_micros))),
        plan.dnd_base,
    ) {
        // Proposed before a crash and already authorized: nothing to pin.
        MappingProgress::Pending | MappingProgress::Landed => {
            return advance_step(db, run, AWAIT_CHANGES_STEP).await;
        }
        MappingProgress::Moved { accepted } => {
            return fail_run(db, run, &moved_message(&key, plan.dnd_base, accepted)).await;
        }
        MappingProgress::AtBase => {}
    }
    let head = accepted.mapping;
    let Some(kicked) = cached_kicked_member(db, &plan.party, &plan.kicked).await? else {
        return fail_run(db, run, &no_owner_key_message(&plan.party, &plan.kicked)).await;
    };
    if let Err(why) = check_kicked_is_owner(&kicked, &head) {
        return fail_run(db, run, &why).await;
    }
    let mapping = topology::build_kick_dnd(&head, &kicked.owner_fingerprint, plan.threshold);
    let expected = topology::dnd_of(&mapping).context("build_kick_dnd returned no DND")?;
    let serial = plan.dnd_base + 1;

    if ensure_live(ctx, run).await?.is_none() {
        return Ok(());
    }
    let pending = topology::list_pending_dnd(config, &ctx.sync_id, &plan.namespace).await?;
    let hash_hex = match find_own_pending(&pending, &plan.proposer_fingerprint, serial, expected) {
        Some(own) => own.hash_hex.clone(),
        None => {
            topology::propose_mapping(config, &ctx.sync_id, mapping, serial)
                .await?
                .hash_hex
        }
    };
    pin_topology_hash(db, &run.instance_name, DND_HASH, &hash_hex).await?;
    advance_step(db, run, AWAIT_CHANGES_STEP).await
}

/// `AwaitChanges`: wait for the DND, then propose the P2P once, then wait
/// for the P2P and finish.
async fn await_changes(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    plan: &KickPlan,
) -> Result<()> {
    let config = ctx.ol.config();
    let db = ctx.db();
    let dnd_key = MappingKey::Dnd(plan.namespace.clone());
    match progress_of(
        config,
        &ctx.sync_id,
        &dnd_key,
        plan.dnd_base,
        ctx.now_micros,
    )
    .await?
    {
        MappingProgress::AtBase | MappingProgress::Pending => {
            tracing::debug!(instance = %run.instance_name, "waiting for the kick DND to become effective");
            return Ok(());
        }
        MappingProgress::Moved { accepted } => {
            return fail_run(db, run, &moved_message(&dnd_key, plan.dnd_base, accepted)).await;
        }
        MappingProgress::Landed => {}
    }

    let p2p_key = MappingKey::P2p(plan.party.clone());
    let Some(accepted) = topology::read_accepted_p2p(config, &ctx.sync_id, &plan.party).await?
    else {
        return fail_run(db, run, &moved_message(&p2p_key, plan.p2p_base, None)).await;
    };
    match mapping_progress(
        Some((accepted.serial, accepted.is_effective(ctx.now_micros))),
        plan.p2p_base,
    ) {
        MappingProgress::Landed => return finish_coordinator(ctx, run, meta, plan).await,
        MappingProgress::Pending => {
            tracing::debug!(instance = %run.instance_name, "waiting for the kick P2P to become effective");
            return Ok(());
        }
        MappingProgress::Moved { accepted } => {
            return fail_run(db, run, &moved_message(&p2p_key, plan.p2p_base, accepted)).await;
        }
        MappingProgress::AtBase => {}
    }
    if meta.topology_hashes.contains_key(P2P_HASH) {
        tracing::debug!(instance = %run.instance_name, "waiting for the kick P2P co-signatures");
        return Ok(());
    }

    let head = accepted.mapping;
    let Some(kicked) = cached_kicked_member(db, &plan.party, &plan.kicked).await? else {
        return fail_run(db, run, &no_owner_key_message(&plan.party, &plan.kicked)).await;
    };
    let claims = signing_keys::known_signing_keys_by_member(config, db, &plan.party).await?;
    let kicked_key = match kicked_key_fingerprint(&head, &kicked, &claims) {
        Ok(fp) => fp,
        Err(e) => return fail_run(db, run, &format!("{e:#}")).await,
    };
    let mapping = topology::build_kick_p2p(&head, &plan.kicked, &kicked_key, plan.threshold);
    let expected = topology::p2p_of(&mapping).context("build_kick_p2p returned no P2P")?;
    let serial = plan.p2p_base + 1;

    if ensure_live(ctx, run).await?.is_none() {
        return Ok(());
    }
    let pending = topology::list_pending_p2p(config, &ctx.sync_id, &plan.party).await?;
    let hash_hex = match find_own_pending(&pending, &plan.proposer_fingerprint, serial, expected) {
        Some(own) => own.hash_hex.clone(),
        None => {
            topology::propose_mapping(config, &ctx.sync_id, mapping, serial)
                .await?
                .hash_hex
        }
    };
    pin_topology_hash(db, &run.instance_name, P2P_HASH, &hash_hex).await
}

/// The kick is effective: heal the local cache, close the row, then finish
/// the proposal. The row closes first because `drive` fails a coordinator
/// whose proposal vanished; a `Finish` that fails afterwards only leaves
/// the proposal to expire, and members complete from the P2P state.
async fn finish_coordinator(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    plan: &KickPlan,
) -> Result<()> {
    prune_cached_member(ctx.db(), &plan.party, &plan.kicked).await;
    complete_run(ctx.db(), run).await?;
    match proposals::finish(ctx.client, &meta.proposal_cid, true, None).await {
        Ok(outcome) => {
            tracing::info!(instance = %run.instance_name, outcome = %outcome, "kick complete")
        }
        Err(e) => tracing::warn!(
            instance = %run.instance_name,
            proposal = %meta.proposal_cid,
            error = %e,
            "kick complete, but WorkflowProposal_Finish failed; the proposal expires on its own"
        ),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Member
// ---------------------------------------------------------------------------

/// Exercise `WorkflowProposal_Accept` with this node's key material for the
/// party (design D6). No key is generated: a kick member already owns one.
async fn accept_on_ledger(ctx: &TickCtx<'_>, proposal_cid: &str, plan: &KickPlan) -> Result<()> {
    let identity = keys::local_identity_for_party(
        ctx.ol.config(),
        ctx.db(),
        Some(&plan.party),
        Some(plan.party.prefix.as_str()),
    )
    .await?;
    let material = ProposerKeyMaterial {
        namespace_fingerprint: sole_fingerprint(&identity.owner_fingerprints),
        // Informational for onboarding and add-party only (design D4).
        signing_public_key_hex: None,
        daml_key_fingerprint: identity.daml_key_fingerprint,
    };
    let member_party = ctx
        .db()
        .get_party_credentials(&plan.party)
        .await?
        .map(|c| c.member_party_id);
    let args = super::accept_args(ctx.identity, &material, member_party);
    let cid = proposals::accept(ctx.client, proposal_cid, &args).await?;
    tracing::info!(proposal = %proposal_cid, acceptance = %cid, "kick invitation accepted on the ledger");
    Ok(())
}

/// Append `hash_hex` to the pinned hashes of the accepted decision. `false`
/// when the decision is not `Accepted`, so the caller does not sign.
///
/// TODO(engine/mod.rs): move next to `pin_topology_hash`; every member
/// driver needs the same write.
async fn pin_decision_hash(db: &SqlitePool, proposal_cid: &str, hash_hex: &str) -> Result<bool> {
    let Some(entry) = db.get_proposal_decision(proposal_cid).await? else {
        return Ok(false);
    };
    if entry.decision != ProposalDecision::Accepted {
        return Ok(false);
    }
    if entry.pinned_hashes.iter().any(|h| h == hash_hex) {
        return Ok(true);
    }
    let mut hashes = entry.pinned_hashes;
    hashes.push(hash_hex.to_string());
    let mut tx = db.begin_transaction().await?;
    tx.set_proposal_pinned_hashes(proposal_cid, &hashes).await?;
    Commitable::commit(tx).await?;
    Ok(true)
}

/// Design D5 steps 5 and 6: re-read, pin, co-sign by hash. A hash pinned
/// earlier for the same mapping must match, or the proposal is another one
/// for a spent mapping and is never signed.
async fn cosign<M>(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    proposal_cid: &str,
    hash_key: &str,
    pending: &PendingProposal<M>,
) -> Result<()> {
    let db = ctx.db();
    let Some(meta) = ensure_live(ctx, run).await? else {
        return Ok(());
    };
    match meta.topology_hashes.get(hash_key) {
        Some(pinned) if *pinned != pending.hash_hex => {
            tracing::warn!(
                instance = %run.instance_name,
                pinned,
                seen = %pending.hash_hex,
                "another proposal for a pinned mapping; not signing"
            );
            return Ok(());
        }
        Some(_) => {}
        None => pin_topology_hash(db, &run.instance_name, hash_key, &pending.hash_hex).await?,
    }
    if !pin_decision_hash(db, proposal_cid, &pending.hash_hex).await? {
        return fail_run(
            db,
            run,
            &format!("proposal_decisions no longer records {proposal_cid} as accepted"),
        )
        .await;
    }
    let outcome = topology::cosign_by_hash(
        ctx.ol.config(),
        &ctx.sync_id,
        &pending.hash_hex,
        &pending.signed_by,
    )
    .await?;
    match outcome {
        CosignOutcome::NotFound => tracing::debug!(
            instance = %run.instance_name,
            hash = %pending.hash_hex,
            "topology proposal not in the local store yet; retry next tick"
        ),
        CosignOutcome::Signed | CosignOutcome::AlreadySigned => tracing::info!(
            instance = %run.instance_name,
            hash = %pending.hash_hex,
            ?outcome,
            "kick {hash_key} co-signed"
        ),
    }
    Ok(())
}

/// The reference set of section 5 for this node. `accepted` stays empty:
/// the kick rules never read it, and the hosting checks it would need cost
/// one topology read per acceptance per tick.
async fn expectations(
    ctx: &TickCtx<'_>,
    proposal: &ActiveProposal,
    plan: &KickPlan,
    head: HeadState,
    kicked: KickedMember,
) -> Result<Expectations> {
    let identity = keys::local_identity_for_party(
        ctx.ol.config(),
        ctx.db(),
        Some(&plan.party),
        Some(plan.party.prefix.as_str()),
    )
    .await?;
    let claims = keys::survivor_key_claims(ctx.db(), &plan.party).await?;
    Ok(Expectations::new(&proposal.record, &[], head, identity)
        .with_kicked(kicked)
        .with_survivor_key_claims(claims))
}

/// `CoSignChanges`: accept on the ledger, co-sign the DND, wait until it is
/// effective, co-sign the P2P, complete when the P2P is effective.
async fn cosign_changes(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &ActiveProposal,
    plan: &KickPlan,
) -> Result<()> {
    let config = ctx.ol.config();
    let db = ctx.db();
    let cid = proposal.contract_id.as_str();

    if !accepted_by(&ctx.proposals.acceptances, cid, ctx.client.node_party()) {
        return accept_on_ledger(ctx, cid, plan).await;
    }

    let dnd_key = MappingKey::Dnd(plan.namespace.clone());
    let Some(dnd) = topology::read_accepted_dnd(config, &ctx.sync_id, &plan.namespace).await?
    else {
        return fail_run(db, run, &moved_message(&dnd_key, plan.dnd_base, None)).await;
    };
    let Some(p2p) = topology::read_accepted_p2p(config, &ctx.sync_id, &plan.party).await? else {
        let key = MappingKey::P2p(plan.party.clone());
        return fail_run(db, run, &moved_message(&key, plan.p2p_base, None)).await;
    };
    let dnd_progress = mapping_progress(
        Some((dnd.serial, dnd.is_effective(ctx.now_micros))),
        plan.dnd_base,
    );
    match dnd_progress {
        MappingProgress::Pending => {
            tracing::debug!(instance = %run.instance_name, "kick DND accepted, not yet effective");
            return Ok(());
        }
        MappingProgress::Moved { accepted } => {
            return fail_run(db, run, &moved_message(&dnd_key, plan.dnd_base, accepted)).await;
        }
        MappingProgress::AtBase => {
            let pending = topology::list_pending_dnd(config, &ctx.sync_id, &plan.namespace).await?;
            if pending.is_empty() {
                return Ok(());
            }
            let Some(kicked) = cached_kicked_member(db, &plan.party, &plan.kicked).await? else {
                return fail_run(db, run, &no_owner_key_message(&plan.party, &plan.kicked)).await;
            };
            if let Err(why) = check_kicked_is_owner(&kicked, &dnd.mapping) {
                return fail_run(db, run, &why).await;
            }
            let head = HeadState {
                dnd: Some(dnd.mapping.clone()),
                p2p: Some(p2p.mapping),
            };
            let exp = expectations(ctx, proposal, plan, head, kicked).await?;
            let cands = candidates(
                pending,
                &plan.proposer_fingerprint,
                &exp.identity.owner_fingerprints,
                meta.topology_hashes.get(DND_HASH),
            );
            return match select_valid(&cands, |p| {
                validation::validate_dnd(p, &exp, Some(dnd.serial))
            }) {
                Ok(Some(p)) => cosign(ctx, run, cid, DND_HASH, p).await,
                Ok(None) => Ok(()),
                Err(why) => fail_run(db, run, &why).await,
            };
        }
        MappingProgress::Landed => {}
    }

    let p2p_key = MappingKey::P2p(plan.party.clone());
    match mapping_progress(
        Some((p2p.serial, p2p.is_effective(ctx.now_micros))),
        plan.p2p_base,
    ) {
        MappingProgress::Landed => {
            prune_cached_member(db, &plan.party, &plan.kicked).await;
            complete_run(db, run).await
        }
        MappingProgress::Pending => {
            tracing::debug!(instance = %run.instance_name, "kick P2P accepted, not yet effective");
            Ok(())
        }
        MappingProgress::Moved { accepted } => {
            fail_run(db, run, &moved_message(&p2p_key, plan.p2p_base, accepted)).await
        }
        MappingProgress::AtBase => {
            let pending = topology::list_pending_p2p(config, &ctx.sync_id, &plan.party).await?;
            if pending.is_empty() {
                return Ok(());
            }
            // The kick DND landed, so the kicked fingerprint is no longer an
            // owner: the owner cross-check does not apply here.
            let Some(kicked) = cached_kicked_member(db, &plan.party, &plan.kicked).await? else {
                return fail_run(db, run, &no_owner_key_message(&plan.party, &plan.kicked)).await;
            };
            let head = HeadState {
                dnd: Some(dnd.mapping),
                p2p: Some(p2p.mapping),
            };
            let exp = expectations(ctx, proposal, plan, head, kicked).await?;
            let cands = candidates(
                pending,
                &plan.proposer_fingerprint,
                &exp.identity.owner_fingerprints,
                meta.topology_hashes.get(P2P_HASH),
            );
            match select_valid(&cands, |p| {
                validation::validate_p2p(p, &exp, Some(p2p.serial))
            }) {
                Ok(Some(p)) => cosign(ctx, run, cid, P2P_HASH, p).await,
                Ok(None) => Ok(()),
                Err(why) => fail_run(db, run, &why).await,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

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

    /// Section 6 preflight for a kick. `engine::check_thresholds` already
    /// refuses `previousThreshold > |owners| - 1` and `threshold >
    /// |resulting owners|`. This adds what only this node can know: the
    /// kicked member's owner key is cached and still owns the DND, this
    /// node holds exactly one owner key, and the kicked member's party
    /// signing key can be attributed (legacy parties by elimination).
    async fn preflight(ol: &OnLedger, req: &StartRequest) -> Result<()> {
        let StartRequest::Kick {
            dec_party_id,
            participant_id,
            ..
        } = req
        else {
            return Ok(());
        };
        let config = ol.config();
        let db = ol.db();
        let sync_id = utils::get_synchronizer_id(config).await?;
        let namespace = dec_party_id.namespace.to_hex();
        let head_dnd = topology::read_accepted_dnd(config, &sync_id, &namespace)
            .await?
            .with_context(|| {
                format!("{dec_party_id} has no accepted DecentralizedNamespaceDefinition")
            })?
            .mapping;
        let head_p2p = topology::read_accepted_p2p(config, &sync_id, dec_party_id)
            .await?
            .with_context(|| format!("{dec_party_id} has no accepted PartyToParticipant"))?
            .mapping;

        let Some(kicked) = cached_kicked_member(db, dec_party_id, participant_id).await? else {
            return Err(
                PreflightRejected::new(no_owner_key_message(dec_party_id, participant_id)).into(),
            );
        };
        if let Err(why) = check_kicked_is_owner(&kicked, &head_dnd) {
            return Err(PreflightRejected::new(why).into());
        }
        let identity = keys::local_identity_for_party(
            config,
            db,
            Some(dec_party_id),
            Some(dec_party_id.prefix.as_str()),
        )
        .await?;
        own_owner_fingerprint(&identity, &head_dnd)
            .map_err(|e| PreflightRejected::new(format!("{e:#}")))?;
        let claims = signing_keys::known_signing_keys_by_member(config, db, dec_party_id).await?;
        kicked_key_fingerprint(&head_p2p, &kicked, &claims)
            .map_err(|e| PreflightRejected::new(format!("{e:#}")))?;
        Ok(())
    }

    /// The proposer's owner fingerprint for the party (design D6). No key is
    /// generated; the key hex rides along when the vault has it.
    async fn prepare(ol: &OnLedger, req: &StartRequest) -> Result<ProposalExtras> {
        let StartRequest::Kick { dec_party_id, .. } = req else {
            bail!("Kick::prepare called with a {} request", req.kind());
        };
        let config = ol.config();
        let sync_id = utils::get_synchronizer_id(config).await?;
        let head_dnd =
            topology::read_accepted_dnd(config, &sync_id, &dec_party_id.namespace.to_hex())
                .await?
                .with_context(|| {
                    format!("{dec_party_id} has no accepted DecentralizedNamespaceDefinition")
                })?
                .mapping;
        let identity = keys::local_identity_for_party(
            config,
            ol.db(),
            Some(dec_party_id),
            Some(dec_party_id.prefix.as_str()),
        )
        .await?;
        let owner = own_owner_fingerprint(&identity, &head_dnd)?;
        let key_hex = keys::list_vault_keys(config)
            .await?
            .into_iter()
            .find(|k| k.fingerprint == owner)
            .map(|k| keys::PartyKey::from_key(k.key).key_hex);
        Ok(ProposalExtras {
            keys: ProposerKeyMaterial {
                namespace_fingerprint: Some(owner),
                signing_public_key_hex: key_hex,
                daml_key_fingerprint: identity.daml_key_fingerprint,
            },
            ..ProposalExtras::default()
        })
    }

    async fn tick_coordinator(ctx: &TickCtx<'_>, run: &WorkflowRun, meta: &RunMeta) -> Result<()> {
        // `drive` already handled a missing proposal this tick.
        let Some(proposal) = ctx.proposals.proposal(&meta.proposal_cid) else {
            return Ok(());
        };
        let plan = match KickPlan::parse(&proposal.record) {
            Ok(plan) => plan,
            Err(why) => return fail_run(ctx.db(), run, &why).await,
        };
        match run.current_step.as_str() {
            WAITING_FOR_ACCEPTANCES_STEP => wait_for_acceptances(ctx, run, proposal, &plan).await,
            PROPOSE_CHANGES_STEP => propose_dnd(ctx, run, meta, &plan).await,
            AWAIT_CHANGES_STEP => await_changes(ctx, run, meta, &plan).await,
            COMPLETE_STEP => Ok(()),
            other => bail!("unknown kick coordinator step {other}"),
        }
    }

    async fn tick_member(ctx: &TickCtx<'_>, run: &WorkflowRun, meta: &RunMeta) -> Result<()> {
        let Some(proposal) = ctx.proposals.proposal(&meta.proposal_cid) else {
            return Ok(());
        };
        let plan = match KickPlan::parse(&proposal.record) {
            Ok(plan) => plan,
            Err(why) => return fail_run(ctx.db(), run, &why).await,
        };
        match run.current_step.as_str() {
            CO_SIGN_CHANGES_STEP => cosign_changes(ctx, run, meta, proposal, &plan).await,
            COMPLETE_STEP => Ok(()),
            other => bail!("unknown kick member step {other}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::enums::TopologyChangeOp;

    use super::*;
    use crate::onledger::{
        daml::{
            ActiveContract,
            codec::tests::{acceptance_full, party, proposal_full},
        },
        topology::{
            build_bootstrap_p2p, build_dnd, build_kick_dnd, dnd_of, p2p_of,
            tests::{NS, fp, key, participant},
        },
        validation::ValidationError,
    };

    fn head_dnd() -> DecentralizedNamespaceDefinition {
        dnd_of(&build_dnd(&[fp(1), fp(2), fp(3)], 2))
            .expect("dnd")
            .clone()
    }

    /// Party signing keys `seeds`, hosts participants 1..=3.
    fn head_p2p(seeds: &[u8]) -> PartyToParticipant {
        let keys: Vec<_> = seeds.iter().map(|s| key(*s)).collect();
        p2p_of(&build_bootstrap_p2p(
            "cbtc",
            &head_dnd().decentralized_namespace,
            &[participant(1), participant(2), participant(3)],
            &keys,
            2,
        ))
        .expect("p2p")
        .clone()
    }

    fn kicked(seed: u8, signing: Option<u8>) -> KickedMember {
        KickedMember {
            participant_id: participant(3).to_string(),
            owner_fingerprint: fp(seed),
            signing_key_fingerprint: signing.map(fp),
        }
    }

    fn claims(entries: &[(u8, u8)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(p, s)| (participant(*p).to_string(), fp(*s)))
            .collect()
    }

    fn pending<M>(mapping: M, serial: u32, signed_by: &[String], hash: &str) -> PendingProposal<M> {
        PendingProposal {
            hash_hex: hash.into(),
            serial,
            signed_by: signed_by.to_vec(),
            operation: TopologyChangeOp::AddReplace as i32,
            mapping,
            sequenced: None,
            valid_from: None,
        }
    }

    fn kick_record() -> WorkflowProposalRecord {
        let mut record = proposal_full();
        record.kind = WorkflowKind::Kick;
        record.proposer_namespace_fingerprint = Some(fp(1));
        record
    }

    // ----- KickPlan -----

    #[test]
    fn plan_parses_every_field_it_needs() {
        let plan = KickPlan::parse(&kick_record()).expect("plan");
        assert_eq!(
            plan.party,
            CantonId::parse(&format!("cbtc::{NS}")).expect("id")
        );
        assert_eq!(plan.namespace, NS);
        assert_eq!(plan.kicked, participant(3));
        assert_eq!(plan.threshold, 2);
        assert_eq!(plan.previous_threshold, Some(3));
        assert_eq!(plan.dnd_base, 4);
        assert_eq!(plan.p2p_base, 5);
        assert_eq!(plan.proposer_fingerprint, fp(1));
    }

    #[test]
    fn plan_fails_closed_on_a_malformed_proposal() {
        let mut no_kicked = kick_record();
        no_kicked.kicked_participant = None;
        assert!(
            KickPlan::parse(&no_kicked)
                .expect_err("kicked")
                .contains("kicked")
        );

        let mut no_threshold = kick_record();
        no_threshold.threshold = None;
        assert!(KickPlan::parse(&no_threshold).is_err());

        let mut zero_threshold = kick_record();
        zero_threshold.threshold = Some(0);
        assert!(KickPlan::parse(&zero_threshold).is_err());

        let mut no_serial = kick_record();
        no_serial.p2p_base_serial = None;
        assert!(
            KickPlan::parse(&no_serial)
                .expect_err("serial")
                .contains("p2pBaseSerial")
        );

        let mut no_proposer = kick_record();
        no_proposer.proposer_namespace_fingerprint = None;
        assert!(KickPlan::parse(&no_proposer).is_err());

        let mut wrong_kind = kick_record();
        wrong_kind.kind = WorkflowKind::ChangeThreshold;
        assert!(
            KickPlan::parse(&wrong_kind)
                .expect_err("kind")
                .contains("not Kick")
        );
    }

    // ----- progress and quorum -----

    #[test]
    fn mapping_progress_follows_the_serial_and_the_clock() {
        assert_eq!(
            mapping_progress(Some((4, true)), 4),
            MappingProgress::AtBase
        );
        assert_eq!(
            mapping_progress(Some((5, false)), 4),
            MappingProgress::Pending
        );
        assert_eq!(
            mapping_progress(Some((5, true)), 4),
            MappingProgress::Landed
        );
        assert_eq!(
            mapping_progress(Some((6, true)), 4),
            MappingProgress::Moved { accepted: Some(6) }
        );
        assert_eq!(
            mapping_progress(Some((3, true)), 4),
            MappingProgress::Moved { accepted: Some(3) }
        );
        assert_eq!(
            mapping_progress(None, 4),
            MappingProgress::Moved { accepted: None }
        );
    }

    /// 4 owners, previous 3, new 2, kick one: the coordinator waits for two
    /// acceptances (three signatures with its own).
    #[test]
    fn quorum_is_the_larger_threshold_minus_the_proposer() {
        assert!(!quorum_reached(1, Some(3), 2));
        assert!(quorum_reached(2, Some(3), 2));
        assert!(!quorum_reached(1, Some(2), 3));
        assert!(quorum_reached(2, Some(2), 3));
        // An unknown previous threshold falls back to the new one.
        assert!(quorum_reached(1, None, 2));
        assert!(!quorum_reached(0, None, 2));
        assert!(quorum_reached(0, Some(1), 1));
    }

    // ----- keys -----

    fn identity(seeds: &[u8]) -> LocalIdentity {
        LocalIdentity {
            participant_id: participant(1),
            owner_fingerprints: seeds.iter().map(|s| fp(*s)).collect(),
            daml_key_fingerprint: Some(fp(1)),
        }
    }

    #[test]
    fn own_owner_fingerprint_needs_exactly_one_head_owner() {
        assert_eq!(
            own_owner_fingerprint(&identity(&[1, 9]), &head_dnd()).expect("one"),
            fp(1)
        );
        let none = own_owner_fingerprint(&identity(&[9]), &head_dnd()).expect_err("none");
        assert!(none.to_string().contains("no owner key"), "{none}");
        let two = own_owner_fingerprint(&identity(&[1, 2]), &head_dnd()).expect_err("two");
        assert!(two.to_string().contains("2 owner keys"), "{two}");
    }

    #[test]
    fn kicked_owner_must_still_own_the_head_dnd() {
        check_kicked_is_owner(&kicked(3, None), &head_dnd()).expect("owner");
        let why = check_kicked_is_owner(&kicked(9, None), &head_dnd()).expect_err("stale");
        assert!(why.contains("stale"), "{why}");
        assert!(why.contains("/decentralized-parties"), "{why}");
    }

    #[test]
    fn dual_usage_member_removes_its_owner_key() {
        let head = head_p2p(&[1, 2, 3]);
        let fp3 = kicked_key_fingerprint(&head, &kicked(3, None), &BTreeMap::new()).expect("dual");
        assert_eq!(fp3, fp(3));
    }

    #[test]
    fn legacy_member_removes_its_cached_daml_key() {
        // Owner keys 1..3 are namespace keys; Daml keys 11..13 differ.
        let head = head_p2p(&[11, 12, 13]);
        let removed =
            kicked_key_fingerprint(&head, &kicked(3, Some(13)), &BTreeMap::new()).expect("cached");
        assert_eq!(removed, fp(13));
    }

    #[test]
    fn legacy_member_is_found_by_elimination() {
        let head = head_p2p(&[11, 12, 13]);
        let removed = kicked_key_fingerprint(&head, &kicked(3, None), &claims(&[(1, 11), (2, 12)]))
            .expect("elimination");
        assert_eq!(removed, fp(13));
    }

    #[test]
    fn legacy_member_without_attribution_fails_closed() {
        let head = head_p2p(&[11, 12, 13]);
        // Two keys unclaimed: ambiguous.
        let err = kicked_key_fingerprint(&head, &kicked(3, None), &claims(&[(1, 11)]))
            .expect_err("ambiguous");
        assert!(err.to_string().contains("Cannot tell"), "{err}");
        // Every key claimed by a survivor: nothing to remove.
        let two_keys = head_p2p(&[11, 12]);
        let err = kicked_key_fingerprint(&two_keys, &kicked(3, None), &claims(&[(1, 11), (2, 12)]))
            .expect_err("none");
        assert!(
            err.to_string().contains("claimed by a remaining member"),
            "{err}"
        );
    }

    #[test]
    fn a_cached_key_that_left_the_p2p_does_not_block_elimination() {
        let head = head_p2p(&[11, 12, 13]);
        let removed =
            kicked_key_fingerprint(&head, &kicked(3, Some(99)), &claims(&[(1, 11), (2, 12)]))
                .expect("elimination");
        assert_eq!(removed, fp(13));
    }

    // ----- candidate selection -----

    #[test]
    fn candidates_apply_step_2_and_the_spend_rule() {
        let dnd = |hash: &str| pending(head_dnd(), 5, &[fp(1)], hash);
        let mut remove = dnd("1220aa");
        remove.operation = TopologyChangeOp::Remove as i32;
        let foreign = pending(head_dnd(), 5, &[fp(9)], "1220bb");
        let mine = pending(head_dnd(), 5, &[fp(1), fp(2)], "1220cc");
        let good = dnd("1220dd");
        let other = dnd("1220ee");
        let own: BTreeSet<String> = [fp(2)].into_iter().collect();

        let kept = candidates(
            vec![remove, foreign, mine, good.clone(), other.clone()],
            &fp(1),
            &own,
            None,
        );
        assert_eq!(kept, [good.clone(), other.clone()]);

        let pinned = "1220ee".to_string();
        let kept = candidates(vec![good, other.clone()], &fp(1), &own, Some(&pinned));
        assert_eq!(kept, [other]);
    }

    #[test]
    fn select_valid_returns_the_first_pass_or_every_reason() {
        let a = pending(head_dnd(), 5, &[fp(1)], "1220aa");
        let b = pending(head_dnd(), 5, &[fp(1)], "1220bb");
        let list = [a.clone(), b.clone()];

        let none: [PendingProposal<DecentralizedNamespaceDefinition>; 0] = [];
        assert_eq!(select_valid(&none, |_| Ok(())).expect("empty"), None);

        let picked = select_valid(&list, |p| {
            if p.hash_hex == "1220bb" {
                Ok(())
            } else {
                Err(ValidationError("wrong owners".into()))
            }
        })
        .expect("one passes");
        assert_eq!(picked, Some(&b));

        let why = select_valid(&list, |p| {
            Err(ValidationError(format!("bad {}", p.hash_hex)))
        })
        .expect_err("none pass");
        assert!(why.contains("1220aa: bad 1220aa"), "{why}");
        assert!(why.contains("1220bb: bad 1220bb"), "{why}");
    }

    #[test]
    fn own_pending_matches_signer_serial_and_mapping() {
        let expected = dnd_of(&build_kick_dnd(&head_dnd(), &fp(3), 2))
            .expect("dnd")
            .clone();
        let own = pending(expected.clone(), 5, &[fp(1)], "1220aa");
        let other_serial = pending(expected.clone(), 6, &[fp(1)], "1220bb");
        let other_signer = pending(expected.clone(), 5, &[fp(2)], "1220cc");
        let other_mapping = pending(head_dnd(), 5, &[fp(1)], "1220dd");
        let list = [other_serial, other_signer, other_mapping, own.clone()];

        assert_eq!(find_own_pending(&list, &fp(1), 5, &expected), Some(&own));
        assert_eq!(find_own_pending(&list[..3], &fp(1), 5, &expected), None);
    }

    #[test]
    fn acceptance_lookup_is_by_proposal_and_acceptor() {
        let acceptances = [ActiveContract {
            contract_id: "00acc".into(),
            offset: 1,
            record: acceptance_full("00proposal", "node-b"),
        }];
        assert!(accepted_by(&acceptances, "00proposal", &party("node-b")));
        assert!(!accepted_by(&acceptances, "00proposal", &party("node-c")));
        assert!(!accepted_by(&acceptances, "00other", &party("node-b")));
    }

    #[test]
    fn sole_fingerprint_needs_exactly_one() {
        assert_eq!(sole_fingerprint(&BTreeSet::new()), None);
        assert_eq!(
            sole_fingerprint(&[fp(1)].into_iter().collect()),
            Some(fp(1))
        );
        assert_eq!(
            sole_fingerprint(&[fp(1), fp(2)].into_iter().collect()),
            None
        );
    }

    #[test]
    fn step_lists_match_design_section_6() {
        assert_eq!(
            Kick::coordinator_steps(),
            [
                WAITING_FOR_ACCEPTANCES_STEP,
                PROPOSE_CHANGES_STEP,
                AWAIT_CHANGES_STEP,
                COMPLETE_STEP
            ]
        );
        assert_eq!(
            Kick::member_steps(None),
            [CO_SIGN_CHANGES_STEP, COMPLETE_STEP]
        );
    }
}
