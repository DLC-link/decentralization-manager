//! Topology-store reads and writes for the co-sign model (design D5).
//!
//! The proposer writes a partially-signed proposal into the synchronizer
//! store. Every member reads it back with `proposals = true`, validates it,
//! and co-signs by transaction hash. Canton merges the signatures. Nothing in
//! this module ships transaction bytes between nodes.
//!
//! Discovery reads use `Snapshot(MaxValue)`, never `HeadState`: a fresh
//! proposal is invisible to `HeadState` until the node observes a sequencer
//! timestamp past `valid_from`, and that lag can exceed one poll interval.
//! "Effective" checks compare `valid_from` with the local clock instead.
//!
//! Mapping builders return canonical bytes: owners sorted, hosts sorted by
//! participant uid, keys sorted by fingerprint. Canton hashes the serialized
//! mapping, so two nodes that build the same change must produce the same
//! bytes, or their proposals never merge.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{SigningKeysWithThreshold, SigningPublicKey},
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToParticipant, SignedTopologyTransaction,
        TopologyMapping,
        enums::{ParticipantPermission, TopologyChangeOp},
        party_to_participant::{HostingParticipant, hosting_participant},
        topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, BaseQuery, BaseResult, ListDecentralizedNamespaceDefinitionRequest,
        ListNamespaceDelegationRequest, ListPartyToParticipantRequest, authorize_request,
        base_query, list_namespace_delegation_response::result::Item as NsDelegationItem,
        list_party_to_participant_response::result::Item as P2pItem,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use common::canton_id::CantonId;
use prost::Message;
use prost_types::Timestamp;
use sha2::{Digest, Sha256};

use crate::{
    config::NodeConfig,
    consts,
    utils::{self, MULTIHASH_SHA256_PREFIX},
    workflow::topology::{authorize_with_topology_retry, synchronizer_store_id},
};

use super::now_micros;

/// Canton `HashPurpose.TopologyTransactionSignature`: the domain separator of
/// a topology transaction hash.
pub const HASH_PURPOSE_TOPOLOGY_TRANSACTION_SIGNATURE: i32 = 11;

/// Canton `HashPurpose.DecentralizedNamespaceNamespace`: the domain separator
/// of a decentralized namespace.
pub const HASH_PURPOSE_DECENTRALIZED_NAMESPACE: i32 = 37;

const MICROS_PER_SEC: i64 = 1_000_000;

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// The Daml-LF maximum timestamp, `9999-12-31T23:59:59.999999Z`. Splice uses
/// it as `TopologySnapshot.Sequenced` so a read sees rows that are sequenced
/// but not yet effective.
pub fn max_timestamp() -> Timestamp {
    Timestamp {
        seconds: 253_402_300_799,
        nanos: 999_999_000,
    }
}

fn snapshot_query(synchronizer_id: &str, proposals: bool) -> BaseQuery {
    BaseQuery {
        store: Some(synchronizer_store_id(synchronizer_id)),
        proposals,
        operation: TopologyChangeOp::AddReplace as i32,
        time_query: Some(base_query::TimeQuery::Snapshot(max_timestamp())),
        filter_signed_key: String::new(),
        protocol_version: None,
        client_version: None,
    }
}

/// The discovery query: pending `ADD_REPLACE` proposals in the synchronizer
/// store, sequenced or effective.
pub fn proposals_query(synchronizer_id: &str) -> BaseQuery {
    snapshot_query(synchronizer_id, true)
}

/// The accepted-state query: the fully authorized `ADD_REPLACE` mapping,
/// sequenced or effective. Compare `valid_from` with the clock before
/// treating it as in force.
pub fn accepted_query(synchronizer_id: &str) -> BaseQuery {
    snapshot_query(synchronizer_id, false)
}

/// Micros since the epoch of a proto timestamp.
pub fn timestamp_micros(ts: &Timestamp) -> i64 {
    ts.seconds
        .saturating_mul(MICROS_PER_SEC)
        .saturating_add(i64::from(ts.nanos) / 1_000)
}

/// Whether `valid_from` has passed on the local clock. A missing timestamp is
/// never effective.
pub fn is_effective(valid_from: Option<&Timestamp>, now: i64) -> bool {
    valid_from.is_some_and(|ts| timestamp_micros(ts) <= now)
}

// ---------------------------------------------------------------------------
// Pending and accepted rows
// ---------------------------------------------------------------------------

/// One pending proposal for a mapping, with the context a co-signer needs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingProposal<M> {
    /// Lowercase hex of `BaseResult.transaction_hash`: the exact string
    /// `Authorize { transaction_hash }` takes.
    pub hash_hex: String,
    pub serial: u32,
    /// Long-term key fingerprints that have signed so far.
    pub signed_by: Vec<String>,
    /// `TopologyChangeOp` as the wire integer. Validation pins `ADD_REPLACE`.
    pub operation: i32,
    pub mapping: M,
    pub sequenced: Option<Timestamp>,
    pub valid_from: Option<Timestamp>,
}

impl<M> PendingProposal<M> {
    pub fn is_add_replace(&self) -> bool {
        self.operation == TopologyChangeOp::AddReplace as i32
    }

    pub fn is_signed_by(&self, fingerprint: &str) -> bool {
        self.signed_by.iter().any(|f| f == fingerprint)
    }
}

/// The accepted mapping at its latest serial.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedMapping<M> {
    pub serial: u32,
    pub mapping: M,
    pub valid_from: Option<Timestamp>,
}

impl<M> AcceptedMapping<M> {
    pub fn is_effective(&self, now: i64) -> bool {
        is_effective(self.valid_from.as_ref(), now)
    }
}

/// Lowercase hex of the transaction hash in a read result.
pub fn hash_hex_of(context: &BaseResult) -> String {
    hex::encode(&context.transaction_hash)
}

