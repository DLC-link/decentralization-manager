//! #448: a party's topology proposals reach the synchronizer only at submit.
//!
//! Runs from `change_threshold` while the shared party sits at threshold 1.
//! At that threshold the coordinator's signature alone authorizes a namespace
//! change, so it is where a proposal published early would take effect.
//!
//! 1. The coordinator's signing step, called directly, pins head + 1 for both
//!    mappings and publishes nothing.
//! 2. After the P2P serial moves, submitting that signed pair is refused
//!    before the namespace change goes out, which would otherwise apply alone.
//! 3. A real change-threshold run raises 1 to 2 while P2 joins and then never
//!    signs. The namespace and the participant mapping both stay as they were,
//!    before and after the run is cancelled.
//!
//! Each step also checks that P1's participant holds no temporary topology
//! store afterwards. The party leaves at threshold 1, as it came.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{
        TopologyMapping, TopologyTransaction, enums::TopologyChangeOp, topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, ListAvailableStoresRequest, authorize_request, store_id,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use common::{api::WorkflowRunsResponse, canton_id::CantonId, types::WorkflowRun};
use dec_party_manager::{
    config::NodeConfig,
    utils,
    workflow::{
        proposal_store::{self, PartyHead},
        topology,
    },
};
use serde_json::json;
use tokio::time::sleep;
use tracing::info;

use super::legacy_key_retirement::configs;
use crate::common::{Fixture, chaos, db, invitations::post_accept_invitation, processes};

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: no_early_publish (#448)");
    let config = configs(f)?.into_iter().next().context("no config for P1")?;
    let party: CantonId = f.party_id()?.parse()?;
    let sync = utils::get_synchronizer_id(&config).await?;

    signing_publishes_nothing_and_a_moved_serial_is_refused(&config, &sync, &party).await?;
    a_member_that_never_signs_changes_nothing(f, &config, &sync, &party).await
}

/// Steps 1 and 2: the signing step and the submit check, without a workflow
/// around them.
async fn signing_publishes_nothing_and_a_moved_serial_is_refused(
    config: &NodeConfig,
    sync: &str,
    party: &CantonId,
) -> anyhow::Result<()> {
    let before = proposal_store::fetch_party_head(config, sync, party).await?;
    anyhow::ensure!(
        before.dns.threshold == 1,
        "this phase needs the party at threshold 1, found {}",
        before.dns.threshold
    );

    chaos::say("448", "P1 signs a 1 -> 2 raise in a temporary store");
    let mut new_dns = before.dns.clone();
    new_dns.threshold = 2;
    let mut new_p2p = before.p2p.clone();
    new_p2p.threshold = 2;
    if let Some(keys) = new_p2p.party_signing_keys.as_mut() {
        keys.threshold = 2;
    }
    let instance = format!(
        "it-no-early-publish-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default()
    );
    let signed = proposal_store::sign_party_proposals(
        config,
        sync,
        &instance,
        party,
        &before,
        Some(new_dns),
        new_p2p,
        topology::party_proposal_force_flags(),
    )
    .await
    .context("sign the raise in a temporary store")?;
    let dns = signed.dns.context("no DNS proposal was signed")?;
    let p2p = signed.p2p;

    let dns_serial = utils::decode_versioned::<TopologyTransaction>(&dns.transaction)?.serial;
    let p2p_serial = utils::decode_versioned::<TopologyTransaction>(&p2p.transaction)?.serial;
    anyhow::ensure!(
        (dns_serial, p2p_serial) == (before.dns_serial + 1, before.p2p_serial + 1),
        "proposals pinned to DNS {dns_serial} / P2P {p2p_serial}, expected {} / {}",
        before.dns_serial + 1,
        before.p2p_serial + 1
    );
    // P1 alone authorizes this DNS at threshold 1, so a proposal published
    // now would be in force within a few seconds.
    holds_still(config, sync, party, &before, "after signing").await?;
    no_stores_left(config, "after signing").await?;

    chaos::say(
        "448",
        "moving the P2P serial under the signed pair; the namespace stays",
    );
    let moved = move_p2p_serial(config, sync, party, &before).await?;
    anyhow::ensure!(
        moved.dns_serial == before.dns_serial && moved.p2p_serial == before.p2p_serial + 2,
        "expected only the P2P serial to move by 2, got DNS {} / P2P {}",
        moved.dns_serial,
        moved.p2p_serial
    );

    chaos::say("448", "submitting the stale pair must publish nothing");
    let result = topology::submit_dns_then_p2p(
        config,
        sync,
        "448 regression",
        topology::DnsP2pSubmission {
            dns,
            p2p,
            force_changes: topology::party_proposal_force_flags(),
        },
        || async { anyhow::Ok(()) },
        || async { anyhow::Ok(()) },
    )
    .await;
    let error = match result {
        Ok(()) => anyhow::bail!("a pair whose P2P serial moved was submitted"),
        Err(e) => format!("{e:#}"),
    };
    anyhow::ensure!(
        error.contains("The P2P proposal is at serial"),
        "the submit failed, but not on the serial check: {error}"
    );
    // Without the check the DNS goes out first and P1 alone puts it in force.
    holds_still(config, sync, party, &moved, "after the refused submit").await
}

