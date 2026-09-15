//! Per-hop health of this node, for the Config tab's Node card.
//!
//! "Is the node healthy?" has no single answer: the browser has to reach
//! DecMan, DecMan has to reach the participant's two gRPC APIs, and the
//! participant has to stay connected to a synchronizer. Each hop fails
//! differently, so each is probed and reported separately and the overall
//! [`NodeHealthStatus`] is derived from them by [`verdict`].
//!
//! The browser→DecMan hop is *not* measured here: the frontend times its own
//! round-trip to `/healthz` (see `pingLatency`), which is the only way to
//! measure the leg this process sits at the far end of.
//!
//! Probing is server-side and cached ([`HealthCache`]), so a Config tab open in
//! ten browsers costs the same two gRPC calls as one.

use std::{
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        GetLedgerApiVersionRequest, version_service_client::VersionServiceClient,
    },
    digitalasset::canton::admin::{
        health::v30::component_status,
        participant::v30::{
            ParticipantStatusRequest, ParticipantStatusResponse, connected_synchronizer,
            participant_status_response::Kind,
            participant_status_service_client::ParticipantStatusServiceClient,
        },
    },
};
use prometheus::{Gauge, IntGauge};
use serde::Serialize;
use tokio::sync::Mutex;
use tonic::transport::Channel;

use crate::config::NodeConfig;

/// How long a probe snapshot is served before the next caller re-probes.
///
/// Matches the frontend's poll interval, so a single watching tab drives
/// roughly one probe per tick and extra tabs are free.
pub const SNAPSHOT_TTL: Duration = Duration::from_secs(5);

/// Cadence of the background refresh that keeps the Prometheus gauges live
/// while no browser is polling. Deliberately slower than [`SNAPSHOT_TTL`]:
/// its job is alerting freshness, not UI smoothness.
pub const BACKGROUND_REFRESH: Duration = Duration::from_secs(30);

/// Bounds one gRPC probe end to end — channel establishment as well as the
/// RPC. Both probes run concurrently, so this is also the worst-case time the
/// handler can spend before answering.
///
/// It has to cover the connect: `NodeConfig::admin_channel` carries its own
/// 10s connect timeout, so a cold cache (or the slot a failed probe cleared)
/// would otherwise block far past this bound before the RPC timer started.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// A topology queue deeper than this reads as a backlog rather than the
/// handful of transactions ordinarily in flight. A UI heuristic for the
/// Degraded verdict, not a Canton-defined limit.
const TOPOLOGY_QUEUE_BACKLOG: u32 = 100;

/// Overall verdict for the node. See [`verdict`] for how it is derived.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(rename_all = "PascalCase")]
pub enum NodeHealthStatus {
    /// Both APIs answered and the participant reports nothing wrong.
    Healthy,
    /// Both APIs answered, but the participant is initializing, passive, has a
    /// component that is not OK, has a backed-up topology queue, or is not
    /// connected to a healthy synchronizer.
    Degraded,
    /// An API could not be reached at all. Nothing downstream of it is known.
    Down,
}

/// One measured hop from this node to a Canton API.
///
/// `reachable` means the endpoint *answered*: a gRPC error status — including
/// the `UNAUTHENTICATED` an auth-enabled Ledger API returns to a token-free
/// probe — proves the transport and the server process are alive and yields a
/// true round-trip. Only a connection that never reached the server is
/// unreachable.
#[derive(Clone, Debug, Default, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct LinkHealth {
    pub reachable: bool,
    /// Round-trip of the probe RPC in milliseconds. Excludes the channel
    /// connect, which only happens on the first probe after a restart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// Why the probe did not succeed. Present both when the endpoint was
    /// unreachable and when it answered with an error status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl LinkHealth {
    fn unreachable(error: String) -> Self {
        Self {
            reachable: false,
            latency_ms: None,
            error: Some(error),
        }
    }

    fn answered(latency_ms: u64, error: Option<String>) -> Self {
        Self {
            reachable: true,
            latency_ms: Some(latency_ms),
            error,
        }
    }
}

