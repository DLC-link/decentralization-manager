use std::{collections::HashMap, future::Future};

use anyhow::{Context, Result};
use canton_proto_rs::com::daml::ledger::api::v2::{
    CumulativeFilter, GetLatestPrunedOffsetsRequest, GetLedgerEndRequest, Identifier, Record,
    Transaction, TransactionFormat, TransactionShape, UpdateFormat, Value, event::Event, value,
};
use serde_json::{Value as JsonValue, json};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
    utils,
};

use super::{
    event_filters::{interface_filter, party_event_format, template_filter, wildcard_filter},
    ledger_paging::{TransactionPage, fetch_transactions_page},
    package_inventory::{fetch_package_names, matching_names, package_name_prefix},
    types::ChainAuditEntry,
};

struct ChainTemplate {
    package_prefix: String,
    module_name: &'static str,
    entity_name: &'static str,
    governance_type: &'static str,
}

struct ChainInterface {
    package_prefix: String,
    module_name: &'static str,
    entity_name: &'static str,
    governance_type: &'static str,
}

struct ChainFilters {
    templates: Vec<ChainTemplate>,
    interfaces: Vec<ChainInterface>,
}

/// The list of governance Daml types we care about, each tagged with the
/// stable name prefix of the package family that defines it. Ledger queries
/// are filtered Canton-side by package-name references resolved from the
/// participant's own inventory, and events are classified client-side purely
/// by `(module_name, entity_name)`, so the audit trail covers events from
/// any package version (rc3, rc4, future). `packages` is kept as an argument
/// so a build that omits some governance kinds (vault / core / cbtc) still
/// skips them entirely.
fn chain_filters(packages: &PackageConfig) -> ChainFilters {
    let mut templates = Vec::new();
    let mut interfaces = Vec::new();

    if let Some(pkg) = &packages.vault_governance {
        let prefix = package_name_prefix(pkg);
        templates.push(ChainTemplate {
            package_prefix: prefix.clone(),
            module_name: "BitsafeVault.VaultGovernance",
            entity_name: "VaultGovernanceRules",
            governance_type: "vault",
        });
        templates.push(ChainTemplate {
            package_prefix: prefix,
            module_name: "BitsafeVault.VaultGovernance",
            entity_name: "VaultGovernanceConfirmation",
            governance_type: "vault",
        });
    }

    if let Some(pkg) = &packages.governance_core {
        let prefix = package_name_prefix(pkg);
        templates.push(ChainTemplate {
            package_prefix: prefix.clone(),
            module_name: "Governance.Rules",
            entity_name: "GovernanceRules",
            governance_type: "core_self",
        });
        templates.push(ChainTemplate {
            package_prefix: prefix.clone(),
            module_name: "Governance.Rules",
            entity_name: "GovernanceSelfConfirmation",
            governance_type: "core_self",
        });
        templates.push(ChainTemplate {
            package_prefix: prefix.clone(),
            module_name: "Governance.Confirmation",
            entity_name: "GovernanceConfirmation",
            governance_type: "core_domain",
        });
        templates.push(ChainTemplate {
            package_prefix: prefix,
            module_name: "Governance.ExecutionResult",
            entity_name: "GovernanceExecutionResult",
            governance_type: "core_domain",
        });
    }

    if let Some(pkg) = &packages.governance_action {
        interfaces.push(ChainInterface {
            package_prefix: package_name_prefix(pkg),
            module_name: "Governance.Action",
            entity_name: "GovernableAction",
            governance_type: "core_domain",
        });
    }

    templates.push(ChainTemplate {
        package_prefix: "cbtc-governance".to_string(),
        module_name: "CBTC.Governance",
        entity_name: "CBTCGovernanceRules",
        governance_type: "cbtc",
    });
    templates.push(ChainTemplate {
        package_prefix: "cbtc-governance".to_string(),
        module_name: "CBTC.Governance",
        entity_name: "Confirmation",
        governance_type: "cbtc",
    });

    ChainFilters {
        templates,
        interfaces,
    }
}

/// Build Canton-side `CumulativeFilter`s for the governance templates using
/// package-name references taken from the participant's own package
/// inventory. Referencing only names the participant actually knows avoids
/// the "Packages not found on participant" failure that unresolvable
/// package-name references cause, while still covering events from renamed
/// historical packages whose DARs remain uploaded.
fn build_canton_filters(filters: &ChainFilters, package_names: &[String]) -> Vec<CumulativeFilter> {
    let mut cumulative = Vec::new();

    for t in &filters.templates {
        for name in matching_names(package_names, &t.package_prefix) {
            cumulative.push(template_filter(
                Identifier {
                    package_id: format!("#{name}"),
                    module_name: t.module_name.to_string(),
                    entity_name: t.entity_name.to_string(),
                },
                false,
            ));
        }
    }

    for i in &filters.interfaces {
        for name in matching_names(package_names, &i.package_prefix) {
            cumulative.push(interface_filter(
                Identifier {
                    package_id: format!("#{name}"),
                    module_name: i.module_name.to_string(),
                    entity_name: i.entity_name.to_string(),
                },
                false,
            ));
        }
    }

    cumulative
}

