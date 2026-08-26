//! Canton ledger query layer.
//!
//! Read-side helpers that query the Canton Ledger API (active contracts,
//! governance state, holdings, transfers, rewards, etc.) and shape the results
//! into the response types served by the HTTP handlers.

use std::{
    collections::HashMap,
    future::Future,
    time::{SystemTime, UNIX_EPOCH},
};

use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        CreatedEvent, GetEventsByContractIdRequest, Identifier, Record, Value,
        admin::{ListKnownPartiesRequest, ListKnownPartiesResponse},
        value,
    },
    digitalasset::canton::admin::participant::v30::{
        ListPackagesRequest, package_service_client::PackageServiceClient,
    },
};
use decman_lib::{
    catalog::interpret::{self, ParsedConfirmation, ParsedDomainConfirmation, ProposalInfo},
    framework::record::{field_numeric, field_party, field_text, field_timestamp},
};

use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
    error::Result,
    utils,
};

use super::{
    event_filters::{interface_filter, party_event_format, template_filter, wildcard_filter},
    ledger_paging::{
        FETCH_CHUNK, fetch_active_contracts_filtered, fetch_first_active_contract,
        for_each_active_contract,
    },
    package_inventory::{
        fetch_package_id_to_name, fetch_package_names, newest_matching_names, package_name_prefix,
    },
    types::{
        ActionType, Claim, ContractInfo, ContractWithBlob, CredentialInfo, CredentialOfferInfo,
        DomainGovernanceAction, GovernanceAction, GovernanceConfirmation, GovernanceState,
        HoldingInfo, InstrumentInfo, PartyMetadata, PendingAction, ProviderConfigurationInfo,
        ProviderServiceInfo, RegistrarServiceInfo, RegistrarServiceRequestInfo, TokenRequestInfo,
        TransferFactoryInfo, TransferInstructionInfo, TransferInstructionStatus, UserServiceInfo,
        VaultInfo,
    },
};

/// Template identifier for Daml contracts
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
fn credential_offer_template(packages: &PackageConfig) -> Option<TemplateId> {
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
fn credential_template(packages: &PackageConfig) -> Option<TemplateId> {
    packages.utility_credential.as_ref().map(|pkg| TemplateId {
        package_id: pkg.clone(),
        module_name: "Utility.Credential.V0.Credential",
        entity_name: "Credential",
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

/// RegistrarServiceRequest template identifier
fn registrar_service_request_template(packages: &PackageConfig) -> Option<TemplateId> {
    packages.utility_registry.as_ref().map(|pkg| TemplateId {
        package_id: pkg.clone(),
        module_name: "Utility.Registry.App.V0.Service.Registrar",
        entity_name: "RegistrarServiceRequest",
    })
}

/// ProviderConfiguration template identifier
fn provider_configuration_template(packages: &PackageConfig) -> Option<TemplateId> {
    packages.utility_registry.as_ref().map(|pkg| TemplateId {
        package_id: pkg.clone(),
        module_name: "Utility.Registry.App.V0.Configuration.Provider",
        entity_name: "ProviderConfiguration",
    })
}
/// Get active contracts for a party
///
/// Queries each template separately, so a package that is not deployed on this
/// participant degrades to "no contracts of that type" rather than failing the
/// whole read.
pub async fn get_contracts(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
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

    {
        // One query per template, so a package missing from this participant
        // degrades to "no contracts of that type" instead of failing the read.
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

pub(crate) fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
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
    let mut client = PackageServiceClient::new(config.admin_channel().await?);
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

/// Fetch contracts for a specific template
async fn fetch_contracts_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
    package_versions: &HashMap<String, String>,
    contracts: &mut Vec<ContractInfo>,
) -> Result {
    let event_format = party_event_format(
        party_id,
        vec![template_filter(
            Identifier {
                package_id: template.package_id.to_string(),
                module_name: template.module_name.to_string(),
                entity_name: template.entity_name.to_string(),
            },
            false,
        )],
        false,
    );

    for_each_active_contract(config, token, event_format, |created| {
        contracts.push(render_contract_info(&created, package_versions));
    })
    .await?;

    Ok(())
}

/// Get party metadata from Ledger API
pub async fn get_party_metadata(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
) -> Result<Option<PartyMetadata>> {
    let client = utils::create_party_client(config, token).await?;
    let party_id_str = party_id.to_string();

    // `filter_party` is a server-side prefix match, so a full party id narrows
    // this to the one party we want. Paging is still walked because the prefix
    // can in principle match more than one id, and a participant hosting more
    // parties than one page holds would otherwise silently report no metadata.
    //
    // `FETCH_CHUNK` rather than the wire `PAGE_SIZE`: this is an internal
    // full-collection read, and on a participant that ignores `filter_party`
    // the wire size would turn the walk into a round trip per 25 parties.
    find_party_annotations(&party_id_str, |page_token| {
        let request = ListKnownPartiesRequest {
            identity_provider_id: String::new(),
            page_token,
            page_size: FETCH_CHUNK,
            filter_party: party_id_str.clone(),
        };
        let mut client = client.clone();

        async move {
            Ok(client
                .list_known_parties(tonic::Request::new(request))
                .await?
                .into_inner())
        }
    })
    .await
}

/// Walk `ListKnownParties` pages for `party_id`, returning its annotations.
///
/// `fetch_page` takes the token of the page to read and is a parameter so the
/// walk can be tested; production passes the Ledger API call.
async fn find_party_annotations<F, Fut>(
    party_id: &str,
    mut fetch_page: F,
) -> Result<Option<PartyMetadata>>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<ListKnownPartiesResponse>>,
{
    let mut page_token = String::new();

    loop {
        let response = fetch_page(page_token.clone()).await?;

        if let Some(details) = response.party_details.iter().find(|p| p.party == party_id) {
            let annotations = details
                .local_metadata
                .as_ref()
                .map(|m| m.annotations.clone())
                .unwrap_or_default();

            return if annotations.is_empty() {
                Ok(None)
            } else {
                Ok(Some(PartyMetadata { annotations }))
            };
        }

        // A repeated token means the server is not advancing; treating it as the
        // end keeps a misbehaving participant from walking forever.
        if response.next_page_token.is_empty() || response.next_page_token == page_token {
            return Ok(None);
        }
        page_token = response.next_page_token;
    }
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
    packages: &PackageConfig,
) -> Result<(Vec<GovernanceAction>, Vec<DomainGovernanceAction>)> {
    // Collect confirmations grouped by action hash (vault + core self-management)
    let mut confirmations_by_hash: HashMap<String, (ActionType, Vec<ParsedConfirmation>)> =
        HashMap::new();
    // Collect domain confirmations grouped by proposal CID (core domain actions)
    let mut domain_confirmations: HashMap<String, (String, Vec<ParsedDomainConfirmation>)> =
        HashMap::new();
    // Map of `contract_id -> ProposalInfo` for every active
    // `GovernableAction` proposal visible to this party on this participant.
    // The presence of a key here is what gates inclusion in `domain_actions`
    // below — `Confirmation`s referencing a proposal that's no longer active
    // (or never reached this participant's ACS) get filtered out, otherwise
    // surfacing them in the notification queue gives the user a Confirm
    // button that always 500s with `CONTRACT_NOT_FOUND` on the proposal cid.
    let mut proposal_infos: HashMap<String, ProposalInfo> = HashMap::new();
    // Whether `proposal_infos` reflects the full active-proposal set
    // for this party on this participant. If the proposal fetch errored we
    // can't tell orphans apart from "we just couldn't read the proposals", so
    // we skip orphan-marking below to avoid surfacing a flood of false
    // orphans to the user.
    let mut proposal_infos_complete = true;
    // Whether `domain_confirmations` reflects every active `GovernanceConfirmation`
    // for this party. A confirmation query that fails leaves a confirmed
    // proposal looking untouched, and synthesizing it as a zero-confirmation
    // card would offer Confirm to a member who has already confirmed. Skip
    // synthesis in that case and wait for a refresh that reads cleanly.
    let mut domain_confirmations_complete = true;

    tracing::debug!("Using TemplateFilter for governance query (per-template)");
    for t in &decman_lib::catalog::templates::governance_templates(packages) {
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
                tracing::debug!("Successfully queried {}:{}", t.module, t.entity);
            }
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("PACKAGE_NAMES_NOT_FOUND") {
                    tracing::debug!(
                        "Package {} not found, skipping {}:{}",
                        t.package_ref,
                        t.module,
                        t.entity
                    );
                } else {
                    tracing::warn!(
                        "Failed to query {}:{}: {e}, continuing...",
                        t.module,
                        t.entity
                    );
                    domain_confirmations_complete = false;
                }
            }
        }
    }
    // Fetch proposal infos via GovernableAction interface query
    if let Err(e) =
        fetch_proposal_infos(config, party_id, token, packages, &mut proposal_infos).await
    {
        // Warn, not debug: this drops every unconfirmed card from the page,
        // and the operator needs to know why the queue looks empty.
        tracing::warn!("Could not fetch proposal infos: {e}");
        proposal_infos_complete = false;
    }

    let now_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Convert to GovernanceAction list, deduplicating confirmations by confirming_party
    let actions: Vec<GovernanceAction> = confirmations_by_hash
        .into_iter()
        .map(|(action_hash, (action, confirmations))| {
            // Newest-first per member, then Daml's `expiresAt > now` filter so
            // the UI doesn't offer an Execute that chain will reject.
            let unique_confirmations = interpret::dedupe_newest_per_party(
                confirmations,
                |c| &c.confirming_party,
                |c| c.created_at,
            );
            let confirmation_count =
                interpret::live_count(&unique_confirmations, |c| c.expires_at, now_seconds);
            let last_confirmation_at = unique_confirmations
                .iter()
                .map(|c| c.created_at)
                .max()
                .unwrap_or(0);
            GovernanceAction {
                action_hash,
                action,
                confirmations: unique_confirmations
                    .into_iter()
                    .map(confirmation_dto)
                    .collect(),
                confirmation_count,
                can_execute: confirmation_count >= threshold,
                last_confirmation_at,
            }
        })
        .collect();

    let domain_actions: Vec<DomainGovernanceAction> = interpret::assemble_domain_actions(
        domain_confirmations,
        proposal_infos,
        proposal_infos_complete,
        domain_confirmations_complete,
        threshold,
        now_seconds,
    )
    .into_iter()
    .map(|action| DomainGovernanceAction {
        proposal_cid: action.proposal_cid,
        action_label: action.action_label,
        description: action.description,
        confirmations: action
            .confirmations
            .into_iter()
            .map(domain_confirmation_dto)
            .collect(),
        confirmation_count: action.confirmation_count,
        can_execute: action.can_execute,
        orphaned: action.orphaned,
        transfer_details: action.transfer_details,
        accept_transfer_details: action.accept_transfer_details,
        service_request_details: action.service_request_details,
        proposer: action.proposer,
        created_at: action.created_at,
    })
    .collect();

    Ok((actions, domain_actions))
}

/// Map a parsed confirmation onto the wire DTO.
fn confirmation_dto(parsed: ParsedConfirmation) -> GovernanceConfirmation {
    GovernanceConfirmation {
        contract_id: parsed.contract_id,
        action: parsed.action,
        confirming_party: parsed.confirming_party,
        created_at: parsed.created_at,
        expires_at: parsed.expires_at,
    }
}

/// The wire DTO requires an action; the on-ledger domain confirmation has
/// none. Keep the legacy placeholder EXACTLY (wire compatibility) until the
/// API drops it.
fn domain_confirmation_dto(parsed: ParsedDomainConfirmation) -> GovernanceConfirmation {
    GovernanceConfirmation {
        contract_id: parsed.contract_id,
        action: ActionType::GovernanceSetThreshold { new_threshold: 0 },
        confirming_party: parsed.confirming_party,
        created_at: parsed.created_at,
        expires_at: parsed.expires_at,
    }
}

/// Fetch governance confirmations for a specific template
async fn fetch_governance_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &decman_lib::framework::TemplateId,
    confirmations_by_hash: &mut HashMap<String, (ActionType, Vec<ParsedConfirmation>)>,
    domain_confirmations: &mut HashMap<String, (String, Vec<ParsedDomainConfirmation>)>,
) -> Result {
    let event_format = party_event_format(
        party_id,
        vec![template_filter(template.into(), false)],
        true,
    );

    for_each_active_contract(config, token, event_format, |created| {
        if created.template_id.as_ref().is_some_and(|t| {
            t.module_name == "Governance.Confirmation" && t.entity_name == "GovernanceConfirmation"
        }) {
            // Domain confirmations carry no inline action — they group by the
            // proposal cid they reference, labelled by whichever confirmation
            // for that proposal arrived first.
            if let Some(parsed) = interpret::parse_domain_confirmation(&created) {
                domain_confirmations
                    .entry(parsed.proposal_cid.clone())
                    .or_insert_with(|| (parsed.action_label.clone(), Vec::new()))
                    .1
                    .push(parsed);
            }
        } else if let Some(parsed) = interpret::parse_confirmation(&created) {
            // By-value confirmations group by a deterministic hash of the
            // action they carry — the hash is decman's, not the lib's.
            let action_hash = compute_action_hash(&parsed.action);
            let action = parsed.action.clone();
            confirmations_by_hash
                .entry(action_hash)
                .or_insert_with(|| (action, Vec::new()))
                .1
                .push(parsed);
        }
    })
    .await?;

    Ok(())
}

/// Resolve each `TransferInstruction` cid captured on
/// `AcceptTransferProposal`s into an `AcceptTransferDetails` and store it on
/// the corresponding `ProposalInfo`. Skips silently per-cid on failure — the
/// card just falls back to its cid-only rendering rather than blocking the
/// whole confirmations response on one bad instruction.
async fn resolve_accept_transfer_details(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    proposal_infos: &mut HashMap<String, ProposalInfo>,
) -> Result {
    let pending: Vec<(String, String)> = proposal_infos
        .iter()
        .filter_map(|(proposal_cid, info)| {
            if info.accept_transfer.is_some() {
                return None;
            }
            info.accept_transfer_instruction_cid
                .as_ref()
                .map(|cid| (proposal_cid.clone(), cid.clone()))
        })
        .collect();
    if pending.is_empty() {
        return Ok(());
    }

    let mut client = utils::create_event_query_client(config, token).await?;

    for (proposal_cid, instruction_cid) in pending {
        let request = GetEventsByContractIdRequest {
            contract_id: instruction_cid.clone(),
            event_format: Some(party_event_format(
                party_id,
                vec![interface_filter(
                    Identifier {
                        package_id: "#splice-api-token-transfer-instruction-v1".to_string(),
                        module_name: "Splice.Api.Token.TransferInstructionV1".to_string(),
                        entity_name: "TransferInstruction".to_string(),
                    },
                    false,
                )],
                true,
            )),
        };
        let created_event = match client
            .get_events_by_contract_id(tonic::Request::new(request))
            .await
        {
            Ok(resp) => resp.into_inner().created.and_then(|c| c.created_event),
            Err(e) => {
                tracing::debug!(
                    "Could not resolve TransferInstruction {instruction_cid} for proposal \
                     {proposal_cid}: {e}"
                );
                continue;
            }
        };
        let Some(created_event) = created_event else {
            continue;
        };
        if let Some(details) = interpret::extract_accept_transfer_details_from_view(&created_event)
            && let Some(info) = proposal_infos.get_mut(&proposal_cid)
        {
            info.accept_transfer = Some(details);
        }
    }
    Ok(())
}

/// Fetch proposal infos via GovernableAction interface query.
///
/// Queries active contracts implementing GovernableAction and extracts the
/// `description` field plus, where applicable, the `TransferProposal`'s
/// recipient/amount/instrument from their create_arguments.
async fn fetch_proposal_infos(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
    proposal_infos: &mut HashMap<String, ProposalInfo>,
) -> Result {
    let Ok(template) = decman_lib::catalog::templates::governable_action_interface(packages) else {
        return Ok(());
    };

    let event_format = party_event_format(
        party_id,
        vec![interface_filter((&template).into(), false)],
        true,
    );

    for_each_active_contract(config, token.clone(), event_format, |created| {
        if let Some((proposal_cid, info)) = interpret::extract_proposal_info(&created, party_id) {
            proposal_infos.insert(proposal_cid, info);
        }
    })
    .await?;

    // Resolve the linked `TransferInstruction` for any
    // `AcceptTransferProposal`s we just captured so the notification card has
    // sender/amount/instrument to render. Errors per-cid are logged and
    // swallowed inside the resolver; an outer error here would only come from
    // a client-creation failure, which we let propagate.
    resolve_accept_transfer_details(config, party_id, token, proposal_infos).await?;

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
    packages: &PackageConfig,
) -> Result<Option<GovernanceState>> {
    // Try each governance template (vault, core) until we find a match
    for template in decman_lib::catalog::templates::governance_state_templates(packages) {
        match fetch_governance_state_for_template(config, party_id, token.clone(), &template).await
        {
            Ok(Some(mut state)) => {
                // Found under the configured package — not out of date.
                state.package_ref = Some(template.package_ref.clone());
                state.out_of_date = false;
                return Ok(Some(state));
            }
            Ok(None) => continue,
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("PACKAGE_NAMES_NOT_FOUND") {
                    continue;
                }
                tracing::warn!(
                    "Failed to query governance state for {}:{}: {e}",
                    template.module,
                    template.entity
                );
            }
        }
    }
    // Nothing under the configured packages — look for a GovernanceRules
    // contract under an older governance-core package version still
    // uploaded to the participant.
    fetch_governance_state_fallback(config, party_id, token, packages).await
}

