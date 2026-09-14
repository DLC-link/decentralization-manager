//! The run engine (design sections 6, 10, 11): how a run starts, how an
//! invitee joins or declines, how a run is cancelled or retried, and the
//! contract every per-kind driver implements for the observer loop.
//!
//! There is no long-lived task per run. `workflow_runs` is the only state
//! machine; the observer re-reads a row every tick and hands it to the
//! [`KindDriver`] for its kind and role. The driver does one bounded piece of
//! work and returns. Cancel, dismiss, and retry are row operations.
//!
//! Until migration `000021` adds the on-ledger columns, a run's on-ledger
//! fields live in `config_json` under the [`RUN_META_KEY`] object
//! ([`RunMeta`]). The observer ignores rows without one, which is how it
//! tells an on-ledger run from a Noise-era row.

pub mod add_party;
pub mod change_threshold;
pub mod contracts;
pub mod dars;
pub mod kick;
pub mod onboarding;

use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::{Context, Result, bail};
use common::{
    api::{ContractDefinition, DarFile},
    canton_id::CantonId,
    types::{WorkflowKind, WorkflowProgress, WorkflowRole, WorkflowRun},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{
    consts,
    db::{
        rows::{ProposalDecision, ProposalDecisionEntry},
        schema::{Commitable, SchemaRead, SchemaWrite},
    },
    utils,
};

use super::{
    CoordinationClient, NodeIdentity, OnLedger,
    daml::codec::{
        AcceptArgs, DarPin, WorkflowAcceptanceRecord, WorkflowDeclineRecord, WorkflowOutcomeRecord,
        WorkflowProposalRecord,
    },
    now_micros, now_secs,
    proposals::{self, Acceptance, ActiveProposal, Decline, Outcome},
    registry::{self, PeerHealthSnapshot},
    topology,
    validation::HeadState,
};

/// The `config_json` key that carries [`RunMeta`].
///
/// TODO(migration 000021): move these fields to the `proposal_cid`,
/// `coordinator_party`, `coordinator_participant`, `member_variant`, and
/// `topology_hashes_json` columns and delete this key.
pub const RUN_META_KEY: &str = "onledger";

/// The last step of every step list.
pub const COMPLETE_STEP: &str = "Complete";

/// The coordinator step that waits for invitees.
pub const WAITING_FOR_ACCEPTANCES_STEP: &str = "WaitingForAcceptances";

// ---------------------------------------------------------------------------
// Start requests
// ---------------------------------------------------------------------------

/// What a start handler asks the engine to run. Mirrors the request DTOs in
/// `common::api` plus the `instance_name` the handler allocated.
#[derive(Clone, Debug)]
pub enum StartRequest {
    Onboarding {
        party_id_prefix: String,
        peer_ids: Vec<CantonId>,
        threshold: Option<i32>,
        instance_name: String,
    },
    AddParty {
        dec_party_id: CantonId,
        new_participant_id: CantonId,
        new_threshold: i32,
        previous_threshold: i32,
        instance_name: String,
    },
    Kick {
        dec_party_id: CantonId,
        participant_id: CantonId,
        new_threshold: i32,
        previous_threshold: i32,
        instance_name: String,
    },
    ChangeThreshold {
        dec_party_id: CantonId,
        new_threshold: i32,
        previous_threshold: i32,
        instance_name: String,
    },
    Contracts {
        dec_party_id: CantonId,
        participant_ids: Vec<CantonId>,
        participant_parties: Vec<CantonId>,
        operator_party: CantonId,
        contracts: Vec<ContractDefinition>,
        instance_name: String,
    },
    Dars {
        dar_files: Vec<DarFile>,
        peer_ids: Vec<CantonId>,
        instance_name: String,
    },
}

impl StartRequest {
    pub fn kind(&self) -> WorkflowKind {
        match self {
            Self::Onboarding { .. } => WorkflowKind::Onboarding,
            Self::AddParty { .. } => WorkflowKind::AddParty,
            Self::Kick { .. } => WorkflowKind::Kick,
            Self::ChangeThreshold { .. } => WorkflowKind::ChangeThreshold,
            Self::Contracts { .. } => WorkflowKind::Contracts,
            Self::Dars { .. } => WorkflowKind::Dars,
        }
    }

    pub fn instance_name(&self) -> &str {
        match self {
            Self::Onboarding { instance_name, .. }
            | Self::AddParty { instance_name, .. }
            | Self::Kick { instance_name, .. }
            | Self::ChangeThreshold { instance_name, .. }
            | Self::Contracts { instance_name, .. }
            | Self::Dars { instance_name, .. } => instance_name,
        }
    }

    /// The existing decentralized party the request acts on, if any.
    pub fn dec_party_id(&self) -> Option<&CantonId> {
        match self {
            Self::AddParty { dec_party_id, .. }
            | Self::Kick { dec_party_id, .. }
            | Self::ChangeThreshold { dec_party_id, .. }
            | Self::Contracts { dec_party_id, .. } => Some(dec_party_id),
            Self::Onboarding { .. } | Self::Dars { .. } => None,
        }
    }

    /// The threshold the request asks for (the new one for changes).
    pub fn threshold(&self) -> Option<i32> {
        match self {
            Self::Onboarding { threshold, .. } => *threshold,
            Self::AddParty { new_threshold, .. }
            | Self::Kick { new_threshold, .. }
            | Self::ChangeThreshold { new_threshold, .. } => Some(*new_threshold),
            Self::Contracts { .. } | Self::Dars { .. } => None,
        }
    }

    /// The threshold the request says the party has today; `0` means the
    /// client did not know.
    pub fn previous_threshold(&self) -> Option<i32> {
        match self {
            Self::AddParty {
                previous_threshold, ..
            }
            | Self::Kick {
                previous_threshold, ..
            }
            | Self::ChangeThreshold {
                previous_threshold, ..
            } => Some(*previous_threshold).filter(|t| *t > 0),
            _ => None,
        }
    }
}

/// A start refused before anything was written. The HTTP layer maps it to
/// `409 Conflict` with `message`; `peers` names the participants that
/// are not ready (design D3).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreflightRejected {
    pub message: String,
    pub peers: Vec<(CantonId, String)>,
}

impl PreflightRejected {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            peers: Vec::new(),
        }
    }

    pub fn peers(peers: Vec<(CantonId, String)>) -> Self {
        let list: Vec<String> = peers.iter().map(|(p, why)| format!("{p}: {why}")).collect();
        Self {
            message: format!(
                "cannot start: {} participant(s) are not ready for on-ledger coordination: {}",
                peers.len(),
                list.join("; ")
            ),
            peers,
        }
    }
}

impl std::fmt::Display for PreflightRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PreflightRejected {}

/// The proposer's own key material for the party (design D6).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProposerKeyMaterial {
    pub namespace_fingerprint: Option<String>,
    pub signing_public_key_hex: Option<String>,
    pub daml_key_fingerprint: Option<String>,
}

/// What a kind contributes to the `WorkflowProposal` before it is created.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProposalExtras {
    pub keys: ProposerKeyMaterial,
    pub dar_pins: Vec<DarPin>,
    pub package_names: Vec<String>,
}

/// What `start_run` produced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartedRun {
    pub instance_name: String,
    pub proposal_cid: String,
}

// ---------------------------------------------------------------------------
// Run metadata
// ---------------------------------------------------------------------------

/// Which member steps a peer row follows (design section 6).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum MemberVariant {
    /// The add-party participant being added.
    Joiner,
    /// Any other invitee.
    Member,
}

impl MemberVariant {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Joiner => "Joiner",
            Self::Member => "Member",
        }
    }
}

/// The on-ledger fields of a run row.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunMeta {
    pub proposal_cid: String,
    pub coordinator_party: CantonId,
    pub coordinator_participant: CantonId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_variant: Option<MemberVariant>,
    /// Topology transaction hashes this run pinned, by mapping (`dnd`,
    /// `p2p`, `clear`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub topology_hashes: BTreeMap<String, String>,
}

/// The on-ledger fields of a run, or `None` for a row the engine does not
/// drive.
pub fn read_run_meta(run: &WorkflowRun) -> Option<RunMeta> {
    let value: serde_json::Value = serde_json::from_str(&run.config_json).ok()?;
    serde_json::from_value(value.get(RUN_META_KEY)?.clone()).ok()
}

