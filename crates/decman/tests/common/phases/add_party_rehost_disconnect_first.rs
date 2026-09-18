//! Host a former member again while the party keeps transacting, with the
//! target off the synchronizer before the mapping that hosts it is authorized.
//!
//! This is the MainNet incident of 2026-09-16 with the fix in place. There the
//! new host was authorized first and disconnected only at the import. For 70
//! minutes it received the party's traffic with no ACS behind it and journaled
//! every archive of a contract it never held; the next replay of that journal
//! killed the participant. Canton documents the reverse order: the target
//! disconnects, then the party authorizes, then the ACS is imported, then the
//! target reconnects.
//!
//! Setup: P3 hosts the party (the add_party phase put it back). P3 removes its
//! own hosting entry, which a participant may do alone, and stays a namespace
//! owner: the shape MainNet is in. Contracts the party observes are created
//! while P3 is out, so P3 holds none of them.
//!
//! P3 is then added back. From the moment P3 accepts the invitation until the
//! run completes, two watchers run alongside: the seeded contracts are archived
//! one at a time (traffic on the party inside the window), and P3's
//! synchronizer connection plus the party's hosting mapping are sampled every
//! 200 ms.
//!
//! Asserts: the run completes with the namespace serial unchanged; P3 left the
//! synchronizer before the mapping hosting it became effective, with manual
//! connection on while out and off afterwards; archives landed inside the
//! window; P3's view of the party's contracts equals P1's afterwards, with none
//! of the archived contracts resurrected; P3's participant is connected and
//! healthy.

