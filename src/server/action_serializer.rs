//! Serialization of ActionType to DAML Values for Vault Governance
//!
//! This module provides bidirectional conversion between `ActionType` enum
//! and DAML `Value` representations for use with the Ledger API.

use anyhow::Context;
use canton_common::{
    decimal::DamlDecimal,
    transfer_factory::{Context as ChoiceContext, ContextValue},
};
use canton_proto_rs::com::daml::ledger::api::v2::{
    List, Optional, Record, RecordField, TextMap, Value, Variant, text_map, value,
};

use crate::{canton_id::CantonId, error::Result};

use super::types::{
    ActionType, AppRewardBeneficiary, BillingParams, Claim, FarConfig, InstrumentAllowance,
    InstrumentId, InstrumentIdentifier, ProposalType, VaultLimits,
};

// ============================================================================
// Helper Functions
// ============================================================================

fn make_party(p: impl std::fmt::Display) -> Value {
    Value {
        sum: Some(value::Sum::Party(p.to_string())),
    }
}

fn make_text(t: &str) -> Value {
    Value {
        sum: Some(value::Sum::Text(t.to_string())),
    }
}

fn make_int64(n: i64) -> Value {
    Value {
        sum: Some(value::Sum::Int64(n)),
    }
}

fn make_numeric(d: &str) -> Value {
    Value {
        sum: Some(value::Sum::Numeric(d.to_string())),
    }
}

fn make_bool(b: bool) -> Value {
    Value {
        sum: Some(value::Sum::Bool(b)),
    }
}

fn make_contract_id(c: &str) -> Value {
    Value {
        sum: Some(value::Sum::ContractId(c.to_string())),
    }
}

fn field(label: &str, value: Value) -> RecordField {
    RecordField {
        label: label.to_string(),
        value: Some(value),
    }
}

fn make_record(fields: Vec<RecordField>) -> Value {
    Value {
        sum: Some(value::Sum::Record(Record {
            record_id: None,
            fields,
        })),
    }
}

fn make_variant(constructor: &str, value: Value) -> Value {
    Value {
        sum: Some(value::Sum::Variant(Box::new(Variant {
            variant_id: None,
            constructor: constructor.to_string(),
            value: Some(Box::new(value)),
        }))),
    }
}

fn make_list(values: Vec<Value>) -> Value {
    Value {
        sum: Some(value::Sum::List(List { elements: values })),
    }
}

fn make_empty_text_map() -> Value {
    make_text_map(vec![])
}

fn make_text_map(entries: Vec<(String, Value)>) -> Value {
    Value {
        sum: Some(value::Sum::TextMap(TextMap {
            entries: entries
                .into_iter()
                .map(|(k, v)| text_map::Entry {
                    key: k,
                    value: Some(v),
                })
                .collect(),
        })),
    }
}

// Splice's `Metadata.values` is typed `TextMap Text` and `ChoiceContext.values`
// is typed `TextMap AnyValue` (see `Splice.Api.Token.MetadataV1`). Both must be
// sent as a `TextMap` value — an empty `GenMap` is rejected by Canton's command
// preprocessor with `mismatching type: TextMap ... and value: ValueGenMap()`.
fn make_empty_metadata() -> Value {
    make_record(vec![field("values", make_empty_text_map())])
}

fn make_empty_extra_args() -> Value {
    make_extra_args(make_empty_text_map())
}

/// Fallback timestamps for serializing a `Transfer` record when no explicit
/// validity window is supplied (tests only — the propose handler always passes
/// a real, bounded window). `0` is epoch and `i64::MAX / 1000` is the maximum
/// Daml `Time` value.
///
/// These were the *production* values once, but an effectively-infinite
/// `executeBefore` meant a two-step transfer offer the receiver never accepted
/// locked the sender's holdings forever. Production now bounds the window via
/// [`TransferValidity`]; see [`TRANSFER_VALIDITY_WINDOW_MICROS`].
pub const TRANSFER_REQUESTED_AT_MICROS: i64 = 0;
pub const TRANSFER_EXECUTE_BEFORE_MICROS: i64 = i64::MAX / 1000;

/// How long a `Transfer` proposal (and, for two-step transfers, the resulting
/// offer) stays executable/acceptable after creation. Bounding this means an
/// unaccepted offer expires and its escrowed holdings can be reclaimed, rather
/// than locking funds indefinitely. 24h matches the daml test fixtures and the
/// governance action timeout.
pub const TRANSFER_VALIDITY_WINDOW_MICROS: i64 = 24 * 60 * 60 * 1_000_000;

/// The `requestedAt` / `executeBefore` pair stamped onto a `Transfer`. The same
/// instance must be used for both the registry choice-context fetch and the
/// on-chain `TransferProposal` create args — the registrar resolves the context
/// for these exact values, so any drift fails interpretation at execute time.
#[derive(Clone, Copy, Debug)]
pub struct TransferValidity {
    pub requested_at_micros: i64,
    pub execute_before_micros: i64,
}

impl TransferValidity {
    /// A window starting at `now_micros` and lasting
    /// [`TRANSFER_VALIDITY_WINDOW_MICROS`]. `now_micros` is captured once by the
    /// caller so the registry and on-chain payloads agree byte-for-byte.
    ///
    /// `executeBefore` is clamped to [`TRANSFER_EXECUTE_BEFORE_MICROS`] (the
    /// module's max Daml `Time`) so an unexpectedly large `now_micros` can never
    /// serialize an out-of-range timestamp.
    pub fn from_now(now_micros: i64) -> Self {
        Self {
            requested_at_micros: now_micros,
            execute_before_micros: now_micros
                .saturating_add(TRANSFER_VALIDITY_WINDOW_MICROS)
                .min(TRANSFER_EXECUTE_BEFORE_MICROS),
        }
    }
}

fn make_extra_args(context_values: Value) -> Value {
    make_record(vec![
        field(
            "context",
            make_record(vec![field("values", context_values)]),
        ),
        field("meta", make_empty_metadata()),
    ])
}

/// Serialize a `Splice.Api.Token.MetadataV1.AnyValue` constructor as a Daml
/// `Variant` Value suitable for the Ledger API.
fn make_any_value(v: &ContextValue) -> Result<Value> {
    let (ctor, inner) = match v {
        ContextValue::Text(s) => ("AV_Text", make_text(s)),
        ContextValue::Int(n) => ("AV_Int", make_int64(*n)),
        ContextValue::Decimal(d) => ("AV_Decimal", make_numeric(&d.to_string())),
        ContextValue::Bool(b) => ("AV_Bool", make_bool(*b)),
        ContextValue::Party(p) => ("AV_Party", make_party(p)),
        ContextValue::ContractId(cid) => ("AV_ContractId", make_contract_id(cid)),
        ContextValue::List(items) => {
            let elements: Result<Vec<Value>> = items.iter().map(make_any_value).collect();
            ("AV_List", make_list(elements?))
        }
        ContextValue::Map(m) => {
            let mut entries: Vec<(String, Value)> = m
                .iter()
                .map(|(k, v)| make_any_value(v).map(|av| (k.clone(), av)))
                .collect::<Result<_>>()?;
            // Stable order so wire bytes are deterministic.
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            ("AV_Map", make_text_map(entries))
        }
        ContextValue::Date(_) | ContextValue::Time(_) | ContextValue::RelTime(_) => {
            anyhow::bail!(
                "ContextValue::{v:?} not supported in choice context: only Text, Int, Decimal, \
                 Bool, Party, ContractId, List, and Map are translated to the Ledger API today",
            );
        }
    };
    Ok(make_variant(ctor, inner))
}

/// Build the `extraArgs` record with the choice-context values populated from
/// a registry response (e.g. `registry::accept_context::get`).
fn make_extra_args_from_context(ctx: &ChoiceContext) -> Result<Value> {
    let mut entries: Vec<(String, Value)> = ctx
        .values
        .iter()
        .map(|(k, v)| make_any_value(v).map(|av| (k.clone(), av)))
        .collect::<Result<_>>()?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(make_extra_args(make_text_map(entries)))
}

// ============================================================================
// Complex Type Serializers
// ============================================================================

fn serialize_instrument_id(id: &InstrumentId) -> Value {
    make_record(vec![
        field("admin", make_party(&id.admin)),
        field("id", make_text(&id.id)),
    ])
}

fn make_optional_numeric(opt: &Option<DamlDecimal>) -> Value {
    Value {
        sum: Some(value::Sum::Optional(Box::new(Optional {
            value: opt.as_ref().map(|n| Box::new(make_numeric(&n.to_string()))),
        }))),
    }
}

fn make_optional_bool(opt: &Option<bool>) -> Value {
    Value {
        sum: Some(value::Sum::Optional(Box::new(Optional {
            value: opt.as_ref().map(|b| Box::new(make_bool(*b))),
        }))),
    }
}

