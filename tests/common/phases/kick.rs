use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture, http::poll_workflow_status, invitations::accept_invitation, scenario::Scenario,
    types::DecentralizedPartiesResponse,
};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: kick");

    Scenario::new("kick participant-3")
        .given("party + member parties present", |f, _| {
            Box::pin(async move {
                f.party_id()?;
                f.party_prefix()?;
                Ok(())
            })
        })
        .given_eventually(
            "P3 owner_key resolved via Noise",
            Duration::from_secs(60),
            |f, _| {
                Box::pin(async move {
                    let prefix = f.party_prefix().ok()?.to_string();
                    let p3_uid = f.p3.participant_id.clone();
                    let path = format!("/decentralized-parties?prefix={prefix}");
                    let r: DecentralizedPartiesResponse =
                        f.get_json(f.p1.http, &path).await.ok()?;
                    let party = r
                        .parties
                        .into_iter()
                        .find(|p| p.party_id.starts_with(&prefix))?;
                    let pi = party
                        .participants
                        .into_iter()
                        .find(|p| p.participant_uid == p3_uid)?;
                    pi.owner_key.map(|_| Ok(()))
                })
            },
        )
        .when("P1 starts kick + P2 accepts invitation", |f, _| {
            Box::pin(async move {
                let party_id = f.party_id()?.to_string();
                let prefix = f.party_prefix()?.to_string();
                let p3_uid = f.p3.participant_id.clone();

                let path = format!("/decentralized-parties?prefix={prefix}");
                let r: DecentralizedPartiesResponse = f.get_json(f.p1.http, &path).await?;
                let party = r
                    .parties
                    .into_iter()
                    .find(|p| p.party_id == party_id)
                    .context("party not found before kick")?;
                let p3 = party
                    .participants
                    .into_iter()
                    .find(|p| p.participant_uid == p3_uid)
                    .context("P3 not in party")?;
                let owner_key = p3.owner_key.context("P3 owner_key not resolved")?;

                let req = json!({
                    "decentralized_party_id": party_id,
                    "participant_id": p3_uid,
                    "namespace_fingerprint": owner_key,
                    "new_threshold": 2_i64,
                });
                let _: Value = f
                    .post_json(f.p1.http, "/kick", &req)
                    .await
                    .context("POST /kick")?;
                accept_invitation(&*f, f.p2.http, "participant-2", "Kick")
                    .await
                    .context("accept Kick on P2")?;
                poll_workflow_status(&*f, f.p1.http, "/kick/status", "kick").await
            })
        })
        .run(f)
        .await
}