use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::{
    admin::participant::v30::{
        ListConnectedSynchronizersRequest, ListRegisteredSynchronizersRequest,
        synchronizer_connectivity_service_client::SynchronizerConnectivityServiceClient,
    },
    protocol::v30::{TopologyMapping, enums::TopologyChangeOp, topology_mapping},
    topology::admin::v30::{
        AuthorizeRequest, ListDecentralizedNamespaceDefinitionRequest,
        ListPartyToParticipantRequest, authorize_request,
        list_party_to_participant_response::result::Item as P2pItem,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use chrono::Utc;
use common::{
    api::DecentralizedPartiesResponse,
    canton_id::CantonId,
    types::{DecentralizedParty, InvitationType, WorkflowKind, WorkflowProgress, WorkflowRole},
};
use dec_party_manager::{
    config::NodeConfig,
    workflow::topology::{authorize_with_topology_retry, head_state_query, synchronizer_store_id},
};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tracing::info;

use crate::common::{
    Fixture, chaos, db,
    http::{probe_workflow_run_visible, probe_workflow_status},
    invitations::{InvitationIds, post_accept_invitation, probe_pending_invitation},
    ledger_api::{
        P1_JSON_API, P3_JSON_API, REWARD_COUPON_V2_TEMPLATE, SeedCoupon, archive_command,
        reward_coupon_create_command,
    },
    phases::deploy_gov_core::grant_rights,
    scenario::Scenario,
};

/// Contracts created while P3 is out. Archived one by one during the run, so
/// several land inside the disconnect window at localnet speed.
const SEEDED_CONTRACTS: usize = 40;

/// Rounds from here up mark this phase's coupons apart from earlier phases'.
const ROUND_BASE: i64 = 100_000;

/// Gap between two archives while the run is in progress.
const ARCHIVE_INTERVAL: Duration = Duration::from_millis(250);

/// How often P3's connection and the party's hosting mapping are sampled.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(200);

/// The fewest archives that must have landed inside the window for the run to
/// count as a reproduction of the incident's precondition.
const MIN_ARCHIVES_IN_WINDOW: usize = 3;

#[derive(Default)]
struct Ctx {
    invites: InvitationIds,
    dns_serial_before: Option<i32>,
    seeded: Vec<String>,
    watch: Option<Arc<Mutex<Watch>>>,
    stop: Option<Arc<AtomicBool>>,
    tasks: Vec<JoinHandle<()>>,
}

/// What the two background watchers observed.
#[derive(Default)]
struct Watch {
    /// First sample in which P3 had no synchronizer connected.
    disconnected_at: Option<SystemTime>,
    /// First sample after that in which P3 was connected and healthy again.
    reconnected_at: Option<SystemTime>,
    /// `valid_from` of the first head-state mapping that hosts P3 again.
    hosting_valid_from: Option<SystemTime>,
    /// Whether any sample while P3 was out showed manual connection on.
    manual_connect_seen: bool,
    /// Every contract the archiver archived, with the time its submit returned.
    archived: Vec<(String, SystemTime)>,
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: add_party_rehost_disconnect_first");

    Scenario::with_ctx(
        "re-host participant-3 with the party transacting",
        Ctx::default(),
    )
    .given("P3 hosts the party", |f, ctx| {
        Box::pin(async move {
            let party = fetch_party(f, f.p1.http).await?;
            let p3_uid: CantonId = f.p3.participant_id.parse()?;
            anyhow::ensure!(
                party
                    .participants
                    .iter()
                    .any(|p| p.participant_uid == p3_uid),
                "P3 must host the party before it can un-host itself"
            );
            anyhow::ensure!(party.participants.len() == 3, "expected P1 + P2 + P3");
            let p1 = admin_config(f, 1)?;
            ctx.dns_serial_before = Some(namespace_serial(&p1, f.party_id()?).await?);
            Ok(())
        })
    })
    .when("P3 removes its own hosting entry", |f, _| {
        Box::pin(async move {
            let p3 = admin_config(f, 3)?;
            let party_id = f.party_id()?.to_string();
            let synchronizer_id = dec_party_manager::utils::get_synchronizer_id(&p3).await?;
            let (mut mapping, _) = read_hosting(&p3, &synchronizer_id, &party_id)
                .await?
                .context("party has no PartyToParticipant in P3's head state")?;
            mapping
                .participants
                .retain(|p| p.participant_uid != f.p3.participant_id);
            anyhow::ensure!(
                mapping.participants.len() == 2,
                "un-hosting should leave P1 + P2"
            );
            authorize_with_topology_retry(
                &p3,
                AuthorizeRequest {
                    r#type: Some(authorize_request::Type::Proposal(
                        authorize_request::Proposal {
                            change: TopologyChangeOp::AddReplace as i32,
                            serial: 0,
                            mapping: Some(authorize_request::proposal::Mapping::V30(
                                TopologyMapping {
                                    mapping: Some(topology_mapping::Mapping::PartyToParticipant(
                                        mapping,
                                    )),
                                },
                            )),
                        },
                    )),
                    must_fully_authorize: true,
                    force_changes: vec![],
                    signed_by: vec![],
                    store: Some(synchronizer_store_id(&synchronizer_id)),
                    wait_to_become_effective: None,
                },
                "un-host P3",
            )
            .await
            .context("P3 un-hosting itself")?;
            Ok(())
        })
    })
    .then(
        "P1's membership cache no longer lists P3",
        Duration::from_secs(90),
        |f, _| {
            Box::pin(async move {
                let prefix = match f.party_prefix() {
                    Ok(v) => v.to_string(),
                    Err(e) => return Some(Err(e)),
                };
                let party_id = match f.party_id() {
                    Ok(v) => v.to_string(),
                    Err(e) => return Some(Err(e)),
                };
                let path = format!("/decentralized-parties?prefix={prefix}&refresh=true");
                let _: Value = f.probe_get_json(f.p1.http, &path).await?;
                match db::dec_party_cache_has_participant(
                    &f.db_path(1),
                    &party_id,
                    &f.p3.participant_id,
                )
                .await
                {
                    Ok(false) => Some(Ok(())),
                    Ok(true) => None,
                    Err(e) => Some(Err(e)),
                }
            })
        },
    )
    .when(
        "contracts the party observes are created while P3 is out",
        |f, ctx| {
            Box::pin(async move {
                let decparty = f.party_id()?.to_string();
                let dso = f.p1_member_party()?.to_string();
                let expires_at = (Utc::now() + chrono::Duration::hours(36))
                    .format("%Y-%m-%dT%H:%M:%SZ")
                    .to_string();
                let seeds: Vec<SeedCoupon> = (0..SEEDED_CONTRACTS)
                    .map(|i| SeedCoupon {
                        dso: dso.clone(),
                        provider: decparty.clone(),
                        amount: "100.0".to_string(),
                        expires_at: expires_at.clone(),
                        round: ROUND_BASE + i as i64,
                    })
                    .collect();
                for (batch, chunk) in seeds.chunks(20).enumerate() {
                    let cmd = reward_coupon_create_command(
                        chunk,
                        &format!("rehost-seed-{}-{batch}", f.run_id),
                    );
                    f.submit_create(P1_JSON_API, &cmd)
                        .await
                        .with_context(|| format!("seed batch {batch}"))?;
                }
                let seeded: Vec<String> = f
                    .active_coupon_ids(P1_JSON_API, &decparty)
                    .await?
                    .into_iter()
                    .filter(|(_, round)| *round >= ROUND_BASE)
                    .map(|(cid, _)| cid)
                    .collect();
                anyhow::ensure!(
                    seeded.len() == SEEDED_CONTRACTS,
                    "expected {SEEDED_CONTRACTS} seeded coupons visible on P1, found {}",
                    seeded.len()
                );
                ctx.seeded = seeded;
                Ok(())
            })
        },
    )
    .when("P1 posts /add-party for P3", |f, _| {
        Box::pin(async move {
            let req = json!({
                "decentralized_party_id": f.party_id()?.to_string(),
                "new_participant_id": f.p3.participant_id.clone(),
                "new_threshold": 2_i64,
                "previous_threshold": 2_i64,
            });
            let _: Value = f
                .post_json(f.p1.http, "/add-party", &req)
                .await
                .context("POST /add-party")?;
            Ok(())
        })
    })
    .then(
        "AddParty invitation visible on P2",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let id = probe_pending_invitation(f, f.p2.http, InvitationType::AddParty).await?;
                ctx.invites.p2 = Some(id);
                Some(Ok(()))
            })
        },
    )
    .then(
        "AddParty invitation visible on P3",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let id = probe_pending_invitation(f, f.p3.http, InvitationType::AddParty).await?;
                ctx.invites.p3 = Some(id);
                Some(Ok(()))
            })
        },
    )
    .when("P2 accepts", |f, ctx| {
        Box::pin(async move {
            let id = ctx.invites.p2.clone().context("P2 invitation id")?;
            post_accept_invitation(f, f.p2.http, &id).await
        })
    })
    .when("the watchers start, then P3 accepts", |f, ctx| {
        Box::pin(async move {
            let watch = Arc::new(Mutex::new(Watch::default()));
            let stop = Arc::new(AtomicBool::new(false));
            ctx.tasks.push(spawn_sampler(
                admin_config(f, 3)?,
                admin_config(f, 1)?,
                f.party_id()?.to_string(),
                f.p3.participant_id.clone(),
                Arc::clone(&watch),
                Arc::clone(&stop),
            ));
            ctx.tasks.push(spawn_archiver(
                f.client.clone(),
                Arc::clone(&f.refresher),
                f.p1_member_party()?.to_string(),
                ctx.seeded.clone(),
                Arc::clone(&watch),
                Arc::clone(&stop),
            ));
            ctx.watch = Some(watch);
            ctx.stop = Some(stop);

            let id = ctx.invites.p3.clone().context("P3 invitation id")?;
            post_accept_invitation(f, f.p3.http, &id).await
        })
    })
    .then(
        "add-party workflow reaches completed",
        Duration::from_secs(420),
        |f, _| {
            Box::pin(async move {
                probe_workflow_status(&*f, f.p1.http, "/add-party/status", "add-party").await
            })
        },
    )
    .then(
        "AddParty completed run visible on P3",
        Duration::from_secs(30),
        |f, _| {
            Box::pin(async move {
                probe_workflow_run_visible(
                    f,
                    f.p3.http,
                    WorkflowKind::AddParty,
                    WorkflowRole::Peer,
                    WorkflowProgress::Completed,
                )
                .await
            })
        },
    )
    .when("the watchers stop", |_, ctx| {
        Box::pin(async move {
            if let Some(stop) = &ctx.stop {
                stop.store(true, Ordering::SeqCst);
            }
            for task in ctx.tasks.drain(..) {
                task.await.context("watcher task panicked")?;
            }
            Ok(())
        })
    })
    .then(
        "P3 hosts the party again and the namespace serial is unchanged",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let party = match fetch_party(f, f.p1.http).await {
                    Ok(p) => p,
                    Err(e) => return Some(Err(e)),
                };
                let p3_uid: CantonId = match f.p3.participant_id.parse() {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                let hosted = party
                    .participants
                    .iter()
                    .any(|p| p.participant_uid == p3_uid);
                if !hosted || party.participants.len() != 3 || party.threshold != 2 {
                    return None;
                }
                let p1 = match admin_config(f, 1) {
                    Ok(c) => c,
                    Err(e) => return Some(Err(e)),
                };
                let party_id = match f.party_id() {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                let after = match namespace_serial(&p1, party_id).await {
                    Ok(s) => s,
                    Err(e) => return Some(Err(e)),
                };
                let before = ctx.dns_serial_before;
                if Some(after) != before {
                    return Some(Err(anyhow::anyhow!(
                        "the namespace was re-issued: serial {before:?} before, {after} after"
                    )));
                }
                Some(Ok(()))
            })
        },
    )
    .then(
        "P3 left the synchronizer before the mapping hosting it took effect",
        Duration::from_secs(5),
        |_, ctx| {
            Box::pin(async move {
                let watch = ctx.watch.as_ref()?.lock().ok()?;
                let Some(disconnected_at) = watch.disconnected_at else {
                    return Some(Err(anyhow::anyhow!(
                        "P3 was never observed without a synchronizer connection"
                    )));
                };
                let Some(valid_from) = watch.hosting_valid_from else {
                    return Some(Err(anyhow::anyhow!(
                        "no head-state mapping hosting P3 was observed during the run"
                    )));
                };
                if disconnected_at >= valid_from {
                    return Some(Err(anyhow::anyhow!(
                        "P3 disconnected at {disconnected_at:?}, after the mapping became \
                             effective at {valid_from:?}: the target was connected while hosted \
                             without an ACS, which is the incident"
                    )));
                }
                if !watch.manual_connect_seen {
                    return Some(Err(anyhow::anyhow!(
                        "P3 never had manual connection on while it was out"
                    )));
                }
                if watch.reconnected_at.is_none() {
                    return Some(Err(anyhow::anyhow!(
                        "P3 was never observed connected and healthy again"
                    )));
                }
                Some(Ok(()))
            })
        },
    )
    .then(
        "archives landed inside the window",
        Duration::from_secs(5),
        |_, ctx| {
            Box::pin(async move {
                let watch = ctx.watch.as_ref()?.lock().ok()?;
                let (Some(from), Some(until)) = (watch.disconnected_at, watch.reconnected_at)
                else {
                    return Some(Err(anyhow::anyhow!("window bounds not observed")));
                };
                let inside = watch
                    .archived
                    .iter()
                    .filter(|(_, at)| *at >= from && *at <= until)
                    .count();
                if inside < MIN_ARCHIVES_IN_WINDOW {
                    return Some(Err(anyhow::anyhow!(
                        "only {inside} archive(s) landed inside the disconnect window \
                             ({} archived in total); the run did not reproduce the incident's \
                             precondition",
                        watch.archived.len()
                    )));
                }
                info!("{inside} archives landed inside the disconnect window");
                Some(Ok(()))
            })
        },
    )
    .when("P3's ledger user may read as the party", |f, _| {
        Box::pin(async move {
            let party = f.party_id()?.to_string();
            grant_rights(&*f, P3_JSON_API, &party, "participant-3").await
        })
    })
    .then(
        "P3 holds exactly the party's contracts, none of the archived ones",
        Duration::from_secs(120),
        |f, ctx| {
            Box::pin(async move {
                let decparty = match f.party_id() {
                    Ok(v) => v.to_string(),
                    Err(e) => return Some(Err(e)),
                };
                let ids = |rows: Vec<(String, i64)>| {
                    rows.into_iter().map(|(cid, _)| cid).collect::<HashSet<_>>()
                };
                let on_p1 = ids(f.active_coupon_ids(P1_JSON_API, &decparty).await.ok()?);
                let on_p3 = ids(f.active_coupon_ids(P3_JSON_API, &decparty).await.ok()?);
                if on_p1.is_empty() || on_p1 != on_p3 {
                    return None;
                }
                let archived: HashSet<String> = ctx
                    .watch
                    .as_ref()?
                    .lock()
                    .ok()?
                    .archived
                    .iter()
                    .map(|(cid, _)| cid.clone())
                    .collect();
                if let Some(ghost) = archived.iter().find(|cid| on_p3.contains(*cid)) {
                    return Some(Err(anyhow::anyhow!(
                        "archived contract {ghost} is active on P3: the import resurrected it"
                    )));
                }
                Some(Ok(()))
            })
        },
    )
    .then(
        "P3's participant is connected, healthy, on automatic connection",
        Duration::from_secs(30),
        |f, _| {
            Box::pin(async move {
                let p3 = match admin_config(f, 3) {
                    Ok(c) => c,
                    Err(e) => return Some(Err(e)),
                };
                let (connected_healthy, manual) = match connection_state(&p3).await {
                    Ok(v) => v,
                    Err(_) => return None,
                };
                if !connected_healthy {
                    return None;
                }
                if manual {
                    return Some(Err(anyhow::anyhow!(
                        "P3's synchronizer is still on manual connection after the run"
                    )));
                }
                Some(Ok(()))
            })
        },
    )
    .run(f)
    .await
}

