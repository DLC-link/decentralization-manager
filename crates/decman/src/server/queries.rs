//! Canton ledger query layer.
//!
//! Read-side helpers that query the Canton Ledger API (active contracts,
//! governance state, holdings, transfers, rewards, etc.) and shape the results
//! into the response types served by the HTTP handlers.

use std::{
    cmp::Reverse,
    collections::HashMap,
    future::Future,
    time::{SystemTime, UNIX_EPOCH},
};

use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        CreatedEvent, GetEventsByContractIdRequest, Identifier, Record,
        admin::{ListKnownPartiesRequest, ListKnownPartiesResponse},
        value,
    },
    digitalasset::canton::admin::participant::v30::{
        ListPackagesRequest, package_service_client::PackageServiceClient,
    },
};

use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
    error::Result,
    utils,
};

use super::{
    action_serializer,
    event_filters::{interface_filter, party_event_format, template_filter, wildcard_filter},
    ledger_paging::{
        FETCH_CHUNK, fetch_active_contracts_filtered, fetch_first_active_contract,
        for_each_active_contract,
    },
    package_inventory::{
        fetch_package_id_to_name, fetch_package_names, newest_matching_names, package_name_prefix,
    },
    record::{field_record, record_field},
    types::{
        AcceptTransferDetails, ActionType, Claim, ContractInfo, ContractWithBlob, CredentialInfo,
        CredentialOfferInfo, DomainGovernanceAction, GovernanceAction, GovernanceConfirmation,
        GovernanceState, HoldingInfo, InstrumentInfo, PartyMetadata, PendingAction,
        ProviderConfigurationInfo, ProviderServiceInfo, RegistrarServiceInfo,
        RegistrarServiceRequestInfo, ServiceRequestDetails, TokenRequestInfo, TransferFactoryInfo,
        TransferInstructionInfo, TransferInstructionStatus, TransferProposalDetails,
        UserServiceInfo, VaultInfo,
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
    let mut confirmations_by_hash: HashMap<String, (ActionType, Vec<GovernanceConfirmation>)> =
        HashMap::new();
    // Collect domain confirmations grouped by proposal CID (core domain actions)
    let mut domain_confirmations: HashMap<String, (String, Vec<GovernanceConfirmation>)> =
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
        .map(|(action_hash, (action, mut confirmations))| {
            // Newest-first so dedupe per-member keeps the freshest confirmation.
            confirmations.sort_by_key(|c| Reverse(c.created_at));
            let mut seen_parties = std::collections::HashSet::new();
            let unique_confirmations: Vec<GovernanceConfirmation> = confirmations
                .into_iter()
                .filter(|c| seen_parties.insert(c.confirming_party.clone()))
                .collect();

            // Mirror Daml's `expiresAt > now` filter so the UI doesn't offer an Execute that chain will reject.
            let confirmation_count = unique_confirmations
                .iter()
                .filter(|c| c.expires_at == 0 || c.expires_at > now_seconds)
                .count();
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

    let domain_actions = build_domain_actions(
        domain_confirmations,
        proposal_infos,
        proposal_infos_complete,
        domain_confirmations_complete,
        threshold,
        now_seconds,
    );

    Ok((actions, domain_actions))
}

/// Label used for a proposal synthesized from a bare `GovernableAction` when
/// nothing names it — no `actionLabel` in the interface view or the
/// create-arguments, and no template id on the event either.
const FALLBACK_PROPOSAL_LABEL: &str = "Proposal";

/// Merge confirmed domain proposals with the full active-proposal set.
///
/// `domain_confirmations` covers only proposals that already have at least
/// one `GovernanceConfirmation`; `proposal_infos` covers every active
/// `GovernableAction` visible to the party, confirmed or not. Confirmations
/// whose proposal isn't in `proposal_infos` are marked `orphaned` (rather
/// than dropped) so the UI can offer a dismiss-only card — the underlying
/// Confirmation contracts are still on-ledger and need to be expired
/// explicitly to clear them.
///
/// Whatever remains in `proposal_infos` after the confirmed proposals are
/// enriched and removed is a proposal nobody has confirmed yet. Those are
/// synthesized into zero-confirmation cards so a freshly created proposal is
/// visible and confirmable from the notifications queue, instead of staying
/// invisible until its first confirmation lands.
///
/// Synthesis needs both fetches to have succeeded. Without the full proposal
/// set we can't tell a genuinely new proposal from one we failed to enrich.
/// Without the full confirmation set a confirmed proposal looks untouched, and
/// its card would offer Confirm to a member who already confirmed. Either way,
/// missing a card for one refresh beats showing a wrong one.
fn build_domain_actions(
    domain_confirmations: HashMap<String, (String, Vec<GovernanceConfirmation>)>,
    mut proposal_infos: HashMap<String, ProposalInfo>,
    proposal_infos_complete: bool,
    domain_confirmations_complete: bool,
    threshold: usize,
    now_seconds: i64,
) -> Vec<DomainGovernanceAction> {
    let mut domain_actions: Vec<DomainGovernanceAction> = domain_confirmations
        .into_iter()
        .map(|(proposal_cid, (action_label, mut confirmations))| {
            confirmations.sort_by_key(|c| Reverse(c.created_at));
            // Only mark as orphaned when we successfully fetched the full
            // active-proposal set; otherwise the missing-from-map signal is
            // unreliable and we'd falsely mark everything as orphaned.
            let (
                description,
                transfer_details,
                accept_transfer_details,
                service_request_details,
                proposer,
                created_at,
                orphaned,
            ) = match proposal_infos.remove(&proposal_cid) {
                Some(info) => (
                    info.description,
                    info.transfer,
                    info.accept_transfer,
                    info.service_request,
                    info.proposer,
                    info.created_at,
                    false,
                ),
                None => (None, None, None, None, None, None, proposal_infos_complete),
            };
            let mut seen_parties = std::collections::HashSet::new();
            let unique_confirmations: Vec<GovernanceConfirmation> = confirmations
                .into_iter()
                .filter(|c| seen_parties.insert(c.confirming_party.clone()))
                .collect();
            let confirmation_count = unique_confirmations
                .iter()
                .filter(|c| c.expires_at == 0 || c.expires_at > now_seconds)
                .count();
            DomainGovernanceAction {
                proposal_cid,
                action_label,
                description,
                confirmations: unique_confirmations,
                confirmation_count,
                // Orphans can't be executed regardless of threshold.
                can_execute: !orphaned && confirmation_count >= threshold,
                orphaned,
                transfer_details,
                accept_transfer_details,
                service_request_details,
                proposer,
                created_at,
            }
        })
        .collect();

    if proposal_infos_complete && domain_confirmations_complete {
        for (proposal_cid, info) in proposal_infos {
            let action_label = info
                .action_label
                .unwrap_or_else(|| FALLBACK_PROPOSAL_LABEL.to_string());
            domain_actions.push(DomainGovernanceAction {
                proposal_cid,
                action_label,
                description: info.description,
                confirmations: Vec::new(),
                confirmation_count: 0,
                can_execute: false,
                orphaned: false,
                transfer_details: info.transfer,
                accept_transfer_details: info.accept_transfer,
                service_request_details: info.service_request,
                // A proposal nobody has confirmed is the likeliest one to
                // retract, so the card needs its proposer as much as any other.
                proposer: info.proposer,
                created_at: info.created_at,
            });
        }
    }

    domain_actions
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
        true,
    );

    for_each_active_contract(config, token, event_format, |created| {
        if created.template_id.as_ref().is_some_and(|t| {
            t.module_name == "Governance.Confirmation" && t.entity_name == "GovernanceConfirmation"
        }) {
            extract_and_add_domain_confirmation(&created, domain_confirmations);
        } else {
            extract_and_add_confirmation(&created, confirmations_by_hash);
        }
    })
    .await?;

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
    let Some(confirming_party_str) =
        field_party(record, "confirmingParty").or_else(|| field_party(record, "confirmer"))
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
        expires_at: field_timestamp(record, "expiresAt")
            .map(|micros| micros / 1_000_000)
            .unwrap_or(0),
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
    let proposal_cid = match record_field(record, "actionProposalCid") {
        Some(value::Sum::ContractId(cid)) => Some(cid.clone()),
        _ => None,
    }
    .unwrap_or_default();

    // Extract actionLabel (Text)
    let action_label = field_text(record, "actionLabel").unwrap_or_default();

    // Extract confirmer (Party). Skip the confirmation if missing or
    // malformed (see the off-chain extractor above for the same rationale).
    let Some(confirmer_str) = field_party(record, "confirmer") else {
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
        expires_at: field_timestamp(record, "expiresAt")
            .map(|micros| micros / 1_000_000)
            .unwrap_or(0),
    };

    domain_confirmations
        .entry(proposal_cid)
        .or_insert_with(|| (action_label, Vec::new()))
        .1
        .push(confirmation);
}

/// Per-proposal info pulled out of a `GovernableAction` contract. `description`
/// and `action_label` come from the interface view, which every proposal
/// implements; `transfer` is populated only for `TransferProposal` templates so
/// the notifications queue can render recipient/amount/instrument on the card
/// without a follow-up fetch.
///
/// `accept_transfer_instruction_cid` is captured for `AcceptTransferProposal`
/// templates (they only carry the linked `TransferInstruction` cid, not the
/// transfer fields themselves). `accept_transfer` is then populated by a
/// follow-up `GetEventsByContractId` per cid against the
/// `Splice.Api.Token.TransferInstructionV1:TransferInstruction` interface so
/// the pending-approval card can render sender/amount/instrument.
pub struct ProposalInfo {
    pub description: Option<String>,
    pub transfer: Option<TransferProposalDetails>,
    pub accept_transfer_instruction_cid: Option<String>,
    pub accept_transfer: Option<AcceptTransferDetails>,
    /// Operator + user/provider parties, populated only for
    /// `Create{User,Provider}ServiceRequest` proposals so the notification card
    /// can render the full summary. `extract_proposal_info` enforces that, so a
    /// consumer renders this field without re-checking the label.
    pub service_request: Option<ServiceRequestDetails>,
    /// `actionLabel` from the `GovernableActionView` interface view, falling
    /// back to a same-named create-argument field and then to the template's
    /// own name. `None` only when the event carries no template id either.
    pub action_label: Option<String>,
    /// The member who created the proposal. Only that member controls
    /// `GovernableAction_ProposerCancel`, so the card offers the retract
    /// button on this field alone. `None` when the party id fails to parse.
    pub proposer: Option<CantonId>,
    /// Seconds of the create event's ledger effective time. The feed sorts
    /// cards on this, so a proposal keeps its place across refreshes even
    /// before its first confirmation exists.
    pub created_at: Option<i64>,
}

/// Pull the `GovernableAction` interface view off a created event. Canton
/// only fills this in when the query asked for it with an `InterfaceFilter`,
/// so a wildcard fetch (test mode) always gets `None`.
fn governable_action_view(created: &CreatedEvent) -> Option<&Record> {
    created
        .interface_views
        .iter()
        .find(|v| {
            v.interface_id.as_ref().is_some_and(|id| {
                id.module_name == "Governance.Action" && id.entity_name == "GovernableAction"
            })
        })?
        .view_value
        .as_ref()
}

/// Whether a contract's create-arguments carry the two fields every
/// in-repo proposal template declares. Used only when no interface view is
/// available: a wildcard fetch returns every contract the party can see, so
/// something has to keep unrelated templates out of the proposal map.
fn looks_like_governable_action(record: &Record) -> bool {
    let has = |label: &str| record.fields.iter().any(|f| f.label == label);
    has("governanceParty") && has("proposer")
}

/// Extract proposal info from a `GovernableAction` contract.
///
/// The interface view is the authoritative source: Canton only attaches one
/// to a contract that really implements `GovernableAction`, and the view
/// carries `actionLabel` and `description` even for templates that compute
/// them rather than store them. Its presence is therefore enough to capture
/// the contract, whatever package declared the template and however that
/// template names its own fields.
///
/// A wildcard fetch (test mode) carries no view, so it falls back to the
/// field-shape heuristic and to create-arguments for the same values.
///
/// `governance_party` is the decentralized party the caller is querying for.
/// Being able to see a proposal does not mean governing it: another package
/// may name our party as an observer on a proposal some other governance party
/// controls. Our members hold no authority there, so Confirm would be rejected
/// on-ledger. Such a proposal is dropped rather than shown.
fn extract_proposal_info(
    created: &CreatedEvent,
    governance_party: &CantonId,
    proposal_infos: &mut HashMap<String, ProposalInfo>,
) {
    let view = governable_action_view(created);
    let record = created.create_arguments.as_ref();

    if view.is_none() && !record.is_some_and(looks_like_governable_action) {
        return;
    }

    // Absent rather than mismatched is not a rejection: a wildcard fetch of a
    // template that computes the field in its view has nothing to compare.
    let governs = view
        .and_then(|v| field_party(v, "governanceParty"))
        .or_else(|| record.and_then(|r| field_party(r, "governanceParty")));
    if let Some(ref found) = governs
        && found != &governance_party.to_string()
    {
        tracing::debug!(
            "Skipping proposal {cid}: governed by {found}, not {governance_party}",
            cid = created.contract_id
        );
        return;
    }

    let description = view
        .and_then(|v| field_text(v, "description"))
        .or_else(|| record.and_then(|r| field_text(r, "description")));

    // Read from the view first for the same reason the label and description
    // do: the interface declares `proposer`, so the view always carries it,
    // while a template may compute it instead of storing a field.
    let proposer = view
        .and_then(|v| field_party(v, "proposer"))
        .or_else(|| record.and_then(|r| field_party(r, "proposer")))
        .and_then(|p| match CantonId::parse(&p) {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::warn!(
                    "Proposal {cid} carries an unparseable proposer '{p}': {e}",
                    cid = created.contract_id
                );
                None
            }
        });

    let transfer = record.and_then(extract_transfer_proposal_details);
    // The template name is a poor label next to the view's `actionLabel`, but
    // it beats a generic placeholder and it needs no per-package knowledge.
    let action_label = view
        .and_then(|v| field_text(v, "actionLabel"))
        .or_else(|| record.and_then(|r| field_text(r, "actionLabel")))
        .or_else(|| created.template_id.as_ref().map(|t| t.entity_name.clone()));

    // `extract_service_request_details` matches on field shape — an `operator`
    // plus a `user` or a `provider` — so an unrelated proposal carrying those
    // names would yield a misleading party summary. Onboarding is only what
    // these two actions do, so gate on the label here and let every consumer
    // trust the field.
    let service_request = match action_label.as_deref() {
        Some("CreateUserServiceRequest") | Some("CreateProviderServiceRequest") => {
            record.and_then(extract_service_request_details)
        }
        _ => None,
    };

    // `AcceptTransferProposal`s carry `transferInstructionCid` instead of the
    // transfer fields. Capture it here; the post-pass in `fetch_proposal_infos`
    // resolves each cid to an `AcceptTransferDetails` via a per-cid event
    // query so the card can render sender/amount/instrument.
    let accept_transfer_instruction_cid =
        record.and_then(|r| match record_field(r, "transferInstructionCid") {
            Some(value::Sum::ContractId(cid)) => Some(cid.clone()),
            _ => None,
        });

    // Always record the cid, even when no description / transfer fields
    // are present — the consumer relies on map membership to gate
    // active-proposal filtering.
    proposal_infos.insert(
        created.contract_id.clone(),
        ProposalInfo {
            description,
            transfer,
            accept_transfer_instruction_cid,
            accept_transfer: None,
            service_request,
            action_label,
            proposer,
            created_at: created.created_at.as_ref().map(|t| t.seconds),
        },
    );
}

/// Pull sender/receiver/amount/instrument out of a `TransferInstruction`
/// interface view, *without* the status / deadline filters that
/// `extract_transfer_instruction_info` (used for the Accept dropdown) applies.
/// Pending-approval cards must render regardless of where the instruction is
/// in its lifecycle — the proposal is still being voted on, and the operator
/// needs to see what they're approving even if the underlying instruction has
/// already advanced or expired.
fn extract_accept_transfer_details_from_view(
    created: &CreatedEvent,
) -> Option<AcceptTransferDetails> {
    let view = created.interface_views.iter().find(|v| {
        v.interface_id.as_ref().is_some_and(|id| {
            id.module_name == "Splice.Api.Token.TransferInstructionV1"
                && id.entity_name == "TransferInstruction"
        })
    })?;
    let view_record = view.view_value.as_ref()?;
    let transfer_record = field_record(view_record, "transfer")?;
    let sender: CantonId = field_party(transfer_record, "sender")?.parse().ok()?;
    let receiver: CantonId = field_party(transfer_record, "receiver")?.parse().ok()?;
    let amount =
        field_numeric(transfer_record, "amount").and_then(|s| DamlDecimal::parse(&s).ok())?;
    let instrument_record = field_record(transfer_record, "instrumentId")?;
    let instrument_admin: CantonId = field_party(instrument_record, "admin")?.parse().ok()?;
    let instrument_id = field_text(instrument_record, "id")?;
    Some(AcceptTransferDetails {
        sender,
        receiver,
        amount,
        instrument_admin,
        instrument_id,
    })
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
        if let Some(details) = extract_accept_transfer_details_from_view(&created_event)
            && let Some(info) = proposal_infos.get_mut(&proposal_cid)
        {
            info.accept_transfer = Some(details);
        }
    }
    Ok(())
}

