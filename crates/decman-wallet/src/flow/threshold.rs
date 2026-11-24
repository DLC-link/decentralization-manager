//! Moving a party's confirmation threshold.

use common::api::{TenantThresholdExecuteRequest, TenantThresholdRequest};

use super::{
    HostReport, WalletHost,
    host::progress_to_status,
    sign::{require_agreement, sign_prepared},
};
use crate::{
    error::{Error, Result},
    signer::Signer,
};

/// Move a party's confirmation threshold.
///
/// A threshold change needs the party namespace alone, so unlike an add there is
/// no participant to co-sign. That does not make one host enough *here*: the
/// wallet cannot bind the hash it signs to the transaction it was shown, so
/// agreement between independent preparers is its only defence, and this
/// requires at least two.
///
/// Do this **after** the joiners' markers clear. A marked host cannot confirm, so
/// a threshold raised to include one is a threshold the party cannot meet.
///
/// # Errors
/// As [`add_hosts`](super::add_hosts), plus whatever the node returns for a
/// threshold its hosts cannot field.
pub async fn raise_threshold(
    hosts: &[WalletHost],
    key: &dyn Signer,
    party_id: &str,
    new_threshold: u32,
    base_serial: u32,
) -> Result<Vec<HostReport>> {
    // Two, not one. The wallet signs a hash it cannot parse, so a lone preparer
    // could return a benign-looking threshold transaction alongside the hash of
    // a different mapping and collect a valid party signature for it. Comparing
    // what several hosts independently built is the only check available, and it
    // is not optional just because a threshold change needs fewer signatures
    // than an add.
    if hosts.len() < 2 {
        return Err(Error::NotEnoughHosts(hosts.len()));
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
        return Err(Error::NoHosts {
            operation: "changing the threshold",
            role: "host",
        });
    };
    require_agreement(preparer, prepared, others, |other, prepared| {
        (other.topology_transactions != prepared.topology_transactions
            || other.transaction_hashes != prepared.transaction_hashes
            || other.serial != prepared.serial)
            .then(|| "different threshold-change bytes".to_string())
    })?;

    // One hash per transaction, checked before signing anything. Without this a
    // preparer can append the hash of a different topology and get it signed:
    // the extra signature travels with the bundle and authorizes a mapping the
    // wallet never saw.
    let signatures = sign_prepared(
        preparer,
        &prepared.transaction_hashes,
        &prepared.topology_transactions,
        key,
    )
    .await?;

    let execute_request = TenantThresholdExecuteRequest {
        party_id: party_id.to_string(),
        base_serial,
        topology_transactions: prepared.topology_transactions.clone(),
        signatures,
        signed_by: key.fingerprint(),
    };

    let mut reports = Vec::with_capacity(hosts.len());
    for host in hosts {
        let report = match host.client.threshold_execute(&execute_request).await {
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
