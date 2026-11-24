//! Confirmation and governance-rules parses over a single `CreatedEvent`.

use canton_proto_rs::com::daml::ledger::api::v2::CreatedEvent;
use common::canton_id::CantonId;

use crate::{
    catalog::action::ActionType,
    framework::record::{
        extract_optional_party_set, extract_optional_reltime, extract_party_set, extract_reltime,
        field_contract_id, field_int64, field_party, field_text, field_timestamp, record_value,
    },
};

/// A parsed CBTC-governance or governance-core self-action confirmation.
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

/// A governance-core domain confirmation. The on-ledger template stores no
/// action — only the proposal cid and a label. DecMan's `DomainConfirmation`
/// wire DTO carries no action either, so no layer invents one.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedDomainConfirmation {
    pub contract_id: String,
    pub proposal_cid: String,
    pub action_label: String,
    pub confirming_party: CantonId,
    pub created_at: i64,
    pub expires_at: i64,
}

/// State of a `GovernanceRules` contract.
#[derive(Clone, Debug, PartialEq)]
pub struct RulesState {
    pub contract_id: String,
    pub governance_party: CantonId,
    pub members: Vec<CantonId>,
    pub threshold: i64,
    pub timeout_micros: Option<i64>,
    /// Parties that may propose but not confirm or execute.
    pub additional_proposers: Vec<CantonId>,
    /// Parties that observe the rules contract without hosting the
    /// governance party.
    pub disclosure_observers: Vec<CantonId>,
}

