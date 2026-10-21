//! `governance-utility-onboarding` proposal payloads.

use std::collections::HashSet;

use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::daml::ledger::api::v2::{RecordField, Value, value};
use common::api::{
    InstrumentId, InstrumentIdentifier, InstrumentIssuerCredentials, PartyCredentialRequirement,
};
use common::canton_id::CantonId;

use crate::catalog::types::{AppRewardBeneficiary, make_optional_beneficiaries};
use crate::error::Error;
use crate::framework::encode::{
    field, make_bool, make_contract_id, make_empty_metadata, make_list, make_numeric,
    make_optional_bool, make_optional_list, make_optional_text, make_optional_timestamp,
    make_party, make_record, make_text, serialize_instrument_id, serialize_instrument_identifier,
    serialize_party_credential_requirements,
};
use crate::framework::validate::{
    validate_beneficiary_weights, validate_block_list_supported, validate_credential_validity,
    validate_positive_amount, validate_self_issued_requirements_have_claims,
    validate_unique_issuers,
};
use crate::framework::{
    DamlProtoEncode, GrpcPayload, PackageResolver, TemplateId, TemplateInfo, Validate,
    ValidationCtx,
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

/// Self-grant a `FeaturedAppRight` to the governance party on DevNet by
/// exercising `AmuletRules_DevNet_FeatureApp`. The choice refuses on any
/// network whose `AmuletRules` is not flagged `isDevNet`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct RequestDevNetFeaturedAppRight {
    /// The DSO's current `AmuletRules` contract, as `GET /network-info`
    /// reports it. The execute path discloses that contract.
    pub amulet_rules_cid: String,
}

impl RequestDevNetFeaturedAppRight {
    pub const MODULE: &'static str = "Governance.UtilityOnboarding.RequestDevNetFeaturedAppRight";
    pub const ENTITY: &'static str = "RequestDevNetFeaturedAppRight";
}

impl TemplateInfo for RequestDevNetFeaturedAppRight {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for RequestDevNetFeaturedAppRight {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![field(
            "amuletRulesCid",
            make_contract_id(&self.amulet_rules_cid),
        )]))
    }
}

impl Validate for RequestDevNetFeaturedAppRight {}

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
    /// `checkBlockList` of the created `InstrumentConfiguration`.
    /// `Some(true)` is refused until every mint, burn and transfer path
    /// sends block list shards (#566).
    #[serde(default)]
    pub use_block_list: Option<bool>,
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
            field("useBlockList", make_optional_bool(&self.use_block_list)),
        ]))
    }
}

impl Validate for SetupUtility {
    fn validate(&self, _ctx: &ValidationCtx) -> Result<(), Error> {
        // Mirrors the template's `ensure`.
        validate_block_list_supported(self.use_block_list)
    }
}

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

/// Create a `TransferRule` on the governance party's `RegistrarService`.
/// Setup creates the first one when asked to. This adds one later.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct CreateTransferRule {
    pub registrar_service_cid: String,
}

impl CreateTransferRule {
    pub const MODULE: &'static str = "Governance.UtilityOnboarding.CreateTransferRule";
    pub const ENTITY: &'static str = "CreateTransferRule";
}

impl TemplateInfo for CreateTransferRule {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for CreateTransferRule {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![field(
            "registrarServiceCid",
            make_contract_id(&self.registrar_service_cid),
        )]))
    }
}