/// Look for a GovernanceRules contract under any OLDER governance-core
/// package version present on the participant. Runs only after the
/// configured templates yielded nothing; returns the newest match tagged
/// `out_of_date` with the package ref it actually lives under.
async fn fetch_governance_state_fallback(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
) -> Result<Option<GovernanceState>> {
    let Some(configured) = packages.governance_core.as_deref() else {
        return Ok(None);
    };
    let names = match fetch_package_names(config).await {
        Ok(names) => names,
        Err(e) => {
            tracing::warn!("Fallback gov-core discovery: cannot list packages: {e:#}");
            return Ok(None);
        }
    };
    let prefix = package_name_prefix(configured);
    let configured_name = configured.trim_start_matches('#');
    for name in newest_matching_names(&names, &prefix) {
        // The configured name was already tried by the caller.
        if name == configured_name {
            continue;
        }
        let template = decman_lib::framework::TemplateId::new(
            format!("#{name}"),
            "Governance.Rules",
            "GovernanceRules",
        );
        match fetch_governance_state_for_template(config, party_id, token.clone(), &template).await
        {
            Ok(Some(mut state)) => {
                tracing::warn!(
                    "GovernanceRules contract for {party_id} found under fallback package \
                     #{name} (configured {configured}); flagging as out of date"
                );
                state.package_ref = Some(template.package_ref);
                state.out_of_date = true;
                return Ok(Some(state));
            }
            Ok(None) => continue,
            Err(e) => {
                if !e.to_string().contains("PACKAGE_NAMES_NOT_FOUND") {
                    tracing::warn!("Fallback gov-core query for #{name} failed: {e}");
                }
                continue;
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
    template: &decman_lib::framework::TemplateId,
) -> Result<Option<GovernanceState>> {
    let event_format = party_event_format(
        party_id,
        vec![template_filter(template.into(), false)],
        true,
    );

    Ok(fetch_first_active_contract(config, token, event_format)
        .await?
        .as_ref()
        .and_then(interpret::extract_governance_state)
        .map(|rules| GovernanceState {
            contract_id: rules.contract_id,
            vault_manager: rules.governance_party,
            members: rules.members,
            threshold: rules.threshold,
            action_confirmation_timeout_microseconds: rules.timeout_micros,
            // Both callers overwrite these with the package the contract was
            // actually found under, so the parse says nothing about them.
            package_ref: None,
            out_of_date: false,
        }))
}

/// Resolve the package-name ref (`#name`) of the package an on-ledger
/// contract was actually created under. Used to exercise choices on
/// governance contracts that may live under an older package version than
/// the configured one. Returns `fallback` (the configured ref) on any
/// failure so callers degrade to the previous behavior instead of erroring.
pub(crate) async fn resolve_contract_package_ref(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    contract_id: &str,
    fallback: &str,
) -> String {
    match fetch_contract_package_ref(config, party_id, token, contract_id).await {
        Ok(Some(package_ref)) => package_ref,
        Ok(None) => {
            tracing::debug!(
                "Could not resolve package ref for {contract_id}; using configured {fallback}"
            );
            fallback.to_string()
        }
        Err(e) => {
            tracing::debug!(
                "Could not resolve package ref for {contract_id}: {e}; \
                 using configured {fallback}"
            );
            fallback.to_string()
        }
    }
}

/// Look up a contract's created event and map its concrete package id back
/// to a `#name` ref via the participant's package inventory.
async fn fetch_contract_package_ref(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    contract_id: &str,
) -> Result<Option<String>> {
    let mut client = utils::create_event_query_client(config, token).await?;

    let request = GetEventsByContractIdRequest {
        contract_id: contract_id.to_string(),
        event_format: Some(party_event_format(
            party_id,
            vec![wildcard_filter(false)],
            false,
        )),
    };

    let package_id = client
        .get_events_by_contract_id(tonic::Request::new(request))
        .await?
        .into_inner()
        .created
        .and_then(|c| c.created_event)
        .and_then(|e| e.template_id)
        .map(|t| t.package_id);
    let Some(package_id) = package_id else {
        return Ok(None);
    };
    // Already a `#name` ref — use it directly.
    if package_id.starts_with('#') {
        return Ok(Some(package_id));
    }
    let id_to_name = fetch_package_id_to_name(config).await?;
    Ok(id_to_name.get(&package_id).map(|name| format!("#{name}")))
}

// ============================================================================
// Vault Contracts Query
// ============================================================================

/// Get all Vault contracts for a party
pub async fn get_vaults(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
) -> Result<Vec<VaultInfo>> {
    match vault_template(packages) {
        Some(template) => fetch_vaults_for_template(config, party_id, token, &template).await,
        None => Ok(Vec::new()),
    }
}

/// Fetch vaults using TemplateFilter
async fn fetch_vaults_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
) -> Result<Vec<VaultInfo>> {
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

    fetch_active_contracts_filtered(config, token, event_format, |created| {
        extract_vault_info(&created)
    })
    .await
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
    packages: &PackageConfig,
) -> Result<Vec<ProviderServiceInfo>> {
    match provider_service_template(packages) {
        Some(template) => {
            fetch_provider_services_for_template(config, party_id, token, &template).await
        }
        None => Ok(Vec::new()),
    }
}

/// Fetch provider services using TemplateFilter
async fn fetch_provider_services_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
) -> Result<Vec<ProviderServiceInfo>> {
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

    fetch_active_contracts_filtered(config, token, event_format, |created| {
        extract_provider_service_info(&created)
    })
    .await
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
    packages: &PackageConfig,
) -> Result<Vec<UserServiceInfo>> {
    match user_service_template(packages) {
        Some(template) => {
            fetch_user_services_for_template(config, party_id, token, &template).await
        }
        None => Ok(Vec::new()),
    }
}

/// Fetch user services using TemplateFilter
async fn fetch_user_services_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
) -> Result<Vec<UserServiceInfo>> {
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

    fetch_active_contracts_filtered(config, token, event_format, |created| {
        extract_user_service_info(&created)
    })
    .await
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
// Credential Offer Queries
// ============================================================================

/// Get all CredentialOffer contracts visible to a party. Includes offers in
/// both directions (party as `holder` or as `issuer`); the caller filters for
/// the side it needs.
pub async fn get_credential_offers(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
) -> Result<Vec<CredentialOfferInfo>> {
    match credential_offer_template(packages) {
        Some(template) => {
            fetch_credential_offers_for_template(config, party_id, token, &template).await
        }
        None => Ok(Vec::new()),
    }
}

/// Fetch credential offers using TemplateFilter
async fn fetch_credential_offers_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
) -> Result<Vec<CredentialOfferInfo>> {
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

    fetch_active_contracts_filtered(config, token, event_format, |created| {
        extract_credential_offer_info(&created)
    })
    .await
}

