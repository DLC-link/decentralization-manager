//! Party topology proposals, signed without being published (#448).
//!
//! Add-party, kick and change-threshold change a party's
//! `DecentralizedNamespaceDefinition` (DNS) and `PartyToParticipant` (P2P)
//! together. `Authorize` against the synchronizer store proposes *and
//! distributes*, and it authorizes outright when this node alone holds enough
//! keys. At namespace threshold 1 that put the DNS in force the moment the
//! coordinator built it, while the paired P2P still waited for the members.
//!
//! The coordinator now authorizes both proposals in a temporary topology
//! store. Canton keeps such a store in the participant's memory, signs into
//! it without distributing anything, and forgets it when it is dropped. The
//! proposals reach the synchronizer only when submit publishes them. The one
//! exception is add-party's P2P, which its create step publishes as a pending
//! proposal for the new member: adding a host needs that host's signature, so
//! the proposal cannot take effect early.
//!
//! Canton accepts an explicit serial in a store only as the successor of that
//! store's head for the mapping, and a new store has no head. So the store is
//! first seeded with the party's history from the synchronizer, replayed in
//! order, until its DNS and P2P heads are the synchronizer's.
//!
//! A replay can fail to rebuild the heads. `signing_route` then decides by
//! the party's current namespace threshold. At threshold 1 the run stops with
//! nothing signed, because only there can the coordinator alone put the DNS in
//! force. Above it the coordinator signs with `Authorize` against the
//! synchronizer store, as before #448: its signature alone cannot meet the
//! threshold, so nothing takes effect early. A participant that cannot be
//! reached is never a replay failure; that error goes back to the step.
//!
//! Every store is dropped on every way out of [`sign_party_proposals`]: after
//! success, after an error, and from a drop guard when the task is aborted.
//! Store names carry the run's instance name, so a run that resumes after a
//! crash drops what its earlier attempt left. [`drop_leftover_stores`] sweeps
//! the rest at boot.

use std::{
    collections::BTreeSet,
    future::Future,
    time::{SystemTime, UNIX_EPOCH},
};

use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToParticipant, SignedTopologyTransaction,
        TopologyMapping, TopologyTransaction, enums, topology_mapping,
    },
    topology::admin::v30::{
        AddTransactionsRequest, AuthorizeRequest, AuthorizeResponse,
        CreateTemporaryTopologyStoreRequest, DropTemporaryTopologyStoreRequest, ForceFlag,
        ListAllRequest, ListAvailableStoresRequest, StoreId, authorize_request, store_id,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use tonic::transport::Channel;

use crate::{canton_id::CantonId, config::NodeConfig, error::Result, utils, workflow::topology};

/// What the replay into a temporary store came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RebuildOutcome {
    /// The store's DNS and P2P heads are the synchronizer's.
    Rebuilt,
    /// The history was read, but replaying it did not rebuild the heads.
    ReplayFailed,
    /// The participant could not be reached, so nothing is known about the
    /// history. The step's own retry handles this.
    Unreachable,
}

/// How the coordinator signs a party's proposals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    /// Sign in the rebuilt temporary store; nothing is published.
    TemporaryStore,
    /// Sign with `Authorize` against the synchronizer store, which publishes
    /// each proposal as it signs it.
    Synchronizer,
    /// Stop the run: nothing is signed or published.
    Refuse,
    /// Return the error to the step, which retries it.
    Retry,
}

/// Decide how to sign, from the party's current namespace threshold and what
/// the replay came to.
///
/// The temporary store is always the first choice. Only a failed replay falls
/// back, and only above threshold 1: there the coordinator's signature alone
/// cannot meet the threshold, so a proposal published early stays pending. At
/// threshold 1 (or a malformed lower one) it would take effect at once, so
/// the run stops. An unreachable participant says nothing about the history,
/// so it never falls back.
fn signing_route(head_threshold: i32, outcome: RebuildOutcome) -> Route {
    match outcome {
        RebuildOutcome::Rebuilt => Route::TemporaryStore,
        RebuildOutcome::Unreachable => Route::Retry,
        RebuildOutcome::ReplayFailed if head_threshold >= 2 => Route::Synchronizer,
        RebuildOutcome::ReplayFailed => Route::Refuse,
    }
}

/// Classify a replay result. An error is [`RebuildOutcome::Unreachable`] when
/// anything in its chain is a transport failure; every other error means the
/// replay itself failed.
fn rebuild_outcome(result: &Result<usize>) -> RebuildOutcome {
    match result {
        Ok(_) => RebuildOutcome::Rebuilt,
        Err(e) if is_unreachable(e) => RebuildOutcome::Unreachable,
        Err(_) => RebuildOutcome::ReplayFailed,
    }
}

/// Whether an error comes from the connection to the participant rather than
/// from Canton refusing what it was asked.
fn is_unreachable(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<tonic::transport::Error>().is_some()
            || cause
                .downcast_ref::<tonic::Status>()
                .is_some_and(|status| is_transport_code(status.code()))
    })
}

/// gRPC codes a connection problem surfaces as. `Unknown` is among them
/// because tonic reports some broken connections that way, and an ambiguous
/// error must retry rather than fall back.
fn is_transport_code(code: tonic::Code) -> bool {
    matches!(
        code,
        tonic::Code::Unavailable
            | tonic::Code::DeadlineExceeded
            | tonic::Code::Cancelled
            | tonic::Code::Unknown
            | tonic::Code::ResourceExhausted
            | tonic::Code::Aborted
    )
}

/// Every temporary store this tool creates is named with this prefix, so a
/// sweep never touches a store something else created.
pub const STORE_PREFIX: &str = "decman-proposals-";

/// Canton caps a temporary store name at 185 characters. The instance part is
/// cut well short of that, leaving room for the prefix and the attempt suffix.
const MAX_INSTANCE_CHARS: usize = 120;

/// Mapping codes a delegation read excludes, so it returns only the
/// `NamespaceDelegation`s. A participant's namespace also holds its vetting,
/// its key mappings and every local party it hosts, none of which the replay
/// needs. Only codes every Canton 3.x release knows are listed, because an
/// unknown code fails the whole read.
const NOT_DELEGATIONS: [&str; 12] = [
    "dnd", "otk", "dtc", "pdp", "phl", "vtp", "ptp", "dop", "mds", "sds", "sep", "ptk",
];

/// A party's namespace definition and participant mapping as the synchronizer
/// holds them, each with the serial of its head transaction.
#[derive(Clone, Debug)]
pub struct PartyHead {
    pub dns_serial: u32,
    pub dns: DecentralizedNamespaceDefinition,
    pub p2p_serial: u32,
    pub p2p: PartyToParticipant,
}

