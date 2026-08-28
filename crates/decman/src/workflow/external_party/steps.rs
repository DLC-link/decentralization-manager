//! Canton calls behind the wallet-driven external-party tenant API.
//!
//! Everything here goes over the **tokenless Canton Admin API**, the same path the
//! decentralized-party workflows use. [`prepare_topology`] builds the onboarding
//! transaction and asks Canton for its hash, [`allocate_party`] attaches the
//! party's signature, has this node co-sign, and submits it, and
//! [`host_onboarding_status`] / [`list_hosted_external_parties`] read topology
//! state. There is no coordinator and no inter-DPM coordination: the wallet calls
//! each host itself.
//!
//! Deliberately NOT the Ledger API's `AllocateExternalParty`. That RPC is a
//! convenience wrapper around exactly this topology write, and the wrapper is where
//! the authorization check and the party-allocation quota live — it demands either
//! `ParticipantAdmin` or a `user_id` matching the caller, and naming a user turns on
//! a quota that defaults to zero. Writing the topology directly needs no ledger
//! credential at all, so onboarding no longer depends on how a node's ledger users
//! happen to be provisioned.

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{
        CryptoKeyFormat, Signature, SignatureFormat, SigningAlgorithmSpec, SigningKeySpec,
        SigningKeyUsage, SigningKeysWithThreshold, SigningPublicKey,
    },
    protocol::v30::{
        PartyToParticipant, SignedTopologyTransaction, TopologyMapping, TopologyTransaction,
        enums::{ParticipantPermission, TopologyChangeOp},
        party_to_participant::HostingParticipant,
        topology_mapping,
    },
    topology::admin::v30::{
        AddTransactionsRequest, BaseQuery, GenerateTransactionsRequest,
        ListDecentralizedNamespaceDefinitionRequest, ListPartyToParticipantRequest,
        SignTransactionsRequest, StoreId, Synchronizer, base_query, generate_transactions_request,
        list_party_to_participant_response::result::Item as P2pItem, store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
    version::v1::{UntypedVersionedMessage, untyped_versioned_message},
};
use prost::Message as _;
use serde::{Deserialize, Serialize};

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{external_party::keys, topology},
};

/// The unsigned onboarding topology, plus the hash the party must sign for each
/// transaction. The two vectors are index-aligned.
pub struct PreparedTopology {
    /// The party id, `{hint}::{fingerprint-of-the-public-key}`.
    pub party_id: String,
    /// The fingerprint of the supplied public key (used as `signed_by`).
    pub public_key_fingerprint: String,
    /// Canton's hash for each transaction, for the party to sign. Canton computes
    /// these, so no Canton hash derivation is reimplemented here.
    pub transaction_hashes: Vec<Vec<u8>>,
    /// The serialized (versioned) topology transactions to submit.
    pub topology_transactions: Vec<Vec<u8>>,
}

/// DER prefix for an Ed25519 `SubjectPublicKeyInfo` (RFC 8410 §4): a 42-byte
/// SEQUENCE holding the `id-Ed25519` (1.3.101.112) AlgorithmIdentifier and a
/// 33-byte BIT STRING (unused-bits octet + the 32-byte key). Fixed for Ed25519,
/// so the whole encoding is this prefix followed by the raw key.
const ED25519_SPKI_DER_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// Wrap a raw 32-byte Ed25519 public key as DER-encoded X.509
/// `SubjectPublicKeyInfo`.
///
/// Canton's external-party RPCs parse the supplied key as X.509 SPKI and reject a
/// bare 32-byte key, even when the format field says `RAW`. The party's namespace
/// fingerprint is still computed over the *raw* key (Canton unwraps the SPKI
/// first — see `utils::compute_fingerprint`), so wrapping here does not change the
/// party id the wallet derived.
fn ed25519_spki_der(public_key: &[u8; 32]) -> Vec<u8> {
    let mut der = Vec::with_capacity(ED25519_SPKI_DER_PREFIX.len() + public_key.len());
    der.extend_from_slice(&ED25519_SPKI_DER_PREFIX);
    der.extend_from_slice(public_key);
    der
}

/// Resolve the confirmation threshold DPM writes into the topology. An unset
/// value defaults to `N-1` (never `N`) so a host can always exit later — the same
/// cap `validate_confirmation_threshold` enforces on an explicit value. Floors at
/// 1 for the degenerate single-host case.
fn resolve_confirmation_threshold(requested: Option<u32>, num_hosts: usize) -> u32 {
    requested.unwrap_or_else(|| num_hosts.saturating_sub(1).max(1) as u32)
}

