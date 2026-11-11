//! Adding hosts to a party that exists, and replicating its contracts.

use std::time::Duration;

use common::api::TenantAddHostsExecuteRequest;
use common::api::{HostPermission, TenantAcsImportRequest, TenantAddHostsRequest};
use serde::Serialize;

use super::host::progress_to_status;
use super::{HostReport, WalletHost};
use crate::client::HostStatus;
use crate::error::{Error, Result};
use crate::signer::Signer;

/// How long to wait for a joiner's onboarding marker to clear, and how often to
/// ask. Clearing is a topology write, so it takes a synchronizer round trip and
/// the host's own signature; two minutes is generous for that and short enough
/// that a stuck synchronizer is reported rather than waited on forever.
const MARKER_CLEAR_TIMEOUT: Duration = Duration::from_secs(120);
const MARKER_CLEAR_POLL: Duration = Duration::from_secs(3);

/// Poll one joiner until it stops carrying the onboarding marker.
///
/// The import endpoint asks Canton to clear and returns; Canton authorizes the
/// clearing transaction on its own schedule. `Hosted` is reported only once the
/// marker is gone, so this is the wallet's read of "the party is usable there".
async fn wait_for_marker_clear(host: &WalletHost, party_id: &str) -> bool {
    let deadline = tokio::time::Instant::now() + MARKER_CLEAR_TIMEOUT;
    loop {
        match host.client.host_status(party_id).await {
            Ok(HostStatus::Hosted) => return true,
            Ok(_) => {}
            Err(e) => tracing::warn!(
                host = host.client.base_url(),
                "reading the onboarding status failed: {e}"
            ),
        }
        if tokio::time::Instant::now() + MARKER_CLEAR_POLL >= deadline {
            tracing::warn!(
                host = host.client.base_url(),
                "the onboarding marker is still set; the party is hosted here and not yet usable"
            );
            return false;
        }
        tokio::time::sleep(MARKER_CLEAR_POLL).await;
    }
}

/// The outcome of adding hosts to a party that already exists.
#[derive(Clone, Debug, Serialize)]
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
/// The threshold is left alone. Raising it belongs in
/// [`raise_threshold`](super::raise_threshold), after the markers clear,
/// because a marked host cannot confirm and so cannot count toward it.
///
/// `permission` picks between the two offers. [`HostPermission::Confirmation`]
/// is co-validation: the hosts confirm for the party and the threshold can rise
/// afterwards. [`HostPermission::Submission`] is failover-only — any one host
/// can submit for the party, so uptime improves with no change to the party's
/// application, but each host acts alone and the threshold must stay 1. Canton
/// enforces that last part, not this crate.
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
    permission: HostPermission,
    base_serial: u32,
) -> Result<AddedHosts> {
    if new_hosts.is_empty() {
        return Err(Error::NoHosts {
            operation: "adding hosts",
            role: "joining host",
        });
    }
    // A current host is needed as an ACS source, not merely as a signer.
    if current_hosts.is_empty() {
        return Err(Error::NoHosts {
            operation: "adding hosts",
            role: "host that already holds the party",
        });
    }

    let request = TenantAddHostsRequest {
        party_id: party_id.to_string(),
        permission,
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
        return Err(Error::NoHosts {
            operation: "adding hosts",
            role: "host",
        });
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

    let execute_request = TenantAddHostsExecuteRequest {
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
        let report = match host.client.add_hosts_execute(&execute_request).await {
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
        // Relay the ACS block by block. The source holds its Canton export
        // stream open, the joiner holds its import stream open, and this loop
        // carries one block between them — so neither node buffers the snapshot
        // and the size of the party never decides whether the transfer is
        // possible. The same shape the add-party path runs over Noise; here the
        // wallet stands in for the channel those two nodes do not have.
        //
        // Forward-only, so there is no resume. A break restarts from block 1
        // with a fresh export, which is what the export stream's inability to
        // rewind forces and what the add-party path accepts too.
        let mut seq = 1u64;
        let mut failed = false;
        // Fixed once the first block lands. Any current host can serve the
        // export — they all hold the party — but blocks stitched from two
        // exports are not a snapshot, so the fallback only applies before
        // anything has been relayed.
        let mut source: Option<&WalletHost> = None;
        loop {
            let block = match source {
                Some(source) => match source
                    .client
                    .acs_block(party_id, &host.participant_id, base_serial, seq)
                    .await
                {
                    Ok(block) => Some(block),
                    Err(e) => {
                        tracing::warn!(
                            source = source.client.base_url(),
                            host = host.client.base_url(),
                            seq,
                            "ACS export failed: {e}"
                        );
                        None
                    }
                },
                None => {
                    let mut found = None;
                    for candidate in current_hosts {
                        match candidate
                            .client
                            .acs_block(party_id, &host.participant_id, base_serial, seq)
                            .await
                        {
                            Ok(block) => {
                                source = Some(candidate);
                                found = Some(block);
                                break;
                            }
                            Err(e) => tracing::warn!(
                                source = candidate.client.base_url(),
                                host = host.client.base_url(),
                                "ACS export failed: {e}"
                            ),
                        }
                    }
                    found
                }
            };
            let Some(block) = block else {
                failed = true;
                break;
            };
            if seq == 1 && !block.package_preflight {
                without_package_preflight.push(host.client.base_url().to_string());
            }

            let import = TenantAcsImportRequest {
                party_id: party_id.to_string(),
                // The same serial the topology write was pinned to, so a target
                // removed and re-added does not inherit the earlier attempt's
                // session.
                base_serial,
                seq: block.seq,
                chunk: block.chunk,
                end: block.end,
                total_len: block.total_len,
                sha256: block.sha256,
                package_ids: block.package_ids,
            };
            // Returns once Canton has taken the block, which is what paces this
            // loop. No readahead: the joiner would have to buffer it.
            let progress = match host.client.acs_import(&import).await {
                Ok(progress) => progress,
                Err(e) => {
                    tracing::warn!(host = host.client.base_url(), seq, "ACS import failed: {e}");
                    failed = true;
                    break;
                }
            };

            if progress.complete {
                // `marker_cleared: false` is the ordinary answer, not a failure:
                // the host asks Canton to clear and returns rather than holding
                // the request open for the synchronizer's safe time. The party
                // is hosted but unusable until the clear is authorized, so wait
                // for it here.
                if !progress.marker_cleared && !wait_for_marker_clear(host, party_id).await {
                    failed = true;
                }
                break;
            }
            seq += 1;
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
