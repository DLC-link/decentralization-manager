use std::collections::HashMap;

use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        CreatedEvent, CumulativeFilter, EventFormat, Filters, GetActiveContractsRequest,
        GetLedgerEndRequest, Identifier, InterfaceFilter, TemplateFilter, Value, WildcardFilter,
        admin::ListKnownPartiesRequest, cumulative_filter,
        get_active_contracts_response::ContractEntry, value,
    },
    digitalasset::canton::admin::participant::v30::{
        ListPackagesRequest, package_service_client::PackageServiceClient,
    },
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
        ActionType, ContractInfo, ContractWithBlob, DomainGovernanceAction, GovernanceAction,
        GovernanceConfirmation, GovernanceState, InstrumentInfo, PartyMetadata,
        ProviderServiceInfo, RegistrarServiceInfo, UserServiceInfo, VaultInfo,
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
    // Governance Core contracts (configurable package ID)
    if let Some(ref pkg) = packages.governance_core {
        templates.push(TemplateId {
            package_id: pkg.clone(),
            module_name: "Governance.Rules",
            entity_name: "GovernanceRules",
        });
    }
    // Vault contracts (configurable package ID)
    if let Some(ref pkg) = packages.vault_governance {
        templates.push(TemplateId {
            package_id: pkg.clone(),
            module_name: "BitsafeVault.VaultGovernance",
            entity_name: "VaultGovernanceRules",
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
    if let Some(ref pkg) = packages.governance_core {
        templates.push(TemplateId {
            package_id: pkg.clone(),
            module_name: "Governance.Rules",
            entity_name: "GovernanceSelfConfirmation",
        });
        templates.push(TemplateId {
            package_id: pkg.clone(),
            module_name: "Governance.Confirmation",
            entity_name: "GovernanceConfirmation",
        });
    }
    templates
}

/// Governance state template identifiers (tries both vault and core)
fn governance_state_templates(packages: &PackageConfig) -> Vec<TemplateId> {
    let mut templates = Vec::new();
    if let Some(ref pkg) = packages.vault_governance {
        templates.push(TemplateId {
            package_id: pkg.clone(),
            module_name: "BitsafeVault.VaultGovernance",
            entity_name: "VaultGovernanceRules",
        });
    }
    if let Some(ref pkg) = packages.governance_core {
        templates.push(TemplateId {
            package_id: pkg.clone(),
            module_name: "Governance.Rules",
            entity_name: "GovernanceRules",
        });
    }
    templates
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

/// RegistrarService template identifier
fn registrar_service_template(packages: &PackageConfig) -> Option<TemplateId> {
    packages.utility_registry.as_ref().map(|pkg| TemplateId {
        package_id: pkg.clone(),
        module_name: "Utility.Registry.App.V0.Service.Registrar",
        entity_name: "RegistrarService",
    })
}

/// Module/entity names for contract templates (used for wildcard filtering)
const CONTRACT_TEMPLATE_NAMES: &[(&str, &str)] = &[
    ("BitsafeVault.VaultGovernance", "VaultGovernanceRules"),
    ("CBTC.DepositAccount", "CBTCDepositAccount"),
    ("CBTC.DepositAccount", "CBTCDepositAccountRules"),
    ("CBTC.Governance", "CBTCGovernanceRules"),
    ("CBTC.WithdrawAccount", "CBTCWithdrawAccount"),
    ("CBTC.WithdrawAccount", "CBTCWithdrawAccountRules"),
    ("Governance.Rules", "GovernanceRules"),
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
    ("Governance.Rules", "GovernanceSelfConfirmation"),
    ("Governance.Confirmation", "GovernanceConfirmation"),
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
    party_id: &CantonId,
    token: Option<String>,
    test_mode: bool,
    packages: &PackageConfig,
) -> Result<Vec<ContractInfo>> {
    let mut contracts = Vec::new();

    // Build a {package_id → version} map once per request from the
    // participant Admin API. The Ledger API itself only returns
    // `package_name` on each created event — version metadata lives on the
    // Admin PackageService. Failure to load is non-fatal: contracts simply
    // ship with an empty version string.
    let package_versions = match fetch_package_versions(config).await {
        Ok(map) => map,
        Err(e) => {
            tracing::warn!("Failed to load package versions from Admin API: {e}");
            HashMap::new()
        }
    };

    if test_mode {
        // Test mode: use WildcardFilter with in-memory filtering
        tracing::debug!("Using WildcardFilter for contracts query (test mode)");
        fetch_contracts_with_wildcard(config, party_id, token, &package_versions, &mut contracts)
            .await?;
    } else {
        // Production mode: query each template separately to handle missing packages
        tracing::debug!("Using TemplateFilter for contracts query (per-template)");
        for t in &contract_templates(packages) {
            match fetch_contracts_for_template(
                config,
                party_id,
                token.clone(),
                t,
                &package_versions,
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

    sort_contracts(&mut contracts);
    Ok(contracts)
}

/// Sort contracts for display and collapse duplicates.
///
/// Sort order:
///   1. `package_name` ascending (case-insensitive)
///   2. `package_version` descending (semver-aware: numeric segments compared
///      numerically; non-numeric tail compared lexicographically so
///      `0.1.18 > 0.1.7`)
///   3. `template_id` ascending (groups duplicate template instances together)
///   4. `created_at` descending (latest first within a duplicate group)
///
/// Then duplicates that share the same
/// `(package_name, package_version, template_id)` triple are collapsed into
/// the latest one — `dedup_by` after the sort keeps the first occurrence,
/// which is the latest by `created_at`.
///
/// Used by both the live ACS path (`get_contracts`) and the cache-read path
/// in `handlers::parties` so the frontend always receives the same ordering.
#[allow(clippy::ptr_arg)] // need Vec for dedup_by truncation
pub fn sort_contracts(contracts: &mut Vec<ContractInfo>) {
    contracts.sort_by(|a, b| {
        a.package_name
            .to_lowercase()
            .cmp(&b.package_name.to_lowercase())
            .then_with(|| compare_versions(&b.package_version, &a.package_version))
            .then_with(|| a.template_id.cmp(&b.template_id))
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
    contracts.dedup_by(|a, b| {
        a.package_name == b.package_name
            && a.package_version == b.package_version
            && a.template_id == b.template_id
    });
}

fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.split('.');
    let mut bi = b.split('.');
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => {
                let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(xn), Ok(yn)) => xn.cmp(&yn),
                    _ => x.cmp(y),
                };
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

/// Load `(package_id → version)` from the participant's Admin PackageService.
/// One call per request — small map (~hundreds of rows), no caching needed.
async fn fetch_package_versions(config: &NodeConfig) -> Result<HashMap<String, String>> {
    let mut client = PackageServiceClient::connect(config.admin_api_url()).await?;
    let response = client
        .list_packages(tonic::Request::new(ListPackagesRequest {
            limit: 0,
            filter_name: String::new(),
        }))
        .await?
        .into_inner();
    Ok(response
        .package_descriptions
        .into_iter()
        .map(|p| (p.package_id, p.version))
        .collect())
}

/// Format a `prost_types::Timestamp` as an ISO 8601 UTC string with
/// nanosecond precision (`YYYY-MM-DDTHH:MM:SS.nnnnnnnnnZ`). Hand-rolled with
/// Howard Hinnant's date algorithm to avoid pulling in chrono just for this.
fn format_timestamp(ts: &::prost_types::Timestamp) -> String {
    let secs = ts.seconds;
    let day_secs = 86_400i64;
    let days = secs.div_euclid(day_secs);
    let sod = secs.rem_euclid(day_secs);
    let hour = sod / 3600;
    let minute = (sod % 3600) / 60;
    let second = sod % 60;

    // Civil-from-days: see https://howardhinnant.github.io/date_algorithms.html
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }

    format!(
        "{y:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}.{nanos:09}Z",
        nanos = ts.nanos
    )
}

fn render_contract_info(
    created: &CreatedEvent,
    package_versions: &HashMap<String, String>,
) -> ContractInfo {
    let template = created.template_id.as_ref();
    let template_id = template
        .map(|t| format!("{}:{}", t.module_name, t.entity_name))
        .unwrap_or_default();
    let package_id = template.map(|t| t.package_id.clone()).unwrap_or_default();
    let package_version = package_versions
        .get(&package_id)
        .cloned()
        .unwrap_or_default();
    let created_at = created
        .created_at
        .as_ref()
        .map(format_timestamp)
        .unwrap_or_default();
    ContractInfo {
        contract_id: created.contract_id.clone(),
        template_id,
        package_id,
        package_name: created.package_name.clone(),
        package_version,
        created_at,
    }
}

/// Fetch contracts using WildcardFilter (for test mode)
async fn fetch_contracts_with_wildcard(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    package_versions: &HashMap<String, String>,
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
            let is_wanted = created
                .template_id
                .as_ref()
                .map(|t| is_contract_template(&t.module_name, &t.entity_name))
                .unwrap_or(false);

            if !is_wanted {
                continue;
            }

            contracts.push(render_contract_info(&created, package_versions));
        }
    }

    Ok(())
}

/// Fetch contracts for a specific template
async fn fetch_contracts_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
    package_versions: &HashMap<String, String>,
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
            contracts.push(render_contract_info(&created, package_versions));
        }
    }

    Ok(())
}

/// Get party metadata from Ledger API
pub async fn get_party_metadata(
    config: &NodeConfig,
    party_id: &CantonId,
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

    let party_id_str = party_id.to_string();
    let party_details = response
        .party_details
        .iter()
        .find(|p| p.party == party_id_str);

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
    party_id: &CantonId,
    threshold: usize,
    token: Option<String>,
    test_mode: bool,
    packages: &PackageConfig,
) -> Result<(Vec<GovernanceAction>, Vec<DomainGovernanceAction>)> {
    // Collect confirmations grouped by action hash (vault + core self-management)
    let mut confirmations_by_hash: HashMap<String, (ActionType, Vec<GovernanceConfirmation>)> =
        HashMap::new();
    // Collect domain confirmations grouped by proposal CID (core domain actions)
    let mut domain_confirmations: HashMap<String, (String, Vec<GovernanceConfirmation>)> =
        HashMap::new();
    // Map of `contract_id -> Option<description>` for every active
    // `GovernableAction` proposal visible to this party on this participant.
    // The presence of a key here is what gates inclusion in `domain_actions`
    // below — `Confirmation`s referencing a proposal that's no longer active
    // (or never reached this participant's ACS) get filtered out, otherwise
    // surfacing them in the notification queue gives the user a Confirm
    // button that always 500s with `CONTRACT_NOT_FOUND` on the proposal cid.
    let mut proposal_descriptions: HashMap<String, Option<String>> = HashMap::new();

    if test_mode {
        tracing::debug!("Using WildcardFilter for governance query (test mode)");
        fetch_governance_with_wildcard(
            config,
            party_id,
            token,
            &mut confirmations_by_hash,
            &mut domain_confirmations,
            &mut proposal_descriptions,
        )
        .await?;
    } else {
        tracing::debug!("Using TemplateFilter for governance query (per-template)");
        for t in &governance_templates(packages) {
            match fetch_governance_for_template(
                config,
                party_id,
                token.clone(),
                t,
                &mut confirmations_by_hash,
                &mut domain_confirmations,
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
        // Fetch proposal descriptions via GovernableAction interface query
        if let Err(e) = fetch_proposal_descriptions(
            config,
            party_id,
            token,
            packages,
            &mut proposal_descriptions,
        )
        .await
        {
            tracing::debug!("Could not fetch proposal descriptions: {e}");
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
            let last_confirmation_at = unique_confirmations
                .iter()
                .map(|c| c.created_at)
                .max()
                .unwrap_or(0);
            GovernanceAction {
                action_hash,
                action,
                confirmations: unique_confirmations,
                confirmation_count,
                can_execute: confirmation_count >= threshold,
                last_confirmation_at,
            }
        })
        .collect();

    // Build domain actions from domain confirmations, dropping any whose
    // proposal isn't in the active set on this participant (see comment on
    // `proposal_descriptions` above).
    let domain_actions: Vec<DomainGovernanceAction> = domain_confirmations
        .into_iter()
        .filter_map(|(proposal_cid, (action_label, confirmations))| {
            let description = proposal_descriptions.remove(&proposal_cid)?;
            let mut seen_parties = std::collections::HashSet::new();
            let unique_confirmations: Vec<GovernanceConfirmation> = confirmations
                .into_iter()
                .filter(|c| seen_parties.insert(c.confirming_party.clone()))
                .collect();
            let confirmation_count = unique_confirmations.len();
            Some(DomainGovernanceAction {
                proposal_cid,
                action_label,
                description,
                confirmations: unique_confirmations,
                confirmation_count,
                can_execute: confirmation_count >= threshold,
            })
        })
        .collect();

    Ok((actions, domain_actions))
}

/// Fetch governance confirmations using WildcardFilter (for test mode)
async fn fetch_governance_with_wildcard(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    confirmations_by_hash: &mut HashMap<String, (ActionType, Vec<GovernanceConfirmation>)>,
    domain_confirmations: &mut HashMap<String, (String, Vec<GovernanceConfirmation>)>,
    proposal_descriptions: &mut HashMap<String, Option<String>>,
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
            && let Some(ref template_id) = created.template_id
        {
            if is_governance_template(&template_id.module_name, &template_id.entity_name) {
                if template_id.module_name == "Governance.Confirmation"
                    && template_id.entity_name == "GovernanceConfirmation"
                {
                    extract_and_add_domain_confirmation(&created, domain_confirmations);
                } else {
                    extract_and_add_confirmation(&created, confirmations_by_hash);
                }
            } else {
                // Capture proposal descriptions from GovernableAction contracts
                extract_proposal_description(&created, proposal_descriptions);
            }
        }
    }

    Ok(())
}

/// Fetch governance confirmations for a specific template
async fn fetch_governance_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
    confirmations_by_hash: &mut HashMap<String, (ActionType, Vec<GovernanceConfirmation>)>,
    domain_confirmations: &mut HashMap<String, (String, Vec<GovernanceConfirmation>)>,
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
            if created.template_id.as_ref().is_some_and(|t| {
                t.module_name == "Governance.Confirmation"
                    && t.entity_name == "GovernanceConfirmation"
            }) {
                extract_and_add_domain_confirmation(&created, domain_confirmations);
            } else {
                extract_and_add_confirmation(&created, confirmations_by_hash);
            }
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

    // Try to parse the action (vault ActionRequiringConfirmation or core GovernanceSelfAction)
    let action = match action_serializer::deserialize_action(action_field) {
        Ok(a) => a,
        Err(_) => match action_serializer::deserialize_self_action(action_field) {
            Ok(a) => a,
            Err(e) => {
                tracing::debug!("Skipping confirmation with unrecognized action shape: {e}");
                return;
            }
        },
    };

    // Extract confirming party. Skip the confirmation entirely if the field
    // is missing or the party string isn't a valid CantonId — propagating
    // garbage upstream (the old code used "unknown") makes the consumer
    // fragile.
    let Some(confirming_party_str) = record
        .fields
        .iter()
        .find(|f| f.label == "confirmingParty" || f.label == "confirmer")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Party(p)) => Some(p.clone()),
            _ => None,
        })
    else {
        tracing::warn!(
            "Skipping confirmation {cid}: missing confirmingParty/confirmer field",
            cid = created.contract_id
        );
        return;
    };
    let confirming_party = match CantonId::parse(&confirming_party_str) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "Skipping confirmation {cid}: bad confirmingParty '{confirming_party_str}': {e}",
                cid = created.contract_id
            );
            return;
        }
    };

    // Compute action hash for grouping (JSON serialization is deterministic enough)
    let action_hash = compute_action_hash(&action);

    let confirmation = GovernanceConfirmation {
        contract_id: created.contract_id.clone(),
        action: action.clone(),
        confirming_party,
        created_at: created.created_at.as_ref().map(|t| t.seconds).unwrap_or(0),
    };

    confirmations_by_hash
        .entry(action_hash)
        .or_insert_with(|| (action, Vec::new()))
        .1
        .push(confirmation);
}

