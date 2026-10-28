//! Governance confirm, execute, revoke, expire and cancel-proposal.
//!
//! Each of these needs ids from `GET /governance/confirmations`. The command
//! reads that list, finds the action the caller names, and builds the same
//! request bodies the web frontend sends.

use anyhow::{Context, Result, bail};
use common::api::{
    CancelConfirmationRequest, CancelProposalRequest, ExpireConfirmationRequest, GovernanceType,
};
use reqwest::Method;
use serde_json::{Value, json};

use super::{canton_id, to_body};
use crate::api::{
    DecmanClient, DomainGovAction, GovAction, GovConfirmation, GovernanceConfirmations,
};

/// `core_domain` requests carry this action, which the server ignores: it
/// builds the choice from `proposal_cid`. The web frontend sends the same.
fn placeholder_action() -> Value {
    json!({ "type": "governance_set_threshold", "new_threshold": 0 })
}

/// How the caller names a pending governance action.
#[derive(Clone, Debug)]
pub enum Target {
    /// An on-chain proposal, by its contract id (`core_domain`).
    Proposal(String),
    /// The action that holds this confirmation contract.
    Confirmation(String),
    /// A self-governance action (`core_self`), by its full action JSON.
    Action(Value),
}

/// A pending action found in the confirmations list.
#[derive(Clone, Copy, Debug)]
pub enum Pending<'a> {
    SelfAction(&'a GovAction),
    Domain(&'a DomainGovAction),
}

impl Pending<'_> {
    fn confirmations(&self) -> &[GovConfirmation] {
        match self {
            Self::SelfAction(action) => &action.confirmations,
            Self::Domain(domain) => &domain.confirmations,
        }
    }

    fn governance_type(&self) -> GovernanceType {
        match self {
            Self::SelfAction(_) => GovernanceType::CoreSelf,
            Self::Domain(_) => GovernanceType::CoreDomain,
        }
    }

    fn can_execute(&self) -> bool {
        match self {
            Self::SelfAction(action) => action.can_execute,
            Self::Domain(domain) => domain.can_execute,
        }
    }

    /// The confirmation ids for the execute call: the server's live subset,
    /// or every confirmation when an older server does not send that subset.
    fn executable_confirmation_cids(&self) -> Vec<String> {
        let executable = match self {
            Self::SelfAction(action) => action.executable_confirmation_cids.as_ref(),
            Self::Domain(domain) => domain.executable_confirmation_cids.as_ref(),
        };
        executable.cloned().unwrap_or_else(|| {
            self.confirmations()
                .iter()
                .map(|confirmation| confirmation.contract_id.clone())
                .collect()
        })
    }

    /// The `action`, `governance_type` and, for a proposal, `proposal_cid`
    /// fields every request about this action carries.
    fn action_fields(&self) -> Value {
        match self {
            Self::SelfAction(action) => json!({
                "action": action.action,
                "governance_type": "core_self",
            }),
            Self::Domain(domain) => json!({
                "action": placeholder_action(),
                "governance_type": "core_domain",
                "proposal_cid": domain.proposal_cid,
            }),
        }
    }
}

/// Proposals per page when the CLI reads every page.
const PAGE_SIZE: usize = 200;

/// Pages the CLI reads at most, so a server that always returns a cursor
/// cannot keep it busy.
const MAX_PAGES: usize = 50;

