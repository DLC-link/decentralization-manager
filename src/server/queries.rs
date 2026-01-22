use std::collections::HashMap;

use canton_proto_rs::com::daml::ledger::api::v2::{
    CreatedEvent, CumulativeFilter, EventFormat, Filters, GetActiveContractsRequest,
    GetLedgerEndRequest, Identifier, TemplateFilter, WildcardFilter,
    admin::ListKnownPartiesRequest, cumulative_filter,
    get_active_contracts_response::ContractEntry, value,
};

use crate::{config::NodeConfig, error::Result, utils};

use super::types::{ContractInfo, GovernanceAction, GovernanceConfirmation, PartyMetadata};

/// Template identifier for DAML contracts
struct TemplateId {
    package_id: &'static str,
    module_name: &'static str,
    entity_name: &'static str,
}

/// Contract template identifiers for the contracts list
/// Each template is queried separately to handle cases where packages may not exist
const CONTRACT_TEMPLATES: &[TemplateId] = &[
    // CBTC contracts
    TemplateId {
        package_id: "#cbtc-governance",
        module_name: "CBTC.Governance",
        entity_name: "CBTCGovernanceRules",
    },
    TemplateId {
        package_id: "#cbtc",
        module_name: "CBTC.DepositAccount",
        entity_name: "CBTCDepositAccountRules",
    },
    TemplateId {
        package_id: "#cbtc",
        module_name: "CBTC.DepositAccount",
        entity_name: "CBTCDepositAccount",
    },
    TemplateId {
        package_id: "#cbtc",
        module_name: "CBTC.WithdrawAccount",
        entity_name: "CBTCWithdrawAccountRules",
    },
    TemplateId {
        package_id: "#cbtc",
        module_name: "CBTC.WithdrawAccount",
        entity_name: "CBTCWithdrawAccount",
    },
    // Vault contracts
    TemplateId {
        package_id: "#bitsafe-vault-governance-v0-rc2",
        module_name: "BitsafeVault.VaultGovernance",
        entity_name: "VaultGovernanceRules",
    },
];

/// Governance confirmation template identifiers
/// Each template is queried separately to handle cases where packages may not exist
const GOVERNANCE_TEMPLATES: &[TemplateId] = &[
    TemplateId {
        package_id: "#bitsafe-vault-governance-v0",
        module_name: "BitsafeVault.VaultGovernance",
        entity_name: "VaultGovernanceConfirmation",
    },
    TemplateId {
        package_id: "#cbtc-governance",
        module_name: "CBTC.Governance",
        entity_name: "Confirmation",
    },
];

/// Check if a template matches any contract template we want to display
fn is_contract_template(module_name: &str, entity_name: &str) -> bool {
    CONTRACT_TEMPLATES
        .iter()
        .any(|t| t.module_name == module_name && t.entity_name == entity_name)
}

/// Get active contracts for a party
///
/// When `test_mode` is true, uses WildcardFilter with in-memory filtering
/// (mock auth doesn't have TemplateFilter permissions).
///
/// In production mode, queries each template separately to gracefully handle
/// cases where some packages may not be deployed on the participant.
pub async fn get_contracts(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
    test_mode: bool,
) -> Result<Vec<ContractInfo>> {
    let mut contracts = Vec::new();

    if test_mode {
        // Test mode: use WildcardFilter with in-memory filtering
        tracing::debug!("Using WildcardFilter for contracts query (test mode)");
        fetch_contracts_with_wildcard(config, party_id, token, &mut contracts).await?;
    } else {
        // Production mode: query each template separately to handle missing packages
        tracing::debug!("Using TemplateFilter for contracts query (per-template)");
        for t in CONTRACT_TEMPLATES {
            match fetch_contracts_for_template(
                config,
                party_id,
                token.clone(),
                t.package_id,
                t.module_name,
                t.entity_name,
                &mut contracts,
            )
            .await
            {
                Ok(()) => {
                    tracing::debug!("Successfully queried {}:{}", t.module_name, t.entity_name);
                }
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("PACKAGE_NAMES_NOT_FOUND") {
                        tracing::debug!(
                            "Package {} not found, skipping {}:{}",
                            t.package_id,
                            t.module_name,
                            t.entity_name
                        );
                    } else {
                        tracing::warn!(
                            "Failed to query {}:{}: {e}, continuing...",
                            t.module_name,
                            t.entity_name
                        );
                    }
                }
            }
        }
    }

    Ok(contracts)
}