fn serial_of(context: &BaseResult) -> u32 {
    u32::try_from(context.serial).unwrap_or(0)
}

fn pending_from<M>(context: &BaseResult, mapping: M) -> PendingProposal<M> {
    PendingProposal {
        hash_hex: hash_hex_of(context),
        serial: serial_of(context),
        signed_by: context.signed_by_fingerprints.clone(),
        operation: context.operation,
        mapping,
        sequenced: context.sequenced,
        valid_from: context.valid_from,
    }
}

/// Keep the row with the highest serial; Canton promises no order.
fn latest<M>(rows: Vec<(BaseResult, M)>) -> Option<AcceptedMapping<M>> {
    rows.into_iter()
        .max_by_key(|(ctx, _)| ctx.serial)
        .map(|(ctx, mapping)| AcceptedMapping {
            serial: serial_of(&ctx),
            mapping,
            valid_from: ctx.valid_from,
        })
}

async fn read_client(
    config: &NodeConfig,
) -> Result<TopologyManagerReadServiceClient<tonic::transport::Channel>> {
    Ok(
        TopologyManagerReadServiceClient::new(config.admin_channel().await?)
            .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE),
    )
}

async fn list_dnd(
    config: &NodeConfig,
    query: BaseQuery,
    namespace: &str,
) -> Result<Vec<(BaseResult, DecentralizedNamespaceDefinition)>> {
    let mut client = read_client(config).await?;
    let response = client
        .list_decentralized_namespace_definition(tonic::Request::new(
            ListDecentralizedNamespaceDefinitionRequest {
                base_query: Some(query),
                filter_namespace: namespace.to_string(),
            },
        ))
        .await
        .context("ListDecentralizedNamespaceDefinition")?
        .into_inner();
    Ok(response
        .results
        .into_iter()
        .filter_map(|r| Some((r.context?, r.item?)))
        // `filter_namespace` is a prefix match; pin the exact namespace.
        .filter(|(_, m)| namespace.is_empty() || m.decentralized_namespace == namespace)
        .collect())
}

async fn list_p2p(
    config: &NodeConfig,
    query: BaseQuery,
    party: &str,
) -> Result<Vec<(BaseResult, PartyToParticipant)>> {
    let mut client = read_client(config).await?;
    let response = client
        .list_party_to_participant(tonic::Request::new(ListPartyToParticipantRequest {
            base_query: Some(query),
            filter_party: party.to_string(),
            filter_participant: String::new(),
        }))
        .await
        .context("ListPartyToParticipant")?
        .into_inner();
    Ok(response
        .results
        .into_iter()
        .filter_map(|r| {
            let P2pItem::V30(mapping) = r.item?;
            Some((r.context?, mapping))
        })
        // `filter_party` is a prefix match; pin the exact party.
        .filter(|(_, m)| party.is_empty() || m.party == party)
        .collect())
}

/// Pending `DecentralizedNamespaceDefinition` proposals for one namespace.
///
/// # Errors
/// Returns an error when the topology read fails.
pub async fn list_pending_dnd(
    config: &NodeConfig,
    synchronizer_id: &str,
    namespace: &str,
) -> Result<Vec<PendingProposal<DecentralizedNamespaceDefinition>>> {
    Ok(
        list_dnd(config, proposals_query(synchronizer_id), namespace)
            .await?
            .into_iter()
            .map(|(ctx, m)| pending_from(&ctx, m))
            .collect(),
    )
}

/// Pending `PartyToParticipant` proposals for one party.
///
/// # Errors
/// Returns an error when the topology read fails.
pub async fn list_pending_p2p(
    config: &NodeConfig,
    synchronizer_id: &str,
    party: &CantonId,
) -> Result<Vec<PendingProposal<PartyToParticipant>>> {
    Ok(
        list_p2p(config, proposals_query(synchronizer_id), &party.to_string())
            .await?
            .into_iter()
            .map(|(ctx, m)| pending_from(&ctx, m))
            .collect(),
    )
}

/// The accepted `DecentralizedNamespaceDefinition` of one namespace, or
/// `None` when the namespace has no authorized definition yet.
///
/// # Errors
/// Returns an error when the topology read fails.
pub async fn read_accepted_dnd(
    config: &NodeConfig,
    synchronizer_id: &str,
    namespace: &str,
) -> Result<Option<AcceptedMapping<DecentralizedNamespaceDefinition>>> {
    Ok(latest(
        list_dnd(config, accepted_query(synchronizer_id), namespace).await?,
    ))
}

/// The accepted `PartyToParticipant` of one party, or `None` when the party
/// has no authorized mapping yet.
///
/// # Errors
/// Returns an error when the topology read fails.
pub async fn read_accepted_p2p(
    config: &NodeConfig,
    synchronizer_id: &str,
    party: &CantonId,
) -> Result<Option<AcceptedMapping<PartyToParticipant>>> {
    Ok(latest(
        list_p2p(config, accepted_query(synchronizer_id), &party.to_string()).await?,
    ))
}

// ---------------------------------------------------------------------------
// Hashing
// ---------------------------------------------------------------------------

/// The Canton transaction hash of a `SignedTopologyTransaction.transaction`
/// (the versioned envelope bytes), as the lowercase multihash hex string
/// `Authorize { transaction_hash }` and `BaseResult.transaction_hash` use.
///
/// `multihash_sha256(be32(HashPurpose 11) || bytes)`.
pub fn transaction_hash_of(versioned_transaction: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(HASH_PURPOSE_TOPOLOGY_TRANSACTION_SIGNATURE.to_be_bytes());
    hasher.update(versioned_transaction);
    format!(
        "{MULTIHASH_SHA256_PREFIX}{}",
        hex::encode(hasher.finalize())
    )
}

