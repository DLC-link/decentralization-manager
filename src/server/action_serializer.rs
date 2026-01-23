//! Serialization of ActionType to DAML Values for Vault Governance
//!
//! This module provides bidirectional conversion between `ActionType` enum
//! and DAML `Value` representations for use with the Ledger API.

use anyhow::Context;
use canton_proto_rs::com::daml::ledger::api::v2::{
    List, Optional, Record, RecordField, Value, Variant, value,
};

use crate::error::Result;

use super::types::{ActionType, AppRewardBeneficiary, FarConfig, InstrumentId, VaultLimits};

// ============================================================================
// Helper Functions
// ============================================================================

fn make_party(p: &str) -> Value {
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

// ============================================================================
// Complex Type Serializers
// ============================================================================

fn serialize_instrument_id(id: &InstrumentId) -> Value {
    make_record(vec![
        field("issuer", make_party(&id.issuer)),
        field("symbol", make_text(&id.symbol)),
    ])
}

fn serialize_vault_limits(limits: &VaultLimits) -> Value {
    make_record(vec![
        field("maxTotalDeposit", make_numeric(&limits.max_total_deposit)),
        field("minDepositAmount", make_numeric(&limits.min_deposit_amount)),
        field(
            "minWithdrawalAmount",
            make_numeric(&limits.min_withdrawal_amount),
        ),
    ])
}

fn serialize_app_reward_beneficiary(b: &AppRewardBeneficiary) -> Value {
    make_record(vec![
        field("beneficiary", make_party(&b.beneficiary)),
        field("weight", make_numeric(&b.weight)),
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

        // DevNet
        ActionType::DevNetFeatureApp { amulet_rules_cid } => make_variant(
            "DevNetFeatureAppAction",
            make_record(vec![field(
                "amuletRulesCid",
                make_contract_id(amulet_rules_cid),
            )]),
        ),
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
// Deserialization Helpers
// ============================================================================

fn extract_party(value: &Value) -> Result<String> {
    match &value.sum {
        Some(value::Sum::Party(p)) => Ok(p.clone()),
        _ => anyhow::bail!("Expected Party value"),
    }
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
        issuer: extract_party(get_field(record, "issuer")?)?,
        symbol: extract_text(get_field(record, "symbol")?)?,
    })
}

fn deserialize_vault_limits(value: &Value) -> Result<VaultLimits> {
    let record = extract_record(value)?;
    Ok(VaultLimits {
        max_total_deposit: extract_numeric(get_field(record, "maxTotalDeposit")?)?,
        min_deposit_amount: extract_numeric(get_field(record, "minDepositAmount")?)?,
        min_withdrawal_amount: extract_numeric(get_field(record, "minWithdrawalAmount")?)?,
    })
}

fn deserialize_app_reward_beneficiary(value: &Value) -> Result<AppRewardBeneficiary> {
    let record = extract_record(value)?;
    Ok(AppRewardBeneficiary {
        beneficiary: extract_party(get_field(record, "beneficiary")?)?,
        weight: extract_numeric(get_field(record, "weight")?)?,
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
                    member: extract_party(get_field(record, "member")?)?,
                    new_threshold: extract_int64(get_field(record, "newThreshold")?)?,
                }),
                "Governance_RemoveMemberAndSetThreshold" => {
                    Ok(ActionType::GovernanceRemoveMember {
                        member: extract_party(get_field(record, "member")?)?,
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
                        operator: extract_party(get_field(record, "operator")?)?,
                    })
                }
                "UtilityOnboarding_CreateUserServiceRequest" => {
                    Ok(ActionType::UtilityCreateUserRequest {
                        operator: extract_party(get_field(record, "operator")?)?,
                    })
                }
                "UtilityOnboarding_SetupUtility" => Ok(ActionType::UtilitySetup {
                    operator: extract_party(get_field(record, "operator")?)?,
                    provider_service_cid: extract_contract_id(get_field(
                        record,
                        "providerServiceCid",
                    )?)?,
                    user_service_cid: extract_contract_id(get_field(record, "userServiceCid")?)?,
                }),
                other => anyhow::bail!("Unknown UtilityOnboardingAction constructor: {other}"),
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
                vault_backend_signatory: extract_party(get_field(
                    record,
                    "vaultBackendSignatory",
                )?)?,
                vault_far_config: deserialize_optional_far_config(get_field(
                    record,
                    "vaultFarConfig",
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
                vault_backend_signatory: extract_party(get_field(
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
                new_backend_signatory: extract_party(get_field(record, "newBackendSignatory")?)?,
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
                vault_backend_signatory: extract_party(get_field(
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
