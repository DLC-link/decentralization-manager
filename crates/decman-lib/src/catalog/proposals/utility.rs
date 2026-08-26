//! `governance-utility-onboarding` proposal payloads.

use std::collections::HashSet;

use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::daml::ledger::api::v2::{Value, value};
use common::api::{
    InstrumentId, InstrumentIdentifier, InstrumentIssuerCredentials, PartyCredentialRequirement,
};
use common::canton_id::CantonId;

use crate::catalog::types::{AppRewardBeneficiary, make_optional_beneficiaries};
use crate::error::Error;
use crate::framework::encode::{
    field, make_bool, make_contract_id, make_empty_metadata, make_list, make_numeric,
    make_optional_bool, make_optional_list, make_party, make_record, make_text,
    serialize_instrument_id, serialize_instrument_identifier,
    serialize_party_credential_requirements,
};
use crate::framework::validate::{
    validate_beneficiary_weights, validate_positive_amount,
    validate_self_issued_requirements_have_claims, validate_unique_issuers,
};
use crate::framework::{
    DamlProtoEncode, PackageResolver, TemplateId, TemplateInfo, Validate, ValidationCtx,
};

/// Provision a Utility-Registry `ProviderService` with `operator = proposer`
/// and `provider = governanceParty`. Produces the ProviderService cid
/// consumed by `SetupUtility`.
///
/// Empty braces, not a unit struct: serde cannot internally-tag a newtype
/// variant wrapping a true unit struct (there is no map to insert the `type`
/// tag into). An empty-braces struct serializes as an empty map, so the
/// enum's `#[serde(tag = "type")]` still produces
/// `{"type":"provision_provider_service"}` on the wire.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ProvisionProviderService {}

impl ProvisionProviderService {
    pub const MODULE: &'static str = "Governance.UtilityOnboarding.ProvisionProviderService";
    pub const ENTITY: &'static str = "ProvisionProviderService";
}

impl TemplateInfo for ProvisionProviderService {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for ProvisionProviderService {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![]))
    }
}

impl Validate for ProvisionProviderService {}

/// Create a `ProviderServiceRequest` for a given `operator` and `provider`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct CreateProviderServiceRequest {
    pub operator: CantonId,
    pub provider: CantonId,
}

impl CreateProviderServiceRequest {
    pub const MODULE: &'static str = "Governance.UtilityOnboarding.CreateProviderServiceRequest";
    pub const ENTITY: &'static str = "CreateProviderServiceRequest";
}

impl TemplateInfo for CreateProviderServiceRequest {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for CreateProviderServiceRequest {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field("operator", make_party(&self.operator)),
            field("provider", make_party(&self.provider)),
        ]))
    }
}

impl Validate for CreateProviderServiceRequest {}

/// Create a `UserServiceRequest` for a given `operator` and `user`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct CreateUserServiceRequest {
    pub operator: CantonId,
    pub user: CantonId,
}

impl CreateUserServiceRequest {
    pub const MODULE: &'static str = "Governance.UtilityOnboarding.CreateUserServiceRequest";
    pub const ENTITY: &'static str = "CreateUserServiceRequest";
}

impl TemplateInfo for CreateUserServiceRequest {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for CreateUserServiceRequest {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field("operator", make_party(&self.operator)),
            field("user", make_party(&self.user)),
        ]))
    }
}

impl Validate for CreateUserServiceRequest {}

/// Authorize the `operator` to create batched activity markers on behalf of
/// the governance party via a `DelegatedBatchedMarkersProxy`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct CreateDelegatedBatchedMarkersProxy {
    pub operator: CantonId,
}

impl CreateDelegatedBatchedMarkersProxy {
    pub const MODULE: &'static str =
        "Governance.UtilityOnboarding.CreateDelegatedBatchedMarkersProxy";
    pub const ENTITY: &'static str = "CreateDelegatedBatchedMarkersProxy";
}

impl TemplateInfo for CreateDelegatedBatchedMarkersProxy {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for CreateDelegatedBatchedMarkersProxy {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![field(
            "operator",
            make_party(&self.operator),
        )]))
    }
}

impl Validate for CreateDelegatedBatchedMarkersProxy {}

/// Run the full Utility-Registry onboarding in one vote. Flags control
/// whether a `TransferRule` / `AllocationFactory` are created during the
/// `RegistrarServiceRequest` accept.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct SetupUtility {
    pub provider_service_cid: String,
    pub operator: CantonId,
    pub instrument_id_text: String,
    #[serde(default)]
    pub additional_identifiers: Vec<InstrumentIdentifier>,
    pub create_transfer_rule: bool,
    pub create_allocation_factory: bool,
}

