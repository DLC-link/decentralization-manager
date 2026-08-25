//! Accessors for the fixed (non-payload-specific) governance template and
//! interface ids: the vault/core rules contracts, the confirmation
//! contracts, and the `GovernableAction` interface. Each payload's own
//! template id lives on its `TemplateInfo` impl instead (see
//! `catalog::proposals`) — this module covers only the ids decman needs
//! independent of any specific proposal payload.

use common::api::PackageConfig;

use crate::error::Error;
use crate::framework::TemplateId;

/// The `VaultGovernanceRules` template — the vault's governance-state
/// contract.
pub fn vault_rules_template(pkgs: &PackageConfig) -> Result<TemplateId, Error> {
    let pkg = pkgs
        .vault_governance
        .as_deref()
        .ok_or(Error::PackageNotConfigured("vault_governance"))?;
    Ok(TemplateId::new(
        pkg,
        "BitsafeVault.VaultGovernance",
        "VaultGovernanceRules",
    ))
}

/// The `GovernanceRules` template — governance-core's governance-state
/// contract.
pub fn governance_rules_template(pkgs: &PackageConfig) -> Result<TemplateId, Error> {
    let pkg = pkgs
        .governance_core
        .as_deref()
        .ok_or(Error::PackageNotConfigured("governance_core"))?;
    Ok(TemplateId::new(pkg, "Governance.Rules", "GovernanceRules"))
}

/// The `VaultGovernanceConfirmation` template.
pub fn vault_confirmation_template(pkgs: &PackageConfig) -> Result<TemplateId, Error> {
    let pkg = pkgs
        .vault_governance
        .as_deref()
        .ok_or(Error::PackageNotConfigured("vault_governance"))?;
    Ok(TemplateId::new(
        pkg,
        "BitsafeVault.VaultGovernance",
        "VaultGovernanceConfirmation",
    ))
}

/// The `GovernanceSelfConfirmation` template — governance-core's
/// vault-style self-management confirmations.
pub fn self_confirmation_template(pkgs: &PackageConfig) -> Result<TemplateId, Error> {
    let pkg = pkgs
        .governance_core
        .as_deref()
        .ok_or(Error::PackageNotConfigured("governance_core"))?;
    Ok(TemplateId::new(
        pkg,
        "Governance.Rules",
        "GovernanceSelfConfirmation",
    ))
}

/// The `GovernanceConfirmation` template — governance-core's domain-action
/// confirmations.
pub fn domain_confirmation_template(pkgs: &PackageConfig) -> Result<TemplateId, Error> {
    let pkg = pkgs
        .governance_core
        .as_deref()
        .ok_or(Error::PackageNotConfigured("governance_core"))?;
    Ok(TemplateId::new(
        pkg,
        "Governance.Confirmation",
        "GovernanceConfirmation",
    ))
}

/// The `GovernableAction` interface every catalog proposal implements.
pub fn governable_action_interface(pkgs: &PackageConfig) -> Result<TemplateId, Error> {
    let pkg = pkgs
        .governance_action
        .as_deref()
        .ok_or(Error::PackageNotConfigured("governance_action"))?;
    Ok(TemplateId::new(
        pkg,
        "Governance.Action",
        "GovernableAction",
    ))
}

/// Governance confirmation template identifiers.
///
/// Each template is queried separately to handle cases where packages may
/// not exist. Port of the pre-extraction `queries.rs::governance_templates`,
/// verbatim — including the hardcoded `#cbtc-governance`
/// `CBTC.Governance:Confirmation` entry, which has no configurable package
/// ref in `PackageConfig`.
pub fn governance_templates(packages: &PackageConfig) -> Vec<TemplateId> {
    let mut templates = Vec::new();
    if let Some(ref pkg) = packages.vault_governance {
        templates.push(TemplateId::new(
            pkg.clone(),
            "BitsafeVault.VaultGovernance",
            "VaultGovernanceConfirmation",
        ));
    }
    templates.push(TemplateId::new(
        "#cbtc-governance",
        "CBTC.Governance",
        "Confirmation",
    ));
    if let Some(ref pkg) = packages.governance_core {
        templates.push(TemplateId::new(
            pkg.clone(),
            "Governance.Rules",
            "GovernanceSelfConfirmation",
        ));
        templates.push(TemplateId::new(
            pkg.clone(),
            "Governance.Confirmation",
            "GovernanceConfirmation",
        ));
    }
    templates
}

