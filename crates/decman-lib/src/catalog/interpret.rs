//! Per-event parses over a Ledger API `CreatedEvent`: governance
//! confirmations, governance-rules state, and `GovernableAction` proposal
//! info.
//!
//! Each function here is a pure, single-event parse — no grouping, no map
//! insertion, no I/O. DecMan's `queries.rs` currently owns its own copies
//! that additionally group results into `HashMap`s and (for the on-chain
//! action hash) call `compute_action_hash`, which stays a decman concern.
//! Task 20 switches `queries.rs` onto these.

use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::daml::ledger::api::v2::{CreatedEvent, Record, value};
use common::canton_id::CantonId;

use crate::catalog::action::ActionType;
use crate::catalog::types::{
    AcceptTransferDetails, ServiceRequestDetails, TransferProposalDetails,
};
use crate::framework::record::{
    extract_optional_reltime, extract_party_set, extract_reltime, field_numeric, field_party,
    field_text, field_timestamp,
};

/// A parsed vault-governance or governance-core self-action confirmation.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedConfirmation {
    pub contract_id: String,
    pub action: ActionType,
    pub confirming_party: CantonId,
    /// Unix seconds when the confirmation contract was created on the
    /// ledger. 0 if the timestamp could not be resolved.
    pub created_at: i64,
    /// Unix seconds of the confirmation's `expiresAt`. 0 if unresolved.
    pub expires_at: i64,
}

/// A governance-core domain confirmation. The on-ledger template stores NO
/// action — only the proposal cid and a label. DecMan's HTTP layer inserts
/// its legacy placeholder action when mapping; the lib does not lie.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedDomainConfirmation {
    pub contract_id: String,
    pub proposal_cid: String,
    pub action_label: String,
    pub confirming_party: CantonId,
    pub created_at: i64,
    pub expires_at: i64,
}

/// State of a `VaultGovernanceRules` or `GovernanceRules` contract.
#[derive(Clone, Debug, PartialEq)]
pub struct RulesState {
    pub contract_id: String,
    pub governance_party: CantonId,
    pub members: Vec<CantonId>,
    pub threshold: i64,
    pub timeout_micros: Option<i64>,
}

/// Parse a confirmation contract's action, confirming party, and timestamps
/// off its created event. Tries the vault `ActionRequiringConfirmation`
/// shape first, then falls back to governance-core's `GovernanceSelfAction`
/// shape. Returns `None` if the action field is absent/unrecognized, or the
/// confirming party is missing/invalid.
pub fn parse_confirmation(created: &CreatedEvent) -> Option<ParsedConfirmation> {
    let record = created.create_arguments.as_ref()?;

    // Extract action field (this is a Variant for VaultGovernance)
    let action_value = record.fields.iter().find(|f| f.label == "action");
    let Some(action_field) = action_value.and_then(|f| f.value.as_ref()) else {
        tracing::warn!("No action field found in confirmation contract");
        return None;
    };

    // Try to parse the action (vault ActionRequiringConfirmation or core GovernanceSelfAction)
    let action = match ActionType::from_vault_proto(action_field) {
        Ok(a) => a,
        Err(_) => match ActionType::from_self_proto(action_field) {
            Ok(a) => a,
            Err(e) => {
                tracing::debug!("Skipping confirmation with unrecognized action shape: {e}");
                return None;
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
        return None;
    };
    let confirming_party = match CantonId::parse(&confirming_party_str) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "Skipping confirmation {cid}: bad confirmingParty '{confirming_party_str}': {e}",
                cid = created.contract_id
            );
            return None;
        }
    };

    Some(ParsedConfirmation {
        contract_id: created.contract_id.clone(),
        action,
        confirming_party,
        created_at: created.created_at.as_ref().map(|t| t.seconds).unwrap_or(0),
        expires_at: field_timestamp(record, "expiresAt")
            .map(|micros| micros / 1_000_000)
            .unwrap_or(0),
    })
}