/// Admin-API view of participant `n`, the way the harness reaches Canton.
fn admin_config(f: &Fixture, n: u8) -> anyhow::Result<NodeConfig> {
    let (port_var, participant_id) = match n {
        1 => ("P1_CANTON_ADMIN", &f.p1.participant_id),
        3 => ("P3_CANTON_ADMIN", &f.p3.participant_id),
        _ => anyhow::bail!("no admin config for participant {n}"),
    };
    let admin_port: u16 = std::env::var(port_var)
        .with_context(|| format!("{port_var} not set"))?
        .parse()
        .with_context(|| format!("{port_var} is not a port"))?;
    let mut config = NodeConfig::default();
    config.canton.admin_api_host = "127.0.0.1".to_string();
    config.canton.admin_api_port = admin_port;
    config.node.participant_id = Some(CantonId::parse(participant_id)?);
    Ok(config)
}

async fn fetch_party(f: &Fixture, port: u16) -> anyhow::Result<DecentralizedParty> {
    let prefix = f.party_prefix()?.to_string();
    let path = format!("/decentralized-parties?prefix={prefix}&refresh=true");
    let r: DecentralizedPartiesResponse = f.get_json(port, &path).await?;
    r.parties
        .into_iter()
        .find(|p| p.party_id.prefix == prefix)
        .with_context(|| format!("party {prefix} not listed"))
}