/// Step 3: the whole workflow, with P2 joining the run and never signing.
async fn a_member_that_never_signs_changes_nothing(
    f: &mut Fixture,
    config: &NodeConfig,
    sync: &str,
    party: &CantonId,
) -> anyhow::Result<()> {
    let before = proposal_store::fetch_party_head(config, sync, party).await?;
    anyhow::ensure!(
        before.dns.threshold == 1,
        "this phase needs the party at threshold 1, found {}",
        before.dns.threshold
    );

    chaos::say(
        "448",
        "P1 raises the threshold 1 -> 2; P2 joins and never signs",
    );
    let instance = chaos::start_workflow_on(
        f,
        f.p1.http,
        "/change-threshold",
        &json!({
            "decentralized_party_id": party.to_string(),
            "new_threshold": 2,
            "previous_threshold": 1,
        }),
    )
    .await?;

    // P2 joins first. A paused node keeps its place among the joined peers,
    // so P3's join then starts the run while P2 can no longer sign.
    let p2_invite =
        chaos::wait_for_invite_for_instance(f, f.p2.http, &instance, Duration::from_secs(60))
            .await?;
    post_accept_invitation(f, f.p2.http, &p2_invite)
        .await
        .context("accept on P2")?;
    let p2_id = f.p2.participant_id.clone();
    chaos::poll_until(Duration::from_secs(60), || async {
        Ok(p1_run(f, &instance)
            .await?
            .is_some_and(|run| run.connected_peers.iter().any(|p| p.to_string() == p2_id)))
    })
    .await
    .context("P2 never joined the run")?;
    processes::pause_node(f, 2).await?;

    let outcome = async {
        let p3_invite =
            chaos::wait_for_invite_for_instance(f, f.p3.http, &instance, Duration::from_secs(60))
                .await?;
        post_accept_invitation(f, f.p3.http, &p3_invite)
            .await
            .context("accept on P3")?;
        let p3_id = f.p3.participant_id.clone();
        // The proposals exist once the run waits on signatures and P3 has
        // given its own.
        chaos::poll_until(Duration::from_secs(120), || async {
            Ok(p1_run(f, &instance).await?.is_some_and(|run| {
                run.current_step == "SignProposals"
                    && run.completed_peers.iter().any(|p| p.to_string() == p3_id)
            }))
        })
        .await
        .context("the run never reached SignProposals with P3's signature")?;
        holds_still(config, sync, party, &before, "while P2 has not signed").await?;
        no_stores_left(config, "while P2 has not signed").await
    }
    .await;

    // Cancel while P2 is still paused, so it never gets to sign. The cancel
    // call itself waits on telling P2, which cannot answer while paused, so
    // P2 resumes as soon as P1 has stopped the run.
    let f = &*f;
    let p1_db = f.db_path(1);
    let cancel_path = format!("/workflows/{instance}/cancel");
    let no_body = json!({});
    let (cancel, (stopped, resumed)) = tokio::join!(
        f.post_expect_status(f.p1.http, &cancel_path, &no_body),
        async {
            let stopped = chaos::poll_until(Duration::from_secs(60), || async {
                Ok(db::workflow_run_status(&p1_db, &instance, "Coordinator")
                    .await?
                    .as_deref()
                    == Some("cancelled"))
            })
            .await;
            (stopped, processes::resume_node(f, 2).await)
        }
    );
    resumed?;
    outcome?;
    let (status, body) = cancel?;
    anyhow::ensure!(status.is_success(), "cancel returned {status}: {body}");
    stopped.context("P1 never recorded the run as cancelled")?;

    holds_still(config, sync, party, &before, "after the cancel").await?;
    no_stores_left(config, "after the cancel").await?;
    clean_up(f, &instance).await
}

/// Leave the mesh as the next phase expects it: P2 reachable again and no
/// run of this phase in anyone's feed.
async fn clean_up(f: &Fixture, instance: &str) -> anyhow::Result<()> {
    let p2_id = f.p2.participant_id.clone();
    chaos::poll_until(Duration::from_secs(120), || async {
        let status: serde_json::Value = f.get_json(f.p1.http, "/participants-status").await?;
        Ok(status["statuses"].as_array().is_some_and(|statuses| {
            statuses
                .iter()
                .any(|s| s["id"] == p2_id.as_str() && s["status"] == "Connected")
        }))
    })
    .await
    .context("P1 never saw P2 again after it resumed")?;

    for (node, port) in [(2u8, f.p2.http), (3, f.p3.http)] {
        let db_path = f.db_path(node);
        // A peer gives up on a cancelled run after a few polls.
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            match db::peer_run_for(&db_path, instance).await? {
                None => break,
                Some((peer_instance, status)) if status != "inprogress" => {
                    chaos::dismiss_on(f, port, &peer_instance).await;
                    break;
                }
                Some(_) if Instant::now() >= deadline => {
                    anyhow::bail!("P{node}'s run for {instance} never stopped")
                }
                Some(_) => sleep(Duration::from_secs(1)).await,
            }
        }
    }
    chaos::dismiss_p1(f, instance).await;
    Ok(())
}

