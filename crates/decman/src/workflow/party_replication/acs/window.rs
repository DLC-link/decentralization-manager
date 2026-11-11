use std::time::Duration;

use canton_proto_rs::com::digitalasset::canton::admin::{
    participant::v30::{
        DisconnectSynchronizerRequest, ListConnectedSynchronizersRequest,
        ListRegisteredSynchronizersRequest, ModifySynchronizerRequest,
        ReconnectSynchronizersRequest, SynchronizerConnectionConfig,
        list_connected_synchronizers_response,
        synchronizer_connectivity_service_client::SynchronizerConnectivityServiceClient,
    },
    sequencer::v30::SequencerConnectionValidation,
};
use sqlx::SqlitePool;

use super::import::DISCONNECT_FIRST_MARKER;
use crate::{
    config::NodeConfig,
    consts::{topology_retry_delay_secs, topology_retry_max_attempts},
    error::Result,
    workflow::party_replication::ReplicationTarget,
};

/// Close the window: reconnect, confirm the synchronizer is healthy, and hand
/// it back to automatic connection when the window had put it on manual.
///
/// Every part runs whatever the others did. Manual connection has to come off
/// before the reconnect, or the reconnect skips the synchronizer; if that
/// fails, the reconnect is attempted anyway and the reset is retried once the
/// participant is connected. A participant left on manual connection stays
/// disconnected after its next restart, so that outcome is an error even when
/// it is connected now.
pub(super) async fn close_import_window(config: &NodeConfig, manual_connection: bool) -> Result {
    let mut manual_err = if manual_connection {
        set_manual_connect(config, false).await.err()
    } else {
        None
    };
    let reconnect = reconnect_and_verify_healthy(config).await;
    if manual_err.is_some() && reconnect.is_ok() {
        manual_err = set_manual_connect(config, false).await.err();
    }
    match (reconnect, manual_err) {
        (Ok(()), None) => Ok(()),
        (Ok(()), Some(e)) => Err(e.context(format!(
            "the participant is connected but its synchronizer '{alias}' stays on manual \
             connection, so its next restart leaves it disconnected; set manual_connect to \
             false on it",
            alias = config.synchronizer()
        ))),
        (Err(reconnect_err), None) => Err(reconnect_err),
        (Err(reconnect_err), Some(manual_err)) => Err(reconnect_err.context(format!(
            "and the synchronizer could not be taken off manual connection either: \
             {manual_err:#}"
        ))),
    }
}

/// Open the disconnect window before the party is authorized onto this
/// participant: put the synchronizer on manual connection so a restart cannot
/// reconnect into the window, record the marker, and disconnect.
///
/// Canton's offline party replication keeps the target off the synchronizer
/// from before the onboarding mapping becomes effective until the ACS import
/// has landed. A participant that is connected in between receives the party's
/// traffic and journals every archive of a contract it never activated, and the
/// next replay of that journal is fatal. Idempotent: a window that is already
/// open is left as it is.
///
/// # Errors
/// Returns an error if the synchronizer is not registered, or if Canton
/// refuses the configuration change or a disconnect.
pub async fn open_import_window(
    config: &NodeConfig,
    storage: &SqlitePool,
    target: &ReplicationTarget,
) -> Result {
    set_manual_connect(config, true).await?;
    target
        .write_artifact(
            storage,
            target.artifacts.import_inflight,
            None,
            DISCONNECT_FIRST_MARKER,
        )
        .await?;

    let mut connectivity =
        SynchronizerConnectivityServiceClient::new(config.admin_channel().await?);
    let connected = connectivity
        .list_connected_synchronizers(tonic::Request::new(ListConnectedSynchronizersRequest {}))
        .await?
        .into_inner()
        .connected_synchronizers;
    for s in &connected {
        connectivity
            .disconnect_synchronizer(tonic::Request::new(DisconnectSynchronizerRequest {
                synchronizer_alias: s.synchronizer_alias.clone(),
            }))
            .await?;
    }
    tracing::info!(
        "ACS import window open: {count} synchronizer(s) disconnected, manual connection on",
        count = connected.len()
    );
    Ok(())
}

/// Set `manual_connect` on the registered configuration of this node's
/// synchronizer. The sequencer connections are left untouched, so Canton is
/// told not to validate them.
pub(super) async fn set_manual_connect(config: &NodeConfig, manual: bool) -> Result {
    let alias = config.synchronizer();
    let mut connectivity =
        SynchronizerConnectivityServiceClient::new(config.admin_channel().await?);
    let mut connection = registered_connection(&mut connectivity, alias).await?;
    if connection.manual_connect == manual {
        return Ok(());
    }
    connection.manual_connect = manual;
    connectivity
        .modify_synchronizer(tonic::Request::new(ModifySynchronizerRequest {
            physical_synchronizer_id: None,
            new_config: Some(connection),
            sequencer_connection_validation: SequencerConnectionValidation::Disabled as i32,
        }))
        .await?;
    tracing::info!("synchronizer '{alias}' manual connection set to {manual}");
    Ok(())
}

/// Whether this node's synchronizer is set to manual connection. DecMan sets
/// that when it keeps the participant off the synchronizer on purpose.
pub(crate) async fn manual_connect_enabled(config: &NodeConfig) -> Result<bool> {
    let mut connectivity =
        SynchronizerConnectivityServiceClient::new(config.admin_channel().await?);
    Ok(
        registered_connection(&mut connectivity, config.synchronizer())
            .await?
            .manual_connect,
    )
}

