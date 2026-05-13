use anyhow::Context;
use serde_json::json;
use tracing::info;

use crate::common::{Fixture, governance::propose_confirm_execute};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: token_custody");

    let p1_member = f.p1_member_party()?.to_string();
    let expected_dso = match f.target {
        crate::common::TestTarget::Localnet => p1_member.clone(),
        crate::common::TestTarget::Devnet => f
            .dso_party
            .clone()
            .context("dso_party not set on devnet — discover_network_parties must run first")?,
    };

    propose_confirm_execute(
        "SetupCcPreapproval",
        json!({
            "type": "setup_cc_preapproval",
            "provider": p1_member,
            "expected_dso": expected_dso,
        }),
    )
    .run(f)
    .await
}