/// Reject any submitted topology that is not exactly this party's onboarding.
///
/// This has to exist because of what [`allocate_party`] does next: it co-signs the
/// caller's bytes with **this node's own topology key** and submits them. That
/// signature is the operator's consent to host, and with an empty `signed_by`
/// Canton picks whichever of the node's keys can authorize the mapping — so
/// unvalidated bytes are not merely an unwanted hosting relationship, they are the
/// node's key applied to a topology change the operator never saw.
///
/// The previous design got this check for free: it submitted through the Ledger
/// API's `AllocateExternalParty`, which validates that a bundle really is an
/// external-party onboarding before the participant authorizes anything. Writing
/// the topology directly is what removed the admin-token requirement, and it also
/// removed that guard, so the guard is reimplemented here.
///
/// Only the party id string is already trustworthy — the handler derives it from
/// the submitted public key's own fingerprint. Everything else is caller-controlled
/// and is checked against a mapping rebuilt locally, so no field can carry anything
/// this node did not itself decide was acceptable.
fn validate_onboarding_topology(
    config: &NodeConfig,
    bundle: &ExternalPartyAllocatePayload,
) -> Result<()> {
    if bundle.topology_transactions.is_empty() {
        anyhow::bail!("external-party onboarding submitted no topology transactions");
    }

    let this_participant = config.participant_id().to_string();
    let expected_key = party_signing_key(&bundle.public_key);

    for (index, serialized) in bundle.topology_transactions.iter().enumerate() {
        // Two layers: Canton wraps a stable-versioned message in
        // `UntypedVersionedMessage`, whose `data` is the `TopologyTransaction`.
        let versioned =
            UntypedVersionedMessage::decode(serialized.as_slice()).with_context(|| {
                format!("topology transaction {index} is not a versioned Canton message")
            })?;
        let Some(untyped_versioned_message::Wrapper::Data(inner)) = versioned.wrapper else {
            anyhow::bail!("topology transaction {index} carries no payload");
        };
        let transaction = TopologyTransaction::decode(inner.as_slice()).with_context(|| {
            format!("topology transaction {index} is not a TopologyTransaction")
        })?;

        if transaction.operation != TopologyChangeOp::AddReplace as i32 {
            anyhow::bail!(
                "topology transaction {index} must be an ADD_REPLACE, got operation {op}",
                op = transaction.operation
            );
        }
        // Onboarding creates a party; it never replaces an existing mapping. A
        // higher serial here would be an attempt to rewrite topology that already
        // exists — including some other party's.
        if transaction.serial != 1 {
            anyhow::bail!(
                "topology transaction {index} must have serial 1 for onboarding, got {serial}",
                serial = transaction.serial
            );
        }

        let Some(TopologyMapping {
            mapping: Some(topology_mapping::Mapping::PartyToParticipant(p2p)),
        }) = transaction.mapping
        else {
            anyhow::bail!(
                "topology transaction {index} must carry a PartyToParticipant mapping; this \
                 endpoint onboards external parties and authorizes nothing else"
            );
        };

        if p2p.party != bundle.party_id {
            anyhow::bail!(
                "topology transaction {index} is for party {found}, not {expected} (the party \
                 derived from the submitted public key)",
                found = p2p.party,
                expected = bundle.party_id
            );
        }

        let mut uids = Vec::with_capacity(p2p.participants.len());
        for participant in &p2p.participants {
            // Submission permission would let the party submit through this node
            // directly. External parties sign their own submissions and never need
            // it, so anything but Confirmation is refused.
            if participant.permission != ParticipantPermission::Confirmation as i32 {
                anyhow::bail!(
                    "topology transaction {index} gives {uid} permission {perm}; every hosting \
                     participant must be Confirmation",
                    uid = participant.participant_uid,
                    perm = participant.permission
                );
            }
            if participant.onboarding.is_some() {
                anyhow::bail!(
                    "topology transaction {index} carries onboarding details for {uid}, which \
                     this endpoint does not submit",
                    uid = participant.participant_uid
                );
            }
            uids.push(participant.participant_uid.clone());
        }

        let mut sorted = uids.clone();
        sorted.sort();
        sorted.dedup();
        if sorted.len() != uids.len() {
            anyhow::bail!("topology transaction {index} lists a hosting participant twice");
        }
        if !uids.contains(&this_participant) {
            anyhow::bail!(
                "topology transaction {index} does not name this participant \
                 ({this_participant}); a host only authorizes topology that hosts it"
            );
        }

        // Same rule the request path enforces: at least one confirmation, and never
        // all of them, so a host can still exit later.
        let max_threshold = uids.len().saturating_sub(1) as u32;
        if p2p.threshold < 1 || p2p.threshold > max_threshold {
            anyhow::bail!(
                "topology transaction {index} has confirmation threshold {t}, which must be \
                 between 1 and {max_threshold} (one less than the {n} hosting participants)",
                t = p2p.threshold,
                n = uids.len()
            );
        }

        // The party must be authorized by the key whose fingerprint *is* its
        // namespace, and by nothing else. An extra key here would let its holder
        // transact as the party.
        let Some(keys) = &p2p.party_signing_keys else {
            anyhow::bail!("topology transaction {index} carries no party signing keys");
        };
        if keys.keys.len() != 1 || keys.threshold != 1 {
            anyhow::bail!(
                "topology transaction {index} must carry exactly one party signing key at \
                 threshold 1, got {n} key(s) at threshold {t}",
                n = keys.keys.len(),
                t = keys.threshold
            );
        }
        let key = &keys.keys[0];
        if key.format != expected_key.format
            || key.public_key != expected_key.public_key
            || key.key_spec != expected_key.key_spec
        {
            anyhow::bail!(
                "topology transaction {index} does not carry the submitted public key as the \
                 party's only signing key"
            );
        }
        // Usage is checked as a floor, not an exact match, because Canton rewrites
        // it: `GenerateTransactions` was handed `[Namespace, Protocol]` and returned
        // `[Namespace, Protocol, ProofOfOwnership]`, so demanding equality here
        // would reject this node's own generated topology. Being lax costs nothing —
        // the security property is that the key *material* is the wallet's and is
        // the only key, which is checked above. Usage flags on that one key grant
        // nobody else anything.
        //
        // The floor still matters: without `Protocol` the party could never
        // authorize a transaction, and without `Namespace` it could not authorize
        // its own topology.
        for required in [SigningKeyUsage::Namespace, SigningKeyUsage::Protocol] {
            if !key.usage.contains(&(required as i32)) {
                anyhow::bail!(
                    "topology transaction {index} declares usage {usage:?} on the party's signing \
                     key, which is missing {required:?}",
                    usage = key.usage
                );
            }
        }

        // Everything above checked a field at a time; this refuses anything the
        // mapping might carry that the checks above do not know about, so a future
        // Canton field cannot ride along unexamined.
        let expected = PartyToParticipant {
            party: bundle.party_id.clone(),
            threshold: p2p.threshold,
            participants: p2p.participants.clone(),
            // The submitted key, not a rebuilt one: its fields were just checked
            // individually, and its `usage` legitimately differs from what this node
            // would construct.
            party_signing_keys: Some(SigningKeysWithThreshold {
                keys: vec![key.clone()],
                threshold: 1,
            }),
        };
        if p2p != expected {
            anyhow::bail!(
                "topology transaction {index} carries fields beyond a plain external-party \
                 PartyToParticipant and will not be authorized"
            );
        }
    }

    Ok(())
}

