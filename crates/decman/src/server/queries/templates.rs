//! Template ids the contract queries filter on, and the fetch of one
//! template's active contracts.

use canton_proto_rs::com::daml::ledger::api::v2::{CreatedEvent, Identifier};

use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
    error::Result,
    server::{
        event_filters::{party_event_format, template_filter},
        ledger_paging::fetch_active_contracts_filtered,
    },
};

/// Template identifier for Daml contracts
pub struct TemplateId {
    pub(super) package_id: String,
    pub(super) module_name: &'static str,
    pub(super) entity_name: &'static str,
}

/// Fetch the active contracts of `template` visible to `party_id`, keeping
/// each one `extract` reads. A `None` template means its package is not
/// configured, so the list is empty and the ledger is not asked.
pub(super) async fn fetch_by_template<T>(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: Option<TemplateId>,
    mut extract: impl FnMut(&CreatedEvent) -> Option<T>,
) -> Result<Vec<T>> {
    let Some(template) = template else {
        return Ok(Vec::new());
    };
    let event_format = party_event_format(
        party_id,
        vec![template_filter(
            Identifier {
                package_id: template.package_id.clone(),
                module_name: template.module_name.to_string(),
                entity_name: template.entity_name.to_string(),
            },
            false,
        )],
        true,
    );

    fetch_active_contracts_filtered(config, token, event_format, |created| extract(&created)).await
}

/// Contract template identifiers for the contracts list
/// Each template is queried separately to handle cases where packages may not exist
/// Every template the parties detail view can show.
pub fn contract_templates_all(packages: &PackageConfig) -> Vec<TemplateId> {
    contract_templates(packages)
}

/// The governance-rules templates only.
///
/// The parties list needs exactly one contract per party: the `GovernanceRules`
/// the notifications flow keys off. Reading all nine templates for every hosted
/// party is what OOMKilled nodes (#424), so the list reads this subset and the
/// full set stays behind `include_contracts`.
pub fn rules_templates(packages: &PackageConfig) -> Vec<TemplateId> {
    contract_templates(packages)
        .into_iter()
        .filter(|t| t.entity_name.contains("GovernanceRules"))
        .collect()
}

fn contract_templates(packages: &PackageConfig) -> Vec<TemplateId> {
    let mut templates = vec![
        // CBTC contracts (hardcoded package IDs)
        TemplateId {
            package_id: "#cbtc-governance".to_string(),
            module_name: "CBTC.Governance",
            entity_name: "CBTCGovernanceRules",
        },
        TemplateId {
            package_id: "#cbtc".to_string(),
            module_name: "CBTC.DepositAccount",
            entity_name: "CBTCDepositAccountRules",
        },
        TemplateId {
            package_id: "#cbtc".to_string(),
            module_name: "CBTC.DepositAccount",
            entity_name: "CBTCDepositAccount",
        },
        TemplateId {
            package_id: "#cbtc".to_string(),
            module_name: "CBTC.WithdrawAccount",
            entity_name: "CBTCWithdrawAccountRules",
        },
        TemplateId {
            package_id: "#cbtc".to_string(),
            module_name: "CBTC.WithdrawAccount",
            entity_name: "CBTCWithdrawAccount",
        },
    ];
    // Governance Core contracts (configurable package ID)
    if let Some(ref pkg) = packages.governance_core {
        templates.push(TemplateId {
            package_id: pkg.clone(),
            module_name: "Governance.Rules",
            entity_name: "GovernanceRules",
        });
    }
    // Utility-Registry offer contracts produced by AllocationFactory_OfferMint /
    // AllocationFactory_OfferBurn (used by the utility-onboarding plugin).
    if let Some(ref pkg) = packages.utility_registry {
        templates.push(TemplateId {
            package_id: pkg.clone(),
            module_name: "Utility.Registry.App.V0.Model.Mint",
            entity_name: "MintOffer",
        });
        templates.push(TemplateId {
            package_id: pkg.clone(),
            module_name: "Utility.Registry.App.V0.Model.Burn",
            entity_name: "BurnOffer",
        });
    }
    templates
}

/// ProviderService template identifier
pub(super) fn provider_service_template(packages: &PackageConfig) -> Option<TemplateId> {
    packages.utility_registry.as_ref().map(|pkg| TemplateId {
        package_id: pkg.clone(),
        module_name: "Utility.Registry.App.V0.Service.Provider",
        entity_name: "ProviderService",
    })
}

