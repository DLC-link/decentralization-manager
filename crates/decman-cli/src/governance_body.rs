//! Wire bodies shared by terminal and command governance operations.

use serde_json::{Value, json};

pub(crate) fn placeholder_action() -> Value {
    json!({ "type": "governance_set_threshold", "new_threshold": 0 })
}

pub(crate) fn confirm(
    party: &str,
    rules: &str,
    action: &Value,
    kind: &str,
    proposal: Option<&str>,
) -> Value {
    let mut body = json!({"party_id": party, "rules_contract_id": rules, "action": action, "governance_type": kind});
    if let Some(cid) = proposal {
        body["proposal_cid"] = json!(cid);
    }
    body
}

pub(crate) fn execute(
    party: &str,
    rules: &str,
    action: &Value,
    kind: &str,
    proposal: Option<&str>,
    confirmations: &[String],
    disclosed: Value,
) -> Value {
    let mut body = confirm(party, rules, action, kind, proposal);
    body["confirmation_cids"] = json!(confirmations);
    body["disclosed_contracts"] = disclosed;
    body
}

pub(crate) fn revoke(party: &str, confirmation: &str, kind: &str) -> Value {
    json!({"party_id": party, "confirmation_cid": confirmation, "governance_type": kind})
}

pub(crate) fn expire(party: &str, rules: &str, confirmation: &str, kind: &str) -> Value {
    let mut body = revoke(party, confirmation, kind);
    body["rules_contract_id"] = json!(rules);
    body
}

pub(crate) fn cancel_proposal(party: &str, proposal: &str, confirmation: Option<&str>) -> Value {
    json!({"party_id": party, "proposal_cid": proposal, "confirmation_cid": confirmation})
}

#[cfg(test)]
mod tests {
    use super::*;

    use common::api::{
        CancelConfirmationRequest, CancelProposalRequest, ExpireConfirmationRequest, GovernanceType,
    };
    use common::canton_id::CantonId;

    const PARTY: &str = "dec::1220dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

    #[test]
    fn confirm_omits_absent_proposal_and_preserves_action() {
        let action = json!({"type": "governance_set_threshold", "new_threshold": 3});
        assert_eq!(
            confirm(PARTY, "rules", &action, "core_self", None),
            json!({
                "party_id": PARTY, "rules_contract_id": "rules", "action": action, "governance_type": "core_self"
            })
        );
    }

    #[test]
    fn domain_execute_preserves_disclosures_and_confirmation_order() {
        let disclosed = json!([{"contract_id": "contract", "blob": "payload"}]);
        assert_eq!(
            execute(
                PARTY,
                "rules",
                &placeholder_action(),
                "core_domain",
                Some("proposal"),
                &["second".into(), "first".into()],
                disclosed.clone()
            ),
            json!({
                "party_id": PARTY, "rules_contract_id": "rules", "action": {"type": "governance_set_threshold", "new_threshold": 0},
                "governance_type": "core_domain", "proposal_cid": "proposal", "confirmation_cids": ["second", "first"], "disclosed_contracts": disclosed
            })
        );
    }

    #[test]
    fn revoke_and_expire_match_server_request_serialization() {
        for (kind, name) in [
            (GovernanceType::CoreSelf, "core_self"),
            (GovernanceType::CoreDomain, "core_domain"),
        ] {
            assert_eq!(
                revoke(PARTY, "confirmation", name),
                serde_json::to_value(CancelConfirmationRequest {
                    party_id: CantonId::parse(PARTY).unwrap(),
                    confirmation_cid: "confirmation".into(),
                    governance_type: kind
                })
                .unwrap()
            );
            assert_eq!(
                expire(PARTY, "rules", "confirmation", name),
                serde_json::to_value(ExpireConfirmationRequest {
                    party_id: CantonId::parse(PARTY).unwrap(),
                    rules_contract_id: "rules".into(),
                    confirmation_cid: "confirmation".into(),
                    governance_type: kind
                })
                .unwrap()
            );
        }
    }

    #[test]
    fn cancellation_matches_server_with_and_without_own_confirmation() {
        for confirmation in [None, Some("mine")] {
            assert_eq!(
                cancel_proposal(PARTY, "proposal", confirmation),
                serde_json::to_value(CancelProposalRequest {
                    party_id: CantonId::parse(PARTY).unwrap(),
                    proposal_cid: "proposal".into(),
                    confirmation_cid: confirmation.map(str::to_owned)
                })
                .unwrap()
            );
        }
        assert!(
            cancel_proposal(PARTY, "proposal", None)
                .get("confirmation_cid")
                .unwrap()
                .is_null()
        );
    }
}
