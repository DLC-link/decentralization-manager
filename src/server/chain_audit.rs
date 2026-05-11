use std::collections::HashMap;

use anyhow::{Context, Result};
use canton_proto_rs::com::daml::ledger::api::v2::{
    CumulativeFilter, EventFormat, Filters, GetLatestPrunedOffsetsRequest, GetLedgerEndRequest,
    GetUpdatesRequest, Identifier, Record, TransactionFormat, TransactionShape, UpdateFormat,
    Value, WildcardFilter, cumulative_filter, event::Event, get_updates_response::Update, value,
};
use serde_json::{Value as JsonValue, json};

use sqlx::SqlitePool;

use crate::{
    config::{NodeConfig, PackageConfig},
    participant_id::CantonId,
    utils,
};

use super::types::ChainAuditEntry;

struct ChainTemplate {
    module_name: &'static str,
    entity_name: &'static str,
    governance_type: &'static str,
}

struct ChainInterface {
    module_name: &'static str,
    entity_name: &'static str,
    governance_type: &'static str,
}

struct ChainFilters {
    templates: Vec<ChainTemplate>,
    interfaces: Vec<ChainInterface>,
}

/// The list of governance Daml types we care about. We no longer pin
/// `package_id` on each entry — events are matched purely by
/// `(module_name, entity_name)` after the wildcard ledger query, so the
/// audit trail covers events from any package version (rc3, rc4, future).
/// `packages` is kept as an argument so a build that omits some governance
/// kinds (vault / core / cbtc) still skips them at the index level.
fn chain_filters(packages: &PackageConfig) -> ChainFilters {
    let mut templates = Vec::new();
    let mut interfaces = Vec::new();

    if packages.vault_governance.is_some() {
        templates.push(ChainTemplate {
            module_name: "BitsafeVault.VaultGovernance",
            entity_name: "VaultGovernanceRules",
            governance_type: "vault",
        });
        templates.push(ChainTemplate {
            module_name: "BitsafeVault.VaultGovernance",
            entity_name: "VaultGovernanceConfirmation",
            governance_type: "vault",
        });
    }

    if packages.governance_core.is_some() {
        templates.push(ChainTemplate {
            module_name: "Governance.Rules",
            entity_name: "GovernanceRules",
            governance_type: "core_self",
        });
        templates.push(ChainTemplate {
            module_name: "Governance.Rules",
            entity_name: "GovernanceSelfConfirmation",
            governance_type: "core_self",
        });
        templates.push(ChainTemplate {
            module_name: "Governance.Confirmation",
            entity_name: "GovernanceConfirmation",
            governance_type: "core_domain",
        });
        templates.push(ChainTemplate {
            module_name: "Governance.ExecutionResult",
            entity_name: "GovernanceExecutionResult",
            governance_type: "core_domain",
        });
        interfaces.push(ChainInterface {
            module_name: "Governance.Action",
            entity_name: "GovernableAction",
            governance_type: "core_domain",
        });
    }

    templates.push(ChainTemplate {
        module_name: "CBTC.Governance",
        entity_name: "CBTCGovernanceRules",
        governance_type: "cbtc",
    });
    templates.push(ChainTemplate {
        module_name: "CBTC.Governance",
        entity_name: "Confirmation",
        governance_type: "cbtc",
    });

    ChainFilters {
        templates,
        interfaces,
    }
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

fn classify_created(tid: &Identifier) -> (String, String) {
    let entity = tid.entity_name.as_str();
    if entity.contains("Confirmation") {
        ("confirm".to_string(), entity.to_string())
    } else if entity.ends_with("Rules") {
        ("create".to_string(), entity.to_string())
    } else if entity.contains("ExecutionResult") {
        ("execute_result".to_string(), entity.to_string())
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
/// Streams `GetUpdates` from offset 0 to the current ledger end, filtered to
/// governance-related templates. Returns entries sorted newest-first.
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

    // Build the (module, entity) → governance_type index used to classify
    // events client-side. We keep this pinned to the static `PackageConfig`
    // because it's purely about *which Daml types we care about*, not which
    // package version they came from.
    let filters = chain_filters(packages);
    if filters.templates.is_empty() && filters.interfaces.is_empty() {
        tracing::warn!("No governance templates configured; returning empty chain audit");
        return Ok(Vec::new());
    }

    // Wildcard at the ledger level — pick up all events for the party
    // regardless of which package version produced them. Filtering happens
    // client-side via `template_index` below. Without this, an old run that
    // emitted events under (say) `#governance-core-v0-rc3` becomes
    // unqueryable on a participant that has only `#governance-core-v0-rc4`
    // vetted: the package-name reference in a `TemplateFilter` fails to
    // resolve and `GetUpdates` errors out with "Packages not found on
    // participant". The audit trail isn't sensitive to package version.
    let cumulative = vec![CumulativeFilter {
        identifier_filter: Some(cumulative_filter::IdentifierFilter::WildcardFilter(
            WildcardFilter {
                include_created_event_blob: false,
            },
        )),
    }];

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
        begin_exclusive: begin_offset,
        end_inclusive: Some(ledger_end),
        update_format: Some(update_format),
    };

    let mut stream = update_client
        .get_updates(tonic::Request::new(req))
        .await
        .context("Failed to call GetUpdates")?
        .into_inner();

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

                    let (event_type, action_summary) = classify_created(tid);

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

    entries.sort_by_key(|e| std::cmp::Reverse(e.offset));
    entries.truncate(limit);

    tracing::info!(
        "Chain audit for {party_id}: {count} entries (ledger_end={ledger_end})",
        count = entries.len()
    );

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
