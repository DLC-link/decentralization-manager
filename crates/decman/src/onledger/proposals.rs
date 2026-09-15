//! `WorkflowProposal` lifecycle (design D6, D10) and its projection into
//! `pending_invitations` (design D11).
//!
//! The proposer creates, cancels, and finishes. An invitee accepts or
//! declines. Everyone reads. The counting predicates that decide which
//! acceptances a proposer trusts live in [`counted_acceptances`], a pure
//! function, so they are unit-tested without a ledger.

use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::{Context, Result, bail};
use common::{
    canton_id::CantonId,
    types::{InvitationType, PendingInvitation, WorkflowKind},
};
use sqlx::SqlitePool;

use crate::{
    config::{NodeConfig, Peer},
    consts,
    db::{
        rows::ProposalDecisionEntry,
        schema::{Commitable, SchemaRead, SchemaWrite},
    },
};

use super::{
    daml::{
        ActiveContract, CoordinationClient, CoordinationTemplate, choices,
        codec::{
            AcceptArgs, ChoiceArgument, DeclineArgs, FinishArgs, WorkflowAcceptanceRecord,
            WorkflowDeclineRecord, WorkflowOutcomeRecord, WorkflowProposalRecord, unit_argument,
        },
    },
    identity::verify_hosting,
};

pub type ActiveProposal = ActiveContract<WorkflowProposalRecord>;
pub type Acceptance = ActiveContract<WorkflowAcceptanceRecord>;
pub type Decline = ActiveContract<WorkflowDeclineRecord>;
pub type Outcome = ActiveContract<WorkflowOutcomeRecord>;

const MICROS_PER_SEC: i64 = 1_000_000;

/// `(createdAt, expiresAt)` for a proposal created at `now` (micros), using
/// the configured TTL. The template requires `expiresAt > createdAt`, which
/// the TTL floor of one second guarantees.
pub fn proposal_lifetime(now: i64) -> (i64, i64) {
    let ttl = i64::try_from(consts::proposal_ttl_secs())
        .unwrap_or(i64::MAX)
        .saturating_mul(MICROS_PER_SEC);
    (now, now.saturating_add(ttl))
}

// ---------------------------------------------------------------------------
// Proposer side
// ---------------------------------------------------------------------------

/// Create a `WorkflowProposal` and return its contract id.
///
/// # Errors
/// Returns an error when the record's proposer is not the node party, or the
/// submission fails.
pub async fn create_proposal(
    client: &CoordinationClient,
    record: &WorkflowProposalRecord,
) -> Result<String> {
    if record.proposer != *client.node_party() {
        bail!(
            "proposal names proposer {} but the client acts as {}",
            record.proposer,
            client.node_party()
        );
    }
    if record.expires_at <= record.created_at {
        bail!("proposal expiresAt must be after createdAt");
    }
    let cid = client
        .create(record)
        .await
        .context("create WorkflowProposal")?;
    tracing::info!(
        contract_id = %cid,
        run_id = %record.run_id,
        kind = %record.kind,
        invitees = record.invitees.len(),
        "created WorkflowProposal"
    );
    Ok(cid)
}

/// Exercise `WorkflowProposal_Cancel`. The proposal vanishes for every
/// invitee, so they stop co-signing (design D10).
///
/// # Errors
/// Returns an error when the submission fails.
pub async fn cancel(client: &CoordinationClient, proposal_cid: &str) -> Result<()> {
    client
        .exercise(
            CoordinationTemplate::WorkflowProposal,
            proposal_cid,
            choices::WORKFLOW_PROPOSAL_CANCEL,
            unit_argument(),
        )
        .await
        .context("WorkflowProposal_Cancel")?;
    Ok(())
}