/// Extract CredentialOfferInfo from a CredentialOffer created event. An offer
/// is free when its `billingParams : Optional BillingParams` field is `None` —
/// only those can be taken via `CredentialOffer_AcceptFree`.
fn extract_credential_offer_info(created: &CreatedEvent) -> Option<CredentialOfferInfo> {
    let record = created.create_arguments.as_ref()?;

    let operator: CantonId = field_party(record, "operator")?.parse().ok()?;
    let issuer: CantonId = field_party(record, "issuer")?.parse().ok()?;
    let holder: CantonId = field_party(record, "holder")?.parse().ok()?;
    let credential_id = field_text(record, "id")?;
    let description = field_text(record, "description").unwrap_or_default();

    let has_billing_params = record
        .fields
        .iter()
        .find(|f| f.label == "billingParams")
        .and_then(|f| f.value.as_ref())
        .is_some_and(|v| match &v.sum {
            Some(value::Sum::Optional(opt)) => opt.value.is_some(),
            _ => false,
        });

    Some(CredentialOfferInfo {
        contract_id: created.contract_id.clone(),
        operator,
        issuer,
        holder,
        credential_id,
        description,
        is_free: !has_billing_params,
    })
}

/// Get all Credential contracts visible to a party
pub async fn get_credentials(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
) -> Result<Vec<CredentialInfo>> {
    match credential_template(packages) {
        Some(template) => fetch_credentials_for_template(config, party_id, token, &template).await,
        None => Ok(Vec::new()),
    }
}

/// Fetch credentials using TemplateFilter
async fn fetch_credentials_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
) -> Result<Vec<CredentialInfo>> {
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

    fetch_active_contracts_filtered(config, token, event_format, |created| {
        extract_credential_info(&created)
    })
    .await
}

/// Extract CredentialInfo from a Credential created event.
fn extract_credential_info(created: &CreatedEvent) -> Option<CredentialInfo> {
    let record = created.create_arguments.as_ref()?;

    let issuer: CantonId = field_party(record, "issuer")?.parse().ok()?;
    let holder: CantonId = field_party(record, "holder")?.parse().ok()?;
    let credential_id = field_text(record, "id")?;
    let description = field_text(record, "description").unwrap_or_default();

    let claims = record
        .fields
        .iter()
        .find(|f| f.label == "claims")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::List(l)) => Some(&l.elements),
            _ => None,
        })
        .map(|elements| {
            elements
                .iter()
                .filter_map(|v| match &v.sum {
                    Some(value::Sum::Record(r)) => Some(Claim {
                        subject: field_text(r, "subject")?,
                        property: field_text(r, "property")?,
                        value: field_text(r, "value")?,
                    }),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    Some(CredentialInfo {
        contract_id: created.contract_id.clone(),
        issuer,
        holder,
        credential_id,
        description,
        claims,
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
    packages: &PackageConfig,
) -> Result<Vec<RegistrarServiceInfo>> {
    match registrar_service_template(packages) {
        Some(template) => {
            fetch_registrar_services_for_template(config, party_id, token, &template).await
        }
        None => Ok(Vec::new()),
    }
}

/// Fetch registrar services using TemplateFilter
async fn fetch_registrar_services_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
) -> Result<Vec<RegistrarServiceInfo>> {
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

    fetch_active_contracts_filtered(config, token, event_format, |created| {
        extract_registrar_service_info(&created)
    })
    .await
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
// Registrar Service Request Queries
// ============================================================================

/// Get all RegistrarServiceRequest contracts visible to a party. The
/// OnboardRegistrar form lists these so the request backing the onboard can
/// be picked instead of pasted in by hand.
pub async fn get_registrar_service_requests(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
) -> Result<Vec<RegistrarServiceRequestInfo>> {
    match registrar_service_request_template(packages) {
        Some(template) => {
            fetch_registrar_service_requests_for_template(config, party_id, token, &template).await
        }
        None => Ok(Vec::new()),
    }
}

/// Fetch registrar service requests using TemplateFilter
async fn fetch_registrar_service_requests_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
) -> Result<Vec<RegistrarServiceRequestInfo>> {
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

    fetch_active_contracts_filtered(config, token, event_format, |created| {
        extract_registrar_service_request_info(&created)
    })
    .await
}

/// Read an `Optional Bool` field. An absent field or a `None` value reads as
/// `false`, matching the SDK's treatment of the request's flags.
fn field_optional_bool_or_false(record: &Record, label: &str) -> bool {
    record
        .fields
        .iter()
        .find(|f| f.label == label)
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Optional(opt)) => {
                opt.value.as_deref().and_then(|inner| match &inner.sum {
                    Some(value::Sum::Bool(b)) => Some(*b),
                    _ => None,
                })
            }
            _ => None,
        })
        .unwrap_or(false)
}

/// Extract RegistrarServiceRequestInfo from a RegistrarServiceRequest
/// created event.
fn extract_registrar_service_request_info(
    created: &CreatedEvent,
) -> Option<RegistrarServiceRequestInfo> {
    let record = created.create_arguments.as_ref()?;

    let operator: CantonId = field_party(record, "operator")?.parse().ok()?;
    let provider: CantonId = field_party(record, "provider")?.parse().ok()?;
    let registrar: CantonId = field_party(record, "registrar")?.parse().ok()?;

    Some(RegistrarServiceRequestInfo {
        contract_id: created.contract_id.clone(),
        operator,
        provider,
        registrar,
        create_transfer_rule: field_optional_bool_or_false(record, "createTransferRule"),
        create_allocation_factory: field_optional_bool_or_false(record, "createAllocationFactory"),
    })
}

// ============================================================================
// Provider Configuration Queries
// ============================================================================

/// Get all ProviderConfiguration contracts visible to a party. The
/// OnboardRegistrar form lists these so the configuration backing the
/// onboard can be picked instead of pasted in by hand.
pub async fn get_provider_configurations(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
) -> Result<Vec<ProviderConfigurationInfo>> {
    match provider_configuration_template(packages) {
        Some(template) => {
            fetch_provider_configurations_for_template(config, party_id, token, &template).await
        }
        None => Ok(Vec::new()),
    }
}

/// Fetch provider configurations using TemplateFilter
async fn fetch_provider_configurations_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
) -> Result<Vec<ProviderConfigurationInfo>> {
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

    fetch_active_contracts_filtered(config, token, event_format, |created| {
        extract_provider_configuration_info(&created)
    })
    .await
}

