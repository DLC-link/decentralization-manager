//! Repair steps that take the participant off its synchronizer, shared by every
//! kind of purge: the checks before one, and the purge itself.

use std::time::Duration;

use canton_proto_rs::com::digitalasset::canton::admin::participant::v30::{
    DisconnectSynchronizerRequest, ListConnectedSynchronizersRequest, PurgeContractsRequest,
    ReconnectSynchronizerRequest,
    participant_repair_service_client::ParticipantRepairServiceClient,
    synchronizer_connectivity_service_client::SynchronizerConnectivityServiceClient,
};
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    db::schema::SchemaRead,
    error::Result,
    workflow::party_replication::acs::{synchronizer_healthy, verify_healthy},
};

/// Serializes the operations that take the participant off its synchronizer
/// or put it back. A purge holds it from its start until it records its
/// outcome. A reconnect takes it, or refuses while a purge holds it.
pub(crate) static CONNECTIVITY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Contract ids per `PurgeContracts` call. A contract id is about 140
/// characters, so this keeps each request far below the 4 MiB gRPC cap.
const PURGE_BATCH: usize = 500;

/// Attempts per batch. The call is idempotent with `ignore_already_purged`.
const PURGE_BATCH_ATTEMPTS: usize = 3;

const PURGE_RETRY_DELAY: Duration = Duration::from_secs(2);

/// The in-progress workflow runs, named for an operator, or `None` when there
/// are none.
pub(crate) async fn running_workflows(db: &SqlitePool) -> Result<Option<String>> {
    let runs = db.get_in_progress_workflow_runs().await?;
    Ok((!runs.is_empty()).then(|| {
        runs.iter()
            .map(|run| format!("{:?} {}", run.kind, run.instance_name))
            .collect::<Vec<_>>()
            .join(", ")
    }))
}

/// Whether the participant is connected and healthy on its synchronizer.
pub(crate) async fn connected_and_healthy(config: &NodeConfig) -> Result<bool> {
    let connected = SynchronizerConnectivityServiceClient::new(config.admin_channel().await?)
        .list_connected_synchronizers(tonic::Request::new(ListConnectedSynchronizersRequest {}))
        .await?
        .into_inner()
        .connected_synchronizers;
    Ok(synchronizer_healthy(&connected, config.synchronizer()))
}

/// Whether the participant serves `PurgeContracts`. With Canton's repair
/// commands off the method does not exist. The probe purges nothing: Canton
/// refuses a request without contract ids before it reads the synchronizer.
pub(crate) async fn purge_offered(config: &NodeConfig, alias: &str) -> Result<bool> {
    let probe = ParticipantRepairServiceClient::new(config.admin_channel().await?)
        .purge_contracts(tonic::Request::new(PurgeContractsRequest {
            synchronizer_alias: alias.to_string(),
            contract_ids: Vec::new(),
            ignore_already_purged: true,
            force_repair_when_topology_transaction_at_ledger_end: false,
        }))
        .await;
    purge_probe_result(probe.map(drop))
}

fn purge_probe_result(probe: std::result::Result<(), tonic::Status>) -> Result<bool> {
    match probe {
        Ok(()) => Ok(true),
        Err(status) => match status.code() {
            tonic::Code::Unimplemented => Ok(false),
            tonic::Code::InvalidArgument => Ok(true),
            _ => Err(status.into()),
        },
    }
}

/// Disconnect the participant from its synchronizer, run `recheck`, purge
/// `contract_ids`, then reconnect it and wait until it is healthy.
///
/// `recheck` runs once the participant's topology has stopped changing and
/// before anything is removed. An error from it stops the purge.
///
/// # Errors
///
/// Returns an error when the participant does not come back connected and
/// healthy, or when the disconnect, `recheck` or the purge fails.
pub(crate) async fn purge_while_disconnected(
    config: &NodeConfig,
    contract_ids: &[String],
    recheck: impl AsyncFnOnce() -> Result,
) -> Result {
    let alias = config.synchronizer().to_string();
    let mut connectivity =
        SynchronizerConnectivityServiceClient::new(config.admin_channel().await?);
    // Disconnect and purge as one fallible unit. The reconnect below runs on
    // every outcome, so the participant is never left off the synchronizer.
    let purged = async {
        connectivity
            .disconnect_synchronizer(tonic::Request::new(DisconnectSynchronizerRequest {
                synchronizer_alias: alias.clone(),
            }))
            .await?;
        recheck().await?;
        purge_in_batches(config, &alias, contract_ids).await
    }
    .await;

    let reconnected = async {
        connectivity
            .reconnect_synchronizer(tonic::Request::new(ReconnectSynchronizerRequest {
                synchronizer_alias: alias.clone(),
                retry: true,
            }))
            .await?;
        verify_healthy(config).await
    }
    .await;

    combine_outcomes(&alias, purged, reconnected)
}

