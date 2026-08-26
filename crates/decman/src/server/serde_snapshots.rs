//! Pins the HTTP-JSON shape of the two governance enums — `ProposalType`'s
//! newtype variants and decman-lib's `ActionType` — so the wire JSON never
//! drifts.

use canton_common::decimal::DamlDecimal;
use common::api::{
    Claim, InstrumentAllowance, InstrumentId, InstrumentIdentifier, InstrumentIssuerCredentials,
    PartyCredentialRequirement, RequiredClaim,
};
use decman_lib::catalog::proposals::core::GenericVote;
use decman_lib::catalog::proposals::credential::{
    AcceptFreeCredential, OfferFreeCredential, OfferPaidCredential,
};
use decman_lib::catalog::proposals::custody::{
    AcceptTransfer, SetupCcPreapproval, SetupTokenPreapproval, Transfer,
};
use decman_lib::catalog::proposals::rewards::{
    AcceptExternalPartySetup, RevokeCouponReassignmentDelegation,
    SetupCouponReassignmentDelegation, SetupMintingDelegation,
};
use decman_lib::catalog::proposals::utility::{
    AcceptBurnRequest, AcceptMintRequest, Burn, CreateDelegatedBatchedMarkersProxy,
    CreateProviderConfiguration, CreateProviderServiceRequest, CreateRegistrarServiceRequest,
    CreateUserServiceRequest, Mint, OffboardInstrumentIssuers, OnboardInstrumentIssuers,
    OnboardRegistrar, ProvisionInstrument, ProvisionProviderService, SetEnableResultContracts,
    SetProviderAppRewardBeneficiaries, SetupUtility,
};
use decman_lib::catalog::types::RewardBeneficiary;

use crate::canton_id::CantonId;
use crate::server::types::{
    ActionType, AppRewardBeneficiary, BillingParams, FarConfig, ProposalType, VaultLimits,
};

fn cid(prefix: &str) -> CantonId {
    let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
    CantonId::parse(&format!("{prefix}::{ns}")).unwrap()
}