/// Governance state template identifiers (tries both vault and core). Port
/// of the pre-extraction `queries.rs::governance_state_templates`, verbatim.
pub fn governance_state_templates(packages: &PackageConfig) -> Vec<TemplateId> {
    let mut templates = Vec::new();
    if let Some(ref pkg) = packages.vault_governance {
        templates.push(TemplateId::new(
            pkg.clone(),
            "BitsafeVault.VaultGovernance",
            "VaultGovernanceRules",
        ));
    }
    if let Some(ref pkg) = packages.governance_core {
        templates.push(TemplateId::new(
            pkg.clone(),
            "Governance.Rules",
            "GovernanceRules",
        ));
    }
    templates
}

#[cfg(test)]
mod tests {
    use canton_common::decimal::DamlDecimal;
    use common::api::InstrumentId;
    use common::canton_id::CantonId;

    use super::*;
    use crate::catalog::proposals::core::GenericVote;
    use crate::catalog::proposals::credential::{
        AcceptFreeCredential, OfferFreeCredential, OfferPaidCredential,
    };
    use crate::catalog::proposals::custody::{
        AcceptTransfer, SetupCcPreapproval, SetupTokenPreapproval, Transfer,
    };
    use crate::catalog::proposals::rewards::{
        AcceptExternalPartySetup, RevokeCouponReassignmentDelegation,
        SetupCouponReassignmentDelegation, SetupMintingDelegation,
    };
    use crate::catalog::proposals::utility::{
        AcceptBurnRequest, AcceptMintRequest, Burn, CreateDelegatedBatchedMarkersProxy,
        CreateProviderConfiguration, CreateProviderServiceRequest, CreateRegistrarServiceRequest,
        CreateUserServiceRequest, Mint, OffboardInstrumentIssuers, OnboardInstrumentIssuers,
        OnboardRegistrar, ProvisionInstrument, ProvisionProviderService, SetEnableResultContracts,
        SetProviderAppRewardBeneficiaries, SetupUtility,
    };
    use crate::catalog::types::BillingParams;
    use crate::framework::TemplateInfo;

    /// Any valid `CantonId` — the exact value is irrelevant to a
    /// `template_id` assertion.
    fn cid(prefix: &str) -> CantonId {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        CantonId::parse(&format!("{prefix}::{ns}")).unwrap()
    }

    fn decimal(s: &str) -> DamlDecimal {
        s.parse().expect("valid decimal")
    }

    fn instrument_id() -> InstrumentId {
        InstrumentId {
            admin: "admin::ns".to_string(),
            id: "inst".to_string(),
        }
    }

    /// The values `decman::config::default_package_config()` (config.rs:428)
    /// ships as the default package configuration for a new party.
    fn decman_default_packages() -> PackageConfig {
        PackageConfig {
            governance_action: Some("#governance-action-v1".to_string()),
            governance_core: Some("#governance-core-v1".to_string()),
            governance_rewards: Some("#governance-rewards-automation-v1".to_string()),
            governance_token_custody: Some("#governance-token-custody-v1".to_string()),
            governance_utility_credential: Some("#governance-utility-credential-v1".to_string()),
            governance_utility_onboarding: Some("#governance-utility-onboarding-v1".to_string()),
            utility_credential: Some("#utility-credential-v0".to_string()),
            utility_credential_app: Some("#utility-credential-app-v0".to_string()),
            utility_registry: Some("#utility-registry-app-v0".to_string()),
            vault: Some("#bitsafe-vault-v0-rc8".to_string()),
            vault_governance: Some("#bitsafe-vault-governance-v0-rc8".to_string()),
        }
    }

