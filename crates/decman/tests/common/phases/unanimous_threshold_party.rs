//! Regression (#261): a decentralized party whose threshold equals its owner
//! count — every owner must sign.
//!
//! A mainnet 2-node Create Party stalled at `P2P did not appear in topology
//! after 30 attempts`. The `PartyToParticipant` sat on the synchronizer as a
//! pending proposal, signed by both hosting participants, both party signing
//! keys, but only **one** of the two owner-namespace keys — the coordinator's
//! was missing. `CreateProposals` authorizes the P2P in the coordinator's
//! Authorized store while the decentralized namespace is still an
//! unauthorized proposal there, so Canton's key auto-selection cannot chain
//! the party's namespace and leaves that signature off. Peers sign later,
//! against the synchronizer store, after the DNS is active — so only the
//! coordinator is short.
//!
//! Every other onboarding phase uses the default majority threshold
//! (`ceil(owners / 2)`), where the peers' namespace signatures alone clear the
//! bar and the missing coordinator signature is invisible. This phase pins the
//! threshold to the owner count, which is what the mainnet operator chose, so
//! the aggregate is one signature short and the proposal can never activate.
//! Retry re-submits the identical bytes forever.
//!
//! Pre-fix this phase fails the onboarding run with the topology timeout;
//! post-fix the coordinator re-signs against the synchronizer store once the
//! DNS is active and the party is created.

use std::time::Duration;

use anyhow::Context;
use common::{api::DecentralizedPartiesResponse, types::InvitationType};
use serde_json::{Value, json};
use tracing::info;

use crate::common::{
    Fixture,
    chaos::fresh_prefix,
    http::probe_workflow_status,
    invitations::{InvitationIds, post_accept_invitation, probe_pending_invitation},
    scenario::Scenario,
};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: unanimous_threshold_party");
    let prefix = fresh_prefix("unanimous");
    info!("Using prefix: {prefix}");

    Scenario::with_ctx(
        format!("create decentralized party {prefix} with a unanimous threshold"),
        InvitationIds::default(),
    )
    .when("P1 posts /onboarding inviting P2 with threshold 2 of 2", {
        let prefix = prefix.clone();
        move |f, _| {
            let prefix = prefix.clone();
            Box::pin(async move {
                // Two owners (P1 + P2) and an explicit threshold of 2: every
                // owner namespace key must sign the P2P, including the
                // coordinator's own.
                let req = json!({
                    "party_id_prefix": prefix,
                    "peer_ids": [&f.p2.participant_id],
                    "threshold": 2,
                });
                let _: Value = f.post_json(f.p1.http, "/onboarding", &req).await?;
                Ok(())
            })
        }
    })
    .then(
        "Onboarding invitation visible on P2",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let id = probe_pending_invitation(f, f.p2.http, InvitationType::Onboarding).await?;
                ctx.p2 = Some(id);
                Some(Ok(()))
            })
        },
    )
    .when("P2 accepts the Onboarding invitation", |f, ctx| {
        Box::pin(async move {
            let p2_id = ctx
                .p2
                .as_deref()
                .context("P2 invitation id not set")?
                .to_string();
            post_accept_invitation(f, f.p2.http, &p2_id)
                .await
                .context("accept on P2")
        })
    })
    .then(
        "onboarding workflow reaches completed",
        Duration::from_secs(240),
        |f, _| {
            Box::pin(async move {
                probe_workflow_status(&*f, f.p1.http, "/onboarding/status", "onboarding").await
            })
        },
    )
    .then(
        "party is visible with threshold 2 over 2 participants",
        Duration::from_secs(30),
        {
            let prefix = prefix.clone();
            move |f, _| {
                let prefix = prefix.clone();
                Box::pin(async move {
                    let r: DecentralizedPartiesResponse = f
                        .probe_get_json(f.p1.http, "/decentralized-parties")
                        .await?;
                    let party = r
                        .parties
                        .into_iter()
                        .find(|p| p.party_id.prefix == prefix)?;
                    // Guards the regression against a vacuous pass: if the
                    // requested threshold were ever dropped on the way to the
                    // topology, the party would come back as the 1-of-2
                    // majority default and would never have exercised the bug.
                    if party.threshold != 2 {
                        return Some(Err(anyhow::anyhow!(
                            "expected threshold 2, got {threshold}",
                            threshold = party.threshold
                        )));
                    }
                    if party.participants.len() != 2 {
                        return Some(Err(anyhow::anyhow!(
                            "expected 2 hosting participants, got {count}",
                            count = party.participants.len()
                        )));
                    }
                    Some(Ok(()))
                })
            }
        },
    )
    .run(f)
    .await
}