/// The full confirmations list for `party_id`, with every page of proposals.
/// The server sends 25 proposals per page by default, so an older proposal
/// is only found on a later page.
///
/// # Errors
///
/// Returns an error if a request fails or a response does not parse.
pub fn fetch(client: &mut DecmanClient, party_id: &str) -> Result<GovernanceConfirmations> {
    let mut cursor: Option<String> = None;
    let mut all: Option<GovernanceConfirmations> = None;
    for page_number in 1..=MAX_PAGES {
        let mut query = vec![
            ("party_id".to_owned(), party_id.to_owned()),
            ("limit".to_owned(), PAGE_SIZE.to_string()),
        ];
        if let Some(cursor) = &cursor {
            query.push(("cursor".to_owned(), cursor.clone()));
        }
        let response = client.request(Method::GET, "/governance/confirmations", &query, None)?;
        let page: GovernanceConfirmations = serde_json::from_value(response)
            .context("failed to parse the /governance/confirmations response")?;
        cursor = page.next_cursor.clone();
        match &mut all {
            None => all = Some(page),
            Some(all) => all.domain_actions.extend(page.domain_actions),
        }
        if cursor.is_none() {
            break;
        }
        if page_number == MAX_PAGES {
            bail!(
                "the party has more than {} pending proposals; \
                 the CLI reads at most {MAX_PAGES} pages",
                MAX_PAGES * PAGE_SIZE
            );
        }
    }
    all.context("the /governance/confirmations response was empty")
}

/// The rules contract id that confirm, execute, expire and propose need.
///
/// # Errors
///
/// Returns an error if the party has no governance rules contract.
pub fn rules_contract_id(confirmations: &GovernanceConfirmations) -> Result<String> {
    confirmations
        .rules_contract_id
        .clone()
        .filter(|id| !id.is_empty())
        .context("no governance rules contract found for this party")
}

/// Find the pending action `target` names, or `None` if it is not pending.
pub fn find<'a>(
    confirmations: &'a GovernanceConfirmations,
    target: &Target,
) -> Option<Pending<'a>> {
    let has = |list: &[GovConfirmation], cid: &str| list.iter().any(|c| c.contract_id == cid);
    match target {
        Target::Proposal(cid) => confirmations
            .domain_actions
            .iter()
            .find(|domain| &domain.proposal_cid == cid)
            .map(Pending::Domain),
        Target::Action(action) => confirmations
            .actions
            .iter()
            .find(|pending| &pending.action == action)
            .map(Pending::SelfAction),
        Target::Confirmation(cid) => confirmations
            .actions
            .iter()
            .find(|pending| has(&pending.confirmations, cid))
            .map(Pending::SelfAction)
            .or_else(|| {
                confirmations
                    .domain_actions
                    .iter()
                    .find(|domain| has(&domain.confirmations, cid))
                    .map(Pending::Domain)
            }),
    }
}

fn require<'a>(confirmations: &'a GovernanceConfirmations, target: &Target) -> Result<Pending<'a>> {
    find(confirmations, target).with_context(|| match target {
        Target::Proposal(cid) => format!("proposal {cid} is not pending for this party"),
        Target::Confirmation(cid) => format!("no pending action holds confirmation {cid}"),
        Target::Action(_) => "the given action is not pending for this party".to_owned(),
    })
}

/// Merge `extra` fields into the object `base`.
fn with(mut base: Value, extra: Value) -> Value {
    if let (Some(base), Value::Object(extra)) = (base.as_object_mut(), extra) {
        base.extend(extra);
    }
    base
}

/// Body for `POST /governance/confirm`. A self action named by its JSON may be
/// new: confirming it is how a member starts it, so it need not be pending.
///
/// # Errors
///
/// Returns an error if a proposal or confirmation target is not pending.
pub fn confirm_body(
    confirmations: &GovernanceConfirmations,
    party_id: &str,
    rules_contract_id: &str,
    target: &Target,
) -> Result<Value> {
    let base = json!({ "party_id": party_id, "rules_contract_id": rules_contract_id });
    if let Target::Action(action) = target {
        return Ok(with(
            base,
            json!({ "action": action, "governance_type": "core_self" }),
        ));
    }
    Ok(with(base, require(confirmations, target)?.action_fields()))
}

