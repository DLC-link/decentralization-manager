//! Payload support types shared across several `ActionType` / `ProposalType`
//! variants — vault limits, Featured-App-Right beneficiaries/config, and
//! paid-credential billing parameters — plus the encoders that lower them
//! into the Daml Ledger API `Value` wire format.
//!
//! The OpenAPI (`utoipa`) and TypeScript (`ts_rs`) derives are gated behind
//! the `openapi` / `typegen` features so dependency-light consumers of this
//! crate don't inherit them — see the `cfg_attr` pattern used throughout.

use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::daml::ledger::api::v2::{Optional, Value, value};
use common::canton_id::CantonId;

use crate::framework::encode::{
    field, make_contract_id, make_int64, make_list, make_numeric, make_optional_numeric,
    make_party, make_record,
};

/// Vault limits configuration (all fields are optional in Daml)
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct VaultLimits {
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_deposit: Option<DamlDecimal>,
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_deposit_amount: Option<DamlDecimal>,
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_withdrawal_amount: Option<DamlDecimal>,
}

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

/// Featured App Right configuration
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct FarConfig {
    pub featured_app_right_cid: String,
    pub beneficiaries: Vec<AppRewardBeneficiary>,
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

pub fn serialize_vault_limits(limits: &VaultLimits) -> Value {
    make_record(vec![
        field(
            "maxTotalDeposit",
            make_optional_numeric(&limits.max_total_deposit),
        ),
        field(
            "minDepositAmount",
            make_optional_numeric(&limits.min_deposit_amount),
        ),
        field(
            "minWithdrawalAmount",
            make_optional_numeric(&limits.min_withdrawal_amount),
        ),
    ])
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

pub fn serialize_far_config(config: &FarConfig) -> Value {
    make_record(vec![
        field(
            "featuredAppRightCid",
            make_contract_id(&config.featured_app_right_cid),
        ),
        field(
            "beneficiaries",
            make_list(
                config
                    .beneficiaries
                    .iter()
                    .map(serialize_app_reward_beneficiary)
                    .collect(),
            ),
        ),
    ])
}

pub fn serialize_optional_far_config(config: &Option<FarConfig>) -> Value {
    Value {
        sum: Some(value::Sum::Optional(Box::new(Optional {
            value: config.as_ref().map(|c| Box::new(serialize_far_config(c))),
        }))),
    }
}