impl PartyHead {
    /// Refuse to build on a namespace that changed after `ExportState` read
    /// it.
    ///
    /// The proposals are built from the exported definition but pinned to the
    /// serial read now. If the two disagree, the proposal would replace a
    /// change this run never looked at.
    ///
    /// # Errors
    ///
    /// Errors when the head namespace differs from `exported`.
    pub fn ensure_namespace_is(&self, exported: &DecentralizedNamespaceDefinition) -> Result {
        if same_namespace(&self.dns, exported) {
            return Ok(());
        }
        anyhow::bail!(
            "The namespace {namespace} changed after this run read it: it now has threshold \
             {now_threshold} and {now_owners} owner(s) at serial {serial}, against threshold \
             {then_threshold} and {then_owners} owner(s) when the run started. Start the \
             workflow again so it is built on the current state",
            namespace = exported.decentralized_namespace,
            now_threshold = self.dns.threshold,
            now_owners = self.dns.owners.len(),
            serial = self.dns_serial,
            then_threshold = exported.threshold,
            then_owners = exported.owners.len(),
        )
    }
}

/// Whether two namespace definitions are the same. Owners are a set to
/// Canton, so their order does not count.
fn same_namespace(
    a: &DecentralizedNamespaceDefinition,
    b: &DecentralizedNamespaceDefinition,
) -> bool {
    a.decentralized_namespace == b.decentralized_namespace
        && a.threshold == b.threshold
        && a.owners.iter().collect::<BTreeSet<_>>() == b.owners.iter().collect::<BTreeSet<_>>()
}

/// Read the party's [`PartyHead`] from the synchronizer.
///
/// # Errors
///
/// Errors when either mapping is missing or a serial is out of range.
pub async fn fetch_party_head(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &CantonId,
) -> Result<PartyHead> {
    let (dns_serial, dns) = topology::fetch_namespace_definition_at_head(
        config,
        synchronizer_id,
        &party_id.namespace.to_hex(),
    )
    .await?;
    let (p2p_serial, p2p) =
        topology::fetch_p2p_mapping_at_head(config, synchronizer_id, party_id).await?;
    Ok(PartyHead {
        dns_serial,
        dns,
        p2p_serial,
        p2p,
    })
}

/// The coordinator's signed proposals, not yet published.
pub struct SignedProposals {
    /// `None` when the run leaves the namespace as it is.
    pub dns: Option<SignedTopologyTransaction>,
    pub p2p: SignedTopologyTransaction,
}

/// Sign the party's new DNS (when `new_dns` is set) and new P2P at the serial
/// after `head`'s.
///
/// `head` must be what the proposals were built from. The proposals are
/// signed in a temporary store seeded until its heads equal `head`'s serials,
/// and nothing is published. When the replay cannot rebuild those heads,
/// `signing_route` decides: above namespace threshold 1 the proposals are
/// signed and published with `Authorize` against the synchronizer store, as
/// before #448; at threshold 1 the run stops.
///
/// # Errors
///
/// Errors when the store cannot be created, when the participant cannot be
/// reached, when a threshold-1 party's history does not replay, or when
/// Canton refuses to sign.
#[allow(clippy::too_many_arguments)]
pub async fn sign_party_proposals(
    config: &NodeConfig,
    synchronizer_id: &str,
    instance_name: &str,
    party_id: &CantonId,
    head: &PartyHead,
    new_dns: Option<DecentralizedNamespaceDefinition>,
    new_p2p: PartyToParticipant,
    force_flags: Vec<i32>,
) -> Result<SignedProposals> {
    let protocol_version = protocol_version_of(synchronizer_id)?;
    let serials = (
        next_serial("DNS", head.dns_serial)?,
        next_serial("P2P", head.p2p_serial)?,
    );
    let channel = config.admin_channel().await?;
    let stores = CantonStores {
        channel: channel.clone(),
    };
    let proposals = (new_dns, new_p2p);

    let attempt = {
        let config = config.clone();
        let synchronizer_id = synchronizer_id.to_string();
        let party_id = party_id.clone();
        let head = head.clone();
        let proposals = proposals.clone();
        let force_flags = force_flags.clone();
        in_store(
            stores,
            instance_name,
            protocol_version,
            move |store| async move {
                let rebuilt = async {
                    let replayed = seed(&channel, &synchronizer_id, &store, &party_id).await?;
                    ensure_seeded_to(&config, &store, &party_id, &head).await?;
                    Ok(replayed)
                }
                .await;
                let route = signing_route(head.dns.threshold, rebuild_outcome(&rebuilt));
                match (route, rebuilt) {
                    (Route::TemporaryStore, Ok(replayed)) => {
                        tracing::info!(
                            replayed,
                            %party_id,
                            %store,
                            dns_serial = serials.0,
                            p2p_serial = serials.1,
                            "Replayed the party's topology history into a temporary store; \
                             signing there"
                        );
                        let signed = sign(
                            &channel,
                            &temporary_store_id(&store),
                            proposals,
                            serials,
                            force_flags,
                        )
                        .await?;
                        Ok(Attempt::Signed(signed))
                    }
                    (Route::Synchronizer, Err(e)) => Ok(Attempt::FallBack(format!("{e:#}"))),
                    (Route::Refuse, Err(e)) => Err(e.context(format!(
                        "Could not rebuild {party_id}'s topology history in a temporary store, \
                         and its namespace threshold is {threshold}. Signing against the \
                         synchronizer would put the namespace change in force on this node's \
                         signature alone, so nothing was signed or published",
                        threshold = head.dns.threshold
                    ))),
                    // `Route::Retry`: the participant could not be reached.
                    (_, Err(e)) => Err(e),
                    (route, Ok(_)) => anyhow::bail!("{route:?} chosen for a rebuilt store"),
                }
            },
        )
        .await?
    };

    match attempt {
        Attempt::Signed(signed) => Ok(signed),
        Attempt::FallBack(reason) => {
            tracing::warn!(
                %party_id,
                threshold = head.dns.threshold,
                reason,
                "Could not rebuild the party's topology history in a temporary store; signing \
                 against the synchronizer store instead. Above threshold 1 the proposals stay \
                 pending until the members sign"
            );
            let store = topology::synchronizer_store_id(synchronizer_id);
            let signed =
                sign_on_synchronizer(config, &store, proposals, serials, force_flags).await?;
            Ok(signed)
        }
    }
}

/// What signing in the temporary store came to.
enum Attempt {
    Signed(SignedProposals),
    /// The replay failed above threshold 1; the reason, for the log.
    FallBack(String),
}

