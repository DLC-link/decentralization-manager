//! The `DecmanNode` registry (design D3): publish and refresh this node's
//! entry, read the peers' entries, and turn them into a health snapshot.
//!
//! Every read is keyed by the contract signatory (`node`), never by the
//! `participantId` text, which is a claim. The claim is cross-checked with
//! [`verify_hosting`] before an entry counts for a peer.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};
use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{enums::TopologyChangeOp, vetted_packages::VettedPackage},
    topology::admin::v30::{
        BaseQuery, ListVettedPackagesRequest,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use common::{
    canton_id::CantonId,
    coordination::{DecmanNodeView, PeerHealthStatus, RegistryResponse},
};
use prost_types::Timestamp;

use crate::{
    build_info,
    config::{NodeConfig, Peer},
    consts,
    server::package_inventory::fetch_package_id_to_name,
    utils,
    workflow::topology,
};

use super::{
    daml::{
        ActiveContract, CoordinationClient, CoordinationTemplate, choices,
        codec::{ChoiceArgument, DecmanNodeRecord, DecmanNodeUpdateArgs, unit_argument},
    },
    identity::{HostingCheck, NodeIdentity, verify_hosting},
};

/// Microseconds per second, for `Time` arithmetic.
const MICROS_PER_SEC: i64 = 1_000_000;

// ---------------------------------------------------------------------------
// Desired state
// ---------------------------------------------------------------------------

/// The `DecmanNode` this node wants on the ledger right now.
///
/// Observers are the peers whose participant has vetted the coordination
/// package (`vetted_peers`) and whose peers-table row names a node party.
/// Canton rejects a create whose informee has not vetted the package, so an
/// unvetted peer is left out and picked up on a later observer tick. The
/// list is sorted and deduplicated so [`needs_update`] compares bytes, not
/// order.
pub fn desired_node_record(
    config: &NodeConfig,
    identity: &NodeIdentity,
    peers: &[Peer],
    vetted_peers: &HashSet<CantonId>,
    last_active_at: i64,
) -> DecmanNodeRecord {
    let mut observers: Vec<CantonId> = peers
        .iter()
        .filter(|p| p.participant_id != identity.participant_id)
        .filter(|p| vetted_peers.contains(&p.participant_id))
        .filter_map(|p| p.party.as_deref())
        .filter_map(|party| match CantonId::parse(party) {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::warn!(party, error = %e, "peer party is not a Canton id; skipping");
                None
            }
        })
        .filter(|id| *id != identity.node_party)
        .collect();
    observers.sort();
    observers.dedup();

    let heartbeat = i64::try_from(consts::heartbeat_interval_secs()).unwrap_or(i64::MAX);
    let floor = i64::try_from(consts::heartbeat_min_interval_secs()).unwrap_or(1);
    DecmanNodeRecord {
        node: identity.node_party.clone(),
        participant_id: identity.participant_id.to_string(),
        display_name: display_name(config),
        version: build_info::SEMVER.to_string(),
        build_version: build_info::build_version().to_string(),
        coordination_version: consts::COORDINATION_VERSION,
        peers: observers,
        last_active_at,
        heartbeat_interval_secs: heartbeat,
        min_heartbeat_interval_secs: floor.min(heartbeat),
    }
}

/// The name this node advertises. The peers table has no row for self, so
/// the participant prefix stands in.
fn display_name(config: &NodeConfig) -> String {
    config
        .node
        .participant_id
        .as_ref()
        .map(|id| id.prefix.clone())
        .unwrap_or_else(|| "decman".to_string())
}

/// Whether the on-ledger entry differs from the desired one in any field a
/// `DecmanNode_Update` can change. `lastActiveAt` is excluded: the heartbeat
/// owns it.
pub fn needs_update(current: &DecmanNodeRecord, desired: &DecmanNodeRecord) -> bool {
    let mut current_peers = current.peers.clone();
    current_peers.sort();
    current_peers.dedup();
    current_peers != desired.peers
        || current.display_name != desired.display_name
        || current.version != desired.version
        || current.build_version != desired.build_version
        || current.coordination_version != desired.coordination_version
        || current.heartbeat_interval_secs != desired.heartbeat_interval_secs
        || current.min_heartbeat_interval_secs != desired.min_heartbeat_interval_secs
        || current.participant_id != desired.participant_id
}

