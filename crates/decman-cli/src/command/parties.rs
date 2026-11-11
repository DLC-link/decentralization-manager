//! Party resolution and the party workflows.

use anyhow::{Context, Result, bail};
use common::api::{AddPartyRequest, ChangeThresholdRequest, KickRequest, OnboardingRequest};
use common::canton_id::CantonId;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use super::Outcome;
use super::args::{PartiesCommand, PartyArg, WaitArgs};
use super::http::{get, optional, to_body};
use super::wait::{RunSelector, wait_for_run};
use crate::api::{ApiError, DecmanClient, mesh_error_hint};

/// A party resolved from a [`PartyArg`], with its current threshold when the
/// node lists it.
pub struct ResolvedParty {
    pub id: String,
    threshold: Option<i32>,
    /// `None` when the node does not list the party, or when its party list
    /// predates the field.
    confirmation_threshold: Option<i32>,
    /// The participant ids that host the party. Empty when the node does not
    /// list the party.
    pub participants: Vec<String>,
}

/// Resolve a party name or id against `GET /decentralized-parties`. A full id
/// that the list does not hold is passed through, so a command can still name
/// a party this node does not cache.
pub fn resolve_party(client: &mut DecmanClient, party: &PartyArg) -> Result<ResolvedParty> {
    let arg = party.party.as_str();
    let name = arg.split("::").next().unwrap_or(arg);
    let response = client.request(
        Method::GET,
        "/decentralized-parties",
        &[("prefix".to_owned(), name.to_owned())],
        None,
    )?;
    let parties = response
        .get("parties")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let matches = matching_parties(&parties, arg);
    match matches.as_slice() {
        [party] => Ok(ResolvedParty {
            id: party["party_id"].as_str().unwrap_or(arg).to_owned(),
            threshold: as_i32(&party["threshold"]),
            confirmation_threshold: as_i32(&party["confirmation_threshold"]),
            participants: party["participants"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|participant| participant["participant_uid"].as_str())
                .map(str::to_owned)
                .collect(),
        }),
        [] if arg.contains("::") => Ok(ResolvedParty {
            id: arg.to_owned(),
            threshold: None,
            confirmation_threshold: None,
            participants: Vec::new(),
        }),
        [] => bail!("this node has no decentralized party named {arg}"),
        several => {
            let ids: Vec<&str> = several
                .iter()
                .filter_map(|party| party["party_id"].as_str())
                .collect();
            bail!(
                "several parties are named {arg}; pass a full party id: {}",
                ids.join(", ")
            )
        }
    }
}

pub fn canton_id(id: &str) -> Result<CantonId> {
    CantonId::parse(id).with_context(|| format!("{id} is not a valid Canton id"))
}

fn canton_ids(ids: &[String]) -> Result<Vec<CantonId>> {
    ids.iter().map(|id| canton_id(id)).collect()
}

fn as_i32(value: &Value) -> Option<i32> {
    value.as_i64().and_then(|value| i32::try_from(value).ok())
}

/// The current threshold of `party`. The server shows it to the peers as the
/// "before" value, so an unknown threshold is an error, not 0.
fn known_threshold(party: &ResolvedParty) -> Result<i32> {
    party.threshold.with_context(|| {
        format!(
            "this node does not list {} with its threshold; run the command on a node that does",
            party.id
        )
    })
}

/// The parties `arg` names: the one with that full id, or every party whose
/// name (the part before `::`) equals `arg`.
fn matching_parties<'a>(parties: &'a [Value], arg: &str) -> Vec<&'a Value> {
    parties
        .iter()
        .filter(|party| {
            let id = party["party_id"].as_str().unwrap_or_default();
            if arg.contains("::") {
                id == arg
            } else {
                id.split("::").next() == Some(arg)
            }
        })
        .collect()
}