    #[test]
    fn template_ids_render_the_exact_strings() {
        let pkgs = decman_default_packages();
        let cases = [
            (
                governance_rules_template(&pkgs).unwrap(),
                "#governance-core-v1:Governance.Rules:GovernanceRules",
            ),
            (
                vault_rules_template(&pkgs).unwrap(),
                "#bitsafe-vault-governance-v0-rc8:BitsafeVault.VaultGovernance:VaultGovernanceRules",
            ),
            (
                vault_confirmation_template(&pkgs).unwrap(),
                "#bitsafe-vault-governance-v0-rc8:BitsafeVault.VaultGovernance:VaultGovernanceConfirmation",
            ),
            (
                self_confirmation_template(&pkgs).unwrap(),
                "#governance-core-v1:Governance.Rules:GovernanceSelfConfirmation",
            ),
            (
                domain_confirmation_template(&pkgs).unwrap(),
                "#governance-core-v1:Governance.Confirmation:GovernanceConfirmation",
            ),
            (
                governable_action_interface(&pkgs).unwrap(),
                "#governance-action-v1:Governance.Action:GovernableAction",
            ),
        ];
        for (id, expected) in cases {
            assert_eq!(id.to_string(), expected);
        }
    }

    #[test]
    fn unconfigured_package_is_a_typed_error() {
        let empty = PackageConfig {
            governance_core: None,
            ..decman_default_packages()
        };
        assert!(matches!(
            governance_rules_template(&empty),
            Err(Error::PackageNotConfigured("governance_core"))
        ));
    }