fn make_optional_beneficiaries(opt: &Option<Vec<AppRewardBeneficiary>>) -> Value {
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

fn serialize_vault_limits(limits: &VaultLimits) -> Value {
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

fn serialize_claim(claim: &Claim) -> Value {
    make_record(vec![
        field("subject", make_text(&claim.subject)),
        field("property", make_text(&claim.property)),
        field("value", make_text(&claim.value)),
    ])
}

fn serialize_billing_params(params: &BillingParams) -> Value {
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

fn serialize_app_reward_beneficiary(b: &AppRewardBeneficiary) -> Value {
    make_record(vec![
        field("beneficiary", make_party(&b.beneficiary)),
        field("weight", make_numeric(&b.weight.to_string())),
    ])
}

fn serialize_instrument_identifier(i: &InstrumentIdentifier) -> Value {
    make_record(vec![
        field("source", make_party(&i.source)),
        field("id", make_text(&i.id)),
        field("scheme", make_text(&i.scheme)),
    ])
}

fn serialize_far_config(config: &FarConfig) -> Value {
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

fn serialize_optional_far_config(config: &Option<FarConfig>) -> Value {
    Value {
        sum: Some(value::Sum::Optional(Box::new(Optional {
            value: config.as_ref().map(|c| Box::new(serialize_far_config(c))),
        }))),
    }
}

/// Serialize RelTime (microseconds wrapped in a record)
fn serialize_reltime(microseconds: i64) -> Value {
    make_record(vec![field("microseconds", make_int64(microseconds))])
}

// ============================================================================
// Action Serialization
// ============================================================================

/// Serialize an ActionType to a DAML Value (ActionRequiringConfirmation variant)
///
/// The DAML ActionRequiringConfirmation type uses nested variants:
/// - GovernanceAction(Governance_AddMemberAndSetThreshold {...})
/// - UtilityOnboardingAction(UtilityOnboarding_CreateProviderServiceRequest {...})
/// - VaultDeploymentAction({...}) - direct record, not nested
pub fn serialize_action(action: &ActionType) -> Value {
    match action {
        // Governance Actions - wrapped in GovernanceAction variant
        ActionType::GovernanceAddMember {
            member,
            new_threshold,
        } => make_variant(
            "GovernanceAction",
            make_variant(
                "Governance_AddMemberAndSetThreshold",
                make_record(vec![
                    field("member", make_party(member)),
                    field("newThreshold", make_int64(*new_threshold)),
                ]),
            ),
        ),

        ActionType::GovernanceRemoveMember {
            member,
            new_threshold,
        } => make_variant(
            "GovernanceAction",
            make_variant(
                "Governance_RemoveMemberAndSetThreshold",
                make_record(vec![
                    field("member", make_party(member)),
                    field("newThreshold", make_int64(*new_threshold)),
                ]),
            ),
        ),

        ActionType::GovernanceSetThreshold { new_threshold } => make_variant(
            "GovernanceAction",
            make_variant(
                "Governance_SetThreshold",
                make_record(vec![field("newThreshold", make_int64(*new_threshold))]),
            ),
        ),

        ActionType::GovernanceSetTimeout {
            new_timeout_microseconds,
        } => make_variant(
            "GovernanceAction",
            make_variant(
                "Governance_SetActionConfirmationTimeout",
                make_record(vec![field(
                    "newActionConfirmationTimeout",
                    serialize_reltime(*new_timeout_microseconds),
                )]),
            ),
        ),

        // Vault Deployment Actions - VaultDeploymentAction wraps VaultGovernanceRules_DeployVault
        ActionType::VaultDeployment {
            vault_rules_cid,
            vault_name,
            share_symbol,
            asset_instrument_id,
            limits,
            vault_backend_signatory,
            vault_far_config,
            allocation_factory_cid,
            registrar_service_cid,
        } => make_variant(
            "VaultDeploymentAction",
            make_record(vec![
                field("vaultRulesCid", make_contract_id(vault_rules_cid)),
                field("vaultName", make_text(vault_name)),
                field("shareSymbol", make_text(share_symbol)),
                field(
                    "assetInstrumentId",
                    serialize_instrument_id(asset_instrument_id),
                ),
                field("limits", serialize_vault_limits(limits)),
                field("vaultBackendSignatory", make_party(vault_backend_signatory)),
                field(
                    "vaultFarConfig",
                    serialize_optional_far_config(vault_far_config),
                ),
                field(
                    "allocationFactoryCid",
                    make_contract_id(allocation_factory_cid),
                ),
                field(
                    "registrarServiceCid",
                    make_contract_id(registrar_service_cid),
                ),
            ]),
        ),

        ActionType::YieldEpochDeployment {
            vault_rules_cid,
            vault_cid,
            asset_instrument_id,
            vault_backend_signatory,
        } => make_variant(
            "YieldEpochDeploymentAction",
            make_record(vec![
                field("vaultRulesCid", make_contract_id(vault_rules_cid)),
                field("vaultCid", make_contract_id(vault_cid)),
                field(
                    "assetInstrumentId",
                    serialize_instrument_id(asset_instrument_id),
                ),
                field("vaultBackendSignatory", make_party(vault_backend_signatory)),
            ]),
        ),

        // Vault Operations - direct variants with DAML field names
        ActionType::VaultPause { vault_id } => make_variant(
            "VaultPauseAction",
            make_record(vec![field("pauseVaultId", make_contract_id(vault_id))]),
        ),

        ActionType::VaultUnpause { vault_id } => make_variant(
            "VaultUnpauseAction",
            make_record(vec![field("unpauseVaultId", make_contract_id(vault_id))]),
        ),

        ActionType::VaultUpdateLimits {
            vault_id,
            new_limits,
        } => make_variant(
            "VaultUpdateLimitsAction",
            make_record(vec![
                field("limitsVaultId", make_contract_id(vault_id)),
                field("newLimits", serialize_vault_limits(new_limits)),
            ]),
        ),

        ActionType::VaultUpdateBackend {
            vault_id,
            new_backend_signatory,
        } => make_variant(
            "VaultUpdateBackendAction",
            make_record(vec![
                field("backendVaultId", make_contract_id(vault_id)),
                field("newBackendSignatory", make_party(new_backend_signatory)),
            ]),
        ),

        ActionType::VaultUpdateFarBeneficiaries {
            vault_id,
            new_beneficiaries,
        } => make_variant(
            "VaultUpdateFARBeneficiariesAction",
            make_record(vec![
                field("farVaultId", make_contract_id(vault_id)),
                field(
                    "newBeneficiaries",
                    make_list(
                        new_beneficiaries
                            .iter()
                            .map(serialize_app_reward_beneficiary)
                            .collect(),
                    ),
                ),
            ]),
        ),

        // Processor - VaultProcessorDeploymentRequestAction wrapping params
        ActionType::ProcessorDeploymentRequest {
            vault_processor_rules_cid,
            vault_backend_signatory,
            allocation_factory_cid,
            processor_far_config,
            initial_supported_vaults,
        } => make_variant(
            "VaultProcessorDeploymentRequestAction",
            make_record(vec![
                field(
                    "vaultProcessorRulesCid",
                    make_contract_id(vault_processor_rules_cid),
                ),
                field("vaultBackendSignatory", make_party(vault_backend_signatory)),
                field(
                    "allocationFactoryCid",
                    make_contract_id(allocation_factory_cid),
                ),
                field(
                    "processorFarConfig",
                    serialize_optional_far_config(processor_far_config),
                ),
                field(
                    "initialSupportedVaults",
                    make_list(
                        initial_supported_vaults
                            .iter()
                            .map(|v| make_contract_id(v))
                            .collect(),
                    ),
                ),
            ]),
        ),

        // Utility Onboarding - wrapped in UtilityOnboardingAction variant
        ActionType::UtilityCreateProviderRequest { operator } => make_variant(
            "UtilityOnboardingAction",
            make_variant(
                "UtilityOnboarding_CreateProviderServiceRequest",
                make_record(vec![field("operator", make_party(operator))]),
            ),
        ),

        ActionType::UtilityCreateUserRequest { operator } => make_variant(
            "UtilityOnboardingAction",
            make_variant(
                "UtilityOnboarding_CreateUserServiceRequest",
                make_record(vec![field("operator", make_party(operator))]),
            ),
        ),

        ActionType::UtilitySetup {
            operator,
            provider_service_cid,
            user_service_cid,
        } => make_variant(
            "UtilityOnboardingAction",
            make_variant(
                "UtilityOnboarding_SetupUtility",
                make_record(vec![
                    field("operator", make_party(operator)),
                    field("providerServiceCid", make_contract_id(provider_service_cid)),
                    field("userServiceCid", make_contract_id(user_service_cid)),
                ]),
            ),
        ),

        ActionType::UtilityAcceptHolderServiceRequest {
            operator,
            provider_service_cid,
            holder_service_request_cid,
            holder,
        } => make_variant(
            "UtilityOnboardingAction",
            make_variant(
                "UtilityOnboarding_AcceptHolderServiceRequest",
                make_record(vec![
                    field("operator", make_party(operator)),
                    field("providerServiceCid", make_contract_id(provider_service_cid)),
                    field(
                        "holderServiceRequestCid",
                        make_contract_id(holder_service_request_cid),
                    ),
                    // Note: payload field is complex (HolderServiceRequest_Accept) - simplified here
                    field("holder", make_party(holder)),
                ]),
            ),
        ),

        // Credential Actions
        ActionType::CredentialOfferFree {
            operator,
            user_service_cid,
            holder,
            id,
            description,
            claims,
        } => make_variant(
            "CredentialAction",
            make_variant(
                "Credential_OfferFreeCredential",
                make_record(vec![
                    field("operator", make_party(operator)),
                    field("userServiceCid", make_contract_id(user_service_cid)),
                    field("holder", make_party(holder)),
                    field("id", make_text(id)),
                    field("description", make_text(description)),
                    field(
                        "claims",
                        make_list(claims.iter().map(serialize_claim).collect()),
                    ),
                ]),
            ),
        ),

        ActionType::CredentialAcceptFree {
            operator,
            user_service_cid,
            credential_offer_cid,
        } => make_variant(
            "CredentialAction",
            make_variant(
                "Credential_AcceptFreeCredential",
                make_record(vec![
                    field("operator", make_party(operator)),
                    field("userServiceCid", make_contract_id(user_service_cid)),
                    field("credentialOfferCid", make_contract_id(credential_offer_cid)),
                ]),
            ),
        ),

        // DevNet
        ActionType::DevNetFeatureApp { amulet_rules_cid } => make_variant(
            "DevNetFeatureAppAction",
            make_record(vec![field(
                "amuletRulesCid",
                make_contract_id(amulet_rules_cid),
            )]),
        ),

        ActionType::GovernanceAddAdditionalProposer { .. }
        | ActionType::GovernanceRemoveAdditionalProposer { .. } => {
            panic!(
                "ActionType {action:?} is a governance self-action, not an ActionRequiringConfirmation"
            )
        }
    }
}

/// Build the ConfirmAction choice argument
///
/// The DAML structure is: { confirmer: Party, action: ActionRequiringConfirmation }
pub fn build_confirm_action_argument(confirmer: &str, action: &ActionType) -> Value {
    make_record(vec![
        field("confirmer", make_party(confirmer)),
        field("action", serialize_action(action)),
    ])
}

/// Build the ExecuteConfirmedAction choice argument
///
/// The DAML structure is:
/// { executor: Party, action: ActionRequiringConfirmation, confirmations: [ContractId], contractCid: Optional ContractId }
pub fn build_execute_action_argument(
    executor: &str,
    action: &ActionType,
    confirmation_cids: &[String],
    contract_cid: Option<&str>,
) -> Value {
    let confirmations = make_list(
        confirmation_cids
            .iter()
            .map(|cid| make_contract_id(cid))
            .collect(),
    );

    let contract_cid_value = Value {
        sum: Some(value::Sum::Optional(Box::new(Optional {
            value: contract_cid.map(|cid| Box::new(make_contract_id(cid))),
        }))),
    };

    make_record(vec![
        field("executor", make_party(executor)),
        field("action", serialize_action(action)),
        field("confirmations", confirmations),
        field("contractCid", contract_cid_value),
    ])
}

// ============================================================================
// Governance-Core Self-Management Serialization
// ============================================================================

/// Serialize an ActionType to a GovernanceSelfAction DAML variant
///
/// Maps the same ActionType variants used for vault governance to the
/// governance-core GovernanceSelfAction enum (different field names).
fn serialize_self_action(action: &ActionType) -> Value {
    match action {
        ActionType::GovernanceAddMember {
            member,
            new_threshold,
        } => make_variant(
            "SelfAction_AddMemberAndSetThreshold",
            make_record(vec![
                field("newMember", make_party(member)),
                field("newThresholdAfterAdd", make_int64(*new_threshold)),
            ]),
        ),
        ActionType::GovernanceRemoveMember {
            member,
            new_threshold,
        } => make_variant(
            "SelfAction_RemoveMemberAndSetThreshold",
            make_record(vec![
                field("removedMember", make_party(member)),
                field("newThresholdAfterRemove", make_int64(*new_threshold)),
            ]),
        ),
        ActionType::GovernanceSetThreshold { new_threshold } => make_variant(
            "SelfAction_SetThreshold",
            make_record(vec![field("updatedThreshold", make_int64(*new_threshold))]),
        ),
        ActionType::GovernanceSetTimeout {
            new_timeout_microseconds,
        } => make_variant(
            "SelfAction_SetTimeout",
            make_record(vec![field(
                "updatedTimeout",
                serialize_reltime(*new_timeout_microseconds),
            )]),
        ),
        ActionType::GovernanceAddAdditionalProposer {
            additional_proposer,
        } => make_variant(
            "SelfAction_AddAdditionalProposer",
            make_record(vec![field(
                "additionalProposer",
                make_party(additional_proposer),
            )]),
        ),
        ActionType::GovernanceRemoveAdditionalProposer {
            additional_proposer,
        } => make_variant(
            "SelfAction_RemoveAdditionalProposer",
            make_record(vec![field(
                "additionalProposer",
                make_party(additional_proposer),
            )]),
        ),
        _ => panic!("ActionType {action:?} is not a governance self-management action"),
    }
}

/// Deserialize a GovernanceSelfAction DAML variant to ActionType
pub fn deserialize_self_action(value: &Value) -> Result<ActionType> {
    let variant = match &value.sum {
        Some(value::Sum::Variant(v)) => v,
        _ => anyhow::bail!("Expected Variant value for GovernanceSelfAction"),
    };

    let inner = variant
        .value
        .as_ref()
        .context("GovernanceSelfAction variant has no inner value")?;

    let record = extract_record(inner).context("Expected GovernanceSelfAction record")?;
    let constructor = &variant.constructor;

    match constructor.as_str() {
        "SelfAction_AddMemberAndSetThreshold" => {
            let member = extract_party_id(get_field(record, "newMember")?)?;
            let new_threshold = extract_int64(get_field(record, "newThresholdAfterAdd")?)?;
            Ok(ActionType::GovernanceAddMember {
                member,
                new_threshold,
            })
        }
        "SelfAction_RemoveMemberAndSetThreshold" => {
            let member = extract_party_id(get_field(record, "removedMember")?)?;
            let new_threshold = extract_int64(get_field(record, "newThresholdAfterRemove")?)?;
            Ok(ActionType::GovernanceRemoveMember {
                member,
                new_threshold,
            })
        }
        "SelfAction_SetThreshold" => {
            let new_threshold = extract_int64(get_field(record, "updatedThreshold")?)?;
            Ok(ActionType::GovernanceSetThreshold { new_threshold })
        }
        "SelfAction_SetTimeout" => {
            let reltime = get_field(record, "updatedTimeout")?;
            let microseconds = deserialize_reltime(reltime)?;
            Ok(ActionType::GovernanceSetTimeout {
                new_timeout_microseconds: microseconds,
            })
        }
        "SelfAction_AddAdditionalProposer" => {
            let additional_proposer = extract_party_id(get_field(record, "additionalProposer")?)?;
            Ok(ActionType::GovernanceAddAdditionalProposer {
                additional_proposer,
            })
        }
        "SelfAction_RemoveAdditionalProposer" => {
            let additional_proposer = extract_party_id(get_field(record, "additionalProposer")?)?;
            Ok(ActionType::GovernanceRemoveAdditionalProposer {
                additional_proposer,
            })
        }
        other => anyhow::bail!("Unknown GovernanceSelfAction constructor: {other}"),
    }
}

/// Build the GovernanceRules_ConfirmGovernanceAction choice argument
///
/// DAML structure: { confirmer: Party, action: GovernanceSelfAction }
pub fn build_confirm_governance_action_arg(confirmer: &str, action: &ActionType) -> Value {
    make_record(vec![
        field("confirmer", make_party(confirmer)),
        field("action", serialize_self_action(action)),
    ])
}

/// Build the GovernanceRules_ExecuteGovernanceAction choice argument
///
/// DAML structure: { executor: Party, action: GovernanceSelfAction, confirmations: [ContractId GovernanceSelfConfirmation] }
pub fn build_execute_governance_action_arg(
    executor: &str,
    action: &ActionType,
    confirmation_cids: &[String],
) -> Value {
    let confirmations = make_list(
        confirmation_cids
            .iter()
            .map(|cid| make_contract_id(cid))
            .collect(),
    );

    make_record(vec![
        field("executor", make_party(executor)),
        field("action", serialize_self_action(action)),
        field("confirmations", confirmations),
    ])
}

// ============================================================================
// Governance-Core Domain Action Proposal Serialization
// ============================================================================

fn serialize_instrument_allowances(allowances: &[InstrumentAllowance]) -> Value {
    make_list(
        allowances
            .iter()
            .map(|a| make_record(vec![field("id", make_text(&a.id))]))
            .collect(),
    )
}

/// Which package a proposal template belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub enum ProposalPackage {
    GovernanceCore,
    GovernanceTokenCustody,
    GovernanceUtilityCredential,
    GovernanceUtilityOnboarding,
}

/// Build the create-command record fields for a governance domain action proposal.
///
/// Returns (package, module_name, entity_name, record_fields) for the CreateCommand.
pub fn build_proposal_create_args(
    governance_party: &str,
    proposer: &str,
    proposal: &ProposalType,
    transfer_choice_context: Option<&ChoiceContext>,
    transfer_validity: Option<TransferValidity>,
) -> Result<(ProposalPackage, &'static str, &'static str, Record)> {
    // Fall back to the (unbounded) const window only when no explicit validity
    // is supplied — i.e. tests; the propose handler always passes a real one.
    let validity = transfer_validity.unwrap_or(TransferValidity {
        requested_at_micros: TRANSFER_REQUESTED_AT_MICROS,
        execute_before_micros: TRANSFER_EXECUTE_BEFORE_MICROS,
    });
    Ok(match proposal {
        ProposalType::SetupCcPreapproval {
            provider,
            expected_dso,
        } => (
            ProposalPackage::GovernanceTokenCustody,
            "Governance.TokenCustody.SetupCcPreapproval",
            "SetupCcPreapprovalProposal",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field("provider", make_party(provider)),
                    field(
                        "expectedDso",
                        Value {
                            sum: Some(value::Sum::Optional(Box::new(Optional {
                                value: Some(Box::new(make_party(expected_dso))),
                            }))),
                        },
                    ),
                ],
            },
        ),
        ProposalType::SetupTokenPreapproval {
            operator,
            instrument_admin,
            instrument_allowances,
        } => (
            ProposalPackage::GovernanceTokenCustody,
            "Governance.TokenCustody.SetupTokenPreapproval",
            "SetupTokenPreapprovalProposal",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field("operator", make_party(operator)),
                    field("instrumentAdmin", make_party(instrument_admin)),
                    field(
                        "instrumentAllowances",
                        serialize_instrument_allowances(instrument_allowances),
                    ),
                ],
            },
        ),
        ProposalType::Transfer {
            transfer_factory_cid,
            expected_admin,
            receiver,
            amount,
            instrument_id,
            input_holding_cids,
        } => {
            let transfer_record = make_record(vec![
                field("sender", make_party(governance_party)),
                field("receiver", make_party(receiver)),
                field("amount", make_numeric(&amount.to_string())),
                field(
                    "instrumentId",
                    make_record(vec![
                        field("admin", make_party(&instrument_id.admin)),
                        field("id", make_text(&instrument_id.id)),
                    ]),
                ),
                field(
                    "requestedAt",
                    Value {
                        sum: Some(value::Sum::Timestamp(validity.requested_at_micros)),
                    },
                ),
                field(
                    "executeBefore",
                    Value {
                        sum: Some(value::Sum::Timestamp(validity.execute_before_micros)),
                    },
                ),
                field(
                    "inputHoldingCids",
                    make_list(
                        input_holding_cids
                            .iter()
                            .map(|cid| make_contract_id(cid))
                            .collect(),
                    ),
                ),
                field("meta", make_empty_metadata()),
            ]);
            let extra_args = match transfer_choice_context {
                Some(ctx) => make_extra_args_from_context(ctx)?,
                None => make_empty_extra_args(),
            };
            (
                ProposalPackage::GovernanceTokenCustody,
                "Governance.TokenCustody.TransferProposal",
                "TransferProposal",
                Record {
                    record_id: None,
                    fields: vec![
                        field("governanceParty", make_party(governance_party)),
                        field("proposer", make_party(proposer)),
                        field("transferFactoryCid", make_contract_id(transfer_factory_cid)),
                        field("expectedAdmin", make_party(expected_admin)),
                        field("transfer", transfer_record),
                        field("extraArgs", extra_args),
                    ],
                },
            )
        }
        ProposalType::AcceptTransfer {
            transfer_instruction_cid,
        } => {
            // The Daml `TransferInstruction_Accept` choice (invoked through
            // `AcceptTransferProposal`) looks up
            // `utility.digitalasset.com/transfer-rule` (and friends) in
            // `extraArgs.context.values` at execution time. An empty context
            // would fail with `Missing context entry for
            // utility.digitalasset.com/transfer-rule`. The handler is
            // expected to fetch the choice context from the token-standard
            // registry and pass it in; if it didn't, fall back to an empty
            // record (legacy callers, e.g. tests).
            let extra_args = match transfer_choice_context {
                Some(ctx) => make_extra_args_from_context(ctx)?,
                None => make_empty_extra_args(),
            };
            (
                ProposalPackage::GovernanceTokenCustody,
                "Governance.TokenCustody.AcceptTransfer",
                "AcceptTransferProposal",
                Record {
                    record_id: None,
                    fields: vec![
                        field("governanceParty", make_party(governance_party)),
                        field("proposer", make_party(proposer)),
                        field(
                            "transferInstructionCid",
                            make_contract_id(transfer_instruction_cid),
                        ),
                        field("extraArgs", extra_args),
                    ],
                },
            )
        }
        ProposalType::GenericVote { description } => (
            ProposalPackage::GovernanceCore,
            "Governance.GenericVote",
            "GenericVoteProposal",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field("description", make_text(description)),
                ],
            },
        ),
        ProposalType::ProvisionProviderService => (
            ProposalPackage::GovernanceUtilityOnboarding,
            "Governance.UtilityOnboarding.ProvisionProviderService",
            "ProvisionProviderService",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                ],
            },
        ),
        ProposalType::SetupUtility {
            provider_service_cid,
            operator,
            instrument_id_text,
            additional_identifiers,
            create_transfer_rule,
            create_allocation_factory,
        } => (
            ProposalPackage::GovernanceUtilityOnboarding,
            "Governance.UtilityOnboarding.SetupUtility",
            "SetupUtility",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field("providerServiceCid", make_contract_id(provider_service_cid)),
                    field("operator", make_party(operator)),
                    field("instrumentIdText", make_text(instrument_id_text)),
                    field(
                        "additionalIdentifiers",
                        make_list(
                            additional_identifiers
                                .iter()
                                .map(serialize_instrument_identifier)
                                .collect(),
                        ),
                    ),
                    field("createTransferRule", make_bool(*create_transfer_rule)),
                    field(
                        "createAllocationFactory",
                        make_bool(*create_allocation_factory),
                    ),
                ],
            },
        ),
        ProposalType::CreateProviderServiceRequest { operator, provider } => (
            ProposalPackage::GovernanceUtilityOnboarding,
            "Governance.UtilityOnboarding.CreateProviderServiceRequest",
            "CreateProviderServiceRequest",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field("operator", make_party(operator)),
                    field("provider", make_party(provider)),
                ],
            },
        ),
        ProposalType::CreateUserServiceRequest { operator, user } => (
            ProposalPackage::GovernanceUtilityOnboarding,
            "Governance.UtilityOnboarding.CreateUserServiceRequest",
            "CreateUserServiceRequest",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field("operator", make_party(operator)),
                    field("user", make_party(user)),
                ],
            },
        ),
        ProposalType::SetProviderAppRewardBeneficiaries {
            instrument_configuration_cid,
            provider_app_reward_beneficiaries,
        } => (
            ProposalPackage::GovernanceUtilityOnboarding,
            "Governance.UtilityOnboarding.SetProviderAppRewardBeneficiaries",
            "SetProviderAppRewardBeneficiaries",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field(
                        "instrumentConfigurationCid",
                        make_contract_id(instrument_configuration_cid),
                    ),
                    field(
                        "providerAppRewardBeneficiaries",
                        make_optional_beneficiaries(provider_app_reward_beneficiaries),
                    ),
                ],
            },
        ),
        ProposalType::SetEnableResultContracts {
            registrar_service_cid,
            enable_result_contracts,
        } => (
            ProposalPackage::GovernanceUtilityOnboarding,
            "Governance.UtilityOnboarding.SetEnableResultContracts",
            "SetEnableResultContracts",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field(
                        "registrarServiceCid",
                        make_contract_id(registrar_service_cid),
                    ),
                    field(
                        "enableResultContracts",
                        make_optional_bool(enable_result_contracts),
                    ),
                ],
            },
        ),
        ProposalType::CreateDelegatedBatchedMarkersProxy { operator } => (
            ProposalPackage::GovernanceUtilityOnboarding,
            "Governance.UtilityOnboarding.CreateDelegatedBatchedMarkersProxy",
            "CreateDelegatedBatchedMarkersProxy",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field("operator", make_party(operator)),
                ],
            },
        ),
        ProposalType::Mint {
            allocation_factory_cid,
            instrument_id,
            instrument_configuration_cid,
            recipient,
            amount,
            description,
        } => (
            ProposalPackage::GovernanceUtilityOnboarding,
            "Governance.TokenIssuance.MintProposal",
            "MintProposal",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field(
                        "allocationFactoryCid",
                        make_contract_id(allocation_factory_cid),
                    ),
                    field("instrumentId", serialize_instrument_id(instrument_id)),
                    field(
                        "instrumentConfigurationCid",
                        make_contract_id(instrument_configuration_cid),
                    ),
                    field("recipient", make_party(recipient)),
                    field("amount", make_numeric(&amount.to_string())),
                    field("description", make_text(description)),
                    field(
                        "requestedAt",
                        Value {
                            sum: Some(value::Sum::Timestamp(0)),
                        },
                    ),
                    field(
                        "executeBefore",
                        Value {
                            sum: Some(value::Sum::Timestamp(i64::MAX / 1000)),
                        },
                    ),
                    field("meta", make_empty_metadata()),
                    field("extraArgsMeta", make_empty_metadata()),
                ],
            },
        ),
        ProposalType::OfferFreeCredential {
            user_service_cid,
            holder,
            id,
            description,
            claims,
        } => (
            ProposalPackage::GovernanceUtilityCredential,
            "Governance.UtilityCredential.OfferFreeCredential",
            "OfferFreeCredential",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field("userServiceCid", make_contract_id(user_service_cid)),
                    field("holder", make_party(holder)),
                    field("id", make_text(id)),
                    field("description", make_text(description)),
                    field(
                        "claims",
                        make_list(claims.iter().map(serialize_claim).collect()),
                    ),
                ],
            },
        ),
        ProposalType::OfferPaidCredential {
            user_service_cid,
            holder,
            id,
            description,
            claims,
            billing_params,
            deposit_initial_amount_usd,
        } => (
            ProposalPackage::GovernanceUtilityCredential,
            "Governance.UtilityCredential.OfferPaidCredential",
            "OfferPaidCredential",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field("userServiceCid", make_contract_id(user_service_cid)),
                    field("holder", make_party(holder)),
                    field("id", make_text(id)),
                    field("description", make_text(description)),
                    field(
                        "claims",
                        make_list(claims.iter().map(serialize_claim).collect()),
                    ),
                    field("billingParams", serialize_billing_params(billing_params)),
                    field(
                        "depositInitialAmountUsd",
                        make_optional_numeric(deposit_initial_amount_usd),
                    ),
                ],
            },
        ),
        ProposalType::AcceptFreeCredential {
            user_service_cid,
            credential_offer_cid,
        } => (
            ProposalPackage::GovernanceUtilityCredential,
            "Governance.UtilityCredential.AcceptFreeCredential",
            "AcceptFreeCredential",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field("userServiceCid", make_contract_id(user_service_cid)),
                    field("credentialOfferCid", make_contract_id(credential_offer_cid)),
                ],
            },
        ),
        ProposalType::Burn {
            allocation_factory_cid,
            instrument_id,
            instrument_configuration_cid,
            holder,
            amount,
            description,
        } => (
            ProposalPackage::GovernanceUtilityOnboarding,
            "Governance.TokenIssuance.BurnProposal",
            "BurnProposal",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field(
                        "allocationFactoryCid",
                        make_contract_id(allocation_factory_cid),
                    ),
                    field("instrumentId", serialize_instrument_id(instrument_id)),
                    field(
                        "instrumentConfigurationCid",
                        make_contract_id(instrument_configuration_cid),
                    ),
                    field("holder", make_party(holder)),
                    field("amount", make_numeric(&amount.to_string())),
                    field("description", make_text(description)),
                    field(
                        "requestedAt",
                        Value {
                            sum: Some(value::Sum::Timestamp(0)),
                        },
                    ),
                    field(
                        "executeBefore",
                        Value {
                            sum: Some(value::Sum::Timestamp(i64::MAX / 1000)),
                        },
                    ),
                    field("meta", make_empty_metadata()),
                    field("extraArgsMeta", make_empty_metadata()),
                ],
            },
        ),
        ProposalType::AcceptMintRequest {
            mint_request_cid,
            instrument_configuration_cid,
            description,
        } => (
            ProposalPackage::GovernanceUtilityOnboarding,
            "Governance.TokenIssuance.AcceptMintRequest",
            "AcceptMintRequest",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field("mintRequestCid", make_contract_id(mint_request_cid)),
                    field(
                        "instrumentConfigurationCid",
                        make_contract_id(instrument_configuration_cid),
                    ),
                    field("description", make_text(description)),
                    field("extraArgsMeta", make_empty_metadata()),
                ],
            },
        ),
        ProposalType::AcceptBurnRequest {
            burn_request_cid,
            instrument_configuration_cid,
            description,
        } => (
            ProposalPackage::GovernanceUtilityOnboarding,
            "Governance.TokenIssuance.AcceptBurnRequest",
            "AcceptBurnRequest",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field("burnRequestCid", make_contract_id(burn_request_cid)),
                    field(
                        "instrumentConfigurationCid",
                        make_contract_id(instrument_configuration_cid),
                    ),
                    field("description", make_text(description)),
                    field("extraArgsMeta", make_empty_metadata()),
                ],
            },
        ),
    })
}