/// The wildcard fallback filter: every event for the party, classified and
/// trimmed client-side.
fn wildcard_filters() -> Vec<CumulativeFilter> {
    vec![wildcard_filter(false)]
}

/// Whether an entry is a governance action worth showing in the audit trail:
/// proposals, confirmations, executions and their outcomes. `create`
/// (downstream contract creations) and `other` (unrelated choices) are
/// subevents the trail should not show.
fn is_governance_entry(entry: &ChainAuditEntry) -> bool {
    matches!(
        entry.event_type.as_str(),
        "propose" | "confirm" | "execute" | "expire" | "cancel" | "execute_result"
    )
}

fn classify_choice(choice: &str) -> String {
    let s = if choice.contains("_Cancel") {
        "cancel"
    } else if choice.contains("_Expire") {
        "expire"
    } else if choice.contains("_Execute") {
        "execute"
    } else if choice.contains("_Confirm") {
        "confirm"
    } else {
        "other"
    };
    s.to_string()
}

fn classify_created(tid: &Identifier, is_child_of_exercise: bool) -> (String, String) {
    let entity = tid.entity_name.as_str();
    if entity.contains("Confirmation") {
        ("confirm".to_string(), entity.to_string())
    } else if entity.ends_with("Rules") {
        ("create".to_string(), entity.to_string())
    } else if entity.contains("ExecutionResult") {
        ("execute_result".to_string(), entity.to_string())
    } else if is_child_of_exercise {
        // Created as a downstream effect of an Exercise (e.g. a service contract
        // produced by `UserServiceRequest_Accept`) — not a fresh proposal.
        ("create".to_string(), entity.to_string())
    } else {
        ("propose".to_string(), entity.to_string())
    }
}

fn value_to_json(v: &Value) -> JsonValue {
    match &v.sum {
        Some(value::Sum::Unit(())) => JsonValue::Null,
        Some(value::Sum::Bool(b)) => JsonValue::Bool(*b),
        Some(value::Sum::Int64(i)) => json!(i),
        Some(value::Sum::Date(d)) => json!(d),
        Some(value::Sum::Timestamp(t)) => json!(t),
        Some(value::Sum::Numeric(n)) => JsonValue::String(n.clone()),
        Some(value::Sum::Party(p)) => JsonValue::String(p.clone()),
        Some(value::Sum::Text(t)) => JsonValue::String(t.clone()),
        Some(value::Sum::ContractId(c)) => JsonValue::String(c.clone()),
        Some(value::Sum::Optional(opt)) => match opt.value.as_ref() {
            Some(inner) => value_to_json(inner),
            None => JsonValue::Null,
        },
        Some(value::Sum::List(list)) => {
            JsonValue::Array(list.elements.iter().map(value_to_json).collect())
        }
        Some(value::Sum::Record(r)) => record_to_json_inner(r),
        Some(value::Sum::Variant(var)) => {
            let inner = var
                .value
                .as_deref()
                .map(value_to_json)
                .unwrap_or(JsonValue::Null);
            json!({ "_variant": var.constructor, "value": inner })
        }
        Some(value::Sum::Enum(e)) => JsonValue::String(e.constructor.clone()),
        Some(value::Sum::TextMap(_)) | Some(value::Sum::GenMap(_)) => {
            json!({ "_unsupported": "map" })
        }
        None => JsonValue::Null,
    }
}

fn record_to_json_inner(r: &Record) -> JsonValue {
    let mut obj = serde_json::Map::new();
    for (idx, f) in r.fields.iter().enumerate() {
        let key = if f.label.is_empty() {
            format!("_{idx}")
        } else {
            f.label.clone()
        };
        let val = f
            .value
            .as_ref()
            .map(value_to_json)
            .unwrap_or(JsonValue::Null);
        obj.insert(key, val);
    }
    JsonValue::Object(obj)
}

fn record_to_json(r: &Option<Record>) -> JsonValue {
    match r {
        Some(r) => record_to_json_inner(r),
        None => JsonValue::Null,
    }
}

fn optional_value_to_json(v: &Option<Value>) -> JsonValue {
    match v {
        Some(v) => value_to_json(v),
        None => JsonValue::Null,
    }
}