/// UserService template identifier
pub(super) fn user_service_template(packages: &PackageConfig) -> Option<TemplateId> {
    packages
        .utility_credential_app
        .as_ref()
        .map(|pkg| TemplateId {
            package_id: pkg.clone(),
            module_name: "Utility.Credential.App.V0.Service.User",
            entity_name: "UserService",
        })
}

/// CredentialOffer template identifier
pub(super) fn credential_offer_template(packages: &PackageConfig) -> Option<TemplateId> {
    packages
        .utility_credential_app
        .as_ref()
        .map(|pkg| TemplateId {
            package_id: pkg.clone(),
            module_name: "Utility.Credential.App.V0.Model.Offer",
            entity_name: "CredentialOffer",
        })
}

/// Credential template identifier. Uses the base `utility_credential`
/// package, which defines the `Credential` template; the app package
/// (`utility_credential_app`) only bundles it as a dependency.
pub(super) fn credential_template(packages: &PackageConfig) -> Option<TemplateId> {
    packages.utility_credential.as_ref().map(|pkg| TemplateId {
        package_id: pkg.clone(),
        module_name: "Utility.Credential.V0.Credential",
        entity_name: "Credential",
    })
}

/// RegistrarService template identifier
pub(super) fn registrar_service_template(packages: &PackageConfig) -> Option<TemplateId> {
    packages.utility_registry.as_ref().map(|pkg| TemplateId {
        package_id: pkg.clone(),
        module_name: "Utility.Registry.App.V0.Service.Registrar",
        entity_name: "RegistrarService",
    })
}

/// RegistrarServiceRequest template identifier
pub(super) fn registrar_service_request_template(packages: &PackageConfig) -> Option<TemplateId> {
    packages.utility_registry.as_ref().map(|pkg| TemplateId {
        package_id: pkg.clone(),
        module_name: "Utility.Registry.App.V0.Service.Registrar",
        entity_name: "RegistrarServiceRequest",
    })
}

/// ProviderConfiguration template identifier
pub(super) fn provider_configuration_template(packages: &PackageConfig) -> Option<TemplateId> {
    packages.utility_registry.as_ref().map(|pkg| TemplateId {
        package_id: pkg.clone(),
        module_name: "Utility.Registry.App.V0.Configuration.Provider",
        entity_name: "ProviderConfiguration",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_template_names_the_defining_package() {
        // The `Credential` template is defined in `utility-credential-v0`;
        // `utility-credential-app-v0` only bundles that dalf as a dependency.
        // Canton resolves a `#name` filter against the defining package's
        // name, so naming the app package matches no contracts.
        let template = credential_template(&crate::config::default_package_config())
            .expect("default package config sets utility_credential");
        assert_eq!(template.package_id, "#utility-credential-v0");
        assert_eq!(template.module_name, "Utility.Credential.V0.Credential");
        assert_eq!(template.entity_name, "Credential");
    }

    fn party() -> anyhow::Result<CantonId> {
        CantonId::parse(&format!("party::1220{}", "a".repeat(64)))
    }

    /// A node config whose ledger port has nothing listening on it.
    fn config_with_closed_ledger() -> anyhow::Result<NodeConfig> {
        let port = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        let mut config = NodeConfig::default();
        config.canton.ledger_api_port = port;
        Ok(config)
    }

    /// A list whose package is not configured is empty, and the ledger is
    /// never asked: here it would refuse the connection.
    #[tokio::test]
    async fn an_unconfigured_template_lists_nothing() -> anyhow::Result<()> {
        let config = config_with_closed_ledger()?;
        let packages = PackageConfig {
            utility_credential: None,
            ..crate::config::default_package_config()
        };

        let rows = fetch_by_template(
            &config,
            &party()?,
            None,
            credential_template(&packages),
            |created| Some(created.contract_id.clone()),
        )
        .await?;

        assert!(rows.is_empty());
        Ok(())
    }

    /// A configured template asks the ledger, and a ledger error reaches the
    /// caller.
    #[tokio::test]
    async fn a_configured_template_reports_a_ledger_error() -> anyhow::Result<()> {
        let config = config_with_closed_ledger()?;

        let rows = fetch_by_template(
            &config,
            &party()?,
            None,
            credential_template(&crate::config::default_package_config()),
            |created| Some(created.contract_id.clone()),
        )
        .await;

        assert!(rows.is_err(), "a refused connection must be an error");
        Ok(())
    }
}