/// Extract a domain confirmation (GovernanceConfirmation from governance-core)
/// and add it to the domain confirmations map, grouped by actionProposalCid
fn extract_and_add_domain_confirmation(
    created: &CreatedEvent,
    domain_confirmations: &mut HashMap<String, (String, Vec<GovernanceConfirmation>)>,
) {
    let Some(record) = &created.create_arguments else {
        return;
    };

    // Extract actionProposalCid (ContractId)
    let proposal_cid = record
        .fields
        .iter()
        .find(|f| f.label == "actionProposalCid")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::ContractId(cid)) => Some(cid.clone()),
            _ => None,
        })
        .unwrap_or_default();

    // Extract actionLabel (Text)
    let action_label = record
        .fields
        .iter()
        .find(|f| f.label == "actionLabel")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Text(t)) => Some(t.clone()),
            _ => None,
        })
        .unwrap_or_default();

    // Extract confirmer (Party). Skip the confirmation if missing or
    // malformed (see the off-chain extractor above for the same rationale).
    let Some(confirmer_str) = record
        .fields
        .iter()
        .find(|f| f.label == "confirmer")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Party(p)) => Some(p.clone()),
            _ => None,
        })
    else {
        tracing::warn!(
            "Skipping domain confirmation {cid}: missing confirmer field",
            cid = created.contract_id
        );
        return;
    };
    let confirming_party = match CantonId::parse(&confirmer_str) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "Skipping domain confirmation {cid}: bad confirmer '{confirmer_str}': {e}",
                cid = created.contract_id
            );
            return;
        }
    };

    // Use a dummy ActionType for the GovernanceConfirmation struct (domain confirmations
    // don't have inline actions — they reference a proposal CID instead)
    let confirmation = GovernanceConfirmation {
        contract_id: created.contract_id.clone(),
        action: ActionType::GovernanceSetThreshold { new_threshold: 0 }, // placeholder
        confirming_party,
        created_at: created.created_at.as_ref().map(|t| t.seconds).unwrap_or(0),
    };

    domain_confirmations
        .entry(proposal_cid)
        .or_insert_with(|| (action_label, Vec::new()))
        .1
        .push(confirmation);
}

