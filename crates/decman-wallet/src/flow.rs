//! The wallet-driven flows: onboard a co-validated party, then transact as it.
//!
//! The shape of onboarding is deliberate and load-bearing: **one** host builds
//! the topology, the wallet signs it **locally**, and the wallet then submits the
//! same signed bundle to **every** host itself. No host relays to another, and no
//! host ever sees the private key. Canton keeps the topology a proposal until the
//! last host has signed, so a party is only live once every host reports it.

use common::{
    api::{
        TenantAcsImportRequest, TenantAddHostsOnboardRequest, TenantAddHostsRequest,
        TenantOnboardRequest, TenantPrepareRequest, TenantThresholdOnboardRequest,
        TenantThresholdRequest,
    },
    canton_id::CantonId,
    types::WorkflowProgress,
};
use serde::Serialize;

use crate::{
    client::{HostStatus, TenantClient},
    error::{Error, Result},
    signer::Signer,
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

/// The outcome of adding hosts to a party that already exists.
#[derive(Debug)]
pub struct AddedHosts {
    /// The party that gained hosts.
    pub party_id: String,
    /// The serial the add-hosts write carries.
    pub serial: u32,
    /// Per-host outcome of the topology write.
    pub hosts: Vec<HostReport>,
    /// Whether every joining host imported the ACS and cleared its onboarding
    /// marker. `false` means at least one is hosted but still suspended, holding
    /// none of the party's contracts.
    pub replicated: bool,
    /// Joining hosts whose source could not preflight the packages, so their
    /// import validated them only after disconnecting. Empty is the good case.
    pub without_package_preflight: Vec<String>,
}

/// Add `new_hosts` to a party that already exists, and replicate its contracts
/// onto them.
///
/// Three phases, in the order Canton forces:
///
/// 1. **Topology.** Every host prepares the serial-N+1 replace and the wallet
///    requires them to agree, for exactly the reason onboarding does: the wallet
///    signs a hash it cannot parse, so agreement between hosts is the only thing
///    standing between it and a host that returns the hash of a mapping it never
///    showed. Canton's rules need the party namespace plus each joining
///    participant, so only the joiners submit.
/// 2. **State.** A current host exports the ACS scoped to each joiner and the
///    wallet relays it. There is no host-to-host channel by design: a partner's
///    node is generally not in anyone else's mesh, and the wallet already talks
///    to all of them.
/// 3. **Activation.** The joiner clears Canton's onboarding marker, which is the
///    moment the party becomes usable there.
///
/// The threshold is left alone. Raising it belongs in [`raise_threshold`], after
/// the markers clear, because a marked host cannot confirm and so cannot count
/// toward it.
///
/// # Errors
/// Fails before signing if `new_hosts` is empty, if any host cannot prepare, or
/// if the hosts disagree about what to sign. A host that rejects the bundle
/// afterwards is recorded in its [`HostReport`] rather than aborting the run.
pub async fn add_hosts(
    current_hosts: &[WalletHost],
    new_hosts: &[WalletHost],
    key: &dyn Signer,
    party_id: &str,
    base_serial: u32,
) -> Result<AddedHosts> {
    if new_hosts.is_empty() {
        return Err(Error::NotEnoughHosts(0));
    }
    let Some(source) = current_hosts.first() else {
        return Err(Error::NotEnoughHosts(0));
    };

    let request = TenantAddHostsRequest {
        party_id: party_id.to_string(),
        new_hosts: new_hosts.iter().map(|h| h.participant_id.clone()).collect(),
        base_serial,
    };

    // Everyone prepares, joiners included: a joiner reads the mapping from the
    // shared synchronizer store, so it can check the others' work too.
    let mut prepared_by_host = Vec::with_capacity(current_hosts.len() + new_hosts.len());
    for host in current_hosts.iter().chain(new_hosts) {
        let prepared = host.client.add_hosts_prepare(&request).await?;
        prepared_by_host.push((host, prepared));
    }

    let Some(((preparer, prepared), others)) = prepared_by_host.split_first() else {
        return Err(Error::NotEnoughHosts(0));
    };
    for (host, other) in others {
        let disagreement = if other.topology_transactions != prepared.topology_transactions {
            Some("different topology transactions".to_string())
        } else if other.transaction_hashes != prepared.transaction_hashes {
            Some("same transactions but different hashes to sign".to_string())
        } else if other.serial != prepared.serial {
            Some(format!(
                "serial {found} vs {expected}",
                found = other.serial,
                expected = prepared.serial
            ))
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

    let onboard_request = TenantAddHostsOnboardRequest {
        party_id: party_id.to_string(),
        base_serial,
        topology_transactions: prepared.topology_transactions.clone(),
        signatures,
        signed_by: key.fingerprint(),
    };

    // Only the joiners submit: Canton needs the party namespace plus each new
    // participant, and the existing hosts are neither.
    let mut reports = Vec::with_capacity(new_hosts.len());
    for host in new_hosts {
        let report = match host.client.add_hosts_onboard(&onboard_request).await {
            Ok(resp) => {
                tracing::info!(
                    host = host.client.base_url(),
                    party_id = %resp.party_id,
                    serial = resp.serial,
                    "submitted add-hosts on joining host"
                );
                HostReport::ok(host, progress_to_status(resp.status))
            }
            Err(e) => {
                tracing::warn!(
                    host = host.client.base_url(),
                    "add-hosts submit failed: {e}"
                );
                HostReport::failed(host, &e)
            }
        };
        reports.push(report);
    }

    // Phase 2 and 3, per joiner. A joiner whose topology write failed is skipped:
    // Canton scopes the export to the joiner's activation, which does not exist.
    let mut replicated = true;
    let mut without_package_preflight = Vec::new();
    for (host, report) in new_hosts.iter().zip(&reports) {
        if report.error.is_some() {
            replicated = false;
            continue;
        }
        // Relay the snapshot a range at a time. Neither end holds the whole
        // thing in a request body, and a failure costs one range rather than the
        // transfer.
        //
        // The joiner is the authority on progress: it reports how much it holds
        // after every range and the next read starts from there. So a wallet
        // that died mid-transfer resumes correctly on a fresh run without having
        // persisted anything itself.
        let mut offset = 0u64;
        let mut seen_first_range = false;
        let mut failed = false;
        loop {
            let range = match source
                .client
                .acs_range(party_id, &host.participant_id, offset)
                .await
            {
                Ok(range) => range,
                Err(e) => {
                    tracing::warn!(
                        host = host.client.base_url(),
                        offset,
                        "ACS export failed: {e}"
                    );
                    failed = true;
                    break;
                }
            };
            if !seen_first_range {
                seen_first_range = true;
                if !range.package_preflight {
                    without_package_preflight.push(host.client.base_url().to_string());
                }
            }

            let import = TenantAcsImportRequest {
                party_id: party_id.to_string(),
                offset,
                total_size: range.total_size,
                chunk: range.chunk,
                package_ids: range.package_ids,
            };
            let progress = match host.client.acs_import(&import).await {
                Ok(progress) => progress,
                Err(e) => {
                    tracing::warn!(
                        host = host.client.base_url(),
                        offset,
                        "ACS import failed: {e}"
                    );
                    failed = true;
                    break;
                }
            };

            if progress.complete {
                if !progress.marker_cleared {
                    tracing::warn!(
                        host = host.client.base_url(),
                        "ACS imported but the onboarding marker is still set; the party is \
                         hosted here and not yet usable"
                    );
                    failed = true;
                }
                break;
            }

            // No forward progress means another round would send the same bytes
            // to the same offset forever.
            if progress.received <= offset {
                tracing::warn!(
                    host = host.client.base_url(),
                    offset,
                    received = progress.received,
                    "the joiner did not advance; abandoning this transfer rather than looping"
                );
                failed = true;
                break;
            }
            offset = progress.received;
        }
        if failed {
            replicated = false;
        }
    }

    Ok(AddedHosts {
        party_id: party_id.to_string(),
        serial: prepared.serial,
        hosts: reports,
        replicated,
        without_package_preflight,
    })
}

/// Move a party's confirmation threshold.
///
/// A threshold change needs the party namespace alone, so unlike an add there is
/// no participant to co-sign and one host is enough to carry it. Every host is
/// still asked to prepare and required to agree, because the wallet's inability
/// to parse the hash it signs does not change with the operation.
///
/// Do this **after** the joiners' markers clear. A marked host cannot confirm, so
/// a threshold raised to include one is a threshold the party cannot meet.
///
/// # Errors
/// As [`add_hosts`], plus whatever the node returns for a threshold its hosts
/// cannot field.
pub async fn raise_threshold(
    hosts: &[WalletHost],
    key: &dyn Signer,
    party_id: &str,
    new_threshold: u32,
    base_serial: u32,
) -> Result<Vec<HostReport>> {
    if hosts.is_empty() {
        return Err(Error::NotEnoughHosts(0));
    }

    let request = TenantThresholdRequest {
        party_id: party_id.to_string(),
        new_threshold,
        base_serial,
    };

    let mut prepared_by_host = Vec::with_capacity(hosts.len());
    for host in hosts {
        prepared_by_host.push((host, host.client.threshold_prepare(&request).await?));
    }
    let Some(((preparer, prepared), others)) = prepared_by_host.split_first() else {
        return Err(Error::NotEnoughHosts(0));
    };
    for (host, other) in others {
        if other.topology_transactions != prepared.topology_transactions
            || other.transaction_hashes != prepared.transaction_hashes
        {
            return Err(Error::HostDisagreement {
                host: host.client.base_url().to_string(),
                reference: preparer.client.base_url().to_string(),
                detail: "different threshold-change bytes".to_string(),
            });
        }
    }

    let mut signatures = Vec::with_capacity(prepared.transaction_hashes.len());
    for encoded in &prepared.transaction_hashes {
        let hash = preparer.client.decode_b64("transaction_hash", encoded)?;
        signatures.push(key.sign_b64_async(&hash).await?);
    }

    let onboard = TenantThresholdOnboardRequest {
        party_id: party_id.to_string(),
        base_serial,
        topology_transactions: prepared.topology_transactions.clone(),
        signatures,
        signed_by: key.fingerprint(),
    };

    let mut reports = Vec::with_capacity(hosts.len());
    for host in hosts {
        let report = match host.client.threshold_onboard(&onboard).await {
            Ok(resp) => HostReport::ok(host, progress_to_status(resp.status)),
            Err(e) => {
                tracing::warn!(
                    host = host.client.base_url(),
                    "threshold submit failed: {e}"
                );
                HostReport::failed(host, &e)
            }
        };
        reports.push(report);
    }
    Ok(reports)
}