/// The party's one signing key, as it appears in `PartyToParticipant`.
///
/// Built in exactly one place because two callers must agree byte-for-byte:
/// [`prepare_topology`] puts it into the mapping it generates, and
/// [`validate_onboarding_topology`] rejects a submitted mapping that does not carry
/// precisely this. If they drifted, the validator would start refusing the topology
/// this node itself produced.
fn party_signing_key(public_key: &[u8; 32]) -> SigningPublicKey {
    SigningPublicKey {
        // Canton parses this as X.509 SubjectPublicKeyInfo and rejects a bare
        // 32-byte key even when the format field says RAW.
        format: CryptoKeyFormat::DerX509SubjectPublicKeyInfo as i32,
        public_key: ed25519_spki_der(public_key),
        key_spec: SigningKeySpec::EcCurve25519 as i32,
        // This one key does two jobs, so it must declare both usages.
        //
        // `Namespace` (1) is what makes the party's own signature authorize its
        // topology: the party id's namespace *is* this key's fingerprint, so the
        // root is self-signed and no NamespaceDelegation is needed.
        //
        // `Protocol` (4) is what lets the key authorize Daml transactions —
        // `party_signing_keys` is documented as holding protocol signing keys, and
        // Canton filters on that usage when executing an externally signed
        // submission. Declaring only `Namespace` still onboards fine, and then
        // every submission fails with "1 external signatures were provided, which
        // is more than the number of registered signing keys (0) with protocol
        // usage" — a party that exists and can never transact.
        usage: vec![
            SigningKeyUsage::Namespace as i32,
            SigningKeyUsage::Protocol as i32,
        ],
        // `scheme` is deprecated in favour of `key_spec`; Default leaves it unset
        // rather than naming a deprecated field.
        ..Default::default()
    }
}

/// Ask Canton to build the external party's onboarding topology and the hash to
/// sign for each transaction.
///
/// One `PartyToParticipant` is the whole topology: since Canton 3.5 the party's
/// signing key rides inside it, so there is no separate `NamespaceDelegation` or
/// `PartyToKeyMapping`. Multi-host: the local participant plus every hosting peer
/// confirm, at the requested confirmation threshold — defaulting to `N-1` when
/// unset (see [`resolve_confirmation_threshold`]), never `N`, so a host can
/// always exit later.
///
/// # Errors
/// Returns an error if the synchronizer id cannot be resolved or the
/// `GenerateTransactions` RPC fails.
pub async fn prepare_topology(
    config: &NodeConfig,
    party_hint: &str,
    hosting_peers: &[CantonId],
    confirmation_threshold: Option<u32>,
    public_key: &[u8; 32],
) -> Result<PreparedTopology> {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    // The party id is `{hint}::{fingerprint of its own key}` — the namespace is the
    // key, which is what makes the party's signature self-authorizing here and why
    // no separate NamespaceDelegation is needed.
    let fingerprint = keys::fingerprint_from_public_key(public_key);
    let party_id = format!("{party_hint}::{fingerprint}");

    let signing_public_key = party_signing_key(public_key);

    // Hosts = this participant plus every confirming peer, all at Confirmation.
    let mut participants = vec![HostingParticipant {
        participant_uid: config.participant_id().to_string(),
        permission: ParticipantPermission::Confirmation as i32,
        onboarding: None,
    }];
    participants.extend(hosting_peers.iter().map(|p| HostingParticipant {
        participant_uid: p.to_string(),
        permission: ParticipantPermission::Confirmation as i32,
        onboarding: None,
    }));
    let threshold = resolve_confirmation_threshold(confirmation_threshold, participants.len());
    // Canonical order, so every host generates byte-identical transactions for the
    // same party. Without it each host lists itself first, the serialized bytes
    // differ, and the wallet cannot compare what the hosts prepared against each
    // other — which is the only thing standing between it and a lying host (see
    // `onboard_co_validated` in the wallet crate). Order carries no meaning to
    // Canton; the hosting set is a set.
    participants.sort_by(|a, b| a.participant_uid.cmp(&b.participant_uid));

    // The party's signing key rides inside PartyToParticipant (Canton 3.5+), so this
    // one mapping is the whole onboarding topology.
    let mapping = TopologyMapping {
        mapping: Some(topology_mapping::Mapping::PartyToParticipant(
            PartyToParticipant {
                party: party_id.clone(),
                threshold,
                participants,
                party_signing_keys: Some(SigningKeysWithThreshold {
                    keys: vec![signing_public_key],
                    threshold: 1,
                }),
            },
        )),
    };

    let mut client = TopologyManagerWriteServiceClient::new(config.admin_channel().await?);
    let response = client
        .generate_transactions(tonic::Request::new(GenerateTransactionsRequest {
            proposals: vec![generate_transactions_request::Proposal {
                operation: TopologyChangeOp::AddReplace as i32,
                serial: 1,
                mapping: Some(generate_transactions_request::proposal::Mapping::V30(
                    mapping,
                )),
                store: Some(topology::synchronizer_store_id(&synchronizer_id)),
            }],
            base_request: None,
        }))
        .await
        .context("GenerateTransactions RPC failed")?
        .into_inner();

    if response.generated_transactions.is_empty() {
        return Err(anyhow::anyhow!(
            "GenerateTransactions returned no transactions for {party_id}"
        ));
    }

    tracing::info!(
        %party_id,
        "external-party: generated onboarding topology ({} txs)",
        response.generated_transactions.len()
    );

    let mut transaction_hashes = Vec::with_capacity(response.generated_transactions.len());
    let mut topology_transactions = Vec::with_capacity(response.generated_transactions.len());
    for tx in response.generated_transactions {
        transaction_hashes.push(tx.transaction_hash);
        topology_transactions.push(tx.serialized_transaction);
    }

    Ok(PreparedTopology {
        party_id,
        public_key_fingerprint: fingerprint,
        transaction_hashes,
        topology_transactions,
    })
}

