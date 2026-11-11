//! Utility setup and the registrar settings it leaves behind: reward
//! beneficiaries, result contracts and the transfer rule.

use canton_proto_rs::com::daml::ledger::api::v2::Value;
use common::api::InstrumentIdentifier;
use common::canton_id::CantonId;

use crate::catalog::types::{AppRewardBeneficiary, make_optional_beneficiaries};
use crate::error::Error;
use crate::framework::ValidationCtx;
use crate::framework::encode::serialize_instrument_identifier;
use crate::framework::encode::{field, make_bool, make_contract_id, make_list, make_optional_bool};
use crate::framework::encode::{make_party, make_record, make_text};
use crate::framework::validate::{validate_beneficiary_weights, validate_block_list_supported};
use crate::framework::{DamlProtoEncode, PackageResolver, TemplateId, TemplateInfo, Validate};

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
