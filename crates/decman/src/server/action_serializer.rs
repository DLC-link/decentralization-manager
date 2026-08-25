//! Daml `Value` argument builders for governance choices, plus
//! serialization of `ProposalType` domain-governance proposals.
//!
//! `ActionType`'s own codec (`to_vault_proto` / `from_vault_proto` /
//! `to_self_proto` / `from_self_proto`) now lives in
//! `decman_lib::catalog::action`; the four `build_*_action*` functions below
//! are thin fallible wrappers around it.

#[cfg(test)]
use canton_common::decimal::DamlDecimal;
use canton_common::transfer_factory::Context as ChoiceContext;
use canton_proto_rs::com::daml::ledger::api::v2::{Optional, Record, Value, value};
use decman_lib::catalog::proposals::core::GenericVote;
use decman_lib::catalog::proposals::custody::{
    AcceptTransfer, AcceptTransferWithContext, SetupCcPreapproval, SetupTokenPreapproval, Transfer,
    TransferWithContext,
};
use decman_lib::catalog::proposals::rewards::{
    AcceptExternalPartySetup, RevokeCouponReassignmentDelegation,
    SetupCouponReassignmentDelegation, SetupMintingDelegation,
};
use decman_lib::catalog::proposals::utility::{
    CreateDelegatedBatchedMarkersProxy, CreateProviderServiceRequest, CreateUserServiceRequest,
    ProvisionProviderService,
};
pub(crate) use decman_lib::catalog::types::{
    make_optional_beneficiaries, serialize_billing_params,
};
use decman_lib::framework::commands::proposal_create_arguments;
pub(crate) use decman_lib::framework::encode::*;

use crate::canton_id::CantonId;
use crate::error::Result;

