use serde_json::json;
use tracing::info;

use crate::common::{Fixture, governance::propose_confirm_execute};

const VOTE_DESCRIPTION: &str = "We should switch to dark theme for our website";

/// A node's `decman_governance_actions_total` series for one step and outcome.
/// A node that has not exported the series yet reads as zero.
async fn action_count(f: &Fixture, port: u16, step: &str, outcome: &str) -> anyhow::Result<f64> {
    let body = f.get_text(port, "/metrics").await?;
    let step = format!("step=\"{step}\"");
    let outcome = format!("outcome=\"{outcome}\"");
    body.lines()
        .filter(|line| line.starts_with("decman_governance_actions_total{"))
        .find(|line| line.contains(&step) && line.contains(&outcome))
        .and_then(|line| line.split_whitespace().last())
        .map_or(Ok(0.0), |value| value.parse().map_err(Into::into))
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: generic_vote");

    // The cycle sends each step to a fixed node: P1 proposes, P2 confirms and
    // P3 executes.
    let steps = [
        (f.p1.metrics, "propose"),
        (f.p2.metrics, "confirm"),
        (f.p3.metrics, "execute"),
    ];
    let mut before = Vec::new();
    for (port, step) in steps {
        before.push(action_count(f, port, step, "ok").await?);
    }

    propose_confirm_execute(
        "GenericVote",
        json!({"type": "generic_vote", "description": VOTE_DESCRIPTION}),
    )
    .run(f)
    .await?;

    // Each handler counts before it answers, so no wait is needed.
    for ((port, step), before) in steps.into_iter().zip(before) {
        let after = action_count(f, port, step, "ok").await?;
        anyhow::ensure!(
            after > before,
            "{step} ok did not rise on metrics port {port}: {before} -> {after}"
        );
    }
    info!("governance action counters verified");
    Ok(())
}
