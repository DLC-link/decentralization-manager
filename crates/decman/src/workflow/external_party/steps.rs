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
        SigningKeysWithThreshold, SigningPublicKey,
    },
    protocol::v30::{
        PartyToParticipant, SignedTopologyTransaction, TopologyMapping,
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
};
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

/// Ask Canton to build the external party's onboarding topology
/// (`NamespaceDelegation` + `PartyToKeyMapping` + `PartyToParticipant`) and the
/// hash to sign for each. Multi-host: the local participant plus every hosting peer
/// confirm, at the requested confirmation threshold — defaulting to `N-1` when
/// unset (see [`resolve_confirmation_threshold`]), never `N`, so a host can
/// always exit later.
///
/// # Errors
/// Returns an error if the synchronizer id cannot be resolved or the
/// `GenerateExternalPartyTopology` RPC fails.
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

    // Canton parses this as X.509 SubjectPublicKeyInfo and rejects a bare 32-byte
    // key even when the format field says RAW.
    let signing_public_key = SigningPublicKey {
        format: CryptoKeyFormat::DerX509SubjectPublicKeyInfo as i32,
        public_key: ed25519_spki_der(public_key),
        key_spec: SigningKeySpec::EcCurve25519 as i32,
        // Namespace usage: this key defines the party's namespace and signs its
        // topology. SigningKeyUsage::Namespace = 1.
        usage: vec![1],
        // `scheme` is deprecated in favour of `key_spec`; Default leaves it unset
        // rather than naming a deprecated field.
        ..Default::default()
    };

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
fn party_query(synchronizer_id: &str, proposals: bool) -> BaseQuery {
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
    /// Confirmation — hosting is live.
    Hosted,
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
        let names_self = response.results.iter().any(|r| {
            matches!(&r.item, Some(P2pItem::V30(p)) if {
                p.participants.iter().any(|h| {
                    h.participant_uid == self_uid
                        && h.permission == ParticipantPermission::Confirmation as i32
                })
            })
        });
        if names_self {
            return Ok(if proposals {
                HostOnboardingStatus::Pending
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
    pub threshold: u32,
    pub host_count: u32,
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
}