fn with_meta(config_json: &str, meta: &RunMeta) -> Result<String> {
    let mut value: serde_json::Value =
        serde_json::from_str(config_json).unwrap_or_else(|_| serde_json::json!({}));
    if !value.is_object() {
        value = serde_json::json!({});
    }
    value[RUN_META_KEY] = serde_json::to_value(meta).context("encode run meta")?;
    serde_json::to_string(&value).context("encode config_json")
}

/// Replace a run's on-ledger fields.
///
/// # Errors
/// Returns an error when the row is missing or the write fails.
pub async fn write_run_meta(db: &SqlitePool, instance_name: &str, meta: &RunMeta) -> Result<()> {
    let Some(mut run) = db.get_workflow_run(instance_name).await? else {
        bail!("workflow run {instance_name} not found");
    };
    run.config_json = with_meta(&run.config_json, meta)?;
    run.updated_at = now_secs();
    let mut tx = db.begin_transaction().await?;
    tx.upsert_workflow_run(&run).await?;
    Commitable::commit(tx).await
}

/// Pin a topology transaction hash on a run (`key` is `dnd`, `p2p`, or
/// `clear`).
///
/// # Errors
/// Returns an error when the row is missing, has no meta, or the write fails.
pub async fn pin_topology_hash(
    db: &SqlitePool,
    instance_name: &str,
    key: &str,
    hash_hex: &str,
) -> Result<()> {
    let Some(run) = db.get_workflow_run(instance_name).await? else {
        bail!("workflow run {instance_name} not found");
    };
    let Some(mut meta) = read_run_meta(&run) else {
        bail!("workflow run {instance_name} carries no on-ledger meta");
    };
    meta.topology_hashes
        .insert(key.to_string(), hash_hex.to_string());
    write_run_meta(db, instance_name, &meta).await
}

// ---------------------------------------------------------------------------
// Steps and quorum
// ---------------------------------------------------------------------------

/// The step list of a run by kind, role, and member variant (section 6).
pub fn steps_for(
    kind: WorkflowKind,
    role: WorkflowRole,
    variant: Option<MemberVariant>,
) -> &'static [&'static str] {
    macro_rules! pick {
        ($driver:ty) => {
            match role {
                WorkflowRole::Coordinator => <$driver>::coordinator_steps(),
                WorkflowRole::Peer => <$driver>::member_steps(variant),
            }
        };
    }
    match kind {
        WorkflowKind::Onboarding => pick!(onboarding::Onboarding),
        WorkflowKind::AddParty => pick!(add_party::AddParty),
        WorkflowKind::Kick => pick!(kick::Kick),
        WorkflowKind::ChangeThreshold => pick!(change_threshold::ChangeThreshold),
        WorkflowKind::Contracts => pick!(contracts::Contracts),
        WorkflowKind::Dars => pick!(dars::Dars),
    }
}

/// Owner signatures a kick or change-threshold mapping needs: the larger of
/// the threshold in force and the threshold proposed, because Canton
/// authorizes the change with the stored threshold and the result must be
/// reachable afterwards (section 6).
pub fn required_owner_signatures(previous_threshold: u32, new_threshold: u32) -> u32 {
    previous_threshold.max(new_threshold).max(1)
}

/// Counted acceptances the coordinator waits for before proposing:
/// [`required_owner_signatures`] minus its own signature.
pub fn acceptances_needed(previous_threshold: u32, new_threshold: u32) -> u32 {
    required_owner_signatures(previous_threshold, new_threshold).saturating_sub(1)
}

/// Whether a kind needs every invitee (onboarding, add-party, contracts,
/// dars) or an owner quorum (kick, change-threshold).
pub fn needs_every_invitee(kind: WorkflowKind) -> bool {
    !matches!(kind, WorkflowKind::Kick | WorkflowKind::ChangeThreshold)
}

// ---------------------------------------------------------------------------
// Row helpers for the drivers
// ---------------------------------------------------------------------------

/// Move a run to `step`, keeping its completed peers.
///
/// # Errors
/// Returns an error when `step` is not in the run's step list or the write
/// fails.
pub async fn advance_step(db: &SqlitePool, run: &WorkflowRun, step: &str) -> Result<()> {
    let variant = read_run_meta(run).and_then(|m| m.member_variant);
    let steps = steps_for(run.kind, run.role, variant);
    let Some(index) = steps.iter().position(|s| *s == step) else {
        bail!(
            "{} is not a step of a {} {} run ({steps:?})",
            step,
            run.role,
            run.kind
        );
    };
    let mut tx = db.begin_transaction().await?;
    tx.update_workflow_run_step(
        &run.instance_name,
        step,
        i64::try_from(index).unwrap_or(0),
        &run.completed_peers,
        now_secs(),
    )
    .await?;
    Commitable::commit(tx).await?;
    tracing::info!(instance = %run.instance_name, step, "run advanced");
    Ok(())
}

async fn set_status(
    db: &SqlitePool,
    instance_name: &str,
    status: WorkflowProgress,
    error: Option<&str>,
) -> Result<()> {
    let mut tx = db.begin_transaction().await?;
    tx.set_workflow_run_status(instance_name, status, error, now_secs())
        .await?;
    Commitable::commit(tx).await
}

/// Mark a run `Failed` with `error`. The UI shows the text unchanged.
///
/// # Errors
/// Returns an error when the write fails.
pub async fn fail_run(db: &SqlitePool, run: &WorkflowRun, error: &str) -> Result<()> {
    tracing::warn!(instance = %run.instance_name, error, "run failed");
    set_status(
        db,
        &run.instance_name,
        WorkflowProgress::Failed,
        Some(error),
    )
    .await
}

/// Mark a run `Cancelled` with `reason`.
///
/// # Errors
/// Returns an error when the write fails.
pub async fn cancel_run_row(db: &SqlitePool, run: &WorkflowRun, reason: &str) -> Result<()> {
    tracing::info!(instance = %run.instance_name, reason, "run cancelled");
    set_status(
        db,
        &run.instance_name,
        WorkflowProgress::Cancelled,
        Some(reason),
    )
    .await
}

/// Move a run to `Complete` and mark it `Completed`.
///
/// # Errors
/// Returns an error when the write fails.
pub async fn complete_run(db: &SqlitePool, run: &WorkflowRun) -> Result<()> {
    advance_step(db, run, COMPLETE_STEP).await?;
    set_status(db, &run.instance_name, WorkflowProgress::Completed, None).await
}

// ---------------------------------------------------------------------------
// The tick context and the driver contract
// ---------------------------------------------------------------------------

/// Everything the observer read this tick about proposals.
#[derive(Clone, Debug, Default)]
pub struct ProposalSnapshot {
    pub proposals: Vec<ActiveProposal>,
    pub acceptances: Vec<Acceptance>,
    pub declines: Vec<Decline>,
    pub outcomes: Vec<Outcome>,
    pub decisions: Vec<ProposalDecisionEntry>,
}

impl ProposalSnapshot {
    /// One ACS pass per template plus the local decisions table.
    ///
    /// # Errors
    /// Returns an error when a read fails.
    pub async fn read(client: &CoordinationClient, db: &SqlitePool) -> Result<Self> {
        Ok(Self {
            proposals: client.list_active::<WorkflowProposalRecord>().await?,
            acceptances: client.list_active::<WorkflowAcceptanceRecord>().await?,
            declines: client.list_active::<WorkflowDeclineRecord>().await?,
            outcomes: client.list_active::<WorkflowOutcomeRecord>().await?,
            decisions: db.get_all_proposal_decisions().await?,
        })
    }

    pub fn proposal(&self, cid: &str) -> Option<&ActiveProposal> {
        self.proposals.iter().find(|p| p.contract_id == cid)
    }

    /// Raw acceptances of one proposal; run `proposals::counted_acceptances`
    /// before trusting them.
    pub fn acceptances_for(&self, cid: &str) -> Vec<Acceptance> {
        self.acceptances
            .iter()
            .filter(|a| a.record.proposal == cid)
            .cloned()
            .collect()
    }

    pub fn declines_for(&self, cid: &str) -> Vec<&Decline> {
        self.declines
            .iter()
            .filter(|d| d.record.proposal == cid)
            .collect()
    }

    /// The outcome a proposer published for one of its runs.
    pub fn outcome_for(&self, proposer: &CantonId, run_id: &str) -> Option<&Outcome> {
        self.outcomes
            .iter()
            .find(|o| o.record.proposer == *proposer && o.record.run_id == run_id)
    }