/// Extract a proposal description from a GovernableAction contract's create_arguments.
///
/// Looks for a `description` field (Text) on the contract. Only captures it if the
/// contract also has a `governanceParty` field (to avoid matching unrelated contracts
/// in wildcard mode).
fn extract_proposal_description(
    created: &CreatedEvent,
    proposal_descriptions: &mut HashMap<String, Option<String>>,
) {
    let Some(record) = &created.create_arguments else {
        return;
    };

    // Only capture contracts that look like GovernableAction proposals
    let has_governance_party = record.fields.iter().any(|f| f.label == "governanceParty");
    let has_proposer = record.fields.iter().any(|f| f.label == "proposer");

    if !has_governance_party || !has_proposer {
        return;
    }

    let description = record
        .fields
        .iter()
        .find(|f| f.label == "description")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Text(t)) => Some(t.clone()),
            _ => None,
        });

    // Always record the cid, even when no description field is present —
    // the consumer relies on map membership to gate active-proposal filtering.
    proposal_descriptions.insert(created.contract_id.clone(), description);
}

/// Fetch proposal descriptions via GovernableAction interface query (production mode).
///
/// Queries active contracts implementing GovernableAction and extracts the
/// `description` field from their create_arguments.
async fn fetch_proposal_descriptions(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
    proposal_descriptions: &mut HashMap<String, Option<String>>,
) -> Result {
    let Some(ref pkg) = packages.governance_core else {
        return Ok(());
    };

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
                identifier_filter: Some(cumulative_filter::IdentifierFilter::InterfaceFilter(
                    InterfaceFilter {
                        interface_id: Some(Identifier {
                            package_id: pkg.clone(),
                            module_name: "Governance.Action".to_string(),
                            entity_name: "GovernableAction".to_string(),
                        }),
                        include_created_event_blob: false,
                        include_interface_view: true,
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
            extract_proposal_description(&created, proposal_descriptions);
        }
    }

    Ok(())
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
    party_id: &CantonId,
    token: Option<String>,
    test_mode: bool,
    packages: &PackageConfig,
) -> Result<Option<GovernanceState>> {
    if test_mode {
        fetch_governance_state_with_wildcard(config, party_id, token).await
    } else {
        // Try each governance template (vault, core) until we find a match
        for template in governance_state_templates(packages) {
            match fetch_governance_state_for_template(config, party_id, token.clone(), &template)
                .await
            {
                Ok(Some(state)) => return Ok(Some(state)),
                Ok(None) => continue,
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("PACKAGE_NAMES_NOT_FOUND") {
                        continue;
                    }
                    tracing::warn!(
                        "Failed to query governance state for {}:{}: {e}",
                        template.module_name,
                        template.entity_name
                    );
                }
            }
        }
        Ok(None)
    }
}