/// The party-signed onboarding bundle the wallet submits to each host's
/// `/v0/tenant/onboard`: the unsigned topology transactions plus the party's
/// signature per transaction. Each host attaches those signatures, adds its own
/// participant authorization, and submits to its own synchronizer store.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExternalPartyAllocatePayload {
    /// The allocated party id (`{hint}::{fingerprint}`).
    pub party_id: String,
    /// The party's raw Ed25519 public key. Needed to check that the submitted
    /// topology names this key — and only this key — as the party's signing key.
    pub public_key: [u8; 32],
    /// The serialized (versioned) topology transactions to submit.
    pub topology_transactions: Vec<Vec<u8>>,
    /// One raw Ed25519 signature per transaction, index-aligned with
    /// `topology_transactions`, each over the hash Canton returned for it.
    pub signatures: Vec<Vec<u8>>,
    /// Fingerprint of the party key that produced the signatures (`signed_by`).
    pub signed_by: String,
}

/// Authorize hosting the external party on this node's participant: attach the
/// party's signatures to the onboarding transactions, have this node co-sign, and
/// submit them to the synchronizer store. Called by `/v0/tenant/onboard`, which the
/// wallet invokes on each host independently.
///
/// All three RPCs are on the tokenless Admin API, so this needs no ledger
/// credential — see the module note on why not `AllocateExternalParty`.
///
/// Re-submitting the same transaction is how the wallet retries a host, and Canton
/// treats an identical, already-authorized transaction as a no-op, so `/onboard`
/// stays idempotent.
///
/// # Errors
/// Returns an error if the signature count does not match the transaction count, if
/// the synchronizer id cannot be resolved, or if any of the topology RPCs fail.
pub async fn allocate_party(
    config: &NodeConfig,
    bundle: &ExternalPartyAllocatePayload,
) -> Result<()> {
    if bundle.signatures.len() != bundle.topology_transactions.len() {
        return Err(anyhow::anyhow!(
            "external-party onboarding needs one signature per transaction: got {sigs} \
             signature(s) for {txs} transaction(s)",
            sigs = bundle.signatures.len(),
            txs = bundle.topology_transactions.len()
        ));
    }

    // Before this node's key goes anywhere near these bytes.
    validate_onboarding_topology(config, bundle)?;

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let store = topology::synchronizer_store_id(&synchronizer_id);

    // Each transaction carries the party's signature over its own hash, and is
    // submitted as a proposal: this host can only add its own participant
    // authorization, so until every hosting participant has submitted, the
    // signatures are valid but insufficient. Canton accumulates them and promotes
    // the mapping once the last host signs — the same convergence the dec-party
    // workflows rely on.
    let signed: Vec<SignedTopologyTransaction> = bundle
        .topology_transactions
        .iter()
        .zip(&bundle.signatures)
        .map(|(transaction, signature)| SignedTopologyTransaction {
            transaction: transaction.clone(),
            signatures: vec![Signature {
                format: SignatureFormat::Concat as i32,
                signature: signature.clone(),
                signed_by: bundle.signed_by.clone(),
                signing_algorithm_spec: SigningAlgorithmSpec::Ed25519 as i32,
                signature_delegation: None,
            }],
            proposal: true,
            multi_transaction_signatures: vec![],
        })
        .collect();

    // The node adds its own participant signature; an empty `signed_by` lets it pick
    // its own key, matching how the dec-party workflows call this.
    let co_signed = topology::sign_transactions_with_topology_retry(
        config,
        SignTransactionsRequest {
            transactions: signed,
            signed_by: vec![],
            store: Some(store.clone()),
            force_flags: vec![],
        },
        "external-party onboarding",
    )
    .await?
    .transactions;

    let mut client = TopologyManagerWriteServiceClient::new(config.admin_channel().await?);
    client
        .add_transactions(tonic::Request::new(AddTransactionsRequest {
            transactions: co_signed,
            force_changes: vec![],
            store: Some(store),
            wait_to_become_effective: None,
        }))
        .await
        .context("AddTransactions RPC failed for external-party onboarding")?;

    tracing::info!(
        party_id = %bundle.party_id,
        "external-party: onboarding topology submitted on this host"
    );

    Ok(())
}

