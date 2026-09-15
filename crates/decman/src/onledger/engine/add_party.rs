//! Add-party driver (design D4, D5, D9, section 6): add a member to an
//! existing decentralized party.
//!
//! The coordinator is a current owner, so it has a key already and proposes
//! with it: the DND with the joiner as a new owner, then, once that is
//! effective, the P2P with the joiner at Confirmation and the `Onboarding`
//! marker. Every current host captures its export offset before its first
//! `Authorize` (design D9), co-signs both mappings after the section-5
//! checks, exports its snapshot when the marked P2P is effective, and
//! publishes an `AcsManifest`. The joiner generates its dual-usage key,
//! accepts with it, co-signs, waits for a verified manifest and the import,
//! and clears its own flag.
//!
//! Every tick does one bounded unit of work for `current_step` and returns.
//! A wait is a step: a wait that is not over leaves the row unchanged, and
//! the observer ticks again a few seconds later.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use canton_proto_rs::com::digitalasset::canton::protocol::v30::{
    DecentralizedNamespaceDefinition, PartyToParticipant, enums::ParticipantPermission,
};
use common::{
    canton_id::CantonId,
    types::{Permission, WorkflowKind, WorkflowProgress, WorkflowRun},
};
use prost::Message;
use sqlx::SqlitePool;

use crate::{
    config::{CredentialKind, NodeConfig},
    db::{
        rows::{DecPartyParticipantRow, DecPartyRow},
        schema::{Commitable, SchemaRead, SchemaWrite},
    },
    onledger::{
        acs::{self, SyncDecision},
        daml::codec::WorkflowProposalRecord,
        identity::verify_hosting,
        keys, now_secs,
        proposals::{self, Acceptance, ActiveProposal},
        topology::{self, AcceptedMapping, AcceptedState, CosignOutcome, PendingProposal},
        validation::{self, Check, Expectations, HeadState},
    },
    utils,
    workflow::{
        party_replication::onboarding_flag::{
            has_onboarding_marker, request_onboarding_flag_clear,
        },
        storage::WorkflowStorage,
    },
};

use super::{
    KindDriver, MemberVariant, OnLedger, PreflightRejected, ProposalExtras, ProposerKeyMaterial,
    RunMeta, StartRequest, TickCtx, accept_args, advance_step, complete_run, fail_run,
    pin_topology_hash, read_run_meta, write_run_meta,
};

pub struct AddParty;

pub const COORDINATOR_STEPS: &[&str] = &[
    STEP_GENERATE_KEYS,
    STEP_WAITING_FOR_ACCEPTANCES,
    STEP_PROPOSE_CHANGES,
    STEP_AWAIT_CHANGES,
    STEP_AWAIT_REPLICATION,
    super::COMPLETE_STEP,
];

/// The participant being added.
pub const JOINER_STEPS: &[&str] = &[
    STEP_GENERATE_KEYS,
    STEP_COSIGN_CHANGES,
    STEP_SYNC_ACS,
    STEP_CLEAR_ONBOARDING,
    super::COMPLETE_STEP,
];

/// Every current host. `CoSignChanges` captures the export offset first.
pub const MEMBER_STEPS: &[&str] = &[
    STEP_COSIGN_CHANGES,
    STEP_PUBLISH_MANIFEST,
    super::COMPLETE_STEP,
];

const STEP_GENERATE_KEYS: &str = "GenerateKeys";
const STEP_WAITING_FOR_ACCEPTANCES: &str = super::WAITING_FOR_ACCEPTANCES_STEP;
const STEP_PROPOSE_CHANGES: &str = "ProposeChanges";
const STEP_AWAIT_CHANGES: &str = "AwaitChanges";
const STEP_AWAIT_REPLICATION: &str = "AwaitReplication";
const STEP_COSIGN_CHANGES: &str = "CoSignChanges";
const STEP_SYNC_ACS: &str = "SyncAcs";
const STEP_CLEAR_ONBOARDING: &str = "ClearOnboarding";
const STEP_PUBLISH_MANIFEST: &str = "PublishManifest";

/// `RunMeta::topology_hashes` keys.
const HASH_DND: &str = "dnd";
const HASH_P2P: &str = "p2p";

/// The `WorkflowAcceptance` contract id this node created for the run. The
/// ACS snapshot of the next tick may not show it yet, and a second accept
/// would make every member's count fail closed (design D6).
///
/// TODO(workflow::storage::artifact_kinds): move next to the other kinds.
const ACCEPTANCE_CID_ARTIFACT: &str = "onledger_acceptance_cid";

/// How long an `Await*` step tolerates a pinned proposal that is missing
/// from the synchronizer store before it re-proposes (design D10, retry).
/// A fresh proposal needs a moment to be sequenced, so the check has a
/// grace period.
const REPROPOSE_GRACE_SECS: i64 = 60;

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// The fields of an add-party proposal every step needs. A missing field
/// fails closed here, once, instead of in every step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Facts {
    pub party: CantonId,
    pub prefix: String,
    pub joiner: CantonId,
    pub threshold: u32,
    pub dnd_base: u32,
    pub p2p_base: u32,
    pub proposer_participant: CantonId,
}

impl Facts {
    pub fn namespace(&self) -> String {
        self.party.namespace.to_hex()
    }

    /// The serial of the P2P that marks the joiner `Onboarding`: the
    /// `AcsManifest.activationSerial` every host publishes (design D9).
    pub fn activation_serial(&self) -> u32 {
        self.p2p_base + 1
    }
}

fn required_serial(value: Option<i64>, name: &str) -> Result<u32> {
    let Some(v) = value else {
        bail!("the add-party proposal carries no {name}");
    };
    let serial = u32::try_from(v).with_context(|| format!("{name} {v} does not fit u32"))?;
    if serial == 0 {
        bail!("{name} is 0, but the party exists so its mappings have a serial");
    }
    Ok(serial)
}

/// Read the [`Facts`] of an add-party proposal.
///
/// # Errors
/// Returns an error when the proposal is not an add-party proposal or a
/// required field is missing or malformed.
pub fn facts_of(record: &WorkflowProposalRecord) -> Result<Facts> {
    if record.kind != WorkflowKind::AddParty {
        bail!("not an add-party proposal ({})", record.kind);
    }
    let Some(party) = record.dec_party_id.as_deref() else {
        bail!("the add-party proposal carries no decPartyId");
    };
    let party = CantonId::parse(party).with_context(|| format!("decPartyId `{party}`"))?;
    let Some(joiner) = record.new_participant.as_deref() else {
        bail!("the add-party proposal carries no newParticipant");
    };
    let joiner = CantonId::parse(joiner).with_context(|| format!("newParticipant `{joiner}`"))?;
    let Some(threshold) = record.threshold else {
        bail!("the add-party proposal carries no threshold");
    };
    let threshold = u32::try_from(threshold).context("threshold does not fit u32")?;
    if threshold == 0 {
        bail!("threshold 0 is not valid");
    }
    let proposer_participant = CantonId::parse(&record.proposer_participant)
        .with_context(|| format!("proposerParticipant `{}`", record.proposer_participant))?;
    Ok(Facts {
        prefix: record
            .prefix
            .clone()
            .unwrap_or_else(|| party.prefix.clone()),
        party,
        joiner,
        threshold,
        dnd_base: required_serial(record.dnd_base_serial, "dndBaseSerial")?,
        p2p_base: required_serial(record.p2p_base_serial, "p2pBaseSerial")?,
        proposer_participant,
    })
}

/// Where the accepted mapping stands relative to the base serial the
/// proposal recorded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Progress {
    /// No accepted mapping although the proposal recorded a base serial.
    Missing,
    /// The accepted serial is below the base: the store is behind or the
    /// proposal named another party.
    Moved(u32),
    /// Exactly the base serial: the change is still open.
    AtBase,
    /// A later serial is accepted but its `valid_from` has not passed.
    Pending(u32),
    /// `base + 1` is effective: the change landed.
    Landed,
    /// A serial past `base + 1` is effective.
    Beyond(u32),
}

/// Classify the accepted state of a mapping against `base` (design D5
/// steps 1, 4, 7, 8).
pub fn progress(accepted: Option<&AcceptedState>, base: u32, now_micros: i64) -> Progress {
    match accepted {
        None => Progress::Missing,
        Some(s) if s.serial < base => Progress::Moved(s.serial),
        Some(s) if s.serial == base => Progress::AtBase,
        Some(s) if !topology::is_effective(s.valid_from.as_ref(), now_micros) => {
            Progress::Pending(s.serial)
        }
        Some(s) if s.serial == base + 1 => Progress::Landed,
        Some(s) => Progress::Beyond(s.serial),
    }
}

