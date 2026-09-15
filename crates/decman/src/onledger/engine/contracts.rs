//! Contracts driver (design sections 6, D7, D8): create contracts for a
//! decentralized party through `SubmissionRound`s.
//!
//! Coordinator: `WaitingForAcceptances` -> `AwaitDars` -> `PrepareSubmissions`
//! -> `CollectSignatures` -> `ExecuteSubmissions` -> `Complete`.
//! Member: `UploadDars` -> `SignSubmissions` -> `Complete`.
//!
//! Every tick reads the rounds on the ledger, does one bounded unit of work
//! for the run's current step, persists progress on the run row, and
//! returns. The ledger is the state machine for rounds and signatures; two
//! local artefacts hold what the ledger cannot: the proposer's own signature
//! per round (the proposer is not a `signer`, so it cannot exercise
//! `SubmissionRound_Sign`) and the update id of an executed round (so a
//! crash between execute and close does not execute the same transaction
//! twice, which the mediator would reject).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::{Context, Result};
use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::Signature as CantonSignature, protocol::v30::PartyToParticipant,
};
use common::{
    api::ContractDefinition,
    canton_id::CantonId,
    types::{WorkflowKind, WorkflowProgress, WorkflowRun},
};
use prost::Message;

use crate::{
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    server::package_inventory,
    utils,
};

use super::{
    COMPLETE_STEP, KindDriver, MemberVariant, OnLedger, PreflightRejected, ProposalExtras, RunMeta,
    StartRequest, TickCtx, WAITING_FOR_ACCEPTANCES_STEP, advance_step, complete_run, fail_run,
    pin_topology_hash,
};
use crate::onledger::{
    daml::{
        ActiveContract,
        codec::{DarPin, SubmissionRoundRecord, SubmissionSignatureRecord},
    },
    keys,
    proposals::{self, ActiveProposal},
    registry,
    submission::{self, ContractsRunConfig},
    topology,
};

pub struct Contracts;

pub const AWAIT_DARS_STEP: &str = "AwaitDars";
pub const PREPARE_SUBMISSIONS_STEP: &str = "PrepareSubmissions";
pub const COLLECT_SIGNATURES_STEP: &str = "CollectSignatures";
pub const EXECUTE_SUBMISSIONS_STEP: &str = "ExecuteSubmissions";
pub const UPLOAD_DARS_STEP: &str = "UploadDars";
pub const SIGN_SUBMISSIONS_STEP: &str = "SignSubmissions";

pub const COORDINATOR_STEPS: &[&str] = &[
    WAITING_FOR_ACCEPTANCES_STEP,
    AWAIT_DARS_STEP,
    PREPARE_SUBMISSIONS_STEP,
    COLLECT_SIGNATURES_STEP,
    EXECUTE_SUBMISSIONS_STEP,
    COMPLETE_STEP,
];

pub const MEMBER_STEPS: &[&str] = &[UPLOAD_DARS_STEP, SIGN_SUBMISSIONS_STEP, COMPLETE_STEP];

/// The proposer's own Canton `Signature` per round, keyed by round cid.
///
/// TODO(workflow/storage.rs): move both kinds into `artifact_kinds` when
/// that file is open for edits.
const OWN_SIGNATURE_ARTIFACT: &str = "contracts_own_signature";

/// The update id of an executed round, keyed by contract index. Written
/// before the round is closed, so a retry closes instead of executing again,
/// and a step-back never re-prepares an index that is already on the ledger.
const EXECUTED_ROUND_ARTIFACT: &str = "contracts_round_executed";

/// Prefix of the `RunMeta.topology_hashes` keys a member pins before it
/// signs a round: `round:{cid}` -> prepared hash.
const ROUND_PIN_PREFIX: &str = "round:";

type Round = ActiveContract<SubmissionRoundRecord>;

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// The package names a contracts request creates under, before resolution:
/// a `#name` ref is stripped, anything else is kept as-is.
pub fn package_names_of(req: &StartRequest) -> Vec<String> {
    let StartRequest::Contracts { contracts, .. } = req else {
        return Vec::new();
    };
    let names: BTreeSet<String> = contracts
        .iter()
        .map(|c| c.package_id.trim_start_matches('#').to_string())
        .filter(|n| !n.is_empty())
        .collect();
    names.into_iter().collect()
}

/// Whether a package reference is a package id (a sha256 in hex) rather
/// than a name.
fn looks_like_package_id(reference: &str) -> bool {
    reference.len() == 64 && reference.chars().all(|c| c.is_ascii_hexdigit())
}

/// The package names of a contracts request with every package id resolved
/// through the local inventory (design D7: members check root creates
/// against names, not ids).
///
/// # Errors
/// Returns an error for a package id the local participant does not hold.
pub fn resolve_package_names(
    req: &StartRequest,
    id_to_name: &HashMap<String, String>,
) -> Result<Vec<String>> {
    let mut names = BTreeSet::new();
    for reference in package_names_of(req) {
        if looks_like_package_id(&reference) {
            let name = id_to_name.get(&reference).with_context(|| {
                format!("package {reference} is not on this participant; upload its DAR first")
            })?;
            names.insert(name.clone());
        } else {
            names.insert(reference);
        }
    }
    Ok(names.into_iter().collect())
}

