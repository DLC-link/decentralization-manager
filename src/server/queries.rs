use std::collections::HashMap;

use canton_proto_rs::com::daml::ledger::api::v2::{
    CreatedEvent, CumulativeFilter, EventFormat, Filters, GetActiveContractsRequest,
    GetLedgerEndRequest, Identifier, TemplateFilter, Value, WildcardFilter,
    admin::ListKnownPartiesRequest, cumulative_filter,
    get_active_contracts_response::ContractEntry, value,
};

use crate::{
    config::{NodeConfig, PackageConfig},
    error::Result,
    participant_id::CantonId,
    utils,
};

use super::{
    action_serializer,
    types::{
        ActionType, ContractInfo, GovernanceAction, GovernanceConfirmation, GovernanceState,
        PartyMetadata, ProviderServiceInfo, UserServiceInfo, VaultInfo,
    },
};

/// Template identifier for DAML contracts
struct TemplateId {
    package_id: String,
    module_name: &'static str,
    entity_name: &'static str,
}

/// Contract template identifiers for the contracts list
/// Each template is queried separately to handle cases where packages may not exist
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
    // Vault contracts (configurable package ID)
    if let Some(ref pkg) = packages.vault_governance {
        templates.push(TemplateId {
            package_id: pkg.clone(),
            module_name: "BitsafeVault.VaultGovernance",
            entity_name: "VaultGovernanceRules",
        });
    }
    templates
}

/// Governance confirmation template identifiers
/// Each template is queried separately to handle cases where packages may not exist
fn governance_templates(packages: &PackageConfig) -> Vec<TemplateId> {
    let mut templates = Vec::new();
    if let Some(ref pkg) = packages.vault_governance {
        templates.push(TemplateId {
            package_id: pkg.clone(),
            module_name: "BitsafeVault.VaultGovernance",
            entity_name: "VaultGovernanceConfirmation",
        });
    }
    templates.push(TemplateId {
        package_id: "#cbtc-governance".to_string(),
        module_name: "CBTC.Governance",
        entity_name: "Confirmation",
    });
    templates
}

/// Governance state template identifier
fn governance_state_template(packages: &PackageConfig) -> Option<TemplateId> {
    packages.vault_governance.as_ref().map(|pkg| TemplateId {
        package_id: pkg.clone(),
        module_name: "BitsafeVault.VaultGovernance",
        entity_name: "VaultGovernanceRules",
    })
}

/// Vault template identifier
fn vault_template(packages: &PackageConfig) -> Option<TemplateId> {
    packages.vault.as_ref().map(|pkg| TemplateId {
        package_id: pkg.clone(),
        module_name: "BitsafeVault.Vault",
        entity_name: "Vault",
    })
}

/// ProviderService template identifier
fn provider_service_template(packages: &PackageConfig) -> Option<TemplateId> {
    packages.utility_registry.as_ref().map(|pkg| TemplateId {
        package_id: pkg.clone(),
        module_name: "Utility.Registry.App.V0.Service.Provider",
        entity_name: "ProviderService",
    })
}

/// UserService template identifier
fn user_service_template(packages: &PackageConfig) -> Option<TemplateId> {
    packages.utility_credential.as_ref().map(|pkg| TemplateId {
        package_id: pkg.clone(),
        module_name: "Utility.Credential.App.V0.Service.User",
        entity_name: "UserService",
    })
}

/// Module/entity names for contract templates (used for wildcard filtering)
const CONTRACT_TEMPLATE_NAMES: &[(&str, &str)] = &[
    ("CBTC.Governance", "CBTCGovernanceRules"),
    ("CBTC.DepositAccount", "CBTCDepositAccountRules"),
    ("CBTC.DepositAccount", "CBTCDepositAccount"),
    ("CBTC.WithdrawAccount", "CBTCWithdrawAccountRules"),
    ("CBTC.WithdrawAccount", "CBTCWithdrawAccount"),
    ("BitsafeVault.VaultGovernance", "VaultGovernanceRules"),
];

/// Check if a template matches any contract template we want to display
fn is_contract_template(module_name: &str, entity_name: &str) -> bool {
    CONTRACT_TEMPLATE_NAMES
        .iter()
        .any(|(m, e)| *m == module_name && *e == entity_name)
}

