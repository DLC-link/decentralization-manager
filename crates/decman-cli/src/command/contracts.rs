//! The governance-core deployment: the `POST /contracts` body that creates a
//! party's `GovernanceRules` contract, built like the terminal UI builds it.

use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde_json::{Value, json};

use crate::api::DecmanClient;

/// Inputs the caller chooses for a governance-core deployment.
#[derive(Clone, Debug)]
pub struct GovernanceCoreInput {
    pub threshold: i64,
    pub confirmation_timeout_micros: i64,
    pub extra_proposers: Vec<String>,
}

/// The facts the node supplies: the governance-core package and the members.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GovernanceCoreContext {
    pub package_id: String,
    /// `(participant id, member party id)` for each member with a known party.
    pub members: Vec<(String, String)>,
    pub operator_party: String,
}

/// Read the package id, the members, and the operator party from the node.
/// A member without a known member party is left out, as in the terminal UI.
///
/// # Errors
///
/// Returns an error if a read fails, or if there is no operator party and the
/// caller gave none.
pub fn fetch_context(
    client: &mut DecmanClient,
    party_id: &str,
    operator_party: Option<String>,
) -> Result<GovernanceCoreContext> {
    let query = [("party_id".to_owned(), party_id.to_owned())];
    let packages = client.request(Method::GET, "/packages", &query, None)?;
    let known = client.request(Method::GET, "/governance/known-members", &query, None)?;
    let operator_party = match operator_party {
        Some(party) => party,
        None => client
            .request(Method::GET, "/operator-info", &[], None)?
            .get("party_id")
            .and_then(Value::as_str)
            .filter(|party| !party.is_empty())
            .map(str::to_owned)
            .context("this node reports no operator party; pass --operator-party")?,
    };

    let members = known
        .get("members")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|member| {
            let participant = member.get("participant_uid")?.as_str()?;
            let party = member
                .get("member_party_id")?
                .as_str()
                .filter(|party| !party.is_empty())?;
            Some((participant.to_owned(), party.to_owned()))
        })
        .collect();

    Ok(GovernanceCoreContext {
        package_id: packages
            .get("governance_core")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        members,
        operator_party,
    })
}

/// Build the `POST /contracts` body.
///
/// # Errors
///
/// Returns an error if the package or the members are unknown, or the
/// threshold or timeout is out of range.
pub fn body(
    party_id: &str,
    context: &GovernanceCoreContext,
    input: &GovernanceCoreInput,
) -> Result<Value> {
    if context.package_id.is_empty() {
        bail!("this node knows no governance-core package; vet the governance-core DAR first");
    }
    if context.members.is_empty() {
        bail!("no governance members resolved; the peers are possibly unreachable");
    }
    let member_count = i64::try_from(context.members.len()).unwrap_or(i64::MAX);
    if !(1..=member_count).contains(&input.threshold) {
        bail!("--threshold must be between 1 and {member_count}, the member count");
    }
    if input.confirmation_timeout_micros <= 0 {
        bail!("--confirmation-timeout-micros must be positive");
    }

    let (participant_ids, member_parties): (Vec<&str>, Vec<&str>) = context
        .members
        .iter()
        .map(|(participant, party)| (participant.as_str(), party.as_str()))
        .unzip();
    Ok(json!({
        "decentralized_party_id": party_id,
        "participant_ids": participant_ids,
        "participant_parties": member_parties,
        "operator_party": context.operator_party,
        "contracts": [{
            "id": "create-governance-rules",
            "name": "GovernanceRules",
            "package_id": context.package_id,
            "module_name": "Governance.Rules",
            "entity_name": "GovernanceRules",
            "fields": [
                { "type": "decentralized_party" },
                { "type": "party_set", "parties": member_parties },
                { "type": "governance_threshold", "value": input.threshold },
                { "type": "rel_time", "microseconds": input.confirmation_timeout_micros },
                {
                    "type": "optional",
                    "inner": { "type": "party_set", "parties": input.extra_proposers },
                },
            ],
        }],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> GovernanceCoreContext {
        GovernanceCoreContext {
            package_id: "pkg-gov".to_owned(),
            members: vec![
                ("PAR::1".to_owned(), "m1::1220".to_owned()),
                ("PAR::2".to_owned(), "m2::1220".to_owned()),
            ],
            operator_party: "op::1220".to_owned(),
        }
    }

    fn input(threshold: i64) -> GovernanceCoreInput {
        GovernanceCoreInput {
            threshold,
            confirmation_timeout_micros: 86_400_000_000,
            extra_proposers: vec!["p::1220".to_owned()],
        }
    }

    #[test]
    fn body_lists_members_in_participant_order() {
        // Act
        let body = body("dec::1220", &context(), &input(2)).unwrap();

        // Assert
        assert_eq!(body["participant_ids"], json!(["PAR::1", "PAR::2"]));
        assert_eq!(body["participant_parties"], json!(["m1::1220", "m2::1220"]));
        assert_eq!(body["operator_party"], "op::1220");
        let fields = &body["contracts"][0]["fields"];
        assert_eq!(fields[1]["parties"], json!(["m1::1220", "m2::1220"]));
        assert_eq!(fields[2]["value"], 2);
        assert_eq!(fields[3]["microseconds"], 86_400_000_000_i64);
        assert_eq!(fields[4]["inner"]["parties"], json!(["p::1220"]));
    }

    #[test]
    fn threshold_above_the_member_count_is_refused() {
        let error = body("dec::1220", &context(), &input(3)).unwrap_err();
        assert!(error.to_string().contains("between 1 and 2"), "{error}");
    }

    #[test]
    fn unknown_package_is_refused() {
        // Arrange
        let mut context = context();
        context.package_id.clear();

        // Act / Assert
        assert!(body("dec::1220", &context, &input(1)).is_err());
    }

    #[test]
    fn no_members_is_refused() {
        // Arrange
        let mut context = context();
        context.members.clear();

        // Act / Assert
        assert!(body("dec::1220", &context, &input(1)).is_err());
    }
}
