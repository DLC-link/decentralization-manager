//! Canton calls behind the wallet-driven external-party tenant API.
//!
//! [`prepare_topology`] builds the unsigned onboarding transactions and the
//! multi-hash (Ledger API), [`allocate_party`] submits them with the party's own
//! Ed25519 signature on the local participant (Ledger API), and
//! [`host_onboarding_status`] / [`list_hosted_external_parties`] read this
//! participant's topology state (tokenless Admin API). There is no coordinator
//! and no inter-DPM coordination: the wallet calls each host itself.

use anyhow::Context;
use canton_proto_rs::com::daml::ledger::api::v2::{
    CryptoKeyFormat, Signature, SignatureFormat, SigningAlgorithmSpec, SigningKeySpec,
    SigningPublicKey,
    admin::{
        AllocateExternalPartyRequest, GenerateExternalPartyTopologyRequest,
        allocate_external_party_request::SignedTransaction,
    },
};
use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::enums::ParticipantPermission,
    topology::admin::v30::{
        BaseQuery, ListDecentralizedNamespaceDefinitionRequest, ListPartyToParticipantRequest,
        StoreId, Synchronizer, base_query,
        list_party_to_participant_response::result::Item as P2pItem, store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use serde::{Deserialize, Serialize};

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    utils::{self, extract_synchronizer_fingerprint},
    workflow::external_party::keys,
};

/// The unsigned onboarding topology returned by `GenerateExternalPartyTopology`.
pub struct PreparedTopology {
    /// The party id Canton derived from the hint + public key.
    pub party_id: String,
    /// The fingerprint of the supplied public key (used as `signed_by`).
    pub public_key_fingerprint: String,
    /// The combined hash over all onboarding transactions, for the party to sign.
    pub multi_hash: Vec<u8>,
    /// The serialized (versioned) topology transactions to submit.
    pub topology_transactions: Vec<Vec<u8>>,
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
/// multi-hash to sign. Multi-host: the local participant plus every hosting peer
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
    let synchronizer = external_party_synchronizer(config).await?;

    let signing_public_key = SigningPublicKey {
        format: CryptoKeyFormat::Raw as i32,
        key_data: public_key.to_vec(),
        key_spec: SigningKeySpec::EcCurve25519 as i32,
    };

    let other_confirming_participant_uids: Vec<String> =
        hosting_peers.iter().map(|p| p.to_string()).collect();
    // Hosts = the local participant + the confirming peers.
    let num_hosts = 1 + other_confirming_participant_uids.len();
    let confirmation_threshold = resolve_confirmation_threshold(confirmation_threshold, num_hosts);

    let request = GenerateExternalPartyTopologyRequest {
        synchronizer,
        party_hint: party_hint.to_string(),
        public_key: Some(signing_public_key),
        local_participant_observation_only: false,
        other_confirming_participant_uids,
        confirmation_threshold,
        observing_participant_uids: Vec::new(),
    };

    let mut client = utils::create_party_client(config, external_party_token_required()?).await?;
    let response = client
        .generate_external_party_topology(tonic::Request::new(request))
        .await
        .context("GenerateExternalPartyTopology RPC failed")?
        .into_inner();

    tracing::info!(
        party_id = %response.party_id,
        "external-party: Canton generated onboarding topology ({} txs)",
        response.topology_transactions.len()
    );

    // The sovereignty model rests on DPM (standing in for the wallet) deriving
    // the exact same party identity Canton does from the client-held key. Assert
    // that invariant here: Canton's party id must be `{hint}::{our_fingerprint}`.
    // A mismatch means `keys.rs` is out of sync with Canton's key-fingerprinting
    // and every downstream identity claim is wrong — fail loudly rather than
    // onboard a party whose namespace DPM cannot reproduce.
    let derived_fingerprint = keys::fingerprint_from_public_key(public_key);
    let canton_fingerprint = response.party_id.split_once("::").map_or("", |(_, fp)| fp);
    if canton_fingerprint != derived_fingerprint {
        return Err(anyhow::anyhow!(
            "external-party fingerprint mismatch: Canton derived {canton_fingerprint} but DPM \
             derived {derived_fingerprint} from the same key — key derivation is out of sync"
        ));
    }

    Ok(PreparedTopology {
        party_id: response.party_id,
        public_key_fingerprint: response.public_key_fingerprint,
        multi_hash: response.multi_hash,
        topology_transactions: response.topology_transactions,
    })
}

/// The party-signed onboarding bundle the wallet submits to each host's
/// `/v0/tenant/onboard`: the unsigned topology transactions plus the party's
/// single signature over the multi-hash. Each host reconstructs the allocate
/// request against its own synchronizer and adds its own participant
/// authorization.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExternalPartyAllocatePayload {
    /// The allocated party id (`{hint}::{fingerprint}`).
    pub party_id: String,
    /// The serialized (versioned) topology transactions to submit.
    pub topology_transactions: Vec<Vec<u8>>,
    /// The party's raw Ed25519 signature over the multi-hash.
    pub signature: Vec<u8>,
    /// Fingerprint of the party key that produced the signature (`signed_by`).
    pub signed_by: String,
}