impl Validate for CreateTransferRule {}

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
    /// `StoredBlockListPartyShas` contract ids of the registrar's block list.
    /// The registry reads them when the instrument configuration checks a
    /// block list, and they must cover the mint holder. Empty sends none.
    #[serde(default)]
    pub block_list_shard_cids: Vec<String>,
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
            field(
                "blockListShardCids",
                make_optional_list(
                    self.block_list_shard_cids
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
    /// `StoredBlockListPartyShas` contract ids of the registrar's block list.
    /// The registry reads them when the instrument configuration checks a
    /// block list, and they must cover the burn holder. Empty sends none.
    #[serde(default)]
    pub block_list_shard_cids: Vec<String>,
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
            field(
                "blockListShardCids",
                make_optional_list(
                    self.block_list_shard_cids
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
    /// `validFrom` of every credential this action mints, as microseconds
    /// since epoch. None: valid from creation.
    #[serde(default)]
    pub credential_valid_from_micros: Option<i64>,
    /// `validUntil` of every credential this action mints, as microseconds
    /// since epoch. None: never expires, so ending it takes an
    /// `OffboardInstrumentIssuers` vote.
    #[serde(default)]
    pub credential_valid_until_micros: Option<i64>,
    /// `checkBlockList` of the created `InstrumentConfiguration`.
    /// `Some(true)` is refused until every mint, burn and transfer path
    /// sends block list shards (#566).
    #[serde(default)]
    pub use_block_list: Option<bool>,
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
        )?;
        validate_credential_validity(
            self.credential_valid_from_micros,
            self.credential_valid_until_micros,
            ctx.now_micros,
        )?;
        validate_block_list_supported(self.use_block_list)
    }
}

/// Replace a live `InstrumentConfiguration` on the registrar decparty. The
/// registry's only update path archives the configuration and creates a new
/// one from a full payload, so every field here is required: a field left
/// out would be reset on the ledger. The action sets the provider app reward
/// beneficiaries again after the replace.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS))]
pub struct UpdateInstrumentConfiguration {
    pub registrar_service_cid: String,
    pub instrument_configuration_cid: String,
    /// Must equal the configuration's default identifier id. The action
    /// refuses to change it, because holdings name the instrument by it.
    pub instrument_id_text: String,
    pub additional_identifiers: Vec<InstrumentIdentifier>,
    pub issuer_requirements: Vec<PartyCredentialRequirement>,
    pub holder_requirements: Vec<PartyCredentialRequirement>,
    /// `checkBlockList` of the new configuration. The key is required and
    /// may be `null`, so a request that leaves it out cannot turn a block
    /// list check off by accident. `Some(true)` is refused until every mint,
    /// burn and transfer path sends block list shards (#566).
    #[serde(deserialize_with = "serde::Deserialize::deserialize")]
    pub use_block_list: Option<bool>,
}

impl UpdateInstrumentConfiguration {
    pub const MODULE: &'static str = "Governance.UtilityOnboarding.UpdateInstrumentConfiguration";
    pub const ENTITY: &'static str = "UpdateInstrumentConfiguration";
}

impl TemplateInfo for UpdateInstrumentConfiguration {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_onboarding")
            .ok_or(Error::PackageNotConfigured("governance_utility_onboarding"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for UpdateInstrumentConfiguration {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field(
                "registrarServiceCid",
                make_contract_id(&self.registrar_service_cid),
            ),
            field(
                "instrumentConfigurationCid",
                make_contract_id(&self.instrument_configuration_cid),
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
            field("useBlockList", make_optional_bool(&self.use_block_list)),
        ]))
    }
}

impl Validate for UpdateInstrumentConfiguration {
    fn validate(&self, ctx: &ValidationCtx) -> Result<(), Error> {
        // Mirrors the template's `ensure`.
        validate_self_issued_requirements_have_claims(
            &self.issuer_requirements,
            ctx.governance_party,
            "issuer_requirements",
        )?;
        validate_block_list_supported(self.use_block_list)
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
    /// `validFrom` of every credential this action mints, as microseconds
    /// since epoch. None: valid from creation.
    #[serde(default)]
    pub credential_valid_from_micros: Option<i64>,
    /// `validUntil` of every credential this action mints, as microseconds
    /// since epoch. None: never expires, so ending it takes an
    /// `OffboardInstrumentIssuers` vote.
    #[serde(default)]
    pub credential_valid_until_micros: Option<i64>,
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
    fn validate(&self, ctx: &ValidationCtx) -> Result<(), Error> {
        // Mirrors the template's `ensure not (null instrumentIssuers)` so the
        // rejection surfaces as a 400 before the ledger sees the proposal.
        if self.instrument_issuers.is_empty() {
            return Err(Error::Validation(
                "instrument_issuers must not be empty".to_string(),
            ));
        }
        validate_unique_issuers(&self.instrument_issuers, "instrument_issuers")?;
        validate_credential_validity(
            self.credential_valid_from_micros,
            self.credential_valid_until_micros,
            ctx.now_micros,
        )
    }
}

/// A proposal whose template mints governance credentials through
/// `mintCredentialsMeetingRequirements`. The template ends with
/// `credentialIdNonce : Optional Text`, which the payload's own encoding
/// leaves out. The two issuer templates follow it with
/// `credentialValidFrom : Optional Time` and
/// `credentialValidUntil : Optional Time`. `ProvisionInstrument` then
/// declares `useBlockList : Optional Bool`, which its own encoding also
/// leaves out. Submit it through [`WithCredentialIdNonce`] to set them.
pub trait MintsCredentials: GrpcPayload {
    /// The `(validFrom, validUntil)` window, in microseconds since epoch,
    /// copied onto every credential the action mints. `None` for a template
    /// that declares no window fields, so none are sent.
    fn credential_validity_micros(&self) -> Option<(Option<i64>, Option<i64>)>;

    /// The fields the template declares after the validity window, in
    /// declaration order.
    fn fields_after_validity(&self) -> Vec<RecordField> {
        Vec::new()
    }
}

/// No window: the SDK checks the registrar credential only inside the
/// accept, and the `RegistrarService` keeps working after it expires.
impl MintsCredentials for OnboardRegistrar {
    fn credential_validity_micros(&self) -> Option<(Option<i64>, Option<i64>)> {
        None
    }
}

impl MintsCredentials for ProvisionInstrument {
    fn credential_validity_micros(&self) -> Option<(Option<i64>, Option<i64>)> {
        Some((
            self.credential_valid_from_micros,
            self.credential_valid_until_micros,
        ))
    }

    fn fields_after_validity(&self) -> Vec<RecordField> {
        vec![field(
            "useBlockList",
            make_optional_bool(&self.use_block_list),
        )]
    }
}

impl MintsCredentials for OnboardInstrumentIssuers {
    fn credential_validity_micros(&self) -> Option<(Option<i64>, Option<i64>)> {
        Some((
            self.credential_valid_from_micros,
            self.credential_valid_until_micros,
        ))
    }
}

/// Encode input for a [`MintsCredentials`] proposal. The nonce is runtime
/// data, not an HTTP payload field: the caller draws a fresh one for each
/// proposal. The template folds it into every credential id it mints, so
/// two actions minting for one subject at one ledger time still produce
/// distinct ids. The template's `ensure` clause refuses an empty nonce and
/// one that contains `/`. For the issuer templates the credential validity
/// window follows the nonce, because they declare it after the nonce.
pub struct WithCredentialIdNonce<'a, P: ?Sized> {
    pub proposal: &'a P,
    pub nonce: &'a str,
}

impl<P: MintsCredentials + ?Sized> DamlProtoEncode for WithCredentialIdNonce<'_, P> {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        let Some(value::Sum::Record(mut record)) = self.proposal.to_daml_proto()?.sum else {
            return Err(Error::Encode(
                "credential-minting proposal must encode to a Record".into(),
            ));
        };
        record.fields.push(field(
            "credentialIdNonce",
            make_optional_text(Some(self.nonce)),
        ));
        if let Some((valid_from, valid_until)) = self.proposal.credential_validity_micros() {
            record.fields.push(field(
                "credentialValidFrom",
                make_optional_timestamp(valid_from),
            ));
            record.fields.push(field(
                "credentialValidUntil",
                make_optional_timestamp(valid_until),
            ));
        }
        record.fields.extend(self.proposal.fields_after_validity());
        Ok(Value {
            sum: Some(value::Sum::Record(record)),
        })
    }
}