/// Query Canton's ledger for on-chain governance events for a party.
///
/// Streams `GetUpdates` from the pruned offset to the current ledger end,
/// filtered to governance templates Canton-side when possible (falling back
/// to a wildcard query otherwise). Returns only governance actions —
/// proposals, confirmations, executions and their outcomes — sorted
/// newest-first.
///
/// # Errors
///
/// Returns an error if the ledger connection fails or the stream errors out.
pub async fn get_chain_audit(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
    limit: usize,
    before_offset: Option<i64>,
) -> Result<AuditPage> {
    let party_id_str = party_id.to_string();
    let party_id = party_id_str.as_str();
    let mut state_client = utils::create_state_client(config, token.clone()).await?;
    let ledger_end = state_client
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {}))
        .await
        .context("Failed to query ledger end")?
        .into_inner()
        .offset;

    if ledger_end == 0 {
        return Ok(AuditPage::default());
    }

    let pruned_offset = state_client
        .get_latest_pruned_offsets(tonic::Request::new(GetLatestPrunedOffsetsRequest {}))
        .await
        .context("Failed to query pruned offsets")?
        .into_inner()
        .participant_pruned_up_to_inclusive;

    let begin_offset = pruned_offset.max(0);

    // Paging back through the trail means capping the range instead of moving
    // a Canton page token across HTTP requests: ledger offsets are stable and
    // survive a cache round trip, whereas a page token is only valid against
    // the exact query that produced it.
    let end_offset = match before_offset {
        Some(cursor) => cursor.saturating_sub(1).min(ledger_end),
        None => ledger_end,
    };
    if end_offset <= begin_offset {
        return Ok(AuditPage::default());
    }
    let range = OffsetRange {
        begin_exclusive: begin_offset,
        end_inclusive: end_offset,
    };

    let filters = chain_filters(packages);
    if filters.templates.is_empty() && filters.interfaces.is_empty() {
        tracing::warn!("No governance templates configured; returning empty chain audit");
        return Ok(AuditPage::default());
    }

    // The (module, entity) → governance_type index used to classify events
    // client-side, independent of which package version produced them.
    let template_index: HashMap<(String, String), &'static str> = filters
        .templates
        .iter()
        .map(|t| {
            (
                (t.module_name.to_string(), t.entity_name.to_string()),
                t.governance_type,
            )
        })
        .chain(filters.interfaces.iter().map(|i| {
            (
                (i.module_name.to_string(), i.entity_name.to_string()),
                i.governance_type,
            )
        }))
        .collect();

    // Filter at the Canton request level when possible: build template and
    // interface filters from package names present in the participant's own
    // inventory. Fall back to the wildcard (every event for the party,
    // classified client-side) if the inventory is unavailable or the
    // filtered query is rejected.
    let canton_filters = match fetch_package_names(config).await {
        Ok(names) => {
            let cumulative = build_canton_filters(&filters, &names);
            if cumulative.is_empty() {
                tracing::warn!(
                    "No governance packages found on participant; falling back to wildcard"
                );
                None
            } else {
                Some(cumulative)
            }
        }
        Err(e) => {
            tracing::warn!("Failed to list participant packages: {e:#}; falling back to wildcard");
            None
        }
    };

    // `collect_entries` already keeps governance entries only, sorted newest
    // first and trimmed to whole offset groups around `limit`.
    let page = match canton_filters {
        Some(cumulative) => {
            let filtered = collect_entries(
                config,
                token.clone(),
                party_id,
                range,
                cumulative,
                &template_index,
                limit,
            )
            .await;
            match filtered {
                Ok(entries) => entries,
                Err(e) => {
                    tracing::warn!(
                        "Filtered chain audit query failed: {e:#}; retrying with wildcard"
                    );
                    collect_entries(
                        config,
                        token,
                        party_id,
                        range,
                        wildcard_filters(),
                        &template_index,
                        limit,
                    )
                    .await?
                }
            }
        }
        None => {
            collect_entries(
                config,
                token,
                party_id,
                range,
                wildcard_filters(),
                &template_index,
                limit,
            )
            .await?
        }
    };

    tracing::info!(
        "Chain audit for {party_id}: {count} entries (ledger_end={ledger_end}, has_more={more})",
        count = page.entries.len(),
        more = page.has_more
    );

    Ok(page)
}

/// The ledger-offset window a chain-audit read covers.
#[derive(Clone, Copy)]
struct OffsetRange {
    begin_exclusive: i64,
    end_inclusive: i64,
}

/// One page of the audit trail, newest-first.
#[derive(Default)]
pub struct AuditPage {
    pub entries: Vec<ChainAuditEntry>,
    /// Whether entries older than this page exist. Derived from what the read
    /// actually saw, not from the row count — a page can hold *more* than
    /// `limit` rows (see [`trim_to_offset_groups`]), so counting rows would
    /// hand out a cursor to a page that turns out to be empty.
    pub has_more: bool,
}