/// The coordinator row for `instance` in P1's feed.
async fn p1_run(f: &Fixture, instance: &str) -> anyhow::Result<Option<WorkflowRun>> {
    let runs: WorkflowRunsResponse = f.get_json(f.p1.http, "/workflows").await?;
    Ok(runs
        .runs
        .into_iter()
        .find(|run| run.instance_name == instance))
}

/// Fail as soon as the party's head differs from `expected`, checking once a
/// second for long enough that a published proposal would have taken effect.
async fn holds_still(
    config: &NodeConfig,
    sync: &str,
    party: &CantonId,
    expected: &PartyHead,
    when: &str,
) -> anyhow::Result<()> {
    const HOLD: Duration = Duration::from_secs(10);
    let start = Instant::now();
    loop {
        let now = proposal_store::fetch_party_head(config, sync, party).await?;
        anyhow::ensure!(
            now.dns_serial == expected.dns_serial
                && now.dns.threshold == expected.dns.threshold
                && now.p2p_serial == expected.p2p_serial
                && now.p2p == expected.p2p,
            "{when}: the party changed. DNS serial {} threshold {} (expected {} / {}), P2P \
             serial {} threshold {} (expected {} / {})",
            now.dns_serial,
            now.dns.threshold,
            expected.dns_serial,
            expected.dns.threshold,
            now.p2p_serial,
            now.p2p.threshold,
            expected.p2p_serial,
            expected.p2p.threshold,
        );
        if start.elapsed() >= HOLD {
            return Ok(());
        }
        sleep(Duration::from_secs(1)).await;
    }
}

/// The temporary stores this tool created that P1's participant still holds.
async fn proposal_stores(config: &NodeConfig) -> anyhow::Result<Vec<String>> {
    let response = TopologyManagerReadServiceClient::new(config.admin_channel().await?)
        .list_available_stores(ListAvailableStoresRequest {})
        .await?
        .into_inner();
    Ok(response
        .store_ids
        .into_iter()
        .filter_map(|id| match id.store {
            Some(store_id::Store::Temporary(store))
                if store.name.starts_with(proposal_store::STORE_PREFIX) =>
            {
                Some(store.name)
            }
            _ => None,
        })
        .collect())
}

async fn no_stores_left(config: &NodeConfig, when: &str) -> anyhow::Result<()> {
    // A cancelled run's store is dropped by a task the guard spawns, so give
    // it a moment rather than reading the list once.
    let result = chaos::poll_until(Duration::from_secs(30), || async {
        Ok(proposal_stores(config).await?.is_empty())
    })
    .await;
    if result.is_err() {
        anyhow::bail!(
            "{when}: P1's participant still holds temporary stores {:?}",
            proposal_stores(config).await?
        );
    }
    Ok(())
}

/// Move the P2P serial by two without touching the namespace: raise the P2P
/// confirmation threshold, then put it back. At namespace threshold 1, P1's
/// signature alone authorizes both.
async fn move_p2p_serial(
    config: &NodeConfig,
    sync: &str,
    party: &CantonId,
    before: &PartyHead,
) -> anyhow::Result<PartyHead> {
    let mut raised = before.p2p.clone();
    raised.threshold = 2;
    for (serial, mapping) in [
        (before.p2p_serial + 1, raised),
        (before.p2p_serial + 2, before.p2p.clone()),
    ] {
        topology::authorize_with_topology_retry(
            config,
            AuthorizeRequest {
                r#type: Some(authorize_request::Type::Proposal(
                    authorize_request::Proposal {
                        change: TopologyChangeOp::AddReplace as i32,
                        serial,
                        mapping: Some(authorize_request::proposal::Mapping::V30(TopologyMapping {
                            mapping: Some(topology_mapping::Mapping::PartyToParticipant(mapping)),
                        })),
                    },
                )),
                must_fully_authorize: true,
                store: Some(topology::synchronizer_store_id(sync)),
                ..Default::default()
            },
            "move the P2P serial",
        )
        .await?;
        chaos::poll_until(Duration::from_secs(60), || async {
            Ok(topology::fetch_p2p_mapping_at_head(config, sync, party)
                .await?
                .0
                == serial)
        })
        .await
        .with_context(|| format!("P2P serial {serial} never took effect"))?;
    }
    proposal_store::fetch_party_head(config, sync, party).await
}