/// Fetch contracts using WildcardFilter (for test mode)
async fn fetch_contracts_with_wildcard(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
    contracts: &mut Vec<ContractInfo>,
) -> Result<()> {
    let mut state_client = utils::create_state_client(config, token).await?;

    let ledger_end = state_client
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {}))
        .await?
        .into_inner()
        .offset;

    let mut filters_by_party = HashMap::new();
    filters_by_party.insert(
        party_id.to_string(),
        Filters {
            cumulative: vec![CumulativeFilter {
                identifier_filter: Some(cumulative_filter::IdentifierFilter::WildcardFilter(
                    WildcardFilter {
                        include_created_event_blob: false,
                    },
                )),
            }],
        },
    );

    let acs_request = GetActiveContractsRequest {
        active_at_offset: ledger_end,
        event_format: Some(EventFormat {
            filters_by_party,
            filters_for_any_party: None,
            verbose: false,
        }),
    };

    let mut stream = state_client
        .get_active_contracts(tonic::Request::new(acs_request))
        .await?
        .into_inner();

    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
        {
            // Filter in-memory for contract templates
            let template = created.template_id.as_ref();
            let is_wanted = template
                .map(|t| is_contract_template(&t.module_name, &t.entity_name))
                .unwrap_or(false);

            if !is_wanted {
                continue;
            }

            let template_id = template
                .map(|t| format!("{}:{}", t.module_name, t.entity_name))
                .unwrap_or_default();
            let package_id = template.map(|t| t.package_id.clone()).unwrap_or_default();

            contracts.push(ContractInfo {
                contract_id: created.contract_id,
                template_id,
                package_id,
            });
        }
    }

    Ok(())
}

/// Fetch contracts for a specific template
async fn fetch_contracts_for_template(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
    package_id: &str,
    module_name: &str,
    entity_name: &str,
    contracts: &mut Vec<ContractInfo>,
) -> Result<()> {
    let mut state_client = utils::create_state_client(config, token).await?;

    let ledger_end = state_client
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {}))
        .await?
        .into_inner()
        .offset;

    let mut filters_by_party = HashMap::new();
    filters_by_party.insert(
        party_id.to_string(),
        Filters {
            cumulative: vec![CumulativeFilter {
                identifier_filter: Some(cumulative_filter::IdentifierFilter::TemplateFilter(
                    TemplateFilter {
                        template_id: Some(Identifier {
                            package_id: package_id.to_string(),
                            module_name: module_name.to_string(),
                            entity_name: entity_name.to_string(),
                        }),
                        include_created_event_blob: false,
                    },
                )),
            }],
        },
    );

    let acs_request = GetActiveContractsRequest {
        active_at_offset: ledger_end,
        event_format: Some(EventFormat {
            filters_by_party,
            filters_for_any_party: None,
            verbose: false,
        }),
    };

    let mut stream = state_client
        .get_active_contracts(tonic::Request::new(acs_request))
        .await?
        .into_inner();

    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
        {
            let template_id = format!("{module_name}:{entity_name}");

            contracts.push(ContractInfo {
                contract_id: created.contract_id,
                template_id,
                package_id: package_id.to_string(),
            });
        }
    }

    Ok(())
}

/// Get party metadata from Ledger API
pub async fn get_party_metadata(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
) -> Result<Option<PartyMetadata>> {
    let mut client = utils::create_party_client(config, token).await?;

    let response = client
        .list_known_parties(tonic::Request::new(ListKnownPartiesRequest {
            identity_provider_id: String::new(),
            page_token: String::new(),
            page_size: 1000,
        }))
        .await?
        .into_inner();

    let party_details = response.party_details.iter().find(|p| p.party == party_id);

    let annotations = party_details
        .and_then(|d| d.local_metadata.as_ref())
        .map(|m| m.annotations.clone())
        .unwrap_or_default();

    if annotations.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PartyMetadata { annotations }))
    }
}

/// Check if a template matches any governance confirmation template
fn is_governance_template(module_name: &str, entity_name: &str) -> bool {
    GOVERNANCE_TEMPLATES
        .iter()
        .any(|t| t.module_name == module_name && t.entity_name == entity_name)
}

/// Get governance confirmations for a decentralized party
///
/// Fetches active contracts filtered by governance confirmation templates,
/// then extracts action field and groups by action.
///
/// When `test_mode` is true, uses WildcardFilter with in-memory filtering
/// (mock auth doesn't have TemplateFilter permissions).
///
/// In production mode, queries each template separately to gracefully handle
/// cases where some packages may not be deployed on the participant.
pub async fn get_governance_confirmations(
    config: &NodeConfig,
    party_id: &str,
    threshold: usize,
    token: Option<String>,
    test_mode: bool,
) -> Result<Vec<GovernanceAction>> {
    // Collect confirmations grouped by action
    let mut confirmations_by_action: HashMap<String, Vec<GovernanceConfirmation>> = HashMap::new();

    if test_mode {
        // Test mode: use WildcardFilter with in-memory filtering
        tracing::debug!("Using WildcardFilter for governance query (test mode)");
        fetch_governance_with_wildcard(config, party_id, token, &mut confirmations_by_action)
            .await?;
    } else {
        // Production mode: query each template separately to handle missing packages
        tracing::debug!("Using TemplateFilter for governance query (per-template)");
        for t in GOVERNANCE_TEMPLATES {
            match fetch_governance_for_template(
                config,
                party_id,
                token.clone(),
                t.package_id,
                t.module_name,
                t.entity_name,
                &mut confirmations_by_action,
            )
            .await
            {
                Ok(()) => {
                    tracing::debug!("Successfully queried {}:{}", t.module_name, t.entity_name);
                }
                Err(e) => {
                    // Log but continue - package might not exist on this participant
                    let err_str = e.to_string();
                    if err_str.contains("PACKAGE_NAMES_NOT_FOUND") {
                        tracing::debug!(
                            "Package {} not found, skipping {}:{}",
                            t.package_id,
                            t.module_name,
                            t.entity_name
                        );
                    } else {
                        tracing::warn!(
                            "Failed to query {}:{}: {e}, continuing...",
                            t.module_name,
                            t.entity_name
                        );
                    }
                }
            }
        }
    }

    // Convert to GovernanceAction list
    let actions: Vec<GovernanceAction> = confirmations_by_action
        .into_iter()
        .map(|(action_id, confirmations)| {
            let confirmation_count = confirmations.len();
            GovernanceAction {
                action_id,
                confirmations,
                confirmation_count,
                can_execute: confirmation_count >= threshold,
            }
        })
        .collect();

    Ok(actions)
}