/// What a member found among the pending proposals of one mapping.
#[derive(Debug)]
pub enum Selection<'a, M> {
    /// No candidate signed by the proposer at the wanted serial yet.
    Waiting,
    /// This node already signed a candidate; wait until it is effective.
    AlreadySigned(&'a PendingProposal<M>),
    /// The candidate to sign: validation passed.
    Match(&'a PendingProposal<M>),
    /// Every candidate failed validation, with the reasons.
    Mismatch(Vec<String>),
    /// The hash pinned earlier is gone while other candidates exist. One
    /// mapping, one transaction: the run fails closed.
    PinMismatch { pinned: String, found: Vec<String> },
}

/// Pick the proposal a member co-signs (design D5 member steps 2 to 4). A
/// candidate is an `ADD_REPLACE` at `wanted_serial` signed by the
/// proposer's owner key. With a pin, only the pinned candidate counts.
/// Without one, a stale candidate from an older attempt fails validation
/// without blocking a valid one; the run fails only when none passes.
pub fn select_pending<'a, M>(
    pending: &'a [PendingProposal<M>],
    wanted_serial: u32,
    proposer_fingerprint: &str,
    pinned: Option<&str>,
    own_fingerprints: &BTreeSet<String>,
    check: impl Fn(&PendingProposal<M>) -> Check,
) -> Selection<'a, M> {
    let candidates: Vec<&PendingProposal<M>> = pending
        .iter()
        .filter(|p| p.serial == wanted_serial)
        .filter(|p| p.is_add_replace() && p.is_signed_by(proposer_fingerprint))
        .collect();
    let signed_by_me =
        |p: &PendingProposal<M>| own_fingerprints.iter().any(|fp| p.is_signed_by(fp));

    if let Some(pin) = pinned {
        return match candidates.iter().find(|p| p.hash_hex == pin) {
            None if candidates.is_empty() => Selection::Waiting,
            None => Selection::PinMismatch {
                pinned: pin.to_string(),
                found: candidates.iter().map(|p| p.hash_hex.clone()).collect(),
            },
            Some(p) if signed_by_me(p) => Selection::AlreadySigned(p),
            Some(p) => match check(p) {
                Ok(()) => Selection::Match(p),
                Err(e) => Selection::Mismatch(vec![format!("{}: {e}", p.hash_hex)]),
            },
        };
    }

    let mut reasons = Vec::new();
    for p in candidates {
        if signed_by_me(p) {
            return Selection::AlreadySigned(p);
        }
        match check(p) {
            Ok(()) => return Selection::Match(p),
            Err(e) => reasons.push(format!("{}: {e}", p.hash_hex)),
        }
    }
    if reasons.is_empty() {
        Selection::Waiting
    } else {
        Selection::Mismatch(reasons)
    }
}

/// Whether an `Await*` step goes back and re-proposes (design D10, retry):
/// a pin exists, the grace period since the last row change is over, and
/// the pinned hash has left the synchronizer store.
pub fn should_repropose(
    pinned: Option<&str>,
    pending_hashes: &[String],
    updated_at_secs: i64,
    now_secs: i64,
) -> bool {
    let Some(pin) = pinned else {
        return false;
    };
    now_secs - updated_at_secs >= REPROPOSE_GRACE_SECS && !pending_hashes.iter().any(|h| h == pin)
}

/// Whether every invitee has a counted acceptance (add-party needs all).
pub fn all_invitees_accepted(invitees: &[CantonId], counted: &[Acceptance]) -> bool {
    let accepted: BTreeSet<&CantonId> = counted.iter().map(|a| &a.record.acceptor).collect();
    invitees.iter().all(|i| accepted.contains(i))
}

/// The acceptors among `counted`, sorted, for the `connected_peers` card.
pub fn accepted_parties(counted: &[Acceptance]) -> Vec<CantonId> {
    counted
        .iter()
        .map(|a| a.record.acceptor.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The joiner's namespace fingerprint from its counted acceptance:
/// `Ok(None)` while the joiner has not accepted, an error when it accepted
/// without key material (fails closed).
pub fn joiner_fingerprint(counted: &[Acceptance], joiner: &CantonId) -> Result<Option<String>> {
    let joiner_uid = joiner.to_string();
    let Some(a) = counted
        .iter()
        .find(|a| a.record.participant_id == joiner_uid)
    else {
        return Ok(None);
    };
    match &a.record.namespace_fingerprint {
        Some(fp) => Ok(Some(fp.clone())),
        None => bail!(
            "the joiner {joiner} accepted without a namespace fingerprint; it cannot become an owner"
        ),
    }
}

/// Who sees an `AcsManifest`: every node party of the run except the
/// exporter itself, which is the signatory.
pub fn manifest_observers(record: &WorkflowProposalRecord, me: &CantonId) -> Vec<CantonId> {
    record
        .invitees
        .iter()
        .chain(std::iter::once(&record.proposer))
        .filter(|p| *p != me)
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The owner fingerprint this node advertises for the party: the dual key
/// named after the party when it owns the DND, else the first owner key in
/// the vault (a legacy `{prefix}-namespace` key).
pub fn pick_owner_fingerprint(
    owners: &BTreeSet<String>,
    vault: &[keys::VaultKey],
    prefix: &str,
) -> Option<String> {
    let dual = keys::party_key_name(prefix);
    vault
        .iter()
        .find(|k| k.name == dual && owners.contains(&k.fingerprint))
        .map(|k| k.fingerprint.clone())
        .or_else(|| owners.iter().next().cloned())
}

fn state_of<M>(accepted: Option<&AcceptedMapping<M>>) -> Option<AcceptedState> {
    accepted.map(|a| AcceptedState {
        serial: a.serial,
        valid_from: a.valid_from,
    })
}

// ---------------------------------------------------------------------------
// Reads shared by both sides
// ---------------------------------------------------------------------------

fn proposal_of<'a>(ctx: &'a TickCtx<'_>, meta: &RunMeta) -> Option<&'a ActiveProposal> {
    let found = ctx.proposals.proposal(&meta.proposal_cid);
    if found.is_none() {
        // `drive` reconciles a vanished proposal before the driver runs, so
        // this is a race with that read, not a state to act on.
        tracing::debug!(proposal = %meta.proposal_cid, "proposal not in this tick's snapshot");
    }
    found
}

async fn accepted_dnd(
    ctx: &TickCtx<'_>,
    facts: &Facts,
) -> Result<Option<AcceptedMapping<DecentralizedNamespaceDefinition>>> {
    topology::read_accepted_dnd(ctx.ol.config(), &ctx.sync_id, &facts.namespace()).await
}

async fn accepted_p2p(
    ctx: &TickCtx<'_>,
    facts: &Facts,
) -> Result<Option<AcceptedMapping<PartyToParticipant>>> {
    topology::read_accepted_p2p(ctx.ol.config(), &ctx.sync_id, &facts.party).await
}

/// Record what each counted acceptance says about its acceptor's keys, so
/// the caches a later kick reads heal on every node (design M6). Only
/// participants with a cached row are written; the joiner gets its row in
/// [`persist_party_cache`] once the P2P landed. Best effort: a failed write
/// is logged and tried again next tick.
async fn record_keys(db: &SqlitePool, party: &CantonId, counted: &[Acceptance]) {
    let cached = match db.get_dec_party_participants(party).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(%party, error = %e, "participant cache unreadable; member keys not recorded");
            return;
        }
    };
    for a in counted {
        if !cached
            .iter()
            .any(|r| r.participant_uid == a.record.participant_id)
        {
            continue;
        }
        let Ok(participant) = CantonId::parse(&a.record.participant_id) else {
            continue;
        };
        if let Err(e) = keys::record_member_keys(
            db,
            party,
            &participant,
            a.record.namespace_fingerprint.as_deref(),
            a.record.daml_key_fingerprint.as_deref(),
        )
        .await
        {
            tracing::warn!(%party, %participant, error = %e, "member keys not recorded");
        }
    }
}

/// What one member claims about its own keys, from the proposal (the
/// proposer) or its counted acceptance (everyone else).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct KeyClaim {
    pub owner: Option<String>,
    pub daml: Option<String>,
}

/// Key claims by participant uid. A claim is what the member said about
/// itself, never what the proposer said about others (design section 5).
pub fn key_claims(
    record: &WorkflowProposalRecord,
    counted: &[Acceptance],
) -> BTreeMap<String, KeyClaim> {
    let mut claims = BTreeMap::new();
    claims.insert(
        record.proposer_participant.clone(),
        KeyClaim {
            owner: record.proposer_namespace_fingerprint.clone(),
            daml: record.proposer_daml_key_fingerprint.clone(),
        },
    );
    for a in counted {
        claims.insert(
            a.record.participant_id.clone(),
            KeyClaim {
                owner: a.record.namespace_fingerprint.clone(),
                daml: a.record.daml_key_fingerprint.clone(),
            },
        );
    }
    claims
}

fn permission_label(permission: i32) -> &'static str {
    match ParticipantPermission::try_from(permission) {
        Ok(ParticipantPermission::Submission) => Permission::Submission.as_str(),
        Ok(ParticipantPermission::Confirmation) => Permission::Confirmation.as_str(),
        Ok(ParticipantPermission::Observation) => Permission::Observation.as_str(),
        _ => Permission::Unknown.as_str(),
    }
}

/// The participant rows for the new head P2P: one per host, keys from the
/// member's own claim first and from the cached row second, so a rewrite
/// never drops a key a node learned earlier.
pub fn cache_rows(
    party: &CantonId,
    p2p: &PartyToParticipant,
    existing: &[DecPartyParticipantRow],
    claims: &BTreeMap<String, KeyClaim>,
) -> Vec<DecPartyParticipantRow> {
    let party_id = party.to_string();
    p2p.participants
        .iter()
        .map(|h| {
            let cached = existing
                .iter()
                .find(|r| r.participant_uid == h.participant_uid);
            let claim = claims.get(&h.participant_uid);
            DecPartyParticipantRow {
                dec_party_id: party_id.clone(),
                participant_uid: h.participant_uid.clone(),
                permission: permission_label(h.permission).to_string(),
                owner_key: claim
                    .and_then(|c| c.owner.clone())
                    .or_else(|| cached.and_then(|r| r.owner_key.clone())),
                signing_key: claim
                    .and_then(|c| c.daml.clone())
                    .or_else(|| cached.and_then(|r| r.signing_key.clone())),
            }
        })
        .collect()
}