/// The decentralized namespace of an owner set:
/// `multihash_sha256(be32(37) || (be32(len) || utf8(owner))* )` over the
/// sorted, deduplicated owners.
pub fn compute_namespace<'a>(owners: impl IntoIterator<Item = &'a String>) -> String {
    let sorted: BTreeSet<&String> = owners.into_iter().collect();
    let mut hasher = Sha256::new();
    hasher.update(HASH_PURPOSE_DECENTRALIZED_NAMESPACE.to_be_bytes());
    for owner in sorted {
        let bytes = owner.as_bytes();
        hasher.update(i32::try_from(bytes.len()).unwrap_or(i32::MAX).to_be_bytes());
        hasher.update(bytes);
    }
    format!(
        "{MULTIHASH_SHA256_PREFIX}{}",
        hex::encode(hasher.finalize())
    )
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// What `propose_mapping` wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct ProposedTx {
    /// Computed locally with [`transaction_hash_of`]; pin it on the run row.
    pub hash_hex: String,
    pub transaction: SignedTopologyTransaction,
}

fn mapping_label(mapping: &TopologyMapping) -> &'static str {
    match &mapping.mapping {
        Some(topology_mapping::Mapping::DecentralizedNamespaceDefinition(_)) => "DND",
        Some(topology_mapping::Mapping::PartyToParticipant(_)) => "P2P",
        Some(topology_mapping::Mapping::NamespaceDelegation(_)) => "NSD",
        _ => "topology mapping",
    }
}

/// Propose `mapping` at `serial` into the synchronizer store with this
/// node's own keys only (`must_fully_authorize = false`, `signed_by = []`,
/// no force flags). Canton pre-validates against the synchronizer head
/// state, so a rejection returns as an error and nothing is enqueued.
///
/// # Errors
/// Returns an error when Canton rejects the proposal, or when the retry
/// budget for `TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE` is exhausted.
pub async fn propose_mapping(
    config: &NodeConfig,
    synchronizer_id: &str,
    mapping: TopologyMapping,
    serial: u32,
) -> Result<ProposedTx> {
    let label = mapping_label(&mapping);
    let request = AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            authorize_request::Proposal {
                change: TopologyChangeOp::AddReplace as i32,
                serial,
                mapping: Some(authorize_request::proposal::Mapping::V30(mapping)),
            },
        )),
        must_fully_authorize: false,
        force_changes: vec![],
        signed_by: vec![],
        store: Some(synchronizer_store_id(synchronizer_id)),
        wait_to_become_effective: None,
    };
    let response = authorize_with_topology_retry(config, request, label).await?;
    let transaction = response
        .transaction
        .with_context(|| format!("{label}: Authorize returned no transaction"))?;
    let hash_hex = transaction_hash_of(&transaction.transaction);
    tracing::info!(%hash_hex, serial, "{label}: proposed into the synchronizer store");
    Ok(ProposedTx {
        hash_hex,
        transaction,
    })
}

/// What a co-sign by hash did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CosignOutcome {
    /// The hash is not in this participant's synchronizer store yet. Retry
    /// on the next poll.
    NotFound,
    /// This node added at least one signature.
    Signed,
    /// Every key this node holds had already signed; Canton returned the
    /// stored transaction unchanged.
    AlreadySigned,
}

fn is_transaction_not_found(err: &anyhow::Error) -> bool {
    let text = format!("{err:#}");
    text.contains("TOPOLOGY_TRANSACTION_NOT_FOUND") || text.contains("TopologyTransactionNotFound")
}

/// Co-sign the pending proposal with hash `hash_hex` using this node's keys
/// (`Authorize { transaction_hash }`, `must_fully_authorize = false`,
/// `signed_by = []`, synchronizer store). Idempotent: Canton skips keys that
/// already signed.
///
/// `previously_signed_by` is the `signed_by` list this node observed on the
/// proposal; the outcome compares the returned signer set against it.
///
/// # Errors
/// Returns an error when Canton rejects the call for a reason other than a
/// missing hash, including `TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE`
/// after the retry budget.
pub async fn cosign_by_hash(
    config: &NodeConfig,
    synchronizer_id: &str,
    hash_hex: &str,
    previously_signed_by: &[String],
) -> Result<CosignOutcome> {
    let request = AuthorizeRequest {
        r#type: Some(authorize_request::Type::TransactionHash(
            hash_hex.to_string(),
        )),
        must_fully_authorize: false,
        force_changes: vec![],
        signed_by: vec![],
        store: Some(synchronizer_store_id(synchronizer_id)),
        wait_to_become_effective: None,
    };
    let response = match authorize_with_topology_retry(config, request, "co-sign").await {
        Ok(r) => r,
        Err(e) if is_transaction_not_found(&e) => return Ok(CosignOutcome::NotFound),
        Err(e) => return Err(e),
    };
    let Some(transaction) = response.transaction else {
        bail!("co-sign of {hash_hex}: Authorize returned no transaction");
    };
    let before: BTreeSet<&str> = previously_signed_by.iter().map(String::as_str).collect();
    let added = transaction
        .signatures
        .iter()
        .any(|s| !before.contains(s.signed_by.as_str()));
    Ok(if added {
        CosignOutcome::Signed
    } else {
        CosignOutcome::AlreadySigned
    })
}

// ---------------------------------------------------------------------------
// Waits
// ---------------------------------------------------------------------------

/// How long a poll may run: `max_attempts` reads, `delay` apart.
#[derive(Clone, Copy, Debug)]
pub struct WaitBudget {
    pub max_attempts: usize,
    pub delay: Duration,
}