/// Read governance audit entries newest-first until at least `limit` of them
/// have been collected.
///
/// Paging descending is what makes the early exit sound: every later page holds
/// strictly lower offsets, so the first `limit` governance entries seen *are*
/// the most recent `limit`.
///
/// The returned page can slightly exceed `limit`: it is extended to the end of
/// the last offset group rather than cut mid-transaction.
async fn collect_entries(
    config: &NodeConfig,
    token: Option<String>,
    party_id: &str,
    range: OffsetRange,
    cumulative: Vec<CumulativeFilter>,
    template_index: &HashMap<(String, String), &'static str>,
    limit: usize,
) -> Result<AuditPage> {
    let update_format = UpdateFormat {
        include_transactions: Some(TransactionFormat {
            event_format: Some(party_event_format(party_id, cumulative, true)),
            transaction_shape: TransactionShape::LedgerEffects as i32,
        }),
        include_reassignments: None,
        include_topology_events: None,
    };

    collect_from_pages(
        |page_token| {
            fetch_transactions_page(
                config,
                token.clone(),
                range.begin_exclusive,
                range.end_inclusive,
                update_format.clone(),
                page_token,
            )
        },
        template_index,
        limit,
    )
    .await
}

/// The page-walk behind [`collect_entries`], over an arbitrary page source.
///
/// `fetch_page` takes the token of the page to read and is a parameter so the
/// walk — and the `has_more` it derives from where it stopped — can be tested
/// against scripted pages. Production passes [`fetch_transactions_page`].
async fn collect_from_pages<F, Fut>(
    mut fetch_page: F,
    template_index: &HashMap<(String, String), &'static str>,
    limit: usize,
) -> Result<AuditPage>
where
    F: FnMut(Option<Vec<u8>>) -> Fut,
    Fut: Future<Output = Result<TransactionPage>>,
{
    if limit == 0 {
        return Ok(AuditPage::default());
    }

    let mut entries: Vec<ChainAuditEntry> = Vec::new();
    let mut page_token = None;
    // Whether Canton still had pages left when we stopped. Distinguishes
    // "stopped because the page was full" from "stopped because the range ran
    // out", which is what decides if an older page exists.
    let mut pages_remain = false;

    loop {
        let page = fetch_page(page_token)
            .await
            .context("Failed to read ledger updates")?;

        for tx in page.transactions {
            entries.extend(
                transaction_entries(tx, template_index)
                    .into_iter()
                    .filter(is_governance_entry),
            );
        }

        if entries.len() >= limit {
            pages_remain = page.next_page_token.is_some();
            break;
        }

        match page.next_page_token {
            Some(next) => page_token = Some(next),
            None => break,
        }
    }

    entries.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let dropped = trim_to_offset_groups(&mut entries, limit);

    Ok(AuditPage {
        entries,
        // Anything trimmed is strictly older than what we return, so it is
        // itself proof that an older page exists.
        has_more: dropped || pages_remain,
    })
}

/// Trim `entries` — sorted newest-first — to roughly `limit`, never ending
/// part-way through an offset group. Returns whether anything was dropped.
///
/// One transaction can yield several entries that all share its offset, and the
/// cursor handed to the client is an offset — so a page cut mid-offset would
/// make the next page (`offset < cursor`) skip the remainder outright.
/// Overshooting `limit` by the tail of one transaction is the cheaper trade.
/// Every entry for a given offset is in hand by construction: `GetUpdatesPage`
/// returns whole transactions, and the caller consumes a full page before
/// stopping.
fn trim_to_offset_groups(entries: &mut Vec<ChainAuditEntry>, limit: usize) -> bool {
    if limit == 0 || entries.len() <= limit {
        return false;
    }

    let boundary = entries[limit - 1].offset;
    let keep = entries
        .iter()
        .position(|e| e.offset < boundary)
        .unwrap_or(entries.len());

    if keep >= entries.len() {
        return false;
    }
    entries.truncate(keep);
    true
}