/// Build the GovernanceRules_ConfirmAction choice argument for domain actions
///
/// DAML structure: { confirmer: Party, actionProposalCid: ContractId GovernableAction }
pub fn build_confirm_domain_action_arg(confirmer: &str, proposal_cid: &str) -> Value {
    make_record(vec![
        field("confirmer", make_party(confirmer)),
        field("actionProposalCid", make_contract_id(proposal_cid)),
    ])
}

/// Build the GovernanceRules_ExecuteConfirmedAction choice argument for domain actions
///
/// DAML structure: { executor: Party, actionProposalCid: ContractId GovernableAction, confirmations: [ContractId GovernanceConfirmation] }
pub fn build_execute_domain_action_arg(
    executor: &str,
    proposal_cid: &str,
    confirmation_cids: &[String],
) -> Value {
    let confirmations = make_list(
        confirmation_cids
            .iter()
            .map(|cid| make_contract_id(cid))
            .collect(),
    );

    make_record(vec![
        field("executor", make_party(executor)),
        field("actionProposalCid", make_contract_id(proposal_cid)),
        field("confirmations", confirmations),
    ])
}

// ============================================================================
// Deserialization Helpers
// ============================================================================

fn extract_party(value: &Value) -> Result<String> {
    match &value.sum {
        Some(value::Sum::Party(p)) => Ok(p.clone()),
        _ => anyhow::bail!("Expected Party value"),
    }
}