/// Participants of the request that do not host the party, for the 409.
pub fn participants_not_hosting(
    participant_ids: &[CantonId],
    hosts: &BTreeSet<String>,
    dec_party_id: &CantonId,
) -> Vec<(CantonId, String)> {
    participant_ids
        .iter()
        .filter(|p| !hosts.contains(&p.to_string()))
        .map(|p| (p.clone(), format!("does not host {dec_party_id}")))
        .collect()
}

/// Invitees without a counted acceptance.
pub fn missing_invitees(invitees: &[CantonId], accepted: &BTreeSet<&CantonId>) -> Vec<CantonId> {
    invitees
        .iter()
        .filter(|i| !accepted.contains(i))
        .cloned()
        .collect()
}

/// Whether enough members accepted for the party to reach its signing
/// threshold. The proposer signs its own rounds, so it counts as one.
///
/// A contracts run needs `party_signing_keys.threshold` signatures, not one
/// per invitee. Waiting for every invitee lets one absent operator hold up a
/// party that has the quorum to act.
pub fn signing_quorum_reached(accepted_members: usize, required_signatures: usize) -> bool {
    accepted_members.saturating_add(1) >= required_signatures
}

/// The pins whose main package id is not in `vetted`, by filename.
pub fn unvetted_pins(pins: &[DarPin], vetted: &HashSet<String>) -> Vec<String> {
    pins.iter()
        .filter(|p| !vetted.contains(&p.main_package_id))
        .map(|p| p.filename.clone())
        .collect()
}

/// What the proposer must do with the rounds it sees (design D7).
#[derive(Debug, Default)]
pub struct RoundPlan<'a> {
    /// Contract indices that are neither executed nor covered by a live
    /// round: prepare and open them.
    pub missing: Vec<i64>,
    /// Rounds of an index that already executed: the close did not land.
    pub done: Vec<&'a Round>,
    /// Rounds past their deadline: close them as `expired`.
    pub expired: Vec<&'a Round>,
    /// A second live round for an index that already has one: close it as
    /// `duplicate` so one prepared transaction is executed per contract.
    pub duplicates: Vec<&'a Round>,
    /// One live round per index, in index order.
    pub live: Vec<&'a Round>,
}

/// Classify the rounds of a run against the contract list. `executed` holds
/// the indices whose transaction is on the ledger; those never come back as
/// missing, or a step-back would create the same contract twice.
pub fn plan_rounds<'a>(
    rounds: &'a [Round],
    contract_count: usize,
    executed: &BTreeSet<i64>,
    now_micros: i64,
) -> RoundPlan<'a> {
    let mut plan = RoundPlan::default();
    let mut live_by_index: BTreeMap<i64, &Round> = BTreeMap::new();
    for round in rounds {
        if executed.contains(&round.record.index) {
            plan.done.push(round);
            continue;
        }
        if round.record.deadline <= now_micros {
            plan.expired.push(round);
            continue;
        }
        match live_by_index.get(&round.record.index) {
            // The earlier round wins so both sides converge on one.
            Some(existing) if existing.offset <= round.offset => plan.duplicates.push(round),
            Some(existing) => {
                plan.duplicates.push(existing);
                live_by_index.insert(round.record.index, round);
            }
            None => {
                live_by_index.insert(round.record.index, round);
            }
        }
    }
    plan.missing = (0..contract_count)
        .map(|i| i64::try_from(i).unwrap_or(i64::MAX))
        .filter(|i| !executed.contains(i) && !live_by_index.contains_key(i))
        .collect();
    plan.live = live_by_index.into_values().collect();
    plan
}

/// Whether a member has pinned at least one round, so an empty round list
/// means "every round is closed" and not "nothing prepared yet".
fn has_pinned_round(meta: &RunMeta) -> bool {
    meta.topology_hashes
        .keys()
        .any(|k| k.starts_with(ROUND_PIN_PREFIX))
}

// ---------------------------------------------------------------------------
// Local artefacts
// ---------------------------------------------------------------------------

async fn write_artifact(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    kind: &str,
    key: &str,
    payload: &[u8],
) -> Result<()> {
    let mut tx = ctx.db().begin_transaction().await?;
    tx.write_workflow_artifact(&run.instance_name, kind, Some(key), payload)
        .await?;
    Commitable::commit(tx).await
}

async fn own_signature(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    round_cid: &str,
) -> Result<Option<CantonSignature>> {
    ctx.db()
        .read_workflow_artifact(&run.instance_name, OWN_SIGNATURE_ARTIFACT, Some(round_cid))
        .await?
        .map(|bytes| {
            CantonSignature::decode(bytes.as_slice()).context("own signature artefact is corrupt")
        })
        .transpose()
}

/// The executed rounds of a run: contract index -> update id.
async fn executed_rounds(ctx: &TickCtx<'_>, run: &WorkflowRun) -> Result<BTreeMap<i64, String>> {
    Ok(ctx
        .db()
        .list_workflow_artifacts(&run.instance_name, EXECUTED_ROUND_ARTIFACT)
        .await?
        .into_iter()
        .filter_map(|(key, payload)| {
            key.parse::<i64>()
                .ok()
                .map(|index| (index, String::from_utf8_lossy(&payload).into_owned()))
        })
        .collect())
}