/// Sign the DNS (when there is one) and then the P2P in `store`.
async fn sign(
    channel: &Channel,
    store: &StoreId,
    (new_dns, new_p2p): (Option<DecentralizedNamespaceDefinition>, PartyToParticipant),
    (dns_serial, p2p_serial): (u32, u32),
    force_flags: Vec<i32>,
) -> Result<SignedProposals> {
    let dns = match new_dns {
        Some(definition) => Some(
            authorize(
                channel,
                proposal_request(
                    store.clone(),
                    topology_mapping::Mapping::DecentralizedNamespaceDefinition(definition),
                    dns_serial,
                    force_flags.clone(),
                ),
            )
            .await?,
        ),
        None => None,
    };
    let p2p = authorize(
        channel,
        proposal_request(
            store.clone(),
            topology_mapping::Mapping::PartyToParticipant(new_p2p),
            p2p_serial,
            force_flags,
        ),
    )
    .await?;
    Ok(SignedProposals { dns, p2p })
}

/// Sign the pair against the synchronizer store, as the coordinator did before
/// #448. `Authorize` there publishes each proposal as it signs it. The serials
/// stay pinned to `head` + 1, so a head that moved in the meantime is refused
/// rather than replaced.
async fn sign_on_synchronizer(
    config: &NodeConfig,
    store: &StoreId,
    (new_dns, new_p2p): (Option<DecentralizedNamespaceDefinition>, PartyToParticipant),
    (dns_serial, p2p_serial): (u32, u32),
    force_flags: Vec<i32>,
) -> Result<SignedProposals> {
    let signed = |response: AuthorizeResponse| {
        response
            .transaction
            .ok_or_else(|| anyhow::anyhow!("Authorize returned no transaction"))
    };
    let dns = match new_dns {
        Some(definition) => Some(signed(
            topology::authorize_with_topology_retry(
                config,
                proposal_request(
                    store.clone(),
                    topology_mapping::Mapping::DecentralizedNamespaceDefinition(definition),
                    dns_serial,
                    force_flags.clone(),
                ),
                "DNS proposal",
            )
            .await?,
        )?),
        None => None,
    };
    let p2p = signed(
        topology::authorize_with_topology_retry(
            config,
            proposal_request(
                store.clone(),
                topology_mapping::Mapping::PartyToParticipant(new_p2p),
                p2p_serial,
                force_flags,
            ),
            "P2P proposal",
        )
        .await?,
    )?;
    Ok(SignedProposals { dns, p2p })
}

/// Drop every temporary store an earlier process of this tool left behind.
///
/// A store outlives its run only when the process dies between creating and
/// dropping it. Canton keeps temporary stores in memory, so a participant
/// restart clears them too, but a participant can run for months. Call this
/// once at boot, before any run resumes: a sweep that ran alongside a resumed
/// run would drop that run's store.
///
/// Assumes one instance of this tool per participant, which is how it is
/// deployed.
///
/// # Errors
///
/// Errors when the participant's stores cannot be listed.
pub async fn drop_leftover_stores(config: &NodeConfig) -> Result<usize> {
    let stores = CantonStores {
        channel: config.admin_channel().await?,
    };
    drop_where(&stores, |name| name.starts_with(STORE_PREFIX)).await
}

// ---------------------------------------------------------------------------
// Store lifecycle
// ---------------------------------------------------------------------------

/// The admin calls a temporary store's lifecycle needs. The Canton admin API
/// is the only production implementation; tests substitute a recorder.
trait StoreAdmin: Clone + Send + Sync + 'static {
    fn temporary_store_names(&self) -> impl Future<Output = Result<Vec<String>>> + Send;
    fn create(&self, name: &str, protocol_version: u32) -> impl Future<Output = Result> + Send;
    fn drop_store(&self, name: &str) -> impl Future<Output = Result> + Send;
}

#[derive(Clone)]
struct CantonStores {
    channel: Channel,
}

impl StoreAdmin for CantonStores {
    async fn temporary_store_names(&self) -> Result<Vec<String>> {
        let response = TopologyManagerReadServiceClient::new(self.channel.clone())
            .list_available_stores(tonic::Request::new(ListAvailableStoresRequest {}))
            .await?
            .into_inner();
        Ok(response
            .store_ids
            .into_iter()
            .filter_map(|id| match id.store {
                Some(store_id::Store::Temporary(temporary)) => Some(temporary.name),
                _ => None,
            })
            .collect())
    }

    async fn create(&self, name: &str, protocol_version: u32) -> Result {
        TopologyManagerWriteServiceClient::new(self.channel.clone())
            .create_temporary_topology_store(tonic::Request::new(
                CreateTemporaryTopologyStoreRequest {
                    name: name.to_string(),
                    protocol_version,
                },
            ))
            .await?;
        Ok(())
    }

    async fn drop_store(&self, name: &str) -> Result {
        TopologyManagerWriteServiceClient::new(self.channel.clone())
            .drop_temporary_topology_store(tonic::Request::new(DropTemporaryTopologyStoreRequest {
                store_id: Some(store_id::Temporary {
                    name: name.to_string(),
                }),
            }))
            .await?;
        Ok(())
    }
}

/// A temporary store that is dropped however its owner exits.
///
/// [`ProposalStore::close`] drops it on the paths that return. A task aborted
/// mid-run (a cancelled workflow) never reaches `close`, so `Drop` hands the
/// drop to the runtime instead.
struct ProposalStore<A: StoreAdmin> {
    admin: A,
    name: String,
    open: bool,
}

impl<A: StoreAdmin> ProposalStore<A> {
    /// Drop what an earlier attempt of the same run left, then create a fresh
    /// store for this attempt.
    async fn open(admin: A, instance_name: &str, protocol_version: u32) -> Result<Self> {
        let earlier = instance_prefix(instance_name);
        if let Err(e) = drop_where(&admin, |name| name.starts_with(&earlier)).await {
            tracing::warn!(
                instance_name,
                error = format!("{e:#}"),
                "Could not look for temporary topology stores an earlier attempt of this run \
                 left behind"
            );
        }
        let name = store_name(instance_name, attempt_suffix());
        admin.create(&name, protocol_version).await?;
        tracing::debug!(store = %name, "Created temporary topology store");
        Ok(Self {
            admin,
            name,
            open: true,
        })
    }

    async fn close(mut self) {
        let result = self.admin.drop_store(&self.name).await;
        // Only now: a task aborted while the drop is in flight still leaves
        // the guard armed.
        self.open = false;
        match result {
            Ok(()) => tracing::debug!(store = %self.name, "Dropped temporary topology store"),
            Err(e) => tracing::warn!(
                store = %self.name,
                error = format!("{e:#}"),
                "Could not drop temporary topology store; the next attempt of this run, or the \
                 next boot, drops it"
            ),
        }
    }
}

