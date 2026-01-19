use std::collections::HashMap;

use canton_proto_rs::com::daml::ledger::api::v2::{
    CumulativeFilter, EventFormat, Filters, GetActiveContractsRequest, GetLedgerEndRequest,
    Identifier, TemplateFilter, WildcardFilter, cumulative_filter,
    get_active_contracts_response::ContractEntry, value,
};

use crate::{config::NodeConfig, error::Result, utils};

use super::types::{ContractInfo, GovernanceAction, GovernanceConfirmation, PartyMetadata};

/// Hardcoded governance confirmation templates
const GOVERNANCE_TEMPLATES: &[(&str, &str, &str)] = &[
    (
        "#bitsafe-vault-governance-v0",
        "BitsafeVault.VaultGovernance",
        "VaultGovernanceConfirmation",
    ),
    (
        "#cbtc-governance-devnet",
        "CBTC.Governance",
        "Confirmation",
    ),
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

/// Get governance confirmations for a decentralized party
///
/// Fetches active contracts matching governance confirmation templates,
/// extracts action field, and groups by action.
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

    // Build filters for all governance templates
    let cumulative_filters: Vec<CumulativeFilter> = GOVERNANCE_TEMPLATES
        .iter()
        .map(|(package_id, module_name, entity_name)| {
            let template_id = Identifier {
                package_id: (*package_id).to_string(),
                module_name: (*module_name).to_string(),
                entity_name: (*entity_name).to_string(),
            };
            CumulativeFilter {
                identifier_filter: Some(cumulative_filter::IdentifierFilter::TemplateFilter(
                    TemplateFilter {
                        template_id: Some(template_id),
                        include_created_event_blob: false,
                    },
                )),
            }
        })
        .collect();

    let mut filters_by_party = HashMap::new();
    filters_by_party.insert(
        party_id.to_string(),
        Filters {
            cumulative: cumulative_filters,
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