impl Default for WaitBudget {
    /// The shared topology retry knobs (`DECPM_TOPOLOGY_RETRY_*`).
    fn default() -> Self {
        Self {
            max_attempts: consts::topology_retry_max_attempts().max(1),
            delay: Duration::from_secs(consts::topology_retry_delay_secs()),
        }
    }
}

/// Which mapping a wait targets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MappingKey {
    /// A `DecentralizedNamespaceDefinition`, by its namespace.
    Dnd(String),
    /// A `PartyToParticipant`, by its party.
    P2p(CantonId),
}

impl std::fmt::Display for MappingKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dnd(ns) => write!(f, "DND {ns}"),
            Self::P2p(party) => write!(f, "P2P {party}"),
        }
    }
}

/// The accepted serial and `valid_from` of a mapping, type-erased.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedState {
    pub serial: u32,
    pub valid_from: Option<Timestamp>,
}

/// Read the accepted serial of `key`, or `None` when no mapping exists.
///
/// # Errors
/// Returns an error when the topology read fails.
pub async fn read_accepted_state(
    config: &NodeConfig,
    synchronizer_id: &str,
    key: &MappingKey,
) -> Result<Option<AcceptedState>> {
    Ok(match key {
        MappingKey::Dnd(ns) => read_accepted_dnd(config, synchronizer_id, ns)
            .await?
            .map(|a| AcceptedState {
                serial: a.serial,
                valid_from: a.valid_from,
            }),
        MappingKey::P2p(party) => read_accepted_p2p(config, synchronizer_id, party)
            .await?
            .map(|a| AcceptedState {
                serial: a.serial,
                valid_from: a.valid_from,
            }),
    })
}

/// Poll the accepted state of `key` until its serial reaches `serial` and
/// its `valid_from` has passed on the local clock (design D5 step 7).
///
/// # Errors
/// Returns an error when the budget runs out, or when the accepted serial
/// has moved past `serial` with a different transaction (the caller reads
/// the mapping again and decides).
pub async fn wait_effective(
    config: &NodeConfig,
    synchronizer_id: &str,
    key: &MappingKey,
    serial: u32,
    budget: WaitBudget,
) -> Result<AcceptedState> {
    for attempt in 1..=budget.max_attempts {
        match read_accepted_state(config, synchronizer_id, key).await {
            Ok(Some(state)) if state.serial >= serial => {
                if is_effective(state.valid_from.as_ref(), now_micros()) {
                    tracing::info!(%key, serial = state.serial, "mapping is effective");
                    return Ok(state);
                }
                tracing::debug!(%key, serial = state.serial, "mapping accepted, not yet effective");
            }
            Ok(Some(state)) => {
                tracing::debug!(%key, accepted = state.serial, wanted = serial, "waiting for serial");
            }
            Ok(None) => tracing::debug!(%key, "no accepted mapping yet"),
            Err(e) => tracing::warn!(%key, attempt, error = %e, "accepted-state read failed"),
        }
        if attempt < budget.max_attempts {
            tokio::time::sleep(budget.delay).await;
        }
    }
    bail!(
        "{key} did not reach serial {serial} and become effective within {} attempts",
        budget.max_attempts
    )
}

// ---------------------------------------------------------------------------
// Root namespace delegations (design D4)
// ---------------------------------------------------------------------------

/// The root `NamespaceDelegation` of `fingerprint` in the synchronizer
/// store, with its `valid_from`: the delegation whose namespace and target
/// key fingerprint are both `fingerprint`.
///
/// # Errors
/// Returns an error when the topology read fails.
pub async fn read_root_delegation(
    config: &NodeConfig,
    synchronizer_id: &str,
    fingerprint: &str,
) -> Result<Option<(SigningPublicKey, Option<Timestamp>)>> {
    let mut client = read_client(config).await?;
    let response = client
        .list_namespace_delegation(tonic::Request::new(ListNamespaceDelegationRequest {
            base_query: Some(accepted_query(synchronizer_id)),
            filter_namespace: fingerprint.to_string(),
            filter_target_key_fingerprint: fingerprint.to_string(),
        }))
        .await
        .context("ListNamespaceDelegation")?
        .into_inner();
    let mut found = None;
    for result in response.results {
        let Some(NsDelegationItem::V30(item)) = result.item else {
            continue;
        };
        // Both filters are prefix matches; pin the exact namespace.
        if item.namespace != fingerprint {
            continue;
        }
        let Some(key) = item.target_key else {
            continue;
        };
        let valid_from = result.context.and_then(|c| c.valid_from);
        found = Some((key, valid_from));
    }
    Ok(found)
}

/// The public key behind a root delegation, asserted to fingerprint to
/// `fingerprint`. This is the single source of key bytes for
/// `party_signing_keys` (design D4).
///
/// # Errors
/// Returns an error when no root delegation exists, when it is not yet
/// effective, or when the key does not fingerprint to `fingerprint`.
pub async fn read_root_delegation_key(
    config: &NodeConfig,
    synchronizer_id: &str,
    fingerprint: &str,
) -> Result<SigningPublicKey> {
    let Some((key, valid_from)) =
        read_root_delegation(config, synchronizer_id, fingerprint).await?
    else {
        bail!("no root NamespaceDelegation for {fingerprint} in the synchronizer store");
    };
    if !is_effective(valid_from.as_ref(), now_micros()) {
        bail!("root NamespaceDelegation for {fingerprint} is not effective yet");
    }
    let computed = utils::compute_fingerprint(&key);
    if computed != fingerprint {
        bail!(
            "root NamespaceDelegation for {fingerprint} carries a key that fingerprints to \
             {computed}"
        );
    }
    Ok(key)
}