/// Fill the local `dec_party` cache from the new head state the way a
/// `/decentralized-parties` refresh would, plus the keys every member
/// claimed for itself, so the joiner's owner key is on every node for a
/// later kick (design section 5) and the joiner learns every member's key.
async fn persist_party_cache(
    db: &SqlitePool,
    me: &CantonId,
    facts: &Facts,
    record: &WorkflowProposalRecord,
    counted: &[Acceptance],
    dnd: &DecentralizedNamespaceDefinition,
    p2p: &PartyToParticipant,
) -> Result<()> {
    let claims = key_claims(record, counted);
    let existing = db.get_dec_party_participants(&facts.party).await?;
    let rows = cache_rows(&facts.party, p2p, &existing, &claims);
    let party_id = facts.party.to_string();
    let cached_party = db
        .get_dec_parties_by_prefix(&facts.prefix)
        .await?
        .into_iter()
        .find(|r| r.party_id == party_id);
    let my_owner_key = claims
        .get(&me.to_string())
        .and_then(|c| c.owner.clone())
        .or_else(|| cached_party.and_then(|r| r.my_owner_key));

    let mut tx = db.begin_transaction().await?;
    tx.upsert_dec_party(&DecPartyRow {
        party_id,
        prefix: facts.prefix.clone(),
        threshold: i64::from(dnd.threshold),
        updated_at: now_secs(),
        my_owner_key,
    })
    .await?;
    tx.replace_dec_party_owners(&facts.party, &dnd.owners)
        .await?;
    tx.replace_dec_party_participants(&facts.party, &rows)
        .await?;
    Commitable::commit(tx).await?;
    tracing::info!(party = %facts.party, members = rows.len(), "dec_party cache written");
    Ok(())
}

/// [`persist_party_cache`] as a best-effort step: a failed cache write must
/// not stall a run whose topology already landed; the next parties refresh
/// or the next tick fills it.
async fn persist_party_cache_best_effort(
    ctx: &TickCtx<'_>,
    proposal: &ActiveProposal,
    facts: &Facts,
    counted: &[Acceptance],
    dnd: Option<&AcceptedMapping<DecentralizedNamespaceDefinition>>,
    p2p: Option<&AcceptedMapping<PartyToParticipant>>,
) {
    let (Some(dnd), Some(p2p)) = (dnd, p2p) else {
        return;
    };
    if let Err(e) = persist_party_cache(
        ctx.db(),
        &ctx.participant_id,
        facts,
        &proposal.record,
        counted,
        &dnd.mapping,
        &p2p.mapping,
    )
    .await
    {
        tracing::warn!(party = %facts.party, error = %e, "dec_party cache not written");
    }
}

/// The counted acceptances (design D6) with the key caches refreshed. The
/// only error is a duplicate acceptor, which is permanent: the caller fails
/// the run.
async fn count_acceptances(
    ctx: &TickCtx<'_>,
    proposal: &ActiveProposal,
    party: &CantonId,
) -> Result<Vec<Acceptance>> {
    let raw = ctx.proposals.acceptances_for(&proposal.contract_id);
    let counted = proposals::counted_acceptances_verified(ctx.ol.config(), proposal, &raw).await?;
    record_keys(ctx.db(), party, &counted).await;
    Ok(counted)
}

