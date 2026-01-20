use std::collections::HashMap;

use canton_proto_rs::com::daml::ledger::api::v2::{
    CumulativeFilter, EventFormat, Filters, GetActiveContractsRequest, GetLedgerEndRequest,
    WildcardFilter, cumulative_filter, get_active_contracts_response::ContractEntry, value,
};

use crate::{config::NodeConfig, error::Result, utils};

use super::types::{ContractInfo, GovernanceAction, GovernanceConfirmation, PartyMetadata};

/// Governance confirmation template identifiers (module_name, entity_name)
/// Package IDs are not specified to support different deployments
const GOVERNANCE_TEMPLATES: &[(&str, &str)] = &[
    ("BitsafeVault.VaultGovernance", "VaultGovernanceConfirmation"),
    ("CBTC.Governance", "Confirmation"),
];

/// Get active contracts for a party
pub async fn get_contracts(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
) -> Result<Vec<ContractInfo>> {
    let mut state_client = utils::create_state_client(config, token).await?;

    // Get current ledger end
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

    let mut contracts = Vec::new();
    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
        {
            let template_id = created
                .template_id
                .as_ref()
                .map(|t| {
                    format!(
                        "{module}:{entity}",
                        module = t.module_name,
                        entity = t.entity_name
                    )
                })
                .unwrap_or_default();
            let package_id = created
                .template_id
                .as_ref()
                .map(|t| t.package_id.clone())
                .unwrap_or_default();

            contracts.push(ContractInfo {
                contract_id: created.contract_id,
                template_id,
                package_id,
            });
        }
    }

    Ok(contracts)
}

/// Get party metadata from Ledger API
pub async fn get_party_metadata(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
) -> Result<Option<PartyMetadata>> {
    use canton_proto_rs::com::daml::ledger::api::v2::admin::ListKnownPartiesRequest;

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
        .any(|(m, e)| *m == module_name && *e == entity_name)
}

/// Get governance confirmations for a decentralized party
///
/// Fetches all active contracts and filters for governance confirmation templates,
/// then extracts action field and groups by action.
pub async fn get_governance_confirmations(
    config: &NodeConfig,
    party_id: &str,
    threshold: usize,
    token: Option<String>,
) -> Result<Vec<GovernanceAction>> {
    let mut state_client = utils::create_state_client(config, token).await?;

    // Get current ledger end
    let ledger_end = state_client
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {}))
        .await?
        .into_inner()
        .offset;

    // Use wildcard filter to get all contracts, then filter in code
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
            verbose: true, // Include field labels for easier parsing
        }),
    };

    let mut stream = state_client
        .get_active_contracts(tonic::Request::new(acs_request))
        .await?
        .into_inner();

    // Collect confirmations grouped by action
    let mut confirmations_by_action: HashMap<String, Vec<GovernanceConfirmation>> = HashMap::new();

    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
        {
            // Check if this is a governance confirmation template
            let template = created.template_id.as_ref();
            let is_governance = template
                .map(|t| is_governance_template(&t.module_name, &t.entity_name))
                .unwrap_or(false);

            if !is_governance {
                continue;
            }

            // Debug: log available fields (temporary info level for debugging)
            if let Some(record) = created.create_arguments.as_ref() {
                let field_names: Vec<&str> = record.fields.iter().map(|f| f.label.as_str()).collect();
                tracing::info!("Governance contract fields: {field_names:?}");
            }

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