/// Fetch governance state using WildcardFilter (for test mode)
async fn fetch_governance_state_with_wildcard(
    config: &NodeConfig,
    party_id: &CantonId,
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
            // Check if this is a governance rules template (vault or core)
            if let Some(ref template_id) = created.template_id
                && ((template_id.module_name == "BitsafeVault.VaultGovernance"
                    && template_id.entity_name == "VaultGovernanceRules")
                    || (template_id.module_name == "Governance.Rules"
                        && template_id.entity_name == "GovernanceRules"))
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
    party_id: &CantonId,
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

/// Extract governance state from a VaultGovernanceRules or GovernanceRules created event
fn extract_governance_state(created: &CreatedEvent) -> Option<GovernanceState> {
    let record = created.create_arguments.as_ref()?;

    // Extract governance party (vaultManager for vault, governanceParty for core)
    let vault_manager: CantonId = record
        .fields
        .iter()
        .find(|f| f.label == "vaultManager" || f.label == "governanceParty")
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

    // Extract actionConfirmationTimeout
    // VaultGovernanceRules: Optional RelTime; GovernanceRules: RelTime (non-optional)
    let timeout = record
        .fields
        .iter()
        .find(|f| f.label == "actionConfirmationTimeout")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| extract_optional_reltime(v).or_else(|| extract_reltime(v)));

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
    party_id: &CantonId,
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
    party_id: &CantonId,
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
    party_id: &CantonId,
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
    party_id: &CantonId,
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
    party_id: &CantonId,
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
    party_id: &CantonId,
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
    party_id: &CantonId,
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
    party_id: &CantonId,
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
    party_id: &CantonId,
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

// ============================================================================
// Registrar Service Queries
// ============================================================================

/// Get all RegistrarService contracts for a party
pub async fn get_registrar_services(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    test_mode: bool,
    packages: &PackageConfig,
) -> Result<Vec<RegistrarServiceInfo>> {
    if test_mode {
        fetch_registrar_services_with_wildcard(config, party_id, token).await
    } else {
        match registrar_service_template(packages) {
            Some(template) => {
                fetch_registrar_services_for_template(config, party_id, token, &template).await
            }
            None => Ok(Vec::new()),
        }
    }
}

/// Fetch registrar services using WildcardFilter (for test mode)
async fn fetch_registrar_services_with_wildcard(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
) -> Result<Vec<RegistrarServiceInfo>> {
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
            && template_id.module_name == "Utility.Registry.App.V0.Service.Registrar"
            && template_id.entity_name == "RegistrarService"
            && let Some(info) = extract_registrar_service_info(&created)
        {
            services.push(info);
        }
    }

    Ok(services)
}