impl<A: StoreAdmin> Drop for ProposalStore<A> {
    fn drop(&mut self) {
        if !self.open {
            return;
        }
        let admin = self.admin.clone();
        let name = std::mem::take(&mut self.name);
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    match admin.drop_store(&name).await {
                        Ok(()) => tracing::info!(
                            store = %name,
                            "Dropped temporary topology store after its run stopped"
                        ),
                        Err(e) => tracing::warn!(
                            store = %name,
                            error = format!("{e:#}"),
                            "Could not drop temporary topology store after its run stopped; \
                             the next boot drops it"
                        ),
                    }
                });
            }
            Err(_) => tracing::warn!(
                store = %name,
                "Temporary topology store outlived its runtime; the next boot drops it"
            ),
        }
    }
}

/// Run `body` against a fresh temporary store for `instance_name`, and drop
/// the store whatever `body` returns.
async fn in_store<A, T, F, Fut>(
    admin: A,
    instance_name: &str,
    protocol_version: u32,
    body: F,
) -> Result<T>
where
    A: StoreAdmin,
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let store = ProposalStore::open(admin, instance_name, protocol_version).await?;
    let result = body(store.name.clone()).await;
    store.close().await;
    result
}

/// Drop every temporary store whose name `matches`, and return how many went.
/// A store that will not drop is logged and skipped, so one bad store does not
/// keep the rest.
async fn drop_where<A: StoreAdmin>(admin: &A, matches: impl Fn(&str) -> bool) -> Result<usize> {
    let mut dropped = 0;
    for name in admin.temporary_store_names().await? {
        if !matches(&name) {
            continue;
        }
        match admin.drop_store(&name).await {
            Ok(()) => {
                tracing::info!(store = %name, "Dropped leftover temporary topology store");
                dropped += 1;
            }
            Err(e) => {
                tracing::warn!(
                    store = %name,
                    error = format!("{e:#}"),
                    "Could not drop leftover temporary topology store"
                )
            }
        }
    }
    Ok(dropped)
}

/// The name every store of `instance_name` starts with.
fn instance_prefix(instance_name: &str) -> String {
    let instance: String = instance_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .take(MAX_INSTANCE_CHARS)
        .collect();
    format!("{STORE_PREFIX}{instance}~")
}

/// A store name for one attempt of `instance_name`.
///
/// Each attempt gets its own name. A drop the guard spawned for an aborted
/// attempt can then never land on the store of the attempt after it.
fn store_name(instance_name: &str, attempt: u128) -> String {
    format!("{prefix}{attempt}", prefix = instance_prefix(instance_name))
}

fn attempt_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default()
}

fn temporary_store_id(name: &str) -> StoreId {
    StoreId {
        store: Some(store_id::Store::Temporary(store_id::Temporary {
            name: name.to_string(),
        })),
    }
}

/// The protocol version a physical synchronizer id ends with, e.g. `35` from
/// `global-domain::1220ab::35-0`.
///
/// A temporary store is created at the protocol version of the store its
/// transactions are published to.
///
/// # Errors
///
/// Errors when the id does not end in a protocol version.
fn protocol_version_of(synchronizer_id: &str) -> Result<u32> {
    synchronizer_id
        .rsplit_once("::")
        .and_then(|(_, suffix)| suffix.split('-').next())
        .and_then(|version| version.parse::<u32>().ok())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Cannot read a protocol version from physical synchronizer id {synchronizer_id}"
            )
        })
}

fn next_serial(what: &str, head: u32) -> Result<u32> {
    head.checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("The {what} serial {head} has no successor"))
}

// ---------------------------------------------------------------------------
// Signing
// ---------------------------------------------------------------------------

/// An `Authorize` that proposes `mapping` at `serial` in `store`.
///
/// `must_fully_authorize` stays false: this node's signature alone is not the
/// party's authorization, and the peers' signatures are merged in before
/// submit. The serial is explicit: a temporary store is not where the
/// transaction is published, and against the synchronizer store it refuses a
/// head that moved.
fn proposal_request(
    store: StoreId,
    mapping: topology_mapping::Mapping,
    serial: u32,
    force_flags: Vec<i32>,
) -> AuthorizeRequest {
    AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            authorize_request::Proposal {
                change: enums::TopologyChangeOp::AddReplace as i32,
                serial,
                mapping: Some(authorize_request::proposal::Mapping::V30(TopologyMapping {
                    mapping: Some(mapping),
                })),
            },
        )),
        must_fully_authorize: false,
        force_changes: force_flags,
        signed_by: vec![],
        store: Some(store),
        wait_to_become_effective: None,
    }
}

/// Sign in the temporary store.
///
/// No retry on `TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE`, unlike the
/// synchronizer-store paths: the store holds exactly what the replay put in,
/// so a missing key will not appear by waiting.
async fn authorize(
    channel: &Channel,
    request: AuthorizeRequest,
) -> Result<SignedTopologyTransaction> {
    TopologyManagerWriteServiceClient::new(channel.clone())
        .authorize(tonic::Request::new(request))
        .await?
        .into_inner()
        .transaction
        .ok_or_else(|| anyhow::anyhow!("Authorize returned no transaction"))
}

// ---------------------------------------------------------------------------
// Seeding
// ---------------------------------------------------------------------------

/// Where a transaction falls in the replay. Delegations go first among
/// transactions that took effect together, roots before the keys they
/// delegate to, and a namespace before the party mapping it authorizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    RootDelegation,
    Delegation,
    Namespace,
    PartyMapping,
}

/// One transaction to replay.
#[derive(Clone, Debug)]
struct Replay {
    /// Effective time as `(seconds, nanos)`; `None` sorts first.
    valid_from: Option<(i64, i32)>,
    rank: Rank,
    serial: u32,
    signed: SignedTopologyTransaction,
}

/// One transaction from the synchronizer's history.
struct HistoryItem {
    valid_from: Option<prost_types::Timestamp>,
    signed: SignedTopologyTransaction,
}

