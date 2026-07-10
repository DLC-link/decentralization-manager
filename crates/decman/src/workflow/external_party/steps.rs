//! Canton Ledger-API calls for external-party onboarding.
//!
//! Two steps wrap the participant's `PartyManagementService` external-party
//! RPCs: [`prepare_topology`] builds the unsigned onboarding transactions and
//! the multi-hash, and [`allocate_party`] submits them with the party's own
//! Ed25519 signature over that multi-hash.

use anyhow::Context;
use canton_proto_rs::com::daml::ledger::api::v2::{
    CryptoKeyFormat, Signature, SignatureFormat, SigningAlgorithmSpec, SigningKeySpec,
    SigningPublicKey,
    admin::{
        AllocateExternalPartyRequest, GenerateExternalPartyTopologyRequest,
        allocate_external_party_request::SignedTransaction,
    },
};
use serde::{Deserialize, Serialize};

use crate::{
    config::NodeConfig,
    error::Result,
    utils::{self, extract_synchronizer_fingerprint},
    workflow::external_party::{ExternalPartyConfig, keys::ExternalKeyPair},
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

/// Ask Canton to build the external party's onboarding topology
/// (`NamespaceDelegation` + `PartyToKeyMapping` + `PartyToParticipant`) and the
/// multi-hash to sign. Multi-host: the local participant plus every hosting peer
/// confirm, at the configured confirmation threshold (defaulting to the number
/// of hosting participants when unset).
///
/// # Errors
/// Returns an error if the synchronizer id cannot be resolved or the
/// `GenerateExternalPartyTopology` RPC fails.
pub async fn prepare_topology(
    config: &NodeConfig,
    external: &ExternalPartyConfig,
    keypair: &ExternalKeyPair,
) -> Result<PreparedTopology> {
    let synchronizer = external_party_synchronizer(config).await?;

    let public_key = SigningPublicKey {
        format: CryptoKeyFormat::Raw as i32,
        key_data: keypair.public_key_bytes().to_vec(),
        key_spec: SigningKeySpec::EcCurve25519 as i32,
    };

    let other_confirming_participant_uids: Vec<String> = external
        .hosting_peers
        .iter()
        .map(|p| p.to_string())
        .collect();
    // Hosts = the coordinator's own participant + the confirming peers. Default
    // the confirmation threshold to all hosts when the caller didn't set one.
    let num_hosts = 1 + other_confirming_participant_uids.len();
    let confirmation_threshold = external
        .confirmation_threshold
        .unwrap_or(num_hosts as u32);

    let request = GenerateExternalPartyTopologyRequest {
        synchronizer,
        party_hint: external.party_hint.clone(),
        public_key: Some(public_key),
        local_participant_observation_only: false,
        other_confirming_participant_uids,
        confirmation_threshold,
        observing_participant_uids: Vec::new(),
    };

    let mut client = utils::create_party_client(config, external_party_token()).await?;
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
    let derived_fingerprint = keypair.fingerprint();
    let canton_fingerprint = response
        .party_id
        .split_once("::")
        .map_or("", |(_, fp)| fp);
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

/// The party-signed onboarding bundle the coordinator fans out to each hosting
/// peer: the unsigned topology transactions plus the party's single signature
/// over the multi-hash. Each host reconstructs the allocate request against its
/// own synchronizer and adds its own participant authorization.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExternalPartyAllocatePayload {
    /// The allocated party id (`{hint}::{fingerprint}`), so each host can record
    /// the party it is hosting on its own run.
    pub party_id: String,
    /// The serialized (versioned) topology transactions to submit.
    pub topology_transactions: Vec<Vec<u8>>,
    /// The party's raw Ed25519 signature over the multi-hash.
    pub signature: Vec<u8>,
    /// Fingerprint of the party key that produced the signature (`signed_by`).
    pub signed_by: String,
}

impl ExternalPartyAllocatePayload {
    /// Sign the prepared multi-hash with the party key and package the bundle
    /// for hosting.
    pub fn sign(prepared: &PreparedTopology, keypair: &ExternalKeyPair) -> Self {
        Self {
            party_id: prepared.party_id.clone(),
            topology_transactions: prepared.topology_transactions.clone(),
            signature: keypair.sign(&prepared.multi_hash).to_vec(),
            signed_by: prepared.public_key_fingerprint.clone(),
        }
    }
}

/// Authorize hosting the external party on this node's participant by submitting
/// the party-signed onboarding `bundle` via `AllocateExternalParty`. Called by
/// the coordinator for its own participant and by each hosting peer for theirs.
///
/// An `ALREADY_EXISTS` response is treated as success so a resumed run or a
/// re-sent fan-out converges instead of failing.
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
    };

    let mut client = utils::create_party_client(config, external_party_token()).await?;
    match client
        .allocate_external_party(tonic::Request::new(request))
        .await
    {
        Ok(_) => Ok(()),
        Err(status) if status.code() == tonic::Code::AlreadyExists => {
            tracing::info!("external-party already allocated on this node; treating as success");
            Ok(())
        }
        Err(status) => Err(anyhow::anyhow!(
            "AllocateExternalParty RPC failed: {status}"
        )),
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
///   token in the per-party auth registry yet. Wiring one in is the open item
///   (see `docs/decentralice-external-party-v0.md`).
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