/// Exercise `WorkflowProposal_Finish` and return the `WorkflowOutcome` cid.
///
/// # Errors
/// Returns an error when the submission fails or created no outcome.
pub async fn finish(
    client: &CoordinationClient,
    proposal_cid: &str,
    succeeded: bool,
    error: Option<String>,
) -> Result<String> {
    let args = FinishArgs { succeeded, error };
    let outcome = client
        .exercise(
            CoordinationTemplate::WorkflowProposal,
            proposal_cid,
            choices::WORKFLOW_PROPOSAL_FINISH,
            args.to_value(),
        )
        .await
        .context("WorkflowProposal_Finish")?;
    outcome
        .created_contract_id
        .context("WorkflowProposal_Finish created no WorkflowOutcome")
}

// ---------------------------------------------------------------------------
// Invitee side
// ---------------------------------------------------------------------------

/// Exercise `WorkflowProposal_Accept` and return the `WorkflowAcceptance` cid.
///
/// # Errors
/// Returns an error when `args.acceptor` is not the node party or the
/// submission fails.
pub async fn accept(
    client: &CoordinationClient,
    proposal_cid: &str,
    args: &AcceptArgs,
) -> Result<String> {
    if args.acceptor != *client.node_party() {
        bail!(
            "acceptance names acceptor {} but the client acts as {}",
            args.acceptor,
            client.node_party()
        );
    }
    let outcome = client
        .exercise(
            CoordinationTemplate::WorkflowProposal,
            proposal_cid,
            choices::WORKFLOW_PROPOSAL_ACCEPT,
            args.to_value(),
        )
        .await
        .context("WorkflowProposal_Accept")?;
    outcome
        .created_contract_id
        .context("WorkflowProposal_Accept created no WorkflowAcceptance")
}

