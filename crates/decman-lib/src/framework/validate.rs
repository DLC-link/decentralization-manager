//! Protocol-constraint validators for `ActionType` / `ProposalType` payloads.
//!
//! These mirror the Daml `ensure`/`require` clauses on the corresponding
//! templates and choices. Catching a malformed value here means the caller
//! gets a precise `Error::Validation` rather than a generic failure after a
//! full propose/confirm round has already been spent submitting it to
//! Canton.

use std::collections::HashSet;

use canton_common::decimal::DamlDecimal;
use common::api::PartyCredentialRequirement;
use common::canton_id::CantonId;

use crate::catalog::types::{AppRewardBeneficiary, RewardBeneficiary};
use crate::error::Error;

pub fn validate_threshold(new_threshold: i64) -> Result<(), Error> {
    if new_threshold < 1 {
        return Err(Error::Validation(format!(
            "new_threshold must be at least 1, got {new_threshold}"
        )));
    }
    Ok(())
}

pub fn validate_timeout(microseconds: i64) -> Result<(), Error> {
    if microseconds <= 0 {
        return Err(Error::Validation(format!(
            "new_timeout_microseconds must be positive, got {microseconds}"
        )));
    }
    Ok(())
}

pub fn validate_unique_issuers(issuers: &[CantonId], field: &str) -> Result<(), Error> {
    let mut seen = HashSet::new();
    for issuer in issuers {
        if !seen.insert(issuer) {
            return Err(Error::Validation(format!(
                "{field} must not list {issuer} more than once"
            )));
        }
    }
    Ok(())
}

/// Mirrors the Daml `selfIssuedRequirementsHaveClaims` guard. A requirement the
/// governance party issues itself must name at least one claim. The mint
/// refuses a claimless self-issued credential, because it attests for nobody.
/// Requirements from other issuers are out of scope: those credentials arrive
/// out of band.
pub fn validate_self_issued_requirements_have_claims(
    requirements: &[PartyCredentialRequirement],
    governance_party: &CantonId,
    field: &str,
) -> Result<(), Error> {
    for requirement in requirements {
        if requirement.issuer == *governance_party && requirement.required_claims.is_empty() {
            return Err(Error::Validation(format!(
                "{field}: a requirement issued by the governance party must list at least one required claim"
            )));
        }
    }
    Ok(())
}

/// Reject an epoch-microsecond instant that is not in the future.
///
/// The on-ledger `executeImpl` asserts the same thing, but only at execute
/// time — after a full propose/confirm round has been spent on a value that
/// could never have worked.
pub fn validate_future_micros(micros: i64, now_micros: i64, field: &str) -> Result<(), Error> {
    if micros <= 0 {
        return Err(Error::Validation(format!(
            "{field} must be positive, got {micros}"
        )));
    }
    if micros <= now_micros {
        return Err(Error::Validation(format!(
            "{field} must be in the future, got {micros} (now {now_micros})"
        )));
    }
    Ok(())
}

pub fn validate_positive_amount(amount: &DamlDecimal, field: &str) -> Result<(), Error> {
    // `DamlDecimal` itself doesn't implement `PartialOrd`; compare via the
    // inner `rust_decimal::Decimal` returned by `value()` against the zero
    // constant so we don't need a direct dep on `rust_decimal`.
    let zero = DamlDecimal::ZERO.value();
    if amount.value() <= zero {
        return Err(Error::Validation(format!(
            "{field} must be strictly positive, got {amount}"
        )));
    }
    Ok(())
}

pub fn validate_beneficiary_weights(beneficiaries: &[AppRewardBeneficiary]) -> Result<(), Error> {
    if beneficiaries.is_empty() {
        return Ok(());
    }
    let sum: DamlDecimal = beneficiaries.iter().map(|b| b.weight).sum();
    let one: DamlDecimal = "1".parse().expect("'1' is a valid DamlDecimal");
    if sum != one {
        return Err(Error::Validation(format!(
            "FAR beneficiary weights must sum to exactly 1.0, got {sum}"
        )));
    }
    Ok(())
}