    /// One assertion per catalog proposal struct — all 29 `TemplateInfo`
    /// impls under `catalog::proposals`. `TransferWithContext` and
    /// `AcceptTransferWithContext` are excluded: they delegate
    /// `template_id` to the `Transfer` / `AcceptTransfer` they wrap, so
    /// their string is already covered here.
    ///
    /// Every expected string is a literal, not built from the struct's own
    /// `MODULE`/`ENTITY` consts, so a typo in either const still fails this
    /// test.
    #[test]
    fn template_id_assertions_per_payload_struct() {
        let pkgs = decman_default_packages();
        let cases: Vec<(String, &str)> = vec![
            (
                GenericVote {
                    description: "a vote".to_string(),
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-core-v1:Governance.GenericVote:GenericVoteProposal",
            ),
            (
                OfferFreeCredential {
                    user_service_cid: "usc".to_string(),
                    holder: cid("holder"),
                    id: "cred-1".to_string(),
                    description: "free cred".to_string(),
                    claims: vec![],
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-credential-v1:Governance.UtilityCredential.OfferFreeCredential:OfferFreeCredential",
            ),
            (
                OfferPaidCredential {
                    user_service_cid: "usc".to_string(),
                    holder: cid("holder"),
                    id: "cred-2".to_string(),
                    description: "paid cred".to_string(),
                    claims: vec![],
                    billing_params: BillingParams {
                        fee_per_day_usd: decimal("1.5"),
                        billing_period_minutes: 60,
                        deposit_target_amount_usd: decimal("10.0"),
                        holder_activity_weight: None,
                    },
                    deposit_initial_amount_usd: None,
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-credential-v1:Governance.UtilityCredential.OfferPaidCredential:OfferPaidCredential",
            ),
            (
                AcceptFreeCredential {
                    user_service_cid: "usc".to_string(),
                    credential_offer_cid: "offer".to_string(),
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-credential-v1:Governance.UtilityCredential.AcceptFreeCredential:AcceptFreeCredential",
            ),
            (
                SetupCcPreapproval {
                    provider: cid("provider"),
                    expected_dso: cid("dso"),
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-token-custody-v1:Governance.TokenCustody.SetupCcPreapproval:SetupCcPreapprovalProposal",
            ),
            (
                SetupTokenPreapproval {
                    operator: cid("operator"),
                    instrument_admin: cid("admin"),
                    instrument_allowances: vec![],
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-token-custody-v1:Governance.TokenCustody.SetupTokenPreapproval:SetupTokenPreapprovalProposal",
            ),
            (
                Transfer {
                    transfer_factory_cid: "tfc".to_string(),
                    expected_admin: cid("admin"),
                    receiver: cid("recv"),
                    amount: decimal("1.0"),
                    instrument_id: instrument_id(),
                    input_holding_cids: vec![],
                    validity_window_hours: None,
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-token-custody-v1:Governance.TokenCustody.TransferProposal:TransferProposal",
            ),
            (
                AcceptTransfer {
                    transfer_instruction_cid: "tic".to_string(),
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-token-custody-v1:Governance.TokenCustody.AcceptTransfer:AcceptTransferProposal",
            ),
            (
                SetupCouponReassignmentDelegation {
                    dso: cid("dso"),
                    assigners: vec![cid("m1")],
                    new_beneficiaries: vec![],
                    prior_delegation: None,
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-rewards-automation-v1:Governance.Rewards.SetupCouponReassignmentDelegation:SetupCouponReassignmentDelegation",
            ),
            (
                RevokeCouponReassignmentDelegation {
                    delegation: "00abc".to_string(),
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-rewards-automation-v1:Governance.Rewards.RevokeCouponReassignmentDelegation:RevokeCouponReassignmentDelegation",
            ),
            (
                SetupMintingDelegation {
                    delegate: cid("delegate"),
                    dso: cid("dso"),
                    expires_at_micros: 1,
                    amulet_merge_limit: 1,
                    description: "d".to_string(),
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-rewards-automation-v1:Governance.Rewards.SetupMintingDelegation:SetupMintingDelegation",
            ),
            (
                AcceptExternalPartySetup {
                    proposal_cid: "00abc123".to_string(),
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-rewards-automation-v1:Governance.Rewards.AcceptExternalPartySetup:AcceptExternalPartySetup",
            ),
            (
                ProvisionProviderService {}
                    .template_id(&pkgs)
                    .unwrap()
                    .to_string(),
                "#governance-utility-onboarding-v1:Governance.UtilityOnboarding.ProvisionProviderService:ProvisionProviderService",
            ),
            (
                CreateProviderServiceRequest {
                    operator: cid("operator"),
                    provider: cid("provider"),
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-onboarding-v1:Governance.UtilityOnboarding.CreateProviderServiceRequest:CreateProviderServiceRequest",
            ),
            (
                CreateUserServiceRequest {
                    operator: cid("operator"),
                    user: cid("user"),
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-onboarding-v1:Governance.UtilityOnboarding.CreateUserServiceRequest:CreateUserServiceRequest",
            ),
            (
                CreateDelegatedBatchedMarkersProxy {
                    operator: cid("operator"),
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-onboarding-v1:Governance.UtilityOnboarding.CreateDelegatedBatchedMarkersProxy:CreateDelegatedBatchedMarkersProxy",
            ),
            (
                SetupUtility {
                    provider_service_cid: "psc".to_string(),
                    operator: cid("operator"),
                    instrument_id_text: "iid".to_string(),
                    additional_identifiers: vec![],
                    create_transfer_rule: true,
                    create_allocation_factory: true,
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-onboarding-v1:Governance.UtilityOnboarding.SetupUtility:SetupUtility",
            ),
            (
                SetProviderAppRewardBeneficiaries {
                    instrument_configuration_cid: "icc".to_string(),
                    provider_app_reward_beneficiaries: None,
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-onboarding-v1:Governance.UtilityOnboarding.SetProviderAppRewardBeneficiaries:SetProviderAppRewardBeneficiaries",
            ),
            (
                SetEnableResultContracts {
                    registrar_service_cid: "rsc".to_string(),
                    enable_result_contracts: None,
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-onboarding-v1:Governance.UtilityOnboarding.SetEnableResultContracts:SetEnableResultContracts",
            ),
            (
                Mint {
                    allocation_factory_cid: "afc".to_string(),
                    instrument_id: instrument_id(),
                    instrument_configuration_cid: "icc".to_string(),
                    recipient: cid("recipient"),
                    amount: decimal("1.0"),
                    description: "d".to_string(),
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-onboarding-v1:Governance.TokenIssuance.MintProposal:MintProposal",
            ),
            (
                Burn {
                    allocation_factory_cid: "afc".to_string(),
                    instrument_id: instrument_id(),
                    instrument_configuration_cid: "icc".to_string(),
                    holder: cid("holder"),
                    amount: decimal("1.0"),
                    description: "d".to_string(),
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-onboarding-v1:Governance.TokenIssuance.BurnProposal:BurnProposal",
            ),
            (
                AcceptMintRequest {
                    mint_request_cid: "mrc".to_string(),
                    instrument_configuration_cid: "icc".to_string(),
                    issuer_credential_cids: vec![],
                    description: "d".to_string(),
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-onboarding-v1:Governance.TokenIssuance.AcceptMintRequest:AcceptMintRequest",
            ),
            (
                AcceptBurnRequest {
                    burn_request_cid: "brc".to_string(),
                    instrument_configuration_cid: "icc".to_string(),
                    issuer_credential_cids: vec![],
                    description: "d".to_string(),
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-onboarding-v1:Governance.TokenIssuance.AcceptBurnRequest:AcceptBurnRequest",
            ),
            (
                CreateProviderConfiguration {
                    provider_service_cid: "psc".to_string(),
                    registrar_requirements: vec![],
                    holder_requirements: vec![],
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-onboarding-v1:Governance.UtilityOnboarding.CreateProviderConfiguration:CreateProviderConfiguration",
            ),
            (
                CreateRegistrarServiceRequest {
                    operator: cid("operator"),
                    provider: cid("provider"),
                    create_transfer_rule: true,
                    create_allocation_factory: true,
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-onboarding-v1:Governance.UtilityOnboarding.CreateRegistrarServiceRequest:CreateRegistrarServiceRequest",
            ),
            (
                OnboardRegistrar {
                    provider_service_cid: "psc".to_string(),
                    registrar_service_request_cid: "rsrc".to_string(),
                    provider_configuration_cid: "pcc".to_string(),
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-onboarding-v1:Governance.UtilityOnboarding.OnboardRegistrar:OnboardRegistrar",
            ),
            (
                ProvisionInstrument {
                    registrar_service_cid: "rsc".to_string(),
                    instrument_id_text: "iid".to_string(),
                    additional_identifiers: vec![],
                    issuer_requirements: vec![],
                    holder_requirements: vec![],
                    initial_instrument_issuers: vec![],
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-onboarding-v1:Governance.UtilityOnboarding.ProvisionInstrument:ProvisionInstrument",
            ),
            (
                OnboardInstrumentIssuers {
                    instrument_configuration_cid: "icc".to_string(),
                    instrument_issuers: vec![cid("issuer")],
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-onboarding-v1:Governance.UtilityOnboarding.OnboardInstrumentIssuers:OnboardInstrumentIssuers",
            ),
            (
                OffboardInstrumentIssuers {
                    instrument_issuers: vec![],
                }
                .template_id(&pkgs)
                .unwrap()
                .to_string(),
                "#governance-utility-onboarding-v1:Governance.UtilityOnboarding.OffboardInstrumentIssuers:OffboardInstrumentIssuers",
            ),
        ];
        assert_eq!(cases.len(), 29, "one case per catalog proposal struct");
        for (actual, expected) in cases {
            assert_eq!(actual, expected);
        }
    }
}
