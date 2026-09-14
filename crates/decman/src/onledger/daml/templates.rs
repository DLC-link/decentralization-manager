//! Template and choice names of the `decman-coordination-v1` package.
//!
//! The package is node-level: one ref for the whole node, resolved through
//! [`CoordinationPackage`] rather than a per-party `PackageConfig`. Module
//! paths are the full Ledger API `module_name`, so an event's `Identifier`
//! matches on module and entity (Canton echoes package hashes, never refs).

use canton_proto_rs::com::daml::ledger::api::v2::Identifier;
use decman_lib::framework::{PackageResolver, TemplateId, TemplateInfo};

use crate::consts;

/// The resolver key every coordination template asks for.
pub const PACKAGE_KEY: &str = "decman_coordination";

/// The one `#package-name` ref the coordination templates live under.
///
/// A [`PackageResolver`] that answers only [`PACKAGE_KEY`], so a template
/// from another catalog cannot resolve against it by accident.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoordinationPackage(String);

impl CoordinationPackage {
    /// The ref from `consts::coordination_package_ref()` (env-overridable).
    pub fn from_env() -> Self {
        Self(consts::coordination_package_ref())
    }

    /// A specific ref, e.g. in tests.
    pub fn new(package_ref: impl Into<String>) -> Self {
        Self(package_ref.into())
    }

    /// The `#name` ref.
    pub fn reference(&self) -> &str {
        &self.0
    }

    /// The package name without the leading `#`, as `ListPackages` reports it.
    pub fn package_name(&self) -> &str {
        self.0.strip_prefix('#').unwrap_or(&self.0)
    }
}

impl PackageResolver for CoordinationPackage {
    fn package_ref(&self, key: &str) -> Option<&str> {
        (key == PACKAGE_KEY).then_some(self.0.as_str())
    }
}

/// Every template in the coordination package.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CoordinationTemplate {
    DecmanNode,
    WorkflowProposal,
    WorkflowAcceptance,
    WorkflowDecline,
    WorkflowOutcome,
    SubmissionRound,
    SubmissionSignature,
    AcsManifest,
}

impl CoordinationTemplate {
    /// The Daml module the template lives in.
    pub fn module(self) -> &'static str {
        match self {
            Self::DecmanNode => modules::NODE,
            Self::WorkflowProposal
            | Self::WorkflowAcceptance
            | Self::WorkflowDecline
            | Self::WorkflowOutcome => modules::WORKFLOW,
            Self::SubmissionRound | Self::SubmissionSignature => modules::SUBMISSION,
            Self::AcsManifest => modules::ACS,
        }
    }

    /// The template name.
    pub fn entity(self) -> &'static str {
        match self {
            Self::DecmanNode => "DecmanNode",
            Self::WorkflowProposal => "WorkflowProposal",
            Self::WorkflowAcceptance => "WorkflowAcceptance",
            Self::WorkflowDecline => "WorkflowDecline",
            Self::WorkflowOutcome => "WorkflowOutcome",
            Self::SubmissionRound => "SubmissionRound",
            Self::SubmissionSignature => "SubmissionSignature",
            Self::AcsManifest => "AcsManifest",
        }
    }

    /// The `Identifier` a command carries, under the given package ref.
    pub fn identifier(self, package: &CoordinationPackage) -> Identifier {
        Identifier {
            package_id: package.reference().to_string(),
            module_name: self.module().to_string(),
            entity_name: self.entity().to_string(),
        }
    }

    /// Whether an event's `Identifier` names this template. Compares module
    /// and entity only, like `TemplateId::matches`.
    pub fn matches(self, id: &Identifier) -> bool {
        id.module_name == self.module() && id.entity_name == self.entity()
    }
}

impl TemplateInfo for CoordinationTemplate {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, decman_lib::Error> {
        let package_ref = pkgs
            .package_ref(PACKAGE_KEY)
            .ok_or(decman_lib::Error::PackageNotConfigured(PACKAGE_KEY))?;
        Ok(TemplateId::new(package_ref, self.module(), self.entity()))
    }
}