    pub fn decision(&self, cid: &str) -> Option<&ProposalDecisionEntry> {
        self.decisions.iter().find(|d| d.proposal_cid == cid)
    }

    /// Proposals that invite `me` and that `me` did not create.
    pub fn for_me(&self, me: &CantonId) -> Vec<ActiveProposal> {
        self.proposals
            .iter()
            .filter(|p| p.record.proposer != *me && p.record.invitees.contains(me))
            .cloned()
            .collect()
    }

    pub fn active_cids(&self) -> HashSet<String> {
        self.proposals
            .iter()
            .map(|p| p.contract_id.clone())
            .collect()
    }
}

/// What one observer tick hands to a driver.
pub struct TickCtx<'a> {
    pub ol: &'a OnLedger,
    pub client: &'a CoordinationClient,
    pub identity: &'a NodeIdentity,
    pub sync_id: String,
    pub participant_id: CantonId,
    pub proposals: &'a ProposalSnapshot,
    pub registry: &'a PeerHealthSnapshot,
    /// Micros since the epoch at the start of the tick.
    pub now_micros: i64,
}

impl TickCtx<'_> {
    pub fn db(&self) -> &SqlitePool {
        self.ol.db()
    }

    /// Whether a proposal has passed its `expiresAt`.
    pub fn is_expired(&self, proposal: &ActiveProposal) -> bool {
        proposal.record.expires_at <= self.now_micros
    }
}

/// One workflow kind's step machine, both sides. Every method is static so
/// the observer dispatches without an instance.
///
/// A tick does one bounded piece of work for the run's current step and
/// returns. It re-reads the row and the proposal before every ledger or
/// topology write (design D5 step 5). It reports validation failures with
/// [`fail_run`] and lets transient errors surface as `Err`, which the
/// observer logs and retries next tick.
#[allow(async_fn_in_trait)]
pub trait KindDriver {
    fn kind() -> WorkflowKind;

    /// `current_step` values of the coordinator row, in order.
    fn coordinator_steps() -> &'static [&'static str];

    /// `current_step` values of a peer row, in order, by variant.
    fn member_steps(variant: Option<MemberVariant>) -> &'static [&'static str];

    /// Kind-specific start refusals (section 6 preflight). Return a
    /// [`PreflightRejected`] through `anyhow` for a 409.
    async fn preflight(ol: &OnLedger, req: &StartRequest) -> Result<()> {
        let _ = (ol, req);
        Ok(())
    }

    /// The proposer's key material, DAR pins, and package names for the
    /// `WorkflowProposal`. Runs before the proposal is created.
    async fn prepare(ol: &OnLedger, req: &StartRequest) -> Result<ProposalExtras> {
        let _ = (ol, req);
        Ok(ProposalExtras::default())
    }

    async fn tick_coordinator(ctx: &TickCtx<'_>, run: &WorkflowRun, meta: &RunMeta) -> Result<()>;

    async fn tick_member(ctx: &TickCtx<'_>, run: &WorkflowRun, meta: &RunMeta) -> Result<()>;
}

/// A stub tick: log at debug level and return, so a run that reaches an
/// unimplemented step stays `InProgress` without noise in the logs.
pub(crate) fn not_implemented(run: &WorkflowRun) {
    tracing::debug!(
        instance = %run.instance_name,
        kind = %run.kind,
        role = %run.role,
        step = %run.current_step,
        "step machine not implemented yet"
    );
}

/// What [`drive`] did with a row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Driven {
    /// Not an on-ledger row.
    Skipped,
    /// The generic reconciliation ended the run this tick.
    Stopped,
    /// The kind driver ran.
    Ticked,
}

