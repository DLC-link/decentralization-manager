//! Daml `Value` argument builders for governance choices, plus
//! serialization of `ProposalType` domain-governance proposals.
//!
//! `ActionType`'s own codec (`to_vault_proto` / `from_vault_proto` /
//! `to_self_proto` / `from_self_proto`) now lives in
//! `decman_lib::catalog::action`; the four `build_*_action*` functions below
//! are thin fallible wrappers around it.

use canton_common::transfer_factory::Context as ChoiceContext;
#[cfg(test)]
use canton_common::{decimal::DamlDecimal, transfer_factory::ContextValue};
use canton_proto_rs::com::daml::ledger::api::v2::{Optional, Record, Value, value};
pub(crate) use decman_lib::catalog::types::{
    make_optional_beneficiaries, serialize_billing_params, serialize_reward_beneficiary,
};
pub(crate) use decman_lib::framework::encode::*;

#[cfg(test)]
use crate::canton_id::CantonId;
use crate::error::Result;

use super::types::{ActionType, InstrumentAllowance, ProposalType};
#[cfg(test)]
use super::types::{
    BillingParams, Claim, InstrumentId, InstrumentIdentifier, PartyCredentialRequirement,
    RewardBeneficiary,
};

// ============================================================================
// Action Serialization
// ============================================================================
//
// `ActionType::to_vault_proto` / `to_self_proto` (decman_lib::catalog::action)
// do the actual encoding; the builders below just wrap them into the choice
// argument shapes each Daml choice expects. Interim until Task 21/22 move the
// argument-envelope construction itself into the lib.

/// Build the ConfirmAction choice argument
///
/// The Daml structure is: { confirmer: Party, action: ActionRequiringConfirmation }
pub fn build_confirm_action_argument(confirmer: &str, action: &ActionType) -> Result<Value> {
    Ok(make_record(vec![
        field("confirmer", make_party(confirmer)),
        field("action", action.to_vault_proto()?),
    ]))
}

/// Build the ExecuteConfirmedAction choice argument
///
/// The Daml structure is:
/// { executor: Party, action: ActionRequiringConfirmation, confirmations: [ContractId], contractCid: Optional ContractId }
pub fn build_execute_action_argument(
    executor: &str,
    action: &ActionType,
    confirmation_cids: &[String],
    contract_cid: Option<&str>,
) -> Result<Value> {
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

    Ok(make_record(vec![
        field("executor", make_party(executor)),
        field("action", action.to_vault_proto()?),
        field("confirmations", confirmations),
        field("contractCid", contract_cid_value),
    ]))
}

// ============================================================================
// Governance-Core Self-Management Serialization
// ============================================================================
//
// `ActionType::to_self_proto` (decman_lib::catalog::action) does the actual
// encoding; the builders below just wrap it into the choice argument shapes
// each Daml choice expects.

/// Build the GovernanceRules_ConfirmGovernanceAction choice argument
///
/// Daml structure: { confirmer: Party, action: GovernanceSelfAction }
pub fn build_confirm_governance_action_arg(confirmer: &str, action: &ActionType) -> Result<Value> {
    Ok(make_record(vec![
        field("confirmer", make_party(confirmer)),
        field("action", action.to_self_proto()?),
    ]))
}

