//! Contracts workflow over `SubmissionRound` (design D7).
//!
//! The proposer prepares one interactive submission per contract, opens a
//! `SubmissionRound` per prepared transaction, collects and verifies
//! `SubmissionSignature`s, and executes each round once with exactly the
//! verified set. A member checks the round against its accepted proposal and
//! the head P2P, recomputes the hash, and signs with its per-party key.
//!
//! The signatures below are the contract. Pure helpers and plain ACS reads
//! are implemented; every ledger write and every crypto step is a stub the
//! contracts agent fills in.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use canton_proto_rs::com::digitalasset::canton::protocol::v30::PartyToParticipant;
use common::{api::ContractDefinition, canton_id::CantonId, types::WorkflowRun};

use crate::config::NodeConfig;

use super::{
    OnLedger,
    daml::{
        ActiveContract, CoordinationClient, CoordinationTemplate, choices,
        codec::{
            ChoiceArgument, CloseArgs, SubmissionRoundRecord, SubmissionSignatureRecord,
            WorkflowProposalRecord, unit_argument,
        },
    },
};

/// `max_record_time = preparation time + 20 h`.
pub const MAX_RECORD_TIME_HORIZON_MICROS: i64 = 20 * 3600 * 1_000_000;

/// Signers must finish 30 minutes before the window closes.
pub const DEADLINE_SAFETY_MARGIN_MICROS: i64 = 30 * 60 * 1_000_000;

/// One prepared interactive submission, ready to become a `SubmissionRound`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedRound {
    pub index: i64,
    pub description: String,
    /// Lowercase hex of the serialized `PreparedTransaction`.
    pub prepared_transaction_hex: String,
    /// Lowercase hex of `prepared_transaction_hash`.
    pub prepared_hash_hex: String,
    pub hashing_scheme_version: i64,
    /// Micros since the epoch.
    pub preparation_time: i64,
    pub max_record_time: i64,
    pub deadline: i64,
}

/// A `SubmissionSignature` the proposer verified locally.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedSignature {
    pub signed_by: String,
    pub signature: Vec<u8>,
    pub format: String,
    pub algorithm: String,
    pub participant_id: String,
}

/// `deadline = min(max_record_time, preparation_time + tolerance) - 30 min`.
pub fn deadline_for(preparation_time: i64, max_record_time: i64, tolerance_micros: i64) -> i64 {
    max_record_time
        .min(preparation_time.saturating_add(tolerance_micros))
        .saturating_sub(DEADLINE_SAFETY_MARGIN_MICROS)
}

/// The live `preparationTimeRecordTimeTolerance` of the synchronizer, in
/// micros.
///
/// # Errors
/// Returns an error when the parameters cannot be read.
pub async fn read_record_time_tolerance(
    _config: &NodeConfig,
    _synchronizer_id: &str,
) -> Result<i64> {
    bail!("not implemented: read preparationTimeRecordTimeTolerance (design D7)")
}

/// `PrepareSubmission` once per contract definition as the party, with
/// `max_record_time = now + 20 h`, and compute each round's deadline.
///
/// # Errors
/// Returns an error when a prepare call fails or the tolerance is unknown.
pub async fn prepare_rounds(
    _ol: &OnLedger,
    _run: &WorkflowRun,
    _dec_party_id: &CantonId,
    _contracts: &[ContractDefinition],
) -> Result<Vec<PreparedRound>> {
    bail!("not implemented: PrepareSubmission per contract (design D7)")
}

/// Create one `SubmissionRound` per prepared transaction, observed by
/// `signers`, and return the contract ids.
///
/// # Errors
/// Returns an error when a create fails.
pub async fn open_rounds(
    _client: &CoordinationClient,
    _run_id: &str,
    _signers: &[CantonId],
    _dec_party_id: &CantonId,
    _act_as: &CantonId,
    _rounds: &[PreparedRound],
) -> Result<Vec<String>> {
    bail!("not implemented: create SubmissionRound contracts (design D7)")
}

/// Every active `SubmissionRound` of one run visible to this node.
///
/// # Errors
/// Returns an error when the read fails.
pub async fn read_rounds_for_run(
    client: &CoordinationClient,
    run_id: &str,
) -> Result<Vec<ActiveContract<SubmissionRoundRecord>>> {
    Ok(client
        .list_active::<SubmissionRoundRecord>()
        .await?
        .into_iter()
        .filter(|r| r.record.run_id == run_id)
        .collect())
}

/// Every active `SubmissionSignature` of one round.
///
/// # Errors
/// Returns an error when the read fails.
pub async fn read_signatures_for_round(
    client: &CoordinationClient,
    round_cid: &str,
) -> Result<Vec<ActiveContract<SubmissionSignatureRecord>>> {
    Ok(client
        .list_active::<SubmissionSignatureRecord>()
        .await?
        .into_iter()
        .filter(|s| s.record.round == round_cid)
        .collect())
}

/// Proposer side: `signedBy` is one of the head P2P's party signing keys and
/// the signature verifies against that key over `hash` (Ed25519 CONCAT,
/// ECDSA DER).
///
/// # Errors
/// Returns an error when the key is unknown or the signature does not
/// verify.
pub fn verify_signature(
    _head_p2p: &PartyToParticipant,
    _signature: &SubmissionSignatureRecord,
    _hash: &[u8],
) -> Result<VerifiedSignature> {
    bail!("not implemented: verify a SubmissionSignature locally (design D7)")
}

