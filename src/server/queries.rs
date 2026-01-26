use std::collections::HashMap;

use canton_proto_rs::com::daml::ledger::api::v2::{
    CreatedEvent, CumulativeFilter, EventFormat, Filters, GetActiveContractsRequest,
    GetLedgerEndRequest, Identifier, TemplateFilter, Value, WildcardFilter,
    admin::ListKnownPartiesRequest, cumulative_filter,
    get_active_contracts_response::ContractEntry, value,
};

use crate::{config::NodeConfig, consts::VAULT_GOVERNANCE_PACKAGE_ID, error::Result, utils};

use super::{
    action_serializer,
    types::{
        ActionType, ContractInfo, GovernanceActionV2, GovernanceConfirmationV2, PartyMetadata,
    },
};

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
        package_id: VAULT_GOVERNANCE_PACKAGE_ID,
        module_name: "BitsafeVault.VaultGovernance",
        entity_name: "VaultGovernanceRules",
    },
];

/// Governance confirmation template identifiers
/// Each template is queried separately to handle cases where packages may not exist
const GOVERNANCE_TEMPLATES: &[TemplateId] = &[
    TemplateId {
        package_id: VAULT_GOVERNANCE_PACKAGE_ID,
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
            match fetch_contracts_for_template(config, party_id, token.clone(), t, &mut contracts)
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
) -> Result {
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
    template: &TemplateId,
    contracts: &mut Vec<ContractInfo>,
) -> Result {
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
                            package_id: template.package_id.to_string(),
                            module_name: template.module_name.to_string(),
                            entity_name: template.entity_name.to_string(),
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
            let template_id_str = format!("{}:{}", template.module_name, template.entity_name);

            contracts.push(ContractInfo {
                contract_id: created.contract_id,
                template_id: template_id_str,
                package_id: template.package_id.to_string(),
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

// ============================================================================
// Governance Queries (with parsed actions)
// ============================================================================

/// Get governance confirmations for a decentralized party with parsed actions
///
/// Similar to get_governance_confirmations but parses the action field into ActionType
/// and groups by deterministic action hash.
pub async fn get_governance_confirmations(
    config: &NodeConfig,
    party_id: &str,
    threshold: usize,
    token: Option<String>,
    test_mode: bool,
) -> Result<Vec<GovernanceActionV2>> {
    // Collect confirmations grouped by action hash
    let mut confirmations_by_hash: HashMap<String, (ActionType, Vec<GovernanceConfirmationV2>)> =
        HashMap::new();

    if test_mode {
        tracing::debug!("Using WildcardFilter for governance V2 query (test mode)");
        fetch_governance_v2_with_wildcard(config, party_id, token, &mut confirmations_by_hash)
            .await?;
    } else {
        tracing::debug!("Using TemplateFilter for governance V2 query (per-template)");
        for t in GOVERNANCE_TEMPLATES {
            match fetch_governance_v2_for_template(
                config,
                party_id,
                token.clone(),
                t,
                &mut confirmations_by_hash,
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

    // Convert to GovernanceActionV2 list, deduplicating confirmations by confirming_party
    let actions: Vec<GovernanceActionV2> = confirmations_by_hash
        .into_iter()
        .map(|(action_hash, (action, confirmations))| {
            // Deduplicate by confirming_party - keep only one confirmation per member
            let mut seen_parties = std::collections::HashSet::new();
            let unique_confirmations: Vec<GovernanceConfirmationV2> = confirmations
                .into_iter()
                .filter(|c| seen_parties.insert(c.confirming_party.clone()))
                .collect();

            let confirmation_count = unique_confirmations.len();
            GovernanceActionV2 {
                action_hash,
                action,
                confirmations: unique_confirmations,
                confirmation_count,
                can_execute: confirmation_count >= threshold,
            }
        })
        .collect();

    Ok(actions)
}

/// Fetch V2 governance confirmations using WildcardFilter (for test mode)
async fn fetch_governance_v2_with_wildcard(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
    confirmations_by_hash: &mut HashMap<String, (ActionType, Vec<GovernanceConfirmationV2>)>,
) -> Result {
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
            // Check if this is a governance template
            if let Some(ref template_id) = created.template_id
                && is_governance_template(&template_id.module_name, &template_id.entity_name)
            {
                extract_and_add_confirmation_v2(&created, confirmations_by_hash);
            }
        }
    }

    Ok(())
}

/// Fetch V2 governance confirmations for a specific template
async fn fetch_governance_v2_for_template(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
    template: &TemplateId,
    confirmations_by_hash: &mut HashMap<String, (ActionType, Vec<GovernanceConfirmationV2>)>,
) -> Result {
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
                            package_id: template.package_id.to_string(),
                            module_name: template.module_name.to_string(),
                            entity_name: template.entity_name.to_string(),
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
            extract_and_add_confirmation_v2(&created, confirmations_by_hash);
        }
    }

    Ok(())
}

/// Extract action and confirming_party from a created event, parse action, and add to map (V2)
fn extract_and_add_confirmation_v2(
    created: &CreatedEvent,
    confirmations_by_hash: &mut HashMap<String, (ActionType, Vec<GovernanceConfirmationV2>)>,
) {
    let Some(record) = &created.create_arguments else {
        return;
    };

    // Extract action field (this is a Variant for VaultGovernance)
    let action_value = record.fields.iter().find(|f| f.label == "action");
    let Some(action_field) = action_value.and_then(|f| f.value.as_ref()) else {
        tracing::warn!("No action field found in confirmation contract");
        return;
    };

    // Try to parse the action
    let action = match action_serializer::deserialize_action(action_field) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("Failed to deserialize action: {e}");
            return;
        }
    };

    // Extract confirming party
    let confirming_party = record
        .fields
        .iter()
        .find(|f| f.label == "confirmingParty" || f.label == "confirmer")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Party(p)) => Some(p.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "unknown".to_string());

    // Compute action hash for grouping (JSON serialization is deterministic enough)
    let action_hash = compute_action_hash(&action);

    let confirmation = GovernanceConfirmationV2 {
        contract_id: created.contract_id.clone(),
        action: action.clone(),
        confirming_party,
    };

    confirmations_by_hash
        .entry(action_hash)
        .or_insert_with(|| (action, Vec::new()))
        .1
        .push(confirmation);
}