/// Replay the party's history into `store`, and return how many transactions
/// the store accepted.
///
/// The head transactions alone do not do. Without a predecessor in the store,
/// Canton judges a transaction as a creation: a DNS then needs every owner's
/// signature, and a P2P every hosting participant's. The heads usually carry
/// only a quorum. Replaying the whole history in the order it took effect
/// lets each transaction be judged against the state it was signed for.
///
/// A transaction the store refuses is logged and skipped;
/// [`ensure_seeded_to`] decides afterwards whether the heads came out right.
async fn seed(
    channel: &Channel,
    synchronizer_id: &str,
    store: &str,
    party_id: &CantonId,
) -> Result<usize> {
    let party_items =
        list_history(channel, synchronizer_id, &party_id.namespace.to_hex(), &[]).await?;
    let (mut replays, namespaces) = party_replays(party_id, party_items)?;
    for namespace in &namespaces {
        let items = list_history(channel, synchronizer_id, namespace, &NOT_DELEGATIONS).await?;
        replays.extend(delegation_replays(namespace, items)?);
    }

    let mut writer = TopologyManagerWriteServiceClient::new(channel.clone());
    let mut accepted = 0;
    for signed in replay_order(replays) {
        match writer
            .add_transactions(tonic::Request::new(AddTransactionsRequest {
                transactions: vec![signed],
                force_changes: replay_force_flags(),
                store: Some(temporary_store_id(store)),
                wait_to_become_effective: None,
            }))
            .await
        {
            Ok(_) => accepted += 1,
            // A lost connection is not a refusal, and must not be read as a
            // history that does not replay.
            Err(status) if is_transport_code(status.code()) => return Err(status.into()),
            // Unexpected, since the synchronizer accepted every one of these.
            // The run can still go on if the heads come out right.
            Err(status) => tracing::warn!(
                store,
                %party_id,
                %status,
                "A temporary store refused a replayed topology transaction"
            ),
        }
    }
    Ok(accepted)
}

/// Force flags for the replay.
///
/// Canton runs its "is this dangerous" checks again on every replayed
/// transaction, relative to this node and its ledger state today rather than
/// to the node that first submitted it. Replaying a former unhosting of this
/// node, for instance, would otherwise trip the active-contracts check. The
/// replay only rebuilds history into a store that is dropped afterwards.
fn replay_force_flags() -> Vec<i32> {
    vec![
        ForceFlag::DisablePartyWithActiveContracts as i32,
        ForceFlag::AllowInsufficientParticipantPermissionForSignatoryParty as i32,
        ForceFlag::AllowInsufficientSignatoryAssigningParticipantsForParty as i32,
        ForceFlag::AllowConfirmingThresholdCannotBeMet as i32,
    ]
}

/// Read every transaction the synchronizer ever held under `namespace`,
/// leaving out mappings with a code in `exclude`.
async fn list_history(
    channel: &Channel,
    synchronizer_id: &str,
    namespace: &str,
    exclude: &[&str],
) -> Result<Vec<HistoryItem>> {
    // `ListAll` is the variant Canton 3.5 serves; its successor is 3.6-only.
    #[allow(deprecated)]
    let response = TopologyManagerReadServiceClient::new(channel.clone())
        .list_all(tonic::Request::new(ListAllRequest {
            base_query: Some(topology::history_query(synchronizer_id)),
            exclude_mappings: exclude.iter().map(|code| (*code).to_string()).collect(),
            filter_namespace: namespace.to_string(),
        }))
        .await?
        .into_inner();
    response
        .result
        .map(|result| result.items)
        .unwrap_or_default()
        .into_iter()
        // A rejected transaction never took effect, so it is not history.
        .filter(|item| item.rejection_reason.is_none())
        .map(|item| {
            Ok(HistoryItem {
                valid_from: item.valid_from,
                signed: utils::decode_versioned(&item.transaction)?,
            })
        })
        .collect()
}

/// The party's DNS and P2P transactions from `items`, and every namespace
/// that signed for them: the owners of every DNS version and the hosting
/// participants of every P2P version.
///
/// A namespace can hold several parties and a filtered read matches on a
/// prefix, so every mapping is pinned to this party exactly.
fn party_replays(
    party_id: &CantonId,
    items: Vec<HistoryItem>,
) -> Result<(Vec<Replay>, BTreeSet<String>)> {
    let namespace = party_id.namespace.to_hex();
    let party = party_id.to_string();
    let mut replays = Vec::new();
    let mut namespaces = BTreeSet::new();
    for item in items {
        let transaction: TopologyTransaction = utils::decode_versioned(&item.signed.transaction)?;
        let rank = match transaction
            .mapping
            .as_ref()
            .and_then(|m| m.mapping.as_ref())
        {
            Some(topology_mapping::Mapping::DecentralizedNamespaceDefinition(def))
                if def.decentralized_namespace == namespace =>
            {
                namespaces.extend(def.owners.iter().cloned());
                Rank::Namespace
            }
            Some(topology_mapping::Mapping::PartyToParticipant(p2p)) if p2p.party == party => {
                namespaces.extend(
                    p2p.participants
                        .iter()
                        .filter_map(|host| participant_namespace(&host.participant_uid)),
                );
                Rank::PartyMapping
            }
            _ => continue,
        };
        replays.push(Replay::new(item, rank, transaction.serial));
    }
    Ok((replays, namespaces))
}

/// The `NamespaceDelegation`s for exactly `namespace` from `items`.
fn delegation_replays(namespace: &str, items: Vec<HistoryItem>) -> Result<Vec<Replay>> {
    let mut replays = Vec::new();
    for item in items {
        let transaction: TopologyTransaction = utils::decode_versioned(&item.signed.transaction)?;
        let Some(topology_mapping::Mapping::NamespaceDelegation(delegation)) = transaction
            .mapping
            .as_ref()
            .and_then(|m| m.mapping.as_ref())
        else {
            continue;
        };
        if delegation.namespace != namespace {
            continue;
        }
        let root = delegation
            .target_key
            .as_ref()
            .is_some_and(|key| utils::compute_fingerprint(key) == namespace);
        let rank = if root {
            Rank::RootDelegation
        } else {
            Rank::Delegation
        };
        replays.push(Replay::new(item, rank, transaction.serial));
    }
    Ok(replays)
}

impl Replay {
    fn new(item: HistoryItem, rank: Rank, serial: u32) -> Self {
        Self {
            valid_from: item.valid_from.map(|t| (t.seconds, t.nanos)),
            rank,
            serial,
            signed: item.signed,
        }
    }
}

/// The order to replay in: by effective time, then [`Rank`], then serial.
fn replay_order(mut replays: Vec<Replay>) -> Vec<SignedTopologyTransaction> {
    replays.sort_by_key(|replay| (replay.valid_from, replay.rank, replay.serial));
    replays.into_iter().map(|replay| replay.signed).collect()
}

/// The namespace of a participant uid `name::namespace`.
fn participant_namespace(participant_uid: &str) -> Option<String> {
    participant_uid
        .rsplit_once("::")
        .map(|(_, namespace)| namespace.to_string())
        .filter(|namespace| !namespace.is_empty())
}