/// Convert one transaction's Created/Exercised events into audit entries.
fn transaction_entries(
    tx: Transaction,
    template_index: &HashMap<(String, String), &'static str>,
) -> Vec<ChainAuditEntry> {
    let mut entries: Vec<ChainAuditEntry> = Vec::new();
    let tx_ts = tx.effective_at.as_ref().map(|t| t.seconds).unwrap_or(0);
    let update_id = tx.update_id.clone();

    // Collect (node_id, last_descendant_node_id) for every Exercise in this
    // transaction so we can later detect Created events that are downstream
    // effects of an Exercise — those aren't fresh proposals.
    let exercise_ranges: Vec<(i32, i32)> = tx
        .events
        .iter()
        .filter_map(|evt| match evt.event.as_ref()? {
            Event::Exercised(x) => Some((x.node_id, x.last_descendant_node_id)),
            _ => None,
        })
        .collect();

    for evt in tx.events {
        let Some(e) = evt.event else { continue };
        match e {
            Event::Created(c) => {
                let Some(tid) = c.template_id.as_ref() else {
                    continue;
                };
                let gov_type = template_index
                    .get(&(tid.module_name.clone(), tid.entity_name.clone()))
                    .copied()
                    .or_else(|| {
                        c.interface_views.iter().find_map(|iv| {
                            let iid = iv.interface_id.as_ref()?;
                            template_index
                                .get(&(iid.module_name.clone(), iid.entity_name.clone()))
                                .copied()
                        })
                    })
                    .unwrap_or("unknown");

                let is_child_of_exercise = exercise_ranges
                    .iter()
                    .any(|(start, end)| c.node_id > *start && c.node_id <= *end);
                let (event_type, action_summary) = classify_created(tid, is_child_of_exercise);

                entries.push(ChainAuditEntry {
                    offset: c.offset,
                    timestamp: tx_ts,
                    event_type,
                    contract_id: c.contract_id,
                    template_id: format!("{}:{}", tid.module_name, tid.entity_name),
                    package_id: tid.package_id.clone(),
                    governance_type: gov_type.to_string(),
                    action_summary,
                    choice: None,
                    acting_parties: c.signatories,
                    update_id: update_id.clone(),
                    details: record_to_json(&c.create_arguments),
                });
            }
            Event::Exercised(x) => {
                let Some(tid) = x.template_id.as_ref() else {
                    continue;
                };
                let gov_type = template_index
                    .get(&(tid.module_name.clone(), tid.entity_name.clone()))
                    .copied()
                    .or_else(|| {
                        let iid = x.interface_id.as_ref()?;
                        template_index
                            .get(&(iid.module_name.clone(), iid.entity_name.clone()))
                            .copied()
                    })
                    .unwrap_or("unknown");

                let event_type = classify_choice(&x.choice);
                let choice = x.choice.clone();
                entries.push(ChainAuditEntry {
                    offset: x.offset,
                    timestamp: tx_ts,
                    event_type,
                    contract_id: x.contract_id,
                    template_id: format!("{}:{}", tid.module_name, tid.entity_name),
                    package_id: tid.package_id.clone(),
                    governance_type: gov_type.to_string(),
                    action_summary: choice.clone(),
                    choice: Some(choice),
                    acting_parties: x.acting_parties,
                    update_id: update_id.clone(),
                    details: optional_value_to_json(&x.choice_argument),
                });
            }
            Event::Archived(_) => {
                // Under LedgerEffects we get Exercised (consuming) instead; skip.
            }
        }
    }

    entries
}