impl SetupUtility {
    pub const MODULE: &'static str = "Governance.UtilityOnboarding.SetupUtility";
    pub const ENTITY: &'static str = "SetupUtility";
}

impl TemplateInfo for SetupUtility {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for SetupUtility {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field(
                "providerServiceCid",
                make_contract_id(&self.provider_service_cid),
            ),
            field("operator", make_party(&self.operator)),
            field("instrumentIdText", make_text(&self.instrument_id_text)),
            field(
                "additionalIdentifiers",
                make_list(
                    self.additional_identifiers
                        .iter()
                        .map(serialize_instrument_identifier)
                        .collect(),
                ),
            ),
            field("createTransferRule", make_bool(self.create_transfer_rule)),
            field(
                "createAllocationFactory",
                make_bool(self.create_allocation_factory),
            ),
        ]))
    }
}

impl Validate for SetupUtility {}

/// Set the provider-app reward beneficiaries on an `InstrumentConfiguration`.
/// `providerAppRewardBeneficiaries = None` clears the current setting.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct SetProviderAppRewardBeneficiaries {
    pub instrument_configuration_cid: String,
    #[serde(default)]
    pub provider_app_reward_beneficiaries: Option<Vec<AppRewardBeneficiary>>,
}

impl SetProviderAppRewardBeneficiaries {
    pub const MODULE: &'static str =
        "Governance.UtilityOnboarding.SetProviderAppRewardBeneficiaries";
    pub const ENTITY: &'static str = "SetProviderAppRewardBeneficiaries";
}

impl TemplateInfo for SetProviderAppRewardBeneficiaries {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for SetProviderAppRewardBeneficiaries {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field(
                "instrumentConfigurationCid",
                make_contract_id(&self.instrument_configuration_cid),
            ),
            field(
                "providerAppRewardBeneficiaries",
                make_optional_beneficiaries(&self.provider_app_reward_beneficiaries),
            ),
        ]))
    }
}

impl Validate for SetProviderAppRewardBeneficiaries {
    fn validate(&self, _ctx: &ValidationCtx) -> Result<(), Error> {
        if let Some(beneficiaries) = &self.provider_app_reward_beneficiaries {
            validate_beneficiary_weights(beneficiaries)?;
        }
        Ok(())
    }
}

/// Toggle result-contract emission on a `RegistrarService`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct SetEnableResultContracts {
    pub registrar_service_cid: String,
    #[serde(default)]
    pub enable_result_contracts: Option<bool>,
}

impl SetEnableResultContracts {
    pub const MODULE: &'static str = "Governance.UtilityOnboarding.SetEnableResultContracts";
    pub const ENTITY: &'static str = "SetEnableResultContracts";
}

impl TemplateInfo for SetEnableResultContracts {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for SetEnableResultContracts {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field(
                "registrarServiceCid",
                make_contract_id(&self.registrar_service_cid),
            ),
            field(
                "enableResultContracts",
                make_optional_bool(&self.enable_result_contracts),
            ),
        ]))
    }
}

impl Validate for SetEnableResultContracts {}

/// Offer a mint of `amount` tokens to `recipient` via
/// `AllocationFactory_OfferMint`. The resulting `MintOffer` is accepted
/// later by the recipient, outside this plugin.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct Mint {
    pub allocation_factory_cid: String,
    pub instrument_id: InstrumentId,
    pub instrument_configuration_cid: String,
    pub recipient: CantonId,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub amount: DamlDecimal,
    pub description: String,
}

impl Mint {
    pub const MODULE: &'static str = "Governance.TokenIssuance.MintProposal";
    pub const ENTITY: &'static str = "MintProposal";
}

impl TemplateInfo for Mint {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for Mint {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field(
                "allocationFactoryCid",
                make_contract_id(&self.allocation_factory_cid),
            ),
            field("instrumentId", serialize_instrument_id(&self.instrument_id)),
            field(
                "instrumentConfigurationCid",
                make_contract_id(&self.instrument_configuration_cid),
            ),
            field("recipient", make_party(&self.recipient)),
            field("amount", make_numeric(&self.amount.to_string())),
            field("description", make_text(&self.description)),
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
        ]))
    }
}

impl Validate for Mint {
    fn validate(&self, _ctx: &ValidationCtx) -> Result<(), Error> {
        validate_positive_amount(&self.amount, "amount")
    }
}

/// Offer a burn of `amount` tokens held by `holder` via
/// `AllocationFactory_OfferBurn`. Holdings are supplied by the holder at
/// `BurnOffer_Accept` time, not here.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct Burn {
    pub allocation_factory_cid: String,
    pub instrument_id: InstrumentId,
    pub instrument_configuration_cid: String,
    pub holder: CantonId,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub amount: DamlDecimal,
    pub description: String,
}