/// Module/entity names for governance templates (used for wildcard filtering)
const GOVERNANCE_TEMPLATE_NAMES: &[(&str, &str)] = &[
    (
        "BitsafeVault.VaultGovernance",
        "VaultGovernanceConfirmation",
    ),
    ("CBTC.Governance", "Confirmation"),
];

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
    packages: &PackageConfig,
) -> Result<Vec<ContractInfo>> {
    let mut contracts = Vec::new();

    if test_mode {
        // Test mode: use WildcardFilter with in-memory filtering
        tracing::debug!("Using WildcardFilter for contracts query (test mode)");
        fetch_contracts_with_wildcard(config, party_id, token, &mut contracts).await?;
    } else {
        // Production mode: query each template separately to handle missing packages
        tracing::debug!("Using TemplateFilter for contracts query (per-template)");
        for t in &contract_templates(packages) {
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
    GOVERNANCE_TEMPLATE_NAMES
        .iter()
        .any(|(m, e)| *m == module_name && *e == entity_name)
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
    packages: &PackageConfig,
) -> Result<Vec<GovernanceAction>> {
    // Collect confirmations grouped by action hash
    let mut confirmations_by_hash: HashMap<String, (ActionType, Vec<GovernanceConfirmation>)> =
        HashMap::new();

    if test_mode {
        tracing::debug!("Using WildcardFilter for governance query (test mode)");
        fetch_governance_with_wildcard(config, party_id, token, &mut confirmations_by_hash).await?;
    } else {
        tracing::debug!("Using TemplateFilter for governance query (per-template)");
        for t in &governance_templates(packages) {
            match fetch_governance_for_template(
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

    // Convert to GovernanceAction list, deduplicating confirmations by confirming_party
    let actions: Vec<GovernanceAction> = confirmations_by_hash
        .into_iter()
        .map(|(action_hash, (action, confirmations))| {
            // Deduplicate by confirming_party - keep only one confirmation per member
            let mut seen_parties = std::collections::HashSet::new();
            let unique_confirmations: Vec<GovernanceConfirmation> = confirmations
                .into_iter()
                .filter(|c| seen_parties.insert(c.confirming_party.clone()))
                .collect();

            let confirmation_count = unique_confirmations.len();
            GovernanceAction {
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

/// Fetch governance confirmations using WildcardFilter (for test mode)
async fn fetch_governance_with_wildcard(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
    confirmations_by_hash: &mut HashMap<String, (ActionType, Vec<GovernanceConfirmation>)>,
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
                extract_and_add_confirmation(&created, confirmations_by_hash);
            }
        }
    }

    Ok(())
}

/// Fetch governance confirmations for a specific template
async fn fetch_governance_for_template(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
    template: &TemplateId,
    confirmations_by_hash: &mut HashMap<String, (ActionType, Vec<GovernanceConfirmation>)>,
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
            extract_and_add_confirmation(&created, confirmations_by_hash);
        }
    }

    Ok(())
}

/// Extract action and confirming_party from a created event, parse action, and add to map
fn extract_and_add_confirmation(
    created: &CreatedEvent,
    confirmations_by_hash: &mut HashMap<String, (ActionType, Vec<GovernanceConfirmation>)>,
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

    let confirmation = GovernanceConfirmation {
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
// Governance State Query
// ============================================================================

/// Get the state of the VaultGovernanceRules contract for a party
pub async fn get_governance_state(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
    test_mode: bool,
    packages: &PackageConfig,
) -> Result<Option<GovernanceState>> {
    if test_mode {
        fetch_governance_state_with_wildcard(config, party_id, token).await
    } else {
        match governance_state_template(packages) {
            Some(template) => {
                fetch_governance_state_for_template(config, party_id, token, &template).await
            }
            None => Ok(None),
        }
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
                && template_id.module_name == "BitsafeVault.VaultGovernance"
                && template_id.entity_name == "VaultGovernanceRules"
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
    template: &TemplateId,
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
                            package_id: template.package_id.clone(),
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
            return Ok(extract_governance_state(&created));
        }
    }

    Ok(None)
}

/// Extract governance state from a VaultGovernanceRules created event
fn extract_governance_state(created: &CreatedEvent) -> Option<GovernanceState> {
    let record = created.create_arguments.as_ref()?;

    // Extract vaultManager (Party)
    let vault_manager: CantonId = record
        .fields
        .iter()
        .find(|f| f.label == "vaultManager")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Party(p)) => p.parse().ok(),
            _ => None,
        })?;

    // Extract members (Set Party - stored as GenMap<Party, Unit> inside a Record)
    let members: Vec<CantonId> = record
        .fields
        .iter()
        .find(|f| f.label == "members")
        .and_then(|f| f.value.as_ref())
        .and_then(extract_party_set)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| s.parse().ok())
        .collect();

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

// ============================================================================
// Vault Contracts Query
// ============================================================================

/// Get all Vault contracts for a party
pub async fn get_vaults(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
    test_mode: bool,
    packages: &PackageConfig,
) -> Result<Vec<VaultInfo>> {
    if test_mode {
        fetch_vaults_with_wildcard(config, party_id, token).await
    } else {
        match vault_template(packages) {
            Some(template) => fetch_vaults_for_template(config, party_id, token, &template).await,
            None => Ok(Vec::new()),
        }
    }
}

/// Fetch vaults using WildcardFilter (for test mode)
async fn fetch_vaults_with_wildcard(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
) -> Result<Vec<VaultInfo>> {
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
        .get_active_contracts(acs_request)
        .await?
        .into_inner();

    let mut vaults = Vec::new();
    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
            && let Some(template_id) = &created.template_id
            && template_id.module_name == "BitsafeVault.Vault"
            && template_id.entity_name == "Vault"
            && let Some(vault_info) = extract_vault_info(&created)
        {
            vaults.push(vault_info);
        }
    }

    Ok(vaults)
}

/// Fetch vaults using TemplateFilter
async fn fetch_vaults_for_template(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
    template: &TemplateId,
) -> Result<Vec<VaultInfo>> {
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
                            package_id: template.package_id.clone(),
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
        .get_active_contracts(acs_request)
        .await?
        .into_inner();

    let mut vaults = Vec::new();
    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
            && let Some(vault_info) = extract_vault_info(&created)
        {
            vaults.push(vault_info);
        }
    }

    Ok(vaults)
}