/// Serial of the party's namespace definition in the synchronizer head state.
async fn namespace_serial(config: &NodeConfig, party_id: &str) -> anyhow::Result<i32> {
    let synchronizer_id = dec_party_manager::utils::get_synchronizer_id(config).await?;
    let namespace = CantonId::parse(party_id)?.namespace.to_hex();
    let mut client = TopologyManagerReadServiceClient::new(config.admin_channel().await?);
    let response = client
        .list_decentralized_namespace_definition(tonic::Request::new(
            ListDecentralizedNamespaceDefinitionRequest {
                base_query: Some(head_state_query(&synchronizer_id)),
                filter_namespace: namespace,
            },
        ))
        .await?
        .into_inner();
    response
        .results
        .into_iter()
        .find_map(|r| r.context.map(|c| c.serial))
        .context("namespace definition not in head state")
}

/// The party's hosting mapping in `config`'s head state, with the time it
/// became effective.
async fn read_hosting(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &str,
) -> anyhow::Result<
    Option<(
        canton_proto_rs::com::digitalasset::canton::protocol::v30::PartyToParticipant,
        Option<SystemTime>,
    )>,
> {
    let mut client = TopologyManagerReadServiceClient::new(config.admin_channel().await?);
    let response = client
        .list_party_to_participant(tonic::Request::new(ListPartyToParticipantRequest {
            base_query: Some(head_state_query(synchronizer_id)),
            filter_party: party_id.to_string(),
            filter_participant: String::new(),
        }))
        .await?
        .into_inner();
    Ok(response.results.into_iter().find_map(|r| {
        let valid_from = r
            .context
            .as_ref()
            .and_then(|c| c.valid_from)
            .map(to_system_time);
        let P2pItem::V30(mapping) = r.item?;
        (mapping.party == party_id).then_some((mapping, valid_from))
    }))
}