impl Burn {
    pub const MODULE: &'static str = "Governance.TokenIssuance.BurnProposal";
    pub const ENTITY: &'static str = "BurnProposal";
}

impl TemplateInfo for Burn {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for Burn {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field(
                "allocationFactoryCid",
                make_contract_id(&self.allocation_factory_cid),
            ),
            field("instrumentId", serialize_instrument_id(&self.instrument_id)),
            field(
                "instrumentConfigurationCid",
                make_contract_id(&self.instrument_configuration_cid),
            ),
            field("holder", make_party(&self.holder)),
            field("amount", make_numeric(&self.amount.to_string())),
            field("description", make_text(&self.description)),
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
        ]))
    }
}

impl Validate for Burn {
    fn validate(&self, _ctx: &ValidationCtx) -> Result<(), Error> {
        validate_positive_amount(&self.amount, "amount")
    }
}

/// Accept a holder-initiated `MintRequest` via `MintRequest_Accept`. The
/// `MintRequest` must already exist on-ledger (typically created by the
/// holder by exercising `AllocationFactory_RequestMint`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AcceptMintRequest {
    pub mint_request_cid: String,
    pub instrument_configuration_cid: String,
    /// Credential contract ids proving the mint holder meets the
    /// instrument's issuer requirements. Empty for instruments without
    /// issuer requirements.
    #[serde(default)]
    pub issuer_credential_cids: Vec<String>,
    pub description: String,
}

impl AcceptMintRequest {
    pub const MODULE: &'static str = "Governance.TokenIssuance.AcceptMintRequest";
    pub const ENTITY: &'static str = "AcceptMintRequest";
}

impl TemplateInfo for AcceptMintRequest {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for AcceptMintRequest {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field("mintRequestCid", make_contract_id(&self.mint_request_cid)),
            field(
                "instrumentConfigurationCid",
                make_contract_id(&self.instrument_configuration_cid),
            ),
            field("description", make_text(&self.description)),
            field("extraArgsMeta", make_empty_metadata()),
            field(
                "issuerCredentialCids",
                make_optional_list(
                    self.issuer_credential_cids
                        .iter()
                        .map(|cid| make_contract_id(cid))
                        .collect(),
                ),
            ),
        ]))
    }
}

impl Validate for AcceptMintRequest {}

/// Accept a holder-initiated `BurnRequest` via `BurnRequest_Accept`. The
/// `BurnRequest` must already exist on-ledger (typically created by the
/// holder by exercising `AllocationFactory_RequestBurn`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AcceptBurnRequest {
    pub burn_request_cid: String,
    pub instrument_configuration_cid: String,
    /// Credential contract ids proving the burn holder meets the
    /// instrument's issuer requirements. Empty for instruments without
    /// issuer requirements.
    #[serde(default)]
    pub issuer_credential_cids: Vec<String>,
    pub description: String,
}

impl AcceptBurnRequest {
    pub const MODULE: &'static str = "Governance.TokenIssuance.AcceptBurnRequest";
    pub const ENTITY: &'static str = "AcceptBurnRequest";
}

impl TemplateInfo for AcceptBurnRequest {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for AcceptBurnRequest {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field("burnRequestCid", make_contract_id(&self.burn_request_cid)),
            field(
                "instrumentConfigurationCid",
                make_contract_id(&self.instrument_configuration_cid),
            ),
            field("description", make_text(&self.description)),
            field("extraArgsMeta", make_empty_metadata()),
            field(
                "issuerCredentialCids",
                make_optional_list(
                    self.issuer_credential_cids
                        .iter()
                        .map(|cid| make_contract_id(cid))
                        .collect(),
                ),
            ),
        ]))
    }
}

impl Validate for AcceptBurnRequest {}

/// Create the provider decparty's `ProviderConfiguration` with credential
/// requirements for registrars and holders. Executed once by the provider
/// decparty at platform setup.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct CreateProviderConfiguration {
    pub provider_service_cid: String,
    #[serde(default)]
    pub registrar_requirements: Vec<PartyCredentialRequirement>,
    #[serde(default)]
    pub holder_requirements: Vec<PartyCredentialRequirement>,
}

impl CreateProviderConfiguration {
    pub const MODULE: &'static str = "Governance.UtilityOnboarding.CreateProviderConfiguration";
    pub const ENTITY: &'static str = "CreateProviderConfiguration";
}

impl TemplateInfo for CreateProviderConfiguration {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for CreateProviderConfiguration {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field(
                "providerServiceCid",
                make_contract_id(&self.provider_service_cid),
            ),
            field(
                "registrarRequirements",
                serialize_party_credential_requirements(&self.registrar_requirements),
            ),
            field(
                "holderRequirements",
                serialize_party_credential_requirements(&self.holder_requirements),
            ),
        ]))
    }
}

