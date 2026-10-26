//! Waiting for a workflow run to end, by polling `GET /workflows`.
//!
//! The server has no push channel. `/workflows` lists in-progress runs and
//! ended runs that nobody has dismissed yet, so a run that stays missing was
//! most likely dismissed by someone else.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use common::api::WorkflowRunsResponse;
use common::types::{WorkflowProgress, WorkflowRole, WorkflowRun};
use reqwest::Method;

use super::Outcome;
use crate::api::DecmanClient;

/// Polls in a row that may miss the run before the wait stops.
const MAX_MISSES: u32 = 5;

/// Polls in a row that may fail, for example on a network error, before the
/// wait stops. The run goes on at the node while the CLI retries.
const MAX_POLL_ERRORS: u32 = 3;

/// How to find the run to wait for.
#[derive(Clone, Debug)]
pub enum RunSelector {
    /// A run this node started or already knows by name.
    Instance(String),
    /// The peer-side run this node starts when it accepts an invitation.
    /// The accept call does not return its name, so the run is found by the
    /// coordinator's run name and public key.
    PeerOf {
        coordinator_instance: String,
        coordinator_pubkey: String,
    },
}

/// Polling settings for a wait.
#[derive(Clone, Copy, Debug)]
pub struct WaitOptions {
    pub timeout: Option<Duration>,
    pub interval: Duration,
}

/// Poll until the selected run ends. The outcome holds the run as JSON and
/// succeeds only if the run completed.
///
/// # Errors
///
/// Returns an error if a poll fails, the run stays missing, or the timeout
/// passes first.
pub fn wait_for_run(
    client: &mut DecmanClient,
    selector: &RunSelector,
    options: WaitOptions,
) -> Result<Outcome> {
    let deadline = options.timeout.map(|timeout| Instant::now() + timeout);
    let timed_out = || {
        anyhow::anyhow!(
            "timed out after {}s waiting for {}",
            options.timeout.unwrap_or_default().as_secs(),
            describe_selector(selector)
        )
    };
    // Sleep for one interval, but never past the deadline. Fails once the
    // deadline has passed.
    let pause = || -> Result<()> {
        let nap = match deadline {
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(timed_out());
                }
                left.min(options.interval)
            }
            None => options.interval,
        };
        std::thread::sleep(nap);
        Ok(())
    };
    let mut misses = 0;
    let mut errors = 0;
    let mut last_progress = String::new();
    loop {
        let response = match client.request(Method::GET, "/workflows", &[], None) {
            Ok(response) => {
                errors = 0;
                response
            }
            Err(error) => {
                errors += 1;
                if errors >= MAX_POLL_ERRORS {
                    return Err(error.context(format!(
                        "polling failed {MAX_POLL_ERRORS} times in a row; the run goes on at \
                         the node, so check it with workflows get"
                    )));
                }
                eprintln!(
                    "poll failed, retrying: {}",
                    super::for_terminal(&format!("{error:#}"))
                );
                pause()?;
                continue;
            }
        };
        let runs: WorkflowRunsResponse =
            serde_json::from_value(response).context("failed to parse the /workflows response")?;

        match find_run(&runs.runs, selector) {
            Some(run) => {
                misses = 0;
                let progress = describe(run);
                if progress != last_progress {
                    eprintln!("{}", super::for_terminal(&progress));
                    last_progress = progress;
                }
                if is_terminal(run.status) {
                    return Ok(Outcome {
                        value: serde_json::to_value(run)?,
                        success: run.status == WorkflowProgress::Completed,
                    });
                }
            }
            None => {
                misses += 1;
                if misses >= MAX_MISSES {
                    bail!(
                        "{} is not in /workflows; someone possibly dismissed it",
                        describe_selector(selector)
                    );
                }
            }
        }

        pause()?;
    }
}