fn dec(s: &str) -> DamlDecimal {
    s.parse().unwrap()
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

fn requirement(issuer: &str) -> PartyCredentialRequirement {
    PartyCredentialRequirement {
        issuer: cid(issuer),
        required_claims: vec![RequiredClaim {
            property: "role".into(),
            value: "issuer".into(),
        }],
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

/// One populated instance per `ActionType` variant (21), every Option Some.
fn all_action_fixtures() -> Vec<ActionType> {
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

/// One populated instance per `ProposalType` variant (29), every Option Some.
fn all_proposal_fixtures() -> Vec<ProposalType> {
    vec![
        ProposalType::SetupCcPreapproval(SetupCcPreapproval {
            provider: cid("prov"),
            expected_dso: cid("dso"),
        }),
        ProposalType::SetupTokenPreapproval(SetupTokenPreapproval {
            operator: cid("op"),
            instrument_admin: cid("iadmin"),
            instrument_allowances: vec![InstrumentAllowance { id: "TOK".into() }],
        }),
        ProposalType::Transfer(Transfer {
            transfer_factory_cid: "00tf".into(),
            expected_admin: cid("iadmin"),
            receiver: cid("recv"),
            amount: dec("12.5"),
            instrument_id: instrument(),
            input_holding_cids: vec!["00hold".into()],
            validity_window_hours: Some(48),
        }),
        ProposalType::AcceptTransfer(AcceptTransfer {
            transfer_instruction_cid: "00ti".into(),
        }),
        ProposalType::GenericVote(GenericVote {
            description: "a vote".into(),
        }),
        ProposalType::ProvisionProviderService(ProvisionProviderService {}),
        ProposalType::SetupUtility(SetupUtility {
            provider_service_cid: "00psc".into(),
            operator: cid("op"),
            instrument_id_text: "uuid-1".into(),
            additional_identifiers: vec![InstrumentIdentifier {
                source: cid("src"),
                id: "ident".into(),
                scheme: "scheme".into(),
            }],
            create_transfer_rule: true,
            create_allocation_factory: true,
        }),
        ProposalType::CreateProviderServiceRequest(CreateProviderServiceRequest {
            operator: cid("op"),
            provider: cid("prov"),
        }),
        ProposalType::CreateUserServiceRequest(CreateUserServiceRequest {
            operator: cid("op"),
            user: cid("user"),
        }),
        ProposalType::SetProviderAppRewardBeneficiaries(SetProviderAppRewardBeneficiaries {
            instrument_configuration_cid: "00icc".into(),
            provider_app_reward_beneficiaries: Some(vec![AppRewardBeneficiary {
                beneficiary: cid("b1"),
                weight: dec("1.0"),
            }]),
        }),
        ProposalType::SetupCouponReassignmentDelegation(SetupCouponReassignmentDelegation {
            dso: cid("dso"),
            assigners: vec![cid("m1"), cid("m2")],
            new_beneficiaries: vec![
                RewardBeneficiary {
                    beneficiary: cid("a"),
                    percentage: dec("0.8"),
                },
                RewardBeneficiary {
                    beneficiary: cid("b"),
                    percentage: dec("0.2"),
                },
            ],
            prior_delegation: Some("00prior".into()),
        }),
        ProposalType::RevokeCouponReassignmentDelegation(RevokeCouponReassignmentDelegation {
            delegation: "00deleg".into(),
        }),
        ProposalType::SetEnableResultContracts(SetEnableResultContracts {
            registrar_service_cid: "00rsc".into(),
            enable_result_contracts: Some(true),
        }),
        ProposalType::CreateDelegatedBatchedMarkersProxy(CreateDelegatedBatchedMarkersProxy {
            operator: cid("op"),
        }),
        ProposalType::SetupMintingDelegation(SetupMintingDelegation {
            delegate: cid("delegate"),
            dso: cid("dso"),
            expires_at_micros: 4_000_000_000_000_000,
            amulet_merge_limit: 10,
            description: "delegate minting".into(),
        }),
        ProposalType::AcceptExternalPartySetup(AcceptExternalPartySetup {
            proposal_cid: "00eps".into(),
        }),
        ProposalType::Mint(Mint {
            allocation_factory_cid: "00alloc".into(),
            instrument_id: instrument(),
            instrument_configuration_cid: "00icc".into(),
            recipient: cid("recv"),
            amount: dec("5"),
            description: "mint".into(),
        }),
        ProposalType::OfferFreeCredential(OfferFreeCredential {
            user_service_cid: "00usc".into(),
            holder: cid("holder"),
            id: "cred-1".into(),
            description: "free cred".into(),
            claims: vec![claim()],
        }),
        ProposalType::OfferPaidCredential(OfferPaidCredential {
            user_service_cid: "00usc".into(),
            holder: cid("holder"),
            id: "cred-2".into(),
            description: "paid cred".into(),
            claims: vec![claim()],
            billing_params: BillingParams {
                fee_per_day_usd: dec("1.5"),
                billing_period_minutes: 60,
                deposit_target_amount_usd: dec("30"),
                holder_activity_weight: Some(dec("0.5")),
            },
            deposit_initial_amount_usd: Some(dec("10")),
        }),
        ProposalType::AcceptFreeCredential(AcceptFreeCredential {
            user_service_cid: "00usc".into(),
            credential_offer_cid: "00offer".into(),
        }),
        ProposalType::Burn(Burn {
            allocation_factory_cid: "00alloc".into(),
            instrument_id: instrument(),
            instrument_configuration_cid: "00icc".into(),
            holder: cid("holder"),
            amount: dec("3"),
            description: "burn".into(),
        }),
        ProposalType::AcceptMintRequest(AcceptMintRequest {
            mint_request_cid: "00mr".into(),
            instrument_configuration_cid: "00icc".into(),
            issuer_credential_cids: vec!["00cred".into()],
            description: "accept mint".into(),
        }),
        ProposalType::AcceptBurnRequest(AcceptBurnRequest {
            burn_request_cid: "00br".into(),
            instrument_configuration_cid: "00icc".into(),
            issuer_credential_cids: vec!["00cred".into()],
            description: "accept burn".into(),
        }),
        ProposalType::CreateProviderConfiguration(CreateProviderConfiguration {
            provider_service_cid: "00psc".into(),
            registrar_requirements: vec![requirement("gov")],
            holder_requirements: vec![requirement("other")],
        }),
        ProposalType::CreateRegistrarServiceRequest(CreateRegistrarServiceRequest {
            operator: cid("op"),
            provider: cid("prov"),
            create_transfer_rule: false,
            create_allocation_factory: true,
        }),
        ProposalType::OnboardRegistrar(OnboardRegistrar {
            provider_service_cid: "00psc".into(),
            registrar_service_request_cid: "00rsr".into(),
            provider_configuration_cid: "00pcc".into(),
        }),
        ProposalType::ProvisionInstrument(ProvisionInstrument {
            registrar_service_cid: "00rsc".into(),
            instrument_id_text: "uuid-2".into(),
            additional_identifiers: vec![InstrumentIdentifier {
                source: cid("src"),
                id: "ident".into(),
                scheme: "scheme".into(),
            }],
            issuer_requirements: vec![requirement("gov")],
            holder_requirements: vec![],
            initial_instrument_issuers: vec![cid("iss1")],
        }),
        ProposalType::OnboardInstrumentIssuers(OnboardInstrumentIssuers {
            instrument_configuration_cid: "00icc".into(),
            instrument_issuers: vec![cid("iss1"), cid("iss2")],
        }),
        ProposalType::OffboardInstrumentIssuers(OffboardInstrumentIssuers {
            instrument_issuers: vec![InstrumentIssuerCredentials {
                instrument_issuer: cid("iss1"),
                credential_cids: vec!["00cred".into()],
            }],
        }),
    ]
}

/// Variants that carry `Option` / `#[serde(default)]` fields, with those
/// fields None/empty — pins the `skip_serializing_if` behavior.
fn minimal_option_fixtures() -> Vec<ProposalType> {
    vec![
        ProposalType::SetupTokenPreapproval(SetupTokenPreapproval {
            operator: cid("op"),
            instrument_admin: cid("iadmin"),
            instrument_allowances: vec![],
        }),
        ProposalType::Transfer(Transfer {
            transfer_factory_cid: "".into(),
            expected_admin: cid("iadmin"),
            receiver: cid("recv"),
            amount: dec("1"),
            instrument_id: instrument(),
            input_holding_cids: vec![],
            validity_window_hours: None,
        }),
        ProposalType::SetProviderAppRewardBeneficiaries(SetProviderAppRewardBeneficiaries {
            instrument_configuration_cid: "00icc".into(),
            provider_app_reward_beneficiaries: None,
        }),
        ProposalType::SetupCouponReassignmentDelegation(SetupCouponReassignmentDelegation {
            dso: cid("dso"),
            assigners: vec![cid("m1")],
            new_beneficiaries: vec![RewardBeneficiary {
                beneficiary: cid("a"),
                percentage: dec("1.0"),
            }],
            prior_delegation: None,
        }),
        ProposalType::SetEnableResultContracts(SetEnableResultContracts {
            registrar_service_cid: "00rsc".into(),
            enable_result_contracts: None,
        }),
        ProposalType::OfferPaidCredential(OfferPaidCredential {
            user_service_cid: "00usc".into(),
            holder: cid("holder"),
            id: "cred-2".into(),
            description: "paid cred".into(),
            claims: vec![],
            billing_params: BillingParams {
                fee_per_day_usd: dec("1.5"),
                billing_period_minutes: 60,
                deposit_target_amount_usd: dec("30"),
                holder_activity_weight: None,
            },
            deposit_initial_amount_usd: None,
        }),
        ProposalType::AcceptMintRequest(AcceptMintRequest {
            mint_request_cid: "00mr".into(),
            instrument_configuration_cid: "00icc".into(),
            issuer_credential_cids: vec![],
            description: "accept mint".into(),
        }),
        ProposalType::ProvisionInstrument(ProvisionInstrument {
            registrar_service_cid: "00rsc".into(),
            instrument_id_text: "uuid-2".into(),
            additional_identifiers: vec![],
            issuer_requirements: vec![],
            holder_requirements: vec![],
            initial_instrument_issuers: vec![],
        }),
    ]
}

#[test]
fn action_type_http_json_is_stable() {
    insta::assert_json_snapshot!("action_types", all_action_fixtures());
}

#[test]
fn proposal_type_http_json_is_stable() {
    insta::assert_json_snapshot!("proposal_types", all_proposal_fixtures());
}

#[test]
fn proposal_type_optional_fields_stay_omitted() {
    insta::assert_json_snapshot!("proposal_types_minimal", minimal_option_fixtures());
}

#[test]
fn action_minimal_options_stay_omitted() {
    let fixtures = vec![
        ActionType::VaultDeployment {
            vault_rules_cid: "00vaultrules".into(),
            vault_name: "Vault One".into(),
            share_symbol: "V1".into(),
            asset_instrument_id: instrument(),
            limits: VaultLimits {
                max_total_deposit: None,
                min_deposit_amount: None,
                min_withdrawal_amount: None,
            },
            vault_backend_signatory: cid("backend"),
            vault_far_config: None,
            allocation_factory_cid: "00alloc".into(),
            registrar_service_cid: "00reg".into(),
        },
        ActionType::ProcessorDeploymentRequest {
            vault_processor_rules_cid: "00proc".into(),
            vault_backend_signatory: cid("backend"),
            allocation_factory_cid: "00alloc".into(),
            processor_far_config: None,
            initial_supported_vaults: vec![],
        },
    ];
    insta::assert_json_snapshot!("action_types_minimal", fixtures);
}