/// Body for `POST /governance/execute`.
///
/// # Errors
///
/// Returns an error if the target is not pending or lacks confirmations.
pub fn execute_body(
    confirmations: &GovernanceConfirmations,
    party_id: &str,
    rules_contract_id: &str,
    target: &Target,
    disclosed_contracts: Value,
) -> Result<Value> {
    let pending = require(confirmations, target)?;
    if !pending.can_execute() {
        bail!(
            "not enough confirmations to execute this action yet (threshold {})",
            confirmations.threshold
        );
    }
    let base = json!({
        "party_id": party_id,
        "rules_contract_id": rules_contract_id,
        "confirmation_cids": pending.executable_confirmation_cids(),
        "disclosed_contracts": disclosed_contracts,
    });
    Ok(with(base, pending.action_fields()))
}

/// This node's own confirmation on `pending`, matched by its member party.
fn own_confirmation<'a>(
    confirmations: &GovernanceConfirmations,
    pending: &Pending<'a>,
) -> Option<&'a GovConfirmation> {
    let member = confirmations.member_party_id.as_deref()?;
    let list: &'a [GovConfirmation] = match pending {
        Pending::SelfAction(action) => &action.confirmations,
        Pending::Domain(domain) => &domain.confirmations,
    };
    list.iter()
        .find(|confirmation| confirmation.confirming_party == member)
}

/// Body for `POST /governance/cancel`, which revokes this node's own
/// confirmation.
///
/// # Errors
///
/// Returns an error if the target is not pending or this node has not
/// confirmed it.
pub fn revoke_body(
    confirmations: &GovernanceConfirmations,
    party_id: &str,
    target: &Target,
) -> Result<Value> {
    let pending = require(confirmations, target)?;
    let mine = own_confirmation(confirmations, &pending)
        .context("this node has no confirmation on that action")?;
    to_body(&CancelConfirmationRequest {
        party_id: canton_id(party_id)?,
        confirmation_cid: mine.contract_id.clone(),
        governance_type: pending.governance_type(),
    })
}

/// Body for `POST /governance/expire`.
///
/// # Errors
///
/// Returns an error if no pending action holds the confirmation.
pub fn expire_body(
    confirmations: &GovernanceConfirmations,
    party_id: &str,
    rules_contract_id: &str,
    confirmation_cid: &str,
) -> Result<Value> {
    let pending = require(
        confirmations,
        &Target::Confirmation(confirmation_cid.to_owned()),
    )?;
    to_body(&ExpireConfirmationRequest {
        party_id: canton_id(party_id)?,
        rules_contract_id: rules_contract_id.to_owned(),
        confirmation_cid: confirmation_cid.to_owned(),
        governance_type: pending.governance_type(),
    })
}