/// Save chain audit entries to the cache table.
/// Uses INSERT OR IGNORE to skip duplicates based on (party_id, offset, contract_id, event_type).
pub async fn save_chain_audit_cache(
    pool: &SqlitePool,
    party_id: &CantonId,
    entries: &[ChainAuditEntry],
) {
    let party_id_str = party_id.to_string();
    for entry in entries {
        let acting_parties = serde_json::to_string(&entry.acting_parties).unwrap_or_default();
        let details = entry.details.to_string();

        if let Err(e) = sqlx::query(
            r"
            INSERT OR IGNORE INTO chain_audit_cache (
                party_id, offset, timestamp, event_type, contract_id,
                template_id, package_id, governance_type, action_summary,
                choice, acting_parties, update_id, details
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ",
        )
        .bind(&party_id_str)
        .bind(entry.offset)
        .bind(entry.timestamp)
        .bind(&entry.event_type)
        .bind(&entry.contract_id)
        .bind(&entry.template_id)
        .bind(&entry.package_id)
        .bind(&entry.governance_type)
        .bind(&entry.action_summary)
        .bind(&entry.choice)
        .bind(&acting_parties)
        .bind(&entry.update_id)
        .bind(&details)
        .execute(pool)
        .await
        {
            tracing::warn!("Failed to cache chain audit entry: {e}");
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use canton_proto_rs::com::daml::ledger::api::v2::{
        CreatedEvent, Enum, Event as EventEnvelope, Optional, RecordField, TextMap, Variant,
    };

    use super::*;

    fn entry_with_event_type(event_type: &str) -> ChainAuditEntry {
        ChainAuditEntry {
            offset: 0,
            timestamp: 0,
            event_type: event_type.to_string(),
            contract_id: String::new(),
            template_id: String::new(),
            package_id: String::new(),
            governance_type: "core_domain".to_string(),
            action_summary: String::new(),
            choice: None,
            acting_parties: Vec::new(),
            update_id: String::new(),
            details: JsonValue::Null,
        }
    }

    /// Entries at the given offsets, newest-first, as `trim_to_offset_groups`
    /// expects its input.
    fn entries_at(offsets: &[i64]) -> Vec<ChainAuditEntry> {
        offsets
            .iter()
            .map(|offset| ChainAuditEntry {
                offset: *offset,
                ..entry_with_event_type("propose")
            })
            .collect()
    }

    fn offsets_of(entries: &[ChainAuditEntry]) -> Vec<i64> {
        entries.iter().map(|e| e.offset).collect()
    }

    #[test]
    fn trim_keeps_everything_when_within_limit() {
        let mut entries = entries_at(&[30, 20, 10]);
        assert!(!trim_to_offset_groups(&mut entries, 5));
        assert_eq!(offsets_of(&entries), vec![30, 20, 10]);
    }

    #[test]
    fn trim_cuts_cleanly_on_an_offset_boundary() {
        let mut entries = entries_at(&[30, 20, 10]);
        assert!(trim_to_offset_groups(&mut entries, 2));
        assert_eq!(offsets_of(&entries), vec![30, 20]);
    }

    /// The case the cursor depends on: the limit lands mid-transaction, so the
    /// page is extended to the end of that offset group. Cutting at exactly
    /// `limit` would strand offset 20's third entry — the next page asks for
    /// `offset < 20` and would never return it.
    #[test]
    fn trim_never_splits_an_offset_group() {
        let mut entries = entries_at(&[30, 20, 20, 20, 10]);
        assert!(trim_to_offset_groups(&mut entries, 2));
        assert_eq!(offsets_of(&entries), vec![30, 20, 20, 20]);
    }

    /// A single transaction bigger than the whole page still comes back whole
    /// rather than being cut into a page the cursor can never revisit.
    #[test]
    fn trim_keeps_an_oversized_group_intact() {
        let mut entries = entries_at(&[20, 20, 20, 20]);
        assert!(!trim_to_offset_groups(&mut entries, 2));
        assert_eq!(offsets_of(&entries), vec![20, 20, 20, 20]);
    }

    /// Nothing was dropped, so the caller must not report `has_more` — the
    /// trailing group runs to the end of what we read.
    #[test]
    fn trim_reports_no_drop_when_the_group_runs_to_the_end() {
        let mut entries = entries_at(&[30, 20, 20]);
        assert!(!trim_to_offset_groups(&mut entries, 2));
        assert_eq!(offsets_of(&entries), vec![30, 20, 20]);
    }

    #[test]
    fn trim_is_a_no_op_at_zero_limit() {
        let mut entries = entries_at(&[30, 20]);
        assert!(!trim_to_offset_groups(&mut entries, 0));
        assert_eq!(offsets_of(&entries), vec![30, 20]);
    }

    #[test]
    fn test_is_governance_entry() {
        let kept = [
            "propose",
            "confirm",
            "execute",
            "expire",
            "cancel",
            "execute_result",
        ];
        let dropped = ["create", "other"];

        for event_type in kept {
            assert!(
                is_governance_entry(&entry_with_event_type(event_type)),
                "{event_type} should be kept"
            );
        }
        for event_type in dropped {
            assert!(
                !is_governance_entry(&entry_with_event_type(event_type)),
                "{event_type} should be dropped"
            );
        }
    }

    fn id(entity_name: &str) -> Identifier {
        Identifier {
            package_id: "#governance-core-v1".to_string(),
            module_name: "Governance.Rules".to_string(),
            entity_name: entity_name.to_string(),
        }
    }

    fn text(s: &str) -> Value {
        Value {
            sum: Some(value::Sum::Text(s.to_string())),
        }
    }

    fn variant(ctor: &str, inner: Value) -> Value {
        Value {
            sum: Some(value::Sum::Variant(Box::new(Variant {
                variant_id: None,
                constructor: ctor.to_string(),
                value: Some(Box::new(inner)),
            }))),
        }
    }

    #[test]
    fn test_classify_choice_precedence() {
        assert_eq!(
            classify_choice("GovernanceRules_ExecuteConfirmedAction"),
            "execute"
        );
        assert_eq!(classify_choice("GovernanceRules_ConfirmAction"), "confirm");
        assert_eq!(classify_choice("GovernanceRules_CancelAction"), "cancel");
        assert_eq!(classify_choice("GovernanceRules_ExpireAction"), "expire");
        assert_eq!(classify_choice("Archive"), "other");

        // Precedence probe: a name containing BOTH `_Cancel` and `_Execute`.
        // The if/else chain tests `_Cancel` first, so it wins. Pins ordering.
        assert_eq!(classify_choice("Foo_Cancel_Execute"), "cancel");
    }

    #[test]
    fn test_classify_created() {
        // "Confirmation" → confirm (checked before the `Rules` / child branches).
        assert_eq!(
            classify_created(&id("GovernanceConfirmation"), false),
            ("confirm".to_string(), "GovernanceConfirmation".to_string())
        );
        // Ends with "Rules" → create.
        assert_eq!(
            classify_created(&id("GovernanceRules"), false),
            ("create".to_string(), "GovernanceRules".to_string())
        );
        // Contains "ExecutionResult" → execute_result.
        assert_eq!(
            classify_created(&id("GovernanceExecutionResult"), false),
            (
                "execute_result".to_string(),
                "GovernanceExecutionResult".to_string()
            )
        );
        // A plain proposal entity, not a child of an exercise → propose.
        assert_eq!(
            classify_created(&id("GovernanceProposal"), false),
            ("propose".to_string(), "GovernanceProposal".to_string())
        );
        // The same entity, but created as a downstream effect of an exercise → create.
        assert_eq!(
            classify_created(&id("GovernanceProposal"), true),
            ("create".to_string(), "GovernanceProposal".to_string())
        );
    }

    #[test]
    fn test_value_to_json() {
        // Variant("AV_Text", inner Text "x")
        assert_eq!(
            value_to_json(&variant("AV_Text", text("x"))),
            json!({ "_variant": "AV_Text", "value": "x" })
        );

        // An empty TextMap → unsupported-map marker.
        let empty_map = Value {
            sum: Some(value::Sum::TextMap(TextMap {
                entries: Vec::new(),
            })),
        };
        assert_eq!(value_to_json(&empty_map), json!({ "_unsupported": "map" }));

        // Numeric is emitted as a JSON STRING to preserve financial precision.
        let numeric = Value {
            sum: Some(value::Sum::Numeric("1.50".to_string())),
        };
        assert_eq!(value_to_json(&numeric), json!("1.50"));

        // Int64 → JSON number.
        let int = Value {
            sum: Some(value::Sum::Int64(7)),
        };
        assert_eq!(value_to_json(&int), json!(7));

        // Optional(None) → Null.
        let none_opt = Value {
            sum: Some(value::Sum::Optional(Box::new(Optional { value: None }))),
        };
        assert_eq!(value_to_json(&none_opt), JsonValue::Null);

        // Unit → Null.
        let unit = Value {
            sum: Some(value::Sum::Unit(())),
        };
        assert_eq!(value_to_json(&unit), JsonValue::Null);

        // Enum constructor "Red" → JSON string.
        let red = Value {
            sum: Some(value::Sum::Enum(Enum {
                enum_id: None,
                constructor: "Red".to_string(),
            })),
        };
        assert_eq!(value_to_json(&red), json!("Red"));
    }

    #[test]
    fn test_record_to_json_inner() {
        let record = Record {
            record_id: None,
            fields: vec![
                RecordField {
                    label: String::new(),
                    value: Some(text("a")),
                },
                RecordField {
                    label: "named".to_string(),
                    value: Some(Value {
                        sum: Some(value::Sum::Int64(1)),
                    }),
                },
                RecordField {
                    label: "gone".to_string(),
                    value: None,
                },
            ],
        };

        let out = record_to_json_inner(&record);
        assert_eq!(out, json!({ "_0": "a", "named": 1, "gone": null }));

        // record_to_json(&None) → Null.
        assert_eq!(record_to_json(&None), JsonValue::Null);
    }

    #[test]
    fn test_build_canton_filters() {
        let filters = ChainFilters {
            templates: vec![ChainTemplate {
                package_prefix: "governance-core".to_string(),
                module_name: "Governance.Rules",
                entity_name: "GovernanceRules",
                governance_type: "core_self",
            }],
            interfaces: vec![ChainInterface {
                package_prefix: "governance-action".to_string(),
                module_name: "Governance.Action",
                entity_name: "GovernableAction",
                governance_type: "core_domain",
            }],
        };
        let names = vec![
            "governance-core-v0-rc4".to_string(),
            "governance-core-v1".to_string(),
            "governance-action-v1".to_string(),
            "unrelated-app-v1".to_string(),
        ];

        let cumulative = build_canton_filters(&filters, &names);

        // One template filter per core package version + one interface filter
        assert_eq!(cumulative.len(), 3);
    }

    // ====================================================================
    // Page walk
    // ====================================================================

    /// One transaction carrying a governable-action Create per offset — each
    /// classifies as `propose`, so all of them survive the governance filter.
    fn tx_at(offsets: &[i64]) -> Transaction {
        Transaction {
            events: offsets
                .iter()
                .map(|offset| EventEnvelope {
                    event: Some(Event::Created(CreatedEvent {
                        offset: *offset,
                        contract_id: format!("c-{offset}"),
                        template_id: Some(Identifier {
                            package_id: "#governance-action-v1".to_string(),
                            module_name: "Governance.Action".to_string(),
                            entity_name: "GovernableAction".to_string(),
                        }),
                        ..Default::default()
                    })),
                })
                .collect(),
            ..Default::default()
        }
    }

    /// A scripted page holding one transaction, and whether Canton claims more
    /// pages follow it.
    fn scripted_page(offsets: &[i64], more: bool) -> TransactionPage {
        TransactionPage {
            transactions: vec![tx_at(offsets)],
            next_page_token: more.then(|| b"next".to_vec()),
        }
    }

    /// Run the walk over `pages`. Asking for a page beyond the script is an
    /// error, so a test that over-reads fails rather than quietly passing.
    async fn walk(pages: Vec<TransactionPage>, limit: usize) -> AuditPage {
        let mut remaining = VecDeque::from(pages);
        let template_index = HashMap::new();

        collect_from_pages(
            |_page_token| {
                let page = remaining.pop_front();
                async move { page.context("asked for a page beyond the script") }
            },
            &template_index,
            limit,
        )
        .await
        .expect("the scripted walk succeeds")
    }

    /// The edge the cursor contract turns on: the limit is met exactly on
    /// Canton's last page, so nothing older exists and the endpoint must not
    /// advertise another page.
    #[tokio::test]
    async fn walk_reports_no_more_when_the_limit_lands_on_the_last_page() {
        let page = walk(vec![scripted_page(&[30, 20], false)], 2).await;

        assert_eq!(offsets_of(&page.entries), vec![30, 20]);
        assert!(!page.has_more);
    }

    /// Stopped on a full page with a token still in hand — there is more.
    #[tokio::test]
    async fn walk_reports_more_when_pages_remain() {
        let page = walk(vec![scripted_page(&[30, 20], true)], 2).await;

        assert_eq!(offsets_of(&page.entries), vec![30, 20]);
        assert!(page.has_more);
    }

    /// A page short of `limit` is followed, and only as far as needed: a third
    /// page is never requested.
    #[tokio::test]
    async fn walk_follows_pages_until_the_limit_is_met() {
        let pages = vec![
            scripted_page(&[30], true),
            scripted_page(&[20], true),
            scripted_page(&[10], true),
        ];
        let page = walk(pages, 2).await;

        assert_eq!(offsets_of(&page.entries), vec![30, 20]);
        assert!(page.has_more);
    }

    /// The range ran out before `limit` was reached, so this is the last page
    /// however short it is.
    #[tokio::test]
    async fn walk_reports_no_more_when_the_range_runs_out() {
        let page = walk(vec![scripted_page(&[30], false)], 5).await;

        assert_eq!(offsets_of(&page.entries), vec![30]);
        assert!(!page.has_more);
    }

    /// Canton had no further pages, but the offset-group trim dropped entries —
    /// which are strictly older than what is returned, so they are themselves
    /// proof of an older page.
    #[tokio::test]
    async fn walk_reports_more_when_the_trim_drops_entries() {
        let page = walk(vec![scripted_page(&[30, 20, 20, 10], false)], 2).await;

        assert_eq!(offsets_of(&page.entries), vec![30, 20, 20]);
        assert!(page.has_more);
    }

    /// A zero-row page reads nothing at all — the script is empty, so any fetch
    /// would fail the walk.
    #[tokio::test]
    async fn walk_at_zero_limit_reads_no_pages() {
        let page = walk(Vec::new(), 0).await;

        assert!(page.entries.is_empty());
        assert!(!page.has_more);
    }

    /// Events that are not governance actions are dropped without counting
    /// toward the limit, so the walk keeps reading for the ones that are.
    #[tokio::test]
    async fn walk_skips_non_governance_events() {
        let noise = Transaction {
            events: vec![EventEnvelope {
                event: Some(Event::Created(CreatedEvent {
                    offset: 40,
                    contract_id: "c-40".to_string(),
                    // Ends with `Rules`, so it classifies as `create`.
                    template_id: Some(id("GovernanceRules")),
                    ..Default::default()
                })),
            }],
            ..Default::default()
        };
        let pages = vec![
            TransactionPage {
                transactions: vec![noise],
                next_page_token: Some(b"next".to_vec()),
            },
            scripted_page(&[30], false),
        ];

        let page = walk(pages, 1).await;

        assert_eq!(offsets_of(&page.entries), vec![30]);
        assert!(!page.has_more);
    }
}