fn describe_declines(declines: &[&Decline]) -> String {
    declines
        .iter()
        .map(|d| {
            format!(
                "{} declined the invitation: {}",
                d.record.decliner, d.record.reason
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Generic transitions that hold for every kind (design D10) before the
/// kind driver runs: the proposal vanished, expired, or was declined.
async fn reconcile(ctx: &TickCtx<'_>, run: &WorkflowRun, meta: &RunMeta) -> Result<bool> {
    let db = ctx.db();
    let proposal = ctx.proposals.proposal(&meta.proposal_cid);
    match run.role {
        WorkflowRole::Coordinator => {
            let Some(proposal) = proposal else {
                // A row is written only after `submit_and_wait` returned, so a
                // missing proposal is a real loss, not a race, once the row
                // is older than a couple of polls.
                let grace = i64::try_from(consts::observer_poll_secs()).unwrap_or(3) * 2;
                if now_secs() - run.created_at < grace {
                    return Ok(false);
                }
                fail_run(
                    db,
                    run,
                    &format!(
                        "WorkflowProposal {} is no longer active on the ledger",
                        meta.proposal_cid
                    ),
                )
                .await?;
                return Ok(false);
            };
            if ctx.is_expired(proposal) {
                let msg = "the WorkflowProposal expired before the run finished".to_string();
                fail_run(db, run, &msg).await?;
                finish_best_effort(ctx.client, &meta.proposal_cid, Some(msg)).await;
                return Ok(false);
            }
            let declines = ctx.proposals.declines_for(&meta.proposal_cid);
            if declines.is_empty() {
                return Ok(true);
            }
            let fail = if needs_every_invitee(run.kind) {
                true
            } else {
                let decliners: BTreeSet<&CantonId> =
                    declines.iter().map(|d| &d.record.decliner).collect();
                let remaining = proposal
                    .record
                    .invitees
                    .iter()
                    .filter(|i| !decliners.contains(i))
                    .count();
                let previous =
                    u32::try_from(proposal.record.previous_threshold.unwrap_or(0)).unwrap_or(0);
                let new = u32::try_from(proposal.record.threshold.unwrap_or(0)).unwrap_or(0);
                let needed = acceptances_needed(previous, new);
                u32::try_from(remaining).unwrap_or(u32::MAX) < needed
            };
            if fail {
                let msg = describe_declines(&declines);
                fail_run(db, run, &msg).await?;
                finish_best_effort(ctx.client, &meta.proposal_cid, Some(msg)).await;
                return Ok(false);
            }
            Ok(true)
        }
        WorkflowRole::Peer => {
            let Some(proposal) = proposal else {
                let run_id = run.coordinator_instance.as_deref().unwrap_or_default();
                match ctx.proposals.outcome_for(&meta.coordinator_party, run_id) {
                    Some(o) if o.record.succeeded => {
                        complete_run(db, run).await?;
                    }
                    Some(o) => {
                        let why = o
                            .record
                            .error
                            .clone()
                            .unwrap_or_else(|| "the proposer reported a failure".into());
                        cancel_run_row(db, run, &format!("cancelled by the proposer: {why}"))
                            .await?;
                    }
                    None => {
                        cancel_run_row(db, run, "the proposer withdrew the WorkflowProposal")
                            .await?;
                    }
                }
                return Ok(false);
            };
            if ctx.is_expired(proposal) {
                fail_run(
                    db,
                    run,
                    "the WorkflowProposal expired before the run finished",
                )
                .await?;
                return Ok(false);
            }
            Ok(true)
        }
    }
}

async fn finish_best_effort(client: &CoordinationClient, cid: &str, error: Option<String>) {
    if let Err(e) = proposals::finish(client, cid, false, error).await {
        tracing::warn!(proposal = %cid, error = %e, "WorkflowProposal_Finish failed; retry next tick");
    }
}

/// Drive one in-progress row for one tick: generic reconciliation, then the
/// kind driver for the row's role.
///
/// # Errors
/// Returns an error when a read or write fails; the observer logs it and
/// tries again next tick.
pub async fn drive(ctx: &TickCtx<'_>, run: &WorkflowRun) -> Result<Driven> {
    let Some(meta) = read_run_meta(run) else {
        return Ok(Driven::Skipped);
    };
    if !reconcile(ctx, run, &meta).await? {
        return Ok(Driven::Stopped);
    }
    macro_rules! tick {
        ($driver:ty) => {
            match run.role {
                WorkflowRole::Coordinator => <$driver>::tick_coordinator(ctx, run, &meta).await?,
                WorkflowRole::Peer => <$driver>::tick_member(ctx, run, &meta).await?,
            }
        };
    }
    match run.kind {
        WorkflowKind::Onboarding => tick!(onboarding::Onboarding),
        WorkflowKind::AddParty => tick!(add_party::AddParty),
        WorkflowKind::Kick => tick!(kick::Kick),
        WorkflowKind::ChangeThreshold => tick!(change_threshold::ChangeThreshold),
        WorkflowKind::Contracts => tick!(contracts::Contracts),
        WorkflowKind::Dars => tick!(dars::Dars),
    }
    Ok(Driven::Ticked)
}

// ---------------------------------------------------------------------------
// start_run
// ---------------------------------------------------------------------------

/// The participants and head state a request resolves to.
#[derive(Clone, Debug, Default, PartialEq)]
struct ResolvedTargets {
    /// Every participant of the resulting workflow, this node included.
    participants: Vec<CantonId>,
    /// The participants to invite: `participants` minus this node.
    invitee_participants: Vec<CantonId>,
    dec_party_id: Option<String>,
    prefix: Option<String>,
    threshold: Option<i64>,
    previous_threshold: Option<i64>,
    dnd_base_serial: Option<i64>,
    p2p_base_serial: Option<i64>,
    head: HeadState,
}

fn sorted_unique(ids: impl IntoIterator<Item = CantonId>) -> Vec<CantonId> {
    let set: BTreeSet<CantonId> = ids.into_iter().collect();
    set.into_iter().collect()
}

async fn resolve_targets(
    ol: &OnLedger,
    identity: &NodeIdentity,
    sync_id: &str,
    req: &StartRequest,
) -> Result<ResolvedTargets> {
    let me = identity.participant_id.clone();
    let mut out = ResolvedTargets::default();

    if let Some(party) = req.dec_party_id() {
        let namespace = party.namespace.to_hex();
        let dnd = topology::read_accepted_dnd(ol.config(), sync_id, &namespace).await?;
        let p2p = topology::read_accepted_p2p(ol.config(), sync_id, party).await?;
        let Some(p2p) = p2p else {
            bail!("{party} has no PartyToParticipant mapping in the synchronizer store");
        };
        let hosts: Vec<CantonId> = p2p
            .mapping
            .participants
            .iter()
            .filter_map(|h| CantonId::parse(&h.participant_uid).ok())
            .collect();
        if !hosts.contains(&me) {
            bail!("this participant ({me}) does not host {party}");
        }
        out.dec_party_id = Some(party.to_string());
        out.prefix = Some(party.prefix.clone());
        out.p2p_base_serial = Some(i64::from(p2p.serial));
        out.dnd_base_serial = dnd.as_ref().map(|d| i64::from(d.serial));
        out.previous_threshold = dnd
            .as_ref()
            .map(|d| i64::from(d.mapping.threshold))
            .or_else(|| req.previous_threshold().map(i64::from));
        out.head = HeadState {
            dnd: dnd.map(|d| d.mapping),
            p2p: Some(p2p.mapping.clone()),
        };
        out.participants = match req {
            StartRequest::AddParty {
                new_participant_id, ..
            } => sorted_unique(hosts.into_iter().chain([new_participant_id.clone()])),
            StartRequest::Kick { participant_id, .. } => {
                if !hosts.contains(participant_id) {
                    bail!("{participant_id} does not host {party}");
                }
                sorted_unique(hosts.into_iter().filter(|h| h != participant_id))
            }
            StartRequest::Contracts {
                participant_ids, ..
            } => sorted_unique(participant_ids.iter().cloned().chain([me.clone()])),
            _ => sorted_unique(hosts),
        };
    } else {
        match req {
            StartRequest::Onboarding {
                party_id_prefix,
                peer_ids,
                ..
            } => {
                out.prefix = Some(party_id_prefix.clone());
                out.participants = sorted_unique(peer_ids.iter().cloned().chain([me.clone()]));
            }
            StartRequest::Dars { peer_ids, .. } => {
                out.participants = sorted_unique(peer_ids.iter().cloned().chain([me.clone()]));
            }
            _ => unreachable!("kinds without a party are Onboarding and Dars"),
        }
    }

    out.threshold = match req {
        StartRequest::Onboarding { threshold, .. } => {
            Some(i64::from(threshold.unwrap_or_else(|| {
                i32::try_from(out.participants.len().div_ceil(2).max(1)).unwrap_or(1)
            })))
        }
        _ => req.threshold().map(i64::from),
    };

    out.invitee_participants = out
        .participants
        .iter()
        .filter(|p| **p != me)
        .cloned()
        .collect();
    if out.invitee_participants.is_empty() {
        bail!("a workflow needs at least one other participant");
    }
    Ok(out)
}

/// Section 6 threshold gates shared by the topology kinds.
fn check_thresholds(req: &StartRequest, targets: &ResolvedTargets) -> Result<()> {
    let owners = match req.kind() {
        WorkflowKind::Onboarding => targets.participants.len(),
        WorkflowKind::AddParty | WorkflowKind::Kick | WorkflowKind::ChangeThreshold => {
            let head = targets
                .head
                .dnd
                .as_ref()
                .map(|d| d.owners.len())
                .unwrap_or(targets.participants.len());
            match req {
                StartRequest::AddParty { .. } => head + 1,
                StartRequest::Kick { .. } => head.saturating_sub(1),
                _ => head,
            }
        }
        _ => return Ok(()),
    };
    let owners = i64::try_from(owners).unwrap_or(i64::MAX);
    let Some(threshold) = targets.threshold else {
        bail!("{} needs a threshold", req.kind());
    };
    if threshold < 1 || threshold > owners {
        return Err(PreflightRejected::new(format!(
            "threshold {threshold} is outside 1..={owners} for the resulting owner set"
        ))
        .into());
    }
    if let StartRequest::Kick { .. } = req
        && let Some(previous) = targets.previous_threshold
        && previous > owners
    {
        return Err(PreflightRejected::new(format!(
            "the current threshold {previous} cannot be met by the {owners} remaining owner(s); \
             lower the threshold before the kick"
        ))
        .into());
    }
    Ok(())
}

/// Map invitee participants to their node parties through the peers table.
async fn invitee_parties(db: &SqlitePool, participants: &[CantonId]) -> Result<Vec<CantonId>> {
    let peers = db.get_all_peers().await?;
    let mut parties = Vec::with_capacity(participants.len());
    let mut missing = Vec::new();
    for p in participants {
        let party = peers
            .iter()
            .find(|peer| peer.participant_id == *p)
            .and_then(|peer| peer.party.as_deref())
            .and_then(|s| CantonId::parse(s).ok());
        match party {
            Some(party) => parties.push(party),
            None => missing.push((p.clone(), "no node party in the peers table".to_string())),
        }
    }
    if !missing.is_empty() {
        return Err(PreflightRejected::peers(missing).into());
    }
    Ok(parties)
}

fn describe(req: &StartRequest, targets: &ResolvedTargets) -> String {
    match req {
        StartRequest::Onboarding {
            party_id_prefix, ..
        } => format!(
            "Create decentralized party {party_id_prefix} with {} members (threshold {})",
            targets.participants.len(),
            targets.threshold.unwrap_or(0)
        ),
        StartRequest::AddParty {
            dec_party_id,
            new_participant_id,
            new_threshold,
            ..
        } => format!("Add {new_participant_id} to {dec_party_id} (threshold {new_threshold})"),
        StartRequest::Kick {
            dec_party_id,
            participant_id,
            new_threshold,
            ..
        } => format!("Remove {participant_id} from {dec_party_id} (threshold {new_threshold})"),
        StartRequest::ChangeThreshold {
            dec_party_id,
            new_threshold,
            ..
        } => format!("Change the threshold of {dec_party_id} to {new_threshold}"),
        StartRequest::Contracts {
            dec_party_id,
            contracts,
            ..
        } => format!("Create {} contract(s) for {dec_party_id}", contracts.len()),
        StartRequest::Dars { dar_files, .. } => {
            format!("Distribute {} DAR file(s)", dar_files.len())
        }
    }
}

/// The `config_json` of a coordinator row: the fields the API layer lifts
/// for the run card, plus [`RunMeta`].
fn coordinator_config_json(
    req: &StartRequest,
    targets: &ResolvedTargets,
    meta: &RunMeta,
) -> Result<String> {
    let participants = &targets.participants;
    let base = match req {
        StartRequest::Onboarding {
            party_id_prefix,
            instance_name,
            ..
        } => serde_json::json!({
            "party_id_prefix": party_id_prefix,
            "instance_name": instance_name,
            "threshold": targets.threshold,
            "participants": participants,
        }),
        StartRequest::AddParty {
            dec_party_id,
            new_participant_id,
            new_threshold,
            instance_name,
            ..
        } => serde_json::json!({
            "decentralized_party_id": dec_party_id,
            "new_participant_id": new_participant_id,
            "new_threshold": new_threshold,
            "previous_threshold": targets.previous_threshold,
            "instance_name": instance_name,
            "participants": participants,
        }),
        StartRequest::Kick {
            dec_party_id,
            participant_id,
            new_threshold,
            instance_name,
            ..
        } => serde_json::json!({
            "decentralized_party_id": dec_party_id,
            "participant_id": participant_id,
            "new_threshold": new_threshold,
            "previous_threshold": targets.previous_threshold,
            "instance_name": instance_name,
            "participants": participants,
        }),
        StartRequest::ChangeThreshold {
            dec_party_id,
            new_threshold,
            instance_name,
            ..
        } => serde_json::json!({
            "decentralized_party_id": dec_party_id,
            "new_threshold": new_threshold,
            "previous_threshold": targets.previous_threshold,
            "instance_name": instance_name,
            "participants": participants,
        }),
        StartRequest::Contracts {
            dec_party_id,
            participant_ids,
            participant_parties,
            operator_party,
            contracts,
            instance_name,
        } => serde_json::json!({
            "decentralized_party_id": dec_party_id,
            "participant_ids": participant_ids,
            "participant_parties": participant_parties,
            "operator_party": operator_party,
            "contracts": contracts,
            "instance_name": instance_name,
            "participants": participants,
        }),
        // DAR bytes stay out of the row: the handler uploads them locally
        // and the pins on the proposal identify them.
        StartRequest::Dars {
            dar_files,
            peer_ids,
            instance_name,
        } => serde_json::json!({
            "dar_filenames": dar_files.iter().map(|d| d.filename.clone()).collect::<Vec<_>>(),
            "peer_ids": peer_ids,
            "instance_name": instance_name,
            "participants": participants,
        }),
    };
    with_meta(&base.to_string(), meta)
}

async fn dispatch_preflight(ol: &OnLedger, req: &StartRequest) -> Result<()> {
    match req.kind() {
        WorkflowKind::Onboarding => onboarding::Onboarding::preflight(ol, req).await,
        WorkflowKind::AddParty => add_party::AddParty::preflight(ol, req).await,
        WorkflowKind::Kick => kick::Kick::preflight(ol, req).await,
        WorkflowKind::ChangeThreshold => {
            change_threshold::ChangeThreshold::preflight(ol, req).await
        }
        WorkflowKind::Contracts => contracts::Contracts::preflight(ol, req).await,
        WorkflowKind::Dars => dars::Dars::preflight(ol, req).await,
    }
}

async fn dispatch_prepare(ol: &OnLedger, req: &StartRequest) -> Result<ProposalExtras> {
    match req.kind() {
        WorkflowKind::Onboarding => onboarding::Onboarding::prepare(ol, req).await,
        WorkflowKind::AddParty => add_party::AddParty::prepare(ol, req).await,
        WorkflowKind::Kick => kick::Kick::prepare(ol, req).await,
        WorkflowKind::ChangeThreshold => change_threshold::ChangeThreshold::prepare(ol, req).await,
        WorkflowKind::Contracts => contracts::Contracts::prepare(ol, req).await,
        WorkflowKind::Dars => dars::Dars::prepare(ol, req).await,
    }
}

/// Start a run as the coordinator (design D6):
///
/// 1. resolve the participants from the request and the head state;
/// 2. preflight: every invitee has a node party, has vetted the package,
///    has a registry entry at this coordination version
///    (`registry::preflight_unready_peers`), thresholds are in range, and
///    the kind's own gates pass;
/// 3. the kind prepares the proposer's key material and pins;
/// 4. create the `WorkflowProposal` with the base serials read in step 1;
/// 5. persist the coordinator row at its first step, with `RunMeta`, only
///    after `submit_and_wait` returned.
///
/// # Errors
/// Returns [`PreflightRejected`] (through `anyhow`, downcast for a 409) when
/// a gate fails, and any other error when a read or write fails.
pub async fn start_run(ol: &OnLedger, req: StartRequest) -> Result<StartedRun> {
    let identity = ol.require_identity().await?;
    let client = ol.client().await?;
    let sync_id = utils::get_synchronizer_id(ol.config()).await?;
    let instance_name = req.instance_name().to_string();

    if ol.db().get_workflow_run(&instance_name).await?.is_some() {
        return Err(PreflightRejected::new(format!(
            "a workflow run named {instance_name} already exists"
        ))
        .into());
    }

    let targets = resolve_targets(ol, &identity, &sync_id, &req).await?;
    check_thresholds(&req, &targets)?;

    let unready = registry::preflight_unready_peers(
        &ol.registry_snapshot().await,
        &targets.invitee_participants,
    );
    if !unready.is_empty() {
        return Err(PreflightRejected::peers(unready).into());
    }
    let invitees = invitee_parties(ol.db(), &targets.invitee_participants).await?;
    dispatch_preflight(ol, &req).await?;

    let extras = dispatch_prepare(ol, &req).await?;

    let (created_at, expires_at) = proposals::proposal_lifetime(now_micros());
    let record = WorkflowProposalRecord {
        proposer: identity.node_party.clone(),
        proposer_participant: identity.participant_id.to_string(),
        proposer_namespace_fingerprint: extras.keys.namespace_fingerprint,
        proposer_signing_public_key_hex: extras.keys.signing_public_key_hex,
        proposer_daml_key_fingerprint: extras.keys.daml_key_fingerprint,
        run_id: instance_name.clone(),
        kind: req.kind(),
        invitees,
        participants: targets
            .participants
            .iter()
            .map(CantonId::to_string)
            .collect(),
        dec_party_id: targets.dec_party_id.clone(),
        prefix: targets.prefix.clone(),
        threshold: targets.threshold,
        previous_threshold: targets.previous_threshold,
        dnd_base_serial: targets.dnd_base_serial,
        p2p_base_serial: targets.p2p_base_serial,
        new_participant: match &req {
            StartRequest::AddParty {
                new_participant_id, ..
            } => Some(new_participant_id.to_string()),
            _ => None,
        },
        kicked_participant: match &req {
            StartRequest::Kick { participant_id, .. } => Some(participant_id.to_string()),
            _ => None,
        },
        dar_pins: extras.dar_pins,
        package_names: extras.package_names,
        description: describe(&req, &targets),
        created_at,
        expires_at,
    };
    let proposal_cid = proposals::create_proposal(&client, &record).await?;

    let meta = RunMeta {
        proposal_cid: proposal_cid.clone(),
        coordinator_party: identity.node_party.clone(),
        coordinator_participant: identity.participant_id.clone(),
        member_variant: None,
        topology_hashes: BTreeMap::new(),
    };
    let steps = steps_for(req.kind(), WorkflowRole::Coordinator, None);
    let now = now_secs();
    let run = WorkflowRun {
        instance_name: instance_name.clone(),
        kind: req.kind(),
        role: WorkflowRole::Coordinator,
        status: WorkflowProgress::InProgress,
        current_step: steps[0].to_string(),
        step_index: 0,
        step_total: i64::try_from(steps.len()).unwrap_or(0),
        config_json: coordinator_config_json(&req, &targets, &meta)?,
        // TODO(migration 000021): `coordinator_participant`. Until then the
        // old column carries this node's participant id.
        coordinator_pubkey: Some(identity.participant_id.to_string()),
        coordinator_instance: None,
        coordinator_name: None,
        expected_peers: targets.invitee_participants.clone(),
        completed_peers: Vec::new(),
        connected_peers: Vec::new(),
        acs_progress: None,
        dec_party_id: targets
            .dec_party_id
            .as_deref()
            .and_then(|s| CantonId::parse(s).ok()),
        prefix: None,
        participants: Vec::new(),
        previous_threshold: None,
        new_threshold: None,
        kicked_participant: None,
        added_participant: None,
        package_names: Vec::new(),
        dar_filenames: Vec::new(),
        error: None,
        dismissed: false,
        created_at: now,
        updated_at: now,
    };
    let mut tx = ol.db().begin_transaction().await?;
    tx.upsert_workflow_run(&run).await?;
    Commitable::commit(tx).await?;
    tracing::info!(instance = %instance_name, proposal = %proposal_cid, kind = %req.kind(), "run started");
    Ok(StartedRun {
        instance_name,
        proposal_cid,
    })
}

// ---------------------------------------------------------------------------
// Invitee actions
// ---------------------------------------------------------------------------

/// The peer `instance_name`: `peer-{kind}-{coordinator_participant_short}-{run_id}`.
pub fn peer_instance_name(
    kind: WorkflowKind,
    coordinator_participant: &str,
    run_id: &str,
) -> String {
    let short: String = coordinator_participant.chars().take(16).collect();
    format!("peer-{}-{short}-{run_id}", kind.as_str().to_lowercase())
}

/// The variant of this node in a proposal: the add-party joiner, or a
/// member.
pub fn member_variant_for(
    proposal: &WorkflowProposalRecord,
    me: &CantonId,
) -> Option<MemberVariant> {
    match proposal.kind {
        WorkflowKind::AddParty => Some(
            if proposal.new_participant.as_deref() == Some(me.to_string().as_str()) {
                MemberVariant::Joiner
            } else {
                MemberVariant::Member
            },
        ),
        _ => None,
    }
}

/// The `config_json` of a peer row: the invitation fields the API layer
/// lifts for the run card, plus [`RunMeta`].
fn peer_config_json(proposal: &WorkflowProposalRecord, meta: &RunMeta) -> Result<String> {
    let base = serde_json::json!({
        "prefix": proposal.prefix,
        "participants": proposal.participants,
        "dar_filenames": proposal.dar_pins.iter().map(|p| p.filename.clone()).collect::<Vec<_>>(),
        "dar_hashes": proposal.dar_pins.iter().map(|p| p.sha256_hex.clone()).collect::<Vec<_>>(),
        "participant_id": proposal.kicked_participant,
        "new_participant_id": proposal.new_participant,
        "new_threshold": proposal.threshold,
        "previous_threshold": proposal.previous_threshold,
        "package_names": proposal.package_names,
    });
    with_meta(&base.to_string(), meta)
}

/// What `accept_invitation` produced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedInvitation {
    pub instance_name: String,
    pub member_variant: Option<MemberVariant>,
}

async fn find_invitation(ol: &OnLedger, proposal_cid: &str) -> Result<ActiveProposal> {
    let client = ol.client().await?;
    let Some(proposal) = proposals::read_proposal(&client, proposal_cid).await? else {
        bail!("WorkflowProposal {proposal_cid} is not active or not visible to this node");
    };
    let me = client.node_party();
    if proposal.record.proposer == *me {
        bail!("WorkflowProposal {proposal_cid} was created by this node");
    }
    if !proposal.record.invitees.contains(me) {
        bail!("WorkflowProposal {proposal_cid} does not invite this node");
    }
    Ok(proposal)
}

/// Accept an invitation (design D6): record the decision, create the peer
/// run row at its first step, and drop the invitation card. The observer
/// generates keys, exercises `WorkflowProposal_Accept`, and drives the run.
///
/// Idempotent: a second accept of the same proposal returns the same row.
///
/// # Errors
/// Returns an error when the proposal is not an active invitation for this
/// node, when it has expired, when it was declined or dismissed before, or
/// when a write fails.
pub async fn accept_invitation(ol: &OnLedger, proposal_cid: &str) -> Result<AcceptedInvitation> {
    let identity = ol.require_identity().await?;
    let proposal = find_invitation(ol, proposal_cid).await?;
    if proposal.record.expires_at <= now_micros() {
        bail!("WorkflowProposal {proposal_cid} has expired");
    }
    let record = &proposal.record;
    let coordinator_participant = CantonId::parse(&record.proposer_participant)
        .with_context(|| format!("proposerParticipant `{}`", record.proposer_participant))?;
    let member_variant = member_variant_for(record, &identity.participant_id);
    let instance_name =
        peer_instance_name(record.kind, &record.proposer_participant, &record.run_id);

    let db = ol.db();
    let mut tx = db.begin_transaction().await?;
    let fresh = tx
        .insert_proposal_decision(&ProposalDecisionEntry {
            proposal_cid: proposal_cid.to_string(),
            decision: ProposalDecision::Accepted,
            decided_at: now_secs(),
            pinned_hashes: Vec::new(),
        })
        .await?;
    if !fresh {
        Commitable::commit(tx).await?;
        let existing = db.get_proposal_decision(proposal_cid).await?;
        match existing.map(|d| d.decision) {
            Some(ProposalDecision::Accepted) => {
                return Ok(AcceptedInvitation {
                    instance_name,
                    member_variant,
                });
            }
            Some(other) => bail!("WorkflowProposal {proposal_cid} was already {other}"),
            None => bail!("proposal_decisions row for {proposal_cid} vanished"),
        }
    }

    let meta = RunMeta {
        proposal_cid: proposal_cid.to_string(),
        coordinator_party: record.proposer.clone(),
        coordinator_participant: coordinator_participant.clone(),
        member_variant,
        topology_hashes: BTreeMap::new(),
    };
    let steps = steps_for(record.kind, WorkflowRole::Peer, member_variant);
    let now = now_secs();
    let run = WorkflowRun {
        instance_name: instance_name.clone(),
        kind: record.kind,
        role: WorkflowRole::Peer,
        status: WorkflowProgress::InProgress,
        current_step: steps[0].to_string(),
        step_index: 0,
        step_total: i64::try_from(steps.len()).unwrap_or(0),
        config_json: peer_config_json(record, &meta)?,
        // TODO(migration 000021): `coordinator_participant`.
        coordinator_pubkey: Some(record.proposer_participant.clone()),
        coordinator_instance: Some(record.run_id.clone()),
        coordinator_name: None,
        expected_peers: record
            .participants
            .iter()
            .filter_map(|p| CantonId::parse(p).ok())
            .collect(),
        completed_peers: Vec::new(),
        connected_peers: Vec::new(),
        acs_progress: None,
        dec_party_id: record
            .dec_party_id
            .as_deref()
            .and_then(|s| CantonId::parse(s).ok()),
        prefix: None,
        participants: Vec::new(),
        previous_threshold: None,
        new_threshold: None,
        kicked_participant: None,
        added_participant: None,
        package_names: Vec::new(),
        dar_filenames: Vec::new(),
        error: None,
        dismissed: false,
        created_at: now,
        updated_at: now,
    };
    tx.upsert_workflow_run(&run).await?;
    tx.delete_pending_invitation(proposal_cid).await?;
    Commitable::commit(tx).await?;
    ol.remove_pending_invitation(proposal_cid).await;
    tracing::info!(instance = %instance_name, proposal = %proposal_cid, "invitation accepted");
    Ok(AcceptedInvitation {
        instance_name,
        member_variant,
    })
}

/// Decline an invitation (design D10): record the decision, exercise
/// `WorkflowProposal_Decline`, and drop the invitation card.
///
/// # Errors
/// Returns an error when the proposal is not an active invitation for this
/// node, when it was accepted before, or when a write fails.
pub async fn decline_invitation(ol: &OnLedger, proposal_cid: &str, reason: &str) -> Result<String> {
    let client = ol.client().await?;
    find_invitation(ol, proposal_cid).await?;
    let db = ol.db();
    let mut tx = db.begin_transaction().await?;
    let fresh = tx
        .insert_proposal_decision(&ProposalDecisionEntry {
            proposal_cid: proposal_cid.to_string(),
            decision: ProposalDecision::Declined,
            decided_at: now_secs(),
            pinned_hashes: Vec::new(),
        })
        .await?;
    if !fresh {
        let existing = db.get_proposal_decision(proposal_cid).await?;
        match existing.map(|d| d.decision) {
            Some(ProposalDecision::Accepted) => {
                bail!(
                    "WorkflowProposal {proposal_cid} was already accepted; cancel the run instead"
                )
            }
            // A dismissed card can still be declined on the ledger.
            Some(ProposalDecision::Dismissed) => {
                tx.update_proposal_decision(proposal_cid, ProposalDecision::Declined, now_secs())
                    .await?;
            }
            Some(ProposalDecision::Declined) | None => {}
        }
    }
    tx.delete_pending_invitation(proposal_cid).await?;
    Commitable::commit(tx).await?;
    ol.remove_pending_invitation(proposal_cid).await;
    let cid = proposals::decline(&client, proposal_cid, reason).await?;
    tracing::info!(proposal = %proposal_cid, decline = %cid, "invitation declined");
    Ok(cid)
}

// ---------------------------------------------------------------------------
// Row operations
// ---------------------------------------------------------------------------

/// Cancel a run (design D10). The row is marked `Cancelled` first so a
/// restart cannot resume it; a coordinator then exercises
/// `WorkflowProposal_Cancel` so invitees stop co-signing. A topology
/// proposal that already reached its threshold still becomes effective.
///
/// # Errors
/// Returns an error when the run is missing or not in progress, or when the
/// row write fails. A failed `Cancel` exercise is logged, not returned: the
/// proposal expires on its own.
pub async fn cancel_run(ol: &OnLedger, instance_name: &str) -> Result<()> {
    let db = ol.db();
    let Some(run) = db.get_workflow_run(instance_name).await? else {
        bail!("workflow run {instance_name} not found");
    };
    if run.status != WorkflowProgress::InProgress {
        bail!(
            "workflow run {instance_name} is {}, not in progress",
            run.status
        );
    }
    cancel_run_row(db, &run, "cancelled by the operator").await?;
    if run.role == WorkflowRole::Coordinator
        && let Some(meta) = read_run_meta(&run)
    {
        match ol.client().await {
            Ok(client) => {
                if let Err(e) = proposals::cancel(&client, &meta.proposal_cid).await {
                    tracing::warn!(
                        proposal = %meta.proposal_cid,
                        error = %e,
                        "WorkflowProposal_Cancel failed; the proposal expires on its own"
                    );
                }
            }
            Err(e) => tracing::warn!(error = %e, "no client to cancel the WorkflowProposal"),
        }
    }
    Ok(())
}

/// Retry a failed run (design D10): flip `Failed` to `InProgress` and let
/// the observer's ensure semantics take over. Nothing is broadcast.
///
/// # Errors
/// Returns an error when the run is missing, is not `Failed`, or carries no
/// on-ledger meta.
pub async fn retry_run(ol: &OnLedger, instance_name: &str) -> Result<()> {
    let db = ol.db();
    let Some(run) = db.get_workflow_run(instance_name).await? else {
        bail!("workflow run {instance_name} not found");
    };
    if run.status != WorkflowProgress::Failed {
        bail!("workflow run {instance_name} is {}, not failed", run.status);
    }
    if read_run_meta(&run).is_none() {
        bail!("workflow run {instance_name} predates on-ledger coordination and cannot be retried");
    }
    set_status(db, instance_name, WorkflowProgress::InProgress, None).await?;
    tracing::info!(instance = %instance_name, "run retried");
    Ok(())
}

/// Build the `WorkflowProposal_Accept` arguments for this node.
pub fn accept_args(
    identity: &NodeIdentity,
    keys: &ProposerKeyMaterial,
    member_party: Option<CantonId>,
) -> AcceptArgs {
    AcceptArgs {
        acceptor: identity.node_party.clone(),
        participant_id: identity.participant_id.to_string(),
        namespace_fingerprint: keys.namespace_fingerprint.clone(),
        signing_public_key_hex: keys.signing_public_key_hex.clone(),
        daml_key_fingerprint: keys.daml_key_fingerprint.clone(),
        member_party,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onledger::daml::codec::tests::{party, proposal_full};

    const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

    fn participant(n: u8) -> CantonId {
        CantonId::parse(&format!("participant{n}::{NS}")).expect("id")
    }

    fn meta() -> RunMeta {
        RunMeta {
            proposal_cid: "00proposal".into(),
            coordinator_party: party("node-a"),
            coordinator_participant: participant(1),
            member_variant: Some(MemberVariant::Joiner),
            topology_hashes: [("dnd".to_string(), "1220ab".to_string())]
                .into_iter()
                .collect(),
        }
    }

    fn run_with(config_json: &str) -> WorkflowRun {
        WorkflowRun {
            instance_name: "cbtc-creation".into(),
            kind: WorkflowKind::Onboarding,
            role: WorkflowRole::Coordinator,
            status: WorkflowProgress::InProgress,
            current_step: "GenerateKeys".into(),
            step_index: 0,
            step_total: 7,
            config_json: config_json.into(),
            coordinator_pubkey: None,
            coordinator_instance: None,
            coordinator_name: None,
            expected_peers: vec![],
            completed_peers: vec![],
            connected_peers: vec![],
            acs_progress: None,
            dec_party_id: None,
            prefix: None,
            participants: vec![],
            previous_threshold: None,
            new_threshold: None,
            kicked_participant: None,
            added_participant: None,
            package_names: vec![],
            dar_filenames: vec![],
            error: None,
            dismissed: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn quorum_is_the_larger_threshold() {
        // 4 owners, previous 3, new 2, kick one: wait for 3 signatures.
        assert_eq!(required_owner_signatures(3, 2), 3);
        assert_eq!(acceptances_needed(3, 2), 2);
        assert_eq!(required_owner_signatures(2, 3), 3);
        assert_eq!(required_owner_signatures(0, 0), 1);
        assert_eq!(acceptances_needed(0, 0), 0);
    }

    #[test]
    fn every_invitee_kinds_are_the_non_quorum_ones() {
        assert!(needs_every_invitee(WorkflowKind::Onboarding));
        assert!(needs_every_invitee(WorkflowKind::AddParty));
        assert!(needs_every_invitee(WorkflowKind::Contracts));
        assert!(needs_every_invitee(WorkflowKind::Dars));
        assert!(!needs_every_invitee(WorkflowKind::Kick));
        assert!(!needs_every_invitee(WorkflowKind::ChangeThreshold));
    }

    #[test]
    fn run_meta_round_trips_through_config_json_and_keeps_other_fields() {
        let json = with_meta(r#"{"party_id_prefix":"cbtc","threshold":2}"#, &meta()).expect("json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("value");
        assert_eq!(value["party_id_prefix"], "cbtc");
        assert_eq!(value["threshold"], 2);
        let run = run_with(&json);
        assert_eq!(read_run_meta(&run), Some(meta()));
        assert!(read_run_meta(&run_with(r#"{"party_id_prefix":"cbtc"}"#)).is_none());
        assert!(read_run_meta(&run_with("not json")).is_none());
        // A non-object config is replaced, not appended to.
        let repaired = with_meta("[1,2]", &meta()).expect("json");
        assert!(read_run_meta(&run_with(&repaired)).is_some());
    }

    #[test]
    fn step_lists_match_design_section_6() {
        assert_eq!(
            steps_for(WorkflowKind::Onboarding, WorkflowRole::Coordinator, None),
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
            steps_for(WorkflowKind::Onboarding, WorkflowRole::Peer, None),
            ["GenerateKeys", "CoSignNamespace", "CoSignParty", "Complete"]
        );
        assert_eq!(
            steps_for(WorkflowKind::AddParty, WorkflowRole::Coordinator, None),
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
            steps_for(
                WorkflowKind::AddParty,
                WorkflowRole::Peer,
                Some(MemberVariant::Joiner)
            ),
            [
                "GenerateKeys",
                "CoSignChanges",
                "SyncAcs",
                "ClearOnboarding",
                "Complete"
            ]
        );
        assert_eq!(
            steps_for(
                WorkflowKind::AddParty,
                WorkflowRole::Peer,
                Some(MemberVariant::Member)
            ),
            ["CoSignChanges", "PublishManifest", "Complete"]
        );
        for kind in [WorkflowKind::Kick, WorkflowKind::ChangeThreshold] {
            assert_eq!(
                steps_for(kind, WorkflowRole::Coordinator, None),
                [
                    "WaitingForAcceptances",
                    "ProposeChanges",
                    "AwaitChanges",
                    "Complete"
                ]
            );
            assert_eq!(
                steps_for(kind, WorkflowRole::Peer, None),
                ["CoSignChanges", "Complete"]
            );
        }
        assert_eq!(
            steps_for(WorkflowKind::Contracts, WorkflowRole::Coordinator, None),
            [
                "WaitingForAcceptances",
                "AwaitDars",
                "PrepareSubmissions",
                "CollectSignatures",
                "ExecuteSubmissions",
                "Complete"
            ]
        );
        assert_eq!(
            steps_for(WorkflowKind::Contracts, WorkflowRole::Peer, None),
            ["UploadDars", "SignSubmissions", "Complete"]
        );
        assert_eq!(
            steps_for(WorkflowKind::Dars, WorkflowRole::Coordinator, None),
            ["WaitingForAcceptances", "AwaitVetting", "Complete"]
        );
        assert_eq!(
            steps_for(WorkflowKind::Dars, WorkflowRole::Peer, None),
            ["UploadDars", "Complete"]
        );
    }

    #[test]
    fn every_step_list_ends_with_complete() {
        for kind in [
            WorkflowKind::Onboarding,
            WorkflowKind::AddParty,
            WorkflowKind::Kick,
            WorkflowKind::ChangeThreshold,
            WorkflowKind::Contracts,
            WorkflowKind::Dars,
        ] {
            for (role, variant) in [
                (WorkflowRole::Coordinator, None),
                (WorkflowRole::Peer, None),
                (WorkflowRole::Peer, Some(MemberVariant::Joiner)),
                (WorkflowRole::Peer, Some(MemberVariant::Member)),
            ] {
                let steps = steps_for(kind, role, variant);
                assert_eq!(
                    steps.last(),
                    Some(&COMPLETE_STEP),
                    "{kind} {role} {variant:?}"
                );
            }
        }
    }

    #[test]
    fn peer_instance_name_follows_the_design_shape() {
        let name = peer_instance_name(
            WorkflowKind::ChangeThreshold,
            &participant(1).to_string(),
            "cbtc-change-threshold-1",
        );
        assert_eq!(
            name,
            "peer-changethreshold-participant1::12-cbtc-change-threshold-1"
        );
    }

    #[test]
    fn member_variant_is_joiner_only_for_the_add_party_new_participant() {
        let mut p = proposal_full();
        p.kind = WorkflowKind::AddParty;
        p.new_participant = Some(participant(4).to_string());
        assert_eq!(
            member_variant_for(&p, &participant(4)),
            Some(MemberVariant::Joiner)
        );
        assert_eq!(
            member_variant_for(&p, &participant(2)),
            Some(MemberVariant::Member)
        );
        p.kind = WorkflowKind::Kick;
        assert_eq!(member_variant_for(&p, &participant(4)), None);
    }

    #[test]
    fn start_request_accessors() {
        let req = StartRequest::Kick {
            dec_party_id: CantonId::parse(&format!("cbtc::{NS}")).expect("id"),
            participant_id: participant(3),
            new_threshold: 2,
            previous_threshold: 0,
            instance_name: "cbtc-kick-1".into(),
        };
        assert_eq!(req.kind(), WorkflowKind::Kick);
        assert_eq!(req.instance_name(), "cbtc-kick-1");
        assert!(req.dec_party_id().is_some());
        assert_eq!(req.threshold(), Some(2));
        assert_eq!(req.previous_threshold(), None, "0 means unknown");
    }

    #[test]
    fn preflight_rejection_names_every_peer() {
        let err = PreflightRejected::peers(vec![
            (
                participant(2),
                "has not vetted the coordination package".into(),
            ),
            (participant(3), "no registry entry visible".into()),
        ]);
        assert!(err.message.contains("participant2"));
        assert!(err.message.contains("participant3"));
        let any: anyhow::Error = err.clone().into();
        assert_eq!(any.downcast_ref::<PreflightRejected>(), Some(&err));
    }

    #[test]
    fn threshold_gates_refuse_out_of_range_and_unreachable_kicks() {
        let party_id = CantonId::parse(&format!("cbtc::{NS}")).expect("id");
        let kick = StartRequest::Kick {
            dec_party_id: party_id.clone(),
            participant_id: participant(3),
            new_threshold: 2,
            previous_threshold: 3,
            instance_name: "k".into(),
        };
        let head_dnd = topology::dnd_of(&topology::build_dnd(
            &["a".into(), "b".into(), "c".into()],
            3,
        ))
        .expect("dnd")
        .clone();
        let targets = ResolvedTargets {
            participants: vec![participant(1), participant(2)],
            threshold: Some(2),
            previous_threshold: Some(3),
            head: HeadState {
                dnd: Some(head_dnd.clone()),
                p2p: None,
            },
            ..Default::default()
        };
        let err = check_thresholds(&kick, &targets).expect_err("previous 3 > 2 owners");
        assert!(err.downcast_ref::<PreflightRejected>().is_some(), "{err}");

        let ok_targets = ResolvedTargets {
            previous_threshold: Some(2),
            ..targets.clone()
        };
        check_thresholds(&kick, &ok_targets).expect("previous 2 fits 2 owners");

        let too_high = ResolvedTargets {
            threshold: Some(3),
            previous_threshold: Some(2),
            ..targets
        };
        let err = check_thresholds(&kick, &too_high).expect_err("threshold 3 > 2 owners");
        assert!(err.to_string().contains("outside 1..=2"), "{err}");

        let onboarding = StartRequest::Onboarding {
            party_id_prefix: "cbtc".into(),
            peer_ids: vec![participant(2), participant(3)],
            threshold: Some(4),
            instance_name: "o".into(),
        };
        let targets = ResolvedTargets {
            participants: vec![participant(1), participant(2), participant(3)],
            threshold: Some(4),
            ..Default::default()
        };
        assert!(check_thresholds(&onboarding, &targets).is_err());
    }

    #[test]
    fn coordinator_config_json_carries_the_card_fields_and_the_meta() {
        let req = StartRequest::AddParty {
            dec_party_id: CantonId::parse(&format!("cbtc::{NS}")).expect("id"),
            new_participant_id: participant(4),
            new_threshold: 3,
            previous_threshold: 2,
            instance_name: "cbtc-add-party-1".into(),
        };
        let targets = ResolvedTargets {
            participants: vec![participant(1), participant(2), participant(4)],
            previous_threshold: Some(2),
            threshold: Some(3),
            ..Default::default()
        };
        let json = coordinator_config_json(&req, &targets, &meta()).expect("json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("value");
        assert_eq!(value["new_threshold"], 3);
        assert_eq!(value["previous_threshold"], 2);
        assert_eq!(value["participants"].as_array().map(Vec::len), Some(3));
        assert_eq!(value[RUN_META_KEY]["proposal_cid"], "00proposal");
        assert_eq!(value[RUN_META_KEY]["member_variant"], "Joiner");
    }

    #[test]
    fn peer_config_json_mirrors_the_invitation_fields() {
        let json = peer_config_json(&proposal_full(), &meta()).expect("json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("value");
        assert_eq!(value["prefix"], "cbtc");
        assert_eq!(value["dar_filenames"][0], "governance-core-v1-0.1.0.dar");
        assert_eq!(value["dar_hashes"][0], "deadbeef");
        assert_eq!(value["new_threshold"], 2);
        assert_eq!(value["previous_threshold"], 3);
        assert_eq!(value["package_names"][0], "governance-core-v1");
        assert_eq!(
            value[RUN_META_KEY]["coordinator_party"],
            party("node-a").to_string()
        );
    }

    #[test]
    fn describe_declines_names_every_decliner() {
        let d = |who: &str, why: &str| Decline {
            contract_id: "00d".into(),
            offset: 1,
            record: WorkflowDeclineRecord {
                proposal: "00proposal".into(),
                proposer: party("node-a"),
                decliner: party(who),
                observers: vec![],
                run_id: "r".into(),
                reason: why.into(),
                declined_at: 1,
            },
        };
        let text = describe_declines(&[&d("node-b", "busy"), &d("node-c", "no")]);
        assert!(text.contains("node-b"));
        assert!(text.contains("busy"));
        assert!(text.contains("node-c"));
    }

    #[test]
    fn snapshot_lookups_filter_by_contract_and_run() {
        let mut p = ActiveProposal {
            contract_id: "00proposal".into(),
            offset: 1,
            record: proposal_full(),
        };
        p.record.invitees = vec![party("node-b")];
        let snapshot = ProposalSnapshot {
            proposals: vec![p],
            outcomes: vec![Outcome {
                contract_id: "00o".into(),
                offset: 2,
                record: WorkflowOutcomeRecord {
                    proposer: party("node-a"),
                    run_id: "cbtc-creation".into(),
                    kind: WorkflowKind::Onboarding,
                    observers: vec![],
                    succeeded: true,
                    error: None,
                    finished_at: 3,
                },
            }],
            ..Default::default()
        };
        assert!(snapshot.proposal("00proposal").is_some());
        assert!(snapshot.proposal("00other").is_none());
        assert_eq!(snapshot.for_me(&party("node-b")).len(), 1);
        assert!(snapshot.for_me(&party("node-a")).is_empty());
        assert!(snapshot.for_me(&party("node-z")).is_empty());
        assert!(
            snapshot
                .outcome_for(&party("node-a"), "cbtc-creation")
                .is_some()
        );
        assert!(snapshot.outcome_for(&party("node-a"), "other").is_none());
        assert_eq!(
            snapshot.active_cids(),
            ["00proposal".to_string()].into_iter().collect()
        );
    }
}
