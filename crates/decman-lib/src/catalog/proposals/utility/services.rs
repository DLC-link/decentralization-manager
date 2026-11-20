//! Utility-Registry service requests, the batched-markers proxy and the
//! DevNet featured-app right.

use canton_proto_rs::com::daml::ledger::api::v2::Value;
use common::canton_id::CantonId;

use crate::{
    error::Error,
    framework::{
        DamlProtoEncode, PackageResolver, TemplateId, TemplateInfo, Validate,
        encode::{field, make_contract_id, make_party, make_record},
    },
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
        TemplateId::for_package(pkgs, super::PACKAGE, Self::MODULE, Self::ENTITY)
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
        TemplateId::for_package(pkgs, super::PACKAGE, Self::MODULE, Self::ENTITY)
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
        TemplateId::for_package(pkgs, super::PACKAGE, Self::MODULE, Self::ENTITY)
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
        TemplateId::for_package(pkgs, super::PACKAGE, Self::MODULE, Self::ENTITY)
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
        TemplateId::for_package(pkgs, super::PACKAGE, Self::MODULE, Self::ENTITY)
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