/// Fetch registrar services using TemplateFilter
async fn fetch_registrar_services_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
) -> Result<Vec<RegistrarServiceInfo>> {
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
            && let Some(info) = extract_registrar_service_info(&created)
        {
            services.push(info);
        }
    }

    Ok(services)
}

/// Extract RegistrarServiceInfo from a RegistrarService created event
fn extract_registrar_service_info(created: &CreatedEvent) -> Option<RegistrarServiceInfo> {
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

    let registrar: CantonId = record
        .fields
        .iter()
        .find(|f| f.label == "registrar")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Party(p)) => p.parse().ok(),
            _ => None,
        })?;

    Some(RegistrarServiceInfo {
        contract_id: created.contract_id.clone(),
        operator,
        registrar,
    })
}

// ============================================================================
// InstrumentConfiguration Queries
// ============================================================================

/// InstrumentConfiguration template identifier. Hard-coded `#utility-registry-v0`
/// because it lives in a different package than `utility_registry`
/// (= `#utility-registry-app-v0`) and PackageConfig has no separate field for
/// it. Canton resolves the `#name-version` selector at query time.
fn instrument_configuration_template() -> TemplateId {
    TemplateId {
        package_id: "#utility-registry-v0".to_string(),
        module_name: "Utility.Registry.V0.Configuration.Instrument",
        entity_name: "InstrumentConfiguration",
    }
}

