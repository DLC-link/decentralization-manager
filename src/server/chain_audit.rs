use std::collections::HashMap;

use anyhow::{Context, Result};
use canton_proto_rs::com::daml::ledger::api::v2::{
    CumulativeFilter, EventFormat, Filters, GetLatestPrunedOffsetsRequest, GetLedgerEndRequest,
    GetUpdatesRequest, Identifier, InterfaceFilter, Record, TemplateFilter, TransactionFormat,
    TransactionShape, UpdateFormat, Value, WildcardFilter, cumulative_filter, event::Event,
    get_updates_response::Update, value,
};
use serde_json::{Value as JsonValue, json};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
    utils,
};

use super::{
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
            cumulative.push(CumulativeFilter {
                identifier_filter: Some(cumulative_filter::IdentifierFilter::TemplateFilter(
                    TemplateFilter {
                        template_id: Some(Identifier {
                            package_id: format!("#{name}"),
                            module_name: t.module_name.to_string(),
                            entity_name: t.entity_name.to_string(),
                        }),
                        include_created_event_blob: false,
                    },
                )),
            });
        }
    }

    for i in &filters.interfaces {
        for name in matching_names(package_names, &i.package_prefix) {
            cumulative.push(CumulativeFilter {
                identifier_filter: Some(cumulative_filter::IdentifierFilter::InterfaceFilter(
                    InterfaceFilter {
                        interface_id: Some(Identifier {
                            package_id: format!("#{name}"),
                            module_name: i.module_name.to_string(),
                            entity_name: i.entity_name.to_string(),
                        }),
                        include_interface_view: true,
                        include_created_event_blob: false,
                    },
                )),
            });
        }
    }

    cumulative
}

/// The wildcard fallback filter: every event for the party, classified and
/// trimmed client-side.
fn wildcard_filters() -> Vec<CumulativeFilter> {
    vec![CumulativeFilter {
        identifier_filter: Some(cumulative_filter::IdentifierFilter::WildcardFilter(
            WildcardFilter {
                include_created_event_blob: false,
            },
        )),
    }]
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
) -> Result<Vec<ChainAuditEntry>> {
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
        return Ok(Vec::new());
    }

    let pruned_offset = state_client
        .get_latest_pruned_offsets(tonic::Request::new(GetLatestPrunedOffsetsRequest {}))
        .await
        .context("Failed to query pruned offsets")?
        .into_inner()
        .participant_pruned_up_to_inclusive;

    let begin_offset = pruned_offset.max(0);

    let filters = chain_filters(packages);
    if filters.templates.is_empty() && filters.interfaces.is_empty() {
        tracing::warn!("No governance templates configured; returning empty chain audit");
        return Ok(Vec::new());
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

    let mut entries = match canton_filters {
        Some(cumulative) => {
            let filtered = stream_entries(
                config,
                token.clone(),
                party_id,
                begin_offset,
                ledger_end,
                cumulative,
                &template_index,
            )
            .await;
            match filtered {
                Ok(entries) => entries,
                Err(e) => {
                    tracing::warn!(
                        "Filtered chain audit query failed: {e:#}; retrying with wildcard"
                    );
                    stream_entries(
                        config,
                        token,
                        party_id,
                        begin_offset,
                        ledger_end,
                        wildcard_filters(),
                        &template_index,
                    )
                    .await?
                }
            }
        }
        None => {
            stream_entries(
                config,
                token,
                party_id,
                begin_offset,
                ledger_end,
                wildcard_filters(),
                &template_index,
            )
            .await?
        }
    };

    // The trail shows governance actions only — drop downstream creates,
    // unrelated choices and the like before caching and returning.
    entries.retain(is_governance_entry);

    entries.sort_by_key(|e| std::cmp::Reverse(e.offset));
    entries.truncate(limit);

    tracing::info!(
        "Chain audit for {party_id}: {count} entries (ledger_end={ledger_end})",
        count = entries.len()
    );

    Ok(entries)
}

/// Stream `GetUpdates` between the offsets with the given event filters and
/// convert matching Created/Exercised events into audit entries.
async fn stream_entries(
    config: &NodeConfig,
    token: Option<String>,
    party_id: &str,
    begin_exclusive: i64,
    end_inclusive: i64,
    cumulative: Vec<CumulativeFilter>,
    template_index: &HashMap<(String, String), &'static str>,
) -> Result<Vec<ChainAuditEntry>> {
    let mut filters_by_party = HashMap::new();
    filters_by_party.insert(party_id.to_string(), Filters { cumulative });

    let event_format = EventFormat {
        filters_by_party,
        filters_for_any_party: None,
        verbose: true,
    };

    let update_format = UpdateFormat {
        include_transactions: Some(TransactionFormat {
            event_format: Some(event_format),
            transaction_shape: TransactionShape::LedgerEffects as i32,
        }),
        include_reassignments: None,
        include_topology_events: None,
    };

    let mut update_client = utils::create_update_client(config, token).await?;
    let req = GetUpdatesRequest {
        begin_exclusive,
        end_inclusive: Some(end_inclusive),
        update_format: Some(update_format),
    };

    let mut stream = update_client
        .get_updates(tonic::Request::new(req))
        .await
        .context("Failed to call GetUpdates")?
        .into_inner();

    let mut entries: Vec<ChainAuditEntry> = Vec::new();

    while let Some(response) = stream
        .message()
        .await
        .context("Stream error while reading ledger updates")?
    {
        let Some(Update::Transaction(tx)) = response.update else {
            continue;
        };

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
    }

    Ok(entries)
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
            "governance-core-v1-rc1".to_string(),
            "governance-action-v1-rc1".to_string(),
            "unrelated-app-v1".to_string(),
        ];

        let cumulative = build_canton_filters(&filters, &names);

        // One template filter per core package version + one interface filter
        assert_eq!(cumulative.len(), 3);
    }
}