/// An unhealthy participant is the failure an operator must act on first, so
/// it is reported even when the purge failed too.
fn combine_outcomes(alias: &str, purged: Result, reconnected: Result) -> Result {
    if let Err(reconnect_err) = reconnected {
        let purge_note = match &purged {
            Ok(()) => "the purge itself completed".to_string(),
            Err(e) => format!("the purge also failed: {e:#}"),
        };
        anyhow::bail!(
            "the participant did not come back connected and healthy on '{alias}' after the \
             purge ({purge_note}): {reconnect_err:#}"
        );
    }
    purged
}

async fn purge_in_batches(config: &NodeConfig, alias: &str, contract_ids: &[String]) -> Result {
    let mut repair = ParticipantRepairServiceClient::new(config.admin_channel().await?);
    for (index, batch) in contract_ids.chunks(PURGE_BATCH).enumerate() {
        let mut attempt = 1;
        loop {
            let request = PurgeContractsRequest {
                synchronizer_alias: alias.to_string(),
                contract_ids: batch.to_vec(),
                ignore_already_purged: true,
                force_repair_when_topology_transaction_at_ledger_end: false,
            };
            match repair.purge_contracts(tonic::Request::new(request)).await {
                Ok(_) => break,
                Err(status) if attempt < PURGE_BATCH_ATTEMPTS => {
                    tracing::warn!(
                        "PurgeContracts batch {} failed (attempt {attempt}/{PURGE_BATCH_ATTEMPTS}): \
                         {status}",
                        index + 1
                    );
                    attempt += 1;
                    tokio::time::sleep(PURGE_RETRY_DELAY).await;
                }
                Err(status) => anyhow::bail!(
                    "PurgeContracts failed on batch {} of {}: {status}",
                    index + 1,
                    contract_ids.len().div_ceil(PURGE_BATCH)
                ),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    #[test]
    fn a_missing_purge_method_means_repair_commands_are_off() -> Result {
        assert!(!purge_probe_result(Err(tonic::Status::unimplemented(
            "Method not found"
        )))?);
        assert!(purge_probe_result(Err(tonic::Status::invalid_argument(
            "Missing contract ids to purge"
        )))?);
        assert!(purge_probe_result(Err(tonic::Status::unavailable("down"))).is_err());
        Ok(())
    }

    #[test]
    fn a_healthy_reconnect_passes_the_purge_outcome_through() {
        assert!(combine_outcomes("global", Ok(()), Ok(())).is_ok());

        let purge_err = anyhow!("batch 1 refused").context("PurgeContracts failed");
        let err = combine_outcomes("global", Err(purge_err), Ok(())).unwrap_err();
        assert_eq!(format!("{err:#}"), "PurgeContracts failed: batch 1 refused");
    }

    #[test]
    fn a_failed_reconnect_is_reported_after_a_completed_purge() {
        let reconnect_err = anyhow!("not healthy").context("reconnect");
        let err = combine_outcomes("global", Ok(()), Err(reconnect_err)).unwrap_err();
        assert_eq!(
            err.to_string(),
            "the participant did not come back connected and healthy on 'global' after the \
             purge (the purge itself completed): reconnect: not healthy"
        );
    }

    #[test]
    fn a_failed_reconnect_is_reported_with_the_purge_failure() {
        let purge_err = anyhow!("batch 1 refused").context("PurgeContracts failed");
        let err =
            combine_outcomes("global", Err(purge_err), Err(anyhow!("not healthy"))).unwrap_err();
        assert_eq!(
            err.to_string(),
            "the participant did not come back connected and healthy on 'global' after the \
             purge (the purge also failed: PurgeContracts failed: batch 1 refused): not healthy"
        );
    }
}