/// Pull `receiver`, `amount`, and the nested `instrumentId` out of a
/// `TransferProposal`'s `transfer` field. Returns `None` for any proposal
/// that doesn't have a `transfer` record (every non-transfer template).
fn extract_transfer_proposal_details(record: &Record) -> Option<TransferProposalDetails> {
    let transfer_record = field_record(record, "transfer")?;
    let receiver: CantonId = field_party(transfer_record, "receiver")?.parse().ok()?;
    let amount =
        field_numeric(transfer_record, "amount").and_then(|s| DamlDecimal::parse(&s).ok())?;
    let instrument_record = field_record(transfer_record, "instrumentId")?;
    let instrument_admin: CantonId = field_party(instrument_record, "admin")?.parse().ok()?;
    let instrument_id = field_text(instrument_record, "id")?;
    Some(TransferProposalDetails {
        receiver,
        amount,
        instrument_admin,
        instrument_id,
    })
}

/// Pull `operator` plus the counterparty (`user` for a
/// `CreateUserServiceRequest`, `provider` for a `CreateProviderServiceRequest`)
/// out of a service-request proposal's create-arguments. Returns `None` when
/// neither counterparty field is present, so non-service-request proposals are
/// left untouched. Both templates carry the parties as top-level `Party`
/// fields (unlike `TransferProposal`, which nests them under `transfer`).
fn extract_service_request_details(record: &Record) -> Option<ServiceRequestDetails> {
    let operator: CantonId = field_party(record, "operator")?.parse().ok()?;
    let user: Option<CantonId> = field_party(record, "user").and_then(|p| p.parse().ok());
    let provider: Option<CantonId> = field_party(record, "provider").and_then(|p| p.parse().ok());
    if user.is_none() && provider.is_none() {
        return None;
    }
    Some(ServiceRequestDetails {
        operator,
        user,
        provider,
    })
}