/// Wait until every owner's root delegation is effective in the synchronizer
/// store. A DND proposal is rejected before that (design D5 order rule).
///
/// # Errors
/// Returns an error naming the owners still missing when the budget runs out.
pub async fn wait_owner_root_delegations(
    config: &NodeConfig,
    synchronizer_id: &str,
    fingerprints: &[String],
    budget: WaitBudget,
) -> Result<BTreeMap<String, SigningPublicKey>> {
    let mut keys: BTreeMap<String, SigningPublicKey> = BTreeMap::new();
    for attempt in 1..=budget.max_attempts {
        for fp in fingerprints {
            if keys.contains_key(fp) {
                continue;
            }
            match read_root_delegation_key(config, synchronizer_id, fp).await {
                Ok(key) => {
                    keys.insert(fp.clone(), key);
                }
                Err(e) => {
                    tracing::debug!(fingerprint = %fp, attempt, error = %e, "root NSD not ready")
                }
            }
        }
        if fingerprints.iter().all(|fp| keys.contains_key(fp)) {
            return Ok(keys);
        }
        if attempt < budget.max_attempts {
            tokio::time::sleep(budget.delay).await;
        }
    }
    let missing: Vec<&String> = fingerprints
        .iter()
        .filter(|fp| !keys.contains_key(*fp))
        .collect();
    bail!(
        "root NamespaceDelegations still missing from the synchronizer store after {} attempts: {missing:?}",
        budget.max_attempts
    )
}

// ---------------------------------------------------------------------------
// Unsolicited scan (design D5, UI only)
// ---------------------------------------------------------------------------

/// A pending proposal this node did not ask for. Read-only; the observer
/// never signs from this list. The DTO lives in `common` so `gen-types`
/// exports it; other `onledger` modules import it from here.
pub use common::coordination::UnsolicitedProposal;

/// Every pending DND and P2P proposal in the synchronizer store.
///
/// # Errors
/// Returns an error when a topology read fails.
pub async fn scan_unsolicited(
    config: &NodeConfig,
    synchronizer_id: &str,
) -> Result<Vec<UnsolicitedProposal>> {
    let mut out = Vec::new();
    for (ctx, m) in list_dnd(config, proposals_query(synchronizer_id), "").await? {
        out.push(UnsolicitedProposal {
            mapping: "DecentralizedNamespaceDefinition".into(),
            key: m.decentralized_namespace,
            hash_hex: hash_hex_of(&ctx),
            serial: serial_of(&ctx),
            signed_by: ctx.signed_by_fingerprints,
            sequenced_at: ctx.sequenced.as_ref().map(timestamp_micros),
        });
    }
    for (ctx, m) in list_p2p(config, proposals_query(synchronizer_id), "").await? {
        out.push(UnsolicitedProposal {
            mapping: "PartyToParticipant".into(),
            key: m.party,
            hash_hex: hash_hex_of(&ctx),
            serial: serial_of(&ctx),
            signed_by: ctx.signed_by_fingerprints,
            sequenced_at: ctx.sequenced.as_ref().map(timestamp_micros),
        });
    }
    out.sort_by(|a, b| (&a.key, a.serial, &a.hash_hex).cmp(&(&b.key, b.serial, &b.hash_hex)));
    Ok(out)
}

// ---------------------------------------------------------------------------
// Mapping builders
// ---------------------------------------------------------------------------

/// The DND inside a mapping, if it is one.
pub fn dnd_of(mapping: &TopologyMapping) -> Option<&DecentralizedNamespaceDefinition> {
    match &mapping.mapping {
        Some(topology_mapping::Mapping::DecentralizedNamespaceDefinition(d)) => Some(d),
        _ => None,
    }
}

/// The P2P inside a mapping, if it is one.
pub fn p2p_of(mapping: &TopologyMapping) -> Option<&PartyToParticipant> {
    match &mapping.mapping {
        Some(topology_mapping::Mapping::PartyToParticipant(p)) => Some(p),
        _ => None,
    }
}

fn wrap_dnd(dnd: DecentralizedNamespaceDefinition) -> TopologyMapping {
    TopologyMapping {
        mapping: Some(topology_mapping::Mapping::DecentralizedNamespaceDefinition(
            dnd,
        )),
    }
}

fn wrap_p2p(p2p: PartyToParticipant) -> TopologyMapping {
    TopologyMapping {
        mapping: Some(topology_mapping::Mapping::PartyToParticipant(p2p)),
    }
}

fn to_i32(threshold: u32) -> i32 {
    i32::try_from(threshold).unwrap_or(i32::MAX)
}

/// A host row at Confirmation, with or without the Onboarding marker.
fn confirmation_host(uid: &str, onboarding: bool) -> HostingParticipant {
    HostingParticipant {
        participant_uid: uid.to_string(),
        permission: ParticipantPermission::Confirmation as i32,
        onboarding: onboarding.then_some(hosting_participant::Onboarding {}),
    }
}

/// Canonical form: hosts sorted by uid (one row per uid, the first wins),
/// keys sorted by fingerprint (one per fingerprint).
fn canonical_p2p(
    party: String,
    hosts: Vec<HostingParticipant>,
    keys: Vec<SigningPublicKey>,
    threshold: u32,
) -> PartyToParticipant {
    let mut by_uid: BTreeMap<String, HostingParticipant> = BTreeMap::new();
    for h in hosts {
        by_uid.entry(h.participant_uid.clone()).or_insert(h);
    }
    let mut by_fp: BTreeMap<String, SigningPublicKey> = BTreeMap::new();
    for k in keys {
        by_fp.entry(utils::compute_fingerprint(&k)).or_insert(k);
    }
    PartyToParticipant {
        party,
        threshold,
        participants: by_uid.into_values().collect(),
        party_signing_keys: Some(SigningKeysWithThreshold {
            keys: by_fp.into_values().collect(),
            threshold,
        }),
    }
}