/// Get all InstrumentConfiguration contracts for a party. Each one represents
/// one token the governance party can mint/burn against.
pub async fn get_instruments(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    test_mode: bool,
) -> Result<Vec<InstrumentInfo>> {
    if test_mode {
        fetch_instruments_with_wildcard(config, party_id, token).await
    } else {
        fetch_instruments_for_template(config, party_id, token, &instrument_configuration_template())
            .await
    }
}

async fn fetch_instruments_with_wildcard(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
) -> Result<Vec<InstrumentInfo>> {
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

    let mut instruments = Vec::new();
    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
            && let Some(template_id) = &created.template_id
            && template_id.module_name == "Utility.Registry.V0.Configuration.Instrument"
            && template_id.entity_name == "InstrumentConfiguration"
            && let Some(info) = extract_instrument_info(&created)
        {
            instruments.push(info);
        }
    }

    Ok(instruments)
}

async fn fetch_instruments_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
) -> Result<Vec<InstrumentInfo>> {
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

    let mut instruments = Vec::new();
    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
            && let Some(info) = extract_instrument_info(&created)
        {
            instruments.push(info);
        }
    }

    Ok(instruments)
}

/// Extract InstrumentInfo from an InstrumentConfiguration created event.
/// Reads `instrument_admin` and `instrument_id` from the contract's
/// `defaultIdentifier` record (fields `source` and `id` respectively, per
/// `Utility.Registry.Holding.V0.Types.InstrumentIdentifier`).
fn extract_instrument_info(created: &CreatedEvent) -> Option<InstrumentInfo> {
    let record = created.create_arguments.as_ref()?;

    let default_identifier = record
        .fields
        .iter()
        .find(|f| f.label == "defaultIdentifier")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Record(r)) => Some(r),
            _ => None,
        })?;

    let instrument_admin: CantonId = default_identifier
        .fields
        .iter()
        .find(|f| f.label == "source")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Party(p)) => p.parse().ok(),
            _ => None,
        })?;

    let instrument_id: String = default_identifier
        .fields
        .iter()
        .find(|f| f.label == "id")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Text(t)) => Some(t.clone()),
            _ => None,
        })?;

    Some(InstrumentInfo {
        contract_id: created.contract_id.clone(),
        instrument_admin,
        instrument_id,
    })
}