/// Extract ProviderConfigurationInfo from a ProviderConfiguration created
/// event. The requirement lists stay behind: the picker labels
/// configurations by contract id alone.
fn extract_provider_configuration_info(
    created: &CreatedEvent,
) -> Option<ProviderConfigurationInfo> {
    let record = created.create_arguments.as_ref()?;

    let operator: CantonId = field_party(record, "operator")?.parse().ok()?;
    let provider: CantonId = field_party(record, "provider")?.parse().ok()?;

    Some(ProviderConfigurationInfo {
        contract_id: created.contract_id.clone(),
        operator,
        provider,
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
) -> Result<Vec<InstrumentInfo>> {
    fetch_instruments_for_template(
        config,
        party_id,
        token,
        &instrument_configuration_template(),
    )
    .await
}

async fn fetch_instruments_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
) -> Result<Vec<InstrumentInfo>> {
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

    fetch_active_contracts_filtered(config, token, event_format, |created| {
        extract_instrument_info(&created)
    })
    .await
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
    /// When true, drop contracts whose `executeBefore` field is already in
    /// the past. No-op for templates that don't carry an `executeBefore`.
    pub active_only: bool,
}

/// Uses TemplateFilter or InterfaceFilter, chosen by `params.use_interface_filter`.
pub async fn query_contracts_by_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    params: &ContractQueryParams,
) -> Result<Vec<ContractWithBlob>> {
    use base64::Engine;

    let identifier = Identifier {
        package_id: params.package_id.clone(),
        module_name: params.module_name.clone(),
        entity_name: params.entity_name.clone(),
    };

    let filter = if params.use_interface_filter {
        interface_filter(identifier, true)
    } else {
        template_filter(identifier, true)
    };

    let event_format = party_event_format(party_id, vec![filter], true);

    fetch_active_contracts_filtered(config, token, event_format, |created| {
        // QA flagged the Accept Mint Request dropdown for surfacing contracts
        // whose `executeBefore` has already passed — accepting them would fail
        // at interpretation with deadline-exceeded. Drop them here when the
        // caller opts in.
        if params.active_only && is_execute_before_expired(&created) {
            return None;
        }

        let blob = base64::engine::general_purpose::STANDARD.encode(&created.created_event_blob);
        Some(ContractWithBlob {
            contract_id: created.contract_id,
            blob,
        })
    })
    .await
}

// ============================================================================
// Token-standard TransferInstruction Query (for Accept Transfer dropdown)
// ============================================================================

/// `TransferInstructionStatus` constructor names — see
/// `Splice.Api.Token.TransferInstructionV1` in the token-standard package.
/// Lifted here so a grep surfaces every place that depends on the spelling.
const TRANSFER_PENDING_RECEIVER_ACCEPTANCE: &str = "TransferPendingReceiverAcceptance";
const TRANSFER_PENDING_INTERNAL_WORKFLOW: &str = "TransferPendingInternalWorkflow";

/// Fetch open `TransferInstruction` contracts (status
/// `TransferPendingReceiverAcceptance`) whose `receiver` is `party_id`.
///
/// The token-standard registry models `TransferInstruction` as an interface
/// (`Splice.Api.Token.TransferInstructionV1:TransferInstruction`), so this
/// uses an `InterfaceFilter` and reads the computed `TransferInstructionView`
/// to surface sender / receiver / amount / instrument for the UI dropdown.
pub async fn get_open_transfer_instructions(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
) -> Result<Vec<TransferInstructionInfo>> {
    let event_format = party_event_format(
        party_id,
        vec![interface_filter(
            Identifier {
                package_id: "#splice-api-token-transfer-instruction-v1".to_string(),
                module_name: "Splice.Api.Token.TransferInstructionV1".to_string(),
                entity_name: "TransferInstruction".to_string(),
            },
            false,
        )],
        true,
    );

    let receiver_str = party_id.to_string();

    // The InterfaceFilter only enforces party visibility — this party can see
    // the contract as sender, receiver, or an instrument-admin stakeholder.
    // Keep only the ones where it's the *receiver*, since those are the only
    // ones it can Accept.
    fetch_active_contracts_filtered(config, token, event_format, |created| {
        extract_transfer_instruction_info(&created)
            .filter(|info| info.receiver.to_string() == receiver_str)
    })
    .await
}

/// Pull sender / receiver / amount / instrument out of a `TransferInstruction`
/// interface view. Returns `None` if the view is missing, the status is not
/// `TransferPendingReceiverAcceptance`, or any expected field is absent.
fn extract_transfer_instruction_info(created: &CreatedEvent) -> Option<TransferInstructionInfo> {
    // The view is delivered under `interface_views` (not `create_arguments`).
    // Pick the first one matching the TransferInstruction interface; there's
    // typically only one for this filter shape.
    let view = created.interface_views.iter().find(|v| {
        v.interface_id.as_ref().is_some_and(|id| {
            id.module_name == "Splice.Api.Token.TransferInstructionV1"
                && id.entity_name == "TransferInstruction"
        })
    })?;
    let view_record = view.view_value.as_ref()?;

    // Surface both pending-acceptance (immediately acceptable) and
    // pending-internal-workflow (blocked on an admin/registrar action). The UI
    // disables the latter with a "Pending: <party> — <action>" subtitle so
    // operators see the offer exists instead of getting silent "no offers".
    let status_value = view_record
        .fields
        .iter()
        .find(|f| f.label == "status")
        .and_then(|f| f.value.as_ref())?;
    let status_variant = match &status_value.sum {
        Some(value::Sum::Variant(v)) => v,
        _ => return None,
    };
    let (status, pending_actions) = match status_variant.constructor.as_str() {
        TRANSFER_PENDING_RECEIVER_ACCEPTANCE => (
            TransferInstructionStatus::PendingReceiverAcceptance,
            Vec::new(),
        ),
        TRANSFER_PENDING_INTERNAL_WORKFLOW => {
            let actions = status_variant
                .value
                .as_ref()
                .and_then(|v| match &v.sum {
                    Some(value::Sum::Record(r)) => Some(r),
                    _ => None,
                })
                .and_then(|r| r.fields.iter().find(|f| f.label == "pendingActions"))
                .and_then(|f| f.value.as_ref())
                .map(extract_pending_actions)
                .unwrap_or_default();
            (TransferInstructionStatus::PendingInternalWorkflow, actions)
        }
        _ => return None,
    };

    let transfer_record = view_record
        .fields
        .iter()
        .find(|f| f.label == "transfer")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Record(r)) => Some(r),
            _ => None,
        })?;

    transfer_instruction_from_transfer(created, transfer_record, status, pending_actions)
}

/// Read the token-standard `Transfer` record shared by every transfer instruction.
///
/// The utility registry supplies it inside the `TransferInstruction` interface view;
/// Canton Coin supplies the same shape in the template's own create arguments. One
/// parser reads both, so the two paths cannot drift.
fn transfer_instruction_from_transfer(
    created: &CreatedEvent,
    transfer_record: &Record,
    status: TransferInstructionStatus,
    pending_actions: Vec<PendingAction>,
) -> Option<TransferInstructionInfo> {
    // Surface the deadline so the UI can disable past-deadline rows; do *not*
    // hide them. Accepting an expired offer would fail at interpretation with
    // `deadline-exceeded`, but staying silent left users wondering where their
    // offers went — surface them as disabled "expired" entries instead.
    let expires_at = field_timestamp(transfer_record, "executeBefore")? / 1_000_000;

    let sender: CantonId = field_party(transfer_record, "sender")?.parse().ok()?;
    let receiver: CantonId = field_party(transfer_record, "receiver")?.parse().ok()?;
    let amount =
        field_numeric(transfer_record, "amount").and_then(|s| DamlDecimal::parse(&s).ok())?;

    let instrument_record = transfer_record
        .fields
        .iter()
        .find(|f| f.label == "instrumentId")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Record(r)) => Some(r),
            _ => None,
        })?;
    let instrument_admin: CantonId = field_party(instrument_record, "admin")?.parse().ok()?;
    let instrument_id = field_text(instrument_record, "id")?;

    Some(TransferInstructionInfo {
        contract_id: created.contract_id.clone(),
        sender,
        receiver,
        amount,
        instrument_admin,
        instrument_id,
        status,
        pending_actions,
        expires_at,
    })
}

/// Fetch active `MintRequest` contracts (`Utility.Registry.App.V0.Model.Mint`)
/// visible to `party_id`. Past-deadline contracts are dropped so the Accept
/// dropdown only offers requests that would still succeed at interpretation.
pub async fn get_open_mint_requests(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
) -> Result<Vec<TokenRequestInfo>> {
    let Some(pkg) = packages.utility_registry.as_ref() else {
        return Ok(Vec::new());
    };
    fetch_token_requests_for_template(
        config,
        party_id,
        token,
        &TemplateId {
            package_id: pkg.clone(),
            module_name: "Utility.Registry.App.V0.Model.Mint",
            entity_name: "MintRequest",
        },
        "mint",
    )
    .await
}

/// Fetch active `BurnRequest` contracts. Mirrors `get_open_mint_requests`.
pub async fn get_open_burn_requests(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
) -> Result<Vec<TokenRequestInfo>> {
    let Some(pkg) = packages.utility_registry.as_ref() else {
        return Ok(Vec::new());
    };
    fetch_token_requests_for_template(
        config,
        party_id,
        token,
        &TemplateId {
            package_id: pkg.clone(),
            module_name: "Utility.Registry.App.V0.Model.Burn",
            entity_name: "BurnRequest",
        },
        "burn",
    )
    .await
}

async fn fetch_token_requests_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
    payload_field: &str,
) -> Result<Vec<TokenRequestInfo>> {
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

    fetch_active_contracts_filtered(config, token, event_format, |created| {
        if is_execute_before_expired_in_payload(&created, payload_field) {
            return None;
        }
        extract_token_request_info(&created, payload_field)
    })
    .await
}