fn head_keys(head: &PartyToParticipant) -> Vec<SigningPublicKey> {
    head.party_signing_keys
        .as_ref()
        .map(|k| k.keys.clone())
        .unwrap_or_default()
}

/// A first DND: owners sorted, namespace computed from them.
pub fn build_dnd(owners: &[String], threshold: u32) -> TopologyMapping {
    let sorted: BTreeSet<String> = owners.iter().cloned().collect();
    wrap_dnd(DecentralizedNamespaceDefinition {
        decentralized_namespace: compute_namespace(sorted.iter()),
        threshold: to_i32(threshold),
        owners: sorted.into_iter().collect(),
    })
}

/// A first P2P for `prefix::namespace`: every host at Confirmation with no
/// Onboarding marker, one key per owner, both thresholds equal.
pub fn build_bootstrap_p2p(
    prefix: &str,
    namespace: &str,
    hosts: &[CantonId],
    keys: &[SigningPublicKey],
    threshold: u32,
) -> TopologyMapping {
    wrap_p2p(canonical_p2p(
        format!("{prefix}::{namespace}"),
        hosts
            .iter()
            .map(|h| confirmation_host(&h.to_string(), false))
            .collect(),
        keys.to_vec(),
        threshold,
    ))
}

/// The head DND plus one owner, at the new threshold. The namespace stays.
pub fn build_add_party_dnd(
    head: &DecentralizedNamespaceDefinition,
    joiner_fingerprint: &str,
    threshold: u32,
) -> TopologyMapping {
    let mut owners: BTreeSet<String> = head.owners.iter().cloned().collect();
    owners.insert(joiner_fingerprint.to_string());
    wrap_dnd(DecentralizedNamespaceDefinition {
        decentralized_namespace: head.decentralized_namespace.clone(),
        threshold: to_i32(threshold),
        owners: owners.into_iter().collect(),
    })
}

/// The head P2P plus the joiner at Confirmation with the Onboarding marker
/// and its key, at the new thresholds. Head hosts keep their tuples.
pub fn build_add_party_p2p(
    head: &PartyToParticipant,
    joiner: &CantonId,
    joiner_key: &SigningPublicKey,
    threshold: u32,
) -> TopologyMapping {
    let joiner_uid = joiner.to_string();
    let mut hosts: Vec<HostingParticipant> = head
        .participants
        .iter()
        .filter(|h| h.participant_uid != joiner_uid)
        .cloned()
        .collect();
    hosts.push(confirmation_host(&joiner_uid, true));
    let mut keys = head_keys(head);
    keys.push(joiner_key.clone());
    wrap_p2p(canonical_p2p(head.party.clone(), hosts, keys, threshold))
}

/// The head DND minus one owner, at the new threshold.
pub fn build_kick_dnd(
    head: &DecentralizedNamespaceDefinition,
    kicked_fingerprint: &str,
    threshold: u32,
) -> TopologyMapping {
    let owners: BTreeSet<String> = head
        .owners
        .iter()
        .filter(|o| o.as_str() != kicked_fingerprint)
        .cloned()
        .collect();
    wrap_dnd(DecentralizedNamespaceDefinition {
        decentralized_namespace: head.decentralized_namespace.clone(),
        threshold: to_i32(threshold),
        owners: owners.into_iter().collect(),
    })
}

/// The head P2P minus the kicked host and minus the key with
/// `kicked_key_fingerprint`, at the new thresholds. Survivors keep their
/// tuples.
pub fn build_kick_p2p(
    head: &PartyToParticipant,
    kicked: &CantonId,
    kicked_key_fingerprint: &str,
    threshold: u32,
) -> TopologyMapping {
    let kicked_uid = kicked.to_string();
    let hosts: Vec<HostingParticipant> = head
        .participants
        .iter()
        .filter(|h| h.participant_uid != kicked_uid)
        .cloned()
        .collect();
    let keys: Vec<SigningPublicKey> = head_keys(head)
        .into_iter()
        .filter(|k| utils::compute_fingerprint(k) != kicked_key_fingerprint)
        .collect();
    wrap_p2p(canonical_p2p(head.party.clone(), hosts, keys, threshold))
}

/// The head DND with only the threshold changed.
pub fn build_change_threshold_dnd(
    head: &DecentralizedNamespaceDefinition,
    threshold: u32,
) -> TopologyMapping {
    let owners: BTreeSet<String> = head.owners.iter().cloned().collect();
    wrap_dnd(DecentralizedNamespaceDefinition {
        decentralized_namespace: head.decentralized_namespace.clone(),
        threshold: to_i32(threshold),
        owners: owners.into_iter().collect(),
    })
}

/// The head P2P with only the two thresholds changed.
pub fn build_change_threshold_p2p(head: &PartyToParticipant, threshold: u32) -> TopologyMapping {
    wrap_p2p(canonical_p2p(
        head.party.clone(),
        head.participants.clone(),
        head_keys(head),
        threshold,
    ))
}

/// Serialized mapping bytes, for byte-stability checks and tests.
pub fn mapping_bytes(mapping: &TopologyMapping) -> Vec<u8> {
    mapping.encode_to_vec()
}

#[cfg(test)]
pub(crate) mod tests {
    use canton_proto_rs::com::digitalasset::canton::crypto::v30::{
        CryptoKeyFormat, SigningKeySpec, SigningKeyUsage,
    };

    use super::*;

    pub(crate) const NS: &str =
        "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

    pub(crate) fn participant(n: u8) -> CantonId {
        CantonId::parse(&format!("participant{n}::{NS}")).expect("valid id")
    }

    /// A key whose fingerprint is stable and distinct per `seed`.
    pub(crate) fn key(seed: u8) -> SigningPublicKey {
        SigningPublicKey {
            format: CryptoKeyFormat::Raw as i32,
            public_key: vec![seed; 32],
            key_spec: SigningKeySpec::EcCurve25519 as i32,
            usage: vec![
                SigningKeyUsage::Namespace as i32,
                SigningKeyUsage::Protocol as i32,
            ],
            ..Default::default()
        }
    }