/// Compute a deterministic hash of an action for grouping confirmations
fn compute_action_hash(action: &ActionType) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // Serialize to JSON for deterministic representation
    let json = serde_json::to_string(action).unwrap_or_default();

    let mut hasher = DefaultHasher::new();
    json.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

// ============================================================================
// V2 Governance State Query
// ============================================================================

use super::types::GovernanceState;

/// VaultGovernanceRules template identifier
const VAULT_GOVERNANCE_RULES: TemplateId = TemplateId {
    package_id: VAULT_GOVERNANCE_PACKAGE_ID,
    module_name: "BitsafeVault.VaultGovernance",
    entity_name: "VaultGovernanceRules",
};

/// Get the state of the VaultGovernanceRules contract for a party
pub async fn get_governance_state(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
    test_mode: bool,
) -> Result<Option<GovernanceState>> {
    if test_mode {
        fetch_governance_state_with_wildcard(config, party_id, token).await
    } else {
        fetch_governance_state_for_template(config, party_id, token).await
    }
}

/// Fetch governance state using WildcardFilter (for test mode)
async fn fetch_governance_state_with_wildcard(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
) -> Result<Option<GovernanceState>> {
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
            // Check if this is the VaultGovernanceRules template
            if let Some(ref template_id) = created.template_id
                && template_id.module_name == VAULT_GOVERNANCE_RULES.module_name
                && template_id.entity_name == VAULT_GOVERNANCE_RULES.entity_name
            {
                return Ok(extract_governance_state(&created));
            }
        }
    }

    Ok(None)
}

/// Fetch governance state for a specific template
async fn fetch_governance_state_for_template(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
) -> Result<Option<GovernanceState>> {
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
                            package_id: VAULT_GOVERNANCE_RULES.package_id.to_string(),
                            module_name: VAULT_GOVERNANCE_RULES.module_name.to_string(),
                            entity_name: VAULT_GOVERNANCE_RULES.entity_name.to_string(),
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
            return Ok(extract_governance_state(&created));
        }
    }

    Ok(None)
}