impl Validate for CreateProviderConfiguration {
    fn validate(&self, ctx: &ValidationCtx) -> Result<(), Error> {
        validate_self_issued_requirements_have_claims(
            &self.registrar_requirements,
            ctx.governance_party,
            "registrar_requirements",
        )
    }
}

/// Create a `RegistrarServiceRequest` asking `provider` for registrar
/// service, with the governance party as the registrar. The provider
/// accepts later via `OnboardRegistrar` on its own decparty.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct CreateRegistrarServiceRequest {
    pub operator: CantonId,
    pub provider: CantonId,
    pub create_transfer_rule: bool,
    pub create_allocation_factory: bool,
}

impl CreateRegistrarServiceRequest {
    pub const MODULE: &'static str = "Governance.UtilityOnboarding.CreateRegistrarServiceRequest";
    pub const ENTITY: &'static str = "CreateRegistrarServiceRequest";
}

impl TemplateInfo for CreateRegistrarServiceRequest {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for CreateRegistrarServiceRequest {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field("operator", make_party(&self.operator)),
            field("provider", make_party(&self.provider)),
            field("createTransferRule", make_bool(self.create_transfer_rule)),
            field(
                "createAllocationFactory",
                make_bool(self.create_allocation_factory),
            ),
        ]))
    }
}

impl Validate for CreateRegistrarServiceRequest {}

/// Accept a `RegistrarServiceRequest` on the provider decparty: mint the
/// registrar credentials the governance party can self-issue against the
/// `ProviderConfiguration`'s registrar requirements, then accept the
/// request in the same vote.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct OnboardRegistrar {
    pub provider_service_cid: String,
    pub registrar_service_request_cid: String,
    pub provider_configuration_cid: String,
}

impl OnboardRegistrar {
    pub const MODULE: &'static str = "Governance.UtilityOnboarding.OnboardRegistrar";
    pub const ENTITY: &'static str = "OnboardRegistrar";
}

impl TemplateInfo for OnboardRegistrar {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for OnboardRegistrar {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field(
                "providerServiceCid",
                make_contract_id(&self.provider_service_cid),
            ),
            field(
                "registrarServiceRequestCid",
                make_contract_id(&self.registrar_service_request_cid),
            ),
            field(
                "providerConfigurationCid",
                make_contract_id(&self.provider_configuration_cid),
            ),
        ]))
    }
}

impl Validate for OnboardRegistrar {}

/// Create an `InstrumentConfiguration` on the registrar decparty and
/// credential the initial instrument issuers against its issuer
/// requirements. Executed once per instrument.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ProvisionInstrument {
    pub registrar_service_cid: String,
    pub instrument_id_text: String,
    #[serde(default)]
    pub additional_identifiers: Vec<InstrumentIdentifier>,
    #[serde(default)]
    pub issuer_requirements: Vec<PartyCredentialRequirement>,
    #[serde(default)]
    pub holder_requirements: Vec<PartyCredentialRequirement>,
    #[serde(default)]
    pub initial_instrument_issuers: Vec<CantonId>,
}

impl ProvisionInstrument {
    pub const MODULE: &'static str = "Governance.UtilityOnboarding.ProvisionInstrument";
    pub const ENTITY: &'static str = "ProvisionInstrument";
}

impl TemplateInfo for ProvisionInstrument {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for ProvisionInstrument {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field(
                "registrarServiceCid",
                make_contract_id(&self.registrar_service_cid),
            ),
            field("instrumentIdText", make_text(&self.instrument_id_text)),
            field(
                "additionalIdentifiers",
                make_list(
                    self.additional_identifiers
                        .iter()
                        .map(serialize_instrument_identifier)
                        .collect(),
                ),
            ),
            field(
                "issuerRequirements",
                serialize_party_credential_requirements(&self.issuer_requirements),
            ),
            field(
                "holderRequirements",
                serialize_party_credential_requirements(&self.holder_requirements),
            ),
            field(
                "initialInstrumentIssuers",
                make_list(
                    self.initial_instrument_issuers
                        .iter()
                        .map(make_party)
                        .collect(),
                ),
            ),
        ]))
    }
}

impl Validate for ProvisionInstrument {
    fn validate(&self, ctx: &ValidationCtx) -> Result<(), Error> {
        validate_self_issued_requirements_have_claims(
            &self.issuer_requirements,
            ctx.governance_party,
            "issuer_requirements",
        )?;
        validate_unique_issuers(
            &self.initial_instrument_issuers,
            "initial_instrument_issuers",
        )
    }
}

/// Credential new instrument issuers against an existing
/// `InstrumentConfiguration`'s issuer requirements.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct OnboardInstrumentIssuers {
    pub instrument_configuration_cid: String,
    pub instrument_issuers: Vec<CantonId>,
}