/// Extract `{holder, amount, instrumentId.{admin,id}, executeBefore}` from a
/// MintRequest/BurnRequest created event. `payload_field` is `"mint"` or
/// `"burn"` — the nested record wrapping the shared `Mint`/`Burn` payload.
fn extract_token_request_info(
    created: &CreatedEvent,
    payload_field: &str,
) -> Option<TokenRequestInfo> {
    let record = created.create_arguments.as_ref()?;
    let payload = record
        .fields
        .iter()
        .find(|f| f.label == payload_field)
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Record(r)) => Some(r),
            _ => None,
        })?;

    let holder: CantonId = field_party(payload, "holder")?.parse().ok()?;
    let amount = field_numeric(payload, "amount").and_then(|s| DamlDecimal::parse(&s).ok())?;

    let instrument_record = payload
        .fields
        .iter()
        .find(|f| f.label == "instrumentId")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Record(r)) => Some(r),
            _ => None,
        })?;
    let instrument_admin: CantonId = field_party(instrument_record, "admin")?.parse().ok()?;
    let instrument_id = field_text(instrument_record, "id")?;

    let expires_at = field_timestamp(payload, "executeBefore")? / 1_000_000;

    Some(TokenRequestInfo {
        contract_id: created.contract_id.clone(),
        holder,
        amount,
        instrument_admin,
        instrument_id,
        expires_at,
    })
}

/// Same as `is_execute_before_expired`, but looks inside the nested `mint`/
/// `burn` payload record where MintRequest/BurnRequest carry their deadline.
fn is_execute_before_expired_in_payload(created: &CreatedEvent, payload_field: &str) -> bool {
    let Some(record) = created.create_arguments.as_ref() else {
        return false;
    };
    let Some(payload) = record
        .fields
        .iter()
        .find(|f| f.label == payload_field)
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Record(r)) => Some(r),
            _ => None,
        })
    else {
        return false;
    };
    let Some(execute_before_micros) = field_timestamp(payload, "executeBefore") else {
        return false;
    };
    let now_micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0);
    execute_before_micros <= now_micros
}

/// Decode the `pendingActions :: Map Party Text` payload of
/// `TransferPendingInternalWorkflow`. Daml `Map` is delivered as a `GenMap` of
/// key/value pairs; we drop entries with malformed party ids rather than
/// failing the whole instruction.
fn extract_pending_actions(value: &Value) -> Vec<PendingAction> {
    let entries = match &value.sum {
        Some(value::Sum::GenMap(m)) => &m.entries,
        Some(value::Sum::TextMap(_)) => return Vec::new(), // party-keyed maps come as GenMap
        _ => return Vec::new(),
    };
    entries
        .iter()
        .filter_map(|entry| {
            let key_party = entry
                .key
                .as_ref()
                .and_then(|v| match &v.sum {
                    Some(value::Sum::Party(p)) => Some(p.clone()),
                    _ => None,
                })
                .and_then(|s| CantonId::parse(&s).ok())?;
            let action = entry
                .value
                .as_ref()
                .and_then(|v| match &v.sum {
                    Some(value::Sum::Text(t)) => Some(t.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            Some(PendingAction {
                party: key_party,
                action,
            })
        })
        .collect()
}

/// Returns true if the contract's create-arguments carry an `executeBefore`
/// Time field whose value is in the past. Returns false when no such field
/// exists, so templates without a deadline are kept as-is.
fn is_execute_before_expired(created: &CreatedEvent) -> bool {
    let Some(record) = created.create_arguments.as_ref() else {
        return false;
    };
    let Some(execute_before_micros) = field_timestamp(record, "executeBefore") else {
        return false;
    };
    let now_micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0);
    execute_before_micros <= now_micros
}

// ============================================================================
// Token-standard TransferFactory Query (for Transfer Proposal form prefill)
// ============================================================================

/// Fetch active `Splice.Api.Token.TransferInstructionV1:TransferFactory`
/// contracts visible to `party_id`. Used by the Transfer Proposal form's
/// instrument dropdown to prefill the factory CID and expected-admin once the
/// user picks an instrument — joined on
/// `expected_admin == holding.instrument_admin`.
pub async fn get_transfer_factories(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
) -> Result<Vec<TransferFactoryInfo>> {
    let event_format = party_event_format(
        party_id,
        vec![interface_filter(
            Identifier {
                package_id: "#splice-api-token-transfer-instruction-v1".to_string(),
                module_name: "Splice.Api.Token.TransferInstructionV1".to_string(),
                entity_name: "TransferFactory".to_string(),
            },
            false,
        )],
        true,
    );

    fetch_active_contracts_filtered(config, token, event_format, |created| {
        extract_transfer_factory_info(&created)
    })
    .await
}

/// Pull `admin` (the instrument admin / expected admin) out of the
/// `TransferFactory` interface view. The view is the standard
/// `TransferFactoryView` which contains an `admin: Party` field.
fn extract_transfer_factory_info(created: &CreatedEvent) -> Option<TransferFactoryInfo> {
    let view = created.interface_views.iter().find(|v| {
        v.interface_id.as_ref().is_some_and(|id| {
            id.module_name == "Splice.Api.Token.TransferInstructionV1"
                && id.entity_name == "TransferFactory"
        })
    })?;
    let view_record = view.view_value.as_ref()?;
    let admin: CantonId = field_party(view_record, "admin")?.parse().ok()?;
    Some(TransferFactoryInfo {
        contract_id: created.contract_id.clone(),
        expected_admin: admin,
    })
}

// ============================================================================
// Token-standard Holding Query (for the Holdings section in PartyDetail)
// ============================================================================

/// Standard `instrumentId.id` for Canton Coin holdings — used to route the
/// preapproval check to `Splice.AmuletRules:TransferPreapproval` (which has no
/// explicit instrument field) instead of the per-instrument Utility registry.
const AMULET_INSTRUMENT_ID: &str = "Amulet";

/// Fetch all token-standard holdings owned by `party_id`, aggregated by
/// instrument. Each returned `HoldingInfo` represents one
/// `(instrument_admin, instrument_id)` pair with the summed amount across
/// every active `Holding` contract.
///
/// `preapproval_set_up` reflects whether the party has a `TransferPreapproval`
/// in place for that instrument: CC holdings match any
/// `Splice.AmuletRules:TransferPreapproval`, other instruments match by
/// `(admin, id)` against `Utility.Registry.App.V0.Model.TransferPreapproval`.
pub async fn get_holdings(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
) -> Result<Vec<HoldingInfo>> {
    let raw = fetch_holding_views(config, party_id, token.clone()).await?;

    // Aggregate amounts by (admin, id). A party can own many Holding contracts
    // for the same instrument (one per UTXO-style entry). Track the locked
    // subtotal separately: locked holdings are escrowed for an in-flight
    // transfer/allocation and can't fund a new one, so the UI shows them apart
    // from the freely-transferable balance.
    let mut totals: HashMap<(String, String), (CantonId, String, DamlDecimal, DamlDecimal)> =
        HashMap::new();
    for raw_holding in raw {
        let key = (
            raw_holding.instrument_admin.to_string(),
            raw_holding.instrument_id.clone(),
        );
        let locked_delta = if raw_holding.is_locked {
            raw_holding.amount
        } else {
            DamlDecimal::ZERO
        };
        totals
            .entry(key)
            .and_modify(|(_, _, total, locked)| {
                *total += raw_holding.amount;
                *locked += locked_delta;
            })
            .or_insert((
                raw_holding.instrument_admin,
                raw_holding.instrument_id,
                raw_holding.amount,
                locked_delta,
            ));
    }

    if totals.is_empty() {
        return Ok(Vec::new());
    }

    // Look up preapprovals once and join.
    let preapprovals = fetch_preapproved_instruments(config, party_id, token).await?;

    let mut holdings: Vec<HoldingInfo> = totals
        .into_values()
        .map(|(instrument_admin, instrument_id, amount, locked_amount)| {
            let preapproval_set_up = if instrument_id == AMULET_INSTRUMENT_ID {
                preapprovals.has_amulet
            } else {
                let admin = instrument_admin.to_string();
                preapprovals
                    .utility
                    .contains(&(admin.clone(), instrument_id.clone()))
                    || preapprovals
                        .utility
                        .contains(&(admin, PREAPPROVAL_WILDCARD_ID.to_string()))
            };
            HoldingInfo {
                instrument_admin,
                instrument_id,
                amount,
                locked_amount,
                preapproval_set_up,
            }
        })
        .collect();

    // Stable display order: admin ascending, then id ascending.
    holdings.sort_by(|a, b| {
        a.instrument_admin
            .to_string()
            .cmp(&b.instrument_admin.to_string())
            .then_with(|| a.instrument_id.cmp(&b.instrument_id))
    });

    Ok(holdings)
}

/// Run the ACS query with `InterfaceFilter` for `Holding` and return one
/// parsed view per active contract owned by `party_id`.
async fn fetch_holding_views(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
) -> Result<Vec<HoldingView>> {
    let event_format = party_event_format(
        party_id,
        vec![interface_filter(
            Identifier {
                package_id: "#splice-api-token-holding-v1".to_string(),
                module_name: "Splice.Api.Token.HoldingV1".to_string(),
                entity_name: "Holding".to_string(),
            },
            false,
        )],
        true,
    );

    let owner_str = party_id.to_string();

    fetch_active_contracts_filtered(config, token, event_format, |created| {
        extract_holding_view(&created).filter(|view| view.owner == owner_str)
    })
    .await
}

/// Intermediate parse result. `owner` lets `fetch_holding_views` drop holdings
/// the party can see (via interface visibility) but doesn't actually own,
/// before the views reach any caller. `is_locked` is `true` when the Holding
/// carries a `lock` (it's reserved for an in-flight transfer/allocation); such
/// holdings can't fund a new `TransferFactory_Transfer`.
struct HoldingView {
    contract_id: String,
    owner: String,
    instrument_admin: CantonId,
    instrument_id: String,
    amount: DamlDecimal,
    is_locked: bool,
}

fn extract_holding_view(created: &CreatedEvent) -> Option<HoldingView> {
    let view = created.interface_views.iter().find(|v| {
        v.interface_id.as_ref().is_some_and(|id| {
            id.module_name == "Splice.Api.Token.HoldingV1" && id.entity_name == "Holding"
        })
    })?;
    let view_record = view.view_value.as_ref()?;

    let owner = field_party(view_record, "owner")?;
    let amount = field_numeric(view_record, "amount").and_then(|s| DamlDecimal::parse(&s).ok())?;

    let instrument_record = view_record
        .fields
        .iter()
        .find(|f| f.label == "instrumentId")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Record(r)) => Some(r),
            _ => None,
        })?;
    let instrument_admin: CantonId = field_party(instrument_record, "admin")?.parse().ok()?;
    let instrument_id = field_text(instrument_record, "id")?;

    // `lock : Optional Lock` — present (`Some`) means the holding is locked for
    // an in-flight transfer/allocation. A missing field is treated as unlocked.
    let is_locked = view_record
        .fields
        .iter()
        .find(|f| f.label == "lock")
        .and_then(|f| f.value.as_ref())
        .is_some_and(|v| match &v.sum {
            Some(value::Sum::Optional(opt)) => opt.value.is_some(),
            _ => false,
        });

    Some(HoldingView {
        contract_id: created.contract_id.clone(),
        owner,
        instrument_admin,
        instrument_id,
        amount,
        is_locked,
    })
}