/// Depth of the participant's topology queues, as it reports them.
#[derive(Clone, Copy, Debug, Default, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TopologyQueues {
    pub manager: u32,
    pub dispatcher: u32,
    pub clients: u32,
}

impl TopologyQueues {
    fn is_backed_up(&self) -> bool {
        self.manager.max(self.dispatcher).max(self.clients) > TOPOLOGY_QUEUE_BACKLOG
    }
}

/// Health of one component the participant depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(rename_all = "PascalCase")]
pub enum ComponentState {
    Ok,
    Degraded,
    Failed,
    Fatal,
    /// The participant reported a component with no state set — a shape newer
    /// than this build understands.
    Unknown,
}

/// One entry of the participant's component-health list.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ComponentHealth {
    pub name: String,
    pub state: ComponentState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A synchronizer the participant is connected to, and whether that connection
/// is healthy.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct SynchronizerHealth {
    /// The live physical synchronizer ID. Worth showing: it changes under a
    /// live upgrade, and a stale cached value is what surfaces elsewhere as
    /// `TOPOLOGY_STORE_NOT_FOUND`.
    pub physical_synchronizer_id: String,
    pub healthy: bool,
}

/// What the participant reports about itself over the Admin API.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ParticipantHealth {
    /// `false` while the participant is still waiting for external input. The
    /// other fields are then empty or zero — it has no status to report yet.
    pub initialized: bool,
    pub uid: String,
    pub uptime_seconds: u64,
    /// `false` on the passive replica of a replicated participant.
    pub active: bool,
    /// The participant's own Canton version, not this node's.
    pub version: String,
    pub topology_queues: TopologyQueues,
    pub components: Vec<ComponentHealth>,
}

/// Health of this node and the participant it drives.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct NodeHealthResponse {
    pub status: NodeHealthStatus,
    /// RFC 3339 timestamp of the probe this snapshot came from. The frontend
    /// shows its age, so a stale snapshot cannot pass for a live one.
    pub checked_at: String,
    pub admin_api: LinkHealth,
    pub ledger_api: LinkHealth,
    /// `None` when the Admin API did not answer with a status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participant: Option<ParticipantHealth>,
    pub synchronizers: Vec<SynchronizerHealth>,
    /// State of the startup coordination-DAR upload (design D8). Filled by
    /// the handler, not by the probe, so the cached snapshot carries `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordination_dar: Option<common::coordination::CoordinationDarStatus>,
}

static ADMIN_LATENCY: LazyLock<Gauge> = LazyLock::new(|| {
    prometheus::register_gauge!(
        "decman_canton_admin_api_latency_ms",
        "Round-trip of the last ParticipantStatus probe to the participant's Admin API, in milliseconds. Absent value means the last probe failed."
    )
    .expect("metric name is a unique literal")
});

static LEDGER_LATENCY: LazyLock<Gauge> = LazyLock::new(|| {
    prometheus::register_gauge!(
        "decman_canton_ledger_api_latency_ms",
        "Round-trip of the last GetLedgerApiVersion probe to the participant's Ledger API, in milliseconds. Absent value means the last probe failed."
    )
    .expect("metric name is a unique literal")
});

static HEALTH_STATUS: LazyLock<IntGauge> = LazyLock::new(|| {
    prometheus::register_int_gauge!(
        "decman_node_health_status",
        "Node health verdict: 2 healthy, 1 degraded, 0 down. Alert on < 2."
    )
    .expect("metric name is a unique literal")
});

static PROBED_AT: LazyLock<IntGauge> = LazyLock::new(|| {
    prometheus::register_int_gauge!(
        "decman_node_health_probed_at_seconds",
        "Unix time of the last health probe. Gate alerts on this so a stale snapshot cannot read as healthy."
    )
    .expect("metric name is a unique literal")
});

/// Forces the health gauges to exist before the first probe, so a dashboard can
/// tell a node that has never probed from a missing instrument.
pub(crate) fn register_metrics() {
    LazyLock::force(&ADMIN_LATENCY);
    LazyLock::force(&LEDGER_LATENCY);
    LazyLock::force(&HEALTH_STATUS);
    LazyLock::force(&PROBED_AT);
}