/// Extract governance state from a VaultGovernanceRules created event
fn extract_governance_state(created: &CreatedEvent) -> Option<GovernanceState> {
    let record = created.create_arguments.as_ref()?;

    // Extract vaultManager (Party)
    let vault_manager = record
        .fields
        .iter()
        .find(|f| f.label == "vaultManager")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Party(p)) => Some(p.clone()),
            _ => None,
        })?;

    // Extract members (Set Party - stored as GenMap<Party, Unit> inside a Record)
    let members = record
        .fields
        .iter()
        .find(|f| f.label == "members")
        .and_then(|f| f.value.as_ref())
        .and_then(extract_party_set)
        .unwrap_or_default();

    // Extract threshold (Int)
    let threshold = record
        .fields
        .iter()
        .find(|f| f.label == "threshold")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Int64(i)) => Some(*i),
            _ => None,
        })
        .unwrap_or(0);

    // Extract actionConfirmationTimeout (Optional RelTime - RelTime is a Record with Int64)
    let timeout = record
        .fields
        .iter()
        .find(|f| f.label == "actionConfirmationTimeout")
        .and_then(|f| f.value.as_ref())
        .and_then(extract_optional_reltime);

    Some(GovernanceState {
        contract_id: created.contract_id.clone(),
        vault_manager,
        members,
        threshold,
        action_confirmation_timeout_microseconds: timeout,
    })
}

/// Extract a Set Party (DA.Set.Types:Set) which is stored as Record { map: GenMap<Party, Unit> }
fn extract_party_set(value: &Value) -> Option<Vec<String>> {
    // Set Party is represented as a Record containing a GenMap
    match &value.sum {
        Some(value::Sum::Record(record)) => {
            // The record should have a "map" field containing the GenMap
            record
                .fields
                .iter()
                .find(|f| f.label == "map")
                .and_then(|f| f.value.as_ref())
                .and_then(extract_genmap_parties)
        }
        // Fallback: try as GenMap directly
        Some(value::Sum::GenMap(gen_map)) => Some(
            gen_map
                .entries
                .iter()
                .filter_map(|entry| {
                    entry.key.as_ref().and_then(|k| match &k.sum {
                        Some(value::Sum::Party(p)) => Some(p.clone()),
                        _ => None,
                    })
                })
                .collect(),
        ),
        _ => None,
    }
}

/// Extract parties from a GenMap<Party, Unit>
fn extract_genmap_parties(value: &Value) -> Option<Vec<String>> {
    match &value.sum {
        Some(value::Sum::GenMap(gen_map)) => Some(
            gen_map
                .entries
                .iter()
                .filter_map(|entry| {
                    entry.key.as_ref().and_then(|k| match &k.sum {
                        Some(value::Sum::Party(p)) => Some(p.clone()),
                        _ => None,
                    })
                })
                .collect(),
        ),
        _ => None,
    }
}

/// Extract Optional RelTime (DA.Time.Types:RelTime is Record { microseconds: Int64 })
fn extract_optional_reltime(value: &Value) -> Option<i64> {
    match &value.sum {
        Some(value::Sum::Optional(opt)) => {
            opt.value.as_ref().and_then(|v| extract_reltime(v.as_ref()))
        }
        _ => None,
    }
}

/// Extract RelTime (stored as Record { microseconds: Int64 })
fn extract_reltime(value: &Value) -> Option<i64> {
    match &value.sum {
        Some(value::Sum::Record(record)) => record
            .fields
            .iter()
            .find(|f| f.label == "microseconds")
            .and_then(|f| f.value.as_ref())
            .and_then(|v| match &v.sum {
                Some(value::Sum::Int64(i)) => Some(*i),
                _ => None,
            }),
        // Fallback: try as Int64 directly
        Some(value::Sum::Int64(i)) => Some(*i),
        _ => None,
    }
}