/// Fetch governance confirmations using WildcardFilter (for test mode)
async fn fetch_governance_with_wildcard(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
    confirmations_by_action: &mut HashMap<String, Vec<GovernanceConfirmation>>,
) -> Result<()> {
    let mut state_client = utils::create_state_client(config, token).await?;

    let ledger_end = state_client
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {}))
        .await?
        .into_inner()
        .offset;

    let mut filters_by_party = HashMap::new();
    filters_by_party.insert(
        party_id.to_string(),
        Filters {
            cumulative: vec![CumulativeFilter {
                identifier_filter: Some(cumulative_filter::IdentifierFilter::WildcardFilter(
                    WildcardFilter {
                        include_created_event_blob: false,
                    },
                )),
            }],
        },
    );

    let acs_request = GetActiveContractsRequest {
        active_at_offset: ledger_end,
        event_format: Some(EventFormat {
            filters_by_party,
            filters_for_any_party: None,
            verbose: true,
        }),
    };

    let mut stream = state_client
        .get_active_contracts(tonic::Request::new(acs_request))
        .await?
        .into_inner();

    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
        {
            // Filter in-memory for governance templates
            let template = created.template_id.as_ref();
            let is_governance = template
                .map(|t| is_governance_template(&t.module_name, &t.entity_name))
                .unwrap_or(false);

            if !is_governance {
                continue;
            }

            extract_and_add_confirmation(&created, confirmations_by_action);
        }
    }

    Ok(())
}

/// Fetch governance confirmations for a specific template
async fn fetch_governance_for_template(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
    package_id: &str,
    module_name: &str,
    entity_name: &str,
    confirmations_by_action: &mut HashMap<String, Vec<GovernanceConfirmation>>,
) -> Result<()> {
    let mut state_client = utils::create_state_client(config, token).await?;

    let ledger_end = state_client
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {}))
        .await?
        .into_inner()
        .offset;

    let mut filters_by_party = HashMap::new();
    filters_by_party.insert(
        party_id.to_string(),
        Filters {
            cumulative: vec![CumulativeFilter {
                identifier_filter: Some(cumulative_filter::IdentifierFilter::TemplateFilter(
                    TemplateFilter {
                        template_id: Some(Identifier {
                            package_id: package_id.to_string(),
                            module_name: module_name.to_string(),
                            entity_name: entity_name.to_string(),
                        }),
                        include_created_event_blob: false,
                    },
                )),
            }],
        },
    );

    let acs_request = GetActiveContractsRequest {
        active_at_offset: ledger_end,
        event_format: Some(EventFormat {
            filters_by_party,
            filters_for_any_party: None,
            verbose: true,
        }),
    };

    let mut stream = state_client
        .get_active_contracts(tonic::Request::new(acs_request))
        .await?
        .into_inner();

    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
        {
            extract_and_add_confirmation(&created, confirmations_by_action);
        }
    }

    Ok(())
}

/// Extract action and confirming_party from a created event and add to the map
fn extract_and_add_confirmation(
    created: &CreatedEvent,
    confirmations_by_action: &mut HashMap<String, Vec<GovernanceConfirmation>>,
) {
    // Extract action field from create_arguments
    let action = created
        .create_arguments
        .as_ref()
        .and_then(|record| {
            record.fields.iter().find_map(|field| {
                if field.label == "action" {
                    field.value.as_ref().and_then(|v| match &v.sum {
                        Some(value::Sum::Text(s)) => Some(s.clone()),
                        _ => None,
                    })
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| "unknown".to_string());

    // Extract confirming party from create_arguments
    let confirming_party = created
        .create_arguments
        .as_ref()
        .and_then(|record| {
            record.fields.iter().find_map(|field| {
                if field.label == "confirmingParty" || field.label == "confirmer" {
                    field.value.as_ref().and_then(|v| match &v.sum {
                        Some(value::Sum::Party(p)) => Some(p.clone()),
                        _ => None,
                    })
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| "unknown".to_string());

    let confirmation = GovernanceConfirmation {
        contract_id: created.contract_id.clone(),
        action: action.clone(),
        confirming_party,
    };

    confirmations_by_action
        .entry(action)
        .or_default()
        .push(confirmation);
}