/// The run `selector` names. The server keeps one peer row per coordinator
/// run, so a peer selector matches at most one run; the newest one wins in
/// case an older server kept more.
fn find_run<'a>(runs: &'a [WorkflowRun], selector: &RunSelector) -> Option<&'a WorkflowRun> {
    match selector {
        RunSelector::Instance(name) => runs.iter().find(|run| &run.instance_name == name),
        RunSelector::PeerOf {
            coordinator_instance,
            coordinator_pubkey,
        } => runs
            .iter()
            .filter(|run| {
                run.role == WorkflowRole::Peer
                    && run.coordinator_instance.as_deref() == Some(coordinator_instance)
                    && run.coordinator_pubkey.as_deref() == Some(coordinator_pubkey)
            })
            .max_by_key(|run| run.created_at),
    }
}

fn is_terminal(status: WorkflowProgress) -> bool {
    matches!(
        status,
        WorkflowProgress::Completed | WorkflowProgress::Failed | WorkflowProgress::Cancelled
    )
}

/// One progress line for stderr.
fn describe(run: &WorkflowRun) -> String {
    let mut line = format!(
        "{name}: {status}, step {index}/{total} {step}",
        name = run.instance_name,
        status = run.status.as_str(),
        index = run.step_index,
        total = run.step_total,
        step = run.current_step,
    );
    if !run.expected_peers.is_empty() {
        line.push_str(&format!(
            ", peers done {done}/{expected}",
            done = run.completed_peers.len(),
            expected = run.expected_peers.len(),
        ));
    }
    if let Some(error) = &run.error {
        line.push_str(&format!(", error: {error}"));
    }
    line
}

fn describe_selector(selector: &RunSelector) -> String {
    match selector {
        RunSelector::Instance(name) => format!("workflow run {name}"),
        RunSelector::PeerOf {
            coordinator_instance,
            ..
        } => format!("the peer run for coordinator run {coordinator_instance}"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn run(name: &str, role: &str, coordinator: Option<&str>, created_at: i64) -> WorkflowRun {
        serde_json::from_value(json!({
            "instance_name": name,
            "kind": "Dars",
            "role": role,
            "status": "inprogress",
            "current_step": "Upload",
            "step_index": 1,
            "step_total": 3,
            "coordinator_instance": coordinator,
            "coordinator_pubkey": coordinator.map(|_| "pk"),
            "expected_peers": [],
            "completed_peers": [],
            "dismissed": false,
            "created_at": created_at,
            "updated_at": created_at,
        }))
        .unwrap()
    }

    #[test]
    fn instance_selector_matches_by_name() {
        // Arrange
        let runs = vec![
            run("a", "Coordinator", None, 1),
            run("b", "Coordinator", None, 2),
        ];

        // Act
        let found = find_run(&runs, &RunSelector::Instance("b".to_owned()));

        // Assert
        assert_eq!(found.unwrap().instance_name, "b");
    }

    #[test]
    fn peer_selector_takes_the_newest_matching_peer_run() {
        // Arrange
        let runs = vec![
            run("old", "Peer", Some("coord-1"), 1),
            run("new", "Peer", Some("coord-1"), 5),
            run("other", "Peer", Some("coord-2"), 9),
            run("mine", "Coordinator", Some("coord-1"), 9),
        ];
        let selector = RunSelector::PeerOf {
            coordinator_instance: "coord-1".to_owned(),
            coordinator_pubkey: "pk".to_owned(),
        };

        // Act
        let found = find_run(&runs, &selector);

        // Assert
        assert_eq!(found.unwrap().instance_name, "new");
    }

    #[test]
    fn only_completed_failed_and_cancelled_end_a_wait() {
        assert!(is_terminal(WorkflowProgress::Completed));
        assert!(is_terminal(WorkflowProgress::Failed));
        assert!(is_terminal(WorkflowProgress::Cancelled));
        assert!(!is_terminal(WorkflowProgress::InProgress));
        assert!(!is_terminal(WorkflowProgress::Idle));
    }

    #[test]
    fn progress_line_shows_step_peers_and_error() {
        // Arrange
        let mut run = run("dars-1", "Coordinator", None, 1);
        run.expected_peers = vec![];
        run.error = Some("boom".to_owned());

        // Act
        let line = describe(&run);

        // Assert
        assert_eq!(line, "dars-1: inprogress, step 1/3 Upload, error: boom");
    }
}
