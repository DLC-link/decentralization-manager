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
    AcceptFreeCredential, AcceptPaidCredential, OfferFreeCredential, OfferPaidCredential,
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
    CreateTransferRule, CreateUserServiceRequest, Mint, OffboardInstrumentIssuers,
    OnboardInstrumentIssuers, OnboardRegistrar, ProvisionInstrument, ProvisionProviderService,
    RequestDevNetFeaturedAppRight, SetEnableResultContracts, SetProviderAppRewardBeneficiaries,
    SetupUtility, UpdateInstrumentConfiguration,
};
use decman_lib::catalog::types::{AppTransferContextInput, RewardBeneficiary};

use crate::canton_id::CantonId;
use crate::server::types::{ActionType, AppRewardBeneficiary, BillingParams, ProposalType};

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

/// One populated instance per `ActionType` variant (15), plus the `None` form
/// of `GovernanceSetDisclosureObservers`, the one variant with an `Option`.
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
        ActionType::GovernanceSetDisclosureObservers {
            disclosure_observers: Some(vec![cid("o1")]),
        },
        ActionType::GovernanceSetDisclosureObservers {
            disclosure_observers: None,
        },
        ActionType::GovernanceCancelAction {
            proposal_cid: "00proposal".into(),
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

/// One populated instance per `ProposalType` variant (30), every Option Some.
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
            use_block_list: Some(true),
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
        ProposalType::CreateTransferRule(CreateTransferRule {
            registrar_service_cid: "00rsc".into(),
        }),
        ProposalType::CreateDelegatedBatchedMarkersProxy(CreateDelegatedBatchedMarkersProxy {
            operator: cid("op"),
        }),
        ProposalType::RequestDevNetFeaturedAppRight(RequestDevNetFeaturedAppRight {
            amulet_rules_cid: "00amulet".into(),
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
        ProposalType::AcceptPaidCredential(AcceptPaidCredential {
            user_service_cid: "00usc".into(),
            credential_offer_cid: "00offer".into(),
            deposit_amulet_cids: vec!["00amulet1".into(), "00amulet2".into()],
            app_transfer_context: AppTransferContextInput {
                amulet_rules_cid: "00rules".into(),
                open_mining_round_cid: "00round".into(),
                featured_app_right_cid: Some("00far".into()),
            },
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
            credential_valid_from_micros: None,
            credential_valid_until_micros: None,
            use_block_list: Some(true),
        }),
        ProposalType::UpdateInstrumentConfiguration(UpdateInstrumentConfiguration {
            registrar_service_cid: "00rsc".into(),
            instrument_configuration_cid: "00icc".into(),
            instrument_id_text: "uuid-2".into(),
            additional_identifiers: vec![InstrumentIdentifier {
                source: cid("src"),
                id: "ident".into(),
                scheme: "scheme".into(),
            }],
            issuer_requirements: vec![requirement("gov")],
            holder_requirements: vec![requirement("gov")],
            use_block_list: Some(true),
        }),
        ProposalType::OnboardInstrumentIssuers(OnboardInstrumentIssuers {
            instrument_configuration_cid: "00icc".into(),
            instrument_issuers: vec![cid("iss1"), cid("iss2")],
            credential_valid_from_micros: None,
            credential_valid_until_micros: None,
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
        ProposalType::AcceptPaidCredential(AcceptPaidCredential {
            user_service_cid: "00usc".into(),
            credential_offer_cid: "00offer".into(),
            deposit_amulet_cids: vec!["00amulet1".into()],
            app_transfer_context: AppTransferContextInput {
                amulet_rules_cid: "00rules".into(),
                open_mining_round_cid: "00round".into(),
                featured_app_right_cid: None,
            },
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
            credential_valid_from_micros: None,
            credential_valid_until_micros: None,
            use_block_list: None,
        }),
        // Every field is required, and an unset flag goes out as null.
        ProposalType::UpdateInstrumentConfiguration(UpdateInstrumentConfiguration {
            registrar_service_cid: "00rsc".into(),
            instrument_configuration_cid: "00icc".into(),
            instrument_id_text: "uuid-2".into(),
            additional_identifiers: vec![],
            issuer_requirements: vec![],
            holder_requirements: vec![],
            use_block_list: None,
        }),
    ]
}

/// Where these snapshots live, relative to this file's directory. insta
/// resolves a snapshot path against the directory of the file holding the
/// assertion, so moving this module changes the path.
const SNAPSHOT_PATH: &str = "../../snapshots";

/// Bind [`SNAPSHOT_PATH`] for the calling test. The returned guard restores
/// the previous settings when it drops, so it must stay alive for the whole
/// test body.
#[must_use]
fn bind_snapshot_path() -> impl Drop {
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path(SNAPSHOT_PATH);
    settings.bind_to_scope()
}

#[test]
fn action_type_http_json_is_stable() {
    let _guard = bind_snapshot_path();
    insta::assert_json_snapshot!("action_types", all_action_fixtures());
}

#[test]
fn proposal_type_http_json_is_stable() {
    let _guard = bind_snapshot_path();
    insta::assert_json_snapshot!("proposal_types", all_proposal_fixtures());
}

#[test]
fn proposal_type_optional_fields_stay_omitted() {
    let _guard = bind_snapshot_path();
    insta::assert_json_snapshot!("proposal_types_minimal", minimal_option_fixtures());
}

// ---- Deserialize direction ----
//
// The snapshots above pin the serialize direction only. Both enums are
// inbound HTTP request bodies, so the frontend depends on deserialize. A
// round trip proves both directions at once: the internal `type` tag, every
// field name, and every `#[serde(default)]`.

#[test]
fn proposal_type_round_trips_through_json() {
    for proposal in all_proposal_fixtures() {
        let json = serde_json::to_value(&proposal).expect("serialize proposal");
        let back: ProposalType =
            serde_json::from_value(json.clone()).expect("deserialize proposal");
        assert_eq!(back, proposal, "round trip changed the value: {json}");
    }
}

/// The minimal fixtures omit every optional field, which is what a frontend
/// request body looks like. Deserializing them proves the omitted fields
/// still take their `#[serde(default)]`.
#[test]
fn proposal_type_minimal_round_trips_through_json() {
    for proposal in minimal_option_fixtures() {
        let json = serde_json::to_value(&proposal).expect("serialize proposal");
        let back: ProposalType =
            serde_json::from_value(json.clone()).expect("deserialize proposal");
        assert_eq!(back, proposal, "round trip changed the value: {json}");
    }
}

/// Clients written before `use_block_list` existed send no such key. Both
/// payloads read its absence as `None`, which leaves the block list check
/// off.
#[test]
fn use_block_list_is_none_when_omitted() {
    let setup: ProposalType = serde_json::from_value(serde_json::json!({
        "type": "setup_utility",
        "provider_service_cid": "00psc",
        "operator": cid("op"),
        "instrument_id_text": "uuid-1",
        "create_transfer_rule": true,
        "create_allocation_factory": true,
    }))
    .expect("deserialize setup_utility");
    let ProposalType::SetupUtility(setup) = setup else {
        panic!("expected setup_utility, got {setup:?}");
    };
    assert_eq!(setup.use_block_list, None);

    let provision: ProposalType = serde_json::from_value(serde_json::json!({
        "type": "provision_instrument",
        "registrar_service_cid": "00rsc",
        "instrument_id_text": "uuid-2",
    }))
    .expect("deserialize provision_instrument");
    let ProposalType::ProvisionInstrument(provision) = provision else {
        panic!("expected provision_instrument, got {provision:?}");
    };
    assert_eq!(provision.use_block_list, None);
}

/// The update replaces the whole configuration, so a request that leaves a
/// field out is refused instead of resetting that field. `null` still
/// clears the block list flag.
#[test]
fn update_instrument_configuration_requires_every_field() {
    let full = serde_json::json!({
        "type": "update_instrument_configuration",
        "registrar_service_cid": "00rsc",
        "instrument_configuration_cid": "00icc",
        "instrument_id_text": "uuid-2",
        "additional_identifiers": [],
        "issuer_requirements": [],
        "holder_requirements": [],
        "use_block_list": null,
    });
    serde_json::from_value::<ProposalType>(full.clone()).expect("a full body deserializes");
    for key in [
        "additional_identifiers",
        "issuer_requirements",
        "holder_requirements",
        "use_block_list",
    ] {
        let mut body = full.clone();
        body.as_object_mut().expect("object").remove(key);
        assert!(
            serde_json::from_value::<ProposalType>(body).is_err(),
            "a body without {key} must be refused"
        );
    }
}

#[test]
fn action_type_round_trips_through_json() {
    for action in all_action_fixtures() {
        let json = serde_json::to_value(&action).expect("serialize action");
        let back: ActionType = serde_json::from_value(json.clone()).expect("deserialize action");
        assert_eq!(back, action, "round trip changed the value: {json}");
    }
}