/// Collect the contract ids of every *unlocked* `Holding` the party owns for a
/// given instrument `(admin, id)`. Used by the Transfer proposal flow to fund
/// the transfer when the caller doesn't pin specific holdings: the token-standard
/// transfer factory rejects an empty `inputHoldingCids` ("No holdings
/// provided"), so we hand it every matching holding and let the choice consume
/// what it needs and return change.
///
/// Locked holdings are excluded: they're reserved for an in-flight
/// transfer/allocation, and feeding one to `TransferFactory_Transfer` fails at
/// execute time with `AssertionFailed: Input holding lock must match`.
pub async fn select_input_holdings(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    instrument_admin: &CantonId,
    instrument_id: &str,
) -> Result<Vec<String>> {
    let raw = fetch_holding_views(config, party_id, token).await?;
    Ok(raw
        .into_iter()
        .filter(|h| {
            !h.is_locked
                && h.instrument_admin == *instrument_admin
                && h.instrument_id == instrument_id
        })
        .map(|h| h.contract_id)
        .collect())
}

/// Result of the per-party preapproval lookup. `utility` is the set of
/// instruments (`(admin, id)`) that have an active utility-registry
/// `TransferPreapproval`; `has_amulet` is true iff at least one Amulet
/// `TransferPreapproval` exists.
struct PartyPreapprovals {
    has_amulet: bool,
    utility: std::collections::HashSet<(String, String)>,
}

/// `NO_TEMPLATES_FOR_PACKAGE_NAME_AND_QUALIFIED_NAME` means the template
/// simply isn't uploaded on this participant — there's nothing to count, not
/// a failure. Demote those to debug so the logs don't fill with red herrings
/// on participants without splice-amulet / utility-registry packages.
fn log_preapproval_lookup_error(label: &str, e: &anyhow::Error) {
    let msg = e.to_string();
    if msg.contains("NO_TEMPLATES_FOR_PACKAGE_NAME_AND_QUALIFIED_NAME") {
        tracing::debug!("No {label} templates on this participant; treating as 0");
    } else {
        tracing::warn!("Failed to query {label}: {e}");
    }
}

async fn fetch_preapproved_instruments(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
) -> Result<PartyPreapprovals> {
    let amulet_params = ContractQueryParams {
        package_id: "#splice-amulet".to_string(),
        module_name: "Splice.AmuletRules".to_string(),
        entity_name: "TransferPreapproval".to_string(),
        use_interface_filter: false,
        active_only: false,
    };
    let has_amulet =
        match query_contracts_by_template(config, party_id, token.clone(), &amulet_params).await {
            Ok(rows) => !rows.is_empty(),
            Err(e) => {
                log_preapproval_lookup_error("Amulet TransferPreapproval", &e);
                false
            }
        };

    // Utility preapprovals carry their instrument on the create-arguments
    // payload, so re-fetch with a TemplateFilter to get create_arguments and
    // parse `instrumentId.{admin,id}` out.
    let utility = match fetch_utility_preapproval_instruments(config, party_id, token).await {
        Ok(set) => set,
        Err(e) => {
            log_preapproval_lookup_error("utility TransferPreapproval", &e);
            std::collections::HashSet::new()
        }
    };

    Ok(PartyPreapprovals {
        has_amulet,
        utility,
    })
}

async fn fetch_utility_preapproval_instruments(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
) -> Result<std::collections::HashSet<(String, String)>> {
    let event_format = party_event_format(
        party_id,
        vec![template_filter(
            Identifier {
                package_id: "#utility-registry-app-v0".to_string(),
                module_name: "Utility.Registry.App.V0.Model.TransferPreapproval".to_string(),
                entity_name: "TransferPreapproval".to_string(),
            },
            false,
        )],
        true,
    );

    let entries = fetch_active_contracts_filtered(config, token, event_format, |created| {
        created
            .create_arguments
            .as_ref()
            .map(extract_preapproval_entries)
    })
    .await?;

    Ok(entries.into_iter().flatten().collect())
}

/// Sentinel `instrument_id` for a preapproval whose `instrumentAllowances` is
/// empty — utility-registry semantics is "any instrument from this admin", so
/// we store the wildcard once and the join check matches all of that admin's
/// holdings.
pub(super) const PREAPPROVAL_WILDCARD_ID: &str = "*";

