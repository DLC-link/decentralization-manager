//! `GovernableAction` proposal info and the detail extractors behind the
//! notification cards.

use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::daml::ledger::api::v2::{CreatedEvent, Identifier, Record};
use common::canton_id::CantonId;

use crate::{
    catalog::types::{AcceptTransferDetails, ServiceRequestDetails, TransferProposalDetails},
    framework::record::{
        field_contract_id, field_numeric, field_party, field_record, field_text, record_has_field,
    },
};

/// Longest proposal description retained by [`extract_proposal_info`].
///
/// The approvals list keeps one [`ProposalInfo`] per proposal, so an unbounded
/// field here scales with a party's entire proposal history rather than with
/// what the page shows. A party carrying 26k proposals of padded text OOMKilled
/// a node at 4Gi (#424). The full text stays on the ledger and in the
/// execution result; this is the card's preview.
pub const DESCRIPTION_PREVIEW_BYTES: usize = 4096;

/// Truncate `text` to at most [`DESCRIPTION_PREVIEW_BYTES`], on a character
/// boundary, marking it when shortened.
fn description_preview(text: String) -> String {
    if text.len() <= DESCRIPTION_PREVIEW_BYTES {
        return text;
    }
    let end = (0..=DESCRIPTION_PREVIEW_BYTES)
        .rev()
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(0);
    let mut out = String::with_capacity(end + 3);
    out.push_str(&text[..end]);
    out.push('…');
    out
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
#[derive(Clone, Debug, Default, PartialEq)]
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
    /// The template the proposal contract was created from, as the event
    /// names it.
    pub template_id: Option<Identifier>,
    /// The proposal contract's create arguments, kept whole. A member who
    /// confirms or executes the proposal acts on every field here, not only
    /// on the ones the summary fields above pick out.
    pub create_arguments: Option<Record>,
}