/// Authorize hosting the external party on this node's participant by submitting
/// the party-signed onboarding `bundle` via `AllocateExternalParty`. Called by
/// `/v0/tenant/onboard`, which the wallet invokes on each host independently.
///
/// An `ALREADY_EXISTS` response is treated as success so a re-sent `/onboard`
/// (the wallet's retry against a host) converges instead of failing.
///
/// # Errors
/// Returns an error if the synchronizer id cannot be resolved or the
/// `AllocateExternalParty` RPC fails for any reason other than the party
/// already existing.
pub async fn allocate_party(
    config: &NodeConfig,
    bundle: &ExternalPartyAllocatePayload,
) -> Result<()> {
    let synchronizer = external_party_synchronizer(config).await?;

    let signature = Signature {
        format: SignatureFormat::Concat as i32,
        signature: bundle.signature.clone(),
        signed_by: bundle.signed_by.clone(),
        signing_algorithm_spec: SigningAlgorithmSpec::Ed25519 as i32,
    };

    let onboarding_transactions = bundle
        .topology_transactions
        .iter()
        .map(|tx| SignedTransaction {
            transaction: tx.clone(),
            signatures: Vec::new(),
        })
        .collect();

    let request = AllocateExternalPartyRequest {
        synchronizer,
        onboarding_transactions,
        multi_hash_signatures: vec![signature],
        identity_provider_id: String::new(),
        user_id: String::new(),
        wait_for_allocation: None,
    };

    let mut client = utils::create_party_client(config, external_party_token_required()?).await?;
    match client
        .allocate_external_party(tonic::Request::new(request))
        .await
    {
        Ok(_) => {
            grant_read_as_hosted_party(config, &bundle.party_id).await;
            Ok(())
        }
        Err(status) if status.code() == tonic::Code::AlreadyExists => {
            tracing::info!("external-party already allocated on this node; treating as success");
            grant_read_as_hosted_party(config, &bundle.party_id).await;
            Ok(())
        }
        Err(status) => Err(anyhow::anyhow!(
            "AllocateExternalParty RPC failed: {status}"
        )),
    }
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

/// Grant this participant's ledger user `CanReadAs` the hosted external party, so
/// the tenant ACS / prepare-submission endpoints can read the party's contracts
/// through the participant's token. Deliberately read-only: never `CanActAs`,
/// which would let a host impersonate the sovereign party without its signature.
///
/// Best-effort — a failure is logged, not propagated (the party is hosted either
/// way). Wired only for insecure/test builds, mirroring [`external_party_token`];
/// production external-party ledger auth is the open item tracked in the design
/// doc.
async fn grant_read_as_hosted_party(config: &NodeConfig, party_id: &str) {
    #[cfg(any(test, feature = "test-mode"))]
    {
        use canton_proto_rs::com::daml::ledger::api::v2::admin::{
            GrantUserRightsRequest, Right,
            right::{CanReadAs, Kind},
        };
        let mut client = match utils::create_user_client(config, external_party_token()).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("external-party: user client for read-as grant failed: {e}");
                return;
            }
        };
        let request = GrantUserRightsRequest {
            user_id: crate::auth::mock::MOCK_USER_ID.to_string(),
            rights: vec![Right {
                kind: Some(Kind::CanReadAs(CanReadAs {
                    party: party_id.to_string(),
                })),
            }],
            identity_provider_id: String::new(),
        };
        if let Err(e) = client.grant_user_rights(tonic::Request::new(request)).await {
            tracing::warn!("external-party: grant CanReadAs {party_id} failed: {e}");
        }
    }
    #[cfg(not(any(test, feature = "test-mode")))]
    {
        let _ = (config, party_id);
    }
}

/// Resolve the `alias::fingerprint` synchronizer id the external-party RPCs
/// expect (the protocol-version suffix is stripped).
async fn external_party_synchronizer(config: &NodeConfig) -> Result<String> {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    extract_synchronizer_fingerprint(&synchronizer_id)
}

/// Bearer token for the external-party Ledger-API admin calls.
///
/// The participant's Ledger API is authenticated (unlike the tokenless Canton
/// Admin API the topology/vault workflows use), so these calls need a bearer:
/// - On the localnet e2e (built with `--features test-mode`) we send the same
///   `MOCK_TOKEN` (`sub: ledger-api-user`) every other ledger call resolves via
///   the mock auth registry.
/// - In production we currently have no token: `AllocateExternalParty` requires
///   `ParticipantAdmin OR IdentityProviderAdmin`, and there is no node-admin
///   token in the per-party auth registry yet. Wiring one in is the open item.
fn external_party_token() -> Option<String> {
    #[cfg(any(test, feature = "test-mode"))]
    {
        Some(crate::auth::mock::MOCK_TOKEN.to_string())
    }
    #[cfg(not(any(test, feature = "test-mode")))]
    {
        None
    }
}

/// The external-party Ledger-API token, or a clear error when it isn't
/// configured. In non-test builds [`external_party_token`] is `None` (the open
/// item), so calling Canton would fail with an opaque `Unauthenticated`; fail
/// fast here with an explicit message instead.
///
/// # Errors
/// Returns an error in production builds, where no participant token is wired yet.
fn external_party_token_required() -> Result<Option<String>> {
    external_party_token().map(Some).ok_or_else(|| {
        anyhow::anyhow!(
            "external-party ledger operations require a participant Ledger-API token, which is \
             not yet configured in production (open item)"
        )
    })
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