/// Build the GovernanceRules_ExecuteGovernanceAction choice argument
///
/// Daml structure: { executor: Party, action: GovernanceSelfAction, confirmations: [ContractId GovernanceSelfConfirmation] }
pub fn build_execute_governance_action_arg(
    executor: &str,
    action: &ActionType,
    confirmation_cids: &[String],
) -> Result<Value> {
    let confirmations = make_list(
        confirmation_cids
            .iter()
            .map(|cid| make_contract_id(cid))
            .collect(),
    );

    Ok(make_record(vec![
        field("executor", make_party(executor)),
        field("action", action.to_self_proto()?),
        field("confirmations", confirmations),
    ]))
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
    GovernanceRewards,
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
            // The validity window is applied via `transfer_validity` (the
            // timestamps below), not serialized as its own field.
            validity_window_hours: _,
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
        ProposalType::SetupCouponReassignmentDelegation {
            dso,
            assigners,
            new_beneficiaries,
            prior_delegation,
        } => (
            ProposalPackage::GovernanceRewards,
            "Governance.Rewards.SetupCouponReassignmentDelegation",
            "SetupCouponReassignmentDelegation",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field(
                        "priorDelegation",
                        make_optional_contract_id(prior_delegation),
                    ),
                    field("dso", make_party(dso)),
                    field(
                        "assigners",
                        make_list(assigners.iter().map(make_party).collect()),
                    ),
                    field(
                        "beneficiaries",
                        make_list(
                            new_beneficiaries
                                .iter()
                                .map(serialize_reward_beneficiary)
                                .collect(),
                        ),
                    ),
                ],
            },
        ),
        ProposalType::RevokeCouponReassignmentDelegation { delegation } => (
            ProposalPackage::GovernanceRewards,
            "Governance.Rewards.RevokeCouponReassignmentDelegation",
            "RevokeCouponReassignmentDelegation",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field("delegation", make_contract_id(delegation)),
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
        ProposalType::SetupMintingDelegation {
            delegate,
            dso,
            expires_at_micros,
            amulet_merge_limit,
            description,
        } => (
            ProposalPackage::GovernanceRewards,
            "Governance.Rewards.SetupMintingDelegation",
            "SetupMintingDelegation",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field("delegate", make_party(delegate)),
                    field("dso", make_party(dso)),
                    field(
                        "expiresAt",
                        Value {
                            sum: Some(value::Sum::Timestamp(*expires_at_micros)),
                        },
                    ),
                    field("amuletMergeLimit", make_int64(*amulet_merge_limit)),
                    field("description", make_text(description)),
                ],
            },
        ),
        ProposalType::AcceptExternalPartySetup { proposal_cid } => (
            ProposalPackage::GovernanceRewards,
            "Governance.Rewards.AcceptExternalPartySetup",
            "AcceptExternalPartySetup",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field("proposalCid", make_contract_id(proposal_cid)),
                    field(
                        "description",
                        make_text(&format!(
                            "Accept external party setup (ValidatorRight + TransferPreapproval) for proposal {proposal_cid}"
                        )),
                    ),
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
            issuer_credential_cids,
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
                    field(
                        "issuerCredentialCids",
                        make_optional_list(
                            issuer_credential_cids
                                .iter()
                                .map(|cid| make_contract_id(cid))
                                .collect(),
                        ),
                    ),
                ],
            },
        ),
        ProposalType::AcceptBurnRequest {
            burn_request_cid,
            instrument_configuration_cid,
            issuer_credential_cids,
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
                    field(
                        "issuerCredentialCids",
                        make_optional_list(
                            issuer_credential_cids
                                .iter()
                                .map(|cid| make_contract_id(cid))
                                .collect(),
                        ),
                    ),
                ],
            },
        ),
        ProposalType::CreateProviderConfiguration {
            provider_service_cid,
            registrar_requirements,
            holder_requirements,
        } => (
            ProposalPackage::GovernanceUtilityOnboarding,
            "Governance.UtilityOnboarding.CreateProviderConfiguration",
            "CreateProviderConfiguration",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field("providerServiceCid", make_contract_id(provider_service_cid)),
                    field(
                        "registrarRequirements",
                        serialize_party_credential_requirements(registrar_requirements),
                    ),
                    field(
                        "holderRequirements",
                        serialize_party_credential_requirements(holder_requirements),
                    ),
                ],
            },
        ),
        ProposalType::CreateRegistrarServiceRequest {
            operator,
            provider,
            create_transfer_rule,
            create_allocation_factory,
        } => (
            ProposalPackage::GovernanceUtilityOnboarding,
            "Governance.UtilityOnboarding.CreateRegistrarServiceRequest",
            "CreateRegistrarServiceRequest",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field("operator", make_party(operator)),
                    field("provider", make_party(provider)),
                    field("createTransferRule", make_bool(*create_transfer_rule)),
                    field(
                        "createAllocationFactory",
                        make_bool(*create_allocation_factory),
                    ),
                ],
            },
        ),
        ProposalType::OnboardRegistrar {
            provider_service_cid,
            registrar_service_request_cid,
            provider_configuration_cid,
        } => (
            ProposalPackage::GovernanceUtilityOnboarding,
            "Governance.UtilityOnboarding.OnboardRegistrar",
            "OnboardRegistrar",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field("providerServiceCid", make_contract_id(provider_service_cid)),
                    field(
                        "registrarServiceRequestCid",
                        make_contract_id(registrar_service_request_cid),
                    ),
                    field(
                        "providerConfigurationCid",
                        make_contract_id(provider_configuration_cid),
                    ),
                ],
            },
        ),
        ProposalType::ProvisionInstrument {
            registrar_service_cid,
            instrument_id_text,
            additional_identifiers,
            issuer_requirements,
            holder_requirements,
            initial_instrument_issuers,
        } => (
            ProposalPackage::GovernanceUtilityOnboarding,
            "Governance.UtilityOnboarding.ProvisionInstrument",
            "ProvisionInstrument",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field(
                        "registrarServiceCid",
                        make_contract_id(registrar_service_cid),
                    ),
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
                    field(
                        "issuerRequirements",
                        serialize_party_credential_requirements(issuer_requirements),
                    ),
                    field(
                        "holderRequirements",
                        serialize_party_credential_requirements(holder_requirements),
                    ),
                    field(
                        "initialInstrumentIssuers",
                        make_list(initial_instrument_issuers.iter().map(make_party).collect()),
                    ),
                ],
            },
        ),
        ProposalType::OnboardInstrumentIssuers {
            instrument_configuration_cid,
            instrument_issuers,
        } => (
            ProposalPackage::GovernanceUtilityOnboarding,
            "Governance.UtilityOnboarding.OnboardInstrumentIssuers",
            "OnboardInstrumentIssuers",
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
                        "instrumentIssuers",
                        make_list(instrument_issuers.iter().map(make_party).collect()),
                    ),
                ],
            },
        ),
        ProposalType::OffboardInstrumentIssuers { instrument_issuers } => (
            ProposalPackage::GovernanceUtilityOnboarding,
            "Governance.UtilityOnboarding.OffboardInstrumentIssuers",
            "OffboardInstrumentIssuers",
            Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", make_party(governance_party)),
                    field("proposer", make_party(proposer)),
                    field(
                        "instrumentIssuers",
                        make_list(
                            instrument_issuers
                                .iter()
                                .map(|row| {
                                    make_record(vec![
                                        field(
                                            "instrumentIssuer",
                                            make_party(&row.instrument_issuer),
                                        ),
                                        field(
                                            "credentialCids",
                                            make_list(
                                                row.credential_cids
                                                    .iter()
                                                    .map(|cid| make_contract_id(cid))
                                                    .collect(),
                                            ),
                                        ),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                ],
            },
        ),
    })
}

/// Build the GovernanceRules_ConfirmAction choice argument for domain actions
///
/// Daml structure: { confirmer: Party, actionProposalCid: ContractId GovernableAction }
pub fn build_confirm_domain_action_arg(confirmer: &str, proposal_cid: &str) -> Value {
    make_record(vec![
        field("confirmer", make_party(confirmer)),
        field("actionProposalCid", make_contract_id(proposal_cid)),
    ])
}

/// Build the GovernanceRules_ExecuteConfirmedAction choice argument for domain actions
///
/// Daml structure: { executor: Party, actionProposalCid: ContractId GovernableAction, confirmations: [ContractId GovernanceConfirmation] }
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use common::api::RequiredClaim;

    use crate::{
        canton_id::{NAMESPACE_LENGTH, Namespace},
        server::types::InstrumentIssuerCredentials,
    };

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

    // ---- ProposalType wire-shape assertions ----
    //
    // These lock the Daml constructor names and field labels emitted for
    // domain governance proposals. The labels are hand-written and consumed
    // by the on-ledger interpreter, so a typo would only surface as a
    // runtime interpretation error far from the source; hence the explicit
    // label assertions below. (`ActionType`'s own wire-shape assertions now
    // live with the codec in `decman_lib::catalog::action`.)

    /// Any valid `CantonId` — the exact value is irrelevant to these
    /// constructor/field-name assertions.
    fn party_id() -> CantonId {
        CantonId::new("p".to_string(), Namespace::new([0u8; NAMESPACE_LENGTH]))
    }

    /// Parse a decimal literal in test fixtures, panicking on invalid input.
    fn dec(s: &str) -> DamlDecimal {
        DamlDecimal::parse(s).expect("valid decimal literal")
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

    /// Unwrap a `value::Sum::List` reference (for descending into a list `Value`
    /// returned by `field_value`).
    fn as_list(value: &Value) -> &[Value] {
        match &value.sum {
            Some(value::Sum::List(l)) => &l.elements,
            other => panic!("expected List, got {other:?}"),
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
            validity_window_hours: None,
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
    fn build_proposal_setup_minting_delegation_shape() -> Result {
        let expires_at_micros = 1_800_000_000_000_000;
        let proposal = ProposalType::SetupMintingDelegation {
            delegate: party_id(),
            dso: party_id(),
            expires_at_micros,
            amulet_merge_limit: 10,
            description: "collect CIP-104 rewards".to_string(),
        };
        let (package, module, entity, record) =
            build_proposal_create_args("gov", "proposer", &proposal, None, None)?;

        assert_eq!(package, ProposalPackage::GovernanceRewards);
        assert_eq!(module, "Governance.Rewards.SetupMintingDelegation");
        assert_eq!(entity, "SetupMintingDelegation");
        assert_eq!(
            owned_labels(&record),
            [
                "governanceParty",
                "proposer",
                "delegate",
                "dso",
                "expiresAt",
                "amuletMergeLimit",
                "description",
            ]
        );
        assert!(matches!(
            field_value(&record, "expiresAt").sum,
            Some(value::Sum::Timestamp(micros)) if micros == expires_at_micros,
        ));
        assert!(matches!(
            field_value(&record, "amuletMergeLimit").sum,
            Some(value::Sum::Int64(10)),
        ));
        Ok(())
    }

    #[test]
    fn build_proposal_accept_external_party_setup_shape() -> Result {
        let proposal = ProposalType::AcceptExternalPartySetup {
            proposal_cid: "00abc123".to_string(),
        };
        let (package, module, entity, record) =
            build_proposal_create_args("gov", "proposer", &proposal, None, None)?;
        assert_eq!(package, ProposalPackage::GovernanceRewards);
        assert_eq!(module, "Governance.Rewards.AcceptExternalPartySetup");
        assert_eq!(entity, "AcceptExternalPartySetup");
        assert_eq!(
            owned_labels(&record),
            ["governanceParty", "proposer", "proposalCid", "description"]
        );
        Ok(())
    }

    #[test]
    fn build_proposal_setup_delegation_shape() -> Result {
        let proposal = ProposalType::SetupCouponReassignmentDelegation {
            dso: party_id(),
            assigners: vec![party_id(), party_id()],
            new_beneficiaries: vec![
                RewardBeneficiary {
                    beneficiary: party_id(),
                    percentage: dec("0.8"),
                },
                RewardBeneficiary {
                    beneficiary: party_id(),
                    percentage: dec("0.2"),
                },
            ],
            prior_delegation: Some("00old".to_string()),
        };
        let (package, module, entity, record) =
            build_proposal_create_args("gov", "proposer", &proposal, None, None)?;
        assert_eq!(package, ProposalPackage::GovernanceRewards);
        assert_eq!(
            module,
            "Governance.Rewards.SetupCouponReassignmentDelegation"
        );
        assert_eq!(entity, "SetupCouponReassignmentDelegation");
        assert_eq!(
            owned_labels(&record),
            [
                "governanceParty",
                "proposer",
                "priorDelegation",
                "dso",
                "assigners",
                "beneficiaries"
            ]
        );

        // priorDelegation: Some -> Optional(Some(ContractId "00old")).
        match &field_value(&record, "priorDelegation").sum {
            Some(value::Sum::Optional(opt)) => {
                let inner = opt.value.as_ref().expect("priorDelegation should be Some");
                assert!(
                    matches!(&inner.sum, Some(value::Sum::ContractId(c)) if c == "00old"),
                    "priorDelegation inner must be ContractId(\"00old\"), got {:?}",
                    inner.sum
                );
            }
            other => panic!("priorDelegation must be Optional, got {other:?}"),
        }

        // assigners: a list of two Party values.
        let assigners = as_list(field_value(&record, "assigners"));
        assert_eq!(assigners.len(), 2);
        assert!(
            assigners
                .iter()
                .all(|v| matches!(v.sum, Some(value::Sum::Party(_)))),
            "assigners elements must be Party"
        );

        // beneficiaries: a list of {beneficiary: Party, percentage: Numeric},
        // carrying the 0.8 / 0.2 split in order. A regression swapping
        // make_party <-> make_contract_id, or renaming `percentage`, fails here.
        let benes = as_list(field_value(&record, "beneficiaries"));
        assert_eq!(benes.len(), 2);
        for (bene, expected_pct) in benes.iter().zip(["0.8", "0.2"]) {
            let rec = as_record(bene);
            assert_eq!(owned_labels(rec), ["beneficiary", "percentage"]);
            assert!(
                matches!(
                    field_value(rec, "beneficiary").sum,
                    Some(value::Sum::Party(_))
                ),
                "beneficiary must be a Party"
            );
            match &field_value(rec, "percentage").sum {
                Some(value::Sum::Numeric(n)) => assert_eq!(n, expected_pct),
                other => panic!("percentage must be Numeric, got {other:?}"),
            }
        }
        Ok(())
    }

    #[test]
    fn build_proposal_revoke_delegation_shape() -> Result {
        let proposal = ProposalType::RevokeCouponReassignmentDelegation {
            delegation: "00abc".to_string(),
        };
        let (package, module, entity, record) =
            build_proposal_create_args("gov", "proposer", &proposal, None, None)?;
        assert_eq!(package, ProposalPackage::GovernanceRewards);
        assert_eq!(
            module,
            "Governance.Rewards.RevokeCouponReassignmentDelegation"
        );
        assert_eq!(entity, "RevokeCouponReassignmentDelegation");
        assert_eq!(
            owned_labels(&record),
            ["governanceParty", "proposer", "delegation"]
        );
        // delegation: a ContractId carrying the passed cid.
        assert!(
            matches!(&field_value(&record, "delegation").sum, Some(value::Sum::ContractId(c)) if c == "00abc"),
            "delegation must be ContractId(\"00abc\"), got {:?}",
            field_value(&record, "delegation").sum
        );
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
                    issuer_credential_cids: vec!["cred-1".to_string(), "cred-2".to_string()],
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
                    "issuerCredentialCids",
                ],
            },
            Case {
                proposal: ProposalType::AcceptBurnRequest {
                    burn_request_cid: "brc".to_string(),
                    instrument_configuration_cid: "icc".to_string(),
                    issuer_credential_cids: vec!["cred-1".to_string()],
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
                    "issuerCredentialCids",
                ],
            },
            Case {
                proposal: ProposalType::CreateProviderConfiguration {
                    provider_service_cid: "psc".to_string(),
                    registrar_requirements: vec![],
                    holder_requirements: vec![],
                },
                package: ProposalPackage::GovernanceUtilityOnboarding,
                module: "Governance.UtilityOnboarding.CreateProviderConfiguration",
                entity: "CreateProviderConfiguration",
                labels: &[
                    "governanceParty",
                    "proposer",
                    "providerServiceCid",
                    "registrarRequirements",
                    "holderRequirements",
                ],
            },
            Case {
                proposal: ProposalType::ProvisionInstrument {
                    registrar_service_cid: "rsc".to_string(),
                    instrument_id_text: "uuid-1".to_string(),
                    additional_identifiers: vec![],
                    issuer_requirements: vec![],
                    holder_requirements: vec![],
                    initial_instrument_issuers: vec![party_id()],
                },
                package: ProposalPackage::GovernanceUtilityOnboarding,
                module: "Governance.UtilityOnboarding.ProvisionInstrument",
                entity: "ProvisionInstrument",
                labels: &[
                    "governanceParty",
                    "proposer",
                    "registrarServiceCid",
                    "instrumentIdText",
                    "additionalIdentifiers",
                    "issuerRequirements",
                    "holderRequirements",
                    "initialInstrumentIssuers",
                ],
            },
            Case {
                proposal: ProposalType::CreateRegistrarServiceRequest {
                    operator: party_id(),
                    provider: party_id(),
                    create_transfer_rule: true,
                    create_allocation_factory: false,
                },
                package: ProposalPackage::GovernanceUtilityOnboarding,
                module: "Governance.UtilityOnboarding.CreateRegistrarServiceRequest",
                entity: "CreateRegistrarServiceRequest",
                labels: &[
                    "governanceParty",
                    "proposer",
                    "operator",
                    "provider",
                    "createTransferRule",
                    "createAllocationFactory",
                ],
            },
            Case {
                proposal: ProposalType::OnboardRegistrar {
                    provider_service_cid: "psc".to_string(),
                    registrar_service_request_cid: "rsrc".to_string(),
                    provider_configuration_cid: "pcc".to_string(),
                },
                package: ProposalPackage::GovernanceUtilityOnboarding,
                module: "Governance.UtilityOnboarding.OnboardRegistrar",
                entity: "OnboardRegistrar",
                labels: &[
                    "governanceParty",
                    "proposer",
                    "providerServiceCid",
                    "registrarServiceRequestCid",
                    "providerConfigurationCid",
                ],
            },
            Case {
                proposal: ProposalType::OnboardInstrumentIssuers {
                    instrument_configuration_cid: "icc".to_string(),
                    instrument_issuers: vec![party_id()],
                },
                package: ProposalPackage::GovernanceUtilityOnboarding,
                module: "Governance.UtilityOnboarding.OnboardInstrumentIssuers",
                entity: "OnboardInstrumentIssuers",
                labels: &[
                    "governanceParty",
                    "proposer",
                    "instrumentConfigurationCid",
                    "instrumentIssuers",
                ],
            },
            Case {
                proposal: ProposalType::OffboardInstrumentIssuers {
                    instrument_issuers: vec![InstrumentIssuerCredentials {
                        instrument_issuer: party_id(),
                        credential_cids: vec!["cred-1".to_string()],
                    }],
                },
                package: ProposalPackage::GovernanceUtilityOnboarding,
                module: "Governance.UtilityOnboarding.OffboardInstrumentIssuers",
                entity: "OffboardInstrumentIssuers",
                labels: &["governanceParty", "proposer", "instrumentIssuers"],
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

    #[test]
    fn build_proposal_accept_requests_serialize_issuer_credentials() -> Result {
        // The accept arms forward the supplied credential cids into the
        // `issuerCredentialCids` field as Some(list of ContractId). Labels
        // alone cannot catch a regression to the old hardcoded empty list.
        let proposals = [
            ProposalType::AcceptMintRequest {
                mint_request_cid: "mrc".to_string(),
                instrument_configuration_cid: "icc".to_string(),
                issuer_credential_cids: vec!["cred-1".to_string(), "cred-2".to_string()],
                description: "accept mint".to_string(),
            },
            ProposalType::AcceptBurnRequest {
                burn_request_cid: "brc".to_string(),
                instrument_configuration_cid: "icc".to_string(),
                issuer_credential_cids: vec!["cred-1".to_string(), "cred-2".to_string()],
                description: "accept burn".to_string(),
            },
        ];
        for proposal in proposals {
            let (_, module, _, record) =
                build_proposal_create_args("gov", "proposer", &proposal, None, None)?;
            let credentials = field_value(&record, "issuerCredentialCids");
            let inner = match &credentials.sum {
                Some(value::Sum::Optional(o)) => o.value.as_deref(),
                other => {
                    panic!("expected Optional for issuerCredentialCids in {module}, got {other:?}")
                }
            };
            let inner = inner.unwrap_or_else(|| panic!("expected Some list in {module}, got None"));
            let elements = match &inner.sum {
                Some(value::Sum::List(l)) => &l.elements,
                other => {
                    panic!("expected List inside Optional in {module}, got {other:?}")
                }
            };
            let cids: Vec<_> = elements
                .iter()
                .map(|v| match &v.sum {
                    Some(value::Sum::ContractId(cid)) => cid.as_str(),
                    other => panic!("expected ContractId element in {module}, got {other:?}"),
                })
                .collect();
            assert_eq!(cids, ["cred-1", "cred-2"], "cids for {module}");
        }
        Ok(())
    }

    #[test]
    fn build_proposal_accept_requests_empty_issuer_credentials_serialize_none() -> Result {
        // An empty list must serialize as Optional None, not Some []. Daml drops a
        // trailing added Optional field on downgrade only when it is None, so
        // Some [] would break every accept on a participant still running 0.2.0.
        let proposals = [
            ProposalType::AcceptMintRequest {
                mint_request_cid: "mrc".to_string(),
                instrument_configuration_cid: "icc".to_string(),
                issuer_credential_cids: vec![],
                description: "d".to_string(),
            },
            ProposalType::AcceptBurnRequest {
                burn_request_cid: "brc".to_string(),
                instrument_configuration_cid: "icc".to_string(),
                issuer_credential_cids: vec![],
                description: "d".to_string(),
            },
        ];
        for proposal in proposals {
            let (_, module, _, record) =
                build_proposal_create_args("gov", "proposer", &proposal, None, None)?;
            match &field_value(&record, "issuerCredentialCids").sum {
                Some(value::Sum::Optional(opt)) => {
                    assert!(opt.value.is_none(), "expected None for {module}");
                }
                other => panic!("expected Optional for {module}, got {other:?}"),
            }
        }
        Ok(())
    }

    /// Unwrap a `value::Sum::List` reference into its elements.
    fn as_list_elements<'a>(value: &'a Value, label: &str) -> &'a Vec<Value> {
        match &value.sum {
            Some(value::Sum::List(l)) => &l.elements,
            other => panic!("expected List for {label}, got {other:?}"),
        }
    }

    #[test]
    fn build_proposal_onboard_instrument_issuers_serializes_parties() -> Result {
        // The arm forwards the issuer parties into the `instrumentIssuers`
        // list as Party values.
        let issuer = party_id();
        let proposal = ProposalType::OnboardInstrumentIssuers {
            instrument_configuration_cid: "icc".to_string(),
            instrument_issuers: vec![issuer.clone()],
        };
        let (_, _, _, record) =
            build_proposal_create_args("gov", "proposer", &proposal, None, None)?;
        let issuers = field_value(&record, "instrumentIssuers");
        let parties: Vec<_> = as_list_elements(issuers, "instrumentIssuers")
            .iter()
            .map(|v| match &v.sum {
                Some(value::Sum::Party(p)) => p.as_str(),
                other => panic!("expected Party element, got {other:?}"),
            })
            .collect();
        assert_eq!(parties, [issuer.to_string().as_str()]);
        Ok(())
    }

    #[test]
    fn build_proposal_offboard_instrument_issuers_serializes_rows() -> Result {
        // The arm forwards each row as a record of a Party and a list of
        // ContractId. Labels alone cannot catch a regression to a flat list.
        let issuer = party_id();
        let proposal = ProposalType::OffboardInstrumentIssuers {
            instrument_issuers: vec![InstrumentIssuerCredentials {
                instrument_issuer: issuer.clone(),
                credential_cids: vec!["cred-1".to_string(), "cred-2".to_string()],
            }],
        };
        let (_, _, _, record) =
            build_proposal_create_args("gov", "proposer", &proposal, None, None)?;
        let rows = as_list_elements(
            field_value(&record, "instrumentIssuers"),
            "instrumentIssuers",
        );
        assert_eq!(rows.len(), 1);
        let row = match &rows[0].sum {
            Some(value::Sum::Record(r)) => r,
            other => panic!("expected Record element, got {other:?}"),
        };
        let party = match &field_value(row, "instrumentIssuer").sum {
            Some(value::Sum::Party(p)) => p.as_str(),
            other => panic!("expected Party, got {other:?}"),
        };
        assert_eq!(party, issuer.to_string().as_str());
        let cids: Vec<_> = as_list_elements(field_value(row, "credentialCids"), "credentialCids")
            .iter()
            .map(|v| match &v.sum {
                Some(value::Sum::ContractId(cid)) => cid.as_str(),
                other => panic!("expected ContractId element, got {other:?}"),
            })
            .collect();
        assert_eq!(cids, ["cred-1", "cred-2"]);
        Ok(())
    }

    /// Decode a serialized `[PartyCredentialRequirement]` field into
    /// `(issuer, [(property, value)])` tuples for terse assertions. Panics on
    /// any shape mismatch, including tuple fields not labeled `_1`/`_2`.
    fn requirement_tuples(record: &Record, label: &str) -> Vec<(String, Vec<(String, String)>)> {
        as_list_elements(field_value(record, label), label)
            .iter()
            .map(|element| {
                let requirement = as_record(element);
                assert_eq!(
                    owned_labels(requirement),
                    ["issuer", "requiredClaims"],
                    "requirement labels in {label}"
                );
                let issuer = match &field_value(requirement, "issuer").sum {
                    Some(value::Sum::Party(p)) => p.clone(),
                    other => panic!("expected Party for issuer in {label}, got {other:?}"),
                };
                let claims =
                    as_list_elements(field_value(requirement, "requiredClaims"), "requiredClaims")
                        .iter()
                        .map(|claim| {
                            let pair = as_record(claim);
                            assert_eq!(owned_labels(pair), ["_1", "_2"], "tuple labels in {label}");
                            let text = |l: &str| match &field_value(pair, l).sum {
                                Some(value::Sum::Text(t)) => t.clone(),
                                other => panic!("expected Text for {l} in {label}, got {other:?}"),
                            };
                            (text("_1"), text("_2"))
                        })
                        .collect();
                (issuer, claims)
            })
            .collect()
    }

    fn requirement(issuer: &CantonId, claims: &[(&str, &str)]) -> PartyCredentialRequirement {
        PartyCredentialRequirement {
            issuer: issuer.clone(),
            required_claims: claims
                .iter()
                .map(|(property, value)| RequiredClaim {
                    property: property.to_string(),
                    value: value.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn build_proposal_create_provider_configuration_serializes_requirements() -> Result {
        // Each requirement is a nested record whose `requiredClaims` list
        // holds `DA.Types:Tuple2 Text Text` records (fields `_1`/`_2`). The
        // registrar and holder lists carry distinct content, so a swap of
        // the two fields cannot pass.
        let issuer = party_id();
        let proposal = ProposalType::CreateProviderConfiguration {
            provider_service_cid: "psc".to_string(),
            registrar_requirements: vec![requirement(
                &issuer,
                &[("role", "registrar"), ("kyc", "passed")],
            )],
            holder_requirements: vec![requirement(&issuer, &[("role", "holder")])],
        };
        let (_, _, _, record) =
            build_proposal_create_args("gov", "proposer", &proposal, None, None)?;

        let registrar = requirement_tuples(&record, "registrarRequirements");
        assert_eq!(
            registrar,
            [(
                issuer.to_string(),
                vec![
                    ("role".to_string(), "registrar".to_string()),
                    ("kyc".to_string(), "passed".to_string()),
                ],
            )]
        );
        let holder = requirement_tuples(&record, "holderRequirements");
        assert_eq!(
            holder,
            [(
                issuer.to_string(),
                vec![("role".to_string(), "holder".to_string())],
            )]
        );
        Ok(())
    }

    #[test]
    fn build_proposal_provision_instrument_shape_and_nested_values() -> Result {
        let issuer = party_id();
        let proposal = ProposalType::ProvisionInstrument {
            registrar_service_cid: "rsc".to_string(),
            instrument_id_text: "uuid-1".to_string(),
            additional_identifiers: vec![InstrumentIdentifier {
                source: party_id(),
                id: "TICK".to_string(),
                scheme: "Ticker".to_string(),
            }],
            issuer_requirements: vec![requirement(&issuer, &[("role", "instrument-issuer")])],
            holder_requirements: vec![requirement(&issuer, &[("role", "holder")])],
            initial_instrument_issuers: vec![issuer.clone()],
        };
        let (_, _, _, record) =
            build_proposal_create_args("gov", "proposer", &proposal, None, None)?;

        // The identifier nesting mirrors the SetupUtility precedent.
        let identifiers = field_value(&record, "additionalIdentifiers");
        let first = as_list_elements(identifiers, "additionalIdentifiers")
            .first()
            .unwrap_or_else(|| panic!("additionalIdentifiers list is empty"));
        assert_eq!(owned_labels(as_record(first)), ["source", "id", "scheme"]);

        // Distinct issuer/holder requirement content, so a swap cannot pass.
        let issuer_reqs = requirement_tuples(&record, "issuerRequirements");
        assert_eq!(
            issuer_reqs,
            [(
                issuer.to_string(),
                vec![("role".to_string(), "instrument-issuer".to_string())],
            )]
        );
        let holder_reqs = requirement_tuples(&record, "holderRequirements");
        assert_eq!(
            holder_reqs,
            [(
                issuer.to_string(),
                vec![("role".to_string(), "holder".to_string())],
            )]
        );

        let issuers: Vec<_> =
            as_list_elements(field_value(&record, "initialInstrumentIssuers"), "issuers")
                .iter()
                .map(|v| match &v.sum {
                    Some(value::Sum::Party(p)) => p.as_str(),
                    other => panic!("expected Party element, got {other:?}"),
                })
                .collect();
        assert_eq!(issuers, [issuer.to_string().as_str()]);
        Ok(())
    }
}