/// A head-state (or proposal) topology query against the synchronizer store.
pub(super) fn party_query(synchronizer_id: &str, proposals: bool) -> BaseQuery {
    BaseQuery {
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.to_string())),
            })),
        }),
        proposals,
        operation: 0,
        time_query: Some(base_query::TimeQuery::HeadState(())),
        filter_signed_key: String::new(),
        protocol_version: None,
        client_version: None,
    }
}

/// Whether this participant hosts an external party yet, from its own topology.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostOnboardingStatus {
    /// An authorized `PartyToParticipant` names this participant with
    /// Confirmation and no onboarding marker — hosting is live.
    Hosted,
    /// Named in the authorized mapping, but Canton's onboarding marker is still
    /// set: the party is assigned here and suspended here. It holds none of the
    /// party's contracts and cannot confirm for it until the marker clears.
    ///
    /// Distinct from [`Pending`](Self::Pending), which is about signatures the
    /// topology still needs. This one is authorized and waiting on replication.
    Onboarding,
    /// A proposal exists but is not yet fully authorized (waiting on more hosts).
    Pending,
    /// No mapping — authorized or proposed — for this party on this participant.
    Absent,
}

/// Read whether this participant hosts `party_id` from its own topology state via
/// the tokenless Canton Admin API. There is no local run row: each host answers
/// only for itself, and the wallet aggregates across hosts.
///
/// # Errors
/// Returns an error if the synchronizer id cannot be resolved or the
/// `ListPartyToParticipant` RPC fails.
pub async fn host_onboarding_status(
    config: &NodeConfig,
    party_id: &str,
) -> Result<HostOnboardingStatus> {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let self_uid = config.participant_id().to_string();
    let mut client = TopologyManagerReadServiceClient::new(config.admin_channel().await?);

    // Authorized head state first (proposals = false); if absent, a pending
    // proposal (proposals = true) means this host has not finished signing.
    for proposals in [false, true] {
        let response = client
            .list_party_to_participant(tonic::Request::new(ListPartyToParticipantRequest {
                base_query: Some(party_query(&synchronizer_id, proposals)),
                filter_party: party_id.to_string(),
                filter_participant: self_uid.clone(),
            }))
            .await?
            .into_inner();
        // The marker matters as much as the name: a host carrying it is in the
        // mapping but suspended, holding none of the party's contracts. Reading
        // only the uid and the permission reports such a host as live, which is
        // how a caller ends up trusting a host that cannot yet confirm anything.
        let mut named = None;
        for result in &response.results {
            let Some(P2pItem::V30(p)) = &result.item else {
                continue;
            };
            if let Some(entry) = p.participants.iter().find(|h| {
                h.participant_uid == self_uid
                    && h.permission == ParticipantPermission::Confirmation as i32
            }) {
                named = Some(entry.onboarding.is_some());
                break;
            }
        }
        if let Some(marked) = named {
            return Ok(if proposals {
                HostOnboardingStatus::Pending
            } else if marked {
                HostOnboardingStatus::Onboarding
            } else {
                HostOnboardingStatus::Hosted
            });
        }
    }
    Ok(HostOnboardingStatus::Absent)
}

/// One external party this participant hosts, read from topology.
pub struct HostedExternalParty {
    pub party_id: String,
    pub fingerprint: String,
    /// How many of the hosting participants must confirm a transaction (the M).
    pub threshold: u32,
    /// How many participants host the party (the N).
    pub host_count: u32,
    /// When the mapping became effective, RFC 3339. `None` if Canton did not
    /// report it.
    pub created_at: Option<String>,
}

/// Render a protobuf timestamp as RFC 3339 (UTC), for display.
fn timestamp_to_rfc3339(ts: &prost_types::Timestamp) -> String {
    chrono::DateTime::from_timestamp(ts.seconds, ts.nanos.max(0) as u32)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default()
}

