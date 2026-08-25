//! `governance-utility-onboarding` proposal payloads.

use canton_proto_rs::com::daml::ledger::api::v2::Value;
use common::api::PackageConfig;
use common::canton_id::CantonId;

use crate::error::Error;
use crate::framework::encode::{field, make_party, make_record};
use crate::framework::{DamlProtoEncode, TemplateId, TemplateInfo, Validate};

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
    fn template_id(&self, pkgs: &PackageConfig) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .governance_utility_onboarding
            .as_deref()
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
    fn template_id(&self, pkgs: &PackageConfig) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .governance_utility_onboarding
            .as_deref()
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
    fn template_id(&self, pkgs: &PackageConfig) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .governance_utility_onboarding
            .as_deref()
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
    fn template_id(&self, pkgs: &PackageConfig) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .governance_utility_onboarding
            .as_deref()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Any valid `CantonId` — the exact value is irrelevant to these
    /// encode-shape snapshots.
    fn cid(prefix: &str) -> CantonId {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        CantonId::parse(&format!("{prefix}::{ns}")).unwrap()
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
    }
}