/// Refuse to sign unless the replay rebuilt the heads the proposals were
/// built on.
///
/// # Errors
///
/// Errors when either head in the store is missing or at another serial.
async fn ensure_seeded_to(
    config: &NodeConfig,
    store: &str,
    party_id: &CantonId,
    head: &PartyHead,
) -> Result {
    let dns = topology::fetch_namespace_definition_at_head_in(
        config,
        temporary_store_id(store),
        &party_id.namespace.to_hex(),
    )
    .await
    .map(|(serial, _)| serial);
    let p2p = topology::fetch_p2p_mapping_at_head_in(config, temporary_store_id(store), party_id)
        .await
        .map(|(serial, _)| serial);
    // A read that could not reach the participant says nothing about the
    // store, so it goes back as it is rather than as a failed replay.
    let unreachable = |read: Result<u32>| match read {
        Err(e) if is_unreachable(&e) => {
            Err(e.context(format!("Could not read the temporary store {store}")))
        }
        other => Ok(other),
    };
    let dns = unreachable(dns)?;
    let p2p = unreachable(p2p)?;
    match (dns, p2p) {
        (Ok(dns), Ok(p2p)) if dns == head.dns_serial && p2p == head.p2p_serial => Ok(()),
        (dns, p2p) => anyhow::bail!(
            "Could not rebuild {party_id}'s topology in a temporary store: it holds DNS serial \
             {dns} and P2P serial {p2p}, the synchronizer DNS serial {want_dns} and P2P serial \
             {want_p2p}. Nothing was signed or published",
            dns = describe(&dns),
            p2p = describe(&p2p),
            want_dns = head.dns_serial,
            want_p2p = head.p2p_serial,
        ),
    }
}