/// Project "invitees that accepted" into `connected_peers`. The row is
/// re-read first so a concurrent cancel or dismiss is not overwritten.
///
/// TODO(engine/mod.rs): shared by every kind driver.
async fn record_connected_peers(
    db: &SqlitePool,
    run: &WorkflowRun,
    accepted: Vec<CantonId>,
) -> Result<()> {
    if run.connected_peers.iter().collect::<BTreeSet<_>>() == accepted.iter().collect() {
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

/// Whether the proposal is still active and unexpired and the row is still
/// in progress at this step. A proposer checks this before every topology
/// write so a cancel that landed during the tick wins.
async fn recheck_live(ctx: &TickCtx<'_>, run: &WorkflowRun, meta: &RunMeta) -> Result<bool> {
    let Some(proposal) = proposals::read_proposal(ctx.client, &meta.proposal_cid).await? else {
        tracing::debug!(proposal = %meta.proposal_cid, "proposal no longer active; no write");
        return Ok(false);
    };
    if ctx.is_expired(&proposal) {
        tracing::debug!(proposal = %meta.proposal_cid, "proposal expired; no write");
        return Ok(false);
    }
    let Some(fresh) = ctx.db().get_workflow_run(&run.instance_name).await? else {
        return Ok(false);
    };
    if fresh.status != WorkflowProgress::InProgress || fresh.current_step != run.current_step {
        tracing::debug!(
            instance = %run.instance_name,
            status = %fresh.status,
            step = %fresh.current_step,
            "row moved under the tick; no write"
        );
        return Ok(false);
    }
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

/// Design D5 step 5, immediately before a co-sign: [`recheck_live`], then
/// the hash is pinned on the row and in `proposal_decisions`. A pin for a
/// different hash of the same mapping fails the run closed: one mapping,
/// one transaction. Returns `false` when the node must not sign this tick.
///
/// TODO(engine/mod.rs): shared by every kind driver.
async fn recheck_and_pin(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    key: &str,
    hash_hex: &str,
) -> Result<bool> {
    if !recheck_live(ctx, run, meta).await? {
        return Ok(false);
    }
    let db = ctx.db();
    let pinned = db
        .get_workflow_run(&run.instance_name)
        .await?
        .and_then(|fresh| read_run_meta(&fresh))
        .and_then(|m| m.topology_hashes.get(key).cloned());
    if let Some(pinned) = pinned
        && pinned != hash_hex
    {
        fail_run(
            db,
            run,
            &format!(
                "a different {key} transaction {pinned} was pinned earlier; refusing to sign {hash_hex}"
            ),
        )
        .await?;
        return Ok(false);
    }
    pin_topology_hash(db, &run.instance_name, key, hash_hex).await?;
    pin_decision_hash(db, &meta.proposal_cid, hash_hex).await?;
    Ok(true)
}

/// Drop a pin so the next tick proposes again (design D10, retry).
async fn unpin(db: &SqlitePool, run: &WorkflowRun, meta: &RunMeta, key: &str) -> Result<()> {
    let mut fresh = meta.clone();
    fresh.topology_hashes.remove(key);
    write_run_meta(db, &run.instance_name, &fresh).await
}

/// Fail a coordinator run and tell the invitees through
/// `WorkflowProposal_Finish { succeeded = false }` (design D10). The finish
/// is best effort: the proposal expires on its own.
async fn fail_coordinator(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    reason: String,
) -> Result<()> {
    fail_run(ctx.db(), run, &reason).await?;
    if let Err(e) = proposals::finish(ctx.client, &meta.proposal_cid, false, Some(reason)).await {
        tracing::warn!(proposal = %meta.proposal_cid, error = %e, "WorkflowProposal_Finish failed");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Key material and acceptance
// ---------------------------------------------------------------------------

/// The key material a current owner puts on its proposal or acceptance: its
/// owner fingerprint for this party, the key bytes when the vault has them,
/// and its Daml key fingerprint (design D6).
///
/// TODO(onledger/keys.rs): shared by the kick and change-threshold drivers.
async fn member_key_material(
    config: &NodeConfig,
    db: &SqlitePool,
    party: &CantonId,
    prefix: &str,
) -> Result<ProposerKeyMaterial> {
    let identity = keys::local_identity_for_party(config, db, Some(party), Some(prefix)).await?;
    let vault = keys::list_vault_keys(config).await?;
    let Some(owner_fp) = pick_owner_fingerprint(&identity.owner_fingerprints, &vault, prefix)
    else {
        bail!("this node holds no owner key of {party}; only a current owner can take part");
    };
    let key_hex = vault
        .iter()
        .find(|k| k.fingerprint == owner_fp)
        .map(|k| hex::encode(k.key.encode_to_vec()));
    Ok(ProposerKeyMaterial {
        namespace_fingerprint: Some(owner_fp),
        signing_public_key_hex: key_hex,
        daml_key_fingerprint: identity.daml_key_fingerprint,
    })
}

/// The member party this node uses for `party`, for `KnownMember` cards.
async fn member_party_for(db: &SqlitePool, party: &CantonId) -> Result<Option<CantonId>> {
    Ok(db
        .get_party_credentials(party)
        .await?
        .filter(|c| c.kind == CredentialKind::Decparty)
        .map(|c| c.member_party_id))
}

/// Whether this node's acceptance exists after this call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Accepted {
    /// An acceptance exists (on the ledger or recorded locally).
    Yes,
    /// The proposal is gone or expired; `drive` reconciles it next tick.
    NotPossible,
}

/// Exercise `WorkflowProposal_Accept` once (design D6). The acceptance is
/// looked for in this tick's snapshot and in the local artefact, because
/// the snapshot may lag one tick behind a write.
async fn ensure_accepted(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    material: &ProposerKeyMaterial,
    member_party: Option<CantonId>,
) -> Result<Accepted> {
    let db = ctx.db();
    let me = &ctx.identity.node_party;
    let on_ledger = ctx
        .proposals
        .acceptances
        .iter()
        .any(|a| a.record.proposal == meta.proposal_cid && a.record.acceptor == *me);
    if on_ledger
        || db
            .read_artifact(&run.instance_name, ACCEPTANCE_CID_ARTIFACT, None)
            .await?
            .is_some()
    {
        return Ok(Accepted::Yes);
    }
    let Some(proposal) = proposals::read_proposal(ctx.client, &meta.proposal_cid).await? else {
        return Ok(Accepted::NotPossible);
    };
    if ctx.is_expired(&proposal) {
        return Ok(Accepted::NotPossible);
    }
    let args = accept_args(ctx.identity, material, member_party);
    let cid = proposals::accept(ctx.client, &meta.proposal_cid, &args).await?;
    db.write_artifact(
        &run.instance_name,
        ACCEPTANCE_CID_ARTIFACT,
        None,
        cid.as_bytes(),
    )
    .await?;
    tracing::info!(instance = %run.instance_name, acceptance = %cid, "WorkflowProposal accepted");
    Ok(Accepted::Yes)
}

/// The section-5 reference set for this node, or `None` while the joiner's
/// root `NamespaceDelegation` is not effective in the synchronizer store
/// (design D4: the NSD is the single source of key bytes).
///
/// # Errors
/// Returns an error when the joiner accepted without key material (fails
/// closed) or a read fails.
async fn expectations(
    ctx: &TickCtx<'_>,
    proposal: &ActiveProposal,
    counted: &[Acceptance],
    facts: &Facts,
    head: HeadState,
) -> Result<Option<Expectations>> {
    let Some(joiner_fp) = joiner_fingerprint(counted, &facts.joiner)? else {
        return Ok(None);
    };
    let config = ctx.ol.config();
    let joiner_key = match topology::read_root_delegation_key(config, &ctx.sync_id, &joiner_fp)
        .await
    {
        Ok(key) => key,
        Err(e) => {
            tracing::debug!(fingerprint = %joiner_fp, error = %e, "waiting for the joiner's root NamespaceDelegation");
            return Ok(None);
        }
    };
    let identity =
        keys::local_identity_for_party(config, ctx.db(), Some(&facts.party), Some(&facts.prefix))
            .await?;
    let hosting_ok = verify_hosting(
        config,
        &proposal.record.proposer,
        &facts.proposer_participant,
    )
    .await?
    .has_submission();
    Ok(Some(
        Expectations::new(&proposal.record, counted, head, identity)
            .with_owner_keys(BTreeMap::from([(joiner_fp, joiner_key)]))
            .with_proposer_hosting(hosting_ok),
    ))
}

// ---------------------------------------------------------------------------
// Co-signing one mapping
// ---------------------------------------------------------------------------

/// What one mapping's co-sign step did this tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StepOutcome {
    /// `base + 1` (or later) is effective: move to the next mapping.
    Effective,
    /// Signed, waiting, or nothing to do yet: stay in the step.
    Waiting,
    /// The run was marked `Failed`.
    Failed,
}

/// One mapping a member co-signs, with what this tick read about it.
struct MappingStep<'a, M> {
    /// `RunMeta::topology_hashes` key: `dnd` or `p2p`.
    key: &'static str,
    /// The base serial the proposal recorded for this mapping.
    base: u32,
    accepted: Option<&'a AcceptedState>,
    pending: &'a [PendingProposal<M>],
    proposer_fingerprint: &'a str,
    own_fingerprints: &'a BTreeSet<String>,
}

/// Drive one mapping (DND or P2P) of a member run: classify the accepted
/// state, pick the proposer's candidate, validate, re-read (design D5 step
/// 5), pin, and `Authorize { transaction_hash }`. Never sleeps.
async fn cosign_mapping<M>(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    step: MappingStep<'_, M>,
    check: impl Fn(&PendingProposal<M>) -> Check,
) -> Result<StepOutcome> {
    let MappingStep {
        key,
        base,
        accepted,
        pending,
        proposer_fingerprint,
        own_fingerprints,
    } = step;
    let db = ctx.db();
    match progress(accepted, base, ctx.now_micros) {
        Progress::Landed | Progress::Beyond(_) => return Ok(StepOutcome::Effective),
        Progress::Pending(serial) => {
            tracing::debug!(instance = %run.instance_name, key, serial, "accepted, not yet effective");
            return Ok(StepOutcome::Waiting);
        }
        Progress::AtBase => {}
        Progress::Missing | Progress::Moved(_) => {
            fail_run(
                db,
                run,
                &format!(
                    "topology moved: the accepted {key} serial is {accepted:?}, the proposal \
                     recorded base serial {base}"
                ),
            )
            .await?;
            return Ok(StepOutcome::Failed);
        }
    }

    let pinned = meta.topology_hashes.get(key).map(String::as_str);
    let candidate = match select_pending(
        pending,
        base + 1,
        proposer_fingerprint,
        pinned,
        own_fingerprints,
        check,
    ) {
        Selection::Waiting => {
            tracing::debug!(instance = %run.instance_name, key, "no proposal from the proposer yet");
            return Ok(StepOutcome::Waiting);
        }
        Selection::AlreadySigned(p) => {
            tracing::debug!(instance = %run.instance_name, key, hash = %p.hash_hex, "already signed; waiting for effect");
            return Ok(StepOutcome::Waiting);
        }
        Selection::Mismatch(reasons) => {
            fail_run(
                db,
                run,
                &format!("refusing to co-sign the {key}: {}", reasons.join("; ")),
            )
            .await?;
            return Ok(StepOutcome::Failed);
        }
        Selection::PinMismatch { pinned, found } => {
            fail_run(
                db,
                run,
                &format!(
                    "the pinned {key} transaction {pinned} is gone and other proposals exist at \
                     the same serial ({found:?}); refusing to sign a second transaction"
                ),
            )
            .await?;
            return Ok(StepOutcome::Failed);
        }
        Selection::Match(p) => p,
    };

    if !recheck_and_pin(ctx, run, meta, key, &candidate.hash_hex).await? {
        return Ok(StepOutcome::Waiting);
    }
    let outcome = topology::cosign_by_hash(
        ctx.ol.config(),
        &ctx.sync_id,
        &candidate.hash_hex,
        &candidate.signed_by,
    )
    .await?;
    match outcome {
        CosignOutcome::NotFound => {
            tracing::debug!(hash = %candidate.hash_hex, "proposal not in the store yet; retry next tick");
        }
        CosignOutcome::Signed | CosignOutcome::AlreadySigned => {
            tracing::info!(instance = %run.instance_name, hash = %candidate.hash_hex, ?outcome, "{key} co-signed");
        }
    }
    Ok(StepOutcome::Waiting)
}

// ---------------------------------------------------------------------------
// Export and manifest (every current host, design D9)
// ---------------------------------------------------------------------------

/// Export the snapshot for the joiner into the spool and publish this
/// node's `AcsManifest`, once. A manifest already on the ledger means both
/// happened; a spool file without a manifest is reused, not re-exported.
async fn publish_manifest_once(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    proposal: &ActiveProposal,
    facts: &Facts,
) -> Result<()> {
    let serial = facts.activation_serial();
    let manifests = acs::read_manifests(ctx.client, &facts.party).await?;
    if acs::own_manifest(&manifests, &ctx.identity.node_party, &facts.joiner, serial).is_some() {
        return Ok(());
    }
    let config = ctx.ol.config();
    let path = acs::spool_path(config, &facts.party, &facts.joiner, serial);
    let replication =
        acs::replication_target(&facts.party, &facts.joiner, run.instance_name.clone());
    // The party's own ledger credential, so the manifest can name the
    // packages its contracts need. Without it the joiner learns about a
    // missing package during the import instead of before it.
    let token = match crate::onledger::submission::dec_party_credentials(ctx.ol, &facts.party).await
    {
        Ok(creds) => Some(creds.token),
        Err(e) => {
            tracing::warn!(party = %facts.party, error = %format!("{e:#}"), "no ledger credential for the party; the manifest will name no packages");
            None
        }
    };
    let file =
        acs::spool_or_export(config, ctx.db(), &replication, &path, token.as_deref()).await?;
    let observers = manifest_observers(&proposal.record, &ctx.identity.node_party);
    acs::publish_manifest(
        ctx.client,
        &observers,
        &facts.party,
        &facts.joiner,
        serial,
        &file,
    )
    .await?;
    Ok(())
}

/// Whether the head P2P still marks the joiner `Onboarding`. `None` when
/// the party has no mapping at all.
async fn joiner_onboarding(ctx: &TickCtx<'_>, facts: &Facts) -> Result<Option<bool>> {
    Ok(accepted_p2p(ctx, facts)
        .await?
        .map(|head| has_onboarding_marker(&head.mapping, &facts.joiner.to_string())))
}

// ---------------------------------------------------------------------------
// Coordinator steps
// ---------------------------------------------------------------------------

/// `GenerateKeys`: the coordinator is a current owner and advertised its key
/// in `prepare`; nothing to generate.
async fn coordinator_generate_keys(ctx: &TickCtx<'_>, run: &WorkflowRun) -> Result<()> {
    advance_step(ctx.db(), run, STEP_WAITING_FOR_ACCEPTANCES).await
}

/// `WaitingForAcceptances`: add-party needs every invitee (section 6).
async fn coordinator_wait_acceptances(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &ActiveProposal,
    facts: &Facts,
) -> Result<()> {
    let counted = match count_acceptances(ctx, proposal, &facts.party).await {
        Ok(c) => c,
        Err(e) => return fail_coordinator(ctx, run, meta, e.to_string()).await,
    };
    record_connected_peers(ctx.db(), run, accepted_parties(&counted)).await?;
    if !all_invitees_accepted(&proposal.record.invitees, &counted) {
        tracing::debug!(
            instance = %run.instance_name,
            accepted = counted.len(),
            invited = proposal.record.invitees.len(),
            "waiting for acceptances"
        );
        return Ok(());
    }
    advance_step(ctx.db(), run, STEP_PROPOSE_CHANGES).await
}

/// `ProposeChanges`: capture the export offset (design D9), wait for the
/// joiner's root NSD, then propose the DND at `dndBaseSerial + 1`.
async fn coordinator_propose_changes(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &ActiveProposal,
    facts: &Facts,
) -> Result<()> {
    let config = ctx.ol.config();
    let db = ctx.db();
    // Before this host's first Authorize, so the snapshot export can find
    // the joiner's activation after it.
    acs::capture_export_offset(db, config, run, &facts.party, &facts.joiner, facts.p2p_base)
        .await?;

    let counted = match count_acceptances(ctx, proposal, &facts.party).await {
        Ok(c) => c,
        Err(e) => return fail_coordinator(ctx, run, meta, e.to_string()).await,
    };
    let joiner_fp = match joiner_fingerprint(&counted, &facts.joiner) {
        Ok(Some(fp)) => fp,
        Ok(None) => {
            tracing::debug!(instance = %run.instance_name, "the joiner's acceptance is not visible yet");
            return Ok(());
        }
        Err(e) => return fail_coordinator(ctx, run, meta, e.to_string()).await,
    };
    // Design D5 order rule: a DND with a new owner is rejected until that
    // owner's root delegation is effective.
    if let Err(e) = topology::read_root_delegation_key(config, &ctx.sync_id, &joiner_fp).await {
        tracing::debug!(fingerprint = %joiner_fp, error = %e, "waiting for the joiner's root NamespaceDelegation");
        return Ok(());
    }

    let head = accepted_dnd(ctx, facts).await?;
    match progress(
        state_of(head.as_ref()).as_ref(),
        facts.dnd_base,
        ctx.now_micros,
    ) {
        Progress::AtBase => {}
        Progress::Landed => return advance_step(db, run, STEP_AWAIT_CHANGES).await,
        Progress::Pending(_) => return Ok(()),
        other => {
            return fail_coordinator(
                ctx,
                run,
                meta,
                format!(
                    "topology moved: DND base serial {} recorded, found {other:?}",
                    facts.dnd_base
                ),
            )
            .await;
        }
    }
    let Some(head) = head else {
        return Ok(());
    };
    let mapping = topology::build_add_party_dnd(&head.mapping, &joiner_fp, facts.threshold);
    let serial = facts.dnd_base + 1;

    // A crash between propose and pin left this node's proposal in the
    // store; adopt it instead of proposing a duplicate Canton rejects.
    let pending = topology::list_pending_dnd(config, &ctx.sync_id, &facts.namespace()).await?;
    if let Some(mine) = pending
        .iter()
        .filter(|p| p.serial == serial && p.is_add_replace())
        .find(|p| Some(&p.mapping) == topology::dnd_of(&mapping))
    {
        pin_topology_hash(db, &run.instance_name, HASH_DND, &mine.hash_hex).await?;
        return advance_step(db, run, STEP_AWAIT_CHANGES).await;
    }

    if !recheck_live(ctx, run, meta).await? {
        return Ok(());
    }
    let tx = topology::propose_mapping(config, &ctx.sync_id, mapping, serial).await?;
    pin_topology_hash(db, &run.instance_name, HASH_DND, &tx.hash_hex).await?;
    advance_step(db, run, STEP_AWAIT_CHANGES).await
}

/// `AwaitChanges`: wait until the DND is effective, then propose the P2P at
/// `p2pBaseSerial + 1` and wait until it is effective (design D5 order).
async fn coordinator_await_changes(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &ActiveProposal,
    facts: &Facts,
) -> Result<()> {
    let config = ctx.ol.config();
    let db = ctx.db();
    let now = ctx.now_micros;

    // DND first.
    let dnd = accepted_dnd(ctx, facts).await?;
    match progress(state_of(dnd.as_ref()).as_ref(), facts.dnd_base, now) {
        Progress::Landed | Progress::Beyond(_) => {}
        Progress::Pending(_) => return Ok(()),
        Progress::AtBase => {
            let pending =
                topology::list_pending_dnd(config, &ctx.sync_id, &facts.namespace()).await?;
            let hashes: Vec<String> = pending.iter().map(|p| p.hash_hex.clone()).collect();
            let pinned = meta.topology_hashes.get(HASH_DND).map(String::as_str);
            if should_repropose(pinned, &hashes, run.updated_at, now_secs()) {
                tracing::warn!(instance = %run.instance_name, ?pinned, "pinned DND proposal is gone; proposing again");
                unpin(db, run, meta, HASH_DND).await?;
                return advance_step(db, run, STEP_PROPOSE_CHANGES).await;
            }
            return Ok(());
        }
        other => {
            return fail_coordinator(
                ctx,
                run,
                meta,
                format!(
                    "topology moved: DND base serial {} recorded, found {other:?}",
                    facts.dnd_base
                ),
            )
            .await;
        }
    }

    // Then the P2P.
    let head = accepted_p2p(ctx, facts).await?;
    let state = state_of(head.as_ref());
    match progress(state.as_ref(), facts.p2p_base, now) {
        Progress::Landed | Progress::Beyond(_) => {
            match count_acceptances(ctx, proposal, &facts.party).await {
                Ok(counted) => {
                    persist_party_cache_best_effort(
                        ctx,
                        proposal,
                        facts,
                        &counted,
                        dnd.as_ref(),
                        head.as_ref(),
                    )
                    .await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "acceptances not countable; cache not written")
                }
            }
            return advance_step(db, run, STEP_AWAIT_REPLICATION).await;
        }
        Progress::Pending(_) => return Ok(()),
        Progress::AtBase => {}
        other => {
            return fail_coordinator(
                ctx,
                run,
                meta,
                format!(
                    "topology moved: P2P base serial {} recorded, found {other:?}",
                    facts.p2p_base
                ),
            )
            .await;
        }
    }
    let Some(head) = head else {
        return Ok(());
    };
    let serial = facts.p2p_base + 1;
    let pending = topology::list_pending_p2p(config, &ctx.sync_id, &facts.party).await?;

    if let Some(pinned) = meta.topology_hashes.get(HASH_P2P) {
        let hashes: Vec<String> = pending.iter().map(|p| p.hash_hex.clone()).collect();
        if should_repropose(Some(pinned), &hashes, run.updated_at, now_secs()) {
            tracing::warn!(instance = %run.instance_name, pinned, "pinned P2P proposal is gone; proposing again");
            unpin(db, run, meta, HASH_P2P).await?;
        }
        return Ok(());
    }

    let counted = match count_acceptances(ctx, proposal, &facts.party).await {
        Ok(c) => c,
        Err(e) => return fail_coordinator(ctx, run, meta, e.to_string()).await,
    };
    let joiner_fp = match joiner_fingerprint(&counted, &facts.joiner) {
        Ok(Some(fp)) => fp,
        Ok(None) => return Ok(()),
        Err(e) => return fail_coordinator(ctx, run, meta, e.to_string()).await,
    };
    let joiner_key = match topology::read_root_delegation_key(config, &ctx.sync_id, &joiner_fp)
        .await
    {
        Ok(key) => key,
        Err(e) => {
            tracing::debug!(fingerprint = %joiner_fp, error = %e, "waiting for the joiner's root NamespaceDelegation");
            return Ok(());
        }
    };
    let mapping =
        topology::build_add_party_p2p(&head.mapping, &facts.joiner, &joiner_key, facts.threshold);
    if let Some(mine) = pending
        .iter()
        .filter(|p| p.serial == serial && p.is_add_replace())
        .find(|p| Some(&p.mapping) == topology::p2p_of(&mapping))
    {
        pin_topology_hash(db, &run.instance_name, HASH_P2P, &mine.hash_hex).await?;
        return Ok(());
    }
    if !recheck_live(ctx, run, meta).await? {
        return Ok(());
    }
    let tx = topology::propose_mapping(config, &ctx.sync_id, mapping, serial).await?;
    pin_topology_hash(db, &run.instance_name, HASH_P2P, &tx.hash_hex).await?;
    Ok(())
}

/// `AwaitReplication`: export and publish this host's manifest (design D9:
/// every current host), then stay until the head P2P no longer marks the
/// joiner `Onboarding`; finish the proposal and complete.
async fn coordinator_await_replication(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &ActiveProposal,
    facts: &Facts,
) -> Result<()> {
    match joiner_onboarding(ctx, facts).await? {
        None => {
            return fail_coordinator(
                ctx,
                run,
                meta,
                format!("{} has no accepted PartyToParticipant", facts.party),
            )
            .await;
        }
        Some(true) => return publish_manifest_once(ctx, run, proposal, facts).await,
        Some(false) => {}
    }
    if let Err(e) = acs::cleanup_spool(ctx.ol.config(), &facts.party, &facts.joiner).await {
        tracing::warn!(error = %e, "spool cleanup failed; files stay until the run is dismissed");
    }
    // The row first: a run that completed must not flip to Failed when the
    // reconciliation sees the proposal gone before the finish is observed.
    complete_run(ctx.db(), run).await?;
    if let Err(e) = proposals::finish(ctx.client, &meta.proposal_cid, true, None).await {
        tracing::warn!(proposal = %meta.proposal_cid, error = %e, "WorkflowProposal_Finish failed; the proposal expires on its own");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Member and joiner steps
// ---------------------------------------------------------------------------

/// Joiner `GenerateKeys` (design D4, D6): the dual key and its root NSD,
/// the pre-activation offset (design D9), then the acceptance with the key
/// material.
async fn joiner_generate_keys(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    facts: &Facts,
) -> Result<()> {
    let config = ctx.ol.config();
    let key = keys::ensure_party_key(config, &facts.prefix).await?;
    if let Err(e) = topology::read_root_delegation_key(config, &ctx.sync_id, &key.fingerprint).await
    {
        tracing::debug!(fingerprint = %key.fingerprint, error = %e, "waiting for own root NamespaceDelegation");
        return Ok(());
    }
    // Before the acceptance: the coordinator proposes right after it, and
    // the offset must predate the activation.
    acs::capture_pre_activation_offset(ctx.db(), config, run, &facts.party).await?;
    let material = keys::proposer_key_material(&key);
    match ensure_accepted(ctx, run, meta, &material, None).await? {
        Accepted::Yes => advance_step(ctx.db(), run, STEP_COSIGN_CHANGES).await,
        Accepted::NotPossible => Ok(()),
    }
}

/// `CoSignChanges` for both variants: a member captures its export offset
/// and accepts first; both then validate and co-sign the DND, and once it
/// is effective the P2P (design D5, section 5).
async fn member_cosign_changes(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &ActiveProposal,
    facts: &Facts,
    variant: MemberVariant,
) -> Result<()> {
    let config = ctx.ol.config();
    let db = ctx.db();
    if variant == MemberVariant::Member {
        // Design D9: the first action of `CoSignChanges`, before any Authorize.
        acs::capture_export_offset(db, config, run, &facts.party, &facts.joiner, facts.p2p_base)
            .await?;
        let material = member_key_material(config, db, &facts.party, &facts.prefix).await?;
        let member_party = member_party_for(db, &facts.party).await?;
        if ensure_accepted(ctx, run, meta, &material, member_party).await? == Accepted::NotPossible
        {
            return Ok(());
        }
    }

    let counted = match count_acceptances(ctx, proposal, &facts.party).await {
        Ok(c) => c,
        Err(e) => return fail_run(db, run, &e.to_string()).await,
    };
    // The coordinator proposes only after every invitee accepted, so a
    // proposal seen before that is not ours to judge yet.
    if !all_invitees_accepted(&proposal.record.invitees, &counted) {
        tracing::debug!(instance = %run.instance_name, "waiting for every acceptance");
        return Ok(());
    }

    let dnd = accepted_dnd(ctx, facts).await?;
    let p2p = accepted_p2p(ctx, facts).await?;
    let head = HeadState {
        dnd: dnd.as_ref().map(|a| a.mapping.clone()),
        p2p: p2p.as_ref().map(|a| a.mapping.clone()),
    };
    let exp = match expectations(ctx, proposal, &counted, facts, head).await {
        Ok(Some(exp)) => exp,
        Ok(None) => return Ok(()),
        Err(e) => return fail_run(db, run, &e.to_string()).await,
    };
    let proposer_fp = match exp.required_proposer_fingerprint() {
        Ok(fp) => fp.to_string(),
        Err(e) => return fail_run(db, run, &e.to_string()).await,
    };
    let own = exp.identity.owner_fingerprints.clone();

    let dnd_state = state_of(dnd.as_ref());
    let dnd_serial = dnd_state.as_ref().map(|s| s.serial);
    let pending_dnd = topology::list_pending_dnd(config, &ctx.sync_id, &facts.namespace()).await?;
    let dnd_step = MappingStep {
        key: HASH_DND,
        base: facts.dnd_base,
        accepted: dnd_state.as_ref(),
        pending: &pending_dnd,
        proposer_fingerprint: &proposer_fp,
        own_fingerprints: &own,
    };
    match cosign_mapping(ctx, run, meta, dnd_step, |p| {
        validation::validate_dnd(p, &exp, dnd_serial)
    })
    .await?
    {
        StepOutcome::Effective => {}
        StepOutcome::Waiting | StepOutcome::Failed => return Ok(()),
    }

    let p2p_state = state_of(p2p.as_ref());
    let p2p_serial = p2p_state.as_ref().map(|s| s.serial);
    let pending_p2p = topology::list_pending_p2p(config, &ctx.sync_id, &facts.party).await?;
    let p2p_step = MappingStep {
        key: HASH_P2P,
        base: facts.p2p_base,
        accepted: p2p_state.as_ref(),
        pending: &pending_p2p,
        proposer_fingerprint: &proposer_fp,
        own_fingerprints: &own,
    };
    match cosign_mapping(ctx, run, meta, p2p_step, |p| {
        validation::validate_p2p(p, &exp, p2p_serial)
    })
    .await?
    {
        StepOutcome::Effective => {
            persist_party_cache_best_effort(
                ctx,
                proposal,
                facts,
                &counted,
                dnd.as_ref(),
                p2p.as_ref(),
            )
            .await;
            let next = match variant {
                MemberVariant::Joiner => STEP_SYNC_ACS,
                MemberVariant::Member => STEP_PUBLISH_MANIFEST,
            };
            advance_step(db, run, next).await
        }
        StepOutcome::Waiting | StepOutcome::Failed => Ok(()),
    }
}

/// Member `PublishManifest` (design D9): export and publish once the marked
/// P2P is effective here; complete when the joiner is observed onboarded.
async fn member_publish_manifest(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    proposal: &ActiveProposal,
    facts: &Facts,
) -> Result<()> {
    let head = accepted_p2p(ctx, facts).await?;
    let Some(head) = head else {
        return fail_run(
            ctx.db(),
            run,
            &format!("{} has no accepted PartyToParticipant", facts.party),
        )
        .await;
    };
    if !has_onboarding_marker(&head.mapping, &facts.joiner.to_string()) {
        if let Err(e) = acs::cleanup_spool(ctx.ol.config(), &facts.party, &facts.joiner).await {
            tracing::warn!(error = %e, "spool cleanup failed; files stay until the run is dismissed");
        }
        return complete_run(ctx.db(), run).await;
    }
    match progress(
        state_of(Some(&head)).as_ref(),
        facts.p2p_base,
        ctx.now_micros,
    ) {
        Progress::Landed | Progress::Beyond(_) => {
            publish_manifest_once(ctx, run, proposal, facts).await
        }
        _ => Ok(()),
    }
}

/// Joiner `SyncAcs` (design D9): the empty fast path on a verified
/// manifest, or the import endpoint's completion marker.
async fn joiner_sync_acs(ctx: &TickCtx<'_>, run: &WorkflowRun, facts: &Facts) -> Result<()> {
    let query = acs::SyncAcsQuery {
        party: &facts.party,
        joiner: &ctx.participant_id,
        activation_serial: facts.activation_serial(),
        instance_name: &run.instance_name,
    };
    let decision =
        acs::empty_fast_path_or_wait(ctx.ol.config(), ctx.db(), ctx.client, &ctx.sync_id, &query)
            .await?;
    match decision {
        SyncDecision::EmptySnapshot {
            exporter_participant,
        } => {
            tracing::info!(instance = %run.instance_name, exporter = %exporter_participant, "verified empty snapshot; skipping the import");
            advance_step(ctx.db(), run, STEP_CLEAR_ONBOARDING).await
        }
        SyncDecision::Imported => {
            tracing::info!(instance = %run.instance_name, "ACS import recorded");
            advance_step(ctx.db(), run, STEP_CLEAR_ONBOARDING).await
        }
        SyncDecision::MissingPackages(missing) => {
            fail_run(
                ctx.db(),
                run,
                &format!(
                    "this participant is missing {n} package(s) the party's contracts need — \
                     vet the corresponding DAR(s) here before the import; the ACS will not \
                     import without them. Missing package ids: {missing:?}",
                    n = missing.len()
                ),
            )
            .await
        }
        SyncDecision::Waiting(why) => {
            tracing::debug!(instance = %run.instance_name, why, "SyncAcs waiting");
            Ok(())
        }
    }
}

/// Joiner `ClearOnboarding` (design D5): one `ClearPartyOnboardingFlag`
/// request per tick until the head P2P shows no marker; Canton needs only
/// this participant's signature for it.
async fn joiner_clear_onboarding(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    facts: &Facts,
) -> Result<()> {
    let config = ctx.ol.config();
    let db = ctx.db();
    match joiner_onboarding(ctx, facts).await? {
        None => {
            return fail_run(
                db,
                run,
                &format!("{} has no accepted PartyToParticipant", facts.party),
            )
            .await;
        }
        Some(false) => return complete_run(db, run).await,
        Some(true) => {}
    }
    let target =
        acs::replication_target(&facts.party, &ctx.participant_id, run.instance_name.clone());
    match request_onboarding_flag_clear(config, db, &target).await {
        Ok(outcome) => {
            tracing::debug!(instance = %run.instance_name, ?outcome, "ClearPartyOnboardingFlag requested")
        }
        Err(e) => {
            tracing::warn!(instance = %run.instance_name, error = %e, "ClearPartyOnboardingFlag failed; retry next tick")
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

impl KindDriver for AddParty {
    fn kind() -> WorkflowKind {
        WorkflowKind::AddParty
    }

    /// This kind's member attaches key material a member step generates,
    /// so it exercises `Accept` itself rather than through [`super::drive`].
    fn member_publishes_own_acceptance() -> bool {
        true
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

    /// Section 6 preflight: the joiner is not a host yet, has a registry
    /// entry, and is not quarantined for this party after a broken import.
    async fn preflight(ol: &OnLedger, req: &StartRequest) -> Result<()> {
        let StartRequest::AddParty {
            dec_party_id,
            new_participant_id,
            ..
        } = req
        else {
            return Ok(());
        };
        let config = ol.config();
        let sync_id = utils::get_synchronizer_id(config).await?;
        let joiner_uid = new_participant_id.to_string();
        if let Some(head) = topology::read_accepted_p2p(config, &sync_id, dec_party_id).await?
            && head
                .mapping
                .participants
                .iter()
                .any(|h| h.participant_uid == joiner_uid)
        {
            return Err(PreflightRejected::new(format!(
                "{new_participant_id} already hosts {dec_party_id}"
            ))
            .into());
        }
        let has_entry = ol
            .registry_snapshot()
            .await
            .get(new_participant_id)
            .and_then(|h| h.node_party.as_ref())
            .is_some();
        if !has_entry {
            return Err(PreflightRejected::peers(vec![(
                new_participant_id.clone(),
                "no registry entry visible (the peer has not added you, or has not vetted the \
                 coordination package)"
                    .to_string(),
            )])
            .into());
        }
        if let Some(reason) = ol
            .db()
            .get_acs_import_quarantine(dec_party_id, new_participant_id)
            .await?
        {
            return Err(PreflightRejected::new(format!(
                "{new_participant_id} is quarantined for {dec_party_id}: {reason}. Lift it with \
                 DELETE /acs-import-quarantine before adding it again"
            ))
            .into());
        }
        Ok(())
    }

    /// The coordinator is a current owner: its existing key material for
    /// the party goes on the proposal (design D6). Nothing is generated.
    async fn prepare(ol: &OnLedger, req: &StartRequest) -> Result<ProposalExtras> {
        let StartRequest::AddParty { dec_party_id, .. } = req else {
            bail!("AddParty::prepare called with a {} request", req.kind());
        };
        let keys =
            member_key_material(ol.config(), ol.db(), dec_party_id, &dec_party_id.prefix).await?;
        Ok(ProposalExtras {
            keys,
            ..ProposalExtras::default()
        })
    }

    async fn tick_coordinator(ctx: &TickCtx<'_>, run: &WorkflowRun, meta: &RunMeta) -> Result<()> {
        let Some(proposal) = proposal_of(ctx, meta) else {
            return Ok(());
        };
        let facts = match facts_of(&proposal.record) {
            Ok(f) => f,
            Err(e) => return fail_coordinator(ctx, run, meta, e.to_string()).await,
        };
        match run.current_step.as_str() {
            STEP_GENERATE_KEYS => coordinator_generate_keys(ctx, run).await,
            STEP_WAITING_FOR_ACCEPTANCES => {
                coordinator_wait_acceptances(ctx, run, meta, proposal, &facts).await
            }
            STEP_PROPOSE_CHANGES => {
                coordinator_propose_changes(ctx, run, meta, proposal, &facts).await
            }
            STEP_AWAIT_CHANGES => coordinator_await_changes(ctx, run, meta, proposal, &facts).await,
            STEP_AWAIT_REPLICATION => {
                coordinator_await_replication(ctx, run, meta, proposal, &facts).await
            }
            _ => Ok(()),
        }
    }

    async fn tick_member(ctx: &TickCtx<'_>, run: &WorkflowRun, meta: &RunMeta) -> Result<()> {
        let Some(proposal) = proposal_of(ctx, meta) else {
            return Ok(());
        };
        let facts = match facts_of(&proposal.record) {
            Ok(f) => f,
            Err(e) => return fail_run(ctx.db(), run, &e.to_string()).await,
        };
        let variant = meta.member_variant.unwrap_or(MemberVariant::Member);
        match (variant, run.current_step.as_str()) {
            (MemberVariant::Joiner, STEP_GENERATE_KEYS) => {
                joiner_generate_keys(ctx, run, meta, &facts).await
            }
            (_, STEP_COSIGN_CHANGES) => {
                member_cosign_changes(ctx, run, meta, proposal, &facts, variant).await
            }
            (MemberVariant::Joiner, STEP_SYNC_ACS) => joiner_sync_acs(ctx, run, &facts).await,
            (MemberVariant::Joiner, STEP_CLEAR_ONBOARDING) => {
                joiner_clear_onboarding(ctx, run, &facts).await
            }
            (MemberVariant::Member, STEP_PUBLISH_MANIFEST) => {
                member_publish_manifest(ctx, run, proposal, &facts).await
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::SigningKeyUsage, protocol::v30::enums::TopologyChangeOp,
    };

    use super::*;
    use crate::onledger::{
        daml::{
            ActiveContract,
            codec::{WorkflowAcceptanceRecord, tests::proposal_full},
        },
        topology::tests::{NS, key, participant},
        validation::ValidationError,
    };

    fn party(prefix: &str) -> CantonId {
        CantonId::parse(&format!("{prefix}::{NS}")).expect("id")
    }

    fn add_party_proposal() -> WorkflowProposalRecord {
        let mut p = proposal_full();
        p.kind = WorkflowKind::AddParty;
        p.invitees = vec![party("node-b"), party("node-d")];
        p.participants = vec![
            participant(1).to_string(),
            participant(2).to_string(),
            participant(4).to_string(),
        ];
        p.new_participant = Some(participant(4).to_string());
        p.kicked_participant = None;
        p.threshold = Some(2);
        p.dnd_base_serial = Some(3);
        p.p2p_base_serial = Some(5);
        p
    }

    fn acceptance(who: &str, participant_n: u8, fp: Option<&str>) -> Acceptance {
        ActiveContract {
            contract_id: format!("acc-{who}"),
            offset: 1,
            record: WorkflowAcceptanceRecord {
                proposal: "00proposal".into(),
                proposer: party("node-a"),
                acceptor: party(who),
                observers: vec![],
                run_id: "r".into(),
                participant_id: participant(participant_n).to_string(),
                namespace_fingerprint: fp.map(str::to_string),
                signing_public_key_hex: None,
                daml_key_fingerprint: None,
                member_party: None,
                accepted_at: 1,
            },
        }
    }

    fn state(serial: u32, valid_from_micros: i64) -> AcceptedState {
        AcceptedState {
            serial,
            valid_from: Some(prost_types::Timestamp {
                seconds: valid_from_micros / 1_000_000,
                nanos: i32::try_from((valid_from_micros % 1_000_000) * 1_000).unwrap_or(0),
            }),
        }
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

    fn ok(_: &PendingProposal<()>) -> Check {
        Ok(())
    }

    fn refuse(_: &PendingProposal<()>) -> Check {
        Err(ValidationError("nope".into()))
    }

    #[test]
    fn step_lists_match_design_section_6() {
        assert_eq!(
            COORDINATOR_STEPS,
            [
                "GenerateKeys",
                "WaitingForAcceptances",
                "ProposeChanges",
                "AwaitChanges",
                "AwaitReplication",
                "Complete"
            ]
        );
        assert_eq!(
            JOINER_STEPS,
            [
                "GenerateKeys",
                "CoSignChanges",
                "SyncAcs",
                "ClearOnboarding",
                "Complete"
            ]
        );
        assert_eq!(
            MEMBER_STEPS,
            ["CoSignChanges", "PublishManifest", "Complete"]
        );
        assert_eq!(AddParty::member_steps(None), MEMBER_STEPS);
        assert_eq!(
            AddParty::member_steps(Some(MemberVariant::Joiner)),
            JOINER_STEPS
        );
    }

    #[test]
    fn facts_read_every_required_field() {
        let facts = facts_of(&add_party_proposal()).expect("facts");
        assert_eq!(facts.party, party("cbtc"));
        assert_eq!(facts.prefix, "cbtc");
        assert_eq!(facts.joiner, participant(4));
        assert_eq!(facts.threshold, 2);
        assert_eq!(facts.dnd_base, 3);
        assert_eq!(facts.p2p_base, 5);
        assert_eq!(facts.activation_serial(), 6);
        assert_eq!(facts.namespace(), NS);
        assert_eq!(facts.proposer_participant, participant(1));
    }

    #[test]
    fn facts_fail_closed_on_missing_fields() {
        let mut no_party = add_party_proposal();
        no_party.dec_party_id = None;
        assert!(facts_of(&no_party).is_err());

        let mut no_joiner = add_party_proposal();
        no_joiner.new_participant = None;
        assert!(facts_of(&no_joiner).is_err());

        let mut no_threshold = add_party_proposal();
        no_threshold.threshold = None;
        assert!(facts_of(&no_threshold).is_err());

        let mut zero_base = add_party_proposal();
        zero_base.dnd_base_serial = Some(0);
        assert!(
            facts_of(&zero_base)
                .unwrap_err()
                .to_string()
                .contains("dndBaseSerial")
        );

        let mut no_p2p_base = add_party_proposal();
        no_p2p_base.p2p_base_serial = None;
        assert!(facts_of(&no_p2p_base).is_err());

        let mut wrong_kind = add_party_proposal();
        wrong_kind.kind = WorkflowKind::Kick;
        assert!(facts_of(&wrong_kind).is_err());
    }

    #[test]
    fn progress_classifies_the_accepted_serial_against_the_base() {
        let now = 1_000_000_000;
        assert_eq!(progress(None, 3, now), Progress::Missing);
        assert_eq!(progress(Some(&state(2, 0)), 3, now), Progress::Moved(2));
        assert_eq!(progress(Some(&state(3, 0)), 3, now), Progress::AtBase);
        assert_eq!(
            progress(Some(&state(4, now + 1)), 3, now),
            Progress::Pending(4)
        );
        assert_eq!(progress(Some(&state(4, now)), 3, now), Progress::Landed);
        assert_eq!(
            progress(Some(&state(5, now - 1)), 3, now),
            Progress::Beyond(5)
        );
        let no_valid_from = AcceptedState {
            serial: 4,
            valid_from: None,
        };
        assert_eq!(progress(Some(&no_valid_from), 3, now), Progress::Pending(4));
    }

    #[test]
    fn selection_waits_without_a_proposer_signed_candidate_at_the_serial() {
        let own = BTreeSet::from(["me".to_string()]);
        let list = vec![
            pending(
                "h-other-serial",
                5,
                &["proposer"],
                TopologyChangeOp::AddReplace,
            ),
            pending("h-unsigned", 4, &["stranger"], TopologyChangeOp::AddReplace),
            pending("h-remove", 4, &["proposer"], TopologyChangeOp::Remove),
        ];
        assert!(matches!(
            select_pending(&list, 4, "proposer", None, &own, ok),
            Selection::Waiting
        ));
        assert!(matches!(
            select_pending(&[], 4, "proposer", Some("h-pinned"), &own, ok),
            Selection::Waiting
        ));
    }

    #[test]
    fn selection_signs_the_first_valid_candidate_and_skips_stale_ones() {
        let own = BTreeSet::from(["me".to_string()]);
        let list = vec![
            pending("h-stale", 4, &["proposer"], TopologyChangeOp::AddReplace),
            pending("h-good", 4, &["proposer"], TopologyChangeOp::AddReplace),
        ];
        let check = |p: &PendingProposal<()>| {
            if p.hash_hex == "h-good" {
                Ok(())
            } else {
                Err(ValidationError("stale".into()))
            }
        };
        match select_pending(&list, 4, "proposer", None, &own, check) {
            Selection::Match(p) => assert_eq!(p.hash_hex, "h-good"),
            other => panic!("{other:?}"),
        }
        match select_pending(&list, 4, "proposer", None, &own, refuse) {
            Selection::Mismatch(reasons) => assert_eq!(reasons.len(), 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn selection_reports_an_own_signature_and_honours_the_pin() {
        let own = BTreeSet::from(["me".to_string()]);
        let list = vec![
            pending("h-a", 4, &["proposer", "me"], TopologyChangeOp::AddReplace),
            pending("h-b", 4, &["proposer"], TopologyChangeOp::AddReplace),
        ];
        assert!(matches!(
            select_pending(&list, 4, "proposer", None, &own, ok),
            Selection::AlreadySigned(p) if p.hash_hex == "h-a"
        ));
        assert!(matches!(
            select_pending(&list, 4, "proposer", Some("h-a"), &own, ok),
            Selection::AlreadySigned(p) if p.hash_hex == "h-a"
        ));
        // A pin narrows the choice to the pinned candidate.
        assert!(matches!(
            select_pending(&list, 4, "proposer", Some("h-b"), &own, ok),
            Selection::Match(p) if p.hash_hex == "h-b"
        ));
        assert!(matches!(
            select_pending(&list, 4, "proposer", Some("h-b"), &own, refuse),
            Selection::Mismatch(_)
        ));
        match select_pending(&list, 4, "proposer", Some("h-gone"), &own, ok) {
            Selection::PinMismatch { pinned, found } => {
                assert_eq!(pinned, "h-gone");
                assert_eq!(found, vec!["h-a".to_string(), "h-b".to_string()]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn repropose_needs_a_pin_the_grace_period_and_a_missing_hash() {
        let hashes = vec!["h-a".to_string()];
        assert!(!should_repropose(None, &[], 0, 10_000));
        assert!(
            !should_repropose(Some("h-a"), &hashes, 0, 10_000),
            "still in the store"
        );
        assert!(!should_repropose(
            Some("h-b"),
            &hashes,
            100,
            100 + REPROPOSE_GRACE_SECS - 1
        ));
        assert!(should_repropose(
            Some("h-b"),
            &hashes,
            100,
            100 + REPROPOSE_GRACE_SECS
        ));
    }

    #[test]
    fn acceptance_helpers_count_invitees_and_find_the_joiner() {
        let invitees = vec![party("node-b"), party("node-d")];
        let b = acceptance("node-b", 2, Some("1220bb"));
        let d = acceptance("node-d", 4, Some("1220dd"));
        assert!(!all_invitees_accepted(&invitees, std::slice::from_ref(&b)));
        assert!(all_invitees_accepted(&invitees, &[b.clone(), d.clone()]));
        assert_eq!(
            accepted_parties(&[d.clone(), b.clone(), b.clone()]),
            vec![party("node-b"), party("node-d")]
        );

        assert_eq!(
            joiner_fingerprint(std::slice::from_ref(&b), &participant(4)).expect("ok"),
            None
        );
        assert_eq!(
            joiner_fingerprint(&[b.clone(), d], &participant(4)).expect("ok"),
            Some("1220dd".to_string())
        );
        let bare = acceptance("node-d", 4, None);
        assert!(joiner_fingerprint(&[b, bare], &participant(4)).is_err());
    }

    #[test]
    fn cache_rows_take_each_members_own_claim_then_the_cached_key() {
        use canton_proto_rs::com::digitalasset::canton::protocol::v30::party_to_participant::{
            HostingParticipant, hosting_participant,
        };
        let record = add_party_proposal();
        let counted = vec![
            acceptance("node-b", 2, Some("1220bb")),
            acceptance("node-d", 4, Some("1220dd")),
        ];
        let claims = key_claims(&record, &counted);
        assert_eq!(
            claims.get(&participant(1).to_string()),
            Some(&KeyClaim {
                owner: Some("1220aa".into()),
                daml: Some("1220bb".into()),
            })
        );
        assert_eq!(
            claims
                .get(&participant(4).to_string())
                .and_then(|c| c.owner.clone()),
            Some("1220dd".into())
        );

        let host =
            |n: u8, permission: ParticipantPermission, onboarding: bool| HostingParticipant {
                participant_uid: participant(n).to_string(),
                permission: permission as i32,
                onboarding: onboarding.then_some(hosting_participant::Onboarding {}),
            };
        let p2p = PartyToParticipant {
            party: party("cbtc").to_string(),
            threshold: 2,
            participants: vec![
                host(1, ParticipantPermission::Confirmation, false),
                host(2, ParticipantPermission::Confirmation, false),
                host(4, ParticipantPermission::Confirmation, true),
            ],
            party_signing_keys: None,
        };
        // Participant 2 accepted without a Daml key; the cache keeps the one
        // it learned before.
        let existing = vec![DecPartyParticipantRow {
            dec_party_id: party("cbtc").to_string(),
            participant_uid: participant(2).to_string(),
            permission: "confirmation".into(),
            owner_key: Some("stale-owner".into()),
            signing_key: Some("daml-2".into()),
        }];
        let rows = cache_rows(&party("cbtc"), &p2p, &existing, &claims);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].owner_key.as_deref(), Some("1220aa"));
        assert_eq!(rows[0].permission, "confirmation");
        assert_eq!(
            rows[1].owner_key.as_deref(),
            Some("1220bb"),
            "own claim wins"
        );
        assert_eq!(
            rows[1].signing_key.as_deref(),
            Some("daml-2"),
            "cached key kept"
        );
        assert_eq!(rows[2].owner_key.as_deref(), Some("1220dd"));
        assert_eq!(rows[2].signing_key, None);
        assert_eq!(
            permission_label(ParticipantPermission::Submission as i32),
            "submission"
        );
        assert_eq!(permission_label(99), "unknown");
    }

    #[test]
    fn manifest_observers_are_every_other_node_party() {
        let record = add_party_proposal();
        assert_eq!(
            manifest_observers(&record, &party("node-b")),
            vec![party("node-a"), party("node-d")]
        );
        assert_eq!(
            manifest_observers(&record, &party("node-a")),
            vec![party("node-b"), party("node-d")]
        );
    }

    #[test]
    fn owner_fingerprint_prefers_the_dual_key_then_any_owner() {
        let mut dual = key(1);
        dual.usage = vec![
            SigningKeyUsage::Namespace as i32,
            SigningKeyUsage::Protocol as i32,
        ];
        let dual_fp = utils::compute_fingerprint(&dual);
        let mut legacy = key(2);
        legacy.usage = vec![SigningKeyUsage::Namespace as i32];
        let legacy_fp = utils::compute_fingerprint(&legacy);
        let vault = vec![
            keys::VaultKey {
                name: "cbtc-namespace".into(),
                key: legacy,
                fingerprint: legacy_fp.clone(),
            },
            keys::VaultKey {
                name: "cbtc-key".into(),
                key: dual,
                fingerprint: dual_fp.clone(),
            },
        ];
        let both = BTreeSet::from([legacy_fp.clone(), dual_fp.clone()]);
        assert_eq!(
            pick_owner_fingerprint(&both, &vault, "cbtc"),
            Some(dual_fp.clone())
        );
        let legacy_only = BTreeSet::from([legacy_fp.clone()]);
        assert_eq!(
            pick_owner_fingerprint(&legacy_only, &vault, "cbtc"),
            Some(legacy_fp)
        );
        // A dual key that does not own the DND is not advertised.
        let other = BTreeSet::from(["1220ff".to_string()]);
        assert_eq!(
            pick_owner_fingerprint(&other, &vault, "cbtc"),
            Some("1220ff".to_string())
        );
        assert_eq!(
            pick_owner_fingerprint(&BTreeSet::new(), &vault, "cbtc"),
            None
        );
    }
}