/// Shared snapshot cache and the warm gRPC channels the probes reuse.
///
/// One `Mutex` guards both, which is what makes the probe single-flight: a
/// second caller blocks until the first finishes, then finds a fresh snapshot
/// and returns it rather than dialling Canton itself. This is the fan-out shape
/// that has OOM-looped nodes before, so it is designed out rather than tuned.
#[derive(Clone, Default)]
pub struct HealthCache(Arc<Mutex<CacheInner>>);

#[derive(Default)]
struct CacheInner {
    snapshot: Option<(NodeHealthResponse, Instant)>,
    /// Connected channels, kept so a probe measures the RPC rather than a
    /// fresh TCP + TLS handshake. Cleared whenever a probe fails to reach the
    /// endpoint, so the next attempt redials.
    admin: Option<Channel>,
    ledger: Option<Channel>,
}

impl HealthCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the cached snapshot, probing first if it is older than
    /// [`SNAPSHOT_TTL`].
    pub async fn get(&self, config: &NodeConfig) -> NodeHealthResponse {
        self.refresh_if_older_than(config, SNAPSHOT_TTL).await
    }

    /// Probe unless the snapshot is younger than `max_age`. The background
    /// refresh passes [`BACKGROUND_REFRESH`] so it never duplicates a probe a
    /// watching browser already paid for.
    pub async fn refresh_if_older_than(
        &self,
        config: &NodeConfig,
        max_age: Duration,
    ) -> NodeHealthResponse {
        let mut inner = self.0.lock().await;
        if let Some((snapshot, taken)) = &inner.snapshot
            && taken.elapsed() < max_age
        {
            return snapshot.clone();
        }

        // Reborrowed so the two probes can hold disjoint field borrows at
        // once, which is what lets them overlap: run sequentially they would
        // stack their timeouts and hold this mutex for twice as long.
        let cache = &mut *inner;
        let ((admin_link, status), ledger_link) = tokio::join!(
            probe_admin_api(config, &mut cache.admin),
            probe_ledger_api(config, &mut cache.ledger),
        );

        let (participant, synchronizers) = match status {
            Some(response) => decode_participant_status(response),
            None => (None, Vec::new()),
        };
        let snapshot = NodeHealthResponse {
            status: verdict(
                &admin_link,
                &ledger_link,
                participant.as_ref(),
                &synchronizers,
            ),
            checked_at: chrono::Utc::now().to_rfc3339(),
            admin_api: admin_link,
            ledger_api: ledger_link,
            participant,
            synchronizers,
            coordination_dar: None,
        };
        publish_metrics(&snapshot);
        inner.snapshot = Some((snapshot.clone(), Instant::now()));
        snapshot
    }
}

fn publish_metrics(snapshot: &NodeHealthResponse) {
    // A failed probe clears its gauge to NaN rather than holding the last good
    // number, which would read as a healthy latency during an outage.
    #[allow(clippy::cast_precision_loss)]
    let ms = |link: &LinkHealth| link.latency_ms.map_or(f64::NAN, |v| v as f64);
    ADMIN_LATENCY.set(ms(&snapshot.admin_api));
    LEDGER_LATENCY.set(ms(&snapshot.ledger_api));
    HEALTH_STATUS.set(match snapshot.status {
        NodeHealthStatus::Healthy => 2,
        NodeHealthStatus::Degraded => 1,
        NodeHealthStatus::Down => 0,
    });
    PROBED_AT.set(chrono::Utc::now().timestamp());
}

