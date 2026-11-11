//! Onboarding a co-validated party across every host.

use common::api::{TenantExecuteRequest, TenantPrepareRequest};

use super::host::progress_to_status;
use super::{HostReport, OnboardedParty, WalletHost};
use crate::error::{Error, Result};
use crate::signer::Signer;

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
pub async fn execute_co_validated(
    hosts: &[WalletHost],
    key: &dyn Signer,
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
        signatures.push(key.sign_b64_async(&hash).await?);
    }

    let execute_request = TenantExecuteRequest {
        party_hint: party_hint.to_string(),
        public_key: public_key.clone(),
        topology_transactions: prepared.topology_transactions.clone(),
        signatures,
        signed_by: key.fingerprint(),
    };

    let mut reports = Vec::with_capacity(hosts.len());
    for host in hosts {
        let report = match host.client.execute(&execute_request).await {
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
                tracing::warn!(host = host.client.base_url(), "execute failed: {e}");
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
