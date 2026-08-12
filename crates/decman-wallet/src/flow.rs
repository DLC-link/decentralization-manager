//! The wallet-driven flows: onboard a co-validated party, then transact as it.
//!
//! The shape of onboarding is deliberate and load-bearing: **one** host builds
//! the topology, the wallet signs it **locally**, and the wallet then submits the
//! same signed bundle to **every** host itself. No host relays to another, and no
//! host ever sees the private key. Canton keeps the topology a proposal until the
//! last host has signed, so a party is only live once every host reports it.

use common::{
    api::{
        TenantAcceptTransferRequest, TenantCommand, TenantCreateCommand, TenantDisclosedContract,
        TenantExecuteSubmissionRequest, TenantExerciseCommand, TenantOnboardRequest,
        TenantPrepareRequest, TenantPrepareSubmissionRequest, TenantPrepareSubmissionResponse,
        TenantTemplateId, TenantTransferRequest,
    },
    canton_id::CantonId,
    types::WorkflowProgress,
};
use serde::Serialize;

use crate::{
    client::{HostStatus, TenantClient},
    error::{Error, Result},
    key::ExternalKeyPair,
};

/// One participant that will host the party, and the DecMan instance in front of
/// it. A wallet configures these once — they are the hosting set it chose.
#[derive(Clone)]
pub struct WalletHost {
    pub client: TenantClient,
    /// This host's Canton participant id.
    pub participant_id: CantonId,
}

impl WalletHost {
    pub fn new(client: TenantClient, participant_id: CantonId) -> Self {
        Self {
            client,
            participant_id,
        }
    }
}

/// What one host reports about the party, or why it could not be reached.
#[derive(Clone, Debug, Serialize)]
pub struct HostReport {
    pub base_url: String,
    pub participant_id: String,
    /// `None` when the call to this host failed — see `error`.
    pub status: Option<HostStatus>,
    /// `None` when the call succeeded.
    pub error: Option<String>,
}

impl HostReport {
    fn ok(host: &WalletHost, status: HostStatus) -> Self {
        Self {
            base_url: host.client.base_url().to_string(),
            participant_id: host.participant_id.to_string(),
            status: Some(status),
            error: None,
        }
    }

    fn failed(host: &WalletHost, error: &Error) -> Self {
        Self {
            base_url: host.client.base_url().to_string(),
            participant_id: host.participant_id.to_string(),
            status: None,
            error: Some(error.to_string()),
        }
    }

    /// Whether this host has the party live.
    pub fn is_hosted(&self) -> bool {
        self.status.is_some_and(HostStatus::is_hosted)
    }
}

/// The outcome of driving onboarding across every host.
#[derive(Clone, Debug, Serialize)]
pub struct OnboardedParty {
    pub party_id: String,
    pub fingerprint: String,
    /// The party's public key, base64-encoded. Safe to publish — and the only
    /// half of the key any host ever sees.
    pub public_key: String,
    pub hosts: Vec<HostReport>,
}

impl OnboardedParty {
    /// Whether every host has authorized the party. Until this is true the
    /// topology is still a proposal somewhere and the party cannot transact.
    pub fn fully_hosted(&self) -> bool {
        !self.hosts.is_empty() && self.hosts.iter().all(HostReport::is_hosted)
    }
}

/// Map a host's own progress report onto the status vocabulary. A host answers
/// `Completed` once its authorized mapping names the party; anything else means
/// it is still working through the proposal.
fn progress_to_status(progress: WorkflowProgress) -> HostStatus {
    if progress == WorkflowProgress::Completed {
        HostStatus::Hosted
    } else {
        HostStatus::Pending
    }
}