/// Derive the overall verdict from the probes.
///
/// Pure, so the whole truth table is unit-testable without a participant.
pub(crate) fn verdict(
    admin: &LinkHealth,
    ledger: &LinkHealth,
    participant: Option<&ParticipantHealth>,
    synchronizers: &[SynchronizerHealth],
) -> NodeHealthStatus {
    if !admin.reachable || !ledger.reachable {
        return NodeHealthStatus::Down;
    }
    // Both endpoints answered, but the Admin API answered with an error status
    // rather than a participant status — reachable, and nothing more known.
    let Some(participant) = participant else {
        return NodeHealthStatus::Degraded;
    };
    let degraded = !participant.initialized
        || !participant.active
        || participant.topology_queues.is_backed_up()
        || participant
            .components
            .iter()
            .any(|c| c.state != ComponentState::Ok)
        || synchronizers.is_empty()
        || synchronizers.iter().any(|s| !s.healthy);

    if degraded {
        NodeHealthStatus::Degraded
    } else {
        NodeHealthStatus::Healthy
    }
}

/// A gRPC status is proof of life unless it is the code tonic reports for a
/// connection that never reached the server.
fn answered(status: &tonic::Status) -> bool {
    status.code() != tonic::Code::Unavailable
}

#[allow(clippy::cast_possible_truncation)]
fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn timed_out() -> String {
    format!("no reply within {}s", PROBE_TIMEOUT.as_secs())
}

async fn probe_admin_api(
    config: &NodeConfig,
    slot: &mut Option<Channel>,
) -> (LinkHealth, Option<ParticipantStatusResponse>) {
    // The deadline wraps the connect too, not just the RPC. `slot` is emptied
    // before the connect, so a probe that times out leaves nothing cached and
    // the next attempt redials.
    match tokio::time::timeout(PROBE_TIMEOUT, admin_status(config, slot)).await {
        Ok(outcome) => outcome,
        Err(_) => (LinkHealth::unreachable(timed_out()), None),
    }
}

async fn admin_status(
    config: &NodeConfig,
    slot: &mut Option<Channel>,
) -> (LinkHealth, Option<ParticipantStatusResponse>) {
    let channel = match slot.take() {
        Some(channel) => channel,
        None => match config.admin_channel().await {
            Ok(channel) => channel,
            Err(e) => return (LinkHealth::unreachable(e.to_string()), None),
        },
    };

    let mut client = ParticipantStatusServiceClient::new(channel.clone());
    // Timed from here, so the reported latency is the RPC alone — a connect
    // only happens on the first probe after a restart and would otherwise
    // show up as a one-off spike.
    let started = Instant::now();

    match client.participant_status(ParticipantStatusRequest {}).await {
        Ok(response) => {
            *slot = Some(channel);
            (
                LinkHealth::answered(elapsed_ms(started), None),
                Some(response.into_inner()),
            )
        }
        Err(status) if answered(&status) => {
            *slot = Some(channel);
            (
                LinkHealth::answered(elapsed_ms(started), Some(status.message().to_string())),
                None,
            )
        }
        Err(status) => (LinkHealth::unreachable(status.message().to_string()), None),
    }
}

/// Probes the Ledger API with the cheapest unary call it offers.
///
/// Deliberately token-free: an auth-enabled Ledger API answers `UNAUTHENTICATED`,
/// which is all the liveness signal this card needs and keeps party credentials
/// out of a health probe.
async fn probe_ledger_api(config: &NodeConfig, slot: &mut Option<Channel>) -> LinkHealth {
    match tokio::time::timeout(PROBE_TIMEOUT, ledger_version(config, slot)).await {
        Ok(link) => link,
        Err(_) => LinkHealth::unreachable(timed_out()),
    }
}

async fn ledger_version(config: &NodeConfig, slot: &mut Option<Channel>) -> LinkHealth {
    let channel = match slot.take() {
        Some(channel) => channel,
        None => match config.ledger_channel().await {
            Ok(channel) => channel,
            Err(e) => return LinkHealth::unreachable(e.to_string()),
        },
    };

    let mut client = VersionServiceClient::new(channel.clone());
    let started = Instant::now();

    match client
        .get_ledger_api_version(GetLedgerApiVersionRequest {})
        .await
    {
        Ok(_) => {
            *slot = Some(channel);
            LinkHealth::answered(elapsed_ms(started), None)
        }
        Err(status) if answered(&status) => {
            *slot = Some(channel);
            LinkHealth::answered(elapsed_ms(started), Some(status.message().to_string()))
        }
        Err(status) => LinkHealth::unreachable(status.message().to_string()),
    }
}