impl OnboardInstrumentIssuers {
    pub const MODULE: &'static str = "Governance.UtilityOnboarding.OnboardInstrumentIssuers";
    pub const ENTITY: &'static str = "OnboardInstrumentIssuers";
}

impl TemplateInfo for OnboardInstrumentIssuers {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for OnboardInstrumentIssuers {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field(
                "instrumentConfigurationCid",
                make_contract_id(&self.instrument_configuration_cid),
            ),
            field(
                "instrumentIssuers",
                make_list(self.instrument_issuers.iter().map(make_party).collect()),
            ),
        ]))
    }
}

impl Validate for OnboardInstrumentIssuers {
    fn validate(&self, _ctx: &ValidationCtx) -> Result<(), Error> {
        // Mirrors the template's `ensure not (null instrumentIssuers)` so the
        // rejection surfaces as a 400 before the ledger sees the proposal.
        if self.instrument_issuers.is_empty() {
            return Err(Error::Validation(
                "instrument_issuers must not be empty".to_string(),
            ));
        }
        validate_unique_issuers(&self.instrument_issuers, "instrument_issuers")
    }
}

/// Revoke the credentials the governance party issued for instrument
/// issuers, removing their issuing privileges. Each row names one issuer
/// and lists that issuer's credentials.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct OffboardInstrumentIssuers {
    pub instrument_issuers: Vec<InstrumentIssuerCredentials>,
}

impl OffboardInstrumentIssuers {
    pub const MODULE: &'static str = "Governance.UtilityOnboarding.OffboardInstrumentIssuers";
    pub const ENTITY: &'static str = "OffboardInstrumentIssuers";
}