// ============================================================================
// Generic Contract ID Query
// ============================================================================

/// Query contracts by template (module_name + entity_name)
///
/// Returns contract IDs with their base64-encoded created_event_blob.
/// Parameters for querying contracts by template or interface
pub struct ContractQueryParams {
    pub package_id: String,
    pub module_name: String,
    pub entity_name: String,
    pub use_interface_filter: bool,
}

/// Uses WildcardFilter in test mode, TemplateFilter or InterfaceFilter in production.
pub async fn query_contracts_by_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    test_mode: bool,
    params: &ContractQueryParams,
) -> Result<Vec<ContractWithBlob>> {
    use base64::Engine;

    let mut state_client = utils::create_state_client(config, token).await?;

    let ledger_end = state_client
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {}))
        .await?
        .into_inner()
        .offset;

    let identifier = Identifier {
        package_id: params.package_id.clone(),
        module_name: params.module_name.clone(),
        entity_name: params.entity_name.clone(),
    };

    let identifier_filter = if test_mode {
        cumulative_filter::IdentifierFilter::WildcardFilter(WildcardFilter {
            include_created_event_blob: true,
        })
    } else if params.use_interface_filter {
        cumulative_filter::IdentifierFilter::InterfaceFilter(InterfaceFilter {
            interface_id: Some(identifier),
            include_interface_view: true,
            include_created_event_blob: true,
        })
    } else {
        cumulative_filter::IdentifierFilter::TemplateFilter(TemplateFilter {
            template_id: Some(identifier),
            include_created_event_blob: true,
        })
    };

    let mut filters_by_party = HashMap::new();
    filters_by_party.insert(
        party_id.to_string(),
        Filters {
            cumulative: vec![CumulativeFilter {
                identifier_filter: Some(identifier_filter),
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

    let mut contracts = Vec::new();
    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
        {
            let matches = if test_mode {
                created.template_id.as_ref().is_some_and(|t| {
                    t.module_name == params.module_name && t.entity_name == params.entity_name
                })
            } else {
                true
            };

            if matches {
                let blob =
                    base64::engine::general_purpose::STANDARD.encode(&created.created_event_blob);
                contracts.push(ContractWithBlob {
                    contract_id: created.contract_id,
                    blob,
                });
            }
        }
    }

    Ok(contracts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ci(name: &str, version: &str, created_at: &str, contract_id: &str) -> ContractInfo {
        ContractInfo {
            contract_id: contract_id.to_string(),
            template_id: format!("Mod:{}", name),
            package_id: format!("pkg-id-of-{name}-{version}"),
            package_name: name.to_string(),
            package_version: version.to_string(),
            created_at: created_at.to_string(),
        }
    }

    #[test]
    fn sort_contracts_by_name_asc_version_desc_created_at_desc() {
        // Arrange — deliberately scrambled order across all three keys, with
        // `alpha 0.1.18` repeated twice (two different created_at) so the
        // dedup keeps only the latest.
        let mut contracts = vec![
            ci("zeta", "1.0.0", "2026-04-30T00:00:00Z", "z-1"),
            ci("alpha", "0.1.7", "2026-04-29T00:00:00Z", "a-1"),
            ci("alpha", "0.1.18", "2026-04-28T00:00:00Z", "a-2"),
            ci("alpha", "0.1.18", "2026-04-30T00:00:00Z", "a-3"),
            ci("beta", "2.0.0", "2026-04-29T00:00:00Z", "b-1"),
        ];

        // Act
        sort_contracts(&mut contracts);

        // Assert — `a-3` (2026-04-30) wins over `a-2` (2026-04-28) within
        // the (alpha, 0.1.18, Mod:alpha) duplicate group.
        let order: Vec<&str> = contracts.iter().map(|c| c.contract_id.as_str()).collect();
        assert_eq!(order, vec!["a-3", "a-1", "b-1", "z-1"]);
    }

    #[test]
    fn sort_contracts_dedups_by_name_version_template_keeping_latest() {
        // Same package+version but DIFFERENT templates → not deduplicated.
        let mut contracts = vec![
            ContractInfo {
                contract_id: "x".to_string(),
                template_id: "Mod:Foo".to_string(),
                package_id: "p".to_string(),
                package_name: "pkg".to_string(),
                package_version: "1.0.0".to_string(),
                created_at: "2026-04-29T00:00:00Z".to_string(),
            },
            ContractInfo {
                contract_id: "y".to_string(),
                template_id: "Mod:Bar".to_string(),
                package_id: "p".to_string(),
                package_name: "pkg".to_string(),
                package_version: "1.0.0".to_string(),
                created_at: "2026-04-28T00:00:00Z".to_string(),
            },
        ];
        sort_contracts(&mut contracts);
        assert_eq!(contracts.len(), 2);
    }

    #[test]
    fn compare_versions_handles_numeric_segments() {
        use std::cmp::Ordering;
        assert_eq!(compare_versions("0.1.18", "0.1.7"), Ordering::Greater);
        assert_eq!(compare_versions("1.0.0", "0.99.99"), Ordering::Greater);
        assert_eq!(compare_versions("1.0.0", "1.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.0", "1.0.0"), Ordering::Less);
    }
}