async fn registered_connection(
    connectivity: &mut SynchronizerConnectivityServiceClient<tonic::transport::Channel>,
    alias: &str,
) -> Result<SynchronizerConnectionConfig> {
    connectivity
        .list_registered_synchronizers(tonic::Request::new(ListRegisteredSynchronizersRequest {
            all_statuses: false,
        }))
        .await?
        .into_inner()
        .results
        .into_iter()
        .filter_map(|r| r.config)
        .find(|c| c.synchronizer_alias == alias)
        .ok_or_else(|| {
            anyhow::anyhow!("synchronizer '{alias}' is not registered on this participant")
        })
}

/// Reconnect the participant to all registered synchronizers and confirm the
/// connection is genuinely healthy. `ReconnectSynchronizers` returning `Ok`
/// only means the request was accepted, not that replay succeeded — so we poll
/// `ListConnectedSynchronizers` until the configured synchronizer reports
/// healthy. A participant that reconnects but then crash-loops (or whose admin
/// API is unreachable) is caught here and surfaced as an error rather than
/// reported as a successful import.
pub(super) async fn reconnect_and_verify_healthy(config: &NodeConfig) -> Result {
    let mut connectivity =
        SynchronizerConnectivityServiceClient::new(config.admin_channel().await?);
    connectivity
        .reconnect_synchronizers(tonic::Request::new(ReconnectSynchronizersRequest {
            ignore_failures: false,
        }))
        .await?;
    verify_healthy(config).await
}

/// Poll `ListConnectedSynchronizers` until the configured synchronizer reports
/// healthy, or give up after the topology retry budget. Never reconnects.
pub(crate) async fn verify_healthy(config: &NodeConfig) -> Result {
    let alias = config.synchronizer();
    let max_attempts = topology_retry_max_attempts();
    let retry_delay = Duration::from_secs(topology_retry_delay_secs());
    // Track the most recent reason we're not yet healthy so the final error
    // carries the actionable root cause (admin API unreachable / RPC error /
    // connected-but-unhealthy) rather than a generic timeout message.
    let mut last_reason = "no successful status poll".to_string();
    for attempt in 1..=max_attempts {
        // Fresh client each poll: a crash-looping participant drops its admin
        // API, so a connect/RPC error is itself the signal recovery has failed.
        let healthy = match config.admin_channel().await {
            Ok(channel) => match SynchronizerConnectivityServiceClient::new(channel)
                .list_connected_synchronizers(tonic::Request::new(
                    ListConnectedSynchronizersRequest {},
                ))
                .await
            {
                Ok(resp) => {
                    let ok =
                        synchronizer_healthy(&resp.into_inner().connected_synchronizers, alias);
                    if !ok {
                        last_reason = format!("'{alias}' not connected or not reporting healthy");
                    }
                    ok
                }
                Err(status) => {
                    tracing::warn!(
                        "ListConnectedSynchronizers failed \
                         (attempt {attempt}/{max_attempts}): {status}"
                    );
                    last_reason = format!("ListConnectedSynchronizers RPC error: {status}");
                    false
                }
            },
            Err(e) => {
                tracing::warn!(
                    "participant admin API unreachable (attempt {attempt}/{max_attempts}): {e}"
                );
                last_reason = format!("admin API unreachable: {e}");
                false
            }
        };
        if healthy {
            return Ok(());
        }
        if attempt < max_attempts {
            tokio::time::sleep(retry_delay).await;
        }
    }
    anyhow::bail!(
        "synchronizer '{alias}' did not become healthy within {max_attempts} attempts \
         (last: {last_reason})"
    )
}

/// True iff the participant reports an active, healthy connection to `alias`.
/// The safety-critical predicate behind `reconnect_and_verify_healthy`: a
/// reconnect only counts as successful when the synchronizer is both present in
/// the connected set AND flagged healthy — never on the mere presence of a row
/// or an accepted `ReconnectSynchronizers` request. Reporting a half-reconnected
/// (or crash-looping) participant as success is exactly the failure this fix
/// exists to prevent.
pub(crate) fn synchronizer_healthy(
    connected: &[list_connected_synchronizers_response::Result],
    alias: &str,
) -> bool {
    connected
        .iter()
        .any(|s| s.synchronizer_alias == alias && s.healthy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected(alias: &str, healthy: bool) -> list_connected_synchronizers_response::Result {
        list_connected_synchronizers_response::Result {
            synchronizer_alias: alias.to_string(),
            healthy,
            ..Default::default()
        }
    }

    #[test]
    fn healthy_only_when_alias_present_and_flagged_healthy() {
        let alias = "global-domain";

        // Present and healthy → the reconnect is genuinely complete.
        assert!(synchronizer_healthy(&[connected(alias, true)], alias));

        // Present but NOT healthy → reconnect accepted but replay not done yet;
        // must not be reported as success (the crux of the fix).
        assert!(!synchronizer_healthy(&[connected(alias, false)], alias));

        // Absent → participant still disconnected (or connected elsewhere).
        assert!(!synchronizer_healthy(&[connected("other", true)], alias));

        // Empty → fully disconnected / crash-looping.
        assert!(!synchronizer_healthy(&[], alias));

        // The right synchronizer healthy among several → success.
        assert!(synchronizer_healthy(
            &[connected("other", false), connected(alias, true)],
            alias,
        ));
    }
}
