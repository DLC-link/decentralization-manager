//! `ActionType` — decman's governance action payload — and its Daml `Value`
//! codec.
//!
//! The Daml side splits this one Rust enum across two closed unions: vault
//! governance's `ActionRequiringConfirmation` and `governance-core`'s
//! `GovernanceSelfAction`. [`ActionType::to_vault_proto`] /
//! [`ActionType::from_vault_proto`] encode/decode the first form;
//! [`ActionType::to_self_proto`] / [`ActionType::from_self_proto`] the
//! second. A variant that exists in only one form returns `Error::Encode`
//! from the other form's encoder rather than panicking.

use canton_proto_rs::com::daml::ledger::api::v2::{Value, value};
use common::api::{Claim, InstrumentId};
use common::canton_id::CantonId;

use crate::catalog::types::{
    AppRewardBeneficiary, FarConfig, VaultLimits, deserialize_app_reward_beneficiary,
    deserialize_claim, deserialize_instrument_id, deserialize_optional_far_config,
    deserialize_reltime, deserialize_vault_limits, serialize_app_reward_beneficiary,
    serialize_optional_far_config, serialize_vault_limits,
};
use crate::error::Error;
use crate::framework::encode::{
    field, make_contract_id, make_int64, make_list, make_party, make_record, make_text,
    make_variant, serialize_claim, serialize_instrument_id, serialize_reltime,
};
use crate::framework::record::{
    extract_contract_id, extract_int64, extract_list, extract_party_id, extract_record,
    extract_text, get_field,
};
use crate::framework::validate::{
    validate_beneficiary_weights, validate_threshold, validate_timeout,
};

/// Structured action types for Vault governance
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionType {
    // Governance (4)
    GovernanceAddMember {
        member: CantonId,
        new_threshold: i64,
    },
    GovernanceRemoveMember {
        member: CantonId,
        new_threshold: i64,
    },
    GovernanceSetThreshold {
        new_threshold: i64,
    },
    GovernanceSetTimeout {
        new_timeout_microseconds: i64,
    },
    GovernanceAddAdditionalProposer {
        additional_proposer: CantonId,
    },
    GovernanceRemoveAdditionalProposer {
        additional_proposer: CantonId,
    },

    // Vault Deployment (2)
    VaultDeployment {
        vault_rules_cid: String,
        vault_name: String,
        share_symbol: String,
        asset_instrument_id: InstrumentId,
        limits: VaultLimits,
        vault_backend_signatory: CantonId,
        #[serde(default)]
        vault_far_config: Option<FarConfig>,
        allocation_factory_cid: String,
        registrar_service_cid: String,
    },
    YieldEpochDeployment {
        vault_rules_cid: String,
        vault_cid: String,
        asset_instrument_id: InstrumentId,
        vault_backend_signatory: CantonId,
    },

    // Vault Operations (5)
    VaultPause {
        vault_id: String,
    },
    VaultUnpause {
        vault_id: String,
    },
    VaultUpdateLimits {
        vault_id: String,
        new_limits: VaultLimits,
    },
    VaultUpdateBackend {
        vault_id: String,
        new_backend_signatory: CantonId,
    },
    VaultUpdateFarBeneficiaries {
        vault_id: String,
        new_beneficiaries: Vec<AppRewardBeneficiary>,
    },

    // Processor (1)
    ProcessorDeploymentRequest {
        vault_processor_rules_cid: String,
        vault_backend_signatory: CantonId,
        allocation_factory_cid: String,
        #[serde(default)]
        processor_far_config: Option<FarConfig>,
        initial_supported_vaults: Vec<String>,
    },

    // Utility Onboarding (4)
    UtilityCreateProviderRequest {
        operator: CantonId,
    },
    UtilityCreateUserRequest {
        operator: CantonId,
    },
    UtilitySetup {
        operator: CantonId,
        provider_service_cid: String,
        user_service_cid: String,
    },
    UtilityAcceptHolderServiceRequest {
        operator: CantonId,
        provider_service_cid: String,
        holder_service_request_cid: String,
        holder: CantonId,
    },
    // Credential Actions (2)
    CredentialOfferFree {
        operator: CantonId,
        user_service_cid: String,
        holder: CantonId,
        id: String,
        description: String,
        claims: Vec<Claim>,
    },
    CredentialAcceptFree {
        operator: CantonId,
        user_service_cid: String,
        credential_offer_cid: String,
    },

    // DevNet (1)
    DevNetFeatureApp {
        amulet_rules_cid: String,
    },
}