/// Fetch proposal infos via GovernableAction interface query (production mode).
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
    let Some(ref pkg) = packages.governance_action else {
        return Ok(());
    };

    let event_format = party_event_format(
        party_id,
        vec![interface_filter(
            Identifier {
                package_id: pkg.clone(),
                module_name: "Governance.Action".to_string(),
                entity_name: "GovernableAction".to_string(),
            },
            false,
        )],
        true,
    );

    for_each_active_contract(config, token.clone(), event_format, |created| {
        extract_proposal_info(&created, party_id, proposal_infos);
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
    for template in governance_state_templates(packages) {
        match fetch_governance_state_for_template(config, party_id, token.clone(), &template).await
        {
            Ok(Some(mut state)) => {
                // Found under the configured package — not out of date.
                state.package_ref = Some(template.package_id.clone());
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
                    template.module_name,
                    template.entity_name
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
        let template = TemplateId {
            package_id: format!("#{name}"),
            module_name: "Governance.Rules",
            entity_name: "GovernanceRules",
        };
        match fetch_governance_state_for_template(config, party_id, token.clone(), &template).await
        {
            Ok(Some(mut state)) => {
                tracing::warn!(
                    "GovernanceRules contract for {party_id} found under fallback package \
                     #{name} (configured {configured}); flagging as out of date"
                );
                state.package_ref = Some(template.package_id);
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
    template: &TemplateId,
) -> Result<Option<GovernanceState>> {
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

    Ok(fetch_first_active_contract(config, token, event_format)
        .await?
        .as_ref()
        .and_then(extract_governance_state))
}

/// Extract governance state from a VaultGovernanceRules or GovernanceRules created event
fn extract_governance_state(created: &CreatedEvent) -> Option<GovernanceState> {
    let record = created.create_arguments.as_ref()?;

    // Extract governance party (vaultManager for vault, governanceParty for core)
    let vault_manager: CantonId = field_party(record, "vaultManager")
        .or_else(|| field_party(record, "governanceParty"))
        .and_then(|p| p.parse().ok())?;

    // Extract members (Set Party - stored as GenMap<Party, Unit> inside a Record)
    let members: Vec<CantonId> = record_field(record, "members")
        .and_then(extract_party_set)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| s.parse().ok())
        .collect();

    // Extract threshold (Int)
    let threshold = match record_field(record, "threshold") {
        Some(value::Sum::Int64(i)) => Some(*i),
        _ => None,
    }
    .unwrap_or(0);

    // Extract actionConfirmationTimeout
    // VaultGovernanceRules: Optional RelTime; GovernanceRules: RelTime (non-optional)
    let timeout = record_field(record, "actionConfirmationTimeout")
        .and_then(|v| extract_optional_reltime(v).or_else(|| extract_reltime(v)));

    Some(GovernanceState {
        contract_id: created.contract_id.clone(),
        vault_manager,
        members,
        threshold,
        action_confirmation_timeout_microseconds: timeout,
        package_ref: None,
        out_of_date: false,
    })
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

/// Extract a Set Party (DA.Set.Types:Set) which is stored as Record { map: GenMap<Party, Unit> }
fn extract_party_set(sum: &value::Sum) -> Option<Vec<String>> {
    match sum {
        // Set Party is represented as a Record containing a GenMap, under a "map" field.
        value::Sum::Record(record) => record_field(record, "map").and_then(extract_genmap_parties),
        // Fallback: try as GenMap directly
        value::Sum::GenMap(gen_map) => Some(
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
fn extract_genmap_parties(sum: &value::Sum) -> Option<Vec<String>> {
    match sum {
        value::Sum::GenMap(gen_map) => Some(
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
fn extract_optional_reltime(sum: &value::Sum) -> Option<i64> {
    match sum {
        value::Sum::Optional(opt) => opt
            .value
            .as_deref()
            .and_then(|v| v.sum.as_ref())
            .and_then(extract_reltime),
        _ => None,
    }
}

/// Extract RelTime (stored as Record { microseconds: Int64 })
fn extract_reltime(sum: &value::Sum) -> Option<i64> {
    match sum {
        value::Sum::Record(record) => match record_field(record, "microseconds") {
            Some(value::Sum::Int64(i)) => Some(*i),
            _ => None,
        },
        // Fallback: try as Int64 directly
        value::Sum::Int64(i) => Some(*i),
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
    let vault_config = field_record(record, "vaultConfig")?;

    let (vault_name, share_symbol) = extract_vault_config(vault_config)?;

    // Extract isPaused (Bool)
    let is_paused = match record_field(record, "isPaused") {
        Some(value::Sum::Bool(b)) => Some(*b),
        _ => None,
    }
    .unwrap_or(false);

    // Extract vaultManager (Party)
    let vault_manager: CantonId =
        field_party(record, "vaultManager").and_then(|p| p.parse().ok())?;

    Some(VaultInfo {
        contract_id: created.contract_id.clone(),
        vault_name,
        share_symbol,
        is_paused,
        vault_manager,
    })
}

/// Extract vault name and share symbol from VaultConfig record
fn extract_vault_config(record: &Record) -> Option<(String, String)> {
    let name = field_text(record, "name")?;
    let share_symbol = field_text(record, "shareSymbol")?;
    Some((name, share_symbol))
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

    let operator: CantonId = field_party(record, "operator").and_then(|p| p.parse().ok())?;
    let provider: CantonId = field_party(record, "provider").and_then(|p| p.parse().ok())?;

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

    let operator: CantonId = field_party(record, "operator").and_then(|p| p.parse().ok())?;
    let user: CantonId = field_party(record, "user").and_then(|p| p.parse().ok())?;

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

    let has_billing_params = match record_field(record, "billingParams") {
        Some(value::Sum::Optional(opt)) => opt.value.is_some(),
        _ => false,
    };

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

    let claims = match record_field(record, "claims") {
        Some(value::Sum::List(l)) => Some(&l.elements),
        _ => None,
    }
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

    let operator: CantonId = field_party(record, "operator").and_then(|p| p.parse().ok())?;
    let registrar: CantonId = field_party(record, "registrar").and_then(|p| p.parse().ok())?;

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
    match record_field(record, label) {
        Some(value::Sum::Optional(opt)) => {
            opt.value.as_deref().and_then(|inner| match &inner.sum {
                Some(value::Sum::Bool(b)) => Some(*b),
                _ => None,
            })
        }
        _ => None,
    }
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

    let default_identifier = field_record(record, "defaultIdentifier")?;

    let instrument_admin: CantonId =
        field_party(default_identifier, "source").and_then(|p| p.parse().ok())?;

    let instrument_id: String = field_text(default_identifier, "id")?;

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

/// Uses WildcardFilter in test mode, TemplateFilter or InterfaceFilter in production.
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
    let status_variant = match record_field(view_record, "status") {
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
                .and_then(|r| record_field(r, "pendingActions"))
                .map(extract_pending_actions)
                .unwrap_or_default();
            (TransferInstructionStatus::PendingInternalWorkflow, actions)
        }
        _ => return None,
    };

    let transfer_record = field_record(view_record, "transfer")?;

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

    let instrument_record = field_record(transfer_record, "instrumentId")?;
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
    let payload = field_record(record, payload_field)?;

    let holder: CantonId = field_party(payload, "holder")?.parse().ok()?;
    let amount = field_numeric(payload, "amount").and_then(|s| DamlDecimal::parse(&s).ok())?;

    let instrument_record = field_record(payload, "instrumentId")?;
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
    let Some(payload) = field_record(record, payload_field) else {
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
fn extract_pending_actions(sum: &value::Sum) -> Vec<PendingAction> {
    let entries = match sum {
        value::Sum::GenMap(m) => &m.entries,
        value::Sum::TextMap(_) => return Vec::new(), // party-keyed maps come as GenMap
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

fn field_party(record: &Record, label: &str) -> Option<String> {
    match record_field(record, label) {
        Some(value::Sum::Party(p)) => Some(p.clone()),
        _ => None,
    }
}

fn field_text(record: &Record, label: &str) -> Option<String> {
    match record_field(record, label) {
        Some(value::Sum::Text(t)) => Some(t.clone()),
        _ => None,
    }
}

fn field_numeric(record: &Record, label: &str) -> Option<String> {
    match record_field(record, label) {
        Some(value::Sum::Numeric(n)) => Some(n.clone()),
        _ => None,
    }
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

fn field_timestamp(record: &Record, label: &str) -> Option<i64> {
    match record_field(record, label) {
        Some(value::Sum::Timestamp(t)) => Some(*t),
        _ => None,
    }
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

    let instrument_record = field_record(view_record, "instrumentId")?;
    let instrument_admin: CantonId = field_party(instrument_record, "admin")?.parse().ok()?;
    let instrument_id = field_text(instrument_record, "id")?;

    // `lock : Optional Lock` — present (`Some`) means the holding is locked for
    // an in-flight transfer/allocation. A missing field is treated as unlocked.
    let is_locked = match record_field(view_record, "lock") {
        Some(value::Sum::Optional(opt)) => opt.value.is_some(),
        _ => false,
    };

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
    let allowances = match record_field(args, "instrumentAllowances") {
        Some(value::Sum::List(l)) => Some(&l.elements),
        _ => None,
    };
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
    use canton_proto_rs::com::daml::ledger::api::v2::{
        Value,
        admin::{ObjectMeta, PartyDetails},
    };

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

    // ------------------------------------------------------------------------
    // extract_service_request_details
    //
    // CreateUserServiceRequest / CreateProviderServiceRequest carry operator +
    // user/provider as top-level Party fields on the proposal contract. The
    // notification card renders operator + the present counterparty so the
    // operator sees the full summary alongside the action_label.
    // ------------------------------------------------------------------------

    // `<prefix>::<34-byte-multihash-hex>`; CantonId::parse rejects other shapes.
    const SR_FP: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

    fn service_request_record(fields: Vec<RecordField>) -> Record {
        Record {
            record_id: None,
            fields,
        }
    }

    #[test]
    fn extract_service_request_details_reads_user_request() {
        let record = service_request_record(vec![
            field("governanceParty", party_value(&format!("gov::{SR_FP}"))),
            field("proposer", party_value(&format!("proposer::{SR_FP}"))),
            field("operator", party_value(&format!("operator::{SR_FP}"))),
            field("user", party_value(&format!("user::{SR_FP}"))),
        ]);
        let Some(details) = extract_service_request_details(&record) else {
            panic!("user service request should yield details");
        };
        assert_eq!(details.operator.to_string(), format!("operator::{SR_FP}"));
        assert_eq!(
            details.user.map(|p| p.to_string()),
            Some(format!("user::{SR_FP}")),
        );
        assert!(details.provider.is_none());
    }

    #[test]
    fn extract_service_request_details_reads_provider_request() {
        let record = service_request_record(vec![
            field("governanceParty", party_value(&format!("gov::{SR_FP}"))),
            field("proposer", party_value(&format!("proposer::{SR_FP}"))),
            field("operator", party_value(&format!("operator::{SR_FP}"))),
            field("provider", party_value(&format!("provider::{SR_FP}"))),
        ]);
        let Some(details) = extract_service_request_details(&record) else {
            panic!("provider service request should yield details");
        };
        assert_eq!(details.operator.to_string(), format!("operator::{SR_FP}"));
        assert_eq!(
            details.provider.map(|p| p.to_string()),
            Some(format!("provider::{SR_FP}")),
        );
        assert!(details.user.is_none());
    }

    #[test]
    fn extract_service_request_details_skips_proposal_without_counterparty() {
        // operator present but no user/provider counterparty → not a service
        // request, so no details (keeps unrelated operator-bearing proposals
        // from rendering a half-empty summary).
        let record = service_request_record(vec![
            field("governanceParty", party_value(&format!("gov::{SR_FP}"))),
            field("proposer", party_value(&format!("proposer::{SR_FP}"))),
            field("operator", party_value(&format!("operator::{SR_FP}"))),
        ]);
        assert!(extract_service_request_details(&record).is_none());
    }

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

    // ------------------------------------------------------------------------
    // extract_proposal_info / build_domain_actions
    //
    // Covers the pending-approvals fix: every active GovernableAction should
    // surface, confirmations or not, using only the interface view.
    // ------------------------------------------------------------------------

    fn bare_created_event(contract_id: &str) -> CreatedEvent {
        CreatedEvent {
            offset: 0,
            node_id: 0,
            contract_id: contract_id.to_string(),
            template_id: None,
            contract_key: None,
            create_arguments: None,
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

    /// A created event as the production `InterfaceFilter` query returns it:
    /// the `GovernableAction` view is present and nothing else is.
    fn governable_action_view_event(action_label: &str, description: &str) -> CreatedEvent {
        let view = InterfaceView {
            interface_id: Some(Identifier {
                package_id: "#governance-action-v1".to_string(),
                module_name: "Governance.Action".to_string(),
                entity_name: "GovernableAction".to_string(),
            }),
            view_status: None,
            view_value: Some(Record {
                record_id: None,
                fields: vec![
                    field("actionLabel", text_value(action_label)),
                    field("description", text_value(description)),
                    field("governanceParty", party_value(&format!("gov::{SR_FP}"))),
                ],
            }),
            implementation_package_id: String::new(),
        };
        CreatedEvent {
            interface_views: vec![view],
            ..bare_created_event("proposal-cid")
        }
    }

    #[test]
    fn governable_action_view_reads_the_view_record() {
        let event = governable_action_view_event("SetupCcPreapproval", "set up the preapproval");
        let Some(view) = governable_action_view(&event) else {
            panic!("the GovernableAction view should be found");
        };
        assert_eq!(
            field_text(view, "actionLabel"),
            Some("SetupCcPreapproval".to_string())
        );
    }

    #[test]
    fn governable_action_view_absent_when_no_matching_view() {
        let mut event = governable_action_view_event("SetupCcPreapproval", "");
        event.interface_views.clear();
        assert!(governable_action_view(&event).is_none());
    }

    #[test]
    fn extract_proposal_info_captures_a_proposal_from_its_view_alone() -> Result {
        // An `InterfaceFilter` query populates `interface_views` and leaves
        // `create_arguments` to the template filter, which this query has none
        // of. The view alone must be enough.
        let event = governable_action_view_event("CreateUserServiceRequest", "onboard the user");
        let mut infos = HashMap::new();

        extract_proposal_info(&event, &gov_party()?, &mut infos);

        let Some(info) = infos.get("proposal-cid") else {
            panic!("a view-only proposal should be captured");
        };
        assert_eq!(
            info.action_label,
            Some("CreateUserServiceRequest".to_string())
        );
        assert_eq!(info.description, Some("onboard the user".to_string()));

        Ok(())
    }

    #[test]
    fn extract_proposal_info_prefers_the_view_description() -> Result {
        // Templates such as CreateUserServiceRequest compute `description` in
        // the view and hold no field of that name, so the view must win.
        let mut event = governable_action_view_event("MintProposal", "computed in the view");
        event.create_arguments = Some(Record {
            record_id: None,
            fields: vec![
                field("governanceParty", party_value(&format!("gov::{SR_FP}"))),
                field("proposer", party_value(&format!("proposer::{SR_FP}"))),
                field("description", text_value("stored on the template")),
            ],
        });
        let mut infos = HashMap::new();

        extract_proposal_info(&event, &gov_party()?, &mut infos);

        assert_eq!(
            infos
                .get("proposal-cid")
                .and_then(|i| i.description.clone()),
            Some("computed in the view".to_string())
        );

        Ok(())
    }

    #[test]
    fn extract_proposal_info_captures_a_proposal_from_an_unknown_package() -> Result {
        // The visibility rule: a package decman has never heard of, whose
        // template names its own fields differently, still gets a card. No
        // allowlist of labels or templates may gate the pending path.
        let event = governable_action_view_event("VaultPause", "pause the vault");
        let mut infos = HashMap::new();
        extract_proposal_info(&event, &gov_party()?, &mut infos);

        let actions = build_domain_actions(HashMap::new(), infos, true, true, 2, 0);

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].action_label, "VaultPause");
        assert_eq!(actions[0].confirmation_count, 0);

        Ok(())
    }

    #[test]
    fn extract_proposal_info_labels_a_wildcard_proposal_with_its_template_name() -> Result {
        // Test mode fetches with a wildcard filter, so no view arrives and the
        // template name is the only name available.
        let mut event = bare_created_event("proposal-cid");
        event.template_id = Some(Identifier {
            package_id: "#governance-utility-onboarding".to_string(),
            module_name: "Governance.TokenIssuance.RegistrarDelegation".to_string(),
            entity_name: "RegistrarDelegationProposal".to_string(),
        });
        event.create_arguments = Some(Record {
            record_id: None,
            fields: vec![
                field("governanceParty", party_value(&format!("gov::{SR_FP}"))),
                field("proposer", party_value(&format!("proposer::{SR_FP}"))),
            ],
        });
        let mut infos = HashMap::new();

        extract_proposal_info(&event, &gov_party()?, &mut infos);

        assert_eq!(
            infos
                .get("proposal-cid")
                .and_then(|i| i.action_label.clone()),
            Some("RegistrarDelegationProposal".to_string())
        );

        Ok(())
    }

    #[test]
    fn extract_proposal_info_prefers_the_view_proposer() -> Result {
        // The retract button keys on this field, and the interface declares
        // `proposer`, so the view is the authoritative source.
        let mut event = governable_action_view_event("MintProposal", "mint some tokens");
        if let Some(view) = event
            .interface_views
            .first_mut()
            .and_then(|v| v.view_value.as_mut())
        {
            view.fields.push(field(
                "proposer",
                party_value(&format!("from-view::{SR_FP}")),
            ));
        }
        event.create_arguments = Some(Record {
            record_id: None,
            fields: vec![
                field("governanceParty", party_value(&format!("gov::{SR_FP}"))),
                field("proposer", party_value(&format!("from-args::{SR_FP}"))),
            ],
        });
        let mut infos = HashMap::new();

        extract_proposal_info(&event, &gov_party()?, &mut infos);

        let Some(proposer) = infos.get("proposal-cid").and_then(|i| i.proposer.as_ref()) else {
            panic!("the proposer should be captured");
        };
        assert_eq!(proposer.to_string(), format!("from-view::{SR_FP}"));

        Ok(())
    }

    #[test]
    fn extract_proposal_info_falls_back_to_the_create_argument_proposer() -> Result {
        // A wildcard fetch carries no view, so the raw field is all there is.
        let mut event = bare_created_event("proposal-cid");
        event.create_arguments = Some(Record {
            record_id: None,
            fields: vec![
                field("governanceParty", party_value(&format!("gov::{SR_FP}"))),
                field("proposer", party_value(&format!("from-args::{SR_FP}"))),
            ],
        });
        let mut infos = HashMap::new();

        extract_proposal_info(&event, &gov_party()?, &mut infos);

        let Some(proposer) = infos.get("proposal-cid").and_then(|i| i.proposer.as_ref()) else {
            panic!("the proposer should come from the create arguments");
        };
        assert_eq!(proposer.to_string(), format!("from-args::{SR_FP}"));

        Ok(())
    }

    #[test]
    fn extract_proposal_info_skips_a_proposal_governed_by_another_party() -> Result {
        // Seeing a proposal is not governing it. Another package may name our
        // party as an observer while a different governance party controls the
        // action, and Confirm there would be rejected on-ledger.
        let mut event = governable_action_view_event("VaultPause", "pause the vault");
        if let Some(view) = event
            .interface_views
            .first_mut()
            .and_then(|v| v.view_value.as_mut())
        {
            view.fields.retain(|f| f.label != "governanceParty");
            view.fields.push(field(
                "governanceParty",
                party_value(&format!("someone-else::{SR_FP}")),
            ));
        }
        let mut infos = HashMap::new();

        extract_proposal_info(&event, &gov_party()?, &mut infos);

        assert!(infos.is_empty());

        Ok(())
    }

    #[test]
    fn extract_proposal_info_captures_created_at() -> Result {
        // The feed sorts on this, so an unconfirmed card holds its place
        // between refreshes instead of shuffling.
        let mut event = governable_action_view_event("MintProposal", "mint some tokens");
        event.created_at = Some(prost_types::Timestamp {
            seconds: 1_700_000_500,
            nanos: 0,
        });
        let mut infos = HashMap::new();

        extract_proposal_info(&event, &gov_party()?, &mut infos);

        assert_eq!(
            infos.get("proposal-cid").and_then(|i| i.created_at),
            Some(1_700_000_500)
        );

        Ok(())
    }

    #[test]
    fn extract_proposal_info_gates_service_request_details_on_the_label() -> Result {
        // The party fields alone must not produce a service-request summary.
        // Onboarding is only what the two Create*ServiceRequest actions do.
        let mut event = governable_action_view_event("MintProposal", "mint some tokens");
        event.create_arguments = Some(Record {
            record_id: None,
            fields: vec![
                field("governanceParty", party_value(&format!("gov::{SR_FP}"))),
                field("proposer", party_value(&format!("proposer::{SR_FP}"))),
                field("operator", party_value(&format!("operator::{SR_FP}"))),
                field("user", party_value(&format!("user::{SR_FP}"))),
            ],
        });
        let mut infos = HashMap::new();

        extract_proposal_info(&event, &gov_party()?, &mut infos);

        let Some(info) = infos.get("proposal-cid") else {
            panic!("the proposal should still be captured");
        };
        assert!(info.service_request.is_none());

        Ok(())
    }

    #[test]
    fn extract_proposal_info_keeps_service_request_details_on_a_matching_label() -> Result {
        let mut event =
            governable_action_view_event("CreateUserServiceRequest", "onboard the user");
        event.create_arguments = Some(Record {
            record_id: None,
            fields: vec![
                field("governanceParty", party_value(&format!("gov::{SR_FP}"))),
                field("proposer", party_value(&format!("proposer::{SR_FP}"))),
                field("operator", party_value(&format!("operator::{SR_FP}"))),
                field("user", party_value(&format!("user::{SR_FP}"))),
            ],
        });
        let mut infos = HashMap::new();

        extract_proposal_info(&event, &gov_party()?, &mut infos);

        let Some(details) = infos
            .get("proposal-cid")
            .and_then(|i| i.service_request.as_ref())
        else {
            panic!("a user service request should carry its parties");
        };
        assert_eq!(details.operator.to_string(), format!("operator::{SR_FP}"));

        Ok(())
    }

    #[test]
    fn extract_proposal_info_skips_an_unrelated_wildcard_contract() -> Result {
        let mut event = bare_created_event("other-cid");
        event.create_arguments = Some(Record {
            record_id: None,
            fields: vec![field("owner", party_value(&format!("owner::{SR_FP}")))],
        });
        let mut infos = HashMap::new();

        extract_proposal_info(&event, &gov_party()?, &mut infos);

        assert!(infos.is_empty());

        Ok(())
    }

    fn proposal_info(action_label: Option<&str>) -> Result<ProposalInfo> {
        Ok(ProposalInfo {
            description: Some("a description".to_string()),
            transfer: None,
            accept_transfer_instruction_cid: None,
            accept_transfer: None,
            service_request: None,
            action_label: action_label.map(str::to_string),
            proposer: Some(CantonId::parse(&format!("proposer::{SR_FP}"))?),
            created_at: Some(1_700_000_000),
        })
    }

    /// The decentralized party the proposal tests query for. Every fixture
    /// names this as its `governanceParty`, so nothing is dropped as foreign.
    fn gov_party() -> Result<CantonId> {
        CantonId::parse(&format!("gov::{SR_FP}"))
    }

    fn confirmation(confirming_party: &str) -> Result<GovernanceConfirmation> {
        Ok(GovernanceConfirmation {
            contract_id: format!("confirmation-{confirming_party}"),
            action: ActionType::GovernanceSetThreshold { new_threshold: 0 },
            confirming_party: CantonId::parse(&format!(
                "{confirming_party}::1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            ))?,
            created_at: 0,
            expires_at: 0,
        })
    }

    #[test]
    fn build_domain_actions_synthesizes_a_card_for_an_unconfirmed_proposal() -> Result {
        let domain_confirmations = HashMap::new();
        let mut proposal_infos = HashMap::new();
        proposal_infos.insert(
            "new-proposal-cid".to_string(),
            proposal_info(Some("SetupCcPreapproval"))?,
        );

        let actions = build_domain_actions(domain_confirmations, proposal_infos, true, true, 2, 0);

        assert_eq!(actions.len(), 1);
        let action = &actions[0];
        assert_eq!(action.proposal_cid, "new-proposal-cid");
        assert_eq!(action.action_label, "SetupCcPreapproval");
        assert_eq!(action.confirmation_count, 0);
        assert!(action.confirmations.is_empty());
        assert!(!action.can_execute);
        assert!(!action.orphaned);
        // The loop assigns every remaining field by hand, so each one is
        // asserted. Without this a swapped assignment would still pass.
        assert_eq!(action.description, Some("a description".to_string()));
        assert_eq!(
            action.proposer.as_ref().map(ToString::to_string),
            Some(format!("proposer::{SR_FP}"))
        );
        assert_eq!(action.created_at, Some(1_700_000_000));
        assert!(action.transfer_details.is_none());
        assert!(action.accept_transfer_details.is_none());
        assert!(action.service_request_details.is_none());

        Ok(())
    }

    #[test]
    fn build_domain_actions_skips_synthesis_when_confirmations_are_incomplete() -> Result {
        // A failed confirmation query leaves a confirmed proposal looking
        // untouched. Synthesizing it would offer Confirm to a member who has
        // already confirmed, so nothing is synthesized until a clean read.
        let mut proposal_infos = HashMap::new();
        proposal_infos.insert(
            "new-proposal-cid".to_string(),
            proposal_info(Some("SetupCcPreapproval"))?,
        );

        let actions = build_domain_actions(HashMap::new(), proposal_infos, true, false, 2, 0);

        assert!(actions.is_empty());

        Ok(())
    }

    #[test]
    fn build_domain_actions_leftover_label_falls_back_when_absent() -> Result {
        let domain_confirmations = HashMap::new();
        let mut proposal_infos = HashMap::new();
        proposal_infos.insert("new-proposal-cid".to_string(), proposal_info(None)?);

        let actions = build_domain_actions(domain_confirmations, proposal_infos, true, true, 2, 0);

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].action_label, FALLBACK_PROPOSAL_LABEL);

        Ok(())
    }

    #[test]
    fn build_domain_actions_does_not_duplicate_an_enriched_proposal() -> Result {
        let mut domain_confirmations = HashMap::new();
        domain_confirmations.insert(
            "confirmed-cid".to_string(),
            (
                "SetupCcPreapproval".to_string(),
                vec![confirmation("alice")?],
            ),
        );
        let mut proposal_infos = HashMap::new();
        proposal_infos.insert(
            "confirmed-cid".to_string(),
            proposal_info(Some("SetupCcPreapproval"))?,
        );

        let actions = build_domain_actions(domain_confirmations, proposal_infos, true, true, 2, 0);

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].confirmation_count, 1);

        Ok(())
    }

    #[test]
    fn build_domain_actions_skips_synthesis_on_incomplete_fetch() -> Result {
        let domain_confirmations = HashMap::new();
        let mut proposal_infos = HashMap::new();
        proposal_infos.insert(
            "new-proposal-cid".to_string(),
            proposal_info(Some("SetupCcPreapproval"))?,
        );

        let actions = build_domain_actions(domain_confirmations, proposal_infos, false, true, 2, 0);

        assert!(actions.is_empty());

        Ok(())
    }

    #[test]
    fn build_domain_actions_does_not_orphan_confirmations_on_incomplete_fetch() -> Result {
        let mut domain_confirmations = HashMap::new();
        domain_confirmations.insert(
            "missing-cid".to_string(),
            (
                "SetupCcPreapproval".to_string(),
                vec![confirmation("alice")?],
            ),
        );
        let proposal_infos = HashMap::new();

        let actions = build_domain_actions(domain_confirmations, proposal_infos, false, true, 2, 0);

        assert_eq!(actions.len(), 1);
        assert!(!actions[0].orphaned);

        Ok(())
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