fn to_system_time(ts: prost_types::Timestamp) -> SystemTime {
    let secs = u64::try_from(ts.seconds).unwrap_or_default();
    let nanos = u32::try_from(ts.nanos).unwrap_or_default();
    UNIX_EPOCH + Duration::new(secs, nanos)
}

/// `(connected and healthy, manual_connect)` for the participant's synchronizer.
async fn connection_state(config: &NodeConfig) -> anyhow::Result<(bool, bool)> {
    let mut client = SynchronizerConnectivityServiceClient::new(config.admin_channel().await?);
    let connected = client
        .list_connected_synchronizers(tonic::Request::new(ListConnectedSynchronizersRequest {}))
        .await?
        .into_inner()
        .connected_synchronizers;
    let healthy = connected.iter().any(|s| s.healthy);
    let manual = client
        .list_registered_synchronizers(tonic::Request::new(ListRegisteredSynchronizersRequest {
            all_statuses: false,
        }))
        .await?
        .into_inner()
        .results
        .into_iter()
        .filter_map(|r| r.config)
        .any(|c| c.manual_connect);
    Ok((healthy, manual))
}

/// Sample P3's connection and the party's hosting mapping until told to stop.
fn spawn_sampler(
    p3: NodeConfig,
    p1: NodeConfig,
    party_id: String,
    p3_participant_id: String,
    watch: Arc<Mutex<Watch>>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let synchronizer_id = match dec_party_manager::utils::get_synchronizer_id(&p1).await {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("sampler: synchronizer id: {e}");
                return;
            }
        };
        let mut ticker = tokio::time::interval(SAMPLE_INTERVAL);
        while !stop.load(Ordering::SeqCst) {
            ticker.tick().await;
            let now = SystemTime::now();
            if let Ok((healthy, manual)) = connection_state(&p3).await {
                let mut w = match watch.lock() {
                    Ok(w) => w,
                    Err(_) => return,
                };
                match (w.disconnected_at, w.reconnected_at) {
                    (None, _) if !healthy => w.disconnected_at = Some(now),
                    (Some(_), None) if !healthy && manual => w.manual_connect_seen = true,
                    (Some(_), None) if healthy => w.reconnected_at = Some(now),
                    _ => {}
                }
            }
            let hosted = match read_hosting(&p1, &synchronizer_id, &party_id).await {
                Ok(Some((mapping, valid_from))) => mapping
                    .participants
                    .iter()
                    .any(|p| p.participant_uid == p3_participant_id)
                    .then_some(valid_from)
                    .flatten(),
                _ => None,
            };
            if let Some(valid_from) = hosted
                && let Ok(mut w) = watch.lock()
                && w.hosting_valid_from.is_none()
            {
                w.hosting_valid_from = Some(valid_from);
            }
        }
    })
}

