use std::fmt;

use canton_proto_rs::com::daml::ledger::api::v2::Identifier;

/// A Daml template or interface id under a `#package-name` reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TemplateId {
    pub package_ref: String,
    pub module: String,
    pub entity: String,
}

impl TemplateId {
    pub fn new(
        package_ref: impl Into<String>,
        module: impl Into<String>,
        entity: impl Into<String>,
    ) -> Self {
        Self {
            package_ref: package_ref.into(),
            module: module.into(),
            entity: entity.into(),
        }
    }

    /// Whether an event's `Identifier` names this template. Compares module
    /// and entity only: Canton echoes resolved package hashes back in
    /// events, so package refs never compare directly.
    pub fn matches(&self, id: &Identifier) -> bool {
        id.module_name == self.module && id.entity_name == self.entity
    }
}

impl From<&TemplateId> for Identifier {
    fn from(t: &TemplateId) -> Self {
        Identifier {
            package_id: t.package_ref.clone(),
            module_name: t.module.clone(),
            entity_name: t.entity.clone(),
        }
    }
}

impl fmt::Display for TemplateId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.package_ref, self.module, self.entity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_renders_ref_module_entity() {
        let t = TemplateId::new("#governance-core-v1", "Governance.Rules", "GovernanceRules");
        assert_eq!(
            t.to_string(),
            "#governance-core-v1:Governance.Rules:GovernanceRules"
        );
    }

    #[test]
    fn matches_ignores_the_package() {
        // Canton echoes resolved package hashes back in events, so matching
        // compares module and entity only.
        let t = TemplateId::new("#governance-core-v1", "Governance.Rules", "GovernanceRules");
        let echoed = Identifier {
            package_id: "abc123hash".into(),
            module_name: "Governance.Rules".into(),
            entity_name: "GovernanceRules".into(),
        };
        assert!(t.matches(&echoed));
        let other = Identifier {
            entity_name: "GovernanceConfirmation".into(),
            ..echoed.clone()
        };
        assert!(!t.matches(&other));
    }

    #[test]
    fn converts_to_identifier() {
        let t = TemplateId::new("#p", "M", "E");
        let id: Identifier = (&t).into();
        assert_eq!(
            (id.package_id, id.module_name, id.entity_name),
            ("#p".into(), "M".into(), "E".into())
        );
    }
}