/// Whether a heartbeat is due: the entry's own interval has elapsed since
/// `last_active_at`. The template floor is always at or below the interval,
/// so an exercise this predicate allows never trips `heartbeat too soon`.
pub fn heartbeat_due(last_active_at: i64, heartbeat_interval_secs: i64, now: i64) -> bool {
    let interval = heartbeat_interval_secs
        .max(1)
        .saturating_mul(MICROS_PER_SEC);
    now.saturating_sub(last_active_at) >= interval
}

/// Whether a peer entry is stale: `now - lastActiveAt` exceeds the stale
/// factor times the peer's own heartbeat interval.
pub fn is_stale(last_active_at: i64, heartbeat_interval_secs: i64, now: i64, factor: u64) -> bool {
    let factor = i64::try_from(factor.max(1)).unwrap_or(i64::MAX);
    let window = heartbeat_interval_secs
        .max(1)
        .saturating_mul(MICROS_PER_SEC)
        .saturating_mul(factor);
    now.saturating_sub(last_active_at) > window
}

// ---------------------------------------------------------------------------
// Vetting
// ---------------------------------------------------------------------------

/// Package ids on this participant whose name is the coordination package.
///
/// The topology store vets package *ids*; the config names the package. This
/// participant's `ListPackages` is the only place that maps one to the other
/// without a token. An empty set means this participant has not uploaded
/// the package yet, in which case nothing can be published either.
///
/// # Errors
/// Returns an error when the Admin API call fails.
pub async fn coordination_package_ids(
    config: &NodeConfig,
    package_name: &str,
) -> Result<HashSet<String>> {
    let id_to_name = fetch_package_id_to_name(config).await?;
    Ok(id_to_name
        .into_iter()
        .filter(|(_, name)| name == package_name)
        .map(|(id, _)| id)
        .collect())
}

/// Package ids `participant_id` has vetted on the synchronizer and that are
/// in their validity window right now.
///
/// TODO(server/package_inventory): this generalizes `fetch_vetted_packages`
/// with a `filter_participant` argument; move it there once that file is
/// open for edits and have the self-read call this.
///
/// # Errors
/// Returns an error when the synchronizer id cannot be resolved or the
/// topology read fails.
pub async fn fetch_vetted_packages_for(
    config: &NodeConfig,
    participant_id: &CantonId,
) -> Result<HashSet<String>> {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let channel = config
        .admin_channel()
        .await
        .context("connect to participant Admin API")?;
    let mut client = TopologyManagerReadServiceClient::new(channel)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let response = client
        .list_vetted_packages(tonic::Request::new(ListVettedPackagesRequest {
            base_query: Some(BaseQuery {
                operation: TopologyChangeOp::AddReplace as i32,
                ..topology::head_state_query(&synchronizer_id)
            }),
            filter_participant: participant_id.to_string(),
        }))
        .await
        .with_context(|| format!("list vetted packages of {participant_id}"))?
        .into_inner();

    let now = now_timestamp();
    let wanted = participant_id.to_string();
    let mut vetted = HashSet::new();
    for result in response.results {
        let Some(item) = result.item else { continue };
        // `filter_participant` is a substring post-filter on some Canton
        // versions, so confirm the exact uid.
        if item.participant_uid != wanted {
            continue;
        }
        for package in item.packages {
            if package_valid_at(&package, &now) {
                vetted.insert(package.package_id);
            }
        }
    }
    Ok(vetted)
}

/// Whether a vetting entry is in effect at `now`.
fn package_valid_at(package: &VettedPackage, now: &Timestamp) -> bool {
    let le = |a: &Timestamp, b: &Timestamp| (a.seconds, a.nanos) <= (b.seconds, b.nanos);
    package
        .valid_from_inclusive
        .as_ref()
        .is_none_or(|from| le(from, now))
        && package
            .valid_until_exclusive
            .as_ref()
            .is_none_or(|until| !le(until, now))
}