impl<P: MintsCredentials + ?Sized> TemplateInfo for WithCredentialIdNonce<'_, P> {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        self.proposal.template_id(pkgs)
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
    use crate::framework::commands::proposal_create_arguments;

    /// Any valid `CantonId` — the exact value is irrelevant to these
    /// encode-shape snapshots.
    fn cid(prefix: &str) -> CantonId {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        CantonId::parse(&format!("{prefix}::{ns}")).expect("valid canton id")
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
        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_path(crate::catalog::proposals::SNAPSHOT_PATH);
        let _guard = settings.bind_to_scope();

        insta::assert_debug_snapshot!(
            "provision_provider_service",
            ProvisionProviderService {}
                .to_daml_proto()
                .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "create_provider_service_request",
            CreateProviderServiceRequest {
                operator: cid("op"),
                provider: cid("prov"),
            }
            .to_daml_proto()
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "create_user_service_request",
            CreateUserServiceRequest {
                operator: cid("op"),
                user: cid("user"),
            }
            .to_daml_proto()
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "create_delegated_batched_markers_proxy",
            CreateDelegatedBatchedMarkersProxy {
                operator: cid("op")
            }
            .to_daml_proto()
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "request_dev_net_featured_app_right",
            RequestDevNetFeaturedAppRight {
                amulet_rules_cid: "00amulet".to_string(),
            }
            .to_daml_proto()
            .expect("payload encodes")
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
                use_block_list: Some(true),
            }
            .to_daml_proto()
            .expect("payload encodes")
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
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "set_provider_app_reward_beneficiaries_none",
            SetProviderAppRewardBeneficiaries {
                instrument_configuration_cid: "00icc".to_string(),
                provider_app_reward_beneficiaries: None,
            }
            .to_daml_proto()
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "set_enable_result_contracts_some",
            SetEnableResultContracts {
                registrar_service_cid: "00rsc".to_string(),
                enable_result_contracts: Some(true),
            }
            .to_daml_proto()
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "set_enable_result_contracts_none",
            SetEnableResultContracts {
                registrar_service_cid: "00rsc".to_string(),
                enable_result_contracts: None,
            }
            .to_daml_proto()
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "create_transfer_rule",
            CreateTransferRule {
                registrar_service_cid: "00rsc".to_string(),
            }
            .to_daml_proto()
            .expect("payload encodes")
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
            .expect("payload encodes")
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
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "accept_mint_request_some",
            AcceptMintRequest {
                mint_request_cid: "00mrc".to_string(),
                instrument_configuration_cid: "00icc".to_string(),
                issuer_credential_cids: vec!["00cred1".to_string(), "00cred2".to_string()],
                description: "accept mint".to_string(),
                block_list_shard_cids: vec!["00shard1".to_string(), "00shard2".to_string()],
            }
            .to_daml_proto()
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "accept_mint_request_none",
            AcceptMintRequest {
                mint_request_cid: "00mrc".to_string(),
                instrument_configuration_cid: "00icc".to_string(),
                issuer_credential_cids: vec![],
                description: "accept mint".to_string(),
                block_list_shard_cids: vec![],
            }
            .to_daml_proto()
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "accept_burn_request_some",
            AcceptBurnRequest {
                burn_request_cid: "00brc".to_string(),
                instrument_configuration_cid: "00icc".to_string(),
                issuer_credential_cids: vec!["00cred1".to_string(), "00cred2".to_string()],
                description: "accept burn".to_string(),
                block_list_shard_cids: vec!["00shard1".to_string(), "00shard2".to_string()],
            }
            .to_daml_proto()
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "accept_burn_request_none",
            AcceptBurnRequest {
                burn_request_cid: "00brc".to_string(),
                instrument_configuration_cid: "00icc".to_string(),
                issuer_credential_cids: vec![],
                description: "accept burn".to_string(),
                block_list_shard_cids: vec![],
            }
            .to_daml_proto()
            .expect("payload encodes")
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
            .expect("payload encodes")
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
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "onboard_registrar",
            OnboardRegistrar {
                provider_service_cid: "00psc".to_string(),
                registrar_service_request_cid: "00rsrc".to_string(),
                provider_configuration_cid: "00pcc".to_string(),
            }
            .to_daml_proto()
            .expect("payload encodes")
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
                credential_valid_from_micros: None,
                credential_valid_until_micros: None,
                use_block_list: None,
            }
            .to_daml_proto()
            .expect("payload encodes")
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
                credential_valid_from_micros: None,
                credential_valid_until_micros: None,
                use_block_list: None,
            }
            .to_daml_proto()
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "update_instrument_configuration",
            UpdateInstrumentConfiguration {
                registrar_service_cid: "00rsc".to_string(),
                instrument_configuration_cid: "00icc".to_string(),
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
                use_block_list: Some(true),
            }
            .to_daml_proto()
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "onboard_instrument_issuers",
            OnboardInstrumentIssuers {
                instrument_configuration_cid: "00icc".to_string(),
                instrument_issuers: vec![cid("iss1"), cid("iss2")],
                credential_valid_from_micros: None,
                credential_valid_until_micros: None,
            }
            .to_daml_proto()
            .expect("payload encodes")
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
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "onboard_instrument_issuers_with_credential_id_nonce",
            WithCredentialIdNonce {
                proposal: &OnboardInstrumentIssuers {
                    instrument_configuration_cid: "00icc".to_string(),
                    instrument_issuers: vec![cid("iss1"), cid("iss2")],
                    credential_valid_from_micros: None,
                    credential_valid_until_micros: None,
                },
                nonce: NONCE,
            }
            .to_daml_proto()
            .expect("payload encodes")
        );
    }

    /// A fixed nonce keeps the encodings deterministic. The server draws a
    /// random one per proposal.
    const NONCE: &str = "0123456789ab";

    /// One instance of each `MintsCredentials` proposal.
    fn credential_minting_proposals() -> Vec<Box<dyn MintsCredentials>> {
        vec![
            Box::new(OnboardRegistrar {
                provider_service_cid: "00psc".to_string(),
                registrar_service_request_cid: "00rsrc".to_string(),
                provider_configuration_cid: "00pcc".to_string(),
            }),
            Box::new(ProvisionInstrument {
                registrar_service_cid: "00rsc".to_string(),
                instrument_id_text: "uuid-1".to_string(),
                additional_identifiers: vec![],
                issuer_requirements: vec![],
                holder_requirements: vec![],
                initial_instrument_issuers: vec![cid("issuer")],
                credential_valid_from_micros: Some(1_700_000_000_000_000),
                credential_valid_until_micros: Some(1_800_000_000_000_000),
                use_block_list: Some(true),
            }),
            Box::new(OnboardInstrumentIssuers {
                instrument_configuration_cid: "00icc".to_string(),
                instrument_issuers: vec![cid("issuer")],
                credential_valid_from_micros: None,
                credential_valid_until_micros: None,
            }),
        ]
    }

    /// The nonce closes the create arguments, followed by the validity window
    /// on the templates that declare one, in the order they declare them.
    /// `ProvisionInstrument` then gets `useBlockList`. `OnboardRegistrar`
    /// declares none of these, so it gets the nonce alone. Every other field
    /// is unchanged.
    #[test]
    fn credential_id_nonce_and_validity_are_the_last_create_arguments() {
        let gov = cid("gov");
        let member = cid("member");
        for proposal in credential_minting_proposals() {
            let bare = proposal_create_arguments(proposal.as_ref(), &gov, &member)
                .expect("proposal arguments encode");
            let wrapped = proposal_create_arguments(
                &WithCredentialIdNonce {
                    proposal: proposal.as_ref(),
                    nonce: NONCE,
                },
                &gov,
                &member,
            )
            .expect("proposal arguments encode");
            let window = proposal.credential_validity_micros();
            let after_window = proposal.fields_after_validity();
            let added = if window.is_some() { 3 } else { 1 } + after_window.len();
            let (head, tail) = wrapped.fields.split_at(wrapped.fields.len() - added);
            assert_eq!(head, bare.fields.as_slice(), "other fields unchanged");
            let labels: Vec<&str> = tail.iter().map(|f| f.label.as_str()).collect();
            let mut expected = vec!["credentialIdNonce"];
            if window.is_some() {
                expected.extend(["credentialValidFrom", "credentialValidUntil"]);
            }
            expected.extend(after_window.iter().map(|f| f.label.as_str()));
            assert_eq!(labels, expected);
            assert_eq!(tail[0].value, Some(make_optional_text(Some(NONCE))));
            if let Some((valid_from, valid_until)) = window {
                assert_eq!(tail[1].value, Some(make_optional_timestamp(valid_from)));
                assert_eq!(tail[2].value, Some(make_optional_timestamp(valid_until)));
            }
            assert_eq!(&tail[added - after_window.len()..], after_window.as_slice());
        }
    }

    /// Only `ProvisionInstrument` declares a field after the window, and it
    /// carries the proposal's block list flag.
    #[test]
    fn provision_instrument_sends_use_block_list_after_the_window() {
        let labels: Vec<Vec<String>> = credential_minting_proposals()
            .iter()
            .map(|p| {
                p.fields_after_validity()
                    .into_iter()
                    .map(|f| f.label)
                    .collect()
            })
            .collect();
        assert_eq!(
            labels,
            vec![vec![], vec!["useBlockList".to_string()], vec![]]
        );
        for flag in [None, Some(false), Some(true)] {
            let proposal = ProvisionInstrument {
                registrar_service_cid: "00rsc".to_string(),
                instrument_id_text: "uuid-1".to_string(),
                additional_identifiers: vec![],
                issuer_requirements: vec![],
                holder_requirements: vec![],
                initial_instrument_issuers: vec![],
                credential_valid_from_micros: None,
                credential_valid_until_micros: None,
                use_block_list: flag,
            };
            assert_eq!(
                proposal.fields_after_validity(),
                vec![field("useBlockList", make_optional_bool(&flag))]
            );
        }
    }

    /// Both accept templates declare `blockListShardCids` last, so Smart
    /// Contract Upgrade accepts it. An empty list goes out as `None`, which
    /// leaves the choice context as it was.
    #[test]
    fn accept_requests_send_block_list_shards_last() {
        let gov = cid("gov");
        let member = cid("member");
        let accepts = |shards: Vec<String>| -> Vec<Box<dyn DamlProtoEncode>> {
            vec![
                Box::new(AcceptMintRequest {
                    mint_request_cid: "00mrc".to_string(),
                    instrument_configuration_cid: "00icc".to_string(),
                    issuer_credential_cids: vec![],
                    description: "accept mint".to_string(),
                    block_list_shard_cids: shards.clone(),
                }),
                Box::new(AcceptBurnRequest {
                    burn_request_cid: "00brc".to_string(),
                    instrument_configuration_cid: "00icc".to_string(),
                    issuer_credential_cids: vec![],
                    description: "accept burn".to_string(),
                    block_list_shard_cids: shards,
                }),
            ]
        };
        let shards = vec!["00shard1".to_string(), "00shard2".to_string()];
        for (cids, expected) in [
            (vec![], make_optional_list(vec![])),
            (
                shards.clone(),
                make_optional_list(shards.iter().map(|c| make_contract_id(c)).collect()),
            ),
        ] {
            for accept in accepts(cids) {
                let arguments = proposal_create_arguments(accept.as_ref(), &gov, &member)
                    .expect("proposal arguments encode");
                let last = arguments.fields.last().expect("arguments have fields");
                assert_eq!(last.label, "blockListShardCids");
                assert_eq!(last.value, Some(expected.clone()));
            }
        }
    }

    #[test]
    fn credential_id_nonce_keeps_the_proposal_template() {
        for proposal in credential_minting_proposals() {
            let wrapped = WithCredentialIdNonce {
                proposal: proposal.as_ref(),
                nonce: NONCE,
            };
            assert_eq!(
                wrapped.template_id(&"#pkg").expect("template id resolves"),
                proposal.template_id(&"#pkg").expect("template id resolves"),
            );
        }
    }

    /// Both issuer proposals mirror the templates' window checks: a window
    /// that does not start before it ends is refused, and so is one that has
    /// already ended, which execution would refuse after the vote.
    #[test]
    fn minting_proposals_validate_the_credential_validity_window() {
        let gov = cid("gov");
        let now = 1_750_000_000_000_000;
        let with_window = |from: Option<i64>, until: Option<i64>| -> Vec<Box<dyn Validate>> {
            vec![
                Box::new(ProvisionInstrument {
                    registrar_service_cid: "00rsc".to_string(),
                    instrument_id_text: "uuid-1".to_string(),
                    additional_identifiers: vec![],
                    issuer_requirements: vec![],
                    holder_requirements: vec![],
                    initial_instrument_issuers: vec![cid("issuer")],
                    credential_valid_from_micros: from,
                    credential_valid_until_micros: until,
                    use_block_list: None,
                }),
                Box::new(OnboardInstrumentIssuers {
                    instrument_configuration_cid: "00icc".to_string(),
                    instrument_issuers: vec![cid("issuer")],
                    credential_valid_from_micros: from,
                    credential_valid_until_micros: until,
                }),
            ]
        };
        let accepted = [
            (None, None),
            (Some(now - 1), None),
            (None, Some(now + 1)),
            (Some(now - 1), Some(now + 1)),
            // A window that opens after now is the operator's call.
            (Some(now + 10), Some(now + 20)),
        ];
        let refused = [
            // Already ended: execution would mint expired credentials.
            (None, Some(now)),
            (Some(now - 20), Some(now - 10)),
            // Empty or inverted window: the ensure clause refuses it.
            (Some(now + 10), Some(now + 10)),
            (Some(now + 20), Some(now + 10)),
            // Not a timestamp after the epoch.
            (Some(0), None),
        ];
        for (from, until) in accepted {
            for proposal in with_window(from, until) {
                assert!(
                    proposal.validate(&ctx(&gov, now)).is_ok(),
                    "{from:?}..{until:?} should be accepted"
                );
            }
        }
        for (from, until) in refused {
            for proposal in with_window(from, until) {
                assert!(
                    proposal.validate(&ctx(&gov, now)).is_err(),
                    "{from:?}..{until:?} should be refused"
                );
            }
        }
    }

    #[test]
    fn onboard_instrument_issuers_rejects_empty_issuer_list() {
        // Mirrors the template's `ensure not (null instrumentIssuers)` so the
        // rejection surfaces as a 400 before the ledger sees the proposal.
        let gov = cid("gov");
        let mk = |issuers: Vec<CantonId>| OnboardInstrumentIssuers {
            instrument_configuration_cid: "icc".to_string(),
            instrument_issuers: issuers,
            credential_valid_from_micros: None,
            credential_valid_until_micros: None,
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
            credential_valid_from_micros: None,
            credential_valid_until_micros: None,
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
            credential_valid_from_micros: None,
            credential_valid_until_micros: None,
            use_block_list: None,
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
            credential_valid_from_micros: None,
            credential_valid_until_micros: None,
            use_block_list: None,
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
    fn update_instrument_configuration_rejects_claimless_self_issued_requirement() {
        // Mirrors the template's `ensure`, as on ProvisionInstrument.
        let gov = cid("gov");
        let mk = |issuer: CantonId, claims: Vec<RequiredClaim>| UpdateInstrumentConfiguration {
            registrar_service_cid: "rsc".to_string(),
            instrument_configuration_cid: "icc".to_string(),
            instrument_id_text: "uuid-1".to_string(),
            additional_identifiers: vec![],
            issuer_requirements: vec![PartyCredentialRequirement {
                issuer,
                required_claims: claims,
            }],
            holder_requirements: vec![],
            use_block_list: None,
        };
        let claim = RequiredClaim {
            property: "role".to_string(),
            value: "instrument-issuer".to_string(),
        };
        assert!(mk(gov.clone(), vec![]).validate(&ctx(&gov, 0)).is_err());
        assert!(mk(gov.clone(), vec![claim]).validate(&ctx(&gov, 0)).is_ok());
        assert!(mk(cid("other"), vec![]).validate(&ctx(&gov, 0)).is_ok());
    }

    /// Mirrors the templates' `blockListSupported` guard: not every mint,
    /// burn and transfer path sends block list shards yet, so `Some(true)` is
    /// refused with a 400 that names #566.
    #[test]
    fn configuration_proposals_refuse_use_block_list_true() {
        let gov = cid("gov");
        let with_flag = |use_block_list: Option<bool>| -> Vec<(&str, Box<dyn Validate>)> {
            vec![
                (
                    "setup_utility",
                    Box::new(SetupUtility {
                        provider_service_cid: "psc".to_string(),
                        operator: cid("op"),
                        instrument_id_text: "uuid-1".to_string(),
                        additional_identifiers: vec![],
                        create_transfer_rule: true,
                        create_allocation_factory: true,
                        use_block_list,
                    }),
                ),
                (
                    "provision_instrument",
                    Box::new(ProvisionInstrument {
                        registrar_service_cid: "rsc".to_string(),
                        instrument_id_text: "uuid-1".to_string(),
                        additional_identifiers: vec![],
                        issuer_requirements: vec![],
                        holder_requirements: vec![],
                        initial_instrument_issuers: vec![],
                        credential_valid_from_micros: None,
                        credential_valid_until_micros: None,
                        use_block_list,
                    }),
                ),
                (
                    "update_instrument_configuration",
                    Box::new(UpdateInstrumentConfiguration {
                        registrar_service_cid: "rsc".to_string(),
                        instrument_configuration_cid: "icc".to_string(),
                        instrument_id_text: "uuid-1".to_string(),
                        additional_identifiers: vec![],
                        issuer_requirements: vec![],
                        holder_requirements: vec![],
                        use_block_list,
                    }),
                ),
            ]
        };
        for flag in [None, Some(false)] {
            for (name, proposal) in with_flag(flag) {
                assert!(
                    proposal.validate(&ctx(&gov, 0)).is_ok(),
                    "{name} should accept use_block_list {flag:?}"
                );
            }
        }
        for (name, proposal) in with_flag(Some(true)) {
            match proposal.validate(&ctx(&gov, 0)) {
                Err(Error::Validation(message)) => assert!(
                    message.contains("use_block_list") && message.contains("#566"),
                    "{name}: unexpected message {message:?}"
                ),
                other => panic!("{name} should refuse use_block_list true, got {other:?}"),
            }
        }
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