/// Body for `POST /governance/cancel-proposal`. It carries this node's own
/// confirmation, so the server archives it with the proposal.
///
/// # Errors
///
/// Returns an error if the proposal is not pending.
pub fn cancel_proposal_body(
    confirmations: &GovernanceConfirmations,
    party_id: &str,
    proposal_cid: &str,
) -> Result<Value> {
    let pending = require(confirmations, &Target::Proposal(proposal_cid.to_owned()))?;
    let mine = own_confirmation(confirmations, &pending).map(|c| c.contract_id.clone());
    to_body(&CancelProposalRequest {
        party_id: canton_id(party_id)?,
        proposal_cid: proposal_cid.to_owned(),
        confirmation_cid: mine,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEC: &str = "dec::1220dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

    fn confirmations() -> GovernanceConfirmations {
        serde_json::from_value(json!({
            "threshold": 2,
            "rules_contract_id": "rules",
            "member_party_id": "me::1220",
            "actions": [{
                "action": { "type": "governance_set_threshold", "new_threshold": 2 },
                "confirmations": [
                    { "contract_id": "c-self-me", "confirming_party": "me::1220", "expires_at": 0 },
                    {
                        "contract_id": "c-self-you",
                        "confirming_party": "you::1220",
                        "expires_at": 0
                    }
                ],
                "executable_confirmation_cids": ["c-self-you"],
                "confirmation_count": 2,
                "can_execute": true
            }],
            "domain_actions": [{
                "proposal_cid": "p-1",
                "action_label": "SetupUtility",
                "confirmations": [
                    { "contract_id": "c-dom-you", "confirming_party": "you::1220", "expires_at": 0 }
                ],
                "confirmation_count": 1,
                "can_execute": false
            }]
        }))
        .unwrap()
    }

    #[test]
    fn confirm_on_a_proposal_sends_the_domain_placeholder() {
        // Act
        let body = confirm_body(
            &confirmations(),
            DEC,
            "rules",
            &Target::Proposal("p-1".to_owned()),
        )
        .unwrap();

        // Assert
        assert_eq!(
            body,
            json!({
                "party_id": DEC,
                "rules_contract_id": "rules",
                "action": placeholder_action(),
                "governance_type": "core_domain",
                "proposal_cid": "p-1",
            })
        );
    }

    #[test]
    fn confirm_with_a_new_action_needs_no_pending_entry() {
        // Arrange
        let action = json!({ "type": "governance_set_threshold", "new_threshold": 3 });

        // Act
        let body = confirm_body(
            &confirmations(),
            DEC,
            "rules",
            &Target::Action(action.clone()),
        )
        .unwrap();

        // Assert
        assert_eq!(body["action"], action);
        assert_eq!(body["governance_type"], "core_self");
    }

    #[test]
    fn confirm_on_an_unknown_proposal_is_an_error() {
        let result = confirm_body(
            &confirmations(),
            DEC,
            "rules",
            &Target::Proposal("missing".to_owned()),
        );
        assert!(result.is_err());
    }

    #[test]
    fn execute_by_confirmation_finds_the_self_action() {
        // Act
        let body = execute_body(
            &confirmations(),
            DEC,
            "rules",
            &Target::Confirmation("c-self-you".to_owned()),
            json!([]),
        )
        .unwrap();

        // Assert
        assert_eq!(body["governance_type"], "core_self");
        // Only the server's live subset, not every confirmation.
        assert_eq!(body["confirmation_cids"], json!(["c-self-you"]));
        assert_eq!(body["action"]["new_threshold"], 2);
        assert_eq!(body["disclosed_contracts"], json!([]));
    }

    #[test]
    fn execute_below_threshold_is_refused() {
        let error = execute_body(
            &confirmations(),
            DEC,
            "rules",
            &Target::Proposal("p-1".to_owned()),
            json!([]),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("not enough confirmations"),
            "{error}"
        );
    }

    #[test]
    fn execute_without_executable_subset_uses_every_confirmation() {
        // Arrange
        let mut confirmations = confirmations();
        confirmations.domain_actions[0].can_execute = true;

        // Act
        let body = execute_body(
            &confirmations,
            DEC,
            "rules",
            &Target::Proposal("p-1".to_owned()),
            json!([]),
        )
        .unwrap();

        // Assert
        assert_eq!(body["confirmation_cids"], json!(["c-dom-you"]));
        assert_eq!(body["proposal_cid"], "p-1");
    }

    #[test]
    fn revoke_picks_this_nodes_confirmation() {
        // Act
        let body = revoke_body(
            &confirmations(),
            DEC,
            &Target::Confirmation("c-self-you".to_owned()),
        )
        .unwrap();

        // Assert
        assert_eq!(body["confirmation_cid"], "c-self-me");
        assert_eq!(body["governance_type"], "core_self");
    }

    #[test]
    fn revoke_without_own_confirmation_is_an_error() {
        let result = revoke_body(&confirmations(), DEC, &Target::Proposal("p-1".to_owned()));
        assert!(result.is_err());
    }

    #[test]
    fn expire_takes_the_type_of_the_action_holding_the_confirmation() {
        // Act
        let body = expire_body(&confirmations(), DEC, "rules", "c-dom-you").unwrap();

        // Assert
        assert_eq!(body["governance_type"], "core_domain");
    }

    #[test]
    fn cancel_proposal_without_own_confirmation_sends_null() {
        // Act
        let body = cancel_proposal_body(&confirmations(), DEC, "p-1").unwrap();

        // Assert
        assert_eq!(body["confirmation_cid"], Value::Null);
        assert_eq!(body["proposal_cid"], "p-1");
    }
}