/// Parse a confirmation contract's action, confirming party, and timestamps
/// off its created event. Tries the CBTC `ActionRequiringConfirmation`
/// shape first, then falls back to governance-core's `GovernanceSelfAction`
/// shape. Returns `None` if the action field is absent/unrecognized, or the
/// confirming party is missing/invalid.
pub fn parse_confirmation(created: &CreatedEvent) -> Option<ParsedConfirmation> {
    let record = created.create_arguments.as_ref()?;

    // Extract action field (a Variant in both shapes)
    let Some(action_field) = record_value(record, "action") else {
        tracing::warn!("No action field found in confirmation contract");
        return None;
    };

    // Try to parse the action (ActionRequiringConfirmation or GovernanceSelfAction)
    let action = match ActionType::from_cbtc_proto(action_field) {
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
    // Two names for one field across templates: governance-core's
    // `GovernanceConfirmation` calls it `confirmer`, the cbtc path
    // `confirmingParty`. No template declares both, so the fallback only ever
    // picks the one that is there. It differs from a single find-either-label
    // only for a record carrying both with the first malformed, which the
    // fixed template schemas do not produce.
    let Some(confirming_party_str) =
        field_party(record, "confirmingParty").or_else(|| field_party(record, "confirmer"))
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
    let proposal_cid = field_contract_id(record, "actionProposalCid").unwrap_or_default();

    // Extract actionLabel (Text)
    let action_label = field_text(record, "actionLabel").unwrap_or_default();

    // Extract confirmer (Party). Skip the confirmation if missing or
    // malformed (see the off-chain extractor above for the same rationale).
    let Some(confirmer_str) = field_party(record, "confirmer") else {
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

/// Extract governance rules state from a `GovernanceRules` created event.
pub fn extract_governance_state(created: &CreatedEvent) -> Option<RulesState> {
    let record = created.create_arguments.as_ref()?;

    let governance_party: CantonId = field_party(record, "governanceParty")?.parse().ok()?;

    // Extract members (Set Party - stored as GenMap<Party, Unit> inside a Record)
    let members: Vec<CantonId> = record_value(record, "members")
        .and_then(extract_party_set)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| s.parse().ok())
        .collect();

    // Extract threshold (Int)
    let threshold = field_int64(record, "threshold").unwrap_or(0);

    // Extract actionConfirmationTimeout. Older rules contracts wrap it in an
    // Optional; the current GovernanceRules declares it non-optional.
    let timeout_micros = record_value(record, "actionConfirmationTimeout")
        .and_then(|v| extract_optional_reltime(v).or_else(|| extract_reltime(v)));

    // additionalProposers : Optional (Set Party). None, and older rules
    // contracts that predate the field, read as no additional proposers.
    let additional_proposers: Vec<CantonId> = record_value(record, "additionalProposers")
        .and_then(extract_optional_party_set)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| s.parse().ok())
        .collect();

    // disclosureObservers : Optional (Set Party). Rules contracts created
    // under governance-core 0.1.0 lack the field and read as no observers.
    let disclosure_observers: Vec<CantonId> = record_value(record, "disclosureObservers")
        .and_then(extract_optional_party_set)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| s.parse().ok())
        .collect();

    Some(RulesState {
        contract_id: created.contract_id.clone(),
        governance_party,
        members,
        threshold,
        timeout_micros,
        additional_proposers,
        disclosure_observers,
    })
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::daml::ledger::api::v2::{GenMap, Optional, Value, gen_map, value};

    use super::*;
    use crate::{
        catalog::interpret::fixtures::{ALICE, BOB, GOV, cid, created_event, record_of},
        framework::encode::{
            field, make_contract_id, make_int64, make_optional_party_set, make_party,
            make_party_set, make_record, make_text, make_variant,
        },
    };

    /// A `Governance_SetThreshold` action in the CBTC
    /// `ActionRequiringConfirmation` shape. Hand-built because
    /// `#cbtc-governance` owns the form and the lib has no encoder for it.
    fn cbtc_set_threshold(new_threshold: i64) -> Value {
        make_variant(
            "GovernanceAction",
            make_variant(
                "Governance_SetThreshold",
                make_record(vec![field("newThreshold", make_int64(new_threshold))]),
            ),
        )
    }

    #[test]
    fn parses_a_cbtc_confirmation_with_its_action() {
        let record = record_of(vec![
            field("action", cbtc_set_threshold(2)),
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
                action: ActionType::GovernanceSetThreshold { new_threshold: 2 },
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
        let cbtc_record = record_of(vec![field("action", cbtc_set_threshold(1))]);
        let cbtc_created = created_event("confirmation-3", cbtc_record, 1_700_000_300);
        assert_eq!(parse_confirmation(&cbtc_created), None);

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
            field("governanceParty", make_party(GOV)),
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
        // Optional RelTime (Some) — an older rules contract wraps it.
        let optional_timeout = Value {
            sum: Some(value::Sum::Optional(Box::new(Optional {
                value: Some(Box::new(serialize_reltime_for_test(120_000_000))),
            }))),
        };
        let record = record_of(vec![
            field("governanceParty", make_party(GOV)),
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
            field("governanceParty", make_party(GOV)),
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
    fn rules_state_reads_additional_proposers() {
        let members = || Value {
            sum: Some(value::Sum::GenMap(gen_map_of(&[ALICE]))),
        };
        let optional = |inner: Option<Value>| Value {
            sum: Some(value::Sum::Optional(Box::new(Optional {
                value: inner.map(Box::new),
            }))),
        };
        let rules = |additional: Option<Value>| {
            let mut fields = vec![
                field("governanceParty", make_party(GOV)),
                field("members", members()),
                field("threshold", make_int64(1)),
            ];
            if let Some(value) = additional {
                fields.push(field("additionalProposers", value));
            }
            created_event("rules-ap", record_of(fields), 1_700_001_000)
        };

        // Some(Set Party): the set's parties.
        let set = make_record(vec![field(
            "map",
            Value {
                sum: Some(value::Sum::GenMap(gen_map_of(&[BOB]))),
            },
        )]);
        let parsed = extract_governance_state(&rules(Some(optional(Some(set))))).expect("parses");
        assert_eq!(parsed.additional_proposers, vec![cid(BOB)]);
        assert_eq!(parsed.members, vec![cid(ALICE)]);

        // None, and a rules contract that predates the field: no proposers.
        let parsed = extract_governance_state(&rules(Some(optional(None)))).expect("parses");
        assert!(parsed.additional_proposers.is_empty());
        let parsed = extract_governance_state(&rules(None)).expect("parses");
        assert!(parsed.additional_proposers.is_empty());
    }

    #[test]
    fn rules_state_reads_disclosure_observers() {
        let rules = |observers: Option<Value>| {
            let mut fields = vec![
                field("governanceParty", make_party(GOV)),
                field("members", make_party_set(&[ALICE])),
                field("threshold", make_int64(1)),
                field(
                    "additionalProposers",
                    make_optional_party_set(Some(&[ALICE])),
                ),
            ];
            if let Some(value) = observers {
                fields.push(field("disclosureObservers", value));
            }
            created_event("rules-do", record_of(fields), 1_700_001_000)
        };

        let parsed = extract_governance_state(&rules(Some(make_optional_party_set(Some(&[BOB])))))
            .expect("parses");
        assert_eq!(parsed.disclosure_observers, vec![cid(BOB)]);
        assert_eq!(parsed.additional_proposers, vec![cid(ALICE)]);

        // None, and a 0.1.0 rules contract that has no such field.
        let none: Option<&[&str]> = None;
        let parsed =
            extract_governance_state(&rules(Some(make_optional_party_set(none)))).expect("parses");
        assert!(parsed.disclosure_observers.is_empty());
        let parsed = extract_governance_state(&rules(None)).expect("parses");
        assert!(parsed.disclosure_observers.is_empty());
        assert_eq!(parsed.members, vec![cid(ALICE)]);
    }
}