/// Daml module paths (design section 3).
pub mod modules {
    pub const TYPES: &str = "Decman.Coordination.Types";
    pub const NODE: &str = "Decman.Coordination.Node";
    pub const WORKFLOW: &str = "Decman.Coordination.Workflow";
    pub const SUBMISSION: &str = "Decman.Coordination.Submission";
    pub const ACS: &str = "Decman.Coordination.Acs";
}

/// Choice names (design section 3). Kept as constants so a typo fails at the
/// call site instead of as a runtime interpretation error.
pub mod choices {
    pub const DECMAN_NODE_HEARTBEAT: &str = "DecmanNode_Heartbeat";
    pub const DECMAN_NODE_UPDATE: &str = "DecmanNode_Update";
    pub const DECMAN_NODE_RETIRE: &str = "DecmanNode_Retire";

    pub const WORKFLOW_PROPOSAL_ACCEPT: &str = "WorkflowProposal_Accept";
    pub const WORKFLOW_PROPOSAL_DECLINE: &str = "WorkflowProposal_Decline";
    pub const WORKFLOW_PROPOSAL_CANCEL: &str = "WorkflowProposal_Cancel";
    pub const WORKFLOW_PROPOSAL_FINISH: &str = "WorkflowProposal_Finish";
    pub const WORKFLOW_ACCEPTANCE_ARCHIVE: &str = "WorkflowAcceptance_Archive";
    pub const WORKFLOW_DECLINE_ARCHIVE: &str = "WorkflowDecline_Archive";
    pub const WORKFLOW_OUTCOME_ARCHIVE: &str = "WorkflowOutcome_Archive";

    pub const SUBMISSION_ROUND_SIGN: &str = "SubmissionRound_Sign";
    pub const SUBMISSION_ROUND_CLOSE: &str = "SubmissionRound_Close";
    pub const SUBMISSION_SIGNATURE_ARCHIVE: &str = "SubmissionSignature_Archive";

    pub const ACS_MANIFEST_ARCHIVE: &str = "AcsManifest_Archive";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_ids_resolve_through_the_node_level_ref() {
        let package = CoordinationPackage::new("#decman-coordination-v1");
        let id = CoordinationTemplate::WorkflowProposal
            .template_id(&package)
            .expect("resolves");
        assert_eq!(
            id.to_string(),
            "#decman-coordination-v1:Decman.Coordination.Workflow:WorkflowProposal"
        );
    }

    #[test]
    fn the_package_resolver_answers_only_its_own_key() {
        let package = CoordinationPackage::new("#x");
        assert_eq!(
            PackageResolver::package_ref(&package, PACKAGE_KEY),
            Some("#x")
        );
        assert_eq!(
            PackageResolver::package_ref(&package, "governance_core"),
            None
        );
        assert_eq!(package.reference(), "#x");
        assert_eq!(package.package_name(), "x");
    }

    #[test]
    fn a_foreign_resolver_without_the_key_is_not_configured() {
        let err = CoordinationTemplate::DecmanNode
            .template_id(&common::api::PackageConfig::default())
            .expect_err("no coordination key in PackageConfig");
        assert!(matches!(err, decman_lib::Error::PackageNotConfigured(_)));
    }

    #[test]
    fn matches_compares_module_and_entity_only() {
        let echoed = Identifier {
            package_id: "abc123".into(),
            module_name: modules::NODE.into(),
            entity_name: "DecmanNode".into(),
        };
        assert!(CoordinationTemplate::DecmanNode.matches(&echoed));
        assert!(!CoordinationTemplate::WorkflowProposal.matches(&echoed));
    }

    #[test]
    fn every_template_has_a_module_in_the_package() {
        for t in [
            CoordinationTemplate::DecmanNode,
            CoordinationTemplate::WorkflowProposal,
            CoordinationTemplate::WorkflowAcceptance,
            CoordinationTemplate::WorkflowDecline,
            CoordinationTemplate::WorkflowOutcome,
            CoordinationTemplate::SubmissionRound,
            CoordinationTemplate::SubmissionSignature,
            CoordinationTemplate::AcsManifest,
        ] {
            assert!(t.module().starts_with("Decman.Coordination."));
            assert!(!t.entity().is_empty());
        }
    }
}