/// Validates a `new_beneficiaries` list (e.g.
/// `SetupCouponReassignmentDelegation::new_beneficiaries`): non-empty,
/// <= 20 entries, no duplicate beneficiary, each percentage in (0.0, 1.0],
/// summing to exactly 1.0.
///
/// The uniqueness rule mirrors the on-ledger `RewardCoupon_AssignBeneficiaries`
/// impl (`require "Beneficaries are unique"`); catching it here means a
/// duplicated split is rejected at propose time rather than passing the vote
/// and then failing every `Delegation_Assign`, which would leave a permanently
/// unusable delegation.
///
/// `DamlDecimal` addition is exact (no float rounding), so an exact `==`
/// against `1.0` is sufficient here — no epsilon tolerance is needed.
pub fn validate_reward_beneficiaries(beneficiaries: &[RewardBeneficiary]) -> Result<(), Error> {
    if beneficiaries.is_empty() {
        return Err(Error::Validation(
            "new_beneficiaries must not be empty".to_string(),
        ));
    }
    if beneficiaries.len() > 20 {
        return Err(Error::Validation(
            "at most 20 beneficiaries per coupon".to_string(),
        ));
    }
    let one = DamlDecimal::parse("1").map_err(|e| Error::Validation(e.to_string()))?;
    let mut seen = std::collections::HashSet::new();
    for b in beneficiaries {
        if b.percentage.value() <= DamlDecimal::ZERO.value() || b.percentage.value() > one.value() {
            return Err(Error::Validation(format!(
                "each percentage must be in (0.0, 1.0], got {}",
                b.percentage
            )));
        }
        if !seen.insert(&b.beneficiary) {
            return Err(Error::Validation(format!(
                "duplicate beneficiary not allowed: {}",
                b.beneficiary
            )));
        }
    }
    let sum: DamlDecimal = beneficiaries.iter().map(|b| b.percentage).sum();
    if sum != one {
        // Say how to fix it. The comparison is exact Decimal, so an even 3-way
        // split does not exist and nothing is implicitly left to the decparty —
        // both are things a proposer discovers at execute otherwise.
        return Err(Error::Validation(format!(
            "reward beneficiary percentages must sum to exactly 1.0, got {sum}. \
             The sum is compared as exact Decimal, so balance the last entry by \
             hand rather than repeating a rounded share. To leave a remainder to \
             the decparty, list the decparty itself as a beneficiary — nothing is \
             implicit"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only helper: builds a `CantonId` with a fixed valid namespace so
    /// tests can vary just the prefix.
    fn cid(prefix: &str) -> CantonId {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        CantonId::parse(&format!("{prefix}::{ns}")).unwrap()
    }

    /// Test-only helper: builds a `RewardBeneficiary` from a Canton-ID prefix
    /// and a decimal percentage string.
    fn rb(prefix: &str, pct: &str) -> RewardBeneficiary {
        RewardBeneficiary {
            beneficiary: cid(prefix),
            percentage: pct.parse().expect("valid decimal"),
        }
    }

    #[test]
    fn validate_reward_beneficiaries_edge_cases() {
        // Empty is rejected.
        assert!(validate_reward_beneficiaries(&[]).is_err());

        // Per-percentage bound is (0.0, 1.0]: 0.0, negative, and > 1.0 all reject.
        assert!(validate_reward_beneficiaries(&[rb("a", "0.0"), rb("b", "1.0")]).is_err());
        assert!(validate_reward_beneficiaries(&[rb("a", "-0.5"), rb("b", "1.5")]).is_err());
        assert!(validate_reward_beneficiaries(&[rb("a", "1.5")]).is_err());

        // A single 1.0 (upper bound inclusive) is accepted.
        assert!(validate_reward_beneficiaries(&[rb("a", "1.0")]).is_ok());

        // Duplicate beneficiary is rejected even when percentages are otherwise valid.
        assert!(validate_reward_beneficiaries(&[rb("dup", "0.5"), rb("dup", "0.5")]).is_err());

        // Count boundary: exactly 20 (each 0.05, summing to 1.0) is accepted; 21 rejects.
        let twenty: Vec<RewardBeneficiary> =
            (0..20).map(|i| rb(&format!("b{i}"), "0.05")).collect();
        assert!(validate_reward_beneficiaries(&twenty).is_ok());
        let twenty_one: Vec<RewardBeneficiary> =
            (0..21).map(|i| rb(&format!("b{i}"), "0.05")).collect();
        assert!(validate_reward_beneficiaries(&twenty_one).is_err());

        // Valid two-way split.
        assert!(validate_reward_beneficiaries(&[rb("a", "0.8"), rb("b", "0.2")]).is_ok());
    }
}
