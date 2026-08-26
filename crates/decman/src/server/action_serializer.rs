//! Serialization of `ProposalType` domain-governance proposals.
//!
//! `ActionType`'s own codec (`to_vault_proto` / `from_vault_proto` /
//! `to_self_proto` / `from_self_proto`) lives in
//! `decman_lib::catalog::action`, and the confirm/execute/expire/cancel
//! choice arguments are built by `decman_lib::catalog::commands`. What is
//! left here is the `ProposalType` create-arguments dispatch, plus the one
//! confirm argument `propose_action`'s step-2 self-confirm still builds by
//! hand.

use canton_common::transfer_factory::Context as ChoiceContext;
use canton_proto_rs::com::daml::ledger::api::v2::{Record, Value};
use decman_lib::catalog::proposals::core::GenericVote;
use decman_lib::catalog::proposals::credential::{
    AcceptFreeCredential, OfferFreeCredential, OfferPaidCredential,
};
use decman_lib::catalog::proposals::custody::{
    AcceptTransfer, AcceptTransferWithContext, SetupCcPreapproval, SetupTokenPreapproval, Transfer,
    TransferWithContext,
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
use decman_lib::framework::commands::proposal_create_arguments;
pub(crate) use decman_lib::framework::encode::*;

use crate::canton_id::CantonId;
use crate::error::Result;

use super::types::ProposalType;
#[cfg(test)]
use common::api::InstrumentAllowance;

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
        ProposalType::SetupUtility(p) => (
            ProposalPackage::GovernanceUtilityOnboarding,
            SetupUtility::MODULE,
            SetupUtility::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
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
        ProposalType::SetProviderAppRewardBeneficiaries(p) => (
            ProposalPackage::GovernanceUtilityOnboarding,
            SetProviderAppRewardBeneficiaries::MODULE,
            SetProviderAppRewardBeneficiaries::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
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
        ProposalType::SetEnableResultContracts(p) => (
            ProposalPackage::GovernanceUtilityOnboarding,
            SetEnableResultContracts::MODULE,
            SetEnableResultContracts::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
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
        ProposalType::Mint(p) => (
            ProposalPackage::GovernanceUtilityOnboarding,
            Mint::MODULE,
            Mint::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        ProposalType::OfferFreeCredential(p) => (
            ProposalPackage::GovernanceUtilityCredential,
            OfferFreeCredential::MODULE,
            OfferFreeCredential::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        ProposalType::OfferPaidCredential(p) => (
            ProposalPackage::GovernanceUtilityCredential,
            OfferPaidCredential::MODULE,
            OfferPaidCredential::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        ProposalType::AcceptFreeCredential(p) => (
            ProposalPackage::GovernanceUtilityCredential,
            AcceptFreeCredential::MODULE,
            AcceptFreeCredential::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        ProposalType::Burn(p) => (
            ProposalPackage::GovernanceUtilityOnboarding,
            Burn::MODULE,
            Burn::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        ProposalType::AcceptMintRequest(p) => (
            ProposalPackage::GovernanceUtilityOnboarding,
            AcceptMintRequest::MODULE,
            AcceptMintRequest::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        ProposalType::AcceptBurnRequest(p) => (
            ProposalPackage::GovernanceUtilityOnboarding,
            AcceptBurnRequest::MODULE,
            AcceptBurnRequest::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        ProposalType::CreateProviderConfiguration(p) => (
            ProposalPackage::GovernanceUtilityOnboarding,
            CreateProviderConfiguration::MODULE,
            CreateProviderConfiguration::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        ProposalType::CreateRegistrarServiceRequest(p) => (
            ProposalPackage::GovernanceUtilityOnboarding,
            CreateRegistrarServiceRequest::MODULE,
            CreateRegistrarServiceRequest::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        ProposalType::OnboardRegistrar(p) => (
            ProposalPackage::GovernanceUtilityOnboarding,
            OnboardRegistrar::MODULE,
            OnboardRegistrar::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        ProposalType::ProvisionInstrument(p) => (
            ProposalPackage::GovernanceUtilityOnboarding,
            ProvisionInstrument::MODULE,
            ProvisionInstrument::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        ProposalType::OnboardInstrumentIssuers(p) => (
            ProposalPackage::GovernanceUtilityOnboarding,
            OnboardInstrumentIssuers::MODULE,
            OnboardInstrumentIssuers::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
        ),
        ProposalType::OffboardInstrumentIssuers(p) => (
            ProposalPackage::GovernanceUtilityOnboarding,
            OffboardInstrumentIssuers::MODULE,
            OffboardInstrumentIssuers::ENTITY,
            proposal_create_arguments(p, governance_party, proposer)
                .map_err(anyhow::Error::from)?,
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

#[cfg(test)]
mod tests {
    use super::*;
    use common::api::InstrumentIssuerCredentials;

    use crate::canton_id::{NAMESPACE_LENGTH, Namespace};

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

    /// The ordered field labels of an owned `Record`.
    fn owned_labels(record: &Record) -> Vec<&str> {
        record.fields.iter().map(|f| f.label.as_str()).collect()
    }

    // `build_proposal_transfer_shape_and_nested_records` moved to
    // `decman_lib::catalog::proposals::custody::tests`, driven through
    // `TransferWithContext` directly — `Transfer` no longer implements
    // `DamlProtoEncode` on its own.

    // `build_proposal_mint_and_burn_shapes_differ_only_in_party_label` moved to
    // `decman_lib::catalog::proposals::utility::tests` as `encode_snapshots`
    // (`mint` / `burn`), driven through `Mint::to_daml_proto` /
    // `Burn::to_daml_proto` directly — same coverage (the nested
    // `instrumentId` record and the `recipient`/`holder` party-label
    // difference included), pinned by insta instead of by hand-written label
    // asserts.

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

    // `build_proposal_offer_paid_credential_shape_and_billing_params` moved to
    // `decman_lib::catalog::proposals::credential::tests` as `encode_snapshots`
    // (`offer_paid_credential_deposit_and_weight_some` /
    // `..._none`), driven through `OfferPaidCredential::to_daml_proto`
    // directly — same coverage (billingParams' nested `feePerDayUsd` record
    // included), pinned by insta instead of by hand-written label asserts.

    // `build_proposal_setup_utility_shape_and_nested_identifier` moved to
    // `decman_lib::catalog::proposals::utility::tests` as `encode_snapshots`
    // (`setup_utility`), driven through `SetupUtility::to_daml_proto`
    // directly — same coverage (the nested `additionalIdentifiers` record
    // included), pinned by insta instead of by hand-written label asserts.

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
                proposal: ProposalType::AcceptFreeCredential(AcceptFreeCredential {
                    user_service_cid: "usc".to_string(),
                    credential_offer_cid: "coc".to_string(),
                }),
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
                proposal: ProposalType::AcceptMintRequest(AcceptMintRequest {
                    mint_request_cid: "mrc".to_string(),
                    instrument_configuration_cid: "icc".to_string(),
                    issuer_credential_cids: vec!["cred-1".to_string(), "cred-2".to_string()],
                    description: "accept mint".to_string(),
                }),
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
                proposal: ProposalType::AcceptBurnRequest(AcceptBurnRequest {
                    burn_request_cid: "brc".to_string(),
                    instrument_configuration_cid: "icc".to_string(),
                    issuer_credential_cids: vec!["cred-1".to_string()],
                    description: "accept burn".to_string(),
                }),
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
                proposal: ProposalType::CreateProviderConfiguration(CreateProviderConfiguration {
                    provider_service_cid: "psc".to_string(),
                    registrar_requirements: vec![],
                    holder_requirements: vec![],
                }),
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
                proposal: ProposalType::ProvisionInstrument(ProvisionInstrument {
                    registrar_service_cid: "rsc".to_string(),
                    instrument_id_text: "uuid-1".to_string(),
                    additional_identifiers: vec![],
                    issuer_requirements: vec![],
                    holder_requirements: vec![],
                    initial_instrument_issuers: vec![party_id()],
                }),
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
                proposal: ProposalType::CreateRegistrarServiceRequest(
                    CreateRegistrarServiceRequest {
                        operator: party_id(),
                        provider: party_id(),
                        create_transfer_rule: true,
                        create_allocation_factory: false,
                    },
                ),
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
                proposal: ProposalType::OnboardRegistrar(OnboardRegistrar {
                    provider_service_cid: "psc".to_string(),
                    registrar_service_request_cid: "rsrc".to_string(),
                    provider_configuration_cid: "pcc".to_string(),
                }),
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
                proposal: ProposalType::OnboardInstrumentIssuers(OnboardInstrumentIssuers {
                    instrument_configuration_cid: "icc".to_string(),
                    instrument_issuers: vec![party_id()],
                }),
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
                proposal: ProposalType::OffboardInstrumentIssuers(OffboardInstrumentIssuers {
                    instrument_issuers: vec![InstrumentIssuerCredentials {
                        instrument_issuer: party_id(),
                        credential_cids: vec!["cred-1".to_string()],
                    }],
                }),
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

    // `build_proposal_accept_requests_serialize_issuer_credentials` and
    // `build_proposal_accept_requests_empty_issuer_credentials_serialize_none`
    // moved to `decman_lib::catalog::proposals::utility::tests` as
    // `encode_snapshots` (`accept_mint_request_some` / `_none`,
    // `accept_burn_request_some` / `_none`), driven through
    // `AcceptMintRequest::to_daml_proto` / `AcceptBurnRequest::to_daml_proto`
    // directly — same coverage (the `issuerCredentialCids` Some/None
    // distinction included), pinned by insta instead of by hand-written
    // asserts.

    // `build_proposal_onboard_instrument_issuers_serializes_parties` and
    // `build_proposal_offboard_instrument_issuers_serializes_rows` moved to
    // `decman_lib::catalog::proposals::utility::tests` as `encode_snapshots`
    // (`onboard_instrument_issuers`, `offboard_instrument_issuers`), driven
    // through the structs' own `DamlProtoEncode` directly.

    // `build_proposal_create_provider_configuration_serializes_requirements`
    // and `build_proposal_provision_instrument_shape_and_nested_values` moved
    // to `decman_lib::catalog::proposals::utility::tests` as
    // `encode_snapshots` (`create_provider_configuration`,
    // `provision_instrument_populated` / `_empty`), driven through the
    // structs' own `DamlProtoEncode` directly — same coverage (the nested
    // `requiredClaims` tuples and `additionalIdentifiers` record included),
    // pinned by insta instead of by hand-written label asserts.
}