/// List the external parties this participant hosts with Confirmation permission,
/// from its own topology (tokenless Admin API). External = a self-owned
/// single-key namespace: excludes decentralized parties (their namespace is a
/// `DecentralizedNamespaceDefinition`) and this node's own local namespace.
///
/// # Errors
/// Returns an error if the synchronizer id cannot be resolved or a topology RPC
/// fails.
pub async fn list_hosted_external_parties(config: &NodeConfig) -> Result<Vec<HostedExternalParty>> {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let self_uid = config.participant_id().to_string();
    let own_namespace = self_uid
        .rsplit_once("::")
        .map(|(_, ns)| ns.to_string())
        .unwrap_or_default();
    let mut client = TopologyManagerReadServiceClient::new(config.admin_channel().await?)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let decentralized: std::collections::HashSet<String> = client
        .list_decentralized_namespace_definition(tonic::Request::new(
            ListDecentralizedNamespaceDefinitionRequest {
                base_query: Some(party_query(&synchronizer_id, false)),
                filter_namespace: String::new(),
            },
        ))
        .await?
        .into_inner()
        .results
        .into_iter()
        .filter_map(|r| r.item.map(|i| i.decentralized_namespace))
        .collect();

    let p2p = client
        .list_party_to_participant(tonic::Request::new(ListPartyToParticipantRequest {
            base_query: Some(party_query(&synchronizer_id, false)),
            filter_party: String::new(),
            filter_participant: self_uid.clone(),
        }))
        .await?
        .into_inner();

    let mut parties = Vec::new();
    for result in p2p.results {
        // `valid_from` is when this mapping became effective — the party's creation
        // time for a serial-1 mapping. Read before `item` is moved out.
        let created_at = result
            .context
            .as_ref()
            .and_then(|c| c.valid_from.as_ref())
            .map(timestamp_to_rfc3339);
        let Some(P2pItem::V30(mapping)) = result.item else {
            continue;
        };
        let hosts_us_confirming = mapping.participants.iter().any(|h| {
            h.participant_uid == self_uid
                && h.permission == ParticipantPermission::Confirmation as i32
        });
        if !hosts_us_confirming {
            continue;
        }
        let Some((_, fingerprint)) = mapping.party.rsplit_once("::") else {
            continue;
        };
        let fingerprint = fingerprint.to_string();
        // Skip this node's own local namespace and any decentralized party — what
        // remains is an external party with a self-owned single-key namespace.
        if fingerprint == own_namespace || decentralized.contains(&fingerprint) {
            continue;
        }
        parties.push(HostedExternalParty {
            party_id: mapping.party.clone(),
            fingerprint,
            threshold: mapping.threshold,
            host_count: mapping.participants.len() as u32,
            created_at,
        });
    }
    Ok(parties)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_threshold_defaults_to_n_minus_1_not_n() {
        // The bug this guards: an unset threshold must NOT default to N (which
        // the N-1 cap rejects) — it defaults to N-1 so a host can still exit.
        assert_eq!(resolve_confirmation_threshold(None, 3), 2);
        assert_eq!(resolve_confirmation_threshold(None, 2), 1);
        // Degenerate single-host case floors at 1 rather than 0.
        assert_eq!(resolve_confirmation_threshold(None, 1), 1);
        // An explicit value is passed through unchanged.
        assert_eq!(resolve_confirmation_threshold(Some(1), 3), 1);
        assert_eq!(resolve_confirmation_threshold(Some(2), 3), 2);
    }

    // ------------------------------------------------------------------
    // Submitted-topology validation
    //
    // These are the reason `validate_onboarding_topology` exists: this node
    // co-signs the caller's bytes with its own topology key, so each case below is
    // something a tenant API key holder could otherwise talk this node into
    // authorizing.
    // ------------------------------------------------------------------

    const PUBLIC_KEY: [u8; 32] = [7u8; 32];

    fn test_config() -> NodeConfig {
        let mut config = NodeConfig::default();
        config.node.participant_id = Some(participant(1));
        config
    }

    fn participant(tag: u8) -> CantonId {
        let namespace = format!("1220{}", format!("{tag:02x}").repeat(32));
        match CantonId::parse(&format!("participant-{tag}::{namespace}")) {
            Ok(id) => id,
            Err(e) => panic!("test participant id must parse: {e}"),
        }
    }

    fn hosting(id: &CantonId, permission: ParticipantPermission) -> HostingParticipant {
        HostingParticipant {
            participant_uid: id.to_string(),
            permission: permission as i32,
            onboarding: None,
        }
    }

    fn test_party_id() -> String {
        format!(
            "alice::{fp}",
            fp = keys::fingerprint_from_public_key(&PUBLIC_KEY)
        )
    }

    /// Serialize a mapping the way Canton hands it back: a `TopologyTransaction`
    /// inside an `UntypedVersionedMessage`.
    fn serialize(mapping: topology_mapping::Mapping, serial: u32) -> Vec<u8> {
        let transaction = TopologyTransaction {
            operation: TopologyChangeOp::AddReplace as i32,
            serial,
            mapping: Some(TopologyMapping {
                mapping: Some(mapping),
            }),
        };
        UntypedVersionedMessage {
            wrapper: Some(untyped_versioned_message::Wrapper::Data(
                transaction.encode_to_vec(),
            )),
            version: 30,
        }
        .encode_to_vec()
    }

    /// A well-formed onboarding for a party hosted on participants 1..=3.
    fn valid_mapping() -> PartyToParticipant {
        PartyToParticipant {
            party: test_party_id(),
            threshold: 2,
            participants: vec![
                hosting(&participant(1), ParticipantPermission::Confirmation),
                hosting(&participant(2), ParticipantPermission::Confirmation),
                hosting(&participant(3), ParticipantPermission::Confirmation),
            ],
            party_signing_keys: Some(SigningKeysWithThreshold {
                keys: vec![party_signing_key(&PUBLIC_KEY)],
                threshold: 1,
            }),
        }
    }

    fn bundle_of(mapping: topology_mapping::Mapping, serial: u32) -> ExternalPartyAllocatePayload {
        ExternalPartyAllocatePayload {
            party_id: test_party_id(),
            public_key: PUBLIC_KEY,
            topology_transactions: vec![serialize(mapping, serial)],
            signatures: vec![vec![0u8; 64]],
            signed_by: keys::fingerprint_from_public_key(&PUBLIC_KEY),
        }
    }

    fn validate(mapping: PartyToParticipant) -> Result<()> {
        validate_onboarding_topology(
            &test_config(),
            &bundle_of(topology_mapping::Mapping::PartyToParticipant(mapping), 1),
        )
    }

    /// The baseline: whatever this node itself generates must pass, or onboarding
    /// breaks. `party_signing_key` is shared with `prepare_topology` precisely so
    /// this cannot drift.
    #[test]
    fn accepts_a_well_formed_onboarding() {
        if let Err(e) = validate(valid_mapping()) {
            panic!("the topology this node generates must validate: {e}");
        }
    }

    /// Canton rewrites the key's `usage`. Handed `[Namespace, Protocol]`,
    /// `GenerateTransactions` on DevNet returns `[Namespace, Protocol,
    /// ProofOfOwnership]` — observed against a live participant. An exact-match
    /// check on the key would therefore refuse this node's own topology, so this
    /// pins the real shape rather than the one we submit.
    #[test]
    fn accepts_the_usage_canton_actually_returns() {
        let mut mapping = valid_mapping();
        let mut canton_key = party_signing_key(&PUBLIC_KEY);
        canton_key.usage = vec![
            SigningKeyUsage::Namespace as i32,
            SigningKeyUsage::Protocol as i32,
            SigningKeyUsage::ProofOfOwnership as i32,
        ];
        mapping.party_signing_keys = Some(SigningKeysWithThreshold {
            keys: vec![canton_key],
            threshold: 1,
        });
        if let Err(e) = validate(mapping) {
            panic!("Canton's own normalized usage must validate: {e}");
        }
    }

    /// The floor that does matter: without Protocol the party can never authorize a
    /// transaction, which is a party that exists and cannot transact.
    #[test]
    fn rejects_a_key_without_protocol_usage() {
        let mut mapping = valid_mapping();
        let mut namespace_only = party_signing_key(&PUBLIC_KEY);
        namespace_only.usage = vec![SigningKeyUsage::Namespace as i32];
        mapping.party_signing_keys = Some(SigningKeysWithThreshold {
            keys: vec![namespace_only],
            threshold: 1,
        });
        let Err(e) = validate(mapping) else {
            panic!("a key without Protocol usage must be refused");
        };
        assert!(e.to_string().contains("Protocol"), "{e}");
    }

    /// A transaction Canton really produced, captured from `GenerateTransactions` on
    /// a live DevNet participant, base64 exactly as the tenant API returns it.
    ///
    /// Every other test here builds its bytes with the same code the validator
    /// checks against, so all of them would pass even if this node's idea of the
    /// wire format were wrong. This one cannot: it is Canton's output, and it is what
    /// catches a validator that has drifted from what the participant actually emits.
    const REAL_TRANSACTION_B64: &str = concat!(
        "Cq0DCAEQARqmA0qjAwpOcmV2cHJvYmU6OjEyMjBmM2NiZDBjZDIyZGE5MzNlOTMwN2NiMmNhYTEw",
        "NTc3MTc2Zjg1OWRmNWQ5NTRlNzYyNGRlYTRkNzM3ZDcyYjNiEAIaWgpWaUJUQy12YWxpZGF0b3It",
        "MTo6MTIyMGZhODU0M2RiNmM2NmZlM2E1NWIxZjE4MGM4ZGZjN2Y4NzYyNjVjNzY2ODRmYmMxZDM1",
        "ZDg5ZTAyYzhhYWZlOGUQAhpaClZpQlRDLXZhbGlkYXRvci0yOjoxMjIwOTk5NTM5MzRkOWZlMTYz",
        "ZmVkMDdkZDM3MWZhMTM5ODJiMmIzMDc0OWQ2ZGY1NmVjZGJhMzg1ZjhjNzhhODY3YRACGloKVmlC",
        "VEMtdmFsaWRhdG9yLTM6OjEyMjBkNTQ0MTI1ZDM2MTllOWM0ZDczNDA0NjZhNzhhODVmNzM4ZTc1",
        "YTc0MDgzN2ZhNjI4M2ExMDUxNDk0ZjVkZjIzEAIyOwo3EAQaLDAqMAUGAytlcAMhADB0XdntKp6w",
        "Tpwyenkc9DU0waM7lv350MybxMJLiPq3KgMBBAUwARABEB4=",
    );

    /// The raw Ed25519 public key that [`REAL_TRANSACTION_B64`] was generated for.
    const REAL_PUBLIC_KEY_B64: &str = "MHRd2e0qnrBOnDJ6eRz0NTTBozuW/fnQzJvEwkuI+rc=";

    #[test]
    fn accepts_a_transaction_canton_really_produced() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let Ok(transaction) = STANDARD.decode(REAL_TRANSACTION_B64) else {
            panic!("the captured fixture must be valid base64");
        };
        let Ok(key_bytes) = STANDARD.decode(REAL_PUBLIC_KEY_B64) else {
            panic!("the captured public key must be valid base64");
        };
        let Ok(public_key) = <[u8; 32]>::try_from(key_bytes.as_slice()) else {
            panic!("the captured public key must be 32 bytes");
        };

        // The fixture hosts the party on iBTC-validator-1..3; validate as the first.
        let mut config = NodeConfig::default();
        let validator_1 = match CantonId::parse(
            "iBTC-validator-1::1220fa8543db6c66fe3a55b1f180c8dfc7f876265c76684fbc1d35d89e02c8aafe8e",
        ) {
            Ok(id) => id,
            Err(e) => panic!("the fixture's participant id must parse: {e}"),
        };
        config.node.participant_id = Some(validator_1);

        let bundle = ExternalPartyAllocatePayload {
            party_id: format!(
                "revprobe::{fp}",
                fp = keys::fingerprint_from_public_key(&public_key)
            ),
            public_key,
            topology_transactions: vec![transaction],
            signatures: vec![vec![0u8; 64]],
            signed_by: keys::fingerprint_from_public_key(&public_key),
        };

        if let Err(e) = validate_onboarding_topology(&config, &bundle) {
            panic!("a transaction Canton itself generated must validate: {e}");
        }
    }

    /// The attack that motivated the review: a mapping that is not an
    /// external-party onboarding at all. Co-signing this would apply the node's own
    /// key to a topology change the operator never saw.
    #[test]
    fn rejects_a_mapping_that_is_not_party_to_participant() {
        let result = validate_onboarding_topology(
            &test_config(),
            &bundle_of(
                topology_mapping::Mapping::NamespaceDelegation(Default::default()),
                1,
            ),
        );
        let Err(e) = result else {
            panic!("a non-PartyToParticipant mapping must be refused");
        };
        assert!(
            e.to_string().contains("PartyToParticipant"),
            "the error should name what is required: {e}"
        );
    }

    /// Submission permission would let the party submit through this node directly —
    /// a hosting relationship the operator never agreed to.
    #[test]
    fn rejects_submission_permission() {
        let mut mapping = valid_mapping();
        mapping.participants[0] = hosting(&participant(1), ParticipantPermission::Submission);
        let Err(e) = validate(mapping) else {
            panic!("Submission permission must be refused");
        };
        assert!(e.to_string().contains("Confirmation"), "{e}");
    }

    /// A second signing key would let its holder transact as the party.
    #[test]
    fn rejects_an_extra_party_signing_key() {
        let mut mapping = valid_mapping();
        if let Some(keys) = mapping.party_signing_keys.as_mut() {
            keys.keys.push(party_signing_key(&[9u8; 32]));
            keys.threshold = 1;
        }
        let Err(e) = validate(mapping) else {
            panic!("an extra signing key must be refused");
        };
        assert!(e.to_string().contains("exactly one"), "{e}");
    }

    /// A substituted key is the same attack without the extra entry.
    #[test]
    fn rejects_a_foreign_party_signing_key() {
        let mut mapping = valid_mapping();
        mapping.party_signing_keys = Some(SigningKeysWithThreshold {
            keys: vec![party_signing_key(&[9u8; 32])],
            threshold: 1,
        });
        let Err(e) = validate(mapping) else {
            panic!("a foreign signing key must be refused");
        };
        assert!(e.to_string().contains("submitted public key"), "{e}");
    }

    /// The party id is derived from the submitted key, so a mapping naming a
    /// different party is an attempt to touch someone else's topology.
    #[test]
    fn rejects_a_different_party() {
        let mut mapping = valid_mapping();
        mapping.party = format!(
            "bob::{fp}",
            fp = keys::fingerprint_from_public_key(&[9u8; 32])
        );
        let Err(e) = validate(mapping) else {
            panic!("a mapping for another party must be refused");
        };
        assert!(e.to_string().contains("not"), "{e}");
    }

    /// A host authorizes hosting *itself*; it has no business co-signing a mapping
    /// that hosts the party somewhere else entirely.
    #[test]
    fn rejects_a_mapping_that_does_not_host_this_participant() {
        let mut mapping = valid_mapping();
        mapping.participants = vec![
            hosting(&participant(2), ParticipantPermission::Confirmation),
            hosting(&participant(3), ParticipantPermission::Confirmation),
        ];
        mapping.threshold = 1;
        let Err(e) = validate(mapping) else {
            panic!("a mapping that omits this participant must be refused");
        };
        assert!(e.to_string().contains("this participant"), "{e}");
    }

    /// Threshold N leaves no host able to exit, which is the cap the request path
    /// enforces; the validator must not be the weaker of the two.
    #[test]
    fn rejects_a_threshold_that_strands_every_host() {
        let mut mapping = valid_mapping();
        mapping.threshold = 3;
        let Err(e) = validate(mapping) else {
            panic!("a threshold of N must be refused");
        };
        assert!(e.to_string().contains("threshold"), "{e}");

        let mut zero = valid_mapping();
        zero.threshold = 0;
        if validate(zero).is_ok() {
            panic!("a threshold of 0 must be refused");
        }
    }

    /// Serial > 1 would replace topology that already exists.
    #[test]
    fn rejects_a_serial_other_than_one() {
        let result = validate_onboarding_topology(
            &test_config(),
            &bundle_of(
                topology_mapping::Mapping::PartyToParticipant(valid_mapping()),
                2,
            ),
        );
        let Err(e) = result else {
            panic!("a serial other than 1 must be refused");
        };
        assert!(e.to_string().contains("serial"), "{e}");
    }

    #[test]
    fn rejects_a_duplicated_hosting_participant() {
        let mut mapping = valid_mapping();
        mapping.participants = vec![
            hosting(&participant(1), ParticipantPermission::Confirmation),
            hosting(&participant(1), ParticipantPermission::Confirmation),
            hosting(&participant(2), ParticipantPermission::Confirmation),
        ];
        let Err(e) = validate(mapping) else {
            panic!("a duplicated participant must be refused");
        };
        assert!(e.to_string().contains("twice"), "{e}");
    }

    #[test]
    fn rejects_bytes_that_are_not_a_topology_transaction() {
        let bundle = ExternalPartyAllocatePayload {
            party_id: test_party_id(),
            public_key: PUBLIC_KEY,
            topology_transactions: vec![b"not a protobuf at all".to_vec()],
            signatures: vec![vec![0u8; 64]],
            signed_by: keys::fingerprint_from_public_key(&PUBLIC_KEY),
        };
        if validate_onboarding_topology(&test_config(), &bundle).is_ok() {
            panic!("garbage bytes must be refused");
        }
    }

    #[test]
    fn rejects_an_empty_bundle() {
        let bundle = ExternalPartyAllocatePayload {
            party_id: test_party_id(),
            public_key: PUBLIC_KEY,
            topology_transactions: Vec::new(),
            signatures: Vec::new(),
            signed_by: keys::fingerprint_from_public_key(&PUBLIC_KEY),
        };
        if validate_onboarding_topology(&test_config(), &bundle).is_ok() {
            panic!("an empty bundle must be refused");
        }
    }
}
