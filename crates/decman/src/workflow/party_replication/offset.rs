//! Ledger-offset capture for a replication run.
//!
//! Both offsets a replication needs name a point BEFORE the party is activated
//! on the target, which is why the capture is once-only and why finding the
//! offset is three tiers deep rather than one call.

use canton_proto_rs::com::{
    daml::ledger::api::v2::GetLedgerEndRequest,
    digitalasset::canton::admin::participant::v30::{
        GetHighestOffsetByTimestampRequest,
        party_management_service_client::PartyManagementServiceClient,
    },
};
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    utils::{self, get_synchronizer_id},
    workflow::storage::WorkflowStorage,
};

/// Capture this participant's ledger offset into `kind` exactly once.
///
/// Both offsets a replication needs — the target's pre-activation offset,
/// scoped to the participant, and the source's export offset, unscoped — exist
/// to name a point BEFORE the party is activated. A resumed run that re-enters its step after
/// the activation must therefore keep the original value: re-capturing would
/// move the offset past the activation, and `ExportPartyAcs` /
/// `ClearPartyOnboardingFlag` would then look for it in a window that no longer
/// contains it.
///
/// So the first capture wins and later calls are a no-op.
pub async fn capture_offset_once(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    kind: &str,
    scope: Option<&str>,
    ledger_token: Option<&str>,
    label: &str,
) -> Result {
    if storage
        .read_artifact(instance_name, kind, scope)
        .await?
        .is_some()
    {
        return Ok(());
    }
    let offset = current_ledger_offset(config, ledger_token).await?;
    storage
        .write_artifact(instance_name, kind, scope, offset.to_string().as_bytes())
        .await?;
    tracing::info!("Captured {label} ledger offset {offset}");
    Ok(())
}

/// Current ledger offset on this participant — the `begin_offset_exclusive`
/// for the activation finders behind `ExportPartyAcs` and
/// `ClearPartyOnboardingFlag`.
///
/// The offset must postdate any EARLIER activation of the same (party,
/// participant) pair: the finders take the FIRST activation event published
/// after the offset, so on a kick-then-re-add path a too-early offset
/// surfaces the stale flag-less activation and the export aborts with
/// INVALID_STATE (observed in CI). Tiers:
///
/// 1. Ledger API `GetLedgerEnd`, with the party's ledger token when the
///    auth registry has one: exact and always current.
/// 2. Admin API `GetHighestOffsetByTimestamp` — strict, then `force: true`.
///    "Now" routinely trips the clean-watermark check, and even forced
///    lookups can fail when the latest events have no synchronizer mapping
///    (both observed in CI).
/// 3. Offset 1 — the smallest POSITIVE value the consumers accept. Loudly
///    warned: correct only when the participant was never hosted on the
///    party before (no stale activation to trip over).
pub async fn current_ledger_offset(config: &NodeConfig, ledger_token: Option<&str>) -> Result<i64> {
    match ledger_end_offset(config, ledger_token).await {
        Ok(offset) if offset > 0 => return Ok(offset),
        Ok(offset) => {
            tracing::warn!("Ledger end reported non-positive offset {offset}; trying admin API");
        }
        Err(e) => {
            tracing::warn!("GetLedgerEnd unavailable ({e}); trying admin API");
        }
    }

    // PartyManagementService wants the LOGICAL synchronizer id
    // (`alias::fingerprint`) — the physical id's trailing `::<version>`
    // fails Canton's fingerprint decoding with a reserved-delimiter error.
    let synchronizer_id =
        utils::extract_synchronizer_fingerprint(&get_synchronizer_id(config).await?)?;
    let mut client = PartyManagementServiceClient::new(config.admin_channel().await?);

    for force in [false, true] {
        let now = std::time::SystemTime::now();
        let request = tonic::Request::new(GetHighestOffsetByTimestampRequest {
            synchronizer_id: synchronizer_id.clone(),
            timestamp: Some(prost_types::Timestamp::from(now)),
            force,
        });

        match client.get_highest_offset_by_timestamp(request).await {
            Ok(response) => {
                let offset = response.into_inner().ledger_offset;
                if offset > 0 {
                    return Ok(offset);
                }
                tracing::warn!(
                    "GetHighestOffsetByTimestamp (force: {force}) returned non-positive \
                     offset {offset}; retrying"
                );
            }
            Err(status) => {
                tracing::warn!("GetHighestOffsetByTimestamp (force: {force}) failed: {status}");
            }
        }
    }

    tracing::warn!(
        "No offset API usable on this participant; using offset 1 as \
         begin_offset_exclusive — UNSAFE if this participant hosted the party \
         before (a stale activation would be found first)"
    );
    Ok(1)
}

/// Ledger API ledger end. Authenticated when a token is supplied; the
/// tokenless form still works on deployments without ledger-API auth.
async fn ledger_end_offset(config: &NodeConfig, token: Option<&str>) -> Result<i64> {
    let mut client = utils::create_state_client(config, token.map(str::to_owned)).await?;
    let response = client
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {}))
        .await?
        .into_inner();
    Ok(response.offset)
}
