//! Serialization of ActionType to DAML Values for Vault Governance
//!
//! This module provides bidirectional conversion between `ActionType` enum
//! and DAML `Value` representations for use with the Ledger API.

use canton_proto_rs::com::daml::ledger::api::v2::{
    List, Optional, Record, RecordField, Value, Variant, value,
};

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

// ============================================================================
// Action Serialization
// ============================================================================

/// Serialize an ActionType to a DAML Value (ActionRequiringConfirmation variant)
pub fn serialize_action(action: &ActionType) -> Value {
    match action {
        // Governance Actions
        ActionType::GovernanceAddMember {
            member,
            new_threshold,
        } => make_variant(
            "GovernanceAddMember",
            make_record(vec![
                field("member", make_party(member)),
                field("newThreshold", make_int64(*new_threshold)),
            ]),
        ),

        ActionType::GovernanceRemoveMember {
            member,
            new_threshold,
        } => make_variant(
            "GovernanceRemoveMember",
            make_record(vec![
                field("member", make_party(member)),
                field("newThreshold", make_int64(*new_threshold)),
            ]),
        ),

        ActionType::GovernanceSetThreshold { new_threshold } => make_variant(
            "GovernanceSetThreshold",
            make_record(vec![field("newThreshold", make_int64(*new_threshold))]),
        ),

        ActionType::GovernanceSetTimeout {
            new_timeout_microseconds,
        } => make_variant(
            "GovernanceSetTimeout",
            make_record(vec![field(
                "newTimeoutMicroseconds",
                make_int64(*new_timeout_microseconds),
            )]),
        ),

        // Vault Deployment Actions
        ActionType::VaultDeployment {
            vault_name,
            share_symbol,
            asset_instrument_id,
            limits,
            vault_manager,
            vault_backend_signatory,
            vault_far_config,
        } => make_variant(
            "VaultDeploymentAction",
            make_record(vec![
                field("vaultName", make_text(vault_name)),
                field("shareSymbol", make_text(share_symbol)),
                field(
                    "assetInstrumentId",
                    serialize_instrument_id(asset_instrument_id),
                ),
                field("limits", serialize_vault_limits(limits)),
                field("vaultManager", make_party(vault_manager)),
                field("vaultBackendSignatory", make_party(vault_backend_signatory)),
                field("vaultFarConfig", serialize_far_config(vault_far_config)),
            ]),
        ),

        ActionType::YieldEpochDeployment {
            vault_cid,
            vault_manager,
            asset_instrument_id,
            vault_backend_signatory,
        } => make_variant(
            "YieldEpochDeploymentAction",
            make_record(vec![
                field("vaultCid", make_contract_id(vault_cid)),
                field("vaultManager", make_party(vault_manager)),
                field(
                    "assetInstrumentId",
                    serialize_instrument_id(asset_instrument_id),
                ),
                field("vaultBackendSignatory", make_party(vault_backend_signatory)),
            ]),
        ),

        // Vault Operations
        ActionType::VaultPause { vault_id } => make_variant(
            "VaultPauseAction",
            make_record(vec![field("vaultId", make_contract_id(vault_id))]),
        ),

        ActionType::VaultUnpause { vault_id } => make_variant(
            "VaultUnpauseAction",
            make_record(vec![field("vaultId", make_contract_id(vault_id))]),
        ),

        ActionType::VaultUpdateLimits {
            vault_id,
            new_limits,
        } => make_variant(
            "VaultUpdateLimitsAction",
            make_record(vec![
                field("vaultId", make_contract_id(vault_id)),
                field("newLimits", serialize_vault_limits(new_limits)),
            ]),
        ),

        ActionType::VaultUpdateBackend {
            vault_id,
            new_backend_signatory,
        } => make_variant(
            "VaultUpdateBackendAction",
            make_record(vec![
                field("vaultId", make_contract_id(vault_id)),
                field("newBackendSignatory", make_party(new_backend_signatory)),
            ]),
        ),

        ActionType::VaultUpdateFarBeneficiaries {
            vault_id,
            new_beneficiaries,
        } => make_variant(
            "VaultUpdateFarBeneficiariesAction",
            make_record(vec![
                field("vaultId", make_contract_id(vault_id)),
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

        // Processor
        ActionType::ProcessorDeploymentRequest {
            authorized_vault_manager,
            vault_backend_signatory,
            allocation_factory_cid,
            processor_far_config,
            initial_supported_vaults,
        } => make_variant(
            "ProcessorDeploymentRequestAction",
            make_record(vec![
                field(
                    "authorizedVaultManager",
                    make_party(authorized_vault_manager),
                ),
                field("vaultBackendSignatory", make_party(vault_backend_signatory)),
                field(
                    "allocationFactoryCid",
                    make_contract_id(allocation_factory_cid),
                ),
                field(
                    "processorFarConfig",
                    serialize_far_config(processor_far_config),
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

        // Utility Onboarding
        ActionType::UtilityCreateProviderRequest { operator } => make_variant(
            "UtilityCreateProviderRequestAction",
            make_record(vec![field("operator", make_party(operator))]),
        ),

        ActionType::UtilityCreateUserRequest { operator } => make_variant(
            "UtilityCreateUserRequestAction",
            make_record(vec![field("operator", make_party(operator))]),
        ),

        ActionType::UtilitySetup {
            operator,
            provider_service_cid,
            user_service_cid,
        } => make_variant(
            "UtilitySetupAction",
            make_record(vec![
                field("operator", make_party(operator)),
                field("providerServiceCid", make_contract_id(provider_service_cid)),
                field("userServiceCid", make_contract_id(user_service_cid)),
            ]),
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