impl ActionType {
    /// Serialize to a Daml Value (`ActionRequiringConfirmation` variant).
    ///
    /// The Daml `ActionRequiringConfirmation` type uses nested variants:
    /// - GovernanceAction(Governance_AddMemberAndSetThreshold {...})
    /// - UtilityOnboardingAction(UtilityOnboarding_CreateProviderServiceRequest {...})
    /// - VaultDeploymentAction({...}) - direct record, not nested
    pub fn to_vault_proto(&self) -> Result<Value, Error> {
        match self {
            // Governance Actions - wrapped in GovernanceAction variant
            ActionType::GovernanceAddMember {
                member,
                new_threshold,
            } => Ok(make_variant(
                "GovernanceAction",
                make_variant(
                    "Governance_AddMemberAndSetThreshold",
                    make_record(vec![
                        field("member", make_party(member)),
                        field("newThreshold", make_int64(*new_threshold)),
                    ]),
                ),
            )),

            ActionType::GovernanceRemoveMember {
                member,
                new_threshold,
            } => Ok(make_variant(
                "GovernanceAction",
                make_variant(
                    "Governance_RemoveMemberAndSetThreshold",
                    make_record(vec![
                        field("member", make_party(member)),
                        field("newThreshold", make_int64(*new_threshold)),
                    ]),
                ),
            )),

            ActionType::GovernanceSetThreshold { new_threshold } => Ok(make_variant(
                "GovernanceAction",
                make_variant(
                    "Governance_SetThreshold",
                    make_record(vec![field("newThreshold", make_int64(*new_threshold))]),
                ),
            )),

            ActionType::GovernanceSetTimeout {
                new_timeout_microseconds,
            } => Ok(make_variant(
                "GovernanceAction",
                make_variant(
                    "Governance_SetActionConfirmationTimeout",
                    make_record(vec![field(
                        "newActionConfirmationTimeout",
                        serialize_reltime(*new_timeout_microseconds),
                    )]),
                ),
            )),

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
            } => Ok(make_variant(
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
            )),

            ActionType::YieldEpochDeployment {
                vault_rules_cid,
                vault_cid,
                asset_instrument_id,
                vault_backend_signatory,
            } => Ok(make_variant(
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
            )),

            // Vault Operations - direct variants with Daml field names
            ActionType::VaultPause { vault_id } => Ok(make_variant(
                "VaultPauseAction",
                make_record(vec![field("pauseVaultId", make_contract_id(vault_id))]),
            )),

            ActionType::VaultUnpause { vault_id } => Ok(make_variant(
                "VaultUnpauseAction",
                make_record(vec![field("unpauseVaultId", make_contract_id(vault_id))]),
            )),

            ActionType::VaultUpdateLimits {
                vault_id,
                new_limits,
            } => Ok(make_variant(
                "VaultUpdateLimitsAction",
                make_record(vec![
                    field("limitsVaultId", make_contract_id(vault_id)),
                    field("newLimits", serialize_vault_limits(new_limits)),
                ]),
            )),

            ActionType::VaultUpdateBackend {
                vault_id,
                new_backend_signatory,
            } => Ok(make_variant(
                "VaultUpdateBackendAction",
                make_record(vec![
                    field("backendVaultId", make_contract_id(vault_id)),
                    field("newBackendSignatory", make_party(new_backend_signatory)),
                ]),
            )),

            ActionType::VaultUpdateFarBeneficiaries {
                vault_id,
                new_beneficiaries,
            } => Ok(make_variant(
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
            )),

            // Processor - VaultProcessorDeploymentRequestAction wrapping params
            ActionType::ProcessorDeploymentRequest {
                vault_processor_rules_cid,
                vault_backend_signatory,
                allocation_factory_cid,
                processor_far_config,
                initial_supported_vaults,
            } => Ok(make_variant(
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
            )),

            // Utility Onboarding - wrapped in UtilityOnboardingAction variant
            ActionType::UtilityCreateProviderRequest { operator } => Ok(make_variant(
                "UtilityOnboardingAction",
                make_variant(
                    "UtilityOnboarding_CreateProviderServiceRequest",
                    make_record(vec![field("operator", make_party(operator))]),
                ),
            )),

            ActionType::UtilityCreateUserRequest { operator } => Ok(make_variant(
                "UtilityOnboardingAction",
                make_variant(
                    "UtilityOnboarding_CreateUserServiceRequest",
                    make_record(vec![field("operator", make_party(operator))]),
                ),
            )),

            ActionType::UtilitySetup {
                operator,
                provider_service_cid,
                user_service_cid,
            } => Ok(make_variant(
                "UtilityOnboardingAction",
                make_variant(
                    "UtilityOnboarding_SetupUtility",
                    make_record(vec![
                        field("operator", make_party(operator)),
                        field("providerServiceCid", make_contract_id(provider_service_cid)),
                        field("userServiceCid", make_contract_id(user_service_cid)),
                    ]),
                ),
            )),

            ActionType::UtilityAcceptHolderServiceRequest {
                operator,
                provider_service_cid,
                holder_service_request_cid,
                holder,
            } => Ok(make_variant(
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
            )),

            // Credential Actions
            ActionType::CredentialOfferFree {
                operator,
                user_service_cid,
                holder,
                id,
                description,
                claims,
            } => Ok(make_variant(
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
            )),

            ActionType::CredentialAcceptFree {
                operator,
                user_service_cid,
                credential_offer_cid,
            } => Ok(make_variant(
                "CredentialAction",
                make_variant(
                    "Credential_AcceptFreeCredential",
                    make_record(vec![
                        field("operator", make_party(operator)),
                        field("userServiceCid", make_contract_id(user_service_cid)),
                        field("credentialOfferCid", make_contract_id(credential_offer_cid)),
                    ]),
                ),
            )),

            // DevNet
            ActionType::DevNetFeatureApp { amulet_rules_cid } => Ok(make_variant(
                "DevNetFeatureAppAction",
                make_record(vec![field(
                    "amuletRulesCid",
                    make_contract_id(amulet_rules_cid),
                )]),
            )),

            ActionType::GovernanceAddAdditionalProposer { .. }
            | ActionType::GovernanceRemoveAdditionalProposer { .. } => Err(Error::Encode(format!(
                "ActionType {self:?} is a governance self-action, not an ActionRequiringConfirmation"
            ))),
        }
    }

    /// Serialize to a `GovernanceSelfAction` Daml variant.
    ///
    /// Maps the same `ActionType` variants used for vault governance to the
    /// governance-core `GovernanceSelfAction` enum (different field names).
    pub fn to_self_proto(&self) -> Result<Value, Error> {
        match self {
            ActionType::GovernanceAddMember {
                member,
                new_threshold,
            } => Ok(make_variant(
                "SelfAction_AddMemberAndSetThreshold",
                make_record(vec![
                    field("newMember", make_party(member)),
                    field("newThresholdAfterAdd", make_int64(*new_threshold)),
                ]),
            )),
            ActionType::GovernanceRemoveMember {
                member,
                new_threshold,
            } => Ok(make_variant(
                "SelfAction_RemoveMemberAndSetThreshold",
                make_record(vec![
                    field("removedMember", make_party(member)),
                    field("newThresholdAfterRemove", make_int64(*new_threshold)),
                ]),
            )),
            ActionType::GovernanceSetThreshold { new_threshold } => Ok(make_variant(
                "SelfAction_SetThreshold",
                make_record(vec![field("updatedThreshold", make_int64(*new_threshold))]),
            )),
            ActionType::GovernanceSetTimeout {
                new_timeout_microseconds,
            } => Ok(make_variant(
                "SelfAction_SetTimeout",
                make_record(vec![field(
                    "updatedTimeout",
                    serialize_reltime(*new_timeout_microseconds),
                )]),
            )),
            ActionType::GovernanceAddAdditionalProposer {
                additional_proposer,
            } => Ok(make_variant(
                "SelfAction_AddAdditionalProposer",
                make_record(vec![field(
                    "additionalProposer",
                    make_party(additional_proposer),
                )]),
            )),
            ActionType::GovernanceRemoveAdditionalProposer {
                additional_proposer,
            } => Ok(make_variant(
                "SelfAction_RemoveAdditionalProposer",
                make_record(vec![field(
                    "additionalProposer",
                    make_party(additional_proposer),
                )]),
            )),
            _ => Err(Error::Encode(format!(
                "ActionType {self:?} is not a governance self-management action"
            ))),
        }
    }

    /// Deserialize a Daml Value (`ActionRequiringConfirmation` variant) to an `ActionType`.
    ///
    /// Handles nested variant structure:
    /// - GovernanceAction(Governance_AddMemberAndSetThreshold {...})
    /// - UtilityOnboardingAction(UtilityOnboarding_CreateProviderServiceRequest {...})
    /// - VaultDeploymentAction({...}) - direct record
    pub fn from_vault_proto(value: &Value) -> Result<Self, Error> {
        let variant = match &value.sum {
            Some(value::Sum::Variant(v)) => v,
            _ => {
                return Err(Error::Decode(
                    "Expected Variant value for action".to_string(),
                ));
            }
        };

        let inner = variant
            .value
            .as_ref()
            .ok_or_else(|| Error::Decode("Variant has no inner value".to_string()))?;

        match variant.constructor.as_str() {
            // Governance Actions - nested variant structure
            "GovernanceAction" => {
                let inner_variant = match &inner.sum {
                    Some(value::Sum::Variant(v)) => v,
                    _ => {
                        return Err(Error::Decode(
                            "Expected nested Variant for GovernanceAction".to_string(),
                        ));
                    }
                };
                let inner_value = inner_variant.value.as_ref().ok_or_else(|| {
                    Error::Decode("GovernanceAction inner variant has no value".to_string())
                })?;
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
                    other => Err(Error::Decode(format!(
                        "Unknown GovernanceAction constructor: {other}"
                    ))),
                }
            }

            // Utility Onboarding Actions - nested variant structure
            "UtilityOnboardingAction" => {
                let inner_variant = match &inner.sum {
                    Some(value::Sum::Variant(v)) => v,
                    _ => {
                        return Err(Error::Decode(
                            "Expected nested Variant for UtilityOnboardingAction".to_string(),
                        ));
                    }
                };
                let inner_value = inner_variant.value.as_ref().ok_or_else(|| {
                    Error::Decode("UtilityOnboardingAction inner variant has no value".to_string())
                })?;
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
                        user_service_cid: extract_contract_id(get_field(
                            record,
                            "userServiceCid",
                        )?)?,
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
                    other => Err(Error::Decode(format!(
                        "Unknown UtilityOnboardingAction constructor: {other}"
                    ))),
                }
            }

            // Credential Actions - nested variant structure
            "CredentialAction" => {
                let inner_variant = match &inner.sum {
                    Some(value::Sum::Variant(v)) => v,
                    _ => {
                        return Err(Error::Decode(
                            "Expected nested Variant for CredentialAction".to_string(),
                        ));
                    }
                };
                let inner_value = inner_variant.value.as_ref().ok_or_else(|| {
                    Error::Decode("CredentialAction inner variant has no value".to_string())
                })?;
                let record = extract_record(inner_value)?;

                match inner_variant.constructor.as_str() {
                    "Credential_OfferFreeCredential" => {
                        let claims_list = extract_list(get_field(record, "claims")?)?;
                        let claims = claims_list
                            .elements
                            .iter()
                            .map(deserialize_claim)
                            .collect::<Result<Vec<_>, Error>>()?;

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
                        user_service_cid: extract_contract_id(get_field(
                            record,
                            "userServiceCid",
                        )?)?,
                        credential_offer_cid: extract_contract_id(get_field(
                            record,
                            "credentialOfferCid",
                        )?)?,
                    }),
                    other => Err(Error::Decode(format!(
                        "Unknown CredentialAction constructor: {other}"
                    ))),
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

            // Vault Operations - direct record with Daml field names
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
                    new_backend_signatory: extract_party_id(get_field(
                        record,
                        "newBackendSignatory",
                    )?)?,
                })
            }

            "VaultUpdateFARBeneficiariesAction" => {
                let record = extract_record(inner)?;
                let beneficiaries_list = extract_list(get_field(record, "newBeneficiaries")?)?;
                let new_beneficiaries = beneficiaries_list
                    .elements
                    .iter()
                    .map(deserialize_app_reward_beneficiary)
                    .collect::<Result<Vec<_>, Error>>()?;

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
                    .collect::<Result<Vec<_>, Error>>()?;

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

            other => Err(Error::Decode(format!(
                "Unknown action constructor: {other}"
            ))),
        }
    }

    /// Deserialize a `GovernanceSelfAction` Daml variant to an `ActionType`.
    pub fn from_self_proto(value: &Value) -> Result<Self, Error> {
        let variant = match &value.sum {
            Some(value::Sum::Variant(v)) => v,
            _ => {
                return Err(Error::Decode(
                    "Expected Variant value for GovernanceSelfAction".to_string(),
                ));
            }
        };

        let inner = variant.value.as_ref().ok_or_else(|| {
            Error::Decode("GovernanceSelfAction variant has no inner value".to_string())
        })?;

        let record = extract_record(inner)
            .map_err(|_| Error::Decode("Expected GovernanceSelfAction record".to_string()))?;
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
                let additional_proposer =
                    extract_party_id(get_field(record, "additionalProposer")?)?;
                Ok(ActionType::GovernanceAddAdditionalProposer {
                    additional_proposer,
                })
            }
            "SelfAction_RemoveAdditionalProposer" => {
                let additional_proposer =
                    extract_party_id(get_field(record, "additionalProposer")?)?;
                Ok(ActionType::GovernanceRemoveAdditionalProposer {
                    additional_proposer,
                })
            }
            other => Err(Error::Decode(format!(
                "Unknown GovernanceSelfAction constructor: {other}"
            ))),
        }
    }

    /// Validate the action's fields. Returns an error message if invalid.
    ///
    /// Catches obviously-malformed inputs (negative thresholds, non-positive
    /// timeouts) before they reach Canton's Daml checks. Canton rejects bad
    /// values too, but here we surface a clear 400 rather than a generic
    /// submission error after the proposal contract is already on the wire.
    pub fn validate(&self) -> Result<(), Error> {
        match self {
            ActionType::GovernanceAddMember { new_threshold, .. }
            | ActionType::GovernanceRemoveMember { new_threshold, .. }
            | ActionType::GovernanceSetThreshold { new_threshold } => {
                validate_threshold(*new_threshold)
            }
            ActionType::GovernanceSetTimeout {
                new_timeout_microseconds,
            } => validate_timeout(*new_timeout_microseconds),
            ActionType::VaultDeployment {
                vault_far_config: Some(far),
                ..
            }
            | ActionType::ProcessorDeploymentRequest {
                processor_far_config: Some(far),
                ..
            } => validate_beneficiary_weights(&far.beneficiaries),
            ActionType::VaultUpdateFarBeneficiaries {
                new_beneficiaries, ..
            } => validate_beneficiary_weights(new_beneficiaries),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use canton_common::decimal::DamlDecimal;

    use super::*;

    /// Test-only helper: builds a `CantonId` with a fixed valid namespace so
    /// tests can vary just the prefix.
    fn cid(prefix: &str) -> CantonId {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        CantonId::parse(&format!("{prefix}::{ns}")).unwrap()
    }

    /// Parse a decimal literal in test fixtures, panicking on invalid input.
    fn dec(s: &str) -> DamlDecimal {
        DamlDecimal::parse(s).expect("valid decimal literal")
    }

    fn instrument() -> InstrumentId {
        InstrumentId {
            admin: "admin-party".into(),
            id: "TOK".into(),
        }
    }

    fn claim() -> Claim {
        Claim {
            subject: "subj".into(),
            property: "prop".into(),
            value: "val".into(),
        }
    }

    fn far() -> FarConfig {
        FarConfig {
            featured_app_right_cid: "00far".into(),
            beneficiaries: vec![AppRewardBeneficiary {
                beneficiary: cid("b1"),
                weight: dec("1.0"),
            }],
        }
    }

    fn limits_full() -> VaultLimits {
        VaultLimits {
            max_total_deposit: Some(dec("100")),
            min_deposit_amount: Some(dec("0.1")),
            min_withdrawal_amount: Some(dec("0.2")),
        }
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

    // ---- `to_vault_proto` / `to_self_proto` wire-shape assertions ----
    //
    // These lock the Daml constructor names and field labels emitted for the
    // governance actions. The labels are hand-written and consumed by the
    // on-ledger interpreter, so a typo or a swap between the two encoders
    // (`to_vault_proto` vs `to_self_proto`, which deliberately use
    // *different* labels for the same action) would only surface as a
    // runtime interpretation error far from the source. A round-trip test
    // cannot catch a wrong-but-symmetric label; explicit label assertions
    // can.

    #[test]
    fn serialize_action_add_member_shape() {
        let action = ActionType::GovernanceAddMember {
            member: cid("p"),
            new_threshold: 3,
        };
        let value = action.to_vault_proto().unwrap();
        let (outer, inner) = as_variant(&value);
        assert_eq!(outer, "GovernanceAction");
        let (ctor, record) = as_variant(inner);
        assert_eq!(ctor, "Governance_AddMemberAndSetThreshold");
        assert_eq!(record_labels(record), ["member", "newThreshold"]);
    }

    #[test]
    fn serialize_action_set_threshold_and_timeout_shape() {
        let threshold = ActionType::GovernanceSetThreshold { new_threshold: 2 }
            .to_vault_proto()
            .unwrap();
        let (outer, inner) = as_variant(&threshold);
        assert_eq!(outer, "GovernanceAction");
        let (ctor, record) = as_variant(inner);
        assert_eq!(ctor, "Governance_SetThreshold");
        assert_eq!(record_labels(record), ["newThreshold"]);

        let timeout = ActionType::GovernanceSetTimeout {
            new_timeout_microseconds: 1_000,
        }
        .to_vault_proto()
        .unwrap();
        let (_, inner) = as_variant(&timeout);
        let (ctor, record) = as_variant(inner);
        assert_eq!(ctor, "Governance_SetActionConfirmationTimeout");
        assert_eq!(record_labels(record), ["newActionConfirmationTimeout"]);
    }

    #[test]
    fn serialize_action_utility_and_credential_and_devnet_shapes() {
        let setup = ActionType::UtilitySetup {
            operator: cid("p"),
            provider_service_cid: "psc".to_string(),
            user_service_cid: "usc".to_string(),
        }
        .to_vault_proto()
        .unwrap();
        let (outer, inner) = as_variant(&setup);
        assert_eq!(outer, "UtilityOnboardingAction");
        let (ctor, record) = as_variant(inner);
        assert_eq!(ctor, "UtilityOnboarding_SetupUtility");
        assert_eq!(
            record_labels(record),
            ["operator", "providerServiceCid", "userServiceCid"]
        );

        let accept = ActionType::CredentialAcceptFree {
            operator: cid("p"),
            user_service_cid: "usc".to_string(),
            credential_offer_cid: "coc".to_string(),
        }
        .to_vault_proto()
        .unwrap();
        let (outer, inner) = as_variant(&accept);
        assert_eq!(outer, "CredentialAction");
        let (ctor, record) = as_variant(inner);
        assert_eq!(ctor, "Credential_AcceptFreeCredential");
        assert_eq!(
            record_labels(record),
            ["operator", "userServiceCid", "credentialOfferCid"]
        );

        // DevNet wraps a bare record (no nested action variant).
        let devnet = ActionType::DevNetFeatureApp {
            amulet_rules_cid: "arc".to_string(),
        }
        .to_vault_proto()
        .unwrap();
        let (ctor, record) = as_variant(&devnet);
        assert_eq!(ctor, "DevNetFeatureAppAction");
        assert_eq!(record_labels(record), ["amuletRulesCid"]);
    }

    #[test]
    fn serialize_self_action_uses_distinct_labels_from_serialize_action() {
        // The self-management encoder maps the SAME ActionType to DIFFERENT
        // constructor + field names than `to_vault_proto`. Pin both so the
        // two paths can't silently converge or drift.
        let add = ActionType::GovernanceAddMember {
            member: cid("p"),
            new_threshold: 3,
        }
        .to_self_proto()
        .unwrap();
        let (ctor, record) = as_variant(&add);
        assert_eq!(ctor, "SelfAction_AddMemberAndSetThreshold");
        assert_eq!(record_labels(record), ["newMember", "newThresholdAfterAdd"]);

        let remove = ActionType::GovernanceRemoveMember {
            member: cid("p"),
            new_threshold: 1,
        }
        .to_self_proto()
        .unwrap();
        let (ctor, record) = as_variant(&remove);
        assert_eq!(ctor, "SelfAction_RemoveMemberAndSetThreshold");
        assert_eq!(
            record_labels(record),
            ["removedMember", "newThresholdAfterRemove"]
        );

        let set_threshold = ActionType::GovernanceSetThreshold { new_threshold: 2 }
            .to_self_proto()
            .unwrap();
        let (ctor, record) = as_variant(&set_threshold);
        assert_eq!(ctor, "SelfAction_SetThreshold");
        assert_eq!(record_labels(record), ["updatedThreshold"]);
    }

    // ---- `validate` assertions ----

    #[test]
    fn action_threshold_rejects_zero_and_negative() {
        let action = ActionType::GovernanceSetThreshold { new_threshold: 0 };
        assert!(action.validate().is_err());
        let action = ActionType::GovernanceSetThreshold { new_threshold: -3 };
        assert!(action.validate().is_err());
        let action = ActionType::GovernanceSetThreshold { new_threshold: 1 };
        assert!(action.validate().is_ok());
    }

    #[test]
    fn action_threshold_rejects_in_add_remove_member() {
        let member = cid("member");
        let action = ActionType::GovernanceAddMember {
            member: member.clone(),
            new_threshold: 0,
        };
        assert!(action.validate().is_err());
        let action = ActionType::GovernanceRemoveMember {
            member,
            new_threshold: -1,
        };
        assert!(action.validate().is_err());
    }

    #[test]
    fn action_timeout_rejects_zero_and_negative() {
        let action = ActionType::GovernanceSetTimeout {
            new_timeout_microseconds: 0,
        };
        assert!(action.validate().is_err());
        let action = ActionType::GovernanceSetTimeout {
            new_timeout_microseconds: -1_000_000,
        };
        assert!(action.validate().is_err());
        let action = ActionType::GovernanceSetTimeout {
            new_timeout_microseconds: 60_000_000,
        };
        assert!(action.validate().is_ok());
    }

    // ---- codec round trips ----

    /// The Task 1 action fixtures minus the two AdditionalProposer variants
    /// (those only exist in the self-management form).
    fn vault_encodable_fixtures() -> Vec<ActionType> {
        vec![
            ActionType::GovernanceAddMember {
                member: cid("m1"),
                new_threshold: 2,
            },
            ActionType::GovernanceRemoveMember {
                member: cid("m1"),
                new_threshold: 1,
            },
            ActionType::GovernanceSetThreshold { new_threshold: 3 },
            ActionType::GovernanceSetTimeout {
                new_timeout_microseconds: 60_000_000,
            },
            ActionType::VaultDeployment {
                vault_rules_cid: "00vaultrules".into(),
                vault_name: "Vault One".into(),
                share_symbol: "V1".into(),
                asset_instrument_id: instrument(),
                limits: limits_full(),
                vault_backend_signatory: cid("backend"),
                vault_far_config: Some(far()),
                allocation_factory_cid: "00alloc".into(),
                registrar_service_cid: "00reg".into(),
            },
            ActionType::YieldEpochDeployment {
                vault_rules_cid: "00vaultrules".into(),
                vault_cid: "00vault".into(),
                asset_instrument_id: instrument(),
                vault_backend_signatory: cid("backend"),
            },
            ActionType::VaultPause {
                vault_id: "00vault".into(),
            },
            ActionType::VaultUnpause {
                vault_id: "00vault".into(),
            },
            ActionType::VaultUpdateLimits {
                vault_id: "00vault".into(),
                new_limits: limits_full(),
            },
            ActionType::VaultUpdateBackend {
                vault_id: "00vault".into(),
                new_backend_signatory: cid("backend2"),
            },
            ActionType::VaultUpdateFarBeneficiaries {
                vault_id: "00vault".into(),
                new_beneficiaries: vec![AppRewardBeneficiary {
                    beneficiary: cid("b1"),
                    weight: dec("1.0"),
                }],
            },
            ActionType::ProcessorDeploymentRequest {
                vault_processor_rules_cid: "00proc".into(),
                vault_backend_signatory: cid("backend"),
                allocation_factory_cid: "00alloc".into(),
                processor_far_config: Some(far()),
                initial_supported_vaults: vec!["00vault".into()],
            },
            ActionType::UtilityCreateProviderRequest {
                operator: cid("op"),
            },
            ActionType::UtilityCreateUserRequest {
                operator: cid("op"),
            },
            ActionType::UtilitySetup {
                operator: cid("op"),
                provider_service_cid: "00psc".into(),
                user_service_cid: "00usc".into(),
            },
            ActionType::UtilityAcceptHolderServiceRequest {
                operator: cid("op"),
                provider_service_cid: "00psc".into(),
                holder_service_request_cid: "00hsr".into(),
                holder: cid("holder"),
            },
            ActionType::CredentialOfferFree {
                operator: cid("op"),
                user_service_cid: "00usc".into(),
                holder: cid("holder"),
                id: "cred-1".into(),
                description: "a credential".into(),
                claims: vec![claim()],
            },
            ActionType::CredentialAcceptFree {
                operator: cid("op"),
                user_service_cid: "00usc".into(),
                credential_offer_cid: "00offer".into(),
            },
            ActionType::DevNetFeatureApp {
                amulet_rules_cid: "00amulet".into(),
            },
        ]
    }

    /// The six member-management variants — the only ones that also exist in
    /// the `GovernanceSelfAction` (self-management) form.
    fn self_encodable_fixtures() -> Vec<ActionType> {
        vec![
            ActionType::GovernanceAddMember {
                member: cid("m1"),
                new_threshold: 2,
            },
            ActionType::GovernanceRemoveMember {
                member: cid("m1"),
                new_threshold: 1,
            },
            ActionType::GovernanceSetThreshold { new_threshold: 3 },
            ActionType::GovernanceSetTimeout {
                new_timeout_microseconds: 60_000_000,
            },
            ActionType::GovernanceAddAdditionalProposer {
                additional_proposer: cid("p1"),
            },
            ActionType::GovernanceRemoveAdditionalProposer {
                additional_proposer: cid("p1"),
            },
        ]
    }

    #[test]
    fn vault_form_round_trips() {
        for action in vault_encodable_fixtures() {
            let value = action.to_vault_proto().unwrap();
            assert_eq!(ActionType::from_vault_proto(&value).unwrap(), action);
        }
    }

    #[test]
    fn self_form_round_trips() {
        for action in self_encodable_fixtures() {
            let value = action.to_self_proto().unwrap();
            assert_eq!(ActionType::from_self_proto(&value).unwrap(), action);
        }
    }

    #[test]
    fn wrong_form_is_an_error_not_a_panic() {
        let self_only = ActionType::GovernanceAddAdditionalProposer {
            additional_proposer: cid("p"),
        };
        assert!(matches!(self_only.to_vault_proto(), Err(Error::Encode(_))));
        let vault_only = ActionType::VaultPause {
            vault_id: "00v".into(),
        };
        assert!(matches!(vault_only.to_self_proto(), Err(Error::Encode(_))));
    }

    #[test]
    fn unknown_constructor_is_a_decode_error() {
        let bogus = make_variant("NoSuchAction", make_record(vec![]));
        assert!(matches!(
            ActionType::from_vault_proto(&bogus),
            Err(Error::Decode(_))
        ));
        assert!(matches!(
            ActionType::from_self_proto(&bogus),
            Err(Error::Decode(_))
        ));
    }
}
