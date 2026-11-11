//! Token-standard holdings and transfer preapprovals.

use std::collections::HashMap;

use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::daml::ledger::api::v2::{CreatedEvent, Identifier, Record, value};
use decman_lib::framework::record::{field_numeric, field_party, field_text};

use super::contracts::{ContractQueryParams, query_contracts_by_template};
use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    server::{
        event_filters::{interface_filter, party_event_format, template_filter},
        ledger_paging::fetch_active_contracts_filtered,
        record::{field_record, record_field},
        types::HoldingInfo,
    },
};

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
        include_payload: false,
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
pub(in crate::server) const PREAPPROVAL_WILDCARD_ID: &str = "*";

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
    use canton_proto_rs::com::daml::ledger::api::v2::{InterfaceView, Value};

    use super::{super::test_values::*, *};

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
}