fn extract_party_id(value: &Value) -> Result<CantonId> {
    let party_str = extract_party(value)?;
    party_str
        .parse()
        .context("Failed to parse party as CantonId")
}

fn extract_text(value: &Value) -> Result<String> {
    match &value.sum {
        Some(value::Sum::Text(t)) => Ok(t.clone()),
        _ => anyhow::bail!("Expected Text value"),
    }
}

fn extract_int64(value: &Value) -> Result<i64> {
    match &value.sum {
        Some(value::Sum::Int64(n)) => Ok(*n),
        _ => anyhow::bail!("Expected Int64 value"),
    }
}

fn extract_numeric(value: &Value) -> Result<String> {
    match &value.sum {
        Some(value::Sum::Numeric(n)) => Ok(n.clone()),
        _ => anyhow::bail!("Expected Numeric value"),
    }
}

fn extract_contract_id(value: &Value) -> Result<String> {
    match &value.sum {
        Some(value::Sum::ContractId(c)) => Ok(c.clone()),
        _ => anyhow::bail!("Expected ContractId value"),
    }
}

fn extract_record(value: &Value) -> Result<&Record> {
    match &value.sum {
        Some(value::Sum::Record(r)) => Ok(r),
        _ => anyhow::bail!("Expected Record value"),
    }
}