/// Parse a governance-core domain confirmation off its created event. The
/// template carries no inline action — only a reference to the proposal
/// contract id and its label. Returns `None` if `create_arguments` is
/// missing, or the confirmer field is missing/invalid.
pub fn parse_domain_confirmation(created: &CreatedEvent) -> Option<ParsedDomainConfirmation> {
    let record = created.create_arguments.as_ref()?;

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
        return None;
    };
    let confirming_party = match CantonId::parse(&confirmer_str) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "Skipping domain confirmation {cid}: bad confirmer '{confirmer_str}': {e}",
                cid = created.contract_id
            );
            return None;
        }
    };

    Some(ParsedDomainConfirmation {
        contract_id: created.contract_id.clone(),
        proposal_cid,
        action_label,
        confirming_party,
        created_at: created.created_at.as_ref().map(|t| t.seconds).unwrap_or(0),
        expires_at: field_timestamp(record, "expiresAt")
            .map(|micros| micros / 1_000_000)
            .unwrap_or(0),
    })
}

/// Extract governance rules state from a `VaultGovernanceRules` or
/// `GovernanceRules` created event.
pub fn extract_governance_state(created: &CreatedEvent) -> Option<RulesState> {
    let record = created.create_arguments.as_ref()?;

    // Extract governance party (vaultManager for vault, governanceParty for core)
    let governance_party: CantonId = record
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
    let timeout_micros = record
        .fields
        .iter()
        .find(|f| f.label == "actionConfirmationTimeout")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| extract_optional_reltime(v).or_else(|| extract_reltime(v)));

    Some(RulesState {
        contract_id: created.contract_id.clone(),
        governance_party,
        members,
        threshold,
        timeout_micros,
    })
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
#[derive(Clone, Debug, PartialEq)]
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
///
/// Returns the proposal's contract id paired with its `ProposalInfo`. Unlike
/// decman's `queries.rs` copy, this is a pure single-event parse — the caller
/// owns the grouping map.
pub fn extract_proposal_info(
    created: &CreatedEvent,
    governance_party: &CantonId,
) -> Option<(String, ProposalInfo)> {
    let view = governable_action_view(created);
    let record = created.create_arguments.as_ref();

    if view.is_none() && !record.is_some_and(looks_like_governable_action) {
        return None;
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
        return None;
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
    // transfer fields. Capture it here; a caller-side post-pass resolves each
    // cid to an `AcceptTransferDetails` via a per-cid event query so the card
    // can render sender/amount/instrument.
    let accept_transfer_instruction_cid = record
        .and_then(|r| {
            r.fields
                .iter()
                .find(|f| f.label == "transferInstructionCid")
        })
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::ContractId(cid)) => Some(cid.clone()),
            _ => None,
        });

    Some((
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
    ))
}

/// Pull sender/receiver/amount/instrument out of a `TransferInstruction`
/// interface view, *without* the status / deadline filters that
/// `extract_transfer_instruction_info` (used for the Accept dropdown) applies.
/// Pending-approval cards must render regardless of where the instruction is
/// in its lifecycle — the proposal is still being voted on, and the operator
/// needs to see what they're approving even if the underlying instruction has
/// already advanced or expired.
pub fn extract_accept_transfer_details_from_view(
    created: &CreatedEvent,
) -> Option<AcceptTransferDetails> {
    let view = created.interface_views.iter().find(|v| {
        v.interface_id.as_ref().is_some_and(|id| {
            id.module_name == "Splice.Api.Token.TransferInstructionV1"
                && id.entity_name == "TransferInstruction"
        })
    })?;
    let view_record = view.view_value.as_ref()?;
    let transfer_record = view_record
        .fields
        .iter()
        .find(|f| f.label == "transfer")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Record(r)) => Some(r),
            _ => None,
        })?;
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
    Some(AcceptTransferDetails {
        sender,
        receiver,
        amount,
        instrument_admin,
        instrument_id,
    })
}