use super::types::{ActionType, ProposalType};
#[cfg(test)]
use super::types::{
    BillingParams, Claim, InstrumentId, InstrumentIdentifier, PartyCredentialRequirement,
};
#[cfg(test)]
use common::api::InstrumentAllowance;

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
    governance_party: &CantonId,
    proposer: &CantonId,
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
        ProposalType::SetupCcPreapproval(p) => (
            ProposalPackage::GovernanceTokenCustody,
            SetupCcPreapproval::MODULE,
            SetupCcPreapproval::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        ProposalType::SetupTokenPreapproval(p) => (
            ProposalPackage::GovernanceTokenCustody,
            SetupTokenPreapproval::MODULE,
            SetupTokenPreapproval::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        // The Daml `TransferFactory_Transfer` choice (invoked through
        // `TransferProposal`) and, for `AcceptTransfer`, the
        // `TransferInstruction_Accept` choice look up registry-published
        // entries (e.g. `utility.digitalasset.com/transfer-rule`) in
        // `extraArgs.context.values` at execution time. An empty context
        // would fail with `Missing context entry for ...`. The handler is
        // expected to fetch the choice context from the token-standard
        // registry and pass it in; if it didn't, the wrapper falls back to
        // an empty record (legacy callers, e.g. tests).
        ProposalType::Transfer(t) => (
            ProposalPackage::GovernanceTokenCustody,
            Transfer::MODULE,
            Transfer::ENTITY,
            proposal_create_arguments(
                &TransferWithContext {
                    transfer: t,
                    sender: governance_party,
                    context: transfer_choice_context,
                    validity,
                },
                governance_party,
                proposer,
            )
            .map_err(anyhow::Error::from)?,
        ),
        ProposalType::AcceptTransfer(a) => (
            ProposalPackage::GovernanceTokenCustody,
            AcceptTransfer::MODULE,
            AcceptTransfer::ENTITY,
            proposal_create_arguments(
                &AcceptTransferWithContext {
                    accept: a,
                    context: transfer_choice_context,
                },
                governance_party,
                proposer,
            )
            .map_err(anyhow::Error::from)?,
        ),
        ProposalType::GenericVote(p) => (
            ProposalPackage::GovernanceCore,
            GenericVote::MODULE,
            GenericVote::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        ProposalType::ProvisionProviderService(p) => (
            ProposalPackage::GovernanceUtilityOnboarding,
            ProvisionProviderService::MODULE,
            ProvisionProviderService::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
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
        ProposalType::CreateProviderServiceRequest(p) => (
            ProposalPackage::GovernanceUtilityOnboarding,
            CreateProviderServiceRequest::MODULE,
            CreateProviderServiceRequest::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        ProposalType::CreateUserServiceRequest(p) => (
            ProposalPackage::GovernanceUtilityOnboarding,
            CreateUserServiceRequest::MODULE,
            CreateUserServiceRequest::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
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
        ProposalType::SetupCouponReassignmentDelegation(p) => (
            ProposalPackage::GovernanceRewards,
            SetupCouponReassignmentDelegation::MODULE,
            SetupCouponReassignmentDelegation::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        ProposalType::RevokeCouponReassignmentDelegation(p) => (
            ProposalPackage::GovernanceRewards,
            RevokeCouponReassignmentDelegation::MODULE,
            RevokeCouponReassignmentDelegation::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
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
        ProposalType::CreateDelegatedBatchedMarkersProxy(p) => (
            ProposalPackage::GovernanceUtilityOnboarding,
            CreateDelegatedBatchedMarkersProxy::MODULE,
            CreateDelegatedBatchedMarkersProxy::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        ProposalType::SetupMintingDelegation(p) => (
            ProposalPackage::GovernanceRewards,
            SetupMintingDelegation::MODULE,
            SetupMintingDelegation::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        ProposalType::AcceptExternalPartySetup(p) => (
            ProposalPackage::GovernanceRewards,
            AcceptExternalPartySetup::MODULE,
            AcceptExternalPartySetup::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
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
    use super::*;
    use common::api::RequiredClaim;

    use crate::{
        canton_id::{NAMESPACE_LENGTH, Namespace},
        server::types::InstrumentIssuerCredentials,
    };

    // `transfer_validity_from_now_bounds_the_window` and
    // `transfer_validity_from_now_clamps_to_max_daml_time` moved to
    // `decman_lib::framework::encode::tests` with `TransferValidity` itself.

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

    /// The `governanceParty` / `proposer` CantonIds `build_proposal_create_args`
    /// injects. Neither value is asserted on below (only field labels /
    /// payload-carried parties are), so a fixed pair suffices everywhere.
    fn gov_id() -> CantonId {
        CantonId::new("gov".to_string(), Namespace::new([0u8; NAMESPACE_LENGTH]))
    }

    fn proposer_id() -> CantonId {
        CantonId::new(
            "proposer".to_string(),
            Namespace::new([0u8; NAMESPACE_LENGTH]),
        )
    }

    #[test]
    fn build_proposal_setup_cc_preapproval_shape() -> Result {
        let proposal = ProposalType::SetupCcPreapproval(SetupCcPreapproval {
            provider: party_id(),
            expected_dso: party_id(),
        });
        let (package, module, entity, record) =
            build_proposal_create_args(&gov_id(), &proposer_id(), &proposal, None, None)?;

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

    // `build_proposal_transfer_shape_and_nested_records` moved to
    // `decman_lib::catalog::proposals::custody::tests`, driven through
    // `TransferWithContext` directly — `Transfer` no longer implements
    // `DamlProtoEncode` on its own.

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
            build_proposal_create_args(&gov_id(), &proposer_id(), &mint, None, None)?;

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
            build_proposal_create_args(&gov_id(), &proposer_id(), &burn, None, None)?;

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

    // `build_proposal_setup_minting_delegation_shape`,
    // `build_proposal_accept_external_party_setup_shape`,
    // `build_proposal_setup_delegation_shape`, and
    // `build_proposal_revoke_delegation_shape` moved to
    // `decman_lib::catalog::proposals::rewards::tests` as `encode_snapshots`,
    // driven through the structs' own `DamlProtoEncode` directly.

    // `build_proposal_accept_transfer_shape_and_context_branches` moved to
    // `decman_lib::catalog::proposals::custody::tests`, driven through
    // `AcceptTransferWithContext` directly — `AcceptTransfer` no longer
    // implements `DamlProtoEncode` on its own.

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
            build_proposal_create_args(&gov_id(), &proposer_id(), &proposal, None, None)?;

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
            build_proposal_create_args(&gov_id(), &proposer_id(), &proposal, None, None)?;

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
                proposal: ProposalType::SetupTokenPreapproval(SetupTokenPreapproval {
                    operator: party_id(),
                    instrument_admin: party_id(),
                    instrument_allowances: vec![InstrumentAllowance {
                        id: "allow-1".to_string(),
                    }],
                }),
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
                proposal: ProposalType::CreateProviderServiceRequest(
                    CreateProviderServiceRequest {
                        operator: party_id(),
                        provider: party_id(),
                    },
                ),
                package: ProposalPackage::GovernanceUtilityOnboarding,
                module: "Governance.UtilityOnboarding.CreateProviderServiceRequest",
                entity: "CreateProviderServiceRequest",
                labels: &["governanceParty", "proposer", "operator", "provider"],
            },
            Case {
                proposal: ProposalType::CreateUserServiceRequest(CreateUserServiceRequest {
                    operator: party_id(),
                    user: party_id(),
                }),
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
                build_proposal_create_args(&gov_id(), &proposer_id(), &case.proposal, None, None)?;
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
                build_proposal_create_args(&gov_id(), &proposer_id(), &proposal, None, None)?;
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
                build_proposal_create_args(&gov_id(), &proposer_id(), &proposal, None, None)?;
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
            build_proposal_create_args(&gov_id(), &proposer_id(), &proposal, None, None)?;
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
            build_proposal_create_args(&gov_id(), &proposer_id(), &proposal, None, None)?;
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
            build_proposal_create_args(&gov_id(), &proposer_id(), &proposal, None, None)?;

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
            build_proposal_create_args(&gov_id(), &proposer_id(), &proposal, None, None)?;

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