/// Start a workflow with `POST path` and, with `--wait`, wait for its run.
pub fn start_workflow(
    client: &mut DecmanClient,
    path: &str,
    body: &Value,
    wait: &WaitArgs,
) -> Result<Outcome> {
    let response = client.request(Method::POST, path, &[], Some(body))?;
    if !wait.wait {
        return Ok(Outcome::ok(response));
    }
    let Some(name) = response
        .get("instance_name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
    else {
        // The workflow started, so print what the server said before failing.
        return Ok(Outcome {
            value: json!({
                "response": response,
                "error": "the workflow started, but the response names no run to wait for",
            }),
            success: false,
        });
    };
    eprintln!("started workflow run {name}");
    wait_for_run(client, &RunSelector::Instance(name), wait.options())
}

pub fn run_parties(client: &mut DecmanClient, command: PartiesCommand) -> Result<Outcome> {
    match command {
        PartiesCommand::List {
            prefix,
            include_contracts,
            refresh,
        } => {
            let mut query = Vec::new();
            optional(&mut query, "prefix", prefix);
            if include_contracts {
                query.push(("include_contracts", "true".to_owned()));
            }
            if refresh {
                query.push(("refresh", "true".to_owned()));
            }
            get(client, "/decentralized-parties", &query)
        }
        PartiesCommand::External => get(client, "/external-parties", &[]),
        PartiesCommand::Create {
            prefix,
            peers,
            threshold,
            confirmation_threshold,
            wait,
        } => {
            let body = to_body(&OnboardingRequest {
                party_id_prefix: prefix,
                peer_ids: canton_ids(&peers)?,
                threshold,
                confirmation_threshold,
            })?;
            start_workflow(client, "/onboarding", &body, &wait).map_err(with_mesh_hint)
        }
        PartiesCommand::AddMember {
            party,
            participant,
            threshold,
            wait,
        } => {
            let party = resolve_party(client, &party)?;
            let body = to_body(&AddPartyRequest {
                decentralized_party_id: canton_id(&party.id)?,
                new_participant_id: canton_id(&participant)?,
                new_threshold: threshold,
                previous_threshold: known_threshold(&party)?,
            })?;
            start_workflow(client, "/add-party", &body, &wait)
        }
        PartiesCommand::RemoveMember {
            party,
            participant,
            threshold,
            wait,
        } => {
            let party = resolve_party(client, &party)?;
            let body = to_body(&KickRequest {
                decentralized_party_id: canton_id(&party.id)?,
                participant_id: Some(canton_id(&participant)?),
                owner_key: None,
                new_threshold: threshold,
                previous_threshold: known_threshold(&party)?,
            })?;
            start_workflow(client, "/kick", &body, &wait)
        }
        PartiesCommand::SetThreshold {
            party,
            threshold,
            confirmation_threshold,
            wait,
        } => {
            let party = resolve_party(client, &party)?;
            let body = to_body(&ChangeThresholdRequest {
                decentralized_party_id: canton_id(&party.id)?,
                new_threshold: threshold,
                confirmation_threshold: new_confirmation_threshold(
                    &party,
                    threshold,
                    confirmation_threshold,
                ),
                previous_threshold: known_threshold(&party)?,
            })?;
            start_workflow(client, "/change-threshold", &body, &wait)
        }
    }
}

/// The confirmation threshold for `set-threshold`, as the web UI picks it: the
/// `--confirmation-threshold` value, else the party's current one when it
/// differs from its signing threshold, else the new signing threshold. `None`
/// means "the same as the new signing threshold", which the server applies.
fn new_confirmation_threshold(
    party: &ResolvedParty,
    new_threshold: i32,
    requested: Option<i32>,
) -> Option<i32> {
    let kept = party
        .confirmation_threshold
        .filter(|current| Some(*current) != party.threshold);
    requested
        .or(kept)
        .filter(|confirmation| *confirmation != new_threshold)
}

/// Replace a `422` onboarding error with its mesh remediation hints.
fn with_mesh_hint(error: anyhow::Error) -> anyhow::Error {
    let hint = error
        .downcast_ref::<ApiError>()
        .filter(|api| api.status == StatusCode::UNPROCESSABLE_ENTITY)
        .and_then(|api| mesh_error_hint(&api.body));
    match hint {
        Some(hint) => anyhow::anyhow!("{hint}"),
        None => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parties() -> Vec<Value> {
        vec![
            json!({ "party_id": "alpha::1220aa", "threshold": 2 }),
            json!({ "party_id": "alpha-v2::1220bb", "threshold": 1 }),
            json!({ "party_id": "gamma::1220cc", "threshold": 1 }),
            json!({ "party_id": "gamma::1220dd", "threshold": 1 }),
        ]
    }

    #[test]
    fn party_name_matches_the_whole_name_not_a_prefix() {
        // Arrange
        let parties = parties();

        // Act
        let matches = matching_parties(&parties, "alpha");

        // Assert
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["party_id"], "alpha::1220aa");
    }

    fn party(threshold: i32, confirmation_threshold: Option<i32>) -> ResolvedParty {
        ResolvedParty {
            id: "alpha::1220aa".to_owned(),
            threshold: Some(threshold),
            confirmation_threshold,
            participants: Vec::new(),
        }
    }

    #[test]
    fn equal_thresholds_stay_equal() {
        assert_eq!(
            new_confirmation_threshold(&party(2, Some(2)), 3, None),
            None
        );
        assert_eq!(new_confirmation_threshold(&party(2, None), 3, None), None);
    }

    #[test]
    fn a_differing_confirmation_threshold_is_kept() {
        assert_eq!(
            new_confirmation_threshold(&party(2, Some(1)), 3, None),
            Some(1)
        );
        assert_eq!(
            new_confirmation_threshold(&party(2, Some(1)), 1, None),
            None
        );
    }

    #[test]
    fn the_option_overrides_the_confirmation_threshold() {
        assert_eq!(
            new_confirmation_threshold(&party(2, Some(1)), 3, Some(2)),
            Some(2)
        );
        assert_eq!(
            new_confirmation_threshold(&party(2, Some(1)), 3, Some(3)),
            None
        );
    }

    #[test]
    fn full_party_id_matches_exactly() {
        let parties = parties();
        let matches = matching_parties(&parties, "gamma::1220dd");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["party_id"], "gamma::1220dd");
    }

    #[test]
    fn ambiguous_party_name_matches_every_party_with_it() {
        assert_eq!(matching_parties(&parties(), "gamma").len(), 2);
    }

    #[test]
    fn mesh_hint_replaces_a_422_onboarding_error() {
        // Arrange
        let error: anyhow::Error = ApiError {
            method: Method::POST,
            path: "/onboarding".to_owned(),
            status: StatusCode::UNPROCESSABLE_ENTITY,
            body: r#"{"error":"Peers are not meshed.","missing_edges":[]}"#.to_owned(),
        }
        .into();

        // Act
        let message = with_mesh_hint(error).to_string();

        // Assert
        assert!(message.contains("Peers are not meshed."), "{message}");
        assert!(!message.contains("returned"), "{message}");
    }

    #[test]
    fn other_errors_keep_their_message() {
        let error = with_mesh_hint(anyhow::anyhow!("boom"));
        assert_eq!(error.to_string(), "boom");
    }
}