/// Archive the seeded contracts one at a time, through P1's JSON Ledger API as
/// their signatory, until told to stop or none are left.
fn spawn_archiver(
    client: reqwest::Client,
    refresher: Arc<crate::common::auth::Refresher>,
    dso: String,
    contract_ids: Vec<String>,
    watch: Arc<Mutex<Watch>>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        for (i, cid) in contract_ids.into_iter().enumerate() {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let body = archive_command(
                REWARD_COUPON_V2_TEMPLATE,
                &cid,
                &dso,
                &format!("rehost-archive-{i}-{}", chaos::fresh_prefix("t")),
            );
            let token = match refresher.token().await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("archiver: token: {e}");
                    break;
                }
            };
            let sent = client
                .post(format!(
                    "http://localhost:{P1_JSON_API}/v2/commands/submit-and-wait"
                ))
                .header(CONTENT_TYPE, "application/json")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .json(&body)
                .send()
                .await;
            match sent {
                Ok(res) if res.status().is_success() => {
                    if let Ok(mut w) = watch.lock() {
                        w.archived.push((cid, SystemTime::now()));
                    }
                }
                Ok(res) => tracing::warn!("archiver: archive {cid} returned {}", res.status()),
                Err(e) => tracing::warn!("archiver: archive {cid}: {e}"),
            }
            tokio::time::sleep(ARCHIVE_INTERVAL).await;
        }
    })
}
