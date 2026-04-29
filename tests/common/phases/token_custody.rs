use serde_json::json;
use tracing::info;

use crate::common::{Fixture, governance::propose_confirm_execute};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: token_custody");

    let p1_member = f.p1_member_party()?.to_string();

    propose_confirm_execute(
        "SetupCcPreapproval",
        json!({
            "type": "setup_cc_preapproval",
            "provider": p1_member,
            "expected_dso": p1_member,
        }),
    )
    .run(f)
    .await
}
