//! Per-event parses over a Ledger API `CreatedEvent`: governance
//! confirmations and governance-rules state.
//!
//! Each function here is a pure, single-event parse — no grouping, no map
//! insertion, no I/O. DecMan's `queries.rs` currently owns its own copies
//! that additionally group results into `HashMap`s and (for the on-chain
//! action hash) call `compute_action_hash`, which stays a decman concern.
//! Task 20 switches `queries.rs` onto these.

use canton_proto_rs::com::daml::ledger::api::v2::{CreatedEvent, value};
use common::canton_id::CantonId;

use crate::catalog::action::ActionType;
use crate::framework::record::{
    extract_optional_reltime, extract_party_set, extract_reltime, field_timestamp,
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

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::daml::ledger::api::v2::{
        GenMap, Optional, Record, RecordField, Value, gen_map,
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
}
