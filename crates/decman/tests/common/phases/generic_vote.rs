use serde_json::json;
use tracing::info;

use crate::common::{Fixture, governance::propose_confirm_execute};

const VOTE_DESCRIPTION: &str = "We should switch to dark theme for our website";

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: generic_vote");

    propose_confirm_execute(
        "GenericVote",
        json!({"type": "generic_vote", "description": VOTE_DESCRIPTION}),
    )
    .run(f)
    .await
}
