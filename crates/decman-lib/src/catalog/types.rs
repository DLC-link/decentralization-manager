//! Payload support types shared across several `ActionType` / `ProposalType`
//! variants — reward beneficiaries and paid-credential billing parameters —
//! plus the encoders and decoders that move them to and from the Daml Ledger
//! API `Value` wire format. Also holds
//! the `GovernableAction` proposal detail DTOs (`ServiceRequestDetails`,
//! `TransferProposalDetails`, `AcceptTransferDetails`) that `catalog::interpret`
//! parses off a `CreatedEvent`.
//!
//! The OpenAPI (`utoipa`) and TypeScript (`ts_rs`) derives are gated behind
//! the `openapi` / `typegen` features so dependency-light consumers of this
//! crate don't inherit them — see the `cfg_attr` pattern used throughout.

use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::daml::ledger::api::v2::{Optional, Value, value};
use common::api::Claim;
use common::canton_id::CantonId;

use crate::error::Error;
use crate::framework::encode::{
    field, make_int64, make_list, make_numeric, make_optional_numeric, make_party, make_record,
};
use crate::framework::record::{extract_int64, extract_record, extract_text, get_field};

/// Featured App Right beneficiary
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AppRewardBeneficiary {
    pub beneficiary: CantonId,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub weight: DamlDecimal,
}

/// A CIP-104 reward-coupon beneficiary assignment.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct RewardBeneficiary {
    pub beneficiary: CantonId,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub percentage: DamlDecimal,
}

/// Billing parameters for a paid credential.
/// Mirrors `Utility.Credential.App.V0.Types.BillingParams`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct BillingParams {
    /// The daily fee for the credential in USD (corresponds to RatePerDay record).
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub fee_per_day_usd: DamlDecimal,
    /// Duration between fee charges, in minutes.
    pub billing_period_minutes: i64,
    /// Target deposit amount in USD.
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub deposit_target_amount_usd: DamlDecimal,
    /// Holder's weight on the activity marker (0.0 - 1.0). None means 0.
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub holder_activity_weight: Option<DamlDecimal>,
}

/// Operator + counterparty parties extracted from a service-request proposal
/// (`CreateUserServiceRequest` / `CreateProviderServiceRequest`). Surfaced
/// inside `DomainGovernanceAction` so the pending-approval card can render who
/// the request onboards. Exactly one of `user` / `provider` is set, matching
/// the proposal kind.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ServiceRequestDetails {
    /// Operator party — present on both request kinds.
    pub operator: CantonId,
    /// User party — present for `CreateUserServiceRequest`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<CantonId>,
    /// Provider party — present for `CreateProviderServiceRequest`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<CantonId>,
}

/// Recipient/amount/instrument extracted from a `TransferProposal`'s
/// `transfer` field. Surfaced inside `DomainGovernanceAction` so the
/// notification queue card shows the meaningful parameters of the proposal.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TransferProposalDetails {
    pub receiver: CantonId,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub amount: DamlDecimal,
    pub instrument_admin: CantonId,
    pub instrument_id: String,
}

/// Sender/receiver/amount/instrument extracted from the `TransferInstruction`
/// referenced by an `AcceptTransferProposal`. Surfaced inside
/// `DomainGovernanceAction` so the pending-approval card for an Accept can
/// render who's transferring what to whom — the proposal contract itself
/// only carries the `TransferInstruction` cid, not these fields.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AcceptTransferDetails {
    pub sender: CantonId,
    pub receiver: CantonId,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub amount: DamlDecimal,
    pub instrument_admin: CantonId,
    pub instrument_id: String,
}

// ============================================================================
// Encoders
// ============================================================================

pub fn make_optional_beneficiaries(opt: &Option<Vec<AppRewardBeneficiary>>) -> Value {
    Value {
        sum: Some(value::Sum::Optional(Box::new(Optional {
            value: opt.as_ref().map(|beneficiaries| {
                Box::new(make_list(
                    beneficiaries
                        .iter()
                        .map(serialize_app_reward_beneficiary)
                        .collect(),
                ))
            }),
        }))),
    }
}

pub fn serialize_billing_params(params: &BillingParams) -> Value {
    make_record(vec![
        field(
            "feePerDayUsd",
            make_record(vec![field(
                "rate",
                make_numeric(&params.fee_per_day_usd.to_string()),
            )]),
        ),
        field(
            "billingPeriodMinutes",
            make_int64(params.billing_period_minutes),
        ),
        field(
            "depositTargetAmountUsd",
            make_numeric(&params.deposit_target_amount_usd.to_string()),
        ),
        field(
            "holderActivityWeight",
            make_optional_numeric(&params.holder_activity_weight),
        ),
    ])
}

pub fn serialize_app_reward_beneficiary(b: &AppRewardBeneficiary) -> Value {
    make_record(vec![
        field("beneficiary", make_party(&b.beneficiary)),
        field("weight", make_numeric(&b.weight.to_string())),
    ])
}

pub fn serialize_reward_beneficiary(b: &RewardBeneficiary) -> Value {
    make_record(vec![
        field("beneficiary", make_party(&b.beneficiary)),
        field("percentage", make_numeric(&b.percentage.to_string())),
    ])
}

// ============================================================================
// Decoders
// ============================================================================
//
// Shared by `catalog::action::ActionType::from_cbtc_proto` and any
// integrator decoding these payload shapes out of a custom `Value`.

pub fn deserialize_claim(value: &Value) -> Result<Claim, Error> {
    let record = extract_record(value)?;
    Ok(Claim {
        subject: extract_text(get_field(record, "subject")?)?,
        property: extract_text(get_field(record, "property")?)?,
        value: extract_text(get_field(record, "value")?)?,
    })
}

/// Deserialize RelTime (record with microseconds field) to i64
pub fn deserialize_reltime(value: &Value) -> Result<i64, Error> {
    let record = extract_record(value)?;
    extract_int64(get_field(record, "microseconds")?)
}