fn extract_list(value: &Value) -> Result<&List> {
    match &value.sum {
        Some(value::Sum::List(l)) => Ok(l),
        _ => anyhow::bail!("Expected List value"),
    }
}

fn get_field<'a>(record: &'a Record, label: &str) -> Result<&'a Value> {
    record
        .fields
        .iter()
        .find(|f| f.label == label)
        .and_then(|f| f.value.as_ref())
        .with_context(|| format!("Missing field: {label}"))
}

fn deserialize_instrument_id(value: &Value) -> Result<InstrumentId> {
    let record = extract_record(value)?;
    Ok(InstrumentId {
        admin: extract_party(get_field(record, "admin")?)?,
        id: extract_text(get_field(record, "id")?)?,
    })
}

fn deserialize_optional_numeric(value: &Value) -> Result<Option<DamlDecimal>> {
    match &value.sum {
        Some(value::Sum::Optional(opt)) => match opt.value.as_ref() {
            Some(inner) => {
                let s = extract_numeric(inner)?;
                Ok(Some(DamlDecimal::parse(&s)?))
            }
            None => Ok(None),
        },
        _ => anyhow::bail!("Expected Optional value for numeric"),
    }
}

fn deserialize_vault_limits(value: &Value) -> Result<VaultLimits> {
    let record = extract_record(value)?;
    Ok(VaultLimits {
        max_total_deposit: deserialize_optional_numeric(get_field(record, "maxTotalDeposit")?)?,
        min_deposit_amount: deserialize_optional_numeric(get_field(record, "minDepositAmount")?)?,
        min_withdrawal_amount: deserialize_optional_numeric(get_field(
            record,
            "minWithdrawalAmount",
        )?)?,
    })
}

fn deserialize_claim(value: &Value) -> Result<Claim> {
    let record = extract_record(value)?;
    Ok(Claim {
        subject: extract_text(get_field(record, "subject")?)?,
        property: extract_text(get_field(record, "property")?)?,
        value: extract_text(get_field(record, "value")?)?,
    })
}

fn deserialize_app_reward_beneficiary(value: &Value) -> Result<AppRewardBeneficiary> {
    let record = extract_record(value)?;
    Ok(AppRewardBeneficiary {
        beneficiary: extract_party_id(get_field(record, "beneficiary")?)?,
        weight: DamlDecimal::parse(&extract_numeric(get_field(record, "weight")?)?)?,
    })
}

fn deserialize_far_config(value: &Value) -> Result<FarConfig> {
    let record = extract_record(value)?;
    let beneficiaries_list = extract_list(get_field(record, "beneficiaries")?)?;
    let beneficiaries = beneficiaries_list
        .elements
        .iter()
        .map(deserialize_app_reward_beneficiary)
        .collect::<Result<Vec<_>>>()?;

    Ok(FarConfig {
        featured_app_right_cid: extract_contract_id(get_field(record, "featuredAppRightCid")?)?,
        beneficiaries,
    })
}

fn deserialize_optional_far_config(value: &Value) -> Result<Option<FarConfig>> {
    match &value.sum {
        Some(value::Sum::Optional(opt)) => match opt.value.as_ref() {
            Some(inner) => Ok(Some(deserialize_far_config(inner)?)),
            None => Ok(None),
        },
        _ => anyhow::bail!("Expected Optional value for FarConfig"),
    }
}

/// Deserialize RelTime (record with microseconds field) to i64
fn deserialize_reltime(value: &Value) -> Result<i64> {
    let record = extract_record(value)?;
    extract_int64(get_field(record, "microseconds")?)
}

// ============================================================================
// Action Deserialization
// ============================================================================

