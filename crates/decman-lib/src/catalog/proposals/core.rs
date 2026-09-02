//! `governance-core` proposal payloads.

use canton_proto_rs::com::daml::ledger::api::v2::Value;

use crate::error::Error;
use crate::framework::encode::{field, make_record, make_text};
use crate::framework::{DamlProtoEncode, PackageResolver, TemplateId, TemplateInfo, Validate};

/// Generic text-based vote (no on-chain effect beyond recording the result).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct GenericVote {
    pub description: String,
}

impl GenericVote {
    pub const MODULE: &'static str = "Governance.GenericVote";
    pub const ENTITY: &'static str = "GenericVoteProposal";
}

impl TemplateInfo for GenericVote {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_core")
            .ok_or(Error::PackageNotConfigured("governance_core"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for GenericVote {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![field(
            "description",
            make_text(&self.description),
        )]))
    }
}

impl Validate for GenericVote {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_snapshots() {
        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_path(crate::catalog::proposals::SNAPSHOT_PATH);
        let _guard = settings.bind_to_scope();

        insta::assert_debug_snapshot!(
            "generic_vote",
            GenericVote {
                description: "a vote".into(),
            }
            .to_daml_proto()
            .unwrap()
        );
    }
}