/// Exercise `WorkflowProposal_Decline` as the node party and return the
/// `WorkflowDecline` cid.
///
/// # Errors
/// Returns an error when the submission fails.
pub async fn decline(
    client: &CoordinationClient,
    proposal_cid: &str,
    reason: &str,
) -> Result<String> {
    let args = DeclineArgs {
        decliner: client.node_party().clone(),
        reason: reason.to_string(),
    };
    let outcome = client
        .exercise(
            CoordinationTemplate::WorkflowProposal,
            proposal_cid,
            choices::WORKFLOW_PROPOSAL_DECLINE,
            args.to_value(),
        )
        .await
        .context("WorkflowProposal_Decline")?;
    outcome
        .created_contract_id
        .context("WorkflowProposal_Decline created no WorkflowDecline")
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// Active proposals that invite this node: the node party is an invitee and
/// not the proposer.
///
/// # Errors
/// Returns an error when the read fails.
pub async fn read_active_proposals_for_me(
    client: &CoordinationClient,
) -> Result<Vec<ActiveProposal>> {
    let me = client.node_party();
    Ok(client
        .list_active::<WorkflowProposalRecord>()
        .await?
        .into_iter()
        .filter(|p| p.record.proposer != *me && p.record.invitees.contains(me))
        .collect())
}

/// Active proposals this node created.
///
/// # Errors
/// Returns an error when the read fails.
pub async fn read_my_proposals(client: &CoordinationClient) -> Result<Vec<ActiveProposal>> {
    let me = client.node_party();
    Ok(client
        .list_active::<WorkflowProposalRecord>()
        .await?
        .into_iter()
        .filter(|p| p.record.proposer == *me)
        .collect())
}

/// One proposal by contract id, if it is still active and visible. This is
/// the re-read design D5 step 5 requires immediately before a co-sign.
///
/// # Errors
/// Returns an error when the read fails.
pub async fn read_proposal(
    client: &CoordinationClient,
    proposal_cid: &str,
) -> Result<Option<ActiveProposal>> {
    Ok(client
        .list_active::<WorkflowProposalRecord>()
        .await?
        .into_iter()
        .find(|p| p.contract_id == proposal_cid))
}

/// Every visible `WorkflowAcceptance` that names `proposal_cid`. Raw: run
/// [`counted_acceptances`] before trusting any of them.
///
/// # Errors
/// Returns an error when the read fails.
pub async fn read_acceptances(
    client: &CoordinationClient,
    proposal_cid: &str,
) -> Result<Vec<Acceptance>> {
    Ok(client
        .list_active::<WorkflowAcceptanceRecord>()
        .await?
        .into_iter()
        .filter(|a| a.record.proposal == proposal_cid)
        .collect())
}

/// Every visible `WorkflowDecline` that names `proposal_cid`.
///
/// # Errors
/// Returns an error when the read fails.
pub async fn read_declines(
    client: &CoordinationClient,
    proposal_cid: &str,
) -> Result<Vec<Decline>> {
    Ok(client
        .list_active::<WorkflowDeclineRecord>()
        .await?
        .into_iter()
        .filter(|d| d.record.proposal == proposal_cid)
        .collect())
}

/// Every visible `WorkflowOutcome`.
///
/// # Errors
/// Returns an error when the read fails.
pub async fn read_outcomes(client: &CoordinationClient) -> Result<Vec<Outcome>> {
    client.list_active::<WorkflowOutcomeRecord>().await
}

// ---------------------------------------------------------------------------
// Counting (design D6)
// ---------------------------------------------------------------------------

/// Apply the D6 counting predicates.
///
/// An acceptance is counted only if all of these hold:
/// * `acceptance.proposal == proposal.contract_id`;
/// * `acceptance.proposer == proposal.proposer`;
/// * the acceptor is an invitee and not the proposer;
/// * `acceptance.participantId` is one of `proposal.participants`;
/// * `hosted(acceptor, participantId)` says the acceptor's node party is
///   hosted on that participant with Submission permission.
///
/// A second counted acceptance from the same acceptor is an error: the run
/// fails closed rather than pick one set of key material. The result is
/// sorted by acceptor.
///
/// `hosted` is injected so this stays pure; [`counted_acceptances_verified`]
/// supplies the topology-backed check.
///
/// # Errors
/// Returns an error on a duplicate acceptor.
pub fn counted_acceptances(
    proposal: &ActiveProposal,
    acceptances: &[Acceptance],
    hosted: &dyn Fn(&CantonId, &CantonId) -> bool,
) -> Result<Vec<Acceptance>> {
    let invitees: HashSet<&CantonId> = proposal.record.invitees.iter().collect();
    let participants: HashSet<&str> = proposal
        .record
        .participants
        .iter()
        .map(String::as_str)
        .collect();
    let mut seen: BTreeSet<CantonId> = BTreeSet::new();
    let mut counted = Vec::new();

    for a in acceptances {
        let r = &a.record;
        let skip = |why: &str| {
            tracing::debug!(
                acceptance = %a.contract_id,
                acceptor = %r.acceptor,
                proposal = %proposal.contract_id,
                "acceptance not counted: {why}"
            );
        };
        if r.proposal != proposal.contract_id {
            skip("names another proposal");
            continue;
        }
        if r.proposer != proposal.record.proposer {
            skip("names another proposer");
            continue;
        }
        if r.acceptor == proposal.record.proposer {
            skip("the proposer accepted its own proposal");
            continue;
        }
        if !invitees.contains(&r.acceptor) {
            skip("acceptor is not an invitee");
            continue;
        }
        if !participants.contains(r.participant_id.as_str()) {
            skip("participantId is not in the proposal's participants");
            continue;
        }
        let Ok(participant) = CantonId::parse(&r.participant_id) else {
            skip("participantId is not a Canton id");
            continue;
        };
        if !hosted(&r.acceptor, &participant) {
            skip("acceptor is not hosted on participantId with Submission");
            continue;
        }
        if !seen.insert(r.acceptor.clone()) {
            bail!(
                "acceptor {} accepted proposal {} more than once; refusing to pick between \
                 conflicting acceptances",
                r.acceptor,
                proposal.contract_id
            );
        }
        counted.push(a.clone());
    }

    counted.sort_by(|x, y| x.record.acceptor.cmp(&y.record.acceptor));
    Ok(counted)
}

/// [`counted_acceptances`] with the hosting predicate backed by the
/// synchronizer topology (one `ListPartyToParticipant` per distinct
/// `(acceptor, participant)` pair). A failed topology read counts as not
/// hosted for this call.
///
/// # Errors
/// As [`counted_acceptances`].
pub async fn counted_acceptances_verified(
    config: &NodeConfig,
    proposal: &ActiveProposal,
    acceptances: &[Acceptance],
) -> Result<Vec<Acceptance>> {
    let mut checks: HashMap<(CantonId, CantonId), bool> = HashMap::new();
    for a in acceptances {
        let Ok(participant) = CantonId::parse(&a.record.participant_id) else {
            continue;
        };
        let key = (a.record.acceptor.clone(), participant);
        if checks.contains_key(&key) {
            continue;
        }
        let ok = match verify_hosting(config, &key.0, &key.1).await {
            Ok(check) => check.has_submission(),
            Err(e) => {
                tracing::warn!(
                    acceptor = %key.0,
                    participant = %key.1,
                    error = %e,
                    "hosting check failed; acceptance not counted this tick"
                );
                false
            }
        };
        checks.insert(key, ok);
    }
    counted_acceptances(proposal, acceptances, &|acceptor, participant| {
        checks
            .get(&(acceptor.clone(), participant.clone()))
            .copied()
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// Projection into pending_invitations (design D11)
// ---------------------------------------------------------------------------

fn invitation_type_of(kind: WorkflowKind) -> InvitationType {
    match kind {
        WorkflowKind::Onboarding => InvitationType::Onboarding,
        WorkflowKind::Kick => InvitationType::Kick,
        WorkflowKind::Contracts => InvitationType::Contracts,
        WorkflowKind::Dars => InvitationType::Dars,
        WorkflowKind::AddParty => InvitationType::AddParty,
        WorkflowKind::ChangeThreshold => InvitationType::ChangeThreshold,
    }
}

/// The `PendingInvitation` card for one proposal. `id` and `proposal_cid`
/// are the proposal contract id.
pub fn invitation_from_proposal(
    proposal: &ActiveProposal,
    peers: &[Peer],
    received_at: i64,
) -> PendingInvitation {
    let r = &proposal.record;
    let coordinator_name = peers
        .iter()
        .find(|p| p.participant_id.to_string() == r.proposer_participant)
        .map(|p| p.name.clone());
    let parse_all = |items: &[String]| -> Vec<CantonId> {
        items
            .iter()
            .filter_map(|s| CantonId::parse(s).ok())
            .collect()
    };
    let parse_opt = |item: &Option<String>| item.as_deref().and_then(|s| CantonId::parse(s).ok());
    let to_i32 = |v: Option<i64>| v.and_then(|n| i32::try_from(n).ok());
    PendingInvitation {
        id: proposal.contract_id.clone(),
        invitation_type: invitation_type_of(r.kind),
        coordinator_participant: r.proposer_participant.clone(),
        coordinator_party: Some(r.proposer.clone()),
        proposal_cid: proposal.contract_id.clone(),
        coordinator_name,
        received_at,
        expires_at: Some(r.expires_at.div_euclid(MICROS_PER_SEC)),
        prefix: r.prefix.clone(),
        participants: parse_all(&r.participants),
        dar_filenames: r.dar_pins.iter().map(|p| p.filename.clone()).collect(),
        dar_hashes: r.dar_pins.iter().map(|p| p.sha256_hex.clone()).collect(),
        kicked_participant: parse_opt(&r.kicked_participant),
        new_threshold: to_i32(r.threshold),
        previous_threshold: to_i32(r.previous_threshold),
        dec_party_id: parse_opt(&r.dec_party_id),
        new_participant: parse_opt(&r.new_participant),
        package_names: r.package_names.clone(),
        workflow_instance: Some(r.run_id.clone()),
    }
}

/// Which rows [`project_pending_invitations`] writes and removes (pure).
///
/// A proposal is projected when it invites this node, has no decision, and
/// has not expired. A row whose proposal is no longer projected is removed.
pub fn plan_projection(
    proposals: &[ActiveProposal],
    decisions: &[ProposalDecisionEntry],
    existing: &[PendingInvitation],
    peers: &[Peer],
    now_micros: i64,
) -> (Vec<PendingInvitation>, Vec<String>) {
    let decided: HashSet<&str> = decisions.iter().map(|d| d.proposal_cid.as_str()).collect();
    let received_at = now_micros.div_euclid(MICROS_PER_SEC);
    let keep: Vec<PendingInvitation> = proposals
        .iter()
        .filter(|p| !decided.contains(p.contract_id.as_str()))
        .filter(|p| p.record.expires_at > now_micros)
        .map(|p| {
            // Keep the first-seen time when the row already exists.
            let received_at = existing
                .iter()
                .find(|e| e.id == p.contract_id)
                .map(|e| e.received_at)
                .unwrap_or(received_at);
            invitation_from_proposal(p, peers, received_at)
        })
        .collect();
    let live: HashSet<&str> = keep.iter().map(|i| i.id.as_str()).collect();
    let remove: Vec<String> = existing
        .iter()
        .filter(|e| !live.contains(e.id.as_str()))
        .map(|e| e.id.clone())
        .collect();
    (keep, remove)
}

/// Write the `pending_invitations` rows for the undecided proposals that
/// invite this node and remove the on-ledger rows that no longer apply.
/// Returns the full table afterwards, so the caller can replace the in-memory
/// `AppState.pending_invitations` list with it.
///
/// # Errors
/// Returns an error when a database read or write fails.
pub async fn project_pending_invitations(
    db: &SqlitePool,
    proposals: &[ActiveProposal],
    decisions: &[ProposalDecisionEntry],
    peers: &[Peer],
    now_micros: i64,
) -> Result<Vec<PendingInvitation>> {
    let existing = db.get_all_pending_invitations().await?;
    let (keep, remove) = plan_projection(proposals, decisions, &existing, peers, now_micros);

    let mut tx = db.begin_transaction().await?;
    for inv in &keep {
        tx.upsert_pending_invitation(inv).await?;
    }
    for id in &remove {
        tx.delete_pending_invitation(id).await?;
    }
    Commitable::commit(tx).await?;

    if !keep.is_empty() || !remove.is_empty() {
        tracing::debug!(
            projected = keep.len(),
            removed = remove.len(),
            "projected WorkflowProposals into pending_invitations"
        );
    }
    db.get_all_pending_invitations().await
}

// ---------------------------------------------------------------------------
// Housekeeping (design D10)
// ---------------------------------------------------------------------------

/// What one sweep archived.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArchiveSweepReport {
    pub acceptances: usize,
    pub declines: usize,
}

/// Archive this node's own `WorkflowAcceptance` and `WorkflowDecline`
/// contracts whose proposal is no longer active. A finished or cancelled
/// proposal is consumed, so "not in `active_proposal_cids`" is the test.
///
/// TODO(submission.rs): archive this node's `SubmissionSignature`s of closed
/// rounds here too once the contracts workflow lands (part 2).
///
/// # Errors
/// Returns an error when a read fails. A failed archive is logged and the
/// sweep continues; the next tick retries it.
pub async fn archive_sweep(
    client: &CoordinationClient,
    active_proposal_cids: &HashSet<String>,
) -> Result<ArchiveSweepReport> {
    let me = client.node_party();
    let mut report = ArchiveSweepReport::default();

    let acceptances = client.list_active::<WorkflowAcceptanceRecord>().await?;
    for a in acceptances
        .iter()
        .filter(|a| a.record.acceptor == *me && !active_proposal_cids.contains(&a.record.proposal))
    {
        match client
            .exercise(
                CoordinationTemplate::WorkflowAcceptance,
                &a.contract_id,
                choices::WORKFLOW_ACCEPTANCE_ARCHIVE,
                unit_argument(),
            )
            .await
        {
            Ok(_) => report.acceptances += 1,
            Err(e) => tracing::warn!(
                contract_id = %a.contract_id,
                error = %e,
                "archiving a stale WorkflowAcceptance failed; retrying next tick"
            ),
        }
    }

    let declines = client.list_active::<WorkflowDeclineRecord>().await?;
    for d in declines
        .iter()
        .filter(|d| d.record.decliner == *me && !active_proposal_cids.contains(&d.record.proposal))
    {
        match client
            .exercise(
                CoordinationTemplate::WorkflowDecline,
                &d.contract_id,
                choices::WORKFLOW_DECLINE_ARCHIVE,
                unit_argument(),
            )
            .await
        {
            Ok(_) => report.declines += 1,
            Err(e) => tracing::warn!(
                contract_id = %d.contract_id,
                error = %e,
                "archiving a stale WorkflowDecline failed; retrying next tick"
            ),
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::rows::ProposalDecision,
        onledger::daml::codec::tests::{acceptance_full, party, proposal_full},
    };

    fn proposal() -> ActiveProposal {
        ActiveContract {
            contract_id: "00proposal".into(),
            offset: 10,
            record: proposal_full(),
        }
    }

    fn acceptance(acceptor: &str, participant_n: u8) -> Acceptance {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        ActiveContract {
            contract_id: format!("00acc-{acceptor}-{participant_n}"),
            offset: 20,
            record: WorkflowAcceptanceRecord {
                participant_id: format!("participant{participant_n}::{ns}"),
                ..acceptance_full("00proposal", acceptor)
            },
        }
    }

    fn all_hosted(_: &CantonId, _: &CantonId) -> bool {
        true
    }

    #[test]
    fn counts_a_valid_acceptance_from_each_invitee() {
        let counted = counted_acceptances(
            &proposal(),
            &[acceptance("node-c", 3), acceptance("node-b", 2)],
            &all_hosted,
        )
        .expect("counts");
        let acceptors: Vec<_> = counted.iter().map(|a| a.record.acceptor.clone()).collect();
        assert_eq!(acceptors, vec![party("node-b"), party("node-c")]);
    }

    #[test]
    fn a_foreign_proposal_cid_is_not_counted() {
        let mut foreign = acceptance("node-b", 2);
        foreign.record.proposal = "00other".into();
        let counted = counted_acceptances(&proposal(), &[foreign], &all_hosted).expect("ok");
        assert!(counted.is_empty());
    }

    #[test]
    fn a_foreign_proposer_is_not_counted() {
        let mut wrong = acceptance("node-b", 2);
        wrong.record.proposer = party("node-z");
        let counted = counted_acceptances(&proposal(), &[wrong], &all_hosted).expect("ok");
        assert!(counted.is_empty());
    }

    #[test]
    fn an_acceptor_outside_the_invitees_is_not_counted() {
        let counted =
            counted_acceptances(&proposal(), &[acceptance("node-z", 2)], &all_hosted).expect("ok");
        assert!(counted.is_empty());
    }

    #[test]
    fn a_participant_outside_the_proposal_is_not_counted() {
        let counted =
            counted_acceptances(&proposal(), &[acceptance("node-b", 9)], &all_hosted).expect("ok");
        assert!(counted.is_empty());
    }

    #[test]
    fn an_unhosted_acceptor_is_not_counted() {
        let counted = counted_acceptances(&proposal(), &[acceptance("node-b", 2)], &|_, _| false)
            .expect("ok");
        assert!(counted.is_empty());
    }

    #[test]
    fn the_proposers_own_acceptance_is_not_counted() {
        // Even if the proposer somehow lists itself as invitee.
        let mut p = proposal();
        p.record.invitees.push(party("node-a"));
        let counted = counted_acceptances(&p, &[acceptance("node-a", 1)], &all_hosted).expect("ok");
        assert!(counted.is_empty());
    }

    #[test]
    fn a_duplicate_acceptor_fails_closed() {
        let err = counted_acceptances(
            &proposal(),
            &[acceptance("node-b", 2), acceptance("node-b", 3)],
            &all_hosted,
        )
        .expect_err("duplicate");
        assert!(err.to_string().contains("more than once"), "{err}");
    }

    #[test]
    fn a_duplicate_that_fails_an_earlier_predicate_is_not_a_conflict() {
        let mut foreign = acceptance("node-b", 2);
        foreign.record.proposal = "00other".into();
        let counted = counted_acceptances(
            &proposal(),
            &[foreign, acceptance("node-b", 2)],
            &all_hosted,
        )
        .expect("ok");
        assert_eq!(counted.len(), 1);
    }

    #[test]
    fn proposal_lifetime_expires_after_created() {
        let (created, expires) = proposal_lifetime(1_000);
        assert_eq!(created, 1_000);
        assert!(expires > created);
    }

    fn peer_row(n: u8, name: &str) -> Peer {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        Peer {
            participant_id: CantonId::parse(&format!("participant{n}::{ns}")).expect("id"),
            name: name.into(),
            party: None,
        }
    }

    #[test]
    fn invitation_mirrors_the_proposal_fields() {
        let inv = invitation_from_proposal(&proposal(), &[peer_row(1, "Alpha")], 77);
        let r = proposal_full();
        assert_eq!(inv.id, "00proposal");
        assert_eq!(inv.proposal_cid, "00proposal");
        assert_eq!(inv.invitation_type, InvitationType::Onboarding);
        assert_eq!(inv.coordinator_participant, r.proposer_participant);
        assert_eq!(inv.coordinator_party.as_ref(), Some(&r.proposer));
        assert_eq!(inv.coordinator_name.as_deref(), Some("Alpha"));
        assert_eq!(inv.received_at, 77);
        assert_eq!(
            inv.expires_at,
            Some(r.expires_at.div_euclid(MICROS_PER_SEC))
        );
        assert_eq!(inv.prefix, r.prefix);
        assert_eq!(inv.participants.len(), 3);
        assert_eq!(inv.dar_filenames, vec!["governance-core-v1-0.1.0.dar"]);
        assert_eq!(inv.dar_hashes, vec!["deadbeef"]);
        assert_eq!(inv.new_threshold, Some(2));
        assert_eq!(inv.previous_threshold, Some(3));
        assert!(inv.dec_party_id.is_some());
        assert!(inv.kicked_participant.is_some());
        assert!(inv.new_participant.is_some());
        assert_eq!(inv.package_names, vec!["governance-core-v1"]);
        assert_eq!(inv.workflow_instance.as_deref(), Some("cbtc-creation"));
    }

    #[test]
    fn projection_skips_decided_and_expired_and_removes_stale_rows() {
        let live = proposal();
        let mut decided = proposal();
        decided.contract_id = "00decided".into();
        let mut expired = proposal();
        expired.contract_id = "00expired".into();
        expired.record.expires_at = 5;
        let decisions = vec![ProposalDecisionEntry {
            proposal_cid: "00decided".into(),
            decision: ProposalDecision::Accepted,
            decided_at: 1,
            pinned_hashes: vec![],
        }];
        let stale_onledger = invitation_from_proposal(
            &ActiveContract {
                contract_id: "00gone".into(),
                ..proposal()
            },
            &[],
            3,
        );
        let earlier_live = invitation_from_proposal(&live, &[], 3);

        let (keep, remove) = plan_projection(
            &[live, decided, expired],
            &decisions,
            &[stale_onledger, earlier_live],
            &[],
            1_000 * MICROS_PER_SEC,
        );

        assert_eq!(keep.len(), 1);
        assert_eq!(keep[0].id, "00proposal");
        assert_eq!(keep[0].received_at, 3, "first-seen time is kept");
        assert_eq!(remove, vec!["00gone".to_string()]);
    }
}
