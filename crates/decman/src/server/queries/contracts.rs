//! Contract lists, party metadata and the generic contract query.

use std::{cmp::Ordering, collections::HashMap, future::Future};

use base64::{Engine, engine::general_purpose::STANDARD};
use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        CreatedEvent, Identifier,
        admin::{ListKnownPartiesRequest, ListKnownPartiesResponse},
    },
    digitalasset::canton::admin::participant::v30::{
        ListPackagesRequest, package_service_client::PackageServiceClient,
    },
};

use super::{templates::TemplateId, transfers::is_execute_before_expired};
use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
    error::Result,
    server::{
        chain_audit::record_to_json,
        event_filters::{interface_filter, party_event_format, template_filter},
        ledger_paging::{FETCH_CHUNK, fetch_active_contracts_filtered, for_each_active_contract},
        types::{ContractInfo, ContractWithBlob, PartyMetadata},
    },
    utils,
};

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
    package_versions: &HashMap<String, String>,
    templates: &[TemplateId],
) -> Result<Vec<ContractInfo>> {
    let _ = packages;
    let mut contracts = Vec::new();

    {
        // One query per template, so a package missing from this participant
        // degrades to "no contracts of that type" instead of failing the read.
        tracing::debug!("Using TemplateFilter for contracts query (per-template)");
        for t in templates {
            match fetch_contracts_for_template(
                config,
                party_id,
                token.clone(),
                t,
                package_versions,
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

pub(crate) fn compare_versions(a: &str, b: &str) -> Ordering {
    let mut ai = a.split('.');
    let mut bi = b.split('.');
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(xn), Ok(yn)) => xn.cmp(&yn),
                    _ => x.cmp(y),
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

/// A `{package_id → version}` map from the participant's Admin PackageService.
///
/// The Ledger API returns only `package_name` on a created event; version
/// metadata lives on the Admin `PackageService`. This is a whole-participant
/// read, identical for every party, so a caller reading many parties must fetch
/// it **once** and pass it to each [`get_contracts`] call — one inventory in
/// flight per party is what OOMKilled a node (#415).
pub(crate) async fn fetch_package_versions(config: &NodeConfig) -> Result<HashMap<String, String>> {
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

    for_each_active_contract(config, token, event_format, None, |created| {
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
    /// When true, fill each result's `payload` with its decoded fields. See
    /// [`contract_payload`].
    pub include_payload: bool,
}

/// A contract's decoded fields for `include_payload`.
///
/// Canton sends create arguments through a template filter only; an interface
/// filter carries the computed view instead. So a template query gets the
/// create arguments and an interface query gets the view of the interface it
/// asked for, which is also the shape the caller filtered on. Field labels are
/// present because the query reads verbose events. A view that failed to
/// compute has no value and renders as `null`.
fn contract_payload(created: &CreatedEvent, params: &ContractQueryParams) -> serde_json::Value {
    if !params.use_interface_filter {
        return record_to_json(&created.create_arguments);
    }
    created
        .interface_views
        .iter()
        .find(|view| {
            view.interface_id.as_ref().is_some_and(|id| {
                id.module_name == params.module_name && id.entity_name == params.entity_name
            })
        })
        .map_or(serde_json::Value::Null, |view| {
            record_to_json(&view.view_value)
        })
}

/// Uses TemplateFilter or InterfaceFilter, chosen by `params.use_interface_filter`.
pub async fn query_contracts_by_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    params: &ContractQueryParams,
) -> Result<Vec<ContractWithBlob>> {
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

        let blob = STANDARD.encode(&created.created_event_blob);
        let payload = params
            .include_payload
            .then(|| contract_payload(&created, params));
        Some(ContractWithBlob {
            contract_id: created.contract_id,
            blob,
            payload,
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::{cmp::Ordering, collections::VecDeque};

    use canton_proto_rs::com::daml::ledger::api::v2::{
        InterfaceView, List, Record, RecordField, Value,
        admin::{ObjectMeta, PartyDetails},
        value,
    };

    use super::{super::test_values::*, *};

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
        assert_eq!(compare_versions("0.1.18", "0.1.7"), Ordering::Greater);
        assert_eq!(compare_versions("1.0.0", "0.99.99"), Ordering::Greater);
        assert_eq!(compare_versions("1.0.0", "1.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.0", "1.0.0"), Ordering::Less);
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
        let mut remaining = VecDeque::from(pages);

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

    // ------------------------------------------------------------------------
    // contract_payload (`/contracts/query?include_payload=true`)
    // ------------------------------------------------------------------------

    fn payload_params(
        module_name: &str,
        entity_name: &str,
        interface: bool,
    ) -> ContractQueryParams {
        ContractQueryParams {
            package_id: "#pkg".to_string(),
            module_name: module_name.to_string(),
            entity_name: entity_name.to_string(),
            use_interface_filter: interface,
            active_only: false,
            include_payload: true,
        }
    }

    fn view_of(module_name: &str, entity_name: &str, fields: Vec<RecordField>) -> InterfaceView {
        InterfaceView {
            interface_id: Some(Identifier {
                package_id: "#pkg".to_string(),
                module_name: module_name.to_string(),
                entity_name: entity_name.to_string(),
            }),
            view_status: None,
            view_value: Some(Record {
                record_id: None,
                fields,
            }),
            implementation_package_id: String::new(),
        }
    }

    /// A template query returns the create arguments, with nested records,
    /// variants and lists decoded the way the chain audit renders them.
    #[test]
    fn template_payload_is_the_create_arguments() {
        let created = CreatedEvent {
            contract_id: "batch-cid".to_string(),
            create_arguments: Some(Record {
                record_id: None,
                fields: vec![
                    field("operator", party_value("operator::1220aa")),
                    field("batchId", text_value("window-1")),
                    field("totalRequested", numeric_value("1000.0")),
                    field(
                        "rows",
                        Value {
                            sum: Some(value::Sum::List(List {
                                elements: vec![record_value(vec![
                                    field("requestId", text_value("r-1")),
                                    field("allocatedUnits", numeric_value("240.0")),
                                ])],
                            })),
                        },
                    ),
                    field("stage", variant_value("Open", unit_value())),
                ],
            }),
            ..Default::default()
        };

        let payload = contract_payload(&created, &payload_params("Fund.Batch", "Batch", false));

        assert_eq!(
            payload,
            serde_json::json!({
                "operator": "operator::1220aa",
                "batchId": "window-1",
                "totalRequested": "1000.0",
                "rows": [{ "requestId": "r-1", "allocatedUnits": "240.0" }],
                "stage": { "_variant": "Open", "value": {} },
            })
        );
    }

    /// Canton sends no create arguments through an interface filter, so an
    /// interface query returns the view — of the interface it asked for, even
    /// when the contract implements others the party can also see.
    #[test]
    fn interface_payload_is_the_queried_interface_view() {
        let created = CreatedEvent {
            contract_id: "holding-cid".to_string(),
            interface_views: vec![
                view_of(
                    "Other.Api",
                    "Lockable",
                    vec![field("locked", text_value("no"))],
                ),
                view_of(
                    "Splice.Api.Token.HoldingV1",
                    "Holding",
                    vec![
                        field("owner", party_value("owner::1220aa")),
                        field("amount", numeric_value("12.5")),
                    ],
                ),
            ],
            ..Default::default()
        };

        let payload = contract_payload(
            &created,
            &payload_params("Splice.Api.Token.HoldingV1", "Holding", true),
        );

        assert_eq!(
            payload,
            serde_json::json!({ "owner": "owner::1220aa", "amount": "12.5" })
        );
    }

    /// No view for the queried interface (or one that failed to compute and
    /// carries no value) is `null`, never another interface's view.
    #[test]
    fn interface_payload_without_a_matching_view_is_null() {
        let mut failed = view_of("Splice.Api.Token.HoldingV1", "Holding", vec![]);
        failed.view_value = None;
        let created = CreatedEvent {
            interface_views: vec![
                view_of(
                    "Other.Api",
                    "Lockable",
                    vec![field("locked", text_value("no"))],
                ),
                failed,
            ],
            ..Default::default()
        };
        let params = payload_params("Splice.Api.Token.HoldingV1", "Holding", true);
        assert_eq!(contract_payload(&created, &params), serde_json::Value::Null);

        let unrelated = CreatedEvent {
            interface_views: vec![view_of("Other.Api", "Lockable", vec![])],
            ..Default::default()
        };
        assert_eq!(
            contract_payload(&unrelated, &params),
            serde_json::Value::Null
        );
    }

    /// Callers that don't ask for the payload get the response they always
    /// had: no `payload` key at all.
    #[test]
    fn contract_without_payload_serializes_as_before() {
        let contract = ContractWithBlob {
            contract_id: "cid".to_string(),
            blob: "YmxvYg==".to_string(),
            payload: None,
        };
        assert_eq!(
            serde_json::to_value(&contract).expect("serializes"),
            serde_json::json!({ "contract_id": "cid", "blob": "YmxvYg==" })
        );
    }
}