/// Extract one `(admin, id)` per allowance from a `Utility.Registry.App.V0
/// .Model.TransferPreapproval.TransferPreapproval` contract. The on-chain
/// shape is `instrumentAdmin: Party` + `instrumentAllowances: [{ id: Text }]`;
/// an empty allowance list is the registrar's wildcard ("preapprove any
/// instrument issued by this admin"), which we represent as
/// `(admin, PREAPPROVAL_WILDCARD_ID)`.
fn extract_preapproval_entries(args: &Record) -> Vec<(String, String)> {
    let Some(admin) = field_party(args, "instrumentAdmin") else {
        return Vec::new();
    };
    let allowances = args
        .fields
        .iter()
        .find(|f| f.label == "instrumentAllowances")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::List(l)) => Some(&l.elements),
            _ => None,
        });
    let Some(elements) = allowances else {
        return vec![(admin, PREAPPROVAL_WILDCARD_ID.to_string())];
    };
    if elements.is_empty() {
        return vec![(admin, PREAPPROVAL_WILDCARD_ID.to_string())];
    }
    elements
        .iter()
        .filter_map(|v| match &v.sum {
            Some(value::Sum::Record(r)) => field_text(r, "id"),
            _ => None,
        })
        .map(|id| (admin.clone(), id))
        .collect()
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::daml::ledger::api::v2::admin::{ObjectMeta, PartyDetails};

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

    fn ci(name: &str, version: &str, created_at: &str, contract_id: &str) -> ContractInfo {
        ContractInfo {
            contract_id: contract_id.to_string(),
            template_id: format!("Mod:{name}"),
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

    // ------------------------------------------------------------------------
    // extract_transfer_instruction_info
    //
    // Locks the two filters that are easy to break by accident: the status
    // constructor match and the `executeBefore` deadline check.
    // ------------------------------------------------------------------------

    use canton_proto_rs::com::daml::ledger::api::v2::{
        InterfaceView, List, Optional, RecordField, Variant,
    };

    fn field(label: &str, value: Value) -> RecordField {
        RecordField {
            label: label.to_string(),
            value: Some(value),
        }
    }

    fn text_value(s: &str) -> Value {
        Value {
            sum: Some(value::Sum::Text(s.to_string())),
        }
    }

    fn party_value(p: &str) -> Value {
        Value {
            sum: Some(value::Sum::Party(p.to_string())),
        }
    }

    fn numeric_value(n: &str) -> Value {
        Value {
            sum: Some(value::Sum::Numeric(n.to_string())),
        }
    }

    fn timestamp_value(micros: i64) -> Value {
        Value {
            sum: Some(value::Sum::Timestamp(micros)),
        }
    }

    fn variant_value(constructor: &str, inner: Value) -> Value {
        Value {
            sum: Some(value::Sum::Variant(Box::new(Variant {
                variant_id: None,
                constructor: constructor.to_string(),
                value: Some(Box::new(inner)),
            }))),
        }
    }

    fn record_value(fields: Vec<RecordField>) -> Value {
        Value {
            sum: Some(value::Sum::Record(Record {
                record_id: None,
                fields,
            })),
        }
    }

    fn unit_value() -> Value {
        record_value(vec![])
    }

    /// Build a `CreatedEvent` carrying a `TransferInstructionView` interface
    /// view. `status_ctor` is the variant constructor on the status field;
    /// `execute_before_micros` populates the transfer record's
    /// `executeBefore` field.
    fn make_event(status_ctor: &str, execute_before_micros: i64) -> CreatedEvent {
        // Canton party id format: `<prefix>::<34-byte-multihash-hex>`.
        // `CantonId::parse` rejects anything else, so use a real-shaped fingerprint.
        const FP: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        let transfer = record_value(vec![
            field("sender", party_value(&format!("alice::{FP}"))),
            field("receiver", party_value(&format!("bob::{FP}"))),
            field("amount", numeric_value("10.0")),
            field(
                "instrumentId",
                record_value(vec![
                    field("admin", party_value(&format!("admin::{FP}"))),
                    field("id", text_value("CBTC")),
                ]),
            ),
            field("executeBefore", timestamp_value(execute_before_micros)),
        ]);
        let view = InterfaceView {
            interface_id: Some(Identifier {
                package_id: "#splice-api-token-transfer-instruction-v1".to_string(),
                module_name: "Splice.Api.Token.TransferInstructionV1".to_string(),
                entity_name: "TransferInstruction".to_string(),
            }),
            view_status: None,
            view_value: Some(Record {
                record_id: None,
                fields: vec![
                    field("status", variant_value(status_ctor, unit_value())),
                    field("transfer", transfer),
                ],
            }),
            implementation_package_id: String::new(),
        };
        CreatedEvent {
            offset: 0,
            node_id: 0,
            contract_id: "cid-1".to_string(),
            template_id: None,
            contract_key: None,
            create_arguments: None,
            created_event_blob: vec![],
            interface_views: vec![view],
            witness_parties: vec![],
            signatories: vec![],
            observers: vec![],
            created_at: None,
            package_name: String::new(),
            representative_package_id: String::new(),
            acs_delta: false,
            contract_key_hash: Vec::new(),
        }
    }

    #[test]
    fn extract_transfer_instruction_info_accepts_pending_in_future() {
        let future_micros = i64::MAX / 4;
        let info = extract_transfer_instruction_info(&make_event(
            TRANSFER_PENDING_RECEIVER_ACCEPTANCE,
            future_micros,
        ))
        .expect("pending + in-future should yield info");
        assert_eq!(info.contract_id, "cid-1");
        assert!(info.sender.to_string().starts_with("alice::"));
        assert!(info.receiver.to_string().starts_with("bob::"));
    }

    #[test]
    fn extract_transfer_instruction_info_drops_non_pending_status() {
        let future_micros = i64::MAX / 4;
        assert!(
            extract_transfer_instruction_info(&make_event("TransferInProgress", future_micros))
                .is_none(),
        );
    }

    #[test]
    fn extract_transfer_instruction_info_keeps_expired_with_zero_deadline() {
        // Expired offers used to be dropped silently; now they're returned so
        // the UI can render them as disabled "expired" rows.
        let info =
            extract_transfer_instruction_info(&make_event(TRANSFER_PENDING_RECEIVER_ACCEPTANCE, 0))
                .expect("expired offer should still be returned, just past-deadline");
        assert_eq!(info.expires_at, 0);
    }

    // ------------------------------------------------------------------------
    // extract_holding_view
    //
    // The `lock` field on the Holding interface view decides whether a holding
    // can fund a transfer. A locked holding fed to TransferFactory_Transfer
    // fails at execute time with "Input holding lock must match", so the parser
    // must surface `is_locked` for select_input_holdings to filter on.
    // ------------------------------------------------------------------------

    // `<prefix>::<34-byte-multihash-hex>`; CantonId::parse rejects other shapes.
    const HOLDING_FP: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

    /// Build a `CreatedEvent` carrying a `HoldingV1.Holding` interface view.
    /// `lock` populates the optional `lock` field — `None` for an unlocked
    /// holding, `Some` (any record) for a locked one.
    fn make_holding_event(amount: &str, lock: Option<Value>) -> CreatedEvent {
        let view = InterfaceView {
            interface_id: Some(Identifier {
                package_id: "#splice-api-token-holding-v1".to_string(),
                module_name: "Splice.Api.Token.HoldingV1".to_string(),
                entity_name: "Holding".to_string(),
            }),
            view_status: None,
            view_value: Some(Record {
                record_id: None,
                fields: vec![
                    field("owner", party_value(&format!("owner::{HOLDING_FP}"))),
                    field("amount", numeric_value(amount)),
                    field(
                        "instrumentId",
                        record_value(vec![
                            field("admin", party_value(&format!("admin::{HOLDING_FP}"))),
                            field("id", text_value("Test01")),
                        ]),
                    ),
                    field("lock", optional_value(lock)),
                ],
            }),
            implementation_package_id: String::new(),
        };
        CreatedEvent {
            offset: 0,
            node_id: 0,
            contract_id: "holding-cid".to_string(),
            template_id: None,
            contract_key: None,
            create_arguments: None,
            created_event_blob: vec![],
            interface_views: vec![view],
            witness_parties: vec![],
            signatories: vec![],
            observers: vec![],
            created_at: None,
            package_name: String::new(),
            representative_package_id: String::new(),
            acs_delta: false,
            contract_key_hash: Vec::new(),
        }
    }

    #[test]
    fn extract_holding_view_unlocked_when_lock_none() {
        // The `lock` field is present but an empty `Optional` (None) — the
        // on-ledger shape for an unlocked holding.
        let view = extract_holding_view(&make_holding_event("20.0", None))
            .expect("unlocked holding view should parse");
        assert!(!view.is_locked);
        assert_eq!(view.instrument_id, "Test01");
        assert_eq!(view.amount, DamlDecimal::parse("20.0").expect("decimal"));
    }

    #[test]
    fn extract_holding_view_unlocked_when_lock_field_missing() {
        // Defensive path: if the interface view omits the `lock` field entirely,
        // the holding is treated as unlocked rather than failing to parse.
        let mut event = make_holding_event("7.0", None);
        if let Some(view) = event.interface_views.first_mut()
            && let Some(record) = view.view_value.as_mut()
        {
            record.fields.retain(|f| f.label != "lock");
        }
        let view =
            extract_holding_view(&event).expect("holding view without a lock field should parse");
        assert!(!view.is_locked);
    }

    #[test]
    fn extract_holding_view_locked_when_lock_present() {
        // A non-empty record stands in for the Lock payload; only presence matters.
        let lock = record_value(vec![field(
            "holders",
            party_value(&format!("locker::{HOLDING_FP}")),
        )]);
        let view = extract_holding_view(&make_holding_event("5.0", Some(lock)))
            .expect("locked holding view should still parse");
        assert!(view.is_locked);
    }

    // `<prefix>::<34-byte-multihash-hex>`; CantonId::parse rejects other shapes.
    const SR_FP: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

    // ------------------------------------------------------------------------
    // extract_credential_offer_info
    //
    // `Utility.Credential.App.V0.Model.Offer:CredentialOffer` carries the
    // issuer/holder parties, the credential id/description, and an optional
    // `billingParams`. Offers with `billingParams = None` are free and the only
    // kind `AcceptFreeCredential` can take, so the extractor surfaces that as
    // `is_free` for the accept-form dropdown to filter on.
    // ------------------------------------------------------------------------

    fn optional_value(inner: Option<Value>) -> Value {
        Value {
            sum: Some(value::Sum::Optional(Box::new(Optional {
                value: inner.map(Box::new),
            }))),
        }
    }

    /// A CredentialOffer created event; `billing_params` is the raw value of
    /// the template's `billingParams : Optional BillingParams` field.
    fn credential_offer_event(billing_params: Value) -> CreatedEvent {
        let record = Record {
            record_id: None,
            fields: vec![
                field("operator", party_value(&format!("operator::{SR_FP}"))),
                field("issuer", party_value(&format!("issuer::{SR_FP}"))),
                field("holder", party_value(&format!("holder::{SR_FP}"))),
                field("dso", party_value(&format!("dso::{SR_FP}"))),
                field("id", text_value("provider-service-credential")),
                field("description", text_value("Provider service access")),
                field("billingParams", billing_params),
                field("depositInitialAmountUsd", optional_value(None)),
            ],
        };
        CreatedEvent {
            offset: 0,
            node_id: 0,
            contract_id: "offer-cid-1".to_string(),
            template_id: None,
            contract_key: None,
            create_arguments: Some(record),
            created_event_blob: vec![],
            interface_views: vec![],
            witness_parties: vec![],
            signatories: vec![],
            observers: vec![],
            created_at: None,
            package_name: String::new(),
            representative_package_id: String::new(),
            acs_delta: false,
            contract_key_hash: Vec::new(),
        }
    }

    #[test]
    fn extract_credential_offer_info_reads_free_offer() {
        let event = credential_offer_event(optional_value(None));
        let Some(info) = extract_credential_offer_info(&event) else {
            panic!("free offer should yield info");
        };
        assert_eq!(info.contract_id, "offer-cid-1");
        assert_eq!(info.operator.to_string(), format!("operator::{SR_FP}"));
        assert_eq!(info.issuer.to_string(), format!("issuer::{SR_FP}"));
        assert_eq!(info.holder.to_string(), format!("holder::{SR_FP}"));
        assert_eq!(info.credential_id, "provider-service-credential");
        assert_eq!(info.description, "Provider service access");
        assert!(info.is_free);
    }

    #[test]
    fn extract_credential_offer_info_marks_paid_offer_not_free() {
        let billing = optional_value(Some(record_value(vec![field(
            "billingPeriodDuration",
            text_value("placeholder"),
        )])));
        let Some(info) = extract_credential_offer_info(&credential_offer_event(billing)) else {
            panic!("paid offer should still yield info");
        };
        assert!(!info.is_free);
    }

    #[test]
    fn extract_credential_offer_info_skips_event_without_holder() {
        let mut event = credential_offer_event(optional_value(None));
        if let Some(record) = event.create_arguments.as_mut() {
            record.fields.retain(|f| f.label != "holder");
        }
        assert!(extract_credential_offer_info(&event).is_none());
    }

    // ------------------------------------------------------------------------
    // extract_credential_info
    //
    // `Utility.Credential.V0.Credential:Credential` carries issuer/holder,
    // the credential id/description, and a `claims` list whose `subject`
    // names the party each claim attests for. The extractor feeds the
    // issuer-credential picker on the accept mint/burn request forms.
    // ------------------------------------------------------------------------

    fn list_value(elements: Vec<Value>) -> Value {
        Value {
            sum: Some(value::Sum::List(List { elements })),
        }
    }

    fn claim_value(subject: &str, property: &str, value: &str) -> Value {
        record_value(vec![
            field("subject", text_value(subject)),
            field("property", text_value(property)),
            field("value", text_value(value)),
        ])
    }

    /// A Credential created event; `claims` is the raw value of the
    /// template's `claims : [Claim]` field.
    fn credential_event(claims: Value) -> CreatedEvent {
        let record = Record {
            record_id: None,
            fields: vec![
                field("issuer", party_value(&format!("issuer::{SR_FP}"))),
                field("holder", party_value(&format!("holder::{SR_FP}"))),
                field(
                    "id",
                    text_value("LAUNCH-TOKEN-instrument-issuer-credential/subject/0-0"),
                ),
                field("description", text_value("Governance-minted credential")),
                field("validFrom", optional_value(None)),
                field("validUntil", optional_value(None)),
                field("claims", claims),
                field("observers", list_value(vec![])),
            ],
        };
        CreatedEvent {
            offset: 0,
            node_id: 0,
            contract_id: "credential-cid-1".to_string(),
            template_id: None,
            contract_key: None,
            create_arguments: Some(record),
            created_event_blob: vec![],
            interface_views: vec![],
            witness_parties: vec![],
            signatories: vec![],
            observers: vec![],
            created_at: None,
            package_name: String::new(),
            representative_package_id: String::new(),
            acs_delta: false,
            contract_key_hash: Vec::new(),
        }
    }

    #[test]
    fn extract_credential_info_reads_credential_with_claims() {
        let claims = list_value(vec![
            claim_value("subject-party", "role", "instrument-issuer"),
            claim_value("subject-party", "kyc", "passed"),
        ]);
        let Some(info) = extract_credential_info(&credential_event(claims)) else {
            panic!("credential should yield info");
        };
        assert_eq!(info.contract_id, "credential-cid-1");
        assert_eq!(info.issuer.to_string(), format!("issuer::{SR_FP}"));
        assert_eq!(info.holder.to_string(), format!("holder::{SR_FP}"));
        assert_eq!(
            info.credential_id,
            "LAUNCH-TOKEN-instrument-issuer-credential/subject/0-0"
        );
        assert_eq!(info.description, "Governance-minted credential");
        assert_eq!(info.claims.len(), 2);
        assert_eq!(info.claims[0].subject, "subject-party");
        assert_eq!(info.claims[0].property, "role");
        assert_eq!(info.claims[0].value, "instrument-issuer");
    }

    #[test]
    fn extract_credential_info_defaults_missing_description_and_empty_claims() {
        let mut event = credential_event(list_value(vec![]));
        if let Some(record) = event.create_arguments.as_mut() {
            record.fields.retain(|f| f.label != "description");
        }
        let Some(info) = extract_credential_info(&event) else {
            panic!("claimless credential should still yield info");
        };
        assert!(info.claims.is_empty());
        assert_eq!(info.description, "");
    }

    #[test]
    fn extract_credential_info_skips_event_without_holder() {
        let mut event = credential_event(list_value(vec![]));
        if let Some(record) = event.create_arguments.as_mut() {
            record.fields.retain(|f| f.label != "holder");
        }
        assert!(extract_credential_info(&event).is_none());
    }

    // ------------------------------------------------------------------------
    // extract_registrar_service_request_info
    //
    // `Utility.Registry.App.V0.Service.Registrar:RegistrarServiceRequest`
    // carries the operator/provider/registrar parties plus two
    // `Optional Bool` flags the SDK reads as `false` when absent. The
    // extractor feeds the request picker on the OnboardRegistrar form.
    // ------------------------------------------------------------------------

    fn bool_value(b: bool) -> Value {
        Value {
            sum: Some(value::Sum::Bool(b)),
        }
    }

    /// A RegistrarServiceRequest created event; the two arguments are the
    /// raw values of the template's `Optional Bool` flag fields.
    fn registrar_service_request_event(
        create_transfer_rule: Value,
        create_allocation_factory: Value,
    ) -> CreatedEvent {
        let record = Record {
            record_id: None,
            fields: vec![
                field("operator", party_value(&format!("operator::{SR_FP}"))),
                field("provider", party_value(&format!("provider::{SR_FP}"))),
                field("registrar", party_value(&format!("registrar::{SR_FP}"))),
                field("createTransferRule", create_transfer_rule),
                field("createAllocationFactory", create_allocation_factory),
            ],
        };
        CreatedEvent {
            offset: 0,
            node_id: 0,
            contract_id: "rsr-cid-1".to_string(),
            template_id: None,
            contract_key: None,
            create_arguments: Some(record),
            created_event_blob: vec![],
            interface_views: vec![],
            witness_parties: vec![],
            signatories: vec![],
            observers: vec![],
            created_at: None,
            package_name: String::new(),
            representative_package_id: String::new(),
            acs_delta: false,
            contract_key_hash: Vec::new(),
        }
    }

    #[test]
    fn extract_registrar_service_request_info_reads_request_with_flags() {
        let event = registrar_service_request_event(
            optional_value(Some(bool_value(true))),
            optional_value(Some(bool_value(false))),
        );
        let Some(info) = extract_registrar_service_request_info(&event) else {
            panic!("request should yield info");
        };
        assert_eq!(info.contract_id, "rsr-cid-1");
        assert_eq!(info.operator.to_string(), format!("operator::{SR_FP}"));
        assert_eq!(info.provider.to_string(), format!("provider::{SR_FP}"));
        assert_eq!(info.registrar.to_string(), format!("registrar::{SR_FP}"));
        assert!(info.create_transfer_rule);
        assert!(!info.create_allocation_factory);
    }

    #[test]
    fn extract_registrar_service_request_info_defaults_absent_flags_to_false() {
        // `None` flags — and fields missing outright — read as `false`,
        // matching the SDK's treatment.
        let mut event = registrar_service_request_event(optional_value(None), optional_value(None));
        if let Some(record) = event.create_arguments.as_mut() {
            record
                .fields
                .retain(|f| f.label != "createAllocationFactory");
        }
        let Some(info) = extract_registrar_service_request_info(&event) else {
            panic!("flagless request should still yield info");
        };
        assert!(!info.create_transfer_rule);
        assert!(!info.create_allocation_factory);
    }

    #[test]
    fn extract_registrar_service_request_info_skips_event_without_registrar() {
        let mut event = registrar_service_request_event(optional_value(None), optional_value(None));
        if let Some(record) = event.create_arguments.as_mut() {
            record.fields.retain(|f| f.label != "registrar");
        }
        assert!(extract_registrar_service_request_info(&event).is_none());
    }

    // ------------------------------------------------------------------------
    // extract_provider_configuration_info
    //
    // `Utility.Registry.App.V0.Configuration.Provider:ProviderConfiguration`
    // carries the operator/provider parties plus the registrar and holder
    // requirement lists. The extractor reads the parties only — the picker
    // labels configurations by contract id — and must tolerate the
    // requirement lists it ignores. It feeds the configuration picker on the
    // OnboardRegistrar form.
    // ------------------------------------------------------------------------

    /// A ProviderConfiguration created event, with empty requirement lists.
    fn provider_configuration_event() -> CreatedEvent {
        let record = Record {
            record_id: None,
            fields: vec![
                field("operator", party_value(&format!("operator::{SR_FP}"))),
                field("provider", party_value(&format!("provider::{SR_FP}"))),
                field("registrarRequirements", list_value(vec![])),
                field("holderRequirements", list_value(vec![])),
            ],
        };
        CreatedEvent {
            offset: 0,
            node_id: 0,
            contract_id: "pc-cid-1".to_string(),
            template_id: None,
            contract_key: None,
            create_arguments: Some(record),
            created_event_blob: vec![],
            interface_views: vec![],
            witness_parties: vec![],
            signatories: vec![],
            observers: vec![],
            created_at: None,
            package_name: String::new(),
            representative_package_id: String::new(),
            acs_delta: false,
            contract_key_hash: Vec::new(),
        }
    }

    #[test]
    fn extract_provider_configuration_info_reads_parties() {
        let Some(info) = extract_provider_configuration_info(&provider_configuration_event())
        else {
            panic!("configuration should yield info");
        };
        assert_eq!(info.contract_id, "pc-cid-1");
        assert_eq!(info.operator.to_string(), format!("operator::{SR_FP}"));
        assert_eq!(info.provider.to_string(), format!("provider::{SR_FP}"));
    }

    #[test]
    fn extract_provider_configuration_info_skips_event_without_provider() {
        let mut event = provider_configuration_event();
        if let Some(record) = event.create_arguments.as_mut() {
            record.fields.retain(|f| f.label != "provider");
        }
        assert!(extract_provider_configuration_info(&event).is_none());
    }

    // ====================================================================
    // Party metadata page walk
    // ====================================================================

    fn party_page(parties: &[(&str, &[(&str, &str)])], next: &str) -> ListKnownPartiesResponse {
        ListKnownPartiesResponse {
            party_details: parties
                .iter()
                .map(|(party, annotations)| PartyDetails {
                    party: (*party).to_string(),
                    is_local: true,
                    local_metadata: Some(ObjectMeta {
                        resource_version: String::new(),
                        annotations: annotations
                            .iter()
                            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                            .collect(),
                    }),
                    identity_provider_id: String::new(),
                })
                .collect(),
            next_page_token: next.to_string(),
        }
    }

    /// Walk `pages` in order. Asking past the script is an error, so a test that
    /// over-reads fails instead of quietly passing.
    async fn walk_parties(
        party_id: &str,
        pages: Vec<ListKnownPartiesResponse>,
    ) -> Result<Option<PartyMetadata>> {
        let mut remaining = std::collections::VecDeque::from(pages);

        find_party_annotations(party_id, |_page_token| {
            let page = remaining.pop_front();
            async move { page.ok_or_else(|| anyhow::anyhow!("asked for a page beyond the script")) }
        })
        .await
    }

    /// `filter_party` is only a prefix match, so the wanted party can sit behind
    /// a page of others — the walk has to follow the token to find it.
    #[tokio::test]
    async fn party_walk_finds_the_party_on_a_later_page() -> Result {
        let pages = vec![
            party_page(&[("other::1220aa", &[("k", "v")])], "page-2"),
            party_page(&[("wanted::1220bb", &[("owner", "alice")])], ""),
        ];

        let metadata = walk_parties("wanted::1220bb", pages).await?;

        assert_eq!(
            metadata.map(|m| m.annotations),
            Some([("owner".to_string(), "alice".to_string())].into())
        );

        Ok(())
    }

    /// An exhausted token list means the party is not hosted here.
    #[tokio::test]
    async fn party_walk_reports_nothing_when_the_tokens_run_out() -> Result {
        let pages = vec![
            party_page(&[("other::1220aa", &[])], "page-2"),
            party_page(&[("another::1220cc", &[])], ""),
        ];

        assert!(walk_parties("wanted::1220bb", pages).await?.is_none());

        Ok(())
    }

    /// A participant that keeps handing back the same token would otherwise walk
    /// forever; the walk treats a repeat as the end. The script holds two pages,
    /// so a third read would error rather than return `None`.
    #[tokio::test]
    async fn party_walk_stops_on_a_repeated_page_token() -> Result {
        let pages = vec![
            party_page(&[("other::1220aa", &[])], "stuck"),
            party_page(&[("other::1220aa", &[])], "stuck"),
        ];

        assert!(walk_parties("wanted::1220bb", pages).await?.is_none());

        Ok(())
    }

    /// Found, but carrying no annotations — there is no metadata to report, and
    /// the walk must not keep looking for a better match.
    #[tokio::test]
    async fn party_walk_reports_nothing_for_a_party_without_annotations() -> Result {
        let pages = vec![
            party_page(&[("wanted::1220bb", &[])], "page-2"),
            party_page(&[("wanted::1220bb", &[("owner", "alice")])], ""),
        ];

        assert!(walk_parties("wanted::1220bb", pages).await?.is_none());

        Ok(())
    }
}