fn now_timestamp() -> Timestamp {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    Timestamp {
        seconds: i64::try_from(now.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(now.subsec_nanos()).unwrap_or(0),
    }
}

/// Whether `participant_id` has vetted any of the coordination package ids.
///
/// # Errors
/// As [`fetch_vetted_packages_for`].
pub async fn participant_has_vetted(
    config: &NodeConfig,
    participant_id: &CantonId,
    coordination_ids: &HashSet<String>,
) -> Result<bool> {
    if coordination_ids.is_empty() {
        return Ok(false);
    }
    let vetted = fetch_vetted_packages_for(config, participant_id).await?;
    Ok(!vetted.is_disjoint(coordination_ids))
}

/// The configured peers (self excluded) whose participant has vetted the
/// coordination package. A peer whose topology read fails is logged and
/// treated as not vetted for this tick.
///
/// # Errors
/// Returns an error when the package id lookup fails.
pub async fn vetted_peers(
    config: &NodeConfig,
    package_name: &str,
    self_participant: &CantonId,
    peers: &[Peer],
) -> Result<HashSet<CantonId>> {
    let ids = coordination_package_ids(config, package_name).await?;
    let mut out = HashSet::new();
    for peer in peers
        .iter()
        .filter(|p| p.participant_id != *self_participant)
    {
        match participant_has_vetted(config, &peer.participant_id, &ids).await {
            Ok(true) => {
                out.insert(peer.participant_id.clone());
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(
                participant = %peer.participant_id,
                error = %e,
                "vetting read failed; treating the peer as unvetted this tick"
            ),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Own entry
// ---------------------------------------------------------------------------

/// What [`publish_or_update`] did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishOutcome {
    Created(String),
    Updated(String),
    Unchanged(String),
}

impl PublishOutcome {
    pub fn contract_id(&self) -> &str {
        match self {
            Self::Created(c) | Self::Updated(c) | Self::Unchanged(c) => c,
        }
    }
}

/// This node's current `DecmanNode`, the newest by offset when several exist.
///
/// # Errors
/// Returns an error when the read fails.
pub async fn read_own_entry(
    client: &CoordinationClient,
) -> Result<Option<ActiveContract<DecmanNodeRecord>>> {
    let mine: Vec<_> = client
        .list_active::<DecmanNodeRecord>()
        .await?
        .into_iter()
        .filter(|c| c.record.node == *client.node_party())
        .collect();
    if mine.len() > 1 {
        tracing::warn!(
            count = mine.len(),
            "several DecmanNode entries signed by this node are active; using the newest"
        );
    }
    Ok(mine.into_iter().max_by_key(|c| c.offset))
}

/// Create this node's entry when absent, or `DecmanNode_Update` it when any
/// field differs from `desired`. Never touches an entry that matches.
///
/// # Errors
/// Returns an error when the read or the submission fails.
pub async fn publish_or_update(
    client: &CoordinationClient,
    desired: &DecmanNodeRecord,
) -> Result<PublishOutcome> {
    if desired.node != *client.node_party() {
        bail!(
            "desired registry entry is signed by {} but the client acts as {}",
            desired.node,
            client.node_party()
        );
    }
    match read_own_entry(client).await? {
        None => {
            let cid = client.create(desired).await.context("create DecmanNode")?;
            tracing::info!(contract_id = %cid, peers = desired.peers.len(), "published DecmanNode");
            Ok(PublishOutcome::Created(cid))
        }
        Some(current) if needs_update(&current.record, desired) => {
            let args = DecmanNodeUpdateArgs::from_desired(desired);
            let outcome = client
                .exercise(
                    CoordinationTemplate::DecmanNode,
                    &current.contract_id,
                    choices::DECMAN_NODE_UPDATE,
                    args.to_value(),
                )
                .await
                .context("DecmanNode_Update")?;
            let cid = outcome
                .created_contract_id
                .unwrap_or_else(|| current.contract_id.clone());
            tracing::info!(contract_id = %cid, peers = desired.peers.len(), "updated DecmanNode");
            Ok(PublishOutcome::Updated(cid))
        }
        Some(current) => Ok(PublishOutcome::Unchanged(current.contract_id)),
    }
}

/// Exercise `DecmanNode_Heartbeat` when the entry's interval has elapsed.
/// Returns the new contract id when a heartbeat was sent.
///
/// # Errors
/// Returns an error when the submission fails.
pub async fn heartbeat_if_due(
    client: &CoordinationClient,
    current: &ActiveContract<DecmanNodeRecord>,
    now: i64,
) -> Result<Option<String>> {
    if !heartbeat_due(
        current.record.last_active_at,
        current.record.heartbeat_interval_secs,
        now,
    ) {
        return Ok(None);
    }
    let outcome = client
        .exercise(
            CoordinationTemplate::DecmanNode,
            &current.contract_id,
            choices::DECMAN_NODE_HEARTBEAT,
            unit_argument(),
        )
        .await
        .context("DecmanNode_Heartbeat")?;
    tracing::debug!(contract_id = ?outcome.created_contract_id, "DecmanNode heartbeat sent");
    Ok(outcome.created_contract_id)
}

/// Exercise `DecmanNode_Retire` on the current entry, if any.
///
/// # Errors
/// Returns an error when the read or the submission fails.
pub async fn retire(client: &CoordinationClient) -> Result<Option<String>> {
    let Some(current) = read_own_entry(client).await? else {
        return Ok(None);
    };
    client
        .exercise(
            CoordinationTemplate::DecmanNode,
            &current.contract_id,
            choices::DECMAN_NODE_RETIRE,
            unit_argument(),
        )
        .await
        .context("DecmanNode_Retire")?;
    Ok(Some(current.contract_id))
}

// ---------------------------------------------------------------------------
// Peer entries
// ---------------------------------------------------------------------------

/// A peer's `DecmanNode` as read, plus the topology cross-check of its
/// `participantId` claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerEntry {
    pub contract_id: String,
    pub offset: i64,
    pub record: DecmanNodeRecord,
    /// How the synchronizer says `record.node` is hosted on the claimed
    /// participant. `None` when the claim does not parse as a Canton id.
    pub hosting: Option<HostingCheck>,
}

impl PeerEntry {
    /// The claim passed: the signatory is hosted on the claimed participant
    /// with Submission permission.
    pub fn hosting_verified(&self) -> bool {
        self.hosting
            .as_ref()
            .is_some_and(HostingCheck::has_submission)
    }

    /// The claimed participant, when it parses.
    pub fn participant(&self) -> Option<CantonId> {
        CantonId::parse(&self.record.participant_id).ok()
    }
}

/// Every `DecmanNode` visible to this node except its own, one per
/// signatory (the newest by offset), with the hosting cross-check applied.
///
/// # Errors
/// Returns an error when the ACS read fails. A failed topology read for one
/// entry is logged and leaves `hosting = None` for that entry.
pub async fn read_peer_entries(
    client: &CoordinationClient,
    config: &NodeConfig,
) -> Result<Vec<PeerEntry>> {
    let all = client.list_active::<DecmanNodeRecord>().await?;
    let mut newest: HashMap<CantonId, ActiveContract<DecmanNodeRecord>> = HashMap::new();
    for contract in all {
        if contract.record.node == *client.node_party() {
            continue;
        }
        let replace = newest
            .get(&contract.record.node)
            .is_none_or(|existing| contract.offset > existing.offset);
        if replace {
            newest.insert(contract.record.node.clone(), contract);
        }
    }

    let mut entries = Vec::with_capacity(newest.len());
    for (_, contract) in newest {
        let hosting = match CantonId::parse(&contract.record.participant_id) {
            Ok(participant) => {
                match verify_hosting(config, &contract.record.node, &participant).await {
                    Ok(check) => Some(check),
                    Err(e) => {
                        tracing::warn!(
                            node = %contract.record.node,
                            participant = %participant,
                            error = %e,
                            "hosting check failed for a registry entry"
                        );
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    node = %contract.record.node,
                    claimed = %contract.record.participant_id,
                    error = %e,
                    "registry entry claims a participant id that does not parse"
                );
                None
            }
        };
        entries.push(PeerEntry {
            contract_id: contract.contract_id,
            offset: contract.offset,
            record: contract.record,
            hosting,
        });
    }
    entries.sort_by(|a, b| a.record.node.cmp(&b.record.node));
    Ok(entries)
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

/// What the observer loop last learned about one configured peer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerHealth {
    /// The node party from the peers table, when the operator entered one.
    pub node_party: Option<CantonId>,
    pub version: Option<String>,
    pub build_version: Option<String>,
    pub coordination_version: Option<i64>,
    /// Micros since the epoch.
    pub last_active_at: Option<i64>,
    pub heartbeat_interval_secs: Option<i64>,
    /// Whether the peer's participant has vetted the coordination package.
    pub vetted: bool,
    pub status: PeerHealthStatus,
}

/// The in-memory view `/participants-status` and the preflight gate read.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PeerHealthSnapshot {
    /// Keyed by the peer's participant id.
    pub entries: HashMap<CantonId, PeerHealth>,
    /// Unix seconds of the last refresh; `None` before the first one.
    pub refreshed_at: Option<i64>,
}

impl PeerHealthSnapshot {
    pub fn get(&self, participant: &CantonId) -> Option<&PeerHealth> {
        self.entries.get(participant)
    }
}

/// Build the snapshot from the configured peers and the entries read.
///
/// An entry counts for a peer only when its signatory is the peer's node
/// party, its `participantId` claim is the peer's participant, and the
/// hosting check passed. `now` is micros since the epoch.
pub fn build_snapshot(
    peers: &[Peer],
    self_participant: &CantonId,
    entries: &[PeerEntry],
    vetted: &HashSet<CantonId>,
    now: i64,
    stale_factor: u64,
    refreshed_at: i64,
) -> PeerHealthSnapshot {
    let by_signatory: HashMap<&CantonId, &PeerEntry> =
        entries.iter().map(|e| (&e.record.node, e)).collect();
    let mut out = HashMap::new();
    for peer in peers
        .iter()
        .filter(|p| p.participant_id != *self_participant)
    {
        let node_party = peer.party.as_deref().and_then(|p| CantonId::parse(p).ok());
        let is_vetted = vetted.contains(&peer.participant_id);
        let matched = node_party.as_ref().and_then(|party| {
            by_signatory.get(party).copied().filter(|e| {
                e.hosting_verified() && e.participant().as_ref() == Some(&peer.participant_id)
            })
        });
        let health = match matched {
            Some(entry) => {
                let status = if is_stale(
                    entry.record.last_active_at,
                    entry.record.heartbeat_interval_secs,
                    now,
                    stale_factor,
                ) {
                    PeerHealthStatus::Stale
                } else {
                    PeerHealthStatus::Active
                };
                PeerHealth {
                    node_party,
                    version: Some(entry.record.version.clone()),
                    build_version: Some(entry.record.build_version.clone()),
                    coordination_version: Some(entry.record.coordination_version),
                    last_active_at: Some(entry.record.last_active_at),
                    heartbeat_interval_secs: Some(entry.record.heartbeat_interval_secs),
                    vetted: is_vetted,
                    status,
                }
            }
            None => PeerHealth {
                node_party,
                version: None,
                build_version: None,
                coordination_version: None,
                last_active_at: None,
                heartbeat_interval_secs: None,
                vetted: is_vetted,
                status: if is_vetted {
                    PeerHealthStatus::Unknown
                } else {
                    PeerHealthStatus::Unvetted
                },
            },
        };
        out.insert(peer.participant_id.clone(), health);
    }
    PeerHealthSnapshot {
        entries: out,
        refreshed_at: Some(refreshed_at),
    }
}

/// The version gate a proposer runs before creating a `WorkflowProposal`
/// (design D3). Returns one `(participant, reason)` per invitee that is not
/// ready; an empty vector means every invitee passed.
///
/// An invitee absent from the snapshot is reported as unknown to this node.
pub fn preflight_unready_peers(
    snapshot: &PeerHealthSnapshot,
    invitees: &[CantonId],
) -> Vec<(CantonId, String)> {
    let mut out = Vec::new();
    for participant in invitees {
        let reason = match snapshot.get(participant) {
            None => Some("not in the peers table".to_string()),
            Some(h) if h.status == PeerHealthStatus::Unvetted => {
                Some("has not vetted the coordination package".to_string())
            }
            Some(h) if h.status == PeerHealthStatus::Unknown => Some(
                "no registry entry visible (peer has not added you, or has not vetted)".to_string(),
            ),
            Some(h) => match h.coordination_version {
                Some(v) if v >= consts::COORDINATION_VERSION => None,
                Some(v) => Some(format!(
                    "registry entry too old: coordinationVersion {v} < {}",
                    consts::COORDINATION_VERSION
                )),
                None => Some("registry entry carries no coordinationVersion".to_string()),
            },
        };
        if let Some(reason) = reason {
            out.push((participant.clone(), reason));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/// The wire view of one entry. `status` is derived from the entry alone
/// (Active or Stale); the caller overrides it for peers it knows better.
pub fn view_of(entry: &PeerEntry, now: i64, stale_factor: u64) -> DecmanNodeView {
    let record = &entry.record;
    let status = if is_stale(
        record.last_active_at,
        record.heartbeat_interval_secs,
        now,
        stale_factor,
    ) {
        PeerHealthStatus::Stale
    } else {
        PeerHealthStatus::Active
    };
    DecmanNodeView {
        contract_id: entry.contract_id.clone(),
        node_party: record.node.clone(),
        participant_id: record.participant_id.clone(),
        hosting_verified: entry.hosting_verified(),
        display_name: record.display_name.clone(),
        version: record.version.clone(),
        build_version: record.build_version.clone(),
        coordination_version: record.coordination_version,
        peers: record.peers.clone(),
        last_active_at: record.last_active_at.div_euclid(MICROS_PER_SEC),
        heartbeat_interval_secs: record.heartbeat_interval_secs,
        min_heartbeat_interval_secs: record.min_heartbeat_interval_secs,
        heartbeat_age_secs: now
            .saturating_sub(record.last_active_at)
            .div_euclid(MICROS_PER_SEC)
            .max(0),
        status,
    }
}

/// Split the visible entries into the `peers` bucket (signatory is a
/// configured peer's node party) and the `inbound` bucket (everything else).
pub fn registry_response(
    own: Option<&ActiveContract<DecmanNodeRecord>>,
    entries: &[PeerEntry],
    peers: &[Peer],
    now: i64,
    stale_factor: u64,
) -> RegistryResponse {
    let known: HashSet<CantonId> = peers
        .iter()
        .filter_map(|p| p.party.as_deref())
        .filter_map(|p| CantonId::parse(p).ok())
        .collect();
    let mut peer_views = Vec::new();
    let mut inbound = Vec::new();
    for entry in entries {
        let view = view_of(entry, now, stale_factor);
        if known.contains(&entry.record.node) {
            peer_views.push(view);
        } else {
            inbound.push(view);
        }
    }
    let self_entry = own.map(|c| {
        view_of(
            &PeerEntry {
                contract_id: c.contract_id.clone(),
                offset: c.offset,
                record: c.record.clone(),
                hosting: None,
            },
            now,
            stale_factor,
        )
    });
    RegistryResponse {
        self_entry,
        peers: peer_views,
        inbound,
    }
}

#[cfg(test)]
mod tests {
    use common::types::Permission;

    use super::*;
    use crate::onledger::daml::codec::tests::{node_record, party};

    const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
    const SEC: i64 = MICROS_PER_SEC;

    fn participant(n: u8) -> CantonId {
        CantonId::parse(&format!("participant{n}::{NS}")).expect("valid id")
    }

    fn peer(n: u8, party_prefix: Option<&str>) -> Peer {
        Peer {
            participant_id: participant(n),
            name: format!("Node {n}"),
            address: String::new(),
            port: 0,
            public_key: String::new(),
            party: party_prefix.map(|p| party(p).to_string()),
        }
    }

    fn hosted(permission: Permission) -> HostingCheck {
        HostingCheck {
            mapping_exists: true,
            hosted: true,
            permission: Some(permission),
            onboarding: false,
            threshold: 1,
        }
    }

    fn entry(
        node: &str,
        participant_n: u8,
        last_active_at: i64,
        hosting: Option<HostingCheck>,
    ) -> PeerEntry {
        PeerEntry {
            contract_id: format!("00{node}"),
            offset: 1,
            record: DecmanNodeRecord {
                participant_id: participant(participant_n).to_string(),
                last_active_at,
                heartbeat_interval_secs: 10,
                ..node_record(node, &[])
            },
            hosting,
        }
    }

    #[test]
    fn heartbeat_is_due_once_the_interval_has_elapsed() {
        assert!(!heartbeat_due(0, 10, 9 * SEC));
        assert!(heartbeat_due(0, 10, 10 * SEC));
        assert!(
            heartbeat_due(0, 0, SEC),
            "a zero interval clamps to one second"
        );
    }

    #[test]
    fn staleness_uses_the_factor_times_the_peer_interval() {
        assert!(!is_stale(0, 10, 30 * SEC, 3));
        assert!(is_stale(0, 10, 30 * SEC + 1, 3));
        assert!(is_stale(0, 10, 11 * SEC, 0), "a zero factor clamps to one");
    }

    #[test]
    fn needs_update_ignores_last_active_at_and_peer_order() {
        let current = node_record("node-a", &["node-c", "node-b"]);
        let mut desired = node_record("node-a", &["node-b", "node-c"]);
        desired.last_active_at += 5 * SEC;
        assert!(!needs_update(&current, &desired));
        desired.version = "2.1.0".into();
        assert!(needs_update(&current, &desired));
        let mut fewer = node_record("node-a", &["node-b"]);
        fewer.last_active_at = current.last_active_at;
        assert!(needs_update(&current, &fewer));
    }

    #[test]
    fn snapshot_is_keyed_by_signatory_and_verified_hosting() {
        let peers = vec![
            peer(2, Some("node-b")),
            peer(3, Some("node-c")),
            peer(4, Some("node-d")),
            peer(5, None),
            peer(6, Some("node-f")),
        ];
        let now = 100 * SEC;
        let entries = vec![
            // b: fresh, hosted with Submission on the claimed participant.
            entry("node-b", 2, 95 * SEC, Some(hosted(Permission::Submission))),
            // c: stale (interval 10, factor 3, age 40).
            entry("node-c", 3, 60 * SEC, Some(hosted(Permission::Submission))),
            // d: claims participant 4 but hosting shows Confirmation only.
            entry(
                "node-d",
                4,
                99 * SEC,
                Some(hosted(Permission::Confirmation)),
            ),
            // f: signed by node-f but claims participant 2 (not its own).
            entry("node-f", 2, 99 * SEC, Some(hosted(Permission::Submission))),
            // x: nobody's peer; must not leak into any bucket by participant.
            entry("node-x", 5, 99 * SEC, Some(hosted(Permission::Submission))),
        ];
        let vetted: HashSet<CantonId> = [
            participant(2),
            participant(3),
            participant(4),
            participant(6),
        ]
        .into_iter()
        .collect();
        let snap = build_snapshot(&peers, &participant(1), &entries, &vetted, now, 3, 100);

        assert_eq!(
            snap.get(&participant(2)).map(|h| h.status),
            Some(PeerHealthStatus::Active)
        );
        assert_eq!(
            snap.get(&participant(3)).map(|h| h.status),
            Some(PeerHealthStatus::Stale)
        );
        assert_eq!(
            snap.get(&participant(4)).map(|h| h.status),
            Some(PeerHealthStatus::Unknown)
        );
        assert_eq!(
            snap.get(&participant(5)).map(|h| h.status),
            Some(PeerHealthStatus::Unvetted)
        );
        assert_eq!(
            snap.get(&participant(6)).map(|h| h.status),
            Some(PeerHealthStatus::Unknown)
        );
        assert!(snap.get(&participant(1)).is_none(), "self is never a peer");
        assert_eq!(
            snap.get(&participant(2)).and_then(|h| h.version.clone()),
            Some("2.0.0".to_string())
        );
    }

    #[test]
    fn preflight_distinguishes_unvetted_unknown_and_too_old() {
        let peers = vec![
            peer(2, Some("node-b")),
            peer(3, Some("node-c")),
            peer(4, Some("node-d")),
        ];
        let mut old = entry("node-b", 2, 99 * SEC, Some(hosted(Permission::Submission)));
        old.record.coordination_version = 0;
        let good = entry("node-c", 3, 99 * SEC, Some(hosted(Permission::Submission)));
        let vetted: HashSet<CantonId> = [participant(2), participant(3)].into_iter().collect();
        let snap = build_snapshot(
            &peers,
            &participant(1),
            &[old, good],
            &vetted,
            100 * SEC,
            3,
            100,
        );

        let unready = preflight_unready_peers(
            &snap,
            &[
                participant(2),
                participant(3),
                participant(4),
                participant(9),
            ],
        );
        let reasons: HashMap<CantonId, String> = unready.into_iter().collect();
        assert!(reasons[&participant(2)].contains("too old"));
        assert!(!reasons.contains_key(&participant(3)));
        assert!(reasons[&participant(4)].contains("has not vetted"));
        assert!(reasons[&participant(9)].contains("not in the peers table"));

        // A vetted peer with no entry names the two possible causes.
        let snap = build_snapshot(&peers, &participant(1), &[], &vetted, 100 * SEC, 3, 100);
        let unready = preflight_unready_peers(&snap, &[participant(2)]);
        assert!(unready[0].1.contains("no registry entry visible"));
    }

    #[test]
    fn registry_response_buckets_by_known_signatory() {
        let peers = vec![peer(2, Some("node-b"))];
        let entries = vec![
            entry("node-b", 2, 99 * SEC, Some(hosted(Permission::Submission))),
            entry("node-z", 9, 99 * SEC, None),
        ];
        let own = ActiveContract {
            contract_id: "00self".into(),
            offset: 0,
            record: node_record("node-a", &["node-b"]),
        };
        let resp = registry_response(Some(&own), &entries, &peers, 100 * SEC, 3);
        assert_eq!(
            resp.self_entry.as_ref().map(|v| v.contract_id.as_str()),
            Some("00self")
        );
        assert_eq!(resp.peers.len(), 1);
        assert!(resp.peers[0].hosting_verified);
        assert_eq!(resp.inbound.len(), 1);
        assert!(!resp.inbound[0].hosting_verified);
        assert_eq!(resp.peers[0].heartbeat_age_secs, 1);
        assert_eq!(resp.peers[0].last_active_at, 99);
    }

    #[tokio::test]
    async fn desired_record_names_only_vetted_peers_with_a_party() {
        let identity =
            crate::onledger::identity::tests::mock_identity("node-a", participant(1)).await;
        let mut config = NodeConfig::default();
        config.node.participant_id = Some(participant(1));
        // Peer 6 names a party that is not a Canton id at all.
        let mut malformed = peer(6, None);
        malformed.party = Some("not-a-canton-id".to_string());
        let peers = vec![
            peer(1, Some("node-a")), // self: never an observer
            peer(3, Some("node-c")), // vetted, named
            peer(2, Some("node-b")), // vetted, named; sorts before node-c
            peer(4, Some("node-d")), // not vetted
            peer(5, None),           // vetted, no party
            malformed,
        ];
        let vetted: HashSet<CantonId> = [
            participant(1),
            participant(2),
            participant(3),
            participant(5),
            participant(6),
        ]
        .into_iter()
        .collect();
        let desired = desired_node_record(&config, &identity, &peers, &vetted, 42);

        assert_eq!(desired.node, party("node-a"));
        assert_eq!(desired.participant_id, participant(1).to_string());
        assert_eq!(desired.peers, vec![party("node-b"), party("node-c")]);
        assert_eq!(desired.last_active_at, 42);
        assert_eq!(desired.coordination_version, consts::COORDINATION_VERSION);
        assert_eq!(desired.version, build_info::SEMVER);
        assert!(desired.min_heartbeat_interval_secs >= 1);
        assert!(desired.min_heartbeat_interval_secs <= desired.heartbeat_interval_secs);
        assert_eq!(desired.display_name, "participant1");
    }
}