/// Pull the `GovernableAction` interface view off a created event. Canton
/// only fills this in when the query asked for it with an `InterfaceFilter`,
/// so a caller that fetches without one always gets `None`.
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
/// available: a fetch without an interface filter can return contracts of any
/// template, so something has to keep unrelated ones out of the proposal map.
fn looks_like_governable_action(record: &Record) -> bool {
    record_has_field(record, "governanceParty") && record_has_field(record, "proposer")
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
/// A caller that fetches without an interface filter gets no view, so it
/// falls back to the field-shape heuristic and to create-arguments for the
/// same values.
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

    // Absent rather than mismatched is not a rejection: without a view, a
    // template that computes the field there has nothing to compare.
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
        .or_else(|| record.and_then(|r| field_text(r, "description")))
        .map(description_preview);

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
    let accept_transfer_instruction_cid =
        record.and_then(|r| field_contract_id(r, "transferInstructionCid"));

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
            template_id: created.template_id.clone(),
            create_arguments: created.create_arguments.clone(),
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

/// Pull `receiver`, `amount`, and the nested `instrumentId` out of a
/// `TransferProposal`'s `transfer` field. Returns `None` for any proposal
/// that doesn't have a `transfer` record (every non-transfer template).
pub fn extract_transfer_proposal_details(record: &Record) -> Option<TransferProposalDetails> {
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

    use super::*;
    use crate::{
        catalog::interpret::fixtures::{
            ALICE, BOB, GOV, cid, created_event, governable_action_interface_view,
            proposal_created_event, record_of,
        },
        framework::encode::{field, make_int64, make_party, make_text},
    };

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

    #[test]
    fn view_description_wins_over_the_create_argument_description() {
        // Templates such as CreateUserServiceRequest compute `description` in
        // the view and hold no field of that name, so the view must win.
        let view = governable_action_interface_view(vec![
            field("actionLabel", make_text("MintProposal")),
            field("description", make_text("computed in the view")),
            field("proposer", make_party(ALICE)),
            field("governanceParty", make_party(GOV)),
        ]);
        let record = record_of(vec![
            field("governanceParty", make_party(GOV)),
            field("proposer", make_party(ALICE)),
            field("description", make_text("stored on the template")),
        ]);
        let created = proposal_created_event("proposal-6", vec![view], Some(record), None);

        let (_, info) = extract_proposal_info(&created, &cid(GOV)).expect("parses");
        assert_eq!(info.description, Some("computed in the view".to_string()));
    }

    #[test]
    fn view_proposer_wins_over_the_create_argument_proposer() {
        // The retract button keys on this field, and the interface declares
        // `proposer`, so the view is the authoritative source.
        let view = governable_action_interface_view(vec![
            field("actionLabel", make_text("MintProposal")),
            field("proposer", make_party(ALICE)),
            field("governanceParty", make_party(GOV)),
        ]);
        let record = record_of(vec![
            field("governanceParty", make_party(GOV)),
            field("proposer", make_party(BOB)),
        ]);
        let created = proposal_created_event("proposal-7", vec![view], Some(record), None);

        let (_, info) = extract_proposal_info(&created, &cid(GOV)).expect("parses");
        assert_eq!(info.proposer, Some(cid(ALICE)));
    }

    #[test]
    fn record_only_proposal_falls_back_to_the_create_argument_proposer() {
        // A fetch without an interface filter carries no view, so the raw
        // fields are all there is — the heuristic captures the contract and
        // the record names the proposer.
        let record = record_of(vec![
            field("governanceParty", make_party(GOV)),
            field("proposer", make_party(BOB)),
        ]);
        let created = created_event("proposal-8", record, 1_700_000_700);

        let (cid_out, info) = extract_proposal_info(&created, &cid(GOV)).expect("parses");
        assert_eq!(cid_out, "proposal-8");
        assert_eq!(info.proposer, Some(cid(BOB)));
    }

    #[test]
    fn proposal_captures_created_at() {
        // The feed sorts on this, so an unconfirmed card holds its place
        // between refreshes instead of shuffling.
        let record = record_of(vec![
            field("governanceParty", make_party(GOV)),
            field("proposer", make_party(ALICE)),
        ]);
        let created = created_event("proposal-9", record, 1_700_000_500);

        let (_, info) = extract_proposal_info(&created, &cid(GOV)).expect("parses");
        assert_eq!(info.created_at, Some(1_700_000_500));
    }

    #[test]
    fn unrelated_wildcard_contract_is_skipped() {
        // No interface view and neither heuristic field: not a proposal.
        let record = record_of(vec![field("owner", make_party(ALICE))]);
        let created = created_event("other-cid", record, 1_700_000_000);

        assert_eq!(extract_proposal_info(&created, &cid(GOV)), None);
    }

    // -- extract_service_request_details --
    //
    // CreateUserServiceRequest / CreateProviderServiceRequest carry operator +
    // user/provider as top-level Party fields on the proposal contract. The
    // notification card renders operator + the present counterparty so the
    // operator sees the full summary alongside the action_label.

    #[test]
    fn service_request_details_reads_a_user_request() {
        let record = record_of(vec![
            field("governanceParty", make_party(GOV)),
            field("proposer", make_party(ALICE)),
            field("operator", make_party(ALICE)),
            field("user", make_party(BOB)),
        ]);
        let details = extract_service_request_details(&record)
            .expect("user service request should yield details");
        assert_eq!(details.operator, cid(ALICE));
        assert_eq!(details.user, Some(cid(BOB)));
        assert!(details.provider.is_none());
    }

    #[test]
    fn service_request_details_reads_a_provider_request() {
        let record = record_of(vec![
            field("governanceParty", make_party(GOV)),
            field("proposer", make_party(ALICE)),
            field("operator", make_party(ALICE)),
            field("provider", make_party(BOB)),
        ]);
        let details = extract_service_request_details(&record)
            .expect("provider service request should yield details");
        assert_eq!(details.operator, cid(ALICE));
        assert_eq!(details.provider, Some(cid(BOB)));
        assert!(details.user.is_none());
    }

    #[test]
    fn service_request_details_skips_a_proposal_without_a_counterparty() {
        // operator present but no user/provider counterparty → not a service
        // request, so no details (keeps unrelated operator-bearing proposals
        // from rendering a half-empty summary).
        let record = record_of(vec![
            field("governanceParty", make_party(GOV)),
            field("proposer", make_party(ALICE)),
            field("operator", make_party(ALICE)),
        ]);
        assert_eq!(extract_service_request_details(&record), None);
    }

    #[test]
    fn service_request_details_are_gated_on_the_action_label() {
        // The party fields alone must not produce a service-request summary.
        // Onboarding is only what the two Create*ServiceRequest actions do.
        let service_request_fields = || {
            record_of(vec![
                field("governanceParty", make_party(GOV)),
                field("proposer", make_party(ALICE)),
                field("operator", make_party(ALICE)),
                field("user", make_party(BOB)),
            ])
        };

        let non_matching = governable_action_interface_view(vec![
            field("actionLabel", make_text("MintProposal")),
            field("proposer", make_party(ALICE)),
            field("governanceParty", make_party(GOV)),
        ]);
        let created = proposal_created_event(
            "proposal-10",
            vec![non_matching],
            Some(service_request_fields()),
            None,
        );
        let (_, info) = extract_proposal_info(&created, &cid(GOV))
            .expect("the proposal should still be captured");
        assert!(info.service_request.is_none());

        // The same create-arguments under a matching label do carry the parties.
        let matching = governable_action_interface_view(vec![
            field("actionLabel", make_text("CreateUserServiceRequest")),
            field("proposer", make_party(ALICE)),
            field("governanceParty", make_party(GOV)),
        ]);
        let created = proposal_created_event(
            "proposal-11",
            vec![matching],
            Some(service_request_fields()),
            None,
        );
        let (_, info) = extract_proposal_info(&created, &cid(GOV)).expect("parses");
        let details = info
            .service_request
            .expect("a user service request should carry its parties");
        assert_eq!(details.operator, cid(ALICE));
        assert_eq!(details.user, Some(cid(BOB)));
    }

    #[test]
    fn proposal_info_keeps_the_whole_create_arguments_and_template() {
        // `nonce` is a field no summary reads. The member still signs it, so
        // it has to survive the parse.
        let record = record_of(vec![
            field("governanceParty", make_party(GOV)),
            field("proposer", make_party(ALICE)),
            field("nonce", make_int64(42)),
        ]);
        let view = governable_action_interface_view(vec![
            field("actionLabel", make_text("GenericVote")),
            field("proposer", make_party(ALICE)),
            field("governanceParty", make_party(GOV)),
        ]);
        let created = proposal_created_event(
            "proposal-full",
            vec![view],
            Some(record.clone()),
            Some("GenericVoteProposal"),
        );

        let (_, info) = extract_proposal_info(&created, &cid(GOV)).expect("parses");

        assert_eq!(info.create_arguments, Some(record));
        assert_eq!(info.template_id, created.template_id);
    }
}