    pub(crate) fn fp(seed: u8) -> String {
        utils::compute_fingerprint(&key(seed))
    }

    fn owner(c: char) -> String {
        format!("1220{}", c.to_string().repeat(64))
    }

    #[test]
    fn queries_target_the_synchronizer_snapshot_at_max_time() {
        let q = proposals_query("global::1220abcd::35-0");
        assert!(q.proposals);
        assert_eq!(q.operation, TopologyChangeOp::AddReplace as i32);
        assert_eq!(
            q.time_query,
            Some(base_query::TimeQuery::Snapshot(max_timestamp()))
        );
        let a = accepted_query("global::1220abcd::35-0");
        assert!(!a.proposals);
        assert_eq!(a.operation, TopologyChangeOp::AddReplace as i32);
        assert_eq!(a.time_query, q.time_query);
        assert_eq!(a.store, q.store);
    }

    #[test]
    fn max_timestamp_is_the_daml_lf_maximum() {
        // 9999-12-31T23:59:59.999999Z
        let ts = max_timestamp();
        assert_eq!(ts.seconds, 253_402_300_799);
        assert_eq!(ts.nanos, 999_999_000);
        assert_eq!(timestamp_micros(&ts), 253_402_300_799_999_999);
    }

    #[test]
    fn effectiveness_compares_valid_from_with_now() {
        let ts = Timestamp {
            seconds: 100,
            nanos: 0,
        };
        assert!(is_effective(Some(&ts), 100 * MICROS_PER_SEC));
        assert!(!is_effective(Some(&ts), 99 * MICROS_PER_SEC));
        assert!(!is_effective(None, i64::MAX));
    }

    #[test]
    fn transaction_hash_is_sha256_with_purpose_11_in_multihash_form() {
        // sha256(be32(11) || "abc"), computed independently.
        assert_eq!(
            transaction_hash_of(b"abc"),
            "1220118011a82e79b5c12c7edc776b3bc1285aefb8a6b8eee34bde8c2381fa329455"
        );
        assert_eq!(transaction_hash_of(b"").len(), 68);
    }

    #[test]
    fn namespace_matches_the_known_vector_regardless_of_order() {
        // sha256(be32(37) || (be32(len) || owner)*) over the sorted owners,
        // the algorithm `compute_decentralized_namespace` implements.
        let expected = "1220a200ccc573e9f149e39dfa0b53b06815e5b062f6898706790d035ee2362a956b";
        let owners = [owner('c'), owner('a'), owner('b')];
        assert_eq!(compute_namespace(owners.iter()), expected);
        let reversed: Vec<String> = owners.iter().rev().cloned().collect();
        assert_eq!(compute_namespace(reversed.iter()), expected);
        let with_duplicate = [owner('a'), owner('b'), owner('c'), owner('a')];
        assert_eq!(compute_namespace(with_duplicate.iter()), expected);
    }

    #[test]
    fn hash_hex_of_is_lowercase_hex_of_the_result_bytes() {
        let ctx = BaseResult {
            transaction_hash: vec![0x12, 0x20, 0xAB, 0xCD],
            serial: 3,
            signed_by_fingerprints: vec!["1220aa".into()],
            operation: TopologyChangeOp::AddReplace as i32,
            ..Default::default()
        };
        assert_eq!(hash_hex_of(&ctx), "1220abcd");
        let pending = pending_from(&ctx, 7u8);
        assert_eq!(pending.serial, 3);
        assert!(pending.is_add_replace());
        assert!(pending.is_signed_by("1220aa"));
        assert!(!pending.is_signed_by("1220bb"));
    }

    #[test]
    fn latest_keeps_the_highest_serial() {
        let row = |serial: i32| {
            (
                BaseResult {
                    serial,
                    ..Default::default()
                },
                serial,
            )
        };
        let accepted = latest(vec![row(2), row(5), row(3)]).expect("some");
        assert_eq!(accepted.serial, 5);
        assert_eq!(accepted.mapping, 5);
        assert!(latest::<i32>(vec![]).is_none());
    }

    #[test]
    fn dnd_builder_sorts_owners_and_derives_the_namespace() {
        let a = build_dnd(&[owner('c'), owner('a'), owner('b')], 2);
        let b = build_dnd(&[owner('b'), owner('c'), owner('a')], 2);
        assert_eq!(mapping_bytes(&a), mapping_bytes(&b));
        let dnd = dnd_of(&a).expect("dnd");
        assert_eq!(dnd.owners, vec![owner('a'), owner('b'), owner('c')]);
        assert_eq!(dnd.threshold, 2);
        assert_eq!(
            dnd.decentralized_namespace,
            compute_namespace(dnd.owners.iter())
        );
    }

    #[test]
    fn bootstrap_p2p_is_byte_stable_regardless_of_input_order() {
        let hosts = [participant(3), participant(1), participant(2)];
        let keys = [key(3), key(1), key(2)];
        let a = build_bootstrap_p2p("cbtc", NS, &hosts, &keys, 2);
        let mut hosts_rev = hosts.clone();
        hosts_rev.reverse();
        let mut keys_rev = keys.clone();
        keys_rev.reverse();
        let b = build_bootstrap_p2p("cbtc", NS, &hosts_rev, &keys_rev, 2);
        assert_eq!(mapping_bytes(&a), mapping_bytes(&b));

        let p2p = p2p_of(&a).expect("p2p");
        assert_eq!(p2p.party, format!("cbtc::{NS}"));
        assert_eq!(p2p.threshold, 2);
        let uids: Vec<&str> = p2p
            .participants
            .iter()
            .map(|h| h.participant_uid.as_str())
            .collect();
        let mut sorted = uids.clone();
        sorted.sort_unstable();
        assert_eq!(uids, sorted);
        assert!(p2p.participants.iter().all(|h| {
            h.permission == ParticipantPermission::Confirmation as i32 && h.onboarding.is_none()
        }));
        let signing = p2p.party_signing_keys.as_ref().expect("keys");
        assert_eq!(signing.threshold, 2);
        assert_eq!(signing.keys.len(), 3);
    }