/// Onboard a co-validated external party across `hosts`.
///
/// Asks **every** host to prepare the topology, requires their answers to be
/// byte-identical, signs each transaction hash with `key` locally, then submits that
/// one signed bundle to every host. A host that rejects the *bundle* is recorded in
/// its [`HostReport`] rather than aborting the run: onboarding is idempotent, so the
/// caller can retry the stragglers.
///
/// Every host prepares because the wallet cannot check a single host's work: it
/// signs hashes it does not compute, so agreement between hosts is what stands
/// between it and a host that returns the hash of a mapping it never showed us. See
/// [`Error::HostDisagreement`].
///
/// `confirmation_threshold` is how many hosts must confirm a transaction. `None`
/// lets the node default it to `N-1`, which is what keeps a host able to exit
/// later. The node rejects a threshold of `N`.
///
/// # Errors
/// Fails before signing anything if there are fewer than two hosts, if **any** host
/// cannot build the topology, if the hosts disagree about what to sign, or if the
/// prepared party id is not the one `key` derives — a mismatch there means the node
/// did not read the public key we sent, and signing would bind the wallet to the
/// wrong party.
pub async fn onboard_co_validated(
    hosts: &[WalletHost],
    key: &ExternalKeyPair,
    party_hint: &str,
    confirmation_threshold: Option<u32>,
) -> Result<OnboardedParty> {
    if hosts.len() < 2 {
        return Err(Error::NotEnoughHosts(hosts.len()));
    }

    let public_key = key.public_key_b64();

    // Prepare on *every* host and require them to agree, rather than trusting one.
    //
    // The wallet signs a hash it cannot parse — it does not implement Canton's
    // topology hashing — so on its own it has no way to know that the hash a host
    // returned belongs to the transaction that host showed it. Since Canton 3.5 the
    // party's signing keys ride inside `PartyToParticipant`, so a single lying
    // preparer could return plausible bytes alongside the hash of a mapping that
    // adds *its* key to `party_signing_keys`: the wallet's signature would authorize
    // it, because the party's namespace is the wallet's key, and that host could
    // then act as the party.
    //
    // Comparing what every host independently prepared closes that: one honest host
    // is enough to expose a lying one, and the wallet still re-derives no crypto.
    // This costs no availability — Canton keeps the topology a proposal until every
    // host authorizes it, so onboarding already needs all N up.
    let mut prepared_by_host = Vec::with_capacity(hosts.len());
    for host in hosts {
        // Each host is told to host the party alongside all the others.
        let hosting_peers = hosts
            .iter()
            .filter(|peer| peer.participant_id != host.participant_id)
            .map(|peer| peer.participant_id.clone())
            .collect();
        let prepared = host
            .client
            .prepare(&TenantPrepareRequest {
                party_hint: party_hint.to_string(),
                public_key: public_key.clone(),
                hosting_peers,
                confirmation_threshold,
            })
            .await?;
        prepared_by_host.push((host, prepared));
    }

    let Some(((preparer, prepared), others)) = prepared_by_host.split_first() else {
        return Err(Error::NotEnoughHosts(0));
    };

    let expected_party_id = key.party_id(party_hint);
    if prepared.party_id != expected_party_id {
        return Err(Error::PartyIdMismatch {
            host: preparer.client.base_url().to_string(),
            expected: expected_party_id,
            returned: prepared.party_id.clone(),
        });
    }

    // Byte-identical or nothing. The node sorts the hosting set precisely so every
    // host serializes the same mapping; any difference here is a host that built
    // something else.
    for (host, other) in others {
        let disagreement = if other.party_id != prepared.party_id {
            Some(format!(
                "party id {found} vs {expected}",
                found = other.party_id,
                expected = prepared.party_id
            ))
        } else if other.topology_transactions != prepared.topology_transactions {
            Some("different topology transactions".to_string())
        } else if other.transaction_hashes != prepared.transaction_hashes {
            Some("same transactions but different hashes to sign".to_string())
        } else {
            None
        };
        if let Some(detail) = disagreement {
            return Err(Error::HostDisagreement {
                host: host.client.base_url().to_string(),
                reference: preparer.client.base_url().to_string(),
                detail,
            });
        }
    }

    // One signature per transaction, over the hash Canton computed for it. Signing
    // Canton's hashes directly means nothing here re-derives a Canton hash.
    if prepared.transaction_hashes.len() != prepared.topology_transactions.len() {
        return Err(Error::MalformedPreparation {
            host: preparer.client.base_url().to_string(),
            detail: format!(
                "{hashes} hash(es) for {txs} transaction(s)",
                hashes = prepared.transaction_hashes.len(),
                txs = prepared.topology_transactions.len()
            ),
        });
    }
    let mut signatures = Vec::with_capacity(prepared.transaction_hashes.len());
    for encoded in &prepared.transaction_hashes {
        let hash = preparer.client.decode_b64("transaction_hash", encoded)?;
        signatures.push(key.sign_b64(&hash));
    }

    let onboard_request = TenantOnboardRequest {
        party_hint: party_hint.to_string(),
        public_key: public_key.clone(),
        topology_transactions: prepared.topology_transactions.clone(),
        signatures,
        signed_by: key.fingerprint(),
    };

    let mut reports = Vec::with_capacity(hosts.len());
    for host in hosts {
        let report = match host.client.onboard(&onboard_request).await {
            Ok(resp) => {
                tracing::info!(
                    host = host.client.base_url(),
                    party_id = %resp.party_id,
                    status = resp.status.as_str(),
                    "onboarded external party on host"
                );
                HostReport::ok(host, progress_to_status(resp.status))
            }
            Err(e) => {
                tracing::warn!(host = host.client.base_url(), "onboard failed: {e}");
                HostReport::failed(host, &e)
            }
        };
        reports.push(report);
    }

    Ok(OnboardedParty {
        party_id: prepared.party_id.clone(),
        fingerprint: key.fingerprint(),
        public_key,
        hosts: reports,
    })
}