/// One verified signature per fingerprint; the first wins.
pub fn dedupe_verified(
    signatures: impl IntoIterator<Item = VerifiedSignature>,
) -> BTreeMap<String, VerifiedSignature> {
    let mut out = BTreeMap::new();
    for s in signatures {
        out.entry(s.signed_by.clone()).or_insert(s);
    }
    out
}

/// `ExecuteSubmissionAndWait` with exactly the verified set, once the count
/// reaches `party_signing_keys.threshold`. Returns the update id.
///
/// # Errors
/// Returns an error when the execute call fails or the round expired.
pub async fn execute_round(
    _ol: &OnLedger,
    _dec_party_id: &CantonId,
    _round: &ActiveContract<SubmissionRoundRecord>,
    _signatures: &BTreeMap<String, VerifiedSignature>,
) -> Result<String> {
    bail!("not implemented: ExecuteSubmissionAndWait (design D7)")
}

/// Exercise `SubmissionRound_Close` with a result text.
///
/// # Errors
/// Returns an error when the submission fails.
pub async fn close_round(client: &CoordinationClient, round_cid: &str, result: &str) -> Result<()> {
    let args = CloseArgs {
        result: result.to_string(),
    };
    client
        .exercise(
            CoordinationTemplate::SubmissionRound,
            round_cid,
            choices::SUBMISSION_ROUND_CLOSE,
            args.to_value(),
        )
        .await
        .context("SubmissionRound_Close")?;
    Ok(())
}

/// Member side (design D7): the round names the accepted party, `act_as` is
/// that party, `maxRecordTime` is present, `deadline > now`, this node's key
/// is among `party_signing_keys`, the hash recomputes with `canton_hash`,
/// and every root node is a `Create` in one of the accepted package names.
///
/// # Errors
/// Returns an error naming the first rule that fails.
pub fn check_round(
    _round: &SubmissionRoundRecord,
    _accepted: &WorkflowProposalRecord,
    _head_p2p: &PartyToParticipant,
    _own_key_fingerprint: &str,
    _now_micros: i64,
) -> Result<()> {
    bail!("not implemented: member checks of a SubmissionRound (design D7)")
}

/// Sign the round's hash with this node's per-party key (vault export or
/// KMS) and exercise `SubmissionRound_Sign`. Returns the signature cid.
///
/// # Errors
/// Returns an error when signing or the submission fails.
pub async fn sign_round(
    _ol: &OnLedger,
    _round: &ActiveContract<SubmissionRoundRecord>,
    _dec_party_id: &CantonId,
) -> Result<String> {
    bail!("not implemented: sign a SubmissionRound (design D7)")
}

/// Archive this node's `SubmissionSignature`s whose round is no longer
/// active (the housekeeping sweep of design D10).
///
/// # Errors
/// Returns an error when the read fails; a failed archive is logged.
pub async fn archive_own_signatures(
    client: &CoordinationClient,
    active_round_cids: &std::collections::HashSet<String>,
) -> Result<usize> {
    let me = client.node_party();
    let mut archived = 0;
    for s in client
        .list_active::<SubmissionSignatureRecord>()
        .await?
        .iter()
        .filter(|s| s.record.signer == *me && !active_round_cids.contains(&s.record.round))
    {
        match client
            .exercise(
                CoordinationTemplate::SubmissionSignature,
                &s.contract_id,
                choices::SUBMISSION_SIGNATURE_ARCHIVE,
                unit_argument(),
            )
            .await
        {
            Ok(_) => archived += 1,
            Err(e) => tracing::warn!(
                contract_id = %s.contract_id,
                error = %e,
                "archiving a stale SubmissionSignature failed; retrying next tick"
            ),
        }
    }
    Ok(archived)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_is_the_earlier_bound_minus_the_margin() {
        let prep = 1_000 * 1_000_000;
        let max_record = prep + MAX_RECORD_TIME_HORIZON_MICROS;
        // A 24 h tolerance: max_record_time is the earlier bound.
        let tolerance = 24 * 3600 * 1_000_000;
        assert_eq!(
            deadline_for(prep, max_record, tolerance),
            max_record - DEADLINE_SAFETY_MARGIN_MICROS
        );
        // A 1 h tolerance: preparation time + tolerance is the earlier bound.
        let tolerance = 3600 * 1_000_000;
        assert_eq!(
            deadline_for(prep, max_record, tolerance),
            prep + tolerance - DEADLINE_SAFETY_MARGIN_MICROS
        );
    }

    #[test]
    fn dedupe_keeps_the_first_signature_per_fingerprint() {
        let sig = |fp: &str, n: u8| VerifiedSignature {
            signed_by: fp.into(),
            signature: vec![n],
            format: "SIGNATURE_FORMAT_CONCAT".into(),
            algorithm: "SIGNING_ALGORITHM_SPEC_ED25519".into(),
            participant_id: "p".into(),
        };
        let deduped = dedupe_verified([sig("a", 1), sig("b", 2), sig("a", 3)]);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped["a"].signature, vec![1]);
    }
}