fn describe(serial: &Result<u32>) -> String {
    match serial {
        Ok(serial) => serial.to_string(),
        Err(e) => format!("none ({e:#})"),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    use canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::SigningPublicKey,
        protocol::v30::{NamespaceDelegation, party_to_participant::HostingParticipant},
        version::v1::{UntypedVersionedMessage, untyped_versioned_message},
    };
    use prost::Message;

    use super::*;

    /// A participant's temporary stores, kept in memory, with every call
    /// recorded in order.
    #[derive(Clone, Default)]
    struct Recorder {
        stores: Arc<Mutex<BTreeSet<String>>>,
        calls: Arc<Mutex<Vec<String>>>,
        refuse_create: bool,
    }

    impl Recorder {
        fn with_stores(names: &[&str]) -> Self {
            let recorder = Self::default();
            if let Ok(mut stores) = recorder.stores.lock() {
                stores.extend(names.iter().map(|name| (*name).to_string()));
            }
            recorder
        }

        fn stores(&self) -> Result<Vec<String>> {
            Ok(self
                .stores
                .lock()
                .map_err(|_| anyhow::anyhow!("poisoned"))?
                .iter()
                .cloned()
                .collect())
        }

        fn calls(&self) -> Result<Vec<String>> {
            Ok(self
                .calls
                .lock()
                .map_err(|_| anyhow::anyhow!("poisoned"))?
                .clone())
        }

        fn record(&self, call: String) -> Result {
            self.calls
                .lock()
                .map_err(|_| anyhow::anyhow!("poisoned"))?
                .push(call);
            Ok(())
        }
    }

    impl StoreAdmin for Recorder {
        async fn temporary_store_names(&self) -> Result<Vec<String>> {
            self.stores()
        }

        async fn create(&self, name: &str, _protocol_version: u32) -> Result {
            self.record(format!("create {name}"))?;
            anyhow::ensure!(!self.refuse_create, "create refused");
            let mut stores = self
                .stores
                .lock()
                .map_err(|_| anyhow::anyhow!("poisoned"))?;
            anyhow::ensure!(stores.insert(name.to_string()), "store {name} exists");
            Ok(())
        }

        async fn drop_store(&self, name: &str) -> Result {
            self.record(format!("drop {name}"))?;
            let mut stores = self
                .stores
                .lock()
                .map_err(|_| anyhow::anyhow!("poisoned"))?;
            anyhow::ensure!(stores.remove(name), "store {name} unknown");
            Ok(())
        }
    }

    const INSTANCE: &str = "party-change-threshold-1760000000";

    #[tokio::test]
    async fn the_store_is_dropped_after_the_body_succeeds() -> Result {
        let recorder = Recorder::default();
        let used = in_store(
            recorder.clone(),
            INSTANCE,
            35,
            |store| async move { Ok(store) },
        )
        .await?;

        assert!(recorder.stores()?.is_empty(), "{:?}", recorder.stores()?);
        assert_eq!(
            recorder.calls()?,
            [format!("create {used}"), format!("drop {used}")]
        );
        Ok(())
    }

    #[tokio::test]
    async fn the_store_is_dropped_when_the_body_fails() -> Result {
        let recorder = Recorder::default();
        let result: Result<()> = in_store(recorder.clone(), INSTANCE, 35, |_| async {
            anyhow::bail!("Canton refused to sign")
        })
        .await;

        let error = result
            .err()
            .ok_or_else(|| anyhow::anyhow!("body error was swallowed"))?;
        assert!(format!("{error}").contains("refused to sign"), "{error}");
        assert!(recorder.stores()?.is_empty(), "{:?}", recorder.stores()?);
        Ok(())
    }

    /// A cancelled workflow aborts its task mid-body, so the code after the
    /// body never runs. The guard has to drop the store on its own.
    #[tokio::test]
    async fn the_store_is_dropped_when_the_run_is_cancelled() -> Result {
        let recorder = Recorder::default();
        let created = Arc::new(tokio::sync::Notify::new());
        let task = tokio::spawn({
            let (recorder, created) = (recorder.clone(), created.clone());
            async move {
                in_store(recorder, INSTANCE, 35, |_| async move {
                    created.notify_one();
                    std::future::pending::<Result>().await
                })
                .await
            }
        });

        created.notified().await;
        assert_eq!(recorder.stores()?.len(), 1, "the body runs with its store");
        task.abort();
        assert!(task.await.is_err_and(|e| e.is_cancelled()));

        tokio::time::timeout(Duration::from_secs(5), async {
            while !recorder.stores().map(|s| s.is_empty()).unwrap_or(false) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("the aborted run's store was never dropped"))?;
        Ok(())
    }

    /// A run that crashed while signing resumes at the same step. Its new
    /// attempt drops what the old one left, and only that.
    #[tokio::test]
    async fn opening_drops_only_what_an_earlier_attempt_of_the_run_left() -> Result {
        let earlier = store_name(INSTANCE, 1);
        let sibling = store_name("party-change-threshold-17600000001", 1);
        let foreign = "someone-elses-store";
        let recorder = Recorder::with_stores(&[&earlier, &sibling, foreign]);

        in_store(recorder.clone(), INSTANCE, 35, |_| async { Ok(()) }).await?;

        assert_eq!(
            recorder.stores()?,
            [sibling.clone(), foreign.to_string()],
            "only {earlier} may go"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_refused_create_leaves_nothing_to_drop() -> Result {
        let recorder = Recorder {
            refuse_create: true,
            ..Recorder::default()
        };
        let result = in_store(recorder.clone(), INSTANCE, 35, |_| async { Ok(()) }).await;

        assert!(result.is_err());
        assert!(
            recorder
                .calls()?
                .iter()
                .all(|call| !call.starts_with("drop")),
            "{:?}",
            recorder.calls()?
        );
        Ok(())
    }

    #[tokio::test]
    async fn the_boot_sweep_drops_only_this_tools_stores() -> Result {
        let ours = [store_name(INSTANCE, 1), store_name("other-kick-1", 2)];
        let recorder = Recorder::with_stores(&[&ours[0], &ours[1], "an-operators-store"]);

        let dropped = drop_where(&recorder, |name| name.starts_with(STORE_PREFIX)).await?;

        assert_eq!(dropped, 2);
        assert_eq!(recorder.stores()?, ["an-operators-store".to_string()]);
        Ok(())
    }

    #[test]
    fn store_names_fit_canton_and_name_their_run() {
        let long = format!("{}-kick-1760000000", "p".repeat(400));
        let name = store_name(&long, u128::MAX);
        assert!(name.len() <= 185, "{} characters", name.len());
        assert!(name.starts_with(STORE_PREFIX));

        let odd = store_name("party with spaces/and::colons", 7);
        assert_eq!(
            odd,
            format!("{STORE_PREFIX}party-with-spaces-and--colons~7")
        );
    }

    /// The separator keeps one run's prefix from matching a run whose
    /// instance name merely starts the same way.
    #[test]
    fn a_runs_prefix_does_not_match_a_longer_instance_name() {
        let prefix = instance_prefix("party-kick-1");
        assert!(store_name("party-kick-1", 5).starts_with(&prefix));
        assert!(!store_name("party-kick-12", 5).starts_with(&prefix));
    }

    #[test]
    fn reads_the_protocol_version_off_a_physical_synchronizer_id() -> Result {
        assert_eq!(protocol_version_of("global-domain::1220ab::35-0")?, 35);
        assert_eq!(protocol_version_of("global-domain::1220ab::34-12")?, 34);
        assert!(protocol_version_of("global-domain::1220ab").is_err());
        assert!(protocol_version_of("no-separator").is_err());
        Ok(())
    }

    #[test]
    fn a_serial_at_the_ceiling_has_no_successor() -> Result {
        assert_eq!(next_serial("DNS", 4)?, 5);
        assert!(next_serial("DNS", u32::MAX).is_err());
        Ok(())
    }

    #[test]
    fn proposals_are_signed_in_the_temporary_store_at_the_pinned_serial() -> Result {
        let request = proposal_request(
            temporary_store_id("decman-proposals-run~1"),
            topology_mapping::Mapping::DecentralizedNamespaceDefinition(Default::default()),
            7,
            vec![ForceFlag::AllowUnvalidatedSigningKeys as i32],
        );

        assert!(!request.must_fully_authorize);
        assert!(request.signed_by.is_empty());
        assert_eq!(
            request.store,
            Some(temporary_store_id("decman-proposals-run~1"))
        );
        let Some(authorize_request::Type::Proposal(proposal)) = request.r#type else {
            anyhow::bail!("expected a proposal");
        };
        assert_eq!(proposal.serial, 7);
        assert_eq!(proposal.change, enums::TopologyChangeOp::AddReplace as i32);
        Ok(())
    }

    #[test]
    fn a_changed_namespace_is_refused_and_a_reordered_one_is_not() -> Result {
        let exported = DecentralizedNamespaceDefinition {
            decentralized_namespace: "ns".into(),
            threshold: 1,
            owners: vec!["a".into(), "b".into()],
        };
        let mut head = PartyHead {
            dns_serial: 3,
            dns: exported.clone(),
            p2p_serial: 5,
            p2p: PartyToParticipant::default(),
        };
        head.dns.owners.reverse();
        head.ensure_namespace_is(&exported)?;

        head.dns.threshold = 2;
        let error = head
            .ensure_namespace_is(&exported)
            .err()
            .ok_or_else(|| anyhow::anyhow!("a raised threshold went unnoticed"))?;
        assert!(
            format!("{error}").contains("changed after this run read it"),
            "{error}"
        );
        Ok(())
    }

    // -- replay ------------------------------------------------------------

    fn party_ns() -> String {
        format!("1220{}", "aa".repeat(32))
    }

    fn party() -> Result<CantonId> {
        CantonId::parse(&format!("party::{ns}", ns = party_ns()))
    }

    fn item(seconds: i64, serial: u32, mapping: topology_mapping::Mapping) -> HistoryItem {
        let transaction = TopologyTransaction {
            operation: enums::TopologyChangeOp::AddReplace as i32,
            serial,
            mapping: Some(TopologyMapping {
                mapping: Some(mapping),
            }),
        };
        HistoryItem {
            valid_from: Some(prost_types::Timestamp { seconds, nanos: 0 }),
            signed: SignedTopologyTransaction {
                transaction: UntypedVersionedMessage {
                    version: 30,
                    wrapper: Some(untyped_versioned_message::Wrapper::Data(
                        transaction.encode_to_vec(),
                    )),
                }
                .encode_to_vec(),
                ..Default::default()
            },
        }
    }

    fn dns(owners: &[&str]) -> topology_mapping::Mapping {
        topology_mapping::Mapping::DecentralizedNamespaceDefinition(
            DecentralizedNamespaceDefinition {
                decentralized_namespace: party_ns(),
                threshold: 1,
                owners: owners.iter().map(|o| (*o).to_string()).collect(),
            },
        )
    }

    fn p2p(party: &str, hosts: &[&str]) -> topology_mapping::Mapping {
        topology_mapping::Mapping::PartyToParticipant(PartyToParticipant {
            party: party.into(),
            threshold: 1,
            participants: hosts
                .iter()
                .map(|uid| HostingParticipant {
                    participant_uid: (*uid).to_string(),
                    permission: enums::ParticipantPermission::Confirmation as i32,
                    onboarding: None,
                })
                .collect(),
            party_signing_keys: None,
        })
    }

    fn serials_and_kinds(order: &[SignedTopologyTransaction]) -> Result<Vec<(u32, &'static str)>> {
        order
            .iter()
            .map(|signed| {
                let tx: TopologyTransaction = utils::decode_versioned(&signed.transaction)?;
                let kind = match tx.mapping.and_then(|m| m.mapping) {
                    Some(topology_mapping::Mapping::NamespaceDelegation(_)) => "nsd",
                    Some(topology_mapping::Mapping::DecentralizedNamespaceDefinition(_)) => "dnd",
                    Some(topology_mapping::Mapping::PartyToParticipant(_)) => "ptp",
                    _ => "other",
                };
                Ok((tx.serial, kind))
            })
            .collect()
    }

    /// Only this party's mappings are replayed, and the namespaces they name
    /// are what the delegation reads go and fetch.
    #[test]
    fn the_replay_keeps_this_party_and_names_every_signer() -> Result {
        let party = party()?;
        let items = vec![
            item(10, 1, dns(&["owner-a", "owner-b"])),
            item(11, 1, p2p(&party.to_string(), &["p1::ns-p1", "p2::ns-p2"])),
            // Another party in the same namespace, and a prefix lookalike.
            item(
                12,
                1,
                p2p(&format!("other::{ns}", ns = party_ns()), &["p9::ns-p9"]),
            ),
            item(13, 2, dns(&["owner-a", "owner-c"])),
            item(14, 2, p2p(&party.to_string(), &["p1::ns-p1", "p3::ns-p3"])),
        ];

        let (replays, namespaces) = party_replays(&party, items)?;

        assert_eq!(replays.len(), 4);
        assert_eq!(
            namespaces.into_iter().collect::<Vec<_>>(),
            ["ns-p1", "ns-p2", "ns-p3", "owner-a", "owner-b", "owner-c"]
        );
        Ok(())
    }

    /// Each transaction is judged against the state it was signed for, so the
    /// replay follows effective time, and within one moment a key's
    /// delegation precedes what it signed.
    #[test]
    fn the_replay_follows_history_and_puts_delegations_first() -> Result {
        let party = party()?;
        let root_key = SigningPublicKey {
            public_key: vec![9; 32],
            ..Default::default()
        };
        let owner = utils::compute_fingerprint(&root_key);
        let root = topology_mapping::Mapping::NamespaceDelegation(NamespaceDelegation {
            namespace: owner.clone(),
            target_key: Some(root_key),
            ..Default::default()
        });
        let intermediate = topology_mapping::Mapping::NamespaceDelegation(NamespaceDelegation {
            namespace: owner.clone(),
            target_key: Some(SigningPublicKey {
                public_key: vec![8; 32],
                ..Default::default()
            }),
            ..Default::default()
        });

        let (mut replays, _) = party_replays(
            &party,
            vec![
                item(20, 2, p2p(&party.to_string(), &["p1::ns"])),
                item(10, 1, p2p(&party.to_string(), &["p1::ns"])),
                item(10, 1, dns(&[&owner])),
            ],
        )?;
        replays.extend(delegation_replays(
            &owner,
            vec![item(10, 1, intermediate), item(10, 5, root.clone())],
        )?);
        // A delegation for a namespace that merely starts the same way.
        replays.extend(delegation_replays(
            &owner[..owner.len() - 2],
            vec![item(1, 1, root)],
        )?);

        assert_eq!(
            serials_and_kinds(&replay_order(replays))?,
            [(5, "nsd"), (1, "nsd"), (1, "dnd"), (1, "ptp"), (2, "ptp")]
        );
        Ok(())
    }

    // -- fallback --------------------------------------------------------

    /// A rebuilt store is used at every threshold. Falling back here would
    /// publish proposals the temporary store exists to hold back.
    #[test]
    fn a_rebuilt_store_is_always_used() {
        for threshold in [1, 2, 3] {
            assert_eq!(
                signing_route(threshold, RebuildOutcome::Rebuilt),
                Route::TemporaryStore,
                "threshold {threshold}"
            );
        }
    }

    /// At threshold 1 the coordinator alone authorizes the namespace change,
    /// so a proposal published early takes effect: a failed replay stops.
    #[test]
    fn a_failed_replay_at_threshold_one_refuses() {
        for threshold in [1, 0] {
            assert_eq!(
                signing_route(threshold, RebuildOutcome::ReplayFailed),
                Route::Refuse,
                "threshold {threshold}"
            );
        }
    }

    /// Above threshold 1 the coordinator alone cannot meet the threshold, so
    /// signing against the synchronizer, as before #448, is safe.
    #[test]
    fn a_failed_replay_above_threshold_one_falls_back() {
        for threshold in [2, 3, 7] {
            assert_eq!(
                signing_route(threshold, RebuildOutcome::ReplayFailed),
                Route::Synchronizer,
                "threshold {threshold}"
            );
        }
    }

    /// An unreachable participant says nothing about the history, so the
    /// step retries at every threshold and never falls back.
    #[test]
    fn an_unreachable_participant_never_falls_back() {
        for threshold in [1, 2, 3] {
            assert_eq!(
                signing_route(threshold, RebuildOutcome::Unreachable),
                Route::Retry,
                "threshold {threshold}"
            );
        }
    }

    #[test]
    fn transport_failures_read_as_unreachable_and_refusals_as_failed_replays() {
        let unreachable: [Result<usize>; 3] = [
            Err(tonic::Status::unavailable("connection refused").into()),
            Err(
                anyhow::Error::from(tonic::Status::deadline_exceeded("slow"))
                    .context("list the party's history"),
            ),
            Err(tonic::Status::unknown("h2 protocol error").into()),
        ];
        for result in &unreachable {
            assert_eq!(
                rebuild_outcome(result),
                RebuildOutcome::Unreachable,
                "{result:?}"
            );
        }

        let failed_replays: [Result<usize>; 3] = [
            Err(tonic::Status::invalid_argument("unknown mapping code").into()),
            Err(tonic::Status::failed_precondition("TOPOLOGY_SERIAL_MISMATCH").into()),
            Err(anyhow::anyhow!("the store holds DNS serial none")),
        ];
        for result in &failed_replays {
            assert_eq!(
                rebuild_outcome(result),
                RebuildOutcome::ReplayFailed,
                "{result:?}"
            );
        }

        assert_eq!(rebuild_outcome(&Ok(12)), RebuildOutcome::Rebuilt);
    }

    #[test]
    fn a_participant_uid_yields_its_namespace() {
        assert_eq!(
            participant_namespace("participant1::1220ab").as_deref(),
            Some("1220ab")
        );
        assert_eq!(participant_namespace("no-namespace"), None);
        assert_eq!(participant_namespace("trailing::"), None);
    }
}