fn decode_participant_status(
    response: ParticipantStatusResponse,
) -> (Option<ParticipantHealth>, Vec<SynchronizerHealth>) {
    match response.kind {
        Some(Kind::Status(status)) => {
            let synchronizers = status
                .connected_synchronizers
                .into_iter()
                .map(|s| SynchronizerHealth {
                    physical_synchronizer_id: s.physical_synchronizer_id,
                    healthy: s.health == connected_synchronizer::Health::Healthy as i32,
                })
                .collect();
            let common = status.common_status.unwrap_or_default();
            let queues = common.topology_queues.unwrap_or_default();
            let participant = ParticipantHealth {
                initialized: true,
                uid: common.uid,
                uptime_seconds: common
                    .uptime
                    .map_or(0, |d| u64::try_from(d.seconds).unwrap_or(0)),
                active: status.active,
                version: common.version,
                topology_queues: TopologyQueues {
                    manager: queues.manager,
                    dispatcher: queues.dispatcher,
                    clients: queues.clients,
                },
                components: common
                    .components
                    .into_iter()
                    .map(|c| {
                        let (state, description) = match c.status {
                            Some(component_status::Status::Ok(d)) => {
                                (ComponentState::Ok, d.description)
                            }
                            Some(component_status::Status::Degraded(d)) => {
                                (ComponentState::Degraded, d.description)
                            }
                            Some(component_status::Status::Failed(d)) => {
                                (ComponentState::Failed, d.description)
                            }
                            Some(component_status::Status::Fatal(d)) => {
                                (ComponentState::Fatal, d.description)
                            }
                            None => (ComponentState::Unknown, None),
                        };
                        ComponentHealth {
                            name: c.name,
                            state,
                            description,
                        }
                    })
                    .collect(),
            };
            (Some(participant), synchronizers)
        }
        Some(Kind::NotInitialized(not_initialized)) => (
            Some(ParticipantHealth {
                initialized: false,
                uid: String::new(),
                uptime_seconds: 0,
                active: not_initialized.active,
                version: not_initialized.version,
                topology_queues: TopologyQueues::default(),
                components: Vec::new(),
            }),
            Vec::new(),
        ),
        None => (None, Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::admin::{
        health::v30::{ComponentStatus, NotInitialized, Status, TopologyQueueStatus},
        participant::v30::{ConnectedSynchronizer, participant_status_response::*},
    };

    use super::*;

    fn reachable() -> LinkHealth {
        LinkHealth::answered(3, None)
    }

    fn healthy_participant() -> ParticipantHealth {
        ParticipantHealth {
            initialized: true,
            uid: "participant1".to_string(),
            uptime_seconds: 120,
            active: true,
            version: "3.5.8".to_string(),
            topology_queues: TopologyQueues::default(),
            components: vec![ComponentHealth {
                name: "sequencer-client".to_string(),
                state: ComponentState::Ok,
                description: None,
            }],
        }
    }

    fn connected() -> Vec<SynchronizerHealth> {
        vec![SynchronizerHealth {
            physical_synchronizer_id: "global-domain::1220ab".to_string(),
            healthy: true,
        }]
    }

    #[test]
    fn everything_answering_and_connected_is_healthy() {
        assert_eq!(
            verdict(
                &reachable(),
                &reachable(),
                Some(&healthy_participant()),
                &connected()
            ),
            NodeHealthStatus::Healthy
        );
    }

    #[test]
    fn either_api_unreachable_is_down() {
        let dead = LinkHealth::unreachable("connection refused".to_string());
        for (admin, ledger) in [
            (dead.clone(), reachable()),
            (reachable(), dead.clone()),
            (dead.clone(), dead),
        ] {
            assert_eq!(
                verdict(&admin, &ledger, Some(&healthy_participant()), &connected()),
                NodeHealthStatus::Down,
            );
        }
    }

    // An endpoint that answers `UNAUTHENTICATED` is alive; only a connection
    // that never reached the server counts as Down. Pins the rule that keeps
    // the token-free Ledger API probe honest.
    #[test]
    fn an_error_status_still_counts_as_reachable() {
        let rejected = LinkHealth::answered(4, Some("no auth token".to_string()));
        assert_eq!(
            verdict(
                &reachable(),
                &rejected,
                Some(&healthy_participant()),
                &connected()
            ),
            NodeHealthStatus::Healthy
        );
    }

    #[test]
    fn each_participant_fault_degrades_on_its_own() {
        let passive = ParticipantHealth {
            active: false,
            ..healthy_participant()
        };
        let initializing = ParticipantHealth {
            initialized: false,
            ..healthy_participant()
        };
        let backlogged = ParticipantHealth {
            topology_queues: TopologyQueues {
                manager: TOPOLOGY_QUEUE_BACKLOG + 1,
                dispatcher: 0,
                clients: 0,
            },
            ..healthy_participant()
        };
        for state in [
            ComponentState::Degraded,
            ComponentState::Failed,
            ComponentState::Fatal,
            ComponentState::Unknown,
        ] {
            let faulty = ParticipantHealth {
                components: vec![ComponentHealth {
                    name: "sequencer-client".to_string(),
                    state,
                    description: None,
                }],
                ..healthy_participant()
            };
            assert_eq!(
                verdict(&reachable(), &reachable(), Some(&faulty), &connected()),
                NodeHealthStatus::Degraded,
                "component state {state:?} should degrade",
            );
        }
        for participant in [passive, initializing, backlogged] {
            assert_eq!(
                verdict(&reachable(), &reachable(), Some(&participant), &connected()),
                NodeHealthStatus::Degraded,
            );
        }
    }

    // A queue at exactly the threshold is still ordinary in-flight traffic —
    // pins the comparison at `>` so flipping it to `>=` fails.
    #[test]
    fn a_queue_at_the_threshold_is_not_a_backlog() {
        let at_threshold = ParticipantHealth {
            topology_queues: TopologyQueues {
                manager: TOPOLOGY_QUEUE_BACKLOG,
                dispatcher: TOPOLOGY_QUEUE_BACKLOG,
                clients: TOPOLOGY_QUEUE_BACKLOG,
            },
            ..healthy_participant()
        };
        assert_eq!(
            verdict(
                &reachable(),
                &reachable(),
                Some(&at_threshold),
                &connected()
            ),
            NodeHealthStatus::Healthy
        );
    }

    #[test]
    fn a_lost_or_unhealthy_synchronizer_degrades() {
        let unhealthy = vec![SynchronizerHealth {
            physical_synchronizer_id: "global-domain::1220ab".to_string(),
            healthy: false,
        }];
        for synchronizers in [Vec::new(), unhealthy] {
            assert_eq!(
                verdict(
                    &reachable(),
                    &reachable(),
                    Some(&healthy_participant()),
                    &synchronizers
                ),
                NodeHealthStatus::Degraded,
            );
        }
    }

    // Both APIs answered but the Admin API returned an error status instead of
    // a participant status: reachable, so not Down, but nothing is known.
    #[test]
    fn a_reachable_admin_api_with_no_status_degrades() {
        assert_eq!(
            verdict(&reachable(), &reachable(), None, &[]),
            NodeHealthStatus::Degraded
        );
    }

    #[test]
    fn decode_maps_a_full_status_reply() {
        let response = ParticipantStatusResponse {
            kind: Some(Kind::Status(ParticipantStatusResponseStatus {
                common_status: Some(Status {
                    uid: "participant1".to_string(),
                    uptime: Some(prost_types::Duration {
                        seconds: 3_600,
                        nanos: 0,
                    }),
                    ports: std::collections::HashMap::new(),
                    active: true,
                    topology_queues: Some(TopologyQueueStatus {
                        manager: 1,
                        dispatcher: 2,
                        clients: 3,
                    }),
                    components: vec![ComponentStatus {
                        name: "sequencer-client".to_string(),
                        status: Some(component_status::Status::Degraded(
                            component_status::StatusData {
                                description: Some("catching up".to_string()),
                            },
                        )),
                    }],
                    version: "3.5.8".to_string(),
                }),
                connected_synchronizers: vec![
                    ConnectedSynchronizer {
                        physical_synchronizer_id: "global-domain::1220ab".to_string(),
                        health: connected_synchronizer::Health::Healthy as i32,
                    },
                    ConnectedSynchronizer {
                        physical_synchronizer_id: "other::1220cd".to_string(),
                        health: connected_synchronizer::Health::Unhealthy as i32,
                    },
                ],
                active: true,
                supported_protocol_versions: vec![35],
            })),
        };

        let (participant, synchronizers) = decode_participant_status(response);
        let participant = participant.expect("a Status reply yields a participant");
        assert!(participant.initialized);
        assert_eq!(participant.uptime_seconds, 3_600);
        assert_eq!(participant.version, "3.5.8");
        assert_eq!(participant.topology_queues.clients, 3);
        assert_eq!(participant.components[0].state, ComponentState::Degraded);
        assert_eq!(
            participant.components[0].description.as_deref(),
            Some("catching up")
        );
        assert_eq!(synchronizers.len(), 2);
        assert!(synchronizers[0].healthy);
        assert!(!synchronizers[1].healthy);
    }

    // A participant still waiting for external input answers NotInitialized,
    // which carries no status at all — it must not read as a healthy node.
    #[test]
    fn decode_maps_a_not_initialized_reply() {
        let response = ParticipantStatusResponse {
            kind: Some(Kind::NotInitialized(NotInitialized {
                active: true,
                waiting_for_external_input: 1,
                version: "3.5.8".to_string(),
            })),
        };

        let (participant, synchronizers) = decode_participant_status(response);
        let participant = participant.expect("a NotInitialized reply still yields a participant");
        assert!(!participant.initialized);
        assert!(synchronizers.is_empty());
        assert_eq!(
            verdict(
                &reachable(),
                &reachable(),
                Some(&participant),
                &synchronizers
            ),
            NodeHealthStatus::Degraded
        );
    }

    /// Accepts a connection and then says nothing. tonic waits for the HTTP/2
    /// preface during `connect`, so this hangs channel establishment — the
    /// path that used to escape `PROBE_TIMEOUT` and run to the 10s connect
    /// timeout instead.
    async fn silent_listener()
    -> crate::error::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let mut accepted = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                // Held open, never spoken to.
                accepted.push(stream);
            }
        });
        Ok((addr, server))
    }

    // Pins both halves of the handler's advertised bound: the deadline covers
    // channel establishment, and the two probes overlap. A regression in
    // either shows up as wall-clock — an unbounded connect runs to tonic's 10s
    // timeout, and sequential probes take twice as long as concurrent ones.
    #[tokio::test]
    async fn a_silent_participant_cannot_outlast_the_probe_timeout() -> crate::error::Result {
        let (addr, server) = silent_listener().await?;
        let mut config = NodeConfig::default();
        config.canton.admin_api_host = addr.ip().to_string();
        config.canton.admin_api_port = addr.port();
        config.canton.ledger_api_host = addr.ip().to_string();
        config.canton.ledger_api_port = addr.port();

        let started = Instant::now();
        let snapshot = HealthCache::new().get(&config).await;
        let elapsed = started.elapsed();
        server.abort();

        assert_eq!(snapshot.status, NodeHealthStatus::Down);
        assert!(!snapshot.admin_api.reachable);
        assert!(!snapshot.ledger_api.reachable);
        assert!(
            elapsed < PROBE_TIMEOUT + Duration::from_millis(750),
            "snapshot took {elapsed:?}, past the concurrent {PROBE_TIMEOUT:?} bound"
        );
        Ok(())
    }

    #[test]
    fn an_unavailable_status_is_the_only_transport_failure() {
        assert!(!answered(&tonic::Status::unavailable("connection refused")));
        for status in [
            tonic::Status::unauthenticated("no token"),
            tonic::Status::permission_denied("no rights"),
            tonic::Status::internal("boom"),
        ] {
            assert!(
                answered(&status),
                "{} should count as answered",
                status.code()
            );
        }
    }
}