/// Deserialize a DAML Value (ActionRequiringConfirmation variant) to an ActionType
///
/// Handles nested variant structure:
/// - GovernanceAction(Governance_AddMemberAndSetThreshold {...})
/// - UtilityOnboardingAction(UtilityOnboarding_CreateProviderServiceRequest {...})
/// - VaultDeploymentAction({...}) - direct record
pub fn deserialize_action(value: &Value) -> Result<ActionType> {
    let variant = match &value.sum {
        Some(value::Sum::Variant(v)) => v,
        _ => anyhow::bail!("Expected Variant value for action"),
    };

    let inner = variant
        .value
        .as_ref()
        .context("Variant has no inner value")?;

    match variant.constructor.as_str() {
        // Governance Actions - nested variant structure
        "GovernanceAction" => {
            let inner_variant = match &inner.sum {
                Some(value::Sum::Variant(v)) => v,
                _ => anyhow::bail!("Expected nested Variant for GovernanceAction"),
            };
            let inner_value = inner_variant
                .value
                .as_ref()
                .context("GovernanceAction inner variant has no value")?;
            let record = extract_record(inner_value)?;

            match inner_variant.constructor.as_str() {
                "Governance_AddMemberAndSetThreshold" => Ok(ActionType::GovernanceAddMember {
                    member: extract_party_id(get_field(record, "member")?)?,
                    new_threshold: extract_int64(get_field(record, "newThreshold")?)?,
                }),
                "Governance_RemoveMemberAndSetThreshold" => {
                    Ok(ActionType::GovernanceRemoveMember {
                        member: extract_party_id(get_field(record, "member")?)?,
                        new_threshold: extract_int64(get_field(record, "newThreshold")?)?,
                    })
                }
                "Governance_SetThreshold" => Ok(ActionType::GovernanceSetThreshold {
                    new_threshold: extract_int64(get_field(record, "newThreshold")?)?,
                }),
                "Governance_SetActionConfirmationTimeout" => {
                    let reltime = get_field(record, "newActionConfirmationTimeout")?;
                    let microseconds = deserialize_reltime(reltime)?;
                    Ok(ActionType::GovernanceSetTimeout {
                        new_timeout_microseconds: microseconds,
                    })
                }
                other => anyhow::bail!("Unknown GovernanceAction constructor: {other}"),
            }
        }

        // Utility Onboarding Actions - nested variant structure
        "UtilityOnboardingAction" => {
            let inner_variant = match &inner.sum {
                Some(value::Sum::Variant(v)) => v,
                _ => anyhow::bail!("Expected nested Variant for UtilityOnboardingAction"),
            };
            let inner_value = inner_variant
                .value
                .as_ref()
                .context("UtilityOnboardingAction inner variant has no value")?;
            let record = extract_record(inner_value)?;

            match inner_variant.constructor.as_str() {
                "UtilityOnboarding_CreateProviderServiceRequest" => {
                    Ok(ActionType::UtilityCreateProviderRequest {
                        operator: extract_party_id(get_field(record, "operator")?)?,
                    })
                }
                "UtilityOnboarding_CreateUserServiceRequest" => {
                    Ok(ActionType::UtilityCreateUserRequest {
                        operator: extract_party_id(get_field(record, "operator")?)?,
                    })
                }
                "UtilityOnboarding_SetupUtility" => Ok(ActionType::UtilitySetup {
                    operator: extract_party_id(get_field(record, "operator")?)?,
                    provider_service_cid: extract_contract_id(get_field(
                        record,
                        "providerServiceCid",
                    )?)?,
                    user_service_cid: extract_contract_id(get_field(record, "userServiceCid")?)?,
                }),
                "UtilityOnboarding_AcceptHolderServiceRequest" => {
                    Ok(ActionType::UtilityAcceptHolderServiceRequest {
                        operator: extract_party_id(get_field(record, "operator")?)?,
                        provider_service_cid: extract_contract_id(get_field(
                            record,
                            "providerServiceCid",
                        )?)?,
                        holder_service_request_cid: extract_contract_id(get_field(
                            record,
                            "holderServiceRequestCid",
                        )?)?,
                        holder: extract_party_id(get_field(record, "holder")?)?,
                    })
                }
                other => anyhow::bail!("Unknown UtilityOnboardingAction constructor: {other}"),
            }
        }

        // Credential Actions - nested variant structure
        "CredentialAction" => {
            let inner_variant = match &inner.sum {
                Some(value::Sum::Variant(v)) => v,
                _ => anyhow::bail!("Expected nested Variant for CredentialAction"),
            };
            let inner_value = inner_variant
                .value
                .as_ref()
                .context("CredentialAction inner variant has no value")?;
            let record = extract_record(inner_value)?;

            match inner_variant.constructor.as_str() {
                "Credential_OfferFreeCredential" => {
                    let claims_list = extract_list(get_field(record, "claims")?)?;
                    let claims = claims_list
                        .elements
                        .iter()
                        .map(deserialize_claim)
                        .collect::<Result<Vec<_>>>()?;

                    Ok(ActionType::CredentialOfferFree {
                        operator: extract_party_id(get_field(record, "operator")?)?,
                        user_service_cid: extract_contract_id(get_field(
                            record,
                            "userServiceCid",
                        )?)?,
                        holder: extract_party_id(get_field(record, "holder")?)?,
                        id: extract_text(get_field(record, "id")?)?,
                        description: extract_text(get_field(record, "description")?)?,
                        claims,
                    })
                }
                "Credential_AcceptFreeCredential" => Ok(ActionType::CredentialAcceptFree {
                    operator: extract_party_id(get_field(record, "operator")?)?,
                    user_service_cid: extract_contract_id(get_field(record, "userServiceCid")?)?,
                    credential_offer_cid: extract_contract_id(get_field(
                        record,
                        "credentialOfferCid",
                    )?)?,
                }),
                other => anyhow::bail!("Unknown CredentialAction constructor: {other}"),
            }
        }

        // Vault Deployment Actions - direct record
        "VaultDeploymentAction" => {
            let record = extract_record(inner)?;
            Ok(ActionType::VaultDeployment {
                vault_rules_cid: extract_contract_id(get_field(record, "vaultRulesCid")?)?,
                vault_name: extract_text(get_field(record, "vaultName")?)?,
                share_symbol: extract_text(get_field(record, "shareSymbol")?)?,
                asset_instrument_id: deserialize_instrument_id(get_field(
                    record,
                    "assetInstrumentId",
                )?)?,
                limits: deserialize_vault_limits(get_field(record, "limits")?)?,
                vault_backend_signatory: extract_party_id(get_field(
                    record,
                    "vaultBackendSignatory",
                )?)?,
                vault_far_config: deserialize_optional_far_config(get_field(
                    record,
                    "vaultFarConfig",
                )?)?,
                allocation_factory_cid: extract_contract_id(get_field(
                    record,
                    "allocationFactoryCid",
                )?)?,
                registrar_service_cid: extract_contract_id(get_field(
                    record,
                    "registrarServiceCid",
                )?)?,
            })
        }

        "YieldEpochDeploymentAction" => {
            let record = extract_record(inner)?;
            Ok(ActionType::YieldEpochDeployment {
                vault_rules_cid: extract_contract_id(get_field(record, "vaultRulesCid")?)?,
                vault_cid: extract_contract_id(get_field(record, "vaultCid")?)?,
                asset_instrument_id: deserialize_instrument_id(get_field(
                    record,
                    "assetInstrumentId",
                )?)?,
                vault_backend_signatory: extract_party_id(get_field(
                    record,
                    "vaultBackendSignatory",
                )?)?,
            })
        }

        // Vault Operations - direct record with DAML field names
        "VaultPauseAction" => {
            let record = extract_record(inner)?;
            Ok(ActionType::VaultPause {
                vault_id: extract_contract_id(get_field(record, "pauseVaultId")?)?,
            })
        }

        "VaultUnpauseAction" => {
            let record = extract_record(inner)?;
            Ok(ActionType::VaultUnpause {
                vault_id: extract_contract_id(get_field(record, "unpauseVaultId")?)?,
            })
        }

        "VaultUpdateLimitsAction" => {
            let record = extract_record(inner)?;
            Ok(ActionType::VaultUpdateLimits {
                vault_id: extract_contract_id(get_field(record, "limitsVaultId")?)?,
                new_limits: deserialize_vault_limits(get_field(record, "newLimits")?)?,
            })
        }

        "VaultUpdateBackendAction" => {
            let record = extract_record(inner)?;
            Ok(ActionType::VaultUpdateBackend {
                vault_id: extract_contract_id(get_field(record, "backendVaultId")?)?,
                new_backend_signatory: extract_party_id(get_field(record, "newBackendSignatory")?)?,
            })
        }

        "VaultUpdateFARBeneficiariesAction" => {
            let record = extract_record(inner)?;
            let beneficiaries_list = extract_list(get_field(record, "newBeneficiaries")?)?;
            let new_beneficiaries = beneficiaries_list
                .elements
                .iter()
                .map(deserialize_app_reward_beneficiary)
                .collect::<Result<Vec<_>>>()?;

            Ok(ActionType::VaultUpdateFarBeneficiaries {
                vault_id: extract_contract_id(get_field(record, "farVaultId")?)?,
                new_beneficiaries,
            })
        }

        // Processor Deployment
        "VaultProcessorDeploymentRequestAction" => {
            let record = extract_record(inner)?;
            let vaults_list = extract_list(get_field(record, "initialSupportedVaults")?)?;
            let initial_supported_vaults = vaults_list
                .elements
                .iter()
                .map(extract_contract_id)
                .collect::<Result<Vec<_>>>()?;

            Ok(ActionType::ProcessorDeploymentRequest {
                vault_processor_rules_cid: extract_contract_id(get_field(
                    record,
                    "vaultProcessorRulesCid",
                )?)?,
                vault_backend_signatory: extract_party_id(get_field(
                    record,
                    "vaultBackendSignatory",
                )?)?,
                allocation_factory_cid: extract_contract_id(get_field(
                    record,
                    "allocationFactoryCid",
                )?)?,
                processor_far_config: deserialize_optional_far_config(get_field(
                    record,
                    "processorFarConfig",
                )?)?,
                initial_supported_vaults,
            })
        }

        // DevNet
        "DevNetFeatureAppAction" => {
            let record = extract_record(inner)?;
            Ok(ActionType::DevNetFeatureApp {
                amulet_rules_cid: extract_contract_id(get_field(record, "amuletRulesCid")?)?,
            })
        }

        other => anyhow::bail!("Unknown action constructor: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::canton_id::{NAMESPACE_LENGTH, Namespace};

    #[test]
    fn transfer_validity_from_now_bounds_the_window() {
        let now = 1_700_000_000_000_000;
        let v = TransferValidity::from_now(now);
        assert_eq!(v.requested_at_micros, now);
        assert_eq!(
            v.execute_before_micros,
            now + TRANSFER_VALIDITY_WINDOW_MICROS
        );
        // The window is finite (24h), not the old effectively-infinite deadline.
        assert!(v.execute_before_micros < TRANSFER_EXECUTE_BEFORE_MICROS);
    }

    #[test]
    fn transfer_validity_from_now_clamps_to_max_daml_time() {
        // A near-max `now` must neither panic on overflow nor serialize past the
        // module's max Daml `Time`; it clamps to TRANSFER_EXECUTE_BEFORE_MICROS.
        let v = TransferValidity::from_now(i64::MAX - 5);
        assert_eq!(v.execute_before_micros, TRANSFER_EXECUTE_BEFORE_MICROS);
    }

    // Locks the AV_* constructor strings against the on-ledger
    // `Splice.Api.Token.MetadataV1.AnyValue` definition. A typo here would
    // surface as a runtime interpretation error far from the source, so the
    // mapping for every supported `ContextValue` variant is asserted explicitly.
    #[test]
    fn make_any_value_maps_each_variant_to_expected_ctor() {
        let cases: Vec<(ContextValue, &str)> = vec![
            (ContextValue::Text("hi".to_string()), "AV_Text"),
            (ContextValue::Int(42), "AV_Int"),
            (
                ContextValue::Decimal(DamlDecimal::parse("1.5").unwrap()),
                "AV_Decimal",
            ),
            (ContextValue::Bool(true), "AV_Bool"),
            (ContextValue::Party("alice::pid".to_string()), "AV_Party"),
            (
                ContextValue::ContractId("cid-1".to_string()),
                "AV_ContractId",
            ),
            (ContextValue::List(vec![ContextValue::Int(1)]), "AV_List"),
            (
                ContextValue::Map(HashMap::from([(
                    "k".to_string(),
                    ContextValue::Text("v".to_string()),
                )])),
                "AV_Map",
            ),
        ];

        for (input, expected_ctor) in cases {
            let value = make_any_value(&input).expect("make_any_value succeeded");
            match value.sum {
                Some(value::Sum::Variant(v)) => assert_eq!(
                    v.constructor, expected_ctor,
                    "wrong constructor for {input:?}",
                ),
                other => panic!("expected Variant for {input:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn make_any_value_recurses_into_nested_map() {
        let nested = ContextValue::Map(HashMap::from([
            ("text".to_string(), ContextValue::Text("hi".to_string())),
            (
                "cid".to_string(),
                ContextValue::ContractId("cid-1".to_string()),
            ),
            (
                "nested".to_string(),
                ContextValue::Map(HashMap::from([("n".to_string(), ContextValue::Int(7))])),
            ),
        ]));

        let value = make_any_value(&nested).expect("nested map serializes");
        let Some(value::Sum::Variant(v)) = value.sum else {
            panic!("expected Variant");
        };
        assert_eq!(v.constructor, "AV_Map");
    }

    #[test]
    fn make_any_value_rejects_unsupported_time_variants() {
        for unsupported in [
            ContextValue::Date("2026-05-19".to_string()),
            ContextValue::Time("2026-05-19T00:00:00Z".to_string()),
            ContextValue::RelTime("PT1H".to_string()),
        ] {
            let err = make_any_value(&unsupported).expect_err("must reject");
            assert!(
                err.to_string().contains("ContextValue::"),
                "error should reference the Rust type, got: {err}",
            );
        }
    }

    // ---- ActionType / ProposalType wire-shape assertions ----
    //
    // These lock the DAML constructor names and field labels emitted for the
    // governance actions. The labels are hand-written and consumed by the
    // on-ledger interpreter, so a typo or a swap between the two governance
    // serializers (`serialize_action` vs `serialize_self_action`, which
    // deliberately use *different* labels for the same action) would only
    // surface as a runtime interpretation error far from the source. A
    // round-trip test cannot catch a wrong-but-symmetric label; explicit
    // label assertions can.

    /// Any valid `CantonId` — the exact value is irrelevant to these
    /// constructor/field-name assertions.
    fn party_id() -> CantonId {
        CantonId::new("p".to_string(), Namespace::new([0u8; NAMESPACE_LENGTH]))
    }

    /// Unwrap a `Variant` value into `(constructor, inner)`.
    fn as_variant(value: &Value) -> (&str, &Value) {
        match &value.sum {
            Some(value::Sum::Variant(v)) => match v.value.as_deref() {
                Some(inner) => (v.constructor.as_str(), inner),
                None => panic!("variant {} has no inner value", v.constructor),
            },
            other => panic!("expected Variant, got {other:?}"),
        }
    }

    /// The ordered field labels of a `Record` value.
    fn record_labels(value: &Value) -> Vec<&str> {
        match &value.sum {
            Some(value::Sum::Record(r)) => r.fields.iter().map(|f| f.label.as_str()).collect(),
            other => panic!("expected Record, got {other:?}"),
        }
    }

    #[test]
    fn serialize_action_add_member_shape() {
        let action = ActionType::GovernanceAddMember {
            member: party_id(),
            new_threshold: 3,
        };
        let value = serialize_action(&action);
        let (outer, inner) = as_variant(&value);
        assert_eq!(outer, "GovernanceAction");
        let (ctor, record) = as_variant(inner);
        assert_eq!(ctor, "Governance_AddMemberAndSetThreshold");
        assert_eq!(record_labels(record), ["member", "newThreshold"]);
    }

    #[test]
    fn serialize_action_set_threshold_and_timeout_shape() {
        let threshold = serialize_action(&ActionType::GovernanceSetThreshold { new_threshold: 2 });
        let (outer, inner) = as_variant(&threshold);
        assert_eq!(outer, "GovernanceAction");
        let (ctor, record) = as_variant(inner);
        assert_eq!(ctor, "Governance_SetThreshold");
        assert_eq!(record_labels(record), ["newThreshold"]);

        let timeout = serialize_action(&ActionType::GovernanceSetTimeout {
            new_timeout_microseconds: 1_000,
        });
        let (_, inner) = as_variant(&timeout);
        let (ctor, record) = as_variant(inner);
        assert_eq!(ctor, "Governance_SetActionConfirmationTimeout");
        assert_eq!(record_labels(record), ["newActionConfirmationTimeout"]);
    }

    #[test]
    fn serialize_action_utility_and_credential_and_devnet_shapes() {
        let setup = serialize_action(&ActionType::UtilitySetup {
            operator: party_id(),
            provider_service_cid: "psc".to_string(),
            user_service_cid: "usc".to_string(),
        });
        let (outer, inner) = as_variant(&setup);
        assert_eq!(outer, "UtilityOnboardingAction");
        let (ctor, record) = as_variant(inner);
        assert_eq!(ctor, "UtilityOnboarding_SetupUtility");
        assert_eq!(
            record_labels(record),
            ["operator", "providerServiceCid", "userServiceCid"]
        );

        let accept = serialize_action(&ActionType::CredentialAcceptFree {
            operator: party_id(),
            user_service_cid: "usc".to_string(),
            credential_offer_cid: "coc".to_string(),
        });
        let (outer, inner) = as_variant(&accept);
        assert_eq!(outer, "CredentialAction");
        let (ctor, record) = as_variant(inner);
        assert_eq!(ctor, "Credential_AcceptFreeCredential");
        assert_eq!(
            record_labels(record),
            ["operator", "userServiceCid", "credentialOfferCid"]
        );

        // DevNet wraps a bare record (no nested action variant).
        let devnet = serialize_action(&ActionType::DevNetFeatureApp {
            amulet_rules_cid: "arc".to_string(),
        });
        let (ctor, record) = as_variant(&devnet);
        assert_eq!(ctor, "DevNetFeatureAppAction");
        assert_eq!(record_labels(record), ["amuletRulesCid"]);
    }

    #[test]
    fn serialize_self_action_uses_distinct_labels_from_serialize_action() {
        // The self-management serializer maps the SAME ActionType to DIFFERENT
        // constructor + field names than `serialize_action`. Pin both so the
        // two paths can't silently converge or drift.
        let add = serialize_self_action(&ActionType::GovernanceAddMember {
            member: party_id(),
            new_threshold: 3,
        });
        let (ctor, record) = as_variant(&add);
        assert_eq!(ctor, "SelfAction_AddMemberAndSetThreshold");
        assert_eq!(record_labels(record), ["newMember", "newThresholdAfterAdd"]);

        let remove = serialize_self_action(&ActionType::GovernanceRemoveMember {
            member: party_id(),
            new_threshold: 1,
        });
        let (ctor, record) = as_variant(&remove);
        assert_eq!(ctor, "SelfAction_RemoveMemberAndSetThreshold");
        assert_eq!(
            record_labels(record),
            ["removedMember", "newThresholdAfterRemove"]
        );

        let set_threshold =
            serialize_self_action(&ActionType::GovernanceSetThreshold { new_threshold: 2 });
        let (ctor, record) = as_variant(&set_threshold);
        assert_eq!(ctor, "SelfAction_SetThreshold");
        assert_eq!(record_labels(record), ["updatedThreshold"]);
    }

    #[test]
    fn build_proposal_setup_cc_preapproval_shape() -> Result {
        let proposal = ProposalType::SetupCcPreapproval {
            provider: party_id(),
            expected_dso: party_id(),
        };
        let (package, module, entity, record) =
            build_proposal_create_args("gov", "proposer", &proposal, None, None)?;

        assert_eq!(package, ProposalPackage::GovernanceTokenCustody);
        assert_eq!(module, "Governance.TokenCustody.SetupCcPreapproval");
        assert_eq!(entity, "SetupCcPreapprovalProposal");
        let labels: Vec<&str> = record.fields.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(
            labels,
            ["governanceParty", "proposer", "provider", "expectedDso"]
        );
        Ok(())
    }

    // ---- build_proposal_create_args financial-arm wire-shape assertions ----
    //
    // These lock the (package, module, entity) routing triple plus the ordered
    // field labels for the proposal arms whose payloads carry money or descend
    // into nested records. The module/entity strings select the on-ledger
    // package+template, and the labels are consumed verbatim by Canton's command
    // preprocessor — a typo or reordering surfaces only as a runtime
    // interpretation failure, so each is pinned explicitly here.

    /// Fetch a nested field's `Value` by label from an owned `Record`. Mirrors
    /// the production `get_field` but panics (these are assertions, not
    /// recoverable paths) so call sites stay terse.
    fn field_value<'a>(record: &'a Record, label: &str) -> &'a Value {
        record
            .fields
            .iter()
            .find(|f| f.label == label)
            .and_then(|f| f.value.as_ref())
            .unwrap_or_else(|| panic!("missing field {label}"))
    }

    /// The ordered field labels of an owned `Record`.
    fn owned_labels(record: &Record) -> Vec<&str> {
        record.fields.iter().map(|f| f.label.as_str()).collect()
    }

    /// Unwrap a `value::Sum::Record` reference (for descending into a nested
    /// record `Value` returned by `field_value`).
    fn as_record(value: &Value) -> &Record {
        match &value.sum {
            Some(value::Sum::Record(r)) => r,
            other => panic!("expected Record, got {other:?}"),
        }
    }

    #[test]
    fn build_proposal_transfer_shape_and_nested_records() -> Result {
        let proposal = ProposalType::Transfer {
            transfer_factory_cid: "tfc".to_string(),
            expected_admin: party_id(),
            receiver: party_id(),
            amount: DamlDecimal::parse("1.5")?,
            instrument_id: InstrumentId {
                admin: "admin::ns".to_string(),
                id: "instr-1".to_string(),
            },
            input_holding_cids: vec!["hc-1".to_string()],
        };
        let (package, module, entity, record) =
            build_proposal_create_args("gov", "proposer", &proposal, None, None)?;

        assert_eq!(package, ProposalPackage::GovernanceTokenCustody);
        assert_eq!(module, "Governance.TokenCustody.TransferProposal");
        assert_eq!(entity, "TransferProposal");
        assert_eq!(
            owned_labels(&record),
            [
                "governanceParty",
                "proposer",
                "transferFactoryCid",
                "expectedAdmin",
                "transfer",
                "extraArgs",
            ]
        );

        // Descend into the nested `transfer` record.
        let transfer = as_record(field_value(&record, "transfer"));
        assert_eq!(
            owned_labels(transfer),
            [
                "sender",
                "receiver",
                "amount",
                "instrumentId",
                "requestedAt",
                "executeBefore",
                "inputHoldingCids",
                "meta",
            ]
        );

        // Nested `instrumentId` record.
        let instrument_id = as_record(field_value(transfer, "instrumentId"));
        assert_eq!(owned_labels(instrument_id), ["admin", "id"]);

        // Placeholder timestamps must be the exposed constants so propose-time
        // and execute-time payloads match (registrar resolves the context for
        // these exact choice arguments).
        assert!(matches!(
            field_value(transfer, "requestedAt").sum,
            Some(value::Sum::Timestamp(TRANSFER_REQUESTED_AT_MICROS)),
        ));
        assert!(matches!(
            field_value(transfer, "executeBefore").sum,
            Some(value::Sum::Timestamp(TRANSFER_EXECUTE_BEFORE_MICROS)),
        ));
        assert!(matches!(
            field_value(transfer, "amount").sum,
            Some(value::Sum::Numeric(_)),
        ));
        Ok(())
    }

    #[test]
    fn build_proposal_mint_and_burn_shapes_differ_only_in_party_label() -> Result {
        let mint = ProposalType::Mint {
            allocation_factory_cid: "afc".to_string(),
            instrument_id: InstrumentId {
                admin: "admin::ns".to_string(),
                id: "instr-1".to_string(),
            },
            instrument_configuration_cid: "icc".to_string(),
            recipient: party_id(),
            amount: DamlDecimal::parse("1.5")?,
            description: "mint it".to_string(),
        };
        let (mint_package, mint_module, mint_entity, mint_record) =
            build_proposal_create_args("gov", "proposer", &mint, None, None)?;

        // Package enum is GovernanceUtilityOnboarding even though the module
        // lives under `Governance.TokenIssuance`.
        assert_eq!(mint_package, ProposalPackage::GovernanceUtilityOnboarding);
        assert_eq!(mint_module, "Governance.TokenIssuance.MintProposal");
        assert_eq!(mint_entity, "MintProposal");
        assert_eq!(
            owned_labels(&mint_record),
            [
                "governanceParty",
                "proposer",
                "allocationFactoryCid",
                "instrumentId",
                "instrumentConfigurationCid",
                "recipient",
                "amount",
                "description",
                "requestedAt",
                "executeBefore",
                "meta",
                "extraArgsMeta",
            ]
        );

        let burn = ProposalType::Burn {
            allocation_factory_cid: "afc".to_string(),
            instrument_id: InstrumentId {
                admin: "admin::ns".to_string(),
                id: "instr-1".to_string(),
            },
            instrument_configuration_cid: "icc".to_string(),
            holder: party_id(),
            amount: DamlDecimal::parse("1.5")?,
            description: "burn it".to_string(),
        };
        let (burn_package, burn_module, burn_entity, burn_record) =
            build_proposal_create_args("gov", "proposer", &burn, None, None)?;

        assert_eq!(burn_package, ProposalPackage::GovernanceUtilityOnboarding);
        assert_eq!(burn_module, "Governance.TokenIssuance.BurnProposal");
        assert_eq!(burn_entity, "BurnProposal");
        assert_eq!(
            owned_labels(&burn_record),
            [
                "governanceParty",
                "proposer",
                "allocationFactoryCid",
                "instrumentId",
                "instrumentConfigurationCid",
                "holder",
                "amount",
                "description",
                "requestedAt",
                "executeBefore",
                "meta",
                "extraArgsMeta",
            ]
        );

        // The ONLY structural difference between the two arms is the party
        // label: Mint carries `recipient`, Burn carries `holder`.
        assert!(owned_labels(&mint_record).contains(&"recipient"));
        assert!(!owned_labels(&mint_record).contains(&"holder"));
        assert!(owned_labels(&burn_record).contains(&"holder"));
        assert!(!owned_labels(&burn_record).contains(&"recipient"));

        // Both carry the two trailing metadata fields.
        assert!(owned_labels(&mint_record).contains(&"meta"));
        assert!(owned_labels(&mint_record).contains(&"extraArgsMeta"));
        assert!(owned_labels(&burn_record).contains(&"meta"));
        assert!(owned_labels(&burn_record).contains(&"extraArgsMeta"));
        Ok(())
    }

    #[test]
    fn build_proposal_accept_transfer_shape_and_context_branches() -> Result {
        let proposal = ProposalType::AcceptTransfer {
            transfer_instruction_cid: "tic".to_string(),
        };

        // ---- No choice context: context.values is an EMPTY TextMap ----
        let (package, module, entity, record) =
            build_proposal_create_args("gov", "proposer", &proposal, None, None)?;
        assert_eq!(package, ProposalPackage::GovernanceTokenCustody);
        assert_eq!(module, "Governance.TokenCustody.AcceptTransfer");
        assert_eq!(entity, "AcceptTransferProposal");
        assert_eq!(
            owned_labels(&record),
            [
                "governanceParty",
                "proposer",
                "transferInstructionCid",
                "extraArgs",
            ]
        );

        // extraArgs -> context -> values must be a TextMap (NOT a GenMap),
        // empty, when no context was supplied.
        let extra_args = as_record(field_value(&record, "extraArgs"));
        let context = as_record(field_value(extra_args, "context"));
        let values = field_value(context, "values");
        match &values.sum {
            Some(value::Sum::TextMap(tm)) => assert!(
                tm.entries.is_empty(),
                "empty-context branch must yield an empty TextMap",
            ),
            other => panic!("expected empty TextMap for context.values, got {other:?}"),
        }

        // ---- With a choice context: one keyed AV_ContractId entry ----
        let key = "utility.digitalasset.com/transfer-rule".to_string();
        let ctx = ChoiceContext {
            values: HashMap::from([(
                key.clone(),
                ContextValue::ContractId("rule-cid".to_string()),
            )]),
        };
        let (_, _, _, record) =
            build_proposal_create_args("gov", "proposer", &proposal, Some(&ctx), None)?;
        let extra_args = as_record(field_value(&record, "extraArgs"));
        let context = as_record(field_value(extra_args, "context"));
        let values = field_value(context, "values");
        match &values.sum {
            Some(value::Sum::TextMap(tm)) => {
                assert_eq!(tm.entries.len(), 1, "exactly one context entry");
                let entry = &tm.entries[0];
                assert_eq!(entry.key, key);
                let entry_value = entry
                    .value
                    .as_ref()
                    .unwrap_or_else(|| panic!("context entry has no value"));
                let (ctor, _) = as_variant(entry_value);
                assert_eq!(ctor, "AV_ContractId");
            }
            other => panic!("expected populated TextMap for context.values, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn build_proposal_offer_paid_credential_shape_and_billing_params() -> Result {
        let proposal = ProposalType::OfferPaidCredential {
            user_service_cid: "usc".to_string(),
            holder: party_id(),
            id: "cred-1".to_string(),
            description: "paid".to_string(),
            claims: vec![Claim {
                subject: "s".to_string(),
                property: "p".to_string(),
                value: "v".to_string(),
            }],
            billing_params: BillingParams {
                fee_per_day_usd: DamlDecimal::parse("1.5")?,
                billing_period_minutes: 60,
                deposit_target_amount_usd: DamlDecimal::parse("10.0")?,
                holder_activity_weight: Some(DamlDecimal::parse("0.5")?),
            },
            deposit_initial_amount_usd: Some(DamlDecimal::parse("5.0")?),
        };
        let (package, module, entity, record) =
            build_proposal_create_args("gov", "proposer", &proposal, None, None)?;

        assert_eq!(package, ProposalPackage::GovernanceUtilityCredential);
        assert_eq!(module, "Governance.UtilityCredential.OfferPaidCredential");
        assert_eq!(entity, "OfferPaidCredential");
        assert_eq!(
            owned_labels(&record),
            [
                "governanceParty",
                "proposer",
                "userServiceCid",
                "holder",
                "id",
                "description",
                "claims",
                "billingParams",
                "depositInitialAmountUsd",
            ]
        );

        // Descend into `billingParams`.
        let billing = as_record(field_value(&record, "billingParams"));
        assert_eq!(
            owned_labels(billing),
            [
                "feePerDayUsd",
                "billingPeriodMinutes",
                "depositTargetAmountUsd",
                "holderActivityWeight",
            ]
        );

        // `feePerDayUsd` is itself a record wrapping a single `rate` field.
        let fee = as_record(field_value(billing, "feePerDayUsd"));
        assert_eq!(owned_labels(fee), ["rate"]);
        Ok(())
    }

    #[test]
    fn build_proposal_setup_utility_shape_and_nested_identifier() -> Result {
        let proposal = ProposalType::SetupUtility {
            provider_service_cid: "psc".to_string(),
            operator: party_id(),
            instrument_id_text: "uuid-1".to_string(),
            additional_identifiers: vec![InstrumentIdentifier {
                source: party_id(),
                id: "TICK".to_string(),
                scheme: "Ticker".to_string(),
            }],
            create_transfer_rule: true,
            create_allocation_factory: false,
        };
        let (package, module, entity, record) =
            build_proposal_create_args("gov", "proposer", &proposal, None, None)?;

        assert_eq!(package, ProposalPackage::GovernanceUtilityOnboarding);
        assert_eq!(module, "Governance.UtilityOnboarding.SetupUtility");
        assert_eq!(entity, "SetupUtility");
        assert_eq!(
            owned_labels(&record),
            [
                "governanceParty",
                "proposer",
                "providerServiceCid",
                "operator",
                "instrumentIdText",
                "additionalIdentifiers",
                "createTransferRule",
                "createAllocationFactory",
            ]
        );

        // Descend into the first element of the `additionalIdentifiers` list.
        let identifiers = field_value(&record, "additionalIdentifiers");
        let first = match &identifiers.sum {
            Some(value::Sum::List(l)) => l
                .elements
                .first()
                .unwrap_or_else(|| panic!("additionalIdentifiers list is empty")),
            other => panic!("expected List for additionalIdentifiers, got {other:?}"),
        };
        assert_eq!(owned_labels(as_record(first)), ["source", "id", "scheme"]);
        Ok(())
    }

    #[test]
    fn build_proposal_flat_record_arms_route_and_label_correctly() -> Result {
        // Table-driven coverage for the trivial flat-record arms: pins the
        // (package, module, entity) routing triple + ordered labels. The
        // module/entity strings select the on-ledger package+template.
        struct Case {
            proposal: ProposalType,
            package: ProposalPackage,
            module: &'static str,
            entity: &'static str,
            labels: &'static [&'static str],
        }

        let cases = vec![
            Case {
                proposal: ProposalType::SetupTokenPreapproval {
                    operator: party_id(),
                    instrument_admin: party_id(),
                    instrument_allowances: vec![InstrumentAllowance {
                        id: "allow-1".to_string(),
                    }],
                },
                package: ProposalPackage::GovernanceTokenCustody,
                module: "Governance.TokenCustody.SetupTokenPreapproval",
                entity: "SetupTokenPreapprovalProposal",
                labels: &[
                    "governanceParty",
                    "proposer",
                    "operator",
                    "instrumentAdmin",
                    "instrumentAllowances",
                ],
            },
            Case {
                proposal: ProposalType::CreateProviderServiceRequest {
                    operator: party_id(),
                    provider: party_id(),
                },
                package: ProposalPackage::GovernanceUtilityOnboarding,
                module: "Governance.UtilityOnboarding.CreateProviderServiceRequest",
                entity: "CreateProviderServiceRequest",
                labels: &["governanceParty", "proposer", "operator", "provider"],
            },
            Case {
                proposal: ProposalType::CreateUserServiceRequest {
                    operator: party_id(),
                    user: party_id(),
                },
                package: ProposalPackage::GovernanceUtilityOnboarding,
                module: "Governance.UtilityOnboarding.CreateUserServiceRequest",
                entity: "CreateUserServiceRequest",
                labels: &["governanceParty", "proposer", "operator", "user"],
            },
            Case {
                proposal: ProposalType::AcceptFreeCredential {
                    user_service_cid: "usc".to_string(),
                    credential_offer_cid: "coc".to_string(),
                },
                package: ProposalPackage::GovernanceUtilityCredential,
                module: "Governance.UtilityCredential.AcceptFreeCredential",
                entity: "AcceptFreeCredential",
                labels: &[
                    "governanceParty",
                    "proposer",
                    "userServiceCid",
                    "credentialOfferCid",
                ],
            },
            Case {
                proposal: ProposalType::AcceptMintRequest {
                    mint_request_cid: "mrc".to_string(),
                    instrument_configuration_cid: "icc".to_string(),
                    description: "accept mint".to_string(),
                },
                package: ProposalPackage::GovernanceUtilityOnboarding,
                module: "Governance.TokenIssuance.AcceptMintRequest",
                entity: "AcceptMintRequest",
                labels: &[
                    "governanceParty",
                    "proposer",
                    "mintRequestCid",
                    "instrumentConfigurationCid",
                    "description",
                    "extraArgsMeta",
                ],
            },
            Case {
                proposal: ProposalType::AcceptBurnRequest {
                    burn_request_cid: "brc".to_string(),
                    instrument_configuration_cid: "icc".to_string(),
                    description: "accept burn".to_string(),
                },
                package: ProposalPackage::GovernanceUtilityOnboarding,
                module: "Governance.TokenIssuance.AcceptBurnRequest",
                entity: "AcceptBurnRequest",
                labels: &[
                    "governanceParty",
                    "proposer",
                    "burnRequestCid",
                    "instrumentConfigurationCid",
                    "description",
                    "extraArgsMeta",
                ],
            },
        ];

        for case in cases {
            let (package, module, entity, record) =
                build_proposal_create_args("gov", "proposer", &case.proposal, None, None)?;
            assert_eq!(package, case.package, "package for {module}");
            assert_eq!(module, case.module);
            assert_eq!(entity, case.entity, "entity for {module}");
            assert_eq!(owned_labels(&record), case.labels, "labels for {module}");
        }
        Ok(())
    }
}