/// Extract VaultInfo from a Vault created event
fn extract_vault_info(created: &CreatedEvent) -> Option<VaultInfo> {
    let record = created.create_arguments.as_ref()?;

    // Extract vaultConfig (Record with name and shareSymbol)
    let vault_config = record
        .fields
        .iter()
        .find(|f| f.label == "vaultConfig")
        .and_then(|f| f.value.as_ref())?;

    let (vault_name, share_symbol) = extract_vault_config(vault_config)?;

    // Extract isPaused (Bool)
    let is_paused = record
        .fields
        .iter()
        .find(|f| f.label == "isPaused")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Bool(b)) => Some(*b),
            _ => None,
        })
        .unwrap_or(false);

    // Extract vaultManager (Party)
    let vault_manager: CantonId = record
        .fields
        .iter()
        .find(|f| f.label == "vaultManager")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Party(p)) => p.parse().ok(),
            _ => None,
        })?;

    Some(VaultInfo {
        contract_id: created.contract_id.clone(),
        vault_name,
        share_symbol,
        is_paused,
        vault_manager,
    })
}

/// Extract vault name and share symbol from VaultConfig record
fn extract_vault_config(value: &Value) -> Option<(String, String)> {
    match &value.sum {
        Some(value::Sum::Record(record)) => {
            let name = record
                .fields
                .iter()
                .find(|f| f.label == "name")
                .and_then(|f| f.value.as_ref())
                .and_then(|v| match &v.sum {
                    Some(value::Sum::Text(t)) => Some(t.clone()),
                    _ => None,
                })?;

            let share_symbol = record
                .fields
                .iter()
                .find(|f| f.label == "shareSymbol")
                .and_then(|f| f.value.as_ref())
                .and_then(|v| match &v.sum {
                    Some(value::Sum::Text(t)) => Some(t.clone()),
                    _ => None,
                })?;

            Some((name, share_symbol))
        }
        _ => None,
    }
}

// ============================================================================
// Utility Service Queries
// ============================================================================