/// Pull `receiver`, `amount`, and the nested `instrumentId` out of a
/// `TransferProposal`'s `transfer` field. Returns `None` for any proposal
/// that doesn't have a `transfer` record (every non-transfer template).
pub fn extract_transfer_proposal_details(record: &Record) -> Option<TransferProposalDetails> {
    let transfer_record = record
        .fields
        .iter()
        .find(|f| f.label == "transfer")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Record(r)) => Some(r),
            _ => None,
        })?;
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
pub fn extract_service_request_details(record: &Record) -> Option<ServiceRequestDetails> {
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

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::daml::ledger::api::v2::{
        GenMap, Identifier, InterfaceView, Optional, Record, RecordField, Value, gen_map,
    };
    use prost_types::Timestamp;

    use super::*;
    use crate::framework::encode::{
        field, make_contract_id, make_int64, make_party, make_record, make_text,
    };

    /// Valid-shape party ids (a 34-byte SHA-256 multihash namespace, hex
    /// encoded) so `CantonId::parse` succeeds.
    const ALICE: &str =
        "alice::1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BOB: &str = "bob::1220bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const GOV: &str = "gov::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    fn cid(prefix: &str) -> CantonId {
        CantonId::parse(prefix).expect("valid test fixture party id")
    }

    fn created_event(contract_id: &str, record: Record, created_at_seconds: i64) -> CreatedEvent {
        CreatedEvent {
            contract_id: contract_id.to_string(),
            create_arguments: Some(record),
            created_at: Some(Timestamp {
                seconds: created_at_seconds,
                nanos: 0,
            }),
            interface_views: vec![],
            ..Default::default()
        }
    }

    fn record_of(fields: Vec<RecordField>) -> Record {
        Record {
            record_id: None,
            fields,
        }
    }

    /// A `GovernableAction` interface view carrying `view_fields`, shaped the
    /// way Canton attaches it when the query asked for it with an
    /// `InterfaceFilter`.
    fn governable_action_interface_view(view_fields: Vec<RecordField>) -> InterfaceView {
        InterfaceView {
            interface_id: Some(Identifier {
                package_id: "#governance-core".to_string(),
                module_name: "Governance.Action".to_string(),
                entity_name: "GovernableAction".to_string(),
            }),
            view_status: None,
            view_value: Some(record_of(view_fields)),
            ..Default::default()
        }
    }

    /// A created event for a `GovernableAction`-implementing proposal
    /// contract, with an optional interface view, create-arguments, and
    /// template entity name.
    fn proposal_created_event(
        contract_id: &str,
        interface_views: Vec<InterfaceView>,
        create_arguments: Option<Record>,
        template_entity_name: Option<&str>,
    ) -> CreatedEvent {
        CreatedEvent {
            contract_id: contract_id.to_string(),
            template_id: template_entity_name.map(|name| Identifier {
                package_id: "#governance-core".to_string(),
                module_name: "Governance.Action".to_string(),
                entity_name: name.to_string(),
            }),
            create_arguments,
            created_at: Some(Timestamp {
                seconds: 1_700_001_000,
                nanos: 0,
            }),
            interface_views,
            ..Default::default()
        }
    }

    #[test]
    fn parses_a_vault_confirmation_with_its_action() {
        let action = ActionType::GovernanceSetThreshold { new_threshold: 2 };
        let record = record_of(vec![
            field("action", action.to_vault_proto().expect("encodes")),
            field("confirmingParty", make_party(ALICE)),
            field(
                "expiresAt",
                Value {
                    sum: Some(value::Sum::Timestamp(2_000_000)),
                },
            ),
        ]);
        let created = created_event("confirmation-1", record, 1_700_000_000);

        let parsed = parse_confirmation(&created).expect("parses");

        assert_eq!(
            parsed,
            ParsedConfirmation {
                contract_id: "confirmation-1".to_string(),
                action,
                confirming_party: cid(ALICE),
                created_at: 1_700_000_000,
                expires_at: 2,
            }
        );
    }

    #[test]
    fn parses_a_self_confirmation_via_the_fallback_order() {
        let action = ActionType::GovernanceSetThreshold { new_threshold: 3 };
        let record = record_of(vec![
            field("action", action.to_self_proto().expect("encodes")),
            field("confirmer", make_party(BOB)),
        ]);
        let created = created_event("confirmation-2", record, 1_700_000_100);

        let parsed = parse_confirmation(&created).expect("parses via the self-proto fallback");

        assert_eq!(
            parsed,
            ParsedConfirmation {
                contract_id: "confirmation-2".to_string(),
                action,
                confirming_party: cid(BOB),
                created_at: 1_700_000_100,
                // No expiresAt field present.
                expires_at: 0,
            }
        );
    }

    #[test]
    fn domain_confirmation_parses_without_an_action() {
        let record = record_of(vec![
            field("actionProposalCid", make_contract_id("proposal-cid-1")),
            field("actionLabel", make_text("Set threshold to 3")),
            field("confirmer", make_party(ALICE)),
            field(
                "expiresAt",
                Value {
                    sum: Some(value::Sum::Timestamp(5_000_000)),
                },
            ),
        ]);
        let created = created_event("domain-confirmation-1", record, 1_700_000_200);

        let parsed = parse_domain_confirmation(&created).expect("parses");

        // `ParsedDomainConfirmation` has no `action` field at all — this
        // struct literal wouldn't compile if one existed, so field
        // exhaustiveness itself proves the type carries none.
        assert_eq!(
            parsed,
            ParsedDomainConfirmation {
                contract_id: "domain-confirmation-1".to_string(),
                proposal_cid: "proposal-cid-1".to_string(),
                action_label: "Set threshold to 3".to_string(),
                confirming_party: cid(ALICE),
                created_at: 1_700_000_200,
                expires_at: 5,
            }
        );
    }

    #[test]
    fn missing_confirmer_skips_the_confirmation() {
        let action = ActionType::GovernanceSetThreshold { new_threshold: 1 };
        let vault_record = record_of(vec![field(
            "action",
            action.to_vault_proto().expect("encodes"),
        )]);
        let vault_created = created_event("confirmation-3", vault_record, 1_700_000_300);
        assert_eq!(parse_confirmation(&vault_created), None);

        let domain_record = record_of(vec![field(
            "actionProposalCid",
            make_contract_id("proposal-cid-2"),
        )]);
        let domain_created = created_event("domain-confirmation-2", domain_record, 1_700_000_400);
        assert_eq!(parse_domain_confirmation(&domain_created), None);
    }

    fn unit_value() -> Value {
        Value {
            sum: Some(value::Sum::Unit(())),
        }
    }

    fn gen_map_of(parties: &[&str]) -> GenMap {
        GenMap {
            entries: parties
                .iter()
                .map(|p| gen_map::Entry {
                    key: Some(make_party(*p)),
                    value: Some(unit_value()),
                })
                .collect(),
        }
    }

    #[test]
    fn rules_state_parses_both_set_party_shapes() {
        // Shape 1: DA.Set.Types:Set Party — a Record wrapping a "map" GenMap.
        let wrapped_members = make_record(vec![field(
            "map",
            Value {
                sum: Some(value::Sum::GenMap(gen_map_of(&[ALICE, BOB]))),
            },
        )]);
        let record = record_of(vec![
            field("vaultManager", make_party(GOV)),
            field("members", wrapped_members),
            field("threshold", make_int64(2)),
            field(
                "actionConfirmationTimeout",
                serialize_reltime_for_test(60_000_000),
            ),
        ]);
        let created = created_event("rules-1", record, 1_700_000_500);

        let mut parsed = extract_governance_state(&created).expect("parses");
        parsed.members.sort_by_key(|a| a.to_string());
        let mut expected_members = vec![cid(ALICE), cid(BOB)];
        expected_members.sort_by_key(|a| a.to_string());

        assert_eq!(parsed.contract_id, "rules-1");
        assert_eq!(parsed.governance_party, cid(GOV));
        assert_eq!(parsed.members, expected_members);
        assert_eq!(parsed.threshold, 2);
        assert_eq!(parsed.timeout_micros, Some(60_000_000));

        // Shape 2: bare GenMap<Party, Unit> (no DA.Set.Types wrapper).
        let bare_members = Value {
            sum: Some(value::Sum::GenMap(gen_map_of(&[ALICE, BOB]))),
        };
        let record2 = record_of(vec![
            field("governanceParty", make_party(GOV)),
            field("members", bare_members),
            field("threshold", make_int64(4)),
        ]);
        let created2 = created_event("rules-2", record2, 1_700_000_600);

        let mut parsed2 = extract_governance_state(&created2).expect("parses");
        parsed2.members.sort_by_key(|a| a.to_string());

        assert_eq!(parsed2.governance_party, cid(GOV));
        assert_eq!(parsed2.members, expected_members);
        assert_eq!(parsed2.threshold, 4);
        assert_eq!(parsed2.timeout_micros, None);
    }

    fn serialize_reltime_for_test(microseconds: i64) -> Value {
        make_record(vec![field("microseconds", make_int64(microseconds))])
    }

    #[test]
    fn rules_state_reads_optional_and_bare_reltime() {
        // Optional RelTime (Some) — VaultGovernanceRules shape.
        let optional_timeout = Value {
            sum: Some(value::Sum::Optional(Box::new(Optional {
                value: Some(Box::new(serialize_reltime_for_test(120_000_000))),
            }))),
        };
        let record = record_of(vec![
            field("vaultManager", make_party(GOV)),
            field(
                "members",
                Value {
                    sum: Some(value::Sum::GenMap(gen_map_of(&[]))),
                },
            ),
            field("threshold", make_int64(1)),
            field("actionConfirmationTimeout", optional_timeout),
        ]);
        let created = created_event("rules-3", record, 1_700_000_700);
        let parsed = extract_governance_state(&created).expect("parses");
        assert_eq!(parsed.timeout_micros, Some(120_000_000));

        // Bare RelTime (non-optional) — GovernanceRules shape.
        let bare_timeout = serialize_reltime_for_test(90_000_000);
        let record2 = record_of(vec![
            field("governanceParty", make_party(GOV)),
            field(
                "members",
                Value {
                    sum: Some(value::Sum::GenMap(gen_map_of(&[]))),
                },
            ),
            field("threshold", make_int64(1)),
            field("actionConfirmationTimeout", bare_timeout),
        ]);
        let created2 = created_event("rules-4", record2, 1_700_000_800);
        let parsed2 = extract_governance_state(&created2).expect("parses");
        assert_eq!(parsed2.timeout_micros, Some(90_000_000));

        // Optional RelTime (None) — the field genuinely absent inside the
        // wrapper reads as no timeout, not a parse failure.
        let optional_none = Value {
            sum: Some(value::Sum::Optional(Box::new(Optional { value: None }))),
        };
        let record3 = record_of(vec![
            field("vaultManager", make_party(GOV)),
            field(
                "members",
                Value {
                    sum: Some(value::Sum::GenMap(gen_map_of(&[]))),
                },
            ),
            field("threshold", make_int64(1)),
            field("actionConfirmationTimeout", optional_none),
        ]);
        let created3 = created_event("rules-5", record3, 1_700_000_900);
        let parsed3 = extract_governance_state(&created3).expect("parses");
        assert_eq!(parsed3.timeout_micros, None);
    }

    #[test]
    fn interface_view_proposal_parses_label_description_and_proposer() {
        let view = governable_action_interface_view(vec![
            field("actionLabel", make_text("SetupCcPreapproval")),
            field("description", make_text("Set up a CC TransferPreapproval")),
            field("proposer", make_party(ALICE)),
            field("governanceParty", make_party(GOV)),
        ]);
        let created = proposal_created_event("proposal-1", vec![view], None, None);

        let (cid_out, info) =
            extract_proposal_info(&created, &cid(GOV)).expect("view-shaped proposal parses");

        assert_eq!(cid_out, "proposal-1");
        assert_eq!(info.action_label, Some("SetupCcPreapproval".to_string()));
        assert_eq!(
            info.description,
            Some("Set up a CC TransferPreapproval".to_string())
        );
        assert_eq!(info.proposer, Some(cid(ALICE)));
    }

    #[test]
    fn mismatched_governance_party_is_dropped() {
        let view = governable_action_interface_view(vec![
            field("actionLabel", make_text("SetupCcPreapproval")),
            field("proposer", make_party(ALICE)),
            field("governanceParty", make_party(GOV)),
        ]);
        let created = proposal_created_event("proposal-2", vec![view], None, None);

        // Querying for BOB's governance while the view says the proposal is
        // governed by GOV must not surface it — BOB's members hold no
        // authority over a proposal they merely observe.
        assert_eq!(extract_proposal_info(&created, &cid(BOB)), None);
    }

    #[test]
    fn record_only_fixture_passes_the_governable_action_heuristic() {
        let record = record_of(vec![
            field("governanceParty", make_party(GOV)),
            field("proposer", make_party(ALICE)),
        ]);
        assert!(looks_like_governable_action(&record));

        // Missing either of the two fields fails the heuristic.
        let missing_proposer = record_of(vec![field("governanceParty", make_party(GOV))]);
        assert!(!looks_like_governable_action(&missing_proposer));
    }

    #[test]
    fn action_label_falls_back_view_then_record_then_template_entity_name() {
        // View carries actionLabel directly: used as-is.
        let view = governable_action_interface_view(vec![
            field("actionLabel", make_text("FromView")),
            field("proposer", make_party(ALICE)),
            field("governanceParty", make_party(GOV)),
        ]);
        let created = proposal_created_event("proposal-3", vec![view], None, None);
        let (_, info) = extract_proposal_info(&created, &cid(GOV)).expect("parses");
        assert_eq!(info.action_label, Some("FromView".to_string()));

        // View carries no actionLabel field, but create-arguments do.
        let view_no_label = governable_action_interface_view(vec![
            field("proposer", make_party(ALICE)),
            field("governanceParty", make_party(GOV)),
        ]);
        let record = record_of(vec![
            field("governanceParty", make_party(GOV)),
            field("proposer", make_party(ALICE)),
            field("actionLabel", make_text("FromRecord")),
        ]);
        let created = proposal_created_event("proposal-4", vec![view_no_label], Some(record), None);
        let (_, info) = extract_proposal_info(&created, &cid(GOV)).expect("parses");
        assert_eq!(info.action_label, Some("FromRecord".to_string()));

        // Neither view nor record carry actionLabel: falls back to the
        // template's own entity name.
        let view_bare = governable_action_interface_view(vec![
            field("proposer", make_party(ALICE)),
            field("governanceParty", make_party(GOV)),
        ]);
        let record_bare = record_of(vec![
            field("governanceParty", make_party(GOV)),
            field("proposer", make_party(ALICE)),
        ]);
        let created = proposal_created_event(
            "proposal-5",
            vec![view_bare],
            Some(record_bare),
            Some("SetupTokenPreapproval"),
        );
        let (_, info) = extract_proposal_info(&created, &cid(GOV)).expect("parses");
        assert_eq!(info.action_label, Some("SetupTokenPreapproval".to_string()));
    }
}
