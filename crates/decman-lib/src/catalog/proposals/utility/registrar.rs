//! Provider configuration and registrar onboarding.

use canton_proto_rs::com::daml::ledger::api::v2::Value;
use common::api::PartyCredentialRequirement;
use common::canton_id::CantonId;

use crate::error::Error;
use crate::framework::ValidationCtx;
use crate::framework::encode::serialize_party_credential_requirements;
use crate::framework::encode::{field, make_bool, make_contract_id, make_party, make_record};
use crate::framework::validate::validate_self_issued_requirements_have_claims;
use crate::framework::{DamlProtoEncode, PackageResolver, TemplateId, TemplateInfo, Validate};

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
