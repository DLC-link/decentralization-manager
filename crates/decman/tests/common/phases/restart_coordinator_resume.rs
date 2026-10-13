//! G1: Coordinator crash mid-workflow → auto-resume on restart.
//!
//! POST /onboarding on P1, wait for the coordinator row + invites delivered,
//! hard-kill P1, restart, accept on both peers, and assert the run
//! reaches Completed via the persisted DB row (the in-memory
//! `<Kind>WorkflowState` is freshly constructed after restart and lags the
//! DB on slow runners). Invariant: exactly one coordinator row.

use std::time::Duration;

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::topology::admin::v30::{
    CreateTemporaryTopologyStoreRequest, ListAvailableStoresRequest, store_id,
    topology_manager_read_service_client::TopologyManagerReadServiceClient,
    topology_manager_write_service_client::TopologyManagerWriteServiceClient,
};
use common::{api::PendingInvitationsResponse, types::InvitationType};
use dec_party_manager::{config::NodeConfig, utils, workflow::proposal_store};

use super::legacy_key_retirement::configs;
use crate::common::{Fixture, chaos, db, invitations::post_accept_invitation, processes};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    chaos::ensure_nodes_healthy(f).await?;
    let prefix = chaos::fresh_prefix("resume-coord");
    let instance = format!("{prefix}-creation");
    chaos::say("G1", &format!("starting onboarding with prefix {prefix}"));
    chaos::post_onboarding(f, &prefix).await?;

    // Wait for coordinator row inprogress AND for invites to actually reach
    // both peers. The resume path doesn't re-send invites — killing P1
    // before the spawned task's 500ms ListenerPauseGuard + send_*_invites
    // runs leaves peers with no pending invitation.
    let p1_db = f.db_path(1);
    let f_imm = &*f;
    chaos::poll_until(Duration::from_secs(60), || async {
        let n = db::count_workflow_runs_inprogress(&p1_db, "Onboarding", "Coordinator").await?;
        if n < 1 {
            return Ok(false);
        }
        let inv_p2: PendingInvitationsResponse =
            f_imm.get_json(f_imm.p2.http, "/invitations").await?;
        let inv_p3: PendingInvitationsResponse =
            f_imm.get_json(f_imm.p3.http, "/invitations").await?;
        let p2_has = inv_p2
            .invitations
            .iter()
            .any(|i| i.invitation_type == InvitationType::Onboarding);
        let p3_has = inv_p3
            .invitations
            .iter()
            .any(|i| i.invitation_type == InvitationType::Onboarding);
        Ok(p2_has && p3_has)
    })
    .await?;

    // A coordinator killed while it signs a party proposal leaves that
    // proposal's temporary store on its participant (#448). Plant one, so the
    // restart below has to sweep it.
    let leftover = plant_leftover_store(f, &instance).await?;

    chaos::say("G1", "row + invites ready; hard-killing P1");
    processes::restart_node(f, 1).await?;

    chaos::say("G1", &format!("the boot sweep must drop {leftover}"));
    let p1 = configs(f)?.into_iter().next().context("no config for P1")?;
    chaos::poll_until(Duration::from_secs(30), || async {
        Ok(!temporary_stores(&p1).await?.contains(&leftover))
    })
    .await
    .with_context(|| format!("P1 restarted but {leftover} is still on its participant"))?;

    // Now accept on both peers.
    let p2_inv = chaos::wait_for_invite(
        f,
        f.p2.http,
        InvitationType::Onboarding,
        Duration::from_secs(60),
    )
    .await?;
    let p3_inv = chaos::wait_for_invite(
        f,
        f.p3.http,
        InvitationType::Onboarding,
        Duration::from_secs(60),
    )
    .await?;
    post_accept_invitation(f, f.p2.http, &p2_inv).await?;
    post_accept_invitation(f, f.p3.http, &p3_inv).await?;

    chaos::say("G1", "waiting for resumed run to reach completed");
    let p1_db = f.db_path(1);
    // 360s budget: a devnet run on post-#158 tip a1b29f0 exhausted the
    // previous 240s deadline waiting for the workflow_run to flip to
    // `completed` after the P1 restart + both peer Accepts. Localnet
    // completes here in milliseconds, so the budget bump is harmless on
    // localnet but gives the kubectl-tunneled devnet Canton a realistic
    // window for the post-restart resume path. Tracked in #161.
    chaos::poll_until(Duration::from_secs(360), || async {
        Ok(matches!(
            db::workflow_run_status(&p1_db, &instance, "Coordinator")
                .await?
                .as_deref(),
            Some("completed")
        ))
    })
    .await?;

    let row_count = db::count_workflow_run_rows(&p1_db, &instance, "Coordinator").await?;
    anyhow::ensure!(
        row_count == 1,
        "expected exactly 1 coordinator row, got {row_count}"
    );

    chaos::say("G1", "coordinator resume verified (single row, completed)");
    chaos::dismiss_p1(f, &instance).await;
    Ok(())
}

/// Create a temporary store on P1's participant named like one a signing run
/// leaves when its coordinator dies mid-step, and return its name.
async fn plant_leftover_store(f: &Fixture, instance: &str) -> anyhow::Result<String> {
    let p1 = configs(f)?.into_iter().next().context("no config for P1")?;
    let sync = utils::get_synchronizer_id(&p1).await?;
    let protocol_version = sync
        .rsplit_once("::")
        .and_then(|(_, suffix)| suffix.split('-').next())
        .and_then(|version| version.parse().ok())
        .with_context(|| format!("no protocol version in {sync}"))?;
    let name = format!(
        "{prefix}{instance}~leftover",
        prefix = proposal_store::STORE_PREFIX
    );
    TopologyManagerWriteServiceClient::new(p1.admin_channel().await?)
        .create_temporary_topology_store(CreateTemporaryTopologyStoreRequest {
            name: name.clone(),
            protocol_version,
        })
        .await
        .context("plant a leftover temporary store on P1")?;
    anyhow::ensure!(
        temporary_stores(&p1).await?.contains(&name),
        "the planted store {name} is not listed"
    );
    Ok(name)
}

async fn temporary_stores(config: &NodeConfig) -> anyhow::Result<Vec<String>> {
    let response = TopologyManagerReadServiceClient::new(config.admin_channel().await?)
        .list_available_stores(ListAvailableStoresRequest {})
        .await?
        .into_inner();
    Ok(response
        .store_ids
        .into_iter()
        .filter_map(|id| match id.store {
            Some(store_id::Store::Temporary(store)) => Some(store.name),
            _ => None,
        })
        .collect())
}
