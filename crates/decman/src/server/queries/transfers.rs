//! Token-standard transfer instructions, mint and burn requests, transfer factories.

use std::time::{SystemTime, UNIX_EPOCH};

use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::daml::ledger::api::v2::{CreatedEvent, Identifier, Record, value};
use decman_lib::framework::record::{field_numeric, field_party, field_text, field_timestamp};

use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
    error::Result,
};

use super::templates::{TemplateId, fetch_by_template};
use crate::server::{
    event_filters::{interface_filter, party_event_format},
    ledger_paging::fetch_active_contracts_filtered,
    record::{field_record, record_field},
    types::{
        PendingAction, TokenRequestInfo, TransferFactoryInfo, TransferInstructionInfo,
        TransferInstructionStatus,
    },
};

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
        TemplateId {
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
        TemplateId {
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
    template: TemplateId,
    payload_field: &str,
) -> Result<Vec<TokenRequestInfo>> {
    fetch_by_template(config, party_id, token, Some(template), |created| {
        if is_execute_before_expired_in_payload(created, payload_field) {
            return None;
        }
        extract_token_request_info(created, payload_field)
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

/// Returns true if the contract's create-arguments carry an `executeBefore`
/// Time field whose value is in the past. Returns false when no such field
/// exists, so templates without a deadline are kept as-is.
pub(super) fn is_execute_before_expired(created: &CreatedEvent) -> bool {
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

#[cfg(test)]
mod tests {
    use super::super::test_values::*;
    use super::*;
    use canton_proto_rs::com::daml::ledger::api::v2::InterfaceView;

    // ------------------------------------------------------------------------
    // extract_transfer_instruction_info
    //
    // Locks the two filters that are easy to break by accident: the status
    // constructor match and the `executeBefore` deadline check.
    // ------------------------------------------------------------------------

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
}