impl TemplateInfo for OffboardInstrumentIssuers {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for OffboardInstrumentIssuers {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![field(
            "instrumentIssuers",
            make_list(
                self.instrument_issuers
                    .iter()
                    .map(|row| {
                        make_record(vec![
                            field("instrumentIssuer", make_party(&row.instrument_issuer)),
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
        )]))
    }
}

impl Validate for OffboardInstrumentIssuers {
    fn validate(&self, _ctx: &ValidationCtx) -> Result<(), Error> {
        // Mirrors the template's four ensure guards.
        if self.instrument_issuers.is_empty() {
            return Err(Error::Validation(
                "instrument_issuers must not be empty".to_string(),
            ));
        }
        let mut seen_parties = HashSet::new();
        let mut seen_cids = HashSet::new();
        for row in &self.instrument_issuers {
            if row.credential_cids.is_empty() {
                return Err(Error::Validation(format!(
                    "credential_cids must not be empty for issuer {}",
                    row.instrument_issuer
                )));
            }
            if !seen_parties.insert(&row.instrument_issuer) {
                return Err(Error::Validation(format!(
                    "duplicate instrument issuer not allowed: {}",
                    row.instrument_issuer
                )));
            }
            for cid in &row.credential_cids {
                if !seen_cids.insert(cid) {
                    return Err(Error::Validation(format!(
                        "duplicate credential cid not allowed: {cid}"
                    )));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use common::api::RequiredClaim;

    use super::*;

    /// Any valid `CantonId` — the exact value is irrelevant to these
    /// encode-shape snapshots.
    fn cid(prefix: &str) -> CantonId {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        CantonId::parse(&format!("{prefix}::{ns}")).unwrap()
    }

    fn ctx(governance_party: &CantonId, now_micros: i64) -> ValidationCtx<'_> {
        ValidationCtx {
            governance_party,
            now_micros,
        }
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
    fn encode_snapshots() {
        insta::assert_debug_snapshot!(
            "provision_provider_service",
            ProvisionProviderService {}.to_daml_proto().unwrap()
        );
        insta::assert_debug_snapshot!(
            "create_provider_service_request",
            CreateProviderServiceRequest {
                operator: cid("op"),
                provider: cid("prov"),
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "create_user_service_request",
            CreateUserServiceRequest {
                operator: cid("op"),
                user: cid("user"),
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "create_delegated_batched_markers_proxy",
            CreateDelegatedBatchedMarkersProxy {
                operator: cid("op")
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "setup_utility",
            SetupUtility {
                provider_service_cid: "00psc".to_string(),
                operator: cid("op"),
                instrument_id_text: "uuid-1".to_string(),
                additional_identifiers: vec![InstrumentIdentifier {
                    source: cid("src"),
                    id: "TICK".to_string(),
                    scheme: "Ticker".to_string(),
                }],
                create_transfer_rule: true,
                create_allocation_factory: false,
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "set_provider_app_reward_beneficiaries_some",
            SetProviderAppRewardBeneficiaries {
                instrument_configuration_cid: "00icc".to_string(),
                provider_app_reward_beneficiaries: Some(vec![AppRewardBeneficiary {
                    beneficiary: cid("b1"),
                    weight: "1.0".parse().expect("valid decimal"),
                }]),
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "set_provider_app_reward_beneficiaries_none",
            SetProviderAppRewardBeneficiaries {
                instrument_configuration_cid: "00icc".to_string(),
                provider_app_reward_beneficiaries: None,
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "set_enable_result_contracts_some",
            SetEnableResultContracts {
                registrar_service_cid: "00rsc".to_string(),
                enable_result_contracts: Some(true),
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "set_enable_result_contracts_none",
            SetEnableResultContracts {
                registrar_service_cid: "00rsc".to_string(),
                enable_result_contracts: None,
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "mint",
            Mint {
                allocation_factory_cid: "00alloc".to_string(),
                instrument_id: InstrumentId {
                    admin: "admin::ns".to_string(),
                    id: "instr-1".to_string(),
                },
                instrument_configuration_cid: "00icc".to_string(),
                recipient: cid("recv"),
                amount: "1.5".parse().expect("valid decimal"),
                description: "mint it".to_string(),
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "burn",
            Burn {
                allocation_factory_cid: "00alloc".to_string(),
                instrument_id: InstrumentId {
                    admin: "admin::ns".to_string(),
                    id: "instr-1".to_string(),
                },
                instrument_configuration_cid: "00icc".to_string(),
                holder: cid("holder"),
                amount: "1.5".parse().expect("valid decimal"),
                description: "burn it".to_string(),
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "accept_mint_request_some",
            AcceptMintRequest {
                mint_request_cid: "00mrc".to_string(),
                instrument_configuration_cid: "00icc".to_string(),
                issuer_credential_cids: vec!["00cred1".to_string(), "00cred2".to_string()],
                description: "accept mint".to_string(),
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "accept_mint_request_none",
            AcceptMintRequest {
                mint_request_cid: "00mrc".to_string(),
                instrument_configuration_cid: "00icc".to_string(),
                issuer_credential_cids: vec![],
                description: "accept mint".to_string(),
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "accept_burn_request_some",
            AcceptBurnRequest {
                burn_request_cid: "00brc".to_string(),
                instrument_configuration_cid: "00icc".to_string(),
                issuer_credential_cids: vec!["00cred1".to_string(), "00cred2".to_string()],
                description: "accept burn".to_string(),
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "accept_burn_request_none",
            AcceptBurnRequest {
                burn_request_cid: "00brc".to_string(),
                instrument_configuration_cid: "00icc".to_string(),
                issuer_credential_cids: vec![],
                description: "accept burn".to_string(),
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "create_provider_configuration",
            CreateProviderConfiguration {
                provider_service_cid: "00psc".to_string(),
                registrar_requirements: vec![requirement(
                    &cid("issuer"),
                    &[("role", "registrar"), ("kyc", "passed")],
                )],
                holder_requirements: vec![requirement(&cid("issuer"), &[("role", "holder")])],
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "create_registrar_service_request",
            CreateRegistrarServiceRequest {
                operator: cid("op"),
                provider: cid("prov"),
                create_transfer_rule: true,
                create_allocation_factory: false,
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "onboard_registrar",
            OnboardRegistrar {
                provider_service_cid: "00psc".to_string(),
                registrar_service_request_cid: "00rsrc".to_string(),
                provider_configuration_cid: "00pcc".to_string(),
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "provision_instrument_populated",
            ProvisionInstrument {
                registrar_service_cid: "00rsc".to_string(),
                instrument_id_text: "uuid-1".to_string(),
                additional_identifiers: vec![InstrumentIdentifier {
                    source: cid("src"),
                    id: "TICK".to_string(),
                    scheme: "Ticker".to_string(),
                }],
                issuer_requirements: vec![requirement(
                    &cid("issuer"),
                    &[("role", "instrument-issuer")]
                )],
                holder_requirements: vec![requirement(&cid("issuer"), &[("role", "holder")])],
                initial_instrument_issuers: vec![cid("issuer")],
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "provision_instrument_empty",
            ProvisionInstrument {
                registrar_service_cid: "00rsc".to_string(),
                instrument_id_text: "uuid-1".to_string(),
                additional_identifiers: vec![],
                issuer_requirements: vec![],
                holder_requirements: vec![],
                initial_instrument_issuers: vec![],
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "onboard_instrument_issuers",
            OnboardInstrumentIssuers {
                instrument_configuration_cid: "00icc".to_string(),
                instrument_issuers: vec![cid("iss1"), cid("iss2")],
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "offboard_instrument_issuers",
            OffboardInstrumentIssuers {
                instrument_issuers: vec![InstrumentIssuerCredentials {
                    instrument_issuer: cid("iss1"),
                    credential_cids: vec!["00cred1".to_string(), "00cred2".to_string()],
                }],
            }
            .to_daml_proto()
            .unwrap()
        );
    }

    #[test]
    fn onboard_instrument_issuers_rejects_empty_issuer_list() {
        // Mirrors the template's `ensure not (null instrumentIssuers)` so the
        // rejection surfaces as a 400 before the ledger sees the proposal.
        let gov = cid("gov");
        let mk = |issuers: Vec<CantonId>| OnboardInstrumentIssuers {
            instrument_configuration_cid: "icc".to_string(),
            instrument_issuers: issuers,
        };
        assert!(mk(Vec::new()).validate(&ctx(&gov, 0)).is_err());
        assert!(mk(vec![cid("issuer")]).validate(&ctx(&gov, 0)).is_ok());
    }

    #[test]
    fn onboard_instrument_issuers_rejects_duplicate_issuers() {
        // Mirrors the template's `ensure unique instrumentIssuers`: a
        // duplicated issuer would mint two credentials sharing one id, so
        // the rejection surfaces as a 400 before the ledger sees it.
        let gov = cid("gov");
        let issuer_a = cid("issuer-a");
        let issuer_b = cid("issuer-b");
        let mk = |issuers: Vec<CantonId>| OnboardInstrumentIssuers {
            instrument_configuration_cid: "icc".to_string(),
            instrument_issuers: issuers,
        };
        assert!(
            mk(vec![issuer_a.clone(), issuer_a.clone()])
                .validate(&ctx(&gov, 0))
                .is_err()
        );
        assert!(mk(vec![issuer_a, issuer_b]).validate(&ctx(&gov, 0)).is_ok());
    }

    #[test]
    fn offboard_instrument_issuers_validates_rows() {
        // Mirrors the template's four ensure guards.
        let gov = cid("gov");
        let issuer_a = cid("issuer-a");
        let issuer_b = cid("issuer-b");
        let row = |issuer: CantonId, cids: Vec<&str>| InstrumentIssuerCredentials {
            instrument_issuer: issuer,
            credential_cids: cids.into_iter().map(str::to_string).collect(),
        };
        let mk = |rows: Vec<InstrumentIssuerCredentials>| OffboardInstrumentIssuers {
            instrument_issuers: rows,
        };

        // No rows: revokes nothing.
        assert!(mk(vec![]).validate(&ctx(&gov, 0)).is_err());
        // A row with no cids: revokes nothing.
        assert!(
            mk(vec![row(issuer_a.clone(), vec![])])
                .validate(&ctx(&gov, 0))
                .is_err()
        );
        // The same party in two rows.
        assert!(
            mk(vec![
                row(issuer_a.clone(), vec!["cred-1"]),
                row(issuer_a.clone(), vec!["cred-2"]),
            ])
            .validate(&ctx(&gov, 0))
            .is_err()
        );
        // The same cid in two rows.
        assert!(
            mk(vec![
                row(issuer_a.clone(), vec!["cred-1"]),
                row(issuer_b.clone(), vec!["cred-1"]),
            ])
            .validate(&ctx(&gov, 0))
            .is_err()
        );
        // The same cid twice inside one row.
        assert!(
            mk(vec![row(issuer_a.clone(), vec!["cred-1", "cred-1"])])
                .validate(&ctx(&gov, 0))
                .is_err()
        );
        // Two issuers, distinct cids.
        assert!(
            mk(vec![
                row(issuer_a, vec!["cred-1", "cred-2"]),
                row(issuer_b, vec!["cred-3"]),
            ])
            .validate(&ctx(&gov, 0))
            .is_ok()
        );
    }

    #[test]
    fn provision_instrument_rejects_duplicate_initial_issuers() {
        // Mirrors the template's `ensure unique initialInstrumentIssuers`.
        // An empty list stays legal: issuers can be onboarded later.
        let gov = cid("gov");
        let issuer_a = cid("issuer-a");
        let issuer_b = cid("issuer-b");
        let mk = |issuers: Vec<CantonId>| ProvisionInstrument {
            registrar_service_cid: "rsc".to_string(),
            instrument_id_text: "uuid-1".to_string(),
            additional_identifiers: vec![],
            issuer_requirements: vec![],
            holder_requirements: vec![],
            initial_instrument_issuers: issuers,
        };
        assert!(
            mk(vec![issuer_a.clone(), issuer_a.clone()])
                .validate(&ctx(&gov, 0))
                .is_err()
        );
        assert!(mk(vec![issuer_a, issuer_b]).validate(&ctx(&gov, 0)).is_ok());
        assert!(mk(Vec::new()).validate(&ctx(&gov, 0)).is_ok());
    }

    #[test]
    fn provision_instrument_rejects_claimless_self_issued_requirement() {
        // The same guard on the other template that carries it in Daml.
        let gov = cid("gov");
        let mk = |issuer: CantonId, claims: Vec<RequiredClaim>| ProvisionInstrument {
            registrar_service_cid: "rsc".to_string(),
            instrument_id_text: "uuid-1".to_string(),
            additional_identifiers: vec![],
            issuer_requirements: vec![PartyCredentialRequirement {
                issuer,
                required_claims: claims,
            }],
            holder_requirements: vec![],
            initial_instrument_issuers: vec![],
        };
        let claim = RequiredClaim {
            property: "role".to_string(),
            value: "instrument-issuer".to_string(),
        };
        assert!(mk(gov.clone(), vec![]).validate(&ctx(&gov, 0)).is_err());
        assert!(mk(gov.clone(), vec![claim]).validate(&ctx(&gov, 0)).is_ok());
        assert!(mk(cid("other"), vec![]).validate(&ctx(&gov, 0)).is_ok());
    }

    #[test]
    fn create_provider_configuration_rejects_claimless_self_issued_requirement() {
        // Mirrors the template's `selfIssuedRequirementsHaveClaims`. The
        // frontend prefills a new requirement row as the governance party
        // with no claims, so the default UI path trips this.
        let gov = cid("gov");
        let mk = |issuer: CantonId, claims: Vec<RequiredClaim>| CreateProviderConfiguration {
            provider_service_cid: "psc".to_string(),
            registrar_requirements: vec![PartyCredentialRequirement {
                issuer,
                required_claims: claims,
            }],
            holder_requirements: vec![],
        };
        let claim = RequiredClaim {
            property: "role".to_string(),
            value: "registrar".to_string(),
        };
        // Self-issued and claimless: rejected.
        assert!(mk(gov.clone(), vec![]).validate(&ctx(&gov, 0)).is_err());
        // Self-issued with a claim: accepted.
        assert!(mk(gov.clone(), vec![claim]).validate(&ctx(&gov, 0)).is_ok());
        // Issued by another party and claimless: accepted, matching the Daml.
        assert!(mk(cid("other"), vec![]).validate(&ctx(&gov, 0)).is_ok());
    }

    #[test]
    fn mint_and_burn_reject_non_positive_amount() {
        let gov = cid("gov");
        let mint = |amount: &str| Mint {
            allocation_factory_cid: "afc".to_string(),
            instrument_id: InstrumentId {
                admin: "admin::ns".to_string(),
                id: "instr-1".to_string(),
            },
            instrument_configuration_cid: "icc".to_string(),
            recipient: cid("recv"),
            amount: amount.parse().expect("valid decimal"),
            description: "mint it".to_string(),
        };
        assert!(mint("1.5").validate(&ctx(&gov, 0)).is_ok());
        assert!(mint("0").validate(&ctx(&gov, 0)).is_err());
        assert!(mint("-1").validate(&ctx(&gov, 0)).is_err());

        let burn = |amount: &str| Burn {
            allocation_factory_cid: "afc".to_string(),
            instrument_id: InstrumentId {
                admin: "admin::ns".to_string(),
                id: "instr-1".to_string(),
            },
            instrument_configuration_cid: "icc".to_string(),
            holder: cid("holder"),
            amount: amount.parse().expect("valid decimal"),
            description: "burn it".to_string(),
        };
        assert!(burn("1.5").validate(&ctx(&gov, 0)).is_ok());
        assert!(burn("0").validate(&ctx(&gov, 0)).is_err());
        assert!(burn("-1").validate(&ctx(&gov, 0)).is_err());
    }

    #[test]
    fn set_provider_app_reward_beneficiaries_validates_weights_only_when_some() {
        let gov = cid("gov");
        let mk =
            |beneficiaries: Option<Vec<AppRewardBeneficiary>>| SetProviderAppRewardBeneficiaries {
                instrument_configuration_cid: "icc".to_string(),
                provider_app_reward_beneficiaries: beneficiaries,
            };
        // None is fine — the check only fires when a value is supplied.
        assert!(mk(None).validate(&ctx(&gov, 0)).is_ok());
        assert!(
            mk(Some(vec![AppRewardBeneficiary {
                beneficiary: cid("b1"),
                weight: "1.0".parse().expect("valid decimal"),
            }]))
            .validate(&ctx(&gov, 0))
            .is_ok()
        );
        assert!(
            mk(Some(vec![AppRewardBeneficiary {
                beneficiary: cid("b1"),
                weight: "0.5".parse().expect("valid decimal"),
            }]))
            .validate(&ctx(&gov, 0))
            .is_err()
        );
    }
}
