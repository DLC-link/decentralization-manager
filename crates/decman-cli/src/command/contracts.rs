//! The governance-core deployment: the `POST /contracts` body that creates a
//! party's `GovernanceRules` contract, built like the web frontend builds it.

use std::collections::HashMap;

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
    /// `(participant id, member party id)` for each participant of the party.
    pub members: Vec<(String, String)>,
    pub operator_party: String,
}

/// Read the package id, the member parties, and the operator party from the
/// node, and pair each of the party's participants with its member party.
///
/// # Errors
///
/// Returns an error if a read fails, if a participant has no member party, or
/// if there is no operator party and the caller gave none.
pub fn fetch_context(
    client: &mut DecmanClient,
    party_id: &str,
    participants: &[String],
    member_overrides: &[(String, String)],
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

    Ok(GovernanceCoreContext {
        package_id: packages
            .get("governance_core")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        members: resolve_members(participants, &known, member_overrides)?,
        operator_party,
    })
}

/// Pair each participant of the party with its member party, in the party's
/// participant order. A `--member` override wins over the member party the
/// peers report through `/governance/known-members`.
///
/// # Errors
///
/// Returns an error if an override names a participant outside the party, or
/// if a participant has no member party.
pub fn resolve_members(
    participants: &[String],
    known: &Value,
    overrides: &[(String, String)],
) -> Result<Vec<(String, String)>> {
    if let Some((stranger, _)) = overrides
        .iter()
        .find(|(participant, _)| !participants.contains(participant))
    {
        bail!("--member names {stranger}, which is not a participant of this party");
    }
    let reported: HashMap<&str, &str> = known
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
            Some((participant, party))
        })
        .collect();

    let mut members = Vec::new();
    let mut missing = Vec::new();
    for participant in participants {
        let party = overrides
            .iter()
            .find(|(id, _)| id == participant)
            .map(|(_, party)| party.as_str())
            .or_else(|| reported.get(participant.as_str()).copied());
        match party {
            Some(party) => members.push((participant.clone(), party.to_owned())),
            None => missing.push(participant.as_str()),
        }
    }
    if !missing.is_empty() {
        bail!(
            "no member party is known for {}; pass --member PARTICIPANT=PARTY for each",
            missing.join(", ")
        );
    }
    Ok(members)
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

    fn known() -> Value {
        json!({ "members": [
            { "participant_uid": "PAR::1", "member_party_id": "m1::1220" },
            { "participant_uid": "PAR::2" },
            { "participant_uid": "PAR::9", "member_party_id": "m9::1220" },
        ] })
    }

    #[test]
    fn members_follow_the_party_and_skip_other_network_participants() {
        // Act
        let members = resolve_members(
            &["PAR::1".to_owned(), "PAR::2".to_owned()],
            &known(),
            &[("PAR::2".to_owned(), "m2::1220".to_owned())],
        )
        .unwrap();

        // Assert
        assert_eq!(
            members,
            vec![
                ("PAR::1".to_owned(), "m1::1220".to_owned()),
                ("PAR::2".to_owned(), "m2::1220".to_owned()),
            ]
        );
    }

    #[test]
    fn an_override_wins_over_a_reported_member_party() {
        // Act
        let members = resolve_members(
            &["PAR::1".to_owned()],
            &known(),
            &[("PAR::1".to_owned(), "x::1220".to_owned())],
        )
        .unwrap();

        // Assert
        assert_eq!(members, vec![("PAR::1".to_owned(), "x::1220".to_owned())]);
    }

    #[test]
    fn a_participant_without_a_member_party_is_named_in_the_error() {
        let error = resolve_members(&["PAR::1".to_owned(), "PAR::2".to_owned()], &known(), &[])
            .unwrap_err();
        assert!(error.to_string().contains("PAR::2"), "{error}");
        assert!(!error.to_string().contains("PAR::1,"), "{error}");
    }

    #[test]
    fn an_override_outside_the_party_is_refused() {
        let result = resolve_members(
            &["PAR::1".to_owned()],
            &known(),
            &[("PAR::7".to_owned(), "x::1220".to_owned())],
        );
        assert!(result.is_err());
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

        // Act
        let error = body("dec::1220", &context, &input(1)).unwrap_err();

        // Assert
        assert!(
            error.to_string().contains("governance-core package"),
            "{error}"
        );
    }

    #[test]
    fn no_members_is_refused() {
        // Arrange
        let mut context = context();
        context.members.clear();

        // Act
        let error = body("dec::1220", &context, &input(1)).unwrap_err();

        // Assert
        assert!(
            error.to_string().contains("no governance members"),
            "{error}"
        );
    }
}