/// Get all ProviderService contracts for a party
pub async fn get_provider_services(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
    test_mode: bool,
    packages: &PackageConfig,
) -> Result<Vec<ProviderServiceInfo>> {
    if test_mode {
        fetch_provider_services_with_wildcard(config, party_id, token).await
    } else {
        match provider_service_template(packages) {
            Some(template) => {
                fetch_provider_services_for_template(config, party_id, token, &template).await
            }
            None => Ok(Vec::new()),
        }
    }
}

/// Fetch provider services using WildcardFilter (for test mode)
async fn fetch_provider_services_with_wildcard(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
) -> Result<Vec<ProviderServiceInfo>> {
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
        .get_active_contracts(acs_request)
        .await?
        .into_inner();

    let mut services = Vec::new();
    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
            && let Some(template_id) = &created.template_id
            && template_id.module_name == "Utility.Registry.App.V0.Service.Provider"
            && template_id.entity_name == "ProviderService"
            && let Some(info) = extract_provider_service_info(&created)
        {
            services.push(info);
        }
    }

    Ok(services)
}

/// Fetch provider services using TemplateFilter
async fn fetch_provider_services_for_template(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
    template: &TemplateId,
) -> Result<Vec<ProviderServiceInfo>> {
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
                            package_id: template.package_id.clone(),
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
        .get_active_contracts(acs_request)
        .await?
        .into_inner();

    let mut services = Vec::new();
    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
            && let Some(info) = extract_provider_service_info(&created)
        {
            services.push(info);
        }
    }

    Ok(services)
}

/// Extract ProviderServiceInfo from a ProviderService created event
fn extract_provider_service_info(created: &CreatedEvent) -> Option<ProviderServiceInfo> {
    let record = created.create_arguments.as_ref()?;

    let operator: CantonId = record
        .fields
        .iter()
        .find(|f| f.label == "operator")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Party(p)) => p.parse().ok(),
            _ => None,
        })?;

    let provider: CantonId = record
        .fields
        .iter()
        .find(|f| f.label == "provider")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Party(p)) => p.parse().ok(),
            _ => None,
        })?;

    Some(ProviderServiceInfo {
        contract_id: created.contract_id.clone(),
        operator,
        provider,
    })
}

/// Get all UserService contracts for a party
pub async fn get_user_services(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
    test_mode: bool,
    packages: &PackageConfig,
) -> Result<Vec<UserServiceInfo>> {
    if test_mode {
        fetch_user_services_with_wildcard(config, party_id, token).await
    } else {
        match user_service_template(packages) {
            Some(template) => {
                fetch_user_services_for_template(config, party_id, token, &template).await
            }
            None => Ok(Vec::new()),
        }
    }
}

/// Fetch user services using WildcardFilter (for test mode)
async fn fetch_user_services_with_wildcard(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
) -> Result<Vec<UserServiceInfo>> {
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
        .get_active_contracts(acs_request)
        .await?
        .into_inner();

    let mut services = Vec::new();
    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
            && let Some(template_id) = &created.template_id
            && template_id.module_name == "Utility.Credential.App.V0.Service.User"
            && template_id.entity_name == "UserService"
            && let Some(info) = extract_user_service_info(&created)
        {
            services.push(info);
        }
    }

    Ok(services)
}

/// Fetch user services using TemplateFilter
async fn fetch_user_services_for_template(
    config: &NodeConfig,
    party_id: &str,
    token: Option<String>,
    template: &TemplateId,
) -> Result<Vec<UserServiceInfo>> {
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
                            package_id: template.package_id.clone(),
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
        .get_active_contracts(acs_request)
        .await?
        .into_inner();

    let mut services = Vec::new();
    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
            && let Some(info) = extract_user_service_info(&created)
        {
            services.push(info);
        }
    }

    Ok(services)
}

/// Extract UserServiceInfo from a UserService created event
fn extract_user_service_info(created: &CreatedEvent) -> Option<UserServiceInfo> {
    let record = created.create_arguments.as_ref()?;

    let operator: CantonId = record
        .fields
        .iter()
        .find(|f| f.label == "operator")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Party(p)) => p.parse().ok(),
            _ => None,
        })?;

    let user: CantonId = record
        .fields
        .iter()
        .find(|f| f.label == "user")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Party(p)) => p.parse().ok(),
            _ => None,
        })?;

    Some(UserServiceInfo {
        contract_id: created.contract_id.clone(),
        operator,
        user,
    })
}