/// Ask every host where the party stands, one round. Authorization is not
/// instant — callers poll this until [`OnboardedParty::fully_hosted`]-equivalent
/// agreement, on whatever schedule suits them.
pub async fn statuses(hosts: &[WalletHost], party_id: &str) -> Vec<HostReport> {
    let mut reports = Vec::with_capacity(hosts.len());
    for host in hosts {
        let report = match host.client.host_status(party_id).await {
            Ok(status) => HostReport::ok(host, status),
            Err(e) => HostReport::failed(host, &e),
        };
        reports.push(report);
    }
    reports
}

/// Sign a prepared submission locally and execute it on the same host.
///
/// Only the signature crosses back — the node holds no key material and cannot
/// produce the submission on its own. Every way the party acts on the ledger
/// funnels through here, so there is exactly one place where the key is used to
/// authorize a transaction.
async fn sign_and_execute(
    host: &TenantClient,
    key: &ExternalKeyPair,
    party_id: &str,
    prepared: TenantPrepareSubmissionResponse,
) -> Result<()> {
    let hash = host.decode_b64(
        "prepared_transaction_hash",
        &prepared.prepared_transaction_hash,
    )?;

    host.execute_submission(
        party_id,
        &TenantExecuteSubmissionRequest {
            prepared_transaction: prepared.prepared_transaction,
            signature: key.sign_b64(&hash),
            signed_by: key.fingerprint(),
            hashing_scheme_version: prepared.hashing_scheme_version,
        },
    )
    .await
}

/// Create a contract as the party: prepare the CREATE on `host`, sign the
/// prepared transaction's hash locally, and execute it.
pub async fn create_contract(
    host: &TenantClient,
    key: &ExternalKeyPair,
    party_id: &str,
    template_id: TenantTemplateId,
    create_arguments: serde_json::Value,
) -> Result<()> {
    let prepared = host
        .prepare_submission(
            party_id,
            &TenantPrepareSubmissionRequest {
                command: TenantCommand::Create(TenantCreateCommand {
                    template_id,
                    create_arguments,
                }),
                disclosed_contracts: Vec::new(),
            },
        )
        .await?;

    sign_and_execute(host, key, party_id, prepared).await
}

/// Exercise a choice as the party.
///
/// `disclosed_contracts` covers contracts the choice's interpretation reads but
/// the party cannot see in its own ACS. For token-standard transfers prefer
/// [`send_transfer`] / [`accept_transfer`], which get those from the instrument's
/// registry instead of asking the caller to supply them.
pub async fn exercise_choice(
    host: &TenantClient,
    key: &ExternalKeyPair,
    party_id: &str,
    command: TenantExerciseCommand,
    disclosed_contracts: Vec<TenantDisclosedContract>,
) -> Result<()> {
    let prepared = host
        .prepare_submission(
            party_id,
            &TenantPrepareSubmissionRequest {
                command: TenantCommand::Exercise(command),
                disclosed_contracts,
            },
        )
        .await?;

    sign_and_execute(host, key, party_id, prepared).await
}

/// Send an asset as the party.
///
/// The host resolves the instrument's token-standard registry, the transfer
/// factory, and the choice context; the wallet signs the result. Works the same
/// for Canton Coin and for utility instruments like CBTC — the difference is a
/// registry lookup on the host's side, not a different flow here.
///
/// Whether the funds land immediately or wait for the receiver is the
/// instrument's rule, not this call's: a receiver with a `TransferPreapproval`
/// gets them at once, and a receiver without one — every freshly onboarded wallet
/// party — gets a `TransferInstruction` to [`accept_transfer`].
pub async fn send_transfer(
    host: &TenantClient,
    key: &ExternalKeyPair,
    party_id: &str,
    request: &TenantTransferRequest,
) -> Result<()> {
    let prepared = host.prepare_transfer(party_id, request).await?;
    sign_and_execute(host, key, party_id, prepared).await
}

/// Accept an inbound transfer as the party, taking the escrowed funds into its
/// own holdings. `transfer_instruction_cid` comes from
/// [`TenantClient::transfer_offers`].
pub async fn accept_transfer(
    host: &TenantClient,
    key: &ExternalKeyPair,
    party_id: &str,
    transfer_instruction_cid: &str,
) -> Result<()> {
    let prepared = host
        .prepare_accept(
            party_id,
            &TenantAcceptTransferRequest {
                transfer_instruction_cid: transfer_instruction_cid.to_string(),
            },
        )
        .await?;
    sign_and_execute(host, key, party_id, prepared).await
}