/// Close every round the plan marks as finished. Returns how many closed.
async fn close_finished_rounds(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    plan: &RoundPlan<'_>,
    executed: &BTreeMap<i64, String>,
) -> Result<usize> {
    let mut closed = 0;
    for round in &plan.done {
        let update_id = executed
            .get(&round.record.index)
            .map(String::as_str)
            .unwrap_or_default();
        submission::close_round(
            ctx.client,
            &round.contract_id,
            &format!("executed:{update_id}"),
        )
        .await?;
        closed += 1;
    }
    for round in &plan.expired {
        tracing::info!(
            instance = %run.instance_name,
            round = %round.contract_id,
            index = round.record.index,
            "round expired"
        );
        submission::close_round(ctx.client, &round.contract_id, "expired").await?;
        closed += 1;
    }
    for round in &plan.duplicates {
        submission::close_round(ctx.client, &round.contract_id, "duplicate").await?;
        closed += 1;
    }
    Ok(closed)
}

/// Sign every live round this node has not signed yet and keep the
/// signature locally. Heals a tick that opened rounds but crashed before
/// signing them.
async fn ensure_own_signatures(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    dec_party_id: &CantonId,
    rounds: &[&Round],
) -> Result<()> {
    for round in rounds {
        if own_signature(ctx, run, &round.contract_id).await?.is_some() {
            continue;
        }
        let hash = hex::decode(&round.record.prepared_hash_hex).context("preparedHashHex")?;
        let signature = submission::sign_hash_with_party_key(ctx.ol, dec_party_id, &hash).await?;
        write_artifact(
            ctx,
            run,
            OWN_SIGNATURE_ARTIFACT,
            &round.contract_id,
            &signature.encode_to_vec(),
        )
        .await?;
        tracing::info!(
            instance = %run.instance_name,
            round = %round.contract_id,
            signed_by = %signature.signed_by,
            "own signature recorded"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared reads
// ---------------------------------------------------------------------------

/// The rounds this node opened for `run_id`, in index order.
async fn my_rounds(ctx: &TickCtx<'_>, run_id: &str) -> Result<Vec<Round>> {
    let me = ctx.client.node_party();
    let mut rounds: Vec<Round> = submission::read_rounds_for_run(ctx.client, run_id)
        .await?
        .into_iter()
        .filter(|r| r.record.proposer == *me)
        .collect();
    rounds.sort_by_key(|r| (r.record.index, r.offset));
    Ok(rounds)
}

async fn head_p2p(ctx: &TickCtx<'_>, dec_party_id: &CantonId) -> Result<PartyToParticipant> {
    topology::read_accepted_p2p(ctx.ol.config(), &ctx.sync_id, dec_party_id)
        .await?
        .map(|a| a.mapping)
        .with_context(|| {
            format!("{dec_party_id} has no PartyToParticipant in the synchronizer store")
        })
}

/// Fail the coordinator row and tell the invitees (design D10).
async fn fail_closed(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    message: &str,
) -> Result<()> {
    fail_run(ctx.db(), run, message).await?;
    if let Err(e) = proposals::finish(
        ctx.client,
        &meta.proposal_cid,
        false,
        Some(message.to_string()),
    )
    .await
    {
        tracing::warn!(proposal = %meta.proposal_cid, error = %e, "WorkflowProposal_Finish failed");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Coordinator steps
// ---------------------------------------------------------------------------

async fn wait_for_acceptances(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &ActiveProposal,
    dec_party_id: &CantonId,
) -> Result<()> {
    let raw = ctx.proposals.acceptances_for(&proposal.contract_id);
    // A transient hosting-read failure leaves an acceptance uncounted for a
    // tick; an `Err` here is a D6 violation (two acceptances by one node).
    let counted =
        match proposals::counted_acceptances_verified(ctx.ol.config(), proposal, &raw).await {
            Ok(counted) => counted,
            Err(e) => {
                return fail_closed(ctx, run, meta, &format!("acceptances rejected: {e:#}")).await;
            }
        };
    let accepted: BTreeSet<&CantonId> = counted.iter().map(|a| &a.record.acceptor).collect();
    let head = head_p2p(ctx, dec_party_id).await?;
    let required = match submission::required_signatures(&head) {
        Ok(required) => required,
        Err(e) => return fail_closed(ctx, run, meta, &e.to_string()).await,
    };
    if !signing_quorum_reached(counted.len(), required) {
        tracing::debug!(
            instance = %run.instance_name,
            accepted = counted.len(),
            required,
            missing = ?missing_invitees(&proposal.record.invitees, &accepted),
            "waiting for a signing quorum of acceptances"
        );
        return Ok(());
    }
    // Members report their own Daml key; the cache a later kick reads is
    // filled from that report (design M6).
    for a in &counted {
        if let Ok(participant) = CantonId::parse(&a.record.participant_id) {
            keys::record_member_keys(
                ctx.db(),
                dec_party_id,
                &participant,
                a.record.namespace_fingerprint.as_deref(),
                a.record.daml_key_fingerprint.as_deref(),
            )
            .await?;
        }
    }
    advance_step(ctx.db(), run, AWAIT_DARS_STEP).await
}

async fn await_dars(ctx: &TickCtx<'_>, run: &WorkflowRun, proposal: &ActiveProposal) -> Result<()> {
    let pins = &proposal.record.dar_pins;
    if pins.is_empty() {
        return advance_step(ctx.db(), run, PREPARE_SUBMISSIONS_STEP).await;
    }
    for participant in &proposal.record.participants {
        let participant = CantonId::parse(participant)
            .with_context(|| format!("participant `{participant}` on the proposal"))?;
        let vetted = registry::fetch_vetted_packages_for(ctx.ol.config(), &participant).await?;
        let missing = unvetted_pins(pins, &vetted);
        if !missing.is_empty() {
            tracing::debug!(
                instance = %run.instance_name,
                %participant,
                ?missing,
                "waiting for the participant to vet the pinned DARs"
            );
            return Ok(());
        }
    }
    advance_step(ctx.db(), run, PREPARE_SUBMISSIONS_STEP).await
}

/// Prepare, open, and self-sign the rounds for `indices`.
async fn open_rounds_for(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    proposal: &ActiveProposal,
    config: &ContractsRunConfig,
    indices: &[i64],
) -> Result<()> {
    let indexed: Vec<(i64, &ContractDefinition)> = indices
        .iter()
        .filter_map(|&i| {
            usize::try_from(i)
                .ok()
                .and_then(|u| config.contracts.get(u))
                .map(|c| (i, c))
        })
        .collect();
    if indexed.is_empty() {
        return Ok(());
    }
    let party = &config.decentralized_party_id;
    let prepared = submission::prepare_rounds_at(ctx.ol, run, party, &indexed).await?;
    let cids = submission::open_rounds(
        ctx.client,
        &run.instance_name,
        &proposal.record.invitees,
        party,
        party,
        &prepared,
    )
    .await?;
    for (cid, round) in cids.iter().zip(&prepared) {
        let hash = hex::decode(&round.prepared_hash_hex).context("preparedHashHex")?;
        let signature = submission::sign_hash_with_party_key(ctx.ol, party, &hash).await?;
        write_artifact(
            ctx,
            run,
            OWN_SIGNATURE_ARTIFACT,
            cid,
            &signature.encode_to_vec(),
        )
        .await?;
    }
    Ok(())
}

async fn prepare_submissions(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    proposal: &ActiveProposal,
    config: &ContractsRunConfig,
) -> Result<()> {
    let rounds = my_rounds(ctx, &run.instance_name).await?;
    let executed = executed_rounds(ctx, run).await?;
    let plan = plan_rounds(
        &rounds,
        config.contracts.len(),
        &executed.keys().copied().collect(),
        ctx.now_micros,
    );
    // Replacements open before old rounds close, so a member never sees an
    // empty run and completes early.
    open_rounds_for(ctx, run, proposal, config, &plan.missing).await?;
    close_finished_rounds(ctx, run, &plan, &executed).await?;
    ensure_own_signatures(ctx, run, &config.decentralized_party_id, &plan.live).await?;
    advance_step(ctx.db(), run, COLLECT_SIGNATURES_STEP).await
}

/// The verified, deduplicated signatures of one live round, this node's
/// own included.
async fn verified_for(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    head: &PartyToParticipant,
    round: &Round,
) -> Result<BTreeMap<String, submission::VerifiedSignature>> {
    let own = own_signature(ctx, run, &round.contract_id).await?;
    let participant = ctx.participant_id.to_string();
    submission::verified_signatures_for_round(
        ctx.client,
        head,
        round,
        own.as_ref().map(|s| (s, participant.as_str())),
    )
    .await
}

async fn collect_signatures(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &ActiveProposal,
    config: &ContractsRunConfig,
) -> Result<()> {
    let party = &config.decentralized_party_id;
    let head = head_p2p(ctx, party).await?;
    let threshold = match submission::required_signatures(&head) {
        Ok(t) => t,
        Err(e) => return fail_closed(ctx, run, meta, &e.to_string()).await,
    };
    let rounds = my_rounds(ctx, &run.instance_name).await?;
    let executed = executed_rounds(ctx, run).await?;
    let plan = plan_rounds(
        &rounds,
        config.contracts.len(),
        &executed.keys().copied().collect(),
        ctx.now_micros,
    );

    if !plan.missing.is_empty()
        || !plan.done.is_empty()
        || !plan.expired.is_empty()
        || !plan.duplicates.is_empty()
    {
        // Replacements open before the old rounds close, so a member never
        // sees an empty run and completes early. One bounded unit: count on
        // the next tick.
        open_rounds_for(ctx, run, proposal, config, &plan.missing).await?;
        close_finished_rounds(ctx, run, &plan, &executed).await?;
        return Ok(());
    }

    ensure_own_signatures(ctx, run, party, &plan.live).await?;
    let mut ready = true;
    for round in &plan.live {
        let verified = verified_for(ctx, run, &head, round).await?;
        tracing::debug!(
            instance = %run.instance_name,
            index = round.record.index,
            verified = verified.len(),
            threshold,
            "signatures collected"
        );
        if verified.len() < threshold {
            ready = false;
        }
    }
    if ready {
        advance_step(ctx.db(), run, EXECUTE_SUBMISSIONS_STEP).await?;
    }
    Ok(())
}

async fn execute_submissions(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    config: &ContractsRunConfig,
) -> Result<()> {
    let party = &config.decentralized_party_id;
    let rounds = my_rounds(ctx, &run.instance_name).await?;
    let executed = executed_rounds(ctx, run).await?;
    let plan = plan_rounds(
        &rounds,
        config.contracts.len(),
        &executed.keys().copied().collect(),
        ctx.now_micros,
    );

    if !plan.done.is_empty() {
        // Executed on an earlier tick; only the close is outstanding.
        close_finished_rounds(ctx, run, &plan, &executed).await?;
        return Ok(());
    }
    if !plan.expired.is_empty() || !plan.duplicates.is_empty() {
        // CollectSignatures re-prepares expired rounds and closes duplicates.
        return advance_step(ctx.db(), run, COLLECT_SIGNATURES_STEP).await;
    }
    let Some(round) = plan.live.first() else {
        if plan.missing.is_empty() {
            // The row first: a run whose contracts are on the ledger must not
            // read as failed when the Finish call fails afterwards.
            complete_run(ctx.db(), run).await?;
            if let Err(e) = proposals::finish(ctx.client, &meta.proposal_cid, true, None).await {
                tracing::warn!(proposal = %meta.proposal_cid, error = %e, "WorkflowProposal_Finish failed; the proposal expires on its own");
            }
            tracing::info!(instance = %run.instance_name, rounds = executed.len(), "contracts run complete");
            return Ok(());
        }
        // Some contract has neither a round nor an execution: prepare again.
        tracing::warn!(instance = %run.instance_name, missing = ?plan.missing, "no open rounds at ExecuteSubmissions; preparing again");
        return advance_step(ctx.db(), run, PREPARE_SUBMISSIONS_STEP).await;
    };

    let head = head_p2p(ctx, party).await?;
    let threshold = match submission::required_signatures(&head) {
        Ok(t) => t,
        Err(e) => return fail_closed(ctx, run, meta, &e.to_string()).await,
    };
    let verified = verified_for(ctx, run, &head, round).await?;
    if verified.len() < threshold {
        // The key set moved under us; collect again.
        return advance_step(ctx.db(), run, COLLECT_SIGNATURES_STEP).await;
    }

    match submission::execute_round_with_events(ctx.ol, party, round, &verified).await {
        Ok(result) => {
            // Keyed by contract index: the plan must never see this index
            // as missing again, whatever happens to the round contract.
            write_artifact(
                ctx,
                run,
                EXECUTED_ROUND_ARTIFACT,
                &round.record.index.to_string(),
                result.update_id.as_bytes(),
            )
            .await?;
            match submission::record_created_contracts(ctx.db(), party, &result.created).await {
                Ok(added) => {
                    tracing::debug!(instance = %run.instance_name, added, "contract cache updated")
                }
                Err(e) => {
                    tracing::warn!(instance = %run.instance_name, error = %e, "contract cache not updated; the next parties refresh fills it")
                }
            }
            submission::close_round(
                ctx.client,
                &round.contract_id,
                &format!("executed:{}", result.update_id),
            )
            .await?;
            tracing::info!(
                instance = %run.instance_name,
                index = round.record.index,
                update_id = %result.update_id,
                created = result.created.len(),
                "round executed"
            );
            Ok(())
        }
        // A blind retry would hit the mediator's duplicate check when the
        // first attempt did commit, so the operator decides (Retry re-enters
        // this step with the same round).
        Err(e) => {
            fail_closed(
                ctx,
                run,
                meta,
                &format!(
                    "ExecuteSubmission for round {} failed: {e:#}",
                    round.record.index
                ),
            )
            .await
        }
    }
}

// ---------------------------------------------------------------------------
// Member steps
// ---------------------------------------------------------------------------

async fn upload_dars(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    proposal: &ActiveProposal,
) -> Result<()> {
    let pins = &proposal.record.dar_pins;
    if pins.is_empty() {
        return advance_step(ctx.db(), run, SIGN_SUBMISSIONS_STEP).await;
    }
    let vetted = registry::fetch_vetted_packages_for(ctx.ol.config(), &ctx.participant_id).await?;
    let missing = unvetted_pins(pins, &vetted);
    if !missing.is_empty() {
        tracing::debug!(
            instance = %run.instance_name,
            ?missing,
            "waiting for the operator to upload the pinned DARs through POST /dars/upload"
        );
        return Ok(());
    }
    advance_step(ctx.db(), run, SIGN_SUBMISSIONS_STEP).await
}

async fn sign_submissions(
    ctx: &TickCtx<'_>,
    run: &WorkflowRun,
    meta: &RunMeta,
    proposal: &ActiveProposal,
) -> Result<()> {
    let Some(party) = proposal
        .record
        .dec_party_id
        .as_deref()
        .and_then(|s| CantonId::parse(s).ok())
    else {
        return fail_run(ctx.db(), run, "the WorkflowProposal names no decPartyId").await;
    };
    let me = ctx.client.node_party();
    let rounds: Vec<Round> = submission::read_rounds_for_run(ctx.client, &proposal.record.run_id)
        .await?
        .into_iter()
        .filter(|r| r.record.proposer == meta.coordinator_party && r.record.signers.contains(me))
        .collect();
    if rounds.is_empty() {
        // Every round this node signed is closed: the proposer executed
        // them. Before the first round exists there is nothing to conclude.
        if has_pinned_round(meta) {
            complete_run(ctx.db(), run).await?;
        } else {
            tracing::debug!(
                instance = %run.instance_name,
                run_id = %proposal.record.run_id,
                "no SubmissionRound names this node yet; waiting for the proposer"
            );
        }
        return Ok(());
    }

    let mine: HashSet<String> = ctx
        .client
        .list_active::<SubmissionSignatureRecord>()
        .await?
        .into_iter()
        .filter(|s| s.record.signer == *me)
        .map(|s| s.record.round)
        .collect();
    let todo: Vec<&Round> = rounds
        .iter()
        .filter(|r| !mine.contains(&r.contract_id) && r.record.deadline > ctx.now_micros)
        .collect();
    if todo.is_empty() {
        return Ok(());
    }

    let config = ctx.ol.config();
    let head = head_p2p(ctx, &party).await?;
    let identity = keys::local_identity_for_party(config, ctx.db(), Some(&party), None).await?;
    let Some(own_fingerprint) = identity.daml_key_fingerprint else {
        return fail_run(
            ctx.db(),
            run,
            &format!("this node holds no Daml signing key for {party}"),
        )
        .await;
    };
    let id_to_name = package_inventory::fetch_package_id_to_name(config).await?;

    for round in todo {
        // Design D5 step 5: never sign from a cached match. The proposal must
        // still be active and the row still in progress right before the
        // signature.
        let Some(live) = proposals::read_proposal(ctx.client, &meta.proposal_cid).await? else {
            return Ok(());
        };
        let Some(row) = ctx.db().get_workflow_run(&run.instance_name).await? else {
            return Ok(());
        };
        if row.status != WorkflowProgress::InProgress {
            return Ok(());
        }
        let checked = submission::check_round(
            &round.record,
            &live.record,
            &head,
            &own_fingerprint,
            ctx.now_micros,
        )
        .and_then(|()| {
            let ids = submission::root_create_package_ids(&round.record)?;
            submission::check_root_packages(&ids, &id_to_name, &live.record.package_names)
        });
        if let Err(e) = checked {
            return fail_run(
                ctx.db(),
                run,
                &format!("SubmissionRound {} rejected: {e:#}", round.record.index),
            )
            .await;
        }
        pin_topology_hash(
            ctx.db(),
            &run.instance_name,
            &format!("{ROUND_PIN_PREFIX}{}", round.contract_id),
            &round.record.prepared_hash_hex,
        )
        .await?;
        let signature_cid = submission::sign_round(ctx.ol, round, &party).await?;
        tracing::info!(
            instance = %run.instance_name,
            round = %round.contract_id,
            index = round.record.index,
            signature = %signature_cid,
            "SubmissionRound signed"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

impl KindDriver for Contracts {
    fn kind() -> WorkflowKind {
        WorkflowKind::Contracts
    }

    fn coordinator_steps() -> &'static [&'static str] {
        COORDINATOR_STEPS
    }

    fn member_steps(_variant: Option<MemberVariant>) -> &'static [&'static str] {
        MEMBER_STEPS
    }

    /// Section 6 gates for contracts: every participant hosts the party, the
    /// party has signing keys, and the participants can reach the signing
    /// threshold. The registry gate runs in `start_run`.
    async fn preflight(ol: &OnLedger, req: &StartRequest) -> Result<()> {
        let StartRequest::Contracts {
            dec_party_id,
            participant_ids,
            contracts,
            ..
        } = req
        else {
            return Ok(());
        };
        if contracts.is_empty() {
            return Err(PreflightRejected::new(
                "a contracts run needs at least one contract definition",
            )
            .into());
        }
        let config = ol.config();
        let sync_id = utils::get_synchronizer_id(config).await?;
        let head = topology::read_accepted_p2p(config, &sync_id, dec_party_id)
            .await?
            .with_context(|| format!("{dec_party_id} has no PartyToParticipant mapping"))?;
        let hosts: BTreeSet<String> = head
            .mapping
            .participants
            .iter()
            .map(|h| h.participant_uid.clone())
            .collect();
        let outsiders = participants_not_hosting(participant_ids, &hosts, dec_party_id);
        if !outsiders.is_empty() {
            return Err(PreflightRejected::peers(outsiders).into());
        }
        let required = submission::required_signatures(&head.mapping)
            .map_err(|e| PreflightRejected::new(e.to_string()))?;
        let signers: BTreeSet<&CantonId> = participant_ids
            .iter()
            .chain([config.participant_id()])
            .collect();
        if required > signers.len() {
            return Err(PreflightRejected::new(format!(
                "{dec_party_id} needs {required} signatures but only {} participant(s) take part",
                signers.len()
            ))
            .into());
        }
        Ok(())
    }

    /// The proposal names the packages members check root creates against
    /// (design D7), with package ids resolved to names. No proposer key
    /// material: rounds are signed with the party's `party_signing_keys`.
    /// No DAR pins: `StartRequest::Contracts` carries no DAR files.
    async fn prepare(ol: &OnLedger, req: &StartRequest) -> Result<ProposalExtras> {
        let id_to_name = package_inventory::fetch_package_id_to_name(ol.config()).await?;
        Ok(ProposalExtras {
            package_names: resolve_package_names(req, &id_to_name)?,
            ..ProposalExtras::default()
        })
    }

    async fn tick_coordinator(ctx: &TickCtx<'_>, run: &WorkflowRun, meta: &RunMeta) -> Result<()> {
        // `drive` already handled a vanished proposal.
        let Some(proposal) = ctx.proposals.proposal(&meta.proposal_cid) else {
            return Ok(());
        };
        let config = ContractsRunConfig::from_run(run)?;
        match run.current_step.as_str() {
            WAITING_FOR_ACCEPTANCES_STEP => {
                wait_for_acceptances(ctx, run, meta, proposal, &config.decentralized_party_id).await
            }
            AWAIT_DARS_STEP => await_dars(ctx, run, proposal).await,
            PREPARE_SUBMISSIONS_STEP => prepare_submissions(ctx, run, proposal, &config).await,
            COLLECT_SIGNATURES_STEP => collect_signatures(ctx, run, meta, proposal, &config).await,
            EXECUTE_SUBMISSIONS_STEP => execute_submissions(ctx, run, meta, &config).await,
            _ => Ok(()),
        }
    }

    async fn tick_member(ctx: &TickCtx<'_>, run: &WorkflowRun, meta: &RunMeta) -> Result<()> {
        let Some(proposal) = ctx.proposals.proposal(&meta.proposal_cid) else {
            return Ok(());
        };
        match run.current_step.as_str() {
            UPLOAD_DARS_STEP => upload_dars(ctx, run, proposal).await,
            SIGN_SUBMISSIONS_STEP => sign_submissions(ctx, run, meta, proposal).await,
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use common::api::ContractDefinition;

    use super::*;
    use crate::onledger::{
        daml::codec::tests::party,
        submission::{DEADLINE_SAFETY_MARGIN_MICROS, MAX_RECORD_TIME_HORIZON_MICROS, tests as sub},
    };

    const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
    const PKG: &str = "c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

    fn participant(n: u8) -> CantonId {
        CantonId::parse(&format!("participant{n}::{NS}")).expect("id")
    }

    fn contract(package_id: &str) -> ContractDefinition {
        ContractDefinition {
            id: "c".into(),
            name: "n".into(),
            package_id: package_id.into(),
            module_name: "M".into(),
            entity_name: "E".into(),
            fields: vec![],
        }
    }

    fn request(package_ids: &[&str]) -> StartRequest {
        StartRequest::Contracts {
            dec_party_id: CantonId::parse(&format!("cbtc::{NS}")).expect("id"),
            participant_ids: vec![],
            participant_parties: vec![],
            operator_party: CantonId::parse(&format!("op::{NS}")).expect("id"),
            contracts: package_ids.iter().map(|p| contract(p)).collect(),
            instance_name: "i".into(),
        }
    }

    fn round(cid: &str, index: i64, offset: i64, deadline: i64) -> Round {
        let prep = 1_700_000_000_000_000;
        let mut record = sub::round_for(&sub::prepared(prep), prep);
        record.index = index;
        record.deadline = deadline;
        Round {
            contract_id: cid.into(),
            offset,
            record,
        }
    }

    #[test]
    fn step_lists_match_design_section_6() {
        assert_eq!(
            COORDINATOR_STEPS,
            [
                "WaitingForAcceptances",
                "AwaitDars",
                "PrepareSubmissions",
                "CollectSignatures",
                "ExecuteSubmissions",
                "Complete"
            ]
        );
        assert_eq!(MEMBER_STEPS, ["UploadDars", "SignSubmissions", "Complete"]);
    }

    #[test]
    fn package_names_strip_the_ref_prefix_and_dedupe() {
        let req = request(&[
            "#governance-core-v1",
            "governance-core-v1",
            "#governance-action-v1",
            "",
        ]);
        assert_eq!(
            package_names_of(&req),
            vec!["governance-action-v1", "governance-core-v1"]
        );
    }

    #[test]
    fn package_ids_resolve_through_the_inventory_or_fail() {
        let known: HashMap<String, String> = [(PKG.to_string(), "governance-core-v1".to_string())]
            .into_iter()
            .collect();
        let req = request(&["#governance-action-v1", PKG]);
        assert_eq!(
            resolve_package_names(&req, &known).expect("resolves"),
            vec!["governance-action-v1", "governance-core-v1"]
        );
        let err = resolve_package_names(&req, &HashMap::new()).expect_err("unknown id");
        assert!(err.to_string().contains("upload its DAR first"), "{err}");
        // A non-contracts request has no package names.
        let other = StartRequest::Dars {
            dar_files: vec![],
            peer_ids: vec![],
            instance_name: "d".into(),
        };
        assert!(
            resolve_package_names(&other, &known)
                .expect("empty")
                .is_empty()
        );
    }

    #[test]
    fn preflight_helpers_name_outsiders_missing_acceptors_and_unvetted_pins() {
        let dec = CantonId::parse(&format!("cbtc::{NS}")).expect("id");
        let hosts: BTreeSet<String> = [participant(1), participant(2)]
            .iter()
            .map(ToString::to_string)
            .collect();
        let outsiders = participants_not_hosting(&[participant(2), participant(3)], &hosts, &dec);
        assert_eq!(outsiders.len(), 1);
        assert_eq!(outsiders[0].0, participant(3));
        assert!(outsiders[0].1.contains("does not host"));

        let invitees = [party("node-b"), party("node-c")];
        let b = party("node-b");
        let accepted: BTreeSet<&CantonId> = [&b].into_iter().collect();
        assert_eq!(
            missing_invitees(&invitees, &accepted),
            vec![party("node-c")]
        );

        // One absent operator must not hold up a party that has the quorum
        // to act: the proposer signs, so two of three reach a threshold of 2.
        assert!(signing_quorum_reached(1, 2));
        assert!(signing_quorum_reached(2, 3));
        assert!(!signing_quorum_reached(1, 3));
        assert!(!signing_quorum_reached(0, 2));
        // A single-signer party needs nobody but the proposer.
        assert!(signing_quorum_reached(0, 1));

        let pins = [
            DarPin {
                filename: "a.dar".into(),
                sha256_hex: "aa".into(),
                main_package_id: "pkg-a".into(),
                size_bytes: 1,
            },
            DarPin {
                filename: "b.dar".into(),
                sha256_hex: "bb".into(),
                main_package_id: "pkg-b".into(),
                size_bytes: 1,
            },
        ];
        let vetted: HashSet<String> = ["pkg-a".to_string()].into_iter().collect();
        assert_eq!(unvetted_pins(&pins, &vetted), vec!["b.dar".to_string()]);
        assert!(
            unvetted_pins(
                &pins,
                &["pkg-a".to_string(), "pkg-b".to_string()]
                    .into_iter()
                    .collect()
            )
            .is_empty()
        );
    }

    #[test]
    fn round_plan_finds_missing_expired_and_duplicate_rounds() {
        let now = 1_700_000_000_000_000 + 3600 * 1_000_000;
        let live = now + MAX_RECORD_TIME_HORIZON_MICROS - DEADLINE_SAFETY_MARGIN_MICROS;
        let rounds = [
            round("r0", 0, 10, live),
            round("r0-old", 0, 5, now - 1),
            round("r2", 2, 20, live),
            round("r2-dup", 2, 30, live),
        ];

        let none = BTreeSet::new();
        let plan = plan_rounds(&rounds, 4, &none, now);
        assert_eq!(plan.missing, vec![1, 3]);
        assert!(plan.done.is_empty());
        assert_eq!(
            plan.expired
                .iter()
                .map(|r| r.contract_id.as_str())
                .collect::<Vec<_>>(),
            vec!["r0-old"]
        );
        assert_eq!(
            plan.duplicates
                .iter()
                .map(|r| r.contract_id.as_str())
                .collect::<Vec<_>>(),
            vec!["r2-dup"]
        );
        assert_eq!(
            plan.live
                .iter()
                .map(|r| r.contract_id.as_str())
                .collect::<Vec<_>>(),
            vec!["r0", "r2"]
        );

        // The earlier round wins regardless of read order.
        let reversed = [round("r2-dup", 2, 30, live), round("r2", 2, 20, live)];
        let plan = plan_rounds(&reversed, 3, &none, now);
        assert_eq!(plan.duplicates[0].contract_id, "r2-dup");
        assert_eq!(plan.live[0].contract_id, "r2");
        assert_eq!(plan.missing, vec![0, 1]);

        // Nothing on the ledger yet: every index is missing.
        let plan = plan_rounds(&[], 2, &none, now);
        assert_eq!(plan.missing, vec![0, 1]);
        assert!(plan.live.is_empty());
    }

    /// An index whose transaction is on the ledger is never missing again,
    /// and a round of that index only needs its close.
    #[test]
    fn round_plan_never_reprepares_an_executed_index() {
        let now = 1_700_000_000_000_000 + 3600 * 1_000_000;
        let live = now + MAX_RECORD_TIME_HORIZON_MICROS - DEADLINE_SAFETY_MARGIN_MICROS;
        let executed: BTreeSet<i64> = [0].into_iter().collect();

        // Round 0 executed but its close did not land; round 1 expired.
        let rounds = [round("r0", 0, 10, live), round("r1", 1, 20, now - 1)];
        let plan = plan_rounds(&rounds, 2, &executed, now);
        assert_eq!(
            plan.done
                .iter()
                .map(|r| r.contract_id.as_str())
                .collect::<Vec<_>>(),
            vec!["r0"]
        );
        assert_eq!(plan.expired[0].contract_id, "r1");
        assert_eq!(plan.missing, vec![1]);
        assert!(plan.live.is_empty());

        // Round 0 closed after execution, round 1 still live: nothing missing.
        let rounds = [round("r1", 1, 20, live)];
        let plan = plan_rounds(&rounds, 2, &executed, now);
        assert!(plan.missing.is_empty());
        assert_eq!(plan.live[0].contract_id, "r1");

        // Everything executed and closed: the plan is empty.
        let all: BTreeSet<i64> = [0, 1].into_iter().collect();
        let plan = plan_rounds(&[], 2, &all, now);
        assert!(plan.missing.is_empty() && plan.live.is_empty() && plan.done.is_empty());
    }

    #[test]
    fn a_member_completes_only_after_it_pinned_a_round() {
        let mut meta = RunMeta {
            proposal_cid: "00p".into(),
            coordinator_party: party("node-a"),
            coordinator_participant: participant(1),
            member_variant: None,
            topology_hashes: BTreeMap::new(),
        };
        assert!(!has_pinned_round(&meta));
        meta.topology_hashes.insert("dnd".into(), "1220aa".into());
        assert!(!has_pinned_round(&meta));
        meta.topology_hashes
            .insert(format!("{ROUND_PIN_PREFIX}00r"), "1220bb".into());
        assert!(has_pinned_round(&meta));
    }
}