    #[test]
    fn add_party_builders_add_exactly_the_joiner() {
        let head_dnd = dnd_of(&build_dnd(&[fp(1), fp(2)], 2)).expect("dnd").clone();
        let dnd = build_add_party_dnd(&head_dnd, &fp(3), 2);
        let dnd = dnd_of(&dnd).expect("dnd");
        assert_eq!(dnd.owners.len(), 3);
        assert!(dnd.owners.contains(&fp(3)));
        assert_eq!(
            dnd.decentralized_namespace,
            head_dnd.decentralized_namespace
        );

        let head_p2p = p2p_of(&build_bootstrap_p2p(
            "cbtc",
            NS,
            &[participant(1), participant(2)],
            &[key(1), key(2)],
            2,
        ))
        .expect("p2p")
        .clone();
        let p2p = build_add_party_p2p(&head_p2p, &participant(3), &key(3), 2);
        let p2p = p2p_of(&p2p).expect("p2p");
        assert_eq!(p2p.participants.len(), 3);
        let joiner = p2p
            .participants
            .iter()
            .find(|h| h.participant_uid == participant(3).to_string())
            .expect("joiner");
        assert!(joiner.onboarding.is_some());
        assert!(
            p2p.participants
                .iter()
                .filter(|h| h.participant_uid != participant(3).to_string())
                .all(|h| h.onboarding.is_none())
        );
        let keys = p2p.party_signing_keys.as_ref().expect("keys");
        assert_eq!(keys.keys.len(), 3);
        // Re-adding the same joiner is idempotent.
        let again = build_add_party_p2p(p2p, &participant(3), &key(3), 2);
        assert_eq!(mapping_bytes(&again), mapping_bytes(&wrap_p2p(p2p.clone())));
    }

    #[test]
    fn kick_builders_remove_exactly_the_kicked_member() {
        let head_dnd = dnd_of(&build_dnd(&[fp(1), fp(2), fp(3)], 2))
            .expect("dnd")
            .clone();
        let dnd = build_kick_dnd(&head_dnd, &fp(2), 2);
        let dnd = dnd_of(&dnd).expect("dnd");
        let mut expected = vec![fp(1), fp(3)];
        expected.sort();
        assert_eq!(dnd.owners, expected);

        let head_p2p = p2p_of(&build_bootstrap_p2p(
            "cbtc",
            NS,
            &[participant(1), participant(2), participant(3)],
            &[key(1), key(2), key(3)],
            2,
        ))
        .expect("p2p")
        .clone();
        let p2p = build_kick_p2p(&head_p2p, &participant(2), &fp(2), 2);
        let p2p = p2p_of(&p2p).expect("p2p");
        assert_eq!(p2p.participants.len(), 2);
        assert!(
            p2p.participants
                .iter()
                .all(|h| h.participant_uid != participant(2).to_string())
        );
        let keys = p2p.party_signing_keys.as_ref().expect("keys");
        assert_eq!(keys.keys.len(), 2);
        assert!(
            keys.keys
                .iter()
                .all(|k| utils::compute_fingerprint(k) != fp(2))
        );
    }

    #[test]
    fn change_threshold_builders_change_only_the_thresholds() {
        let head_dnd = dnd_of(&build_dnd(&[fp(1), fp(2), fp(3)], 2))
            .expect("dnd")
            .clone();
        let dnd = build_change_threshold_dnd(&head_dnd, 3);
        let dnd = dnd_of(&dnd).expect("dnd");
        assert_eq!(dnd.owners, head_dnd.owners);
        assert_eq!(
            dnd.decentralized_namespace,
            head_dnd.decentralized_namespace
        );
        assert_eq!(dnd.threshold, 3);

        let head_p2p = p2p_of(&build_bootstrap_p2p(
            "cbtc",
            NS,
            &[participant(1), participant(2), participant(3)],
            &[key(1), key(2), key(3)],
            2,
        ))
        .expect("p2p")
        .clone();
        let p2p = build_change_threshold_p2p(&head_p2p, 3);
        let p2p = p2p_of(&p2p).expect("p2p");
        assert_eq!(p2p.participants, head_p2p.participants);
        assert_eq!(p2p.threshold, 3);
        let keys = p2p.party_signing_keys.as_ref().expect("keys");
        assert_eq!(keys.threshold, 3);
        assert_eq!(
            keys.keys,
            head_p2p.party_signing_keys.as_ref().expect("keys").keys
        );
    }

    #[test]
    fn wait_budget_default_uses_the_topology_retry_knobs() {
        let b = WaitBudget::default();
        assert!(b.max_attempts >= 1);
        assert_eq!(
            b.delay,
            Duration::from_secs(consts::topology_retry_delay_secs())
        );
    }

    #[test]
    fn transaction_not_found_is_detected_through_the_error_chain() {
        let status = tonic::Status::not_found(
            "TOPOLOGY_TRANSACTION_NOT_FOUND(11,0): Unable to find topology transaction",
        );
        let err: anyhow::Error = status.into();
        assert!(is_transaction_not_found(&err.context("co-sign")));
        let other: anyhow::Error = tonic::Status::internal("boom").into();
        assert!(!is_transaction_not_found(&other));
    }
}
