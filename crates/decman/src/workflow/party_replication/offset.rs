//! Ledger-offset capture for a replication run.
//!
//! Both offsets a replication needs name a point BEFORE the party is activated
//! on the target, which is why the capture is once-only and why finding the
//! offset is three tiers deep rather than one call.

use canton_proto_rs::com::{
    daml::ledger::api::v2::GetLedgerEndRequest,
    digitalasset::canton::{
        admin::participant::v30::{
            GetHighestOffsetByTimestampRequest,
            party_management_service_client::PartyManagementServiceClient,
        },
        protocol::v30::PartyToParticipant,
    },
};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    utils::{self, get_synchronizer_id},
    workflow::{
        party_replication::onboarding_flag::has_onboarding_marker, storage::WorkflowStorage,
        topology,
    },
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

/// The persisted offset for a replication, or one derived from the chain.
///
/// Falling back rather than failing is what lets a replication be picked up
/// after the run that started it is gone. `scope` mirrors
/// [`capture_offset_once`]: the export offset is unscoped, the target's
/// pre-activation offset is scoped to the participant.
///
/// A derived offset is written back under the same key, so the rest of the run
/// and any later retry read one stable value instead of re-deriving.
///
/// # Errors
/// Returns an error if nothing is persisted and the offset cannot be derived
/// (see [`derive_pre_activation_offset`]).
pub async fn persisted_or_derived_offset(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    kind: &str,
    scope: Option<&str>,
    party_id: &CantonId,
    target: &CantonId,
) -> Result<i64> {
    if let Some(bytes) = storage.read_artifact(instance_name, kind, scope).await? {
        return String::from_utf8(bytes)?
            .trim()
            .parse()
            .map_err(|e| anyhow::anyhow!("Failed to parse the persisted {kind}: {e}"));
    }

    tracing::warn!(
        "No {kind} was persisted for this run; deriving it from {target}'s activation \
         on {party_id}. Expected when a replication is resumed after the run that \
         started it was dismissed or swept."
    );
    let offset = derive_pre_activation_offset(config, party_id, target).await?;
    storage
        .write_artifact(instance_name, kind, scope, offset.to_string().as_bytes())
        .await?;
    Ok(offset)
}

/// The moment `target` was first activated on `party_id` with the onboarding
/// marker, from the party's `PartyToParticipant` history.
///
/// The EARLIEST matching serial, not the latest: a mapping re-issued after the
/// activation (a threshold change, another member joining) carries the marker
/// forward, and its `valid_from` is long after the activation the export has to
/// find.
fn earliest_onboarding_activation(
    history: &[(prost_types::Timestamp, PartyToParticipant)],
    target: &str,
) -> Option<prost_types::Timestamp> {
    history
        .iter()
        .find(|(_, mapping)| has_onboarding_marker(mapping, target))
        .map(|(valid_from, _)| *valid_from)
}

/// One microsecond before `ts`, Canton's timestamp resolution.
///
/// `GetHighestOffsetByTimestamp` returns the highest offset at or before the
/// timestamp, so asking at the activation itself can return an offset that
/// already includes it. The activation finders take `begin_offset_exclusive`
/// strictly before what they are looking for.
fn just_before(ts: prost_types::Timestamp) -> prost_types::Timestamp {
    const NANOS_PER_SEC: i32 = 1_000_000_000;
    const ONE_MICRO: i32 = 1_000;
    if ts.nanos >= ONE_MICRO {
        prost_types::Timestamp {
            seconds: ts.seconds,
            nanos: ts.nanos - ONE_MICRO,
        }
    } else {
        prost_types::Timestamp {
            seconds: ts.seconds - 1,
            nanos: ts.nanos + NANOS_PER_SEC - ONE_MICRO,
        }
    }
}

/// Recover a `begin_offset_exclusive` for a party already activated on
/// `target`, by reading when that activation happened rather than by
/// remembering it.
///
/// [`capture_offset_once`] records the offset before the topology moves, which
/// only helps a run that is still alive. A replication that has to be picked up
/// later — the run failed and was dismissed, or the artifacts were swept — has
/// no record, and its participant is left flagged onboarding with no way
/// forward. The chain still knows: the party's `PartyToParticipant` history
/// says when the marker first appeared, and an offset from just before that is
/// exactly what `ExportPartyAcs` and `ClearPartyOnboardingFlag` search from.
///
/// # Errors
/// Returns an error if the topology read fails, if `target` is not flagged
/// onboarding anywhere in the party's history, or if no offset can be resolved
/// for that time.
pub async fn derive_pre_activation_offset(
    config: &NodeConfig,
    party_id: &CantonId,
    target: &CantonId,
) -> Result<i64> {
    let synchronizer_id = get_synchronizer_id(config).await?;
    let history = topology::fetch_p2p_history(config, &synchronizer_id, party_id).await?;

    let activation =
        earliest_onboarding_activation(&history, &target.to_string()).ok_or_else(|| {
            anyhow::anyhow!(
                "cannot derive a pre-activation offset: {target} is not flagged onboarding \
                 in any serial of {party_id}'s topology history, so there is no activation \
                 to replicate from"
            )
        })?;

    let before = just_before(activation);
    tracing::info!(
        "Deriving the pre-activation offset for {target} from its activation at \
         {activation:?}; asking for the ledger offset at {before:?}"
    );

    // PartyManagementService wants the LOGICAL synchronizer id — see
    // `current_ledger_offset` for the physical-id pitfall.
    let logical = utils::extract_synchronizer_fingerprint(&synchronizer_id)?;
    let mut client = PartyManagementServiceClient::new(config.admin_channel().await?);
    let response = client
        .get_highest_offset_by_timestamp(tonic::Request::new(GetHighestOffsetByTimestampRequest {
            synchronizer_id: logical,
            timestamp: Some(before),
            // The timestamp is historical, so the clean-watermark check
            // that makes "now" fail does not apply; force covers a
            // participant whose latest events carry no synchronizer mapping.
            force: true,
        }))
        .await?
        .into_inner();

    if response.ledger_offset <= 0 {
        anyhow::bail!(
            "derived a non-positive offset {offset} for {target}'s activation — the \
             consumers reject it, so the replication cannot be resumed from here",
            offset = response.ledger_offset
        );
    }
    tracing::info!(
        "Derived pre-activation offset {offset} for {target}",
        offset = response.ledger_offset
    );
    Ok(response.ledger_offset)
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::party_to_participant::{
        HostingParticipant, hosting_participant,
    };

    use super::*;

    fn ts(seconds: i64, nanos: i32) -> prost_types::Timestamp {
        prost_types::Timestamp { seconds, nanos }
    }

    fn mapping(entries: &[(&str, bool)]) -> PartyToParticipant {
        PartyToParticipant {
            party: "cbtc-network::1220aa".to_string(),
            threshold: 1,
            participants: entries
                .iter()
                .map(|(uid, onboarding)| HostingParticipant {
                    participant_uid: (*uid).to_string(),
                    permission: 2,
                    onboarding: onboarding.then_some(hosting_participant::Onboarding {}),
                })
                .collect(),
            party_signing_keys: None,
        }
    }

    /// The activation is the FIRST serial carrying the marker. A later serial
    /// carries it forward unchanged, and picking that one would derive an
    /// offset after the activation, which is exactly what the finders cannot
    /// search from.
    #[test]
    fn picks_the_earliest_serial_carrying_the_marker() {
        let history = vec![
            (ts(100, 0), mapping(&[("p1", false)])),
            (ts(200, 0), mapping(&[("p1", false), ("p2", true)])),
            (ts(300, 0), mapping(&[("p1", false), ("p2", true)])),
        ];
        assert_eq!(
            earliest_onboarding_activation(&history, "p2"),
            Some(ts(200, 0))
        );
    }

    /// A member that joined cleanly never carries the marker in the head
    /// state, and a member that was never added carries it nowhere. Both must
    /// report "nothing to resume" rather than yielding some other member's
    /// activation.
    #[test]
    fn reports_nothing_for_a_participant_that_is_not_onboarding() {
        let history = vec![
            (ts(100, 0), mapping(&[("p1", false)])),
            (ts(200, 0), mapping(&[("p1", false), ("p2", true)])),
        ];
        assert_eq!(earliest_onboarding_activation(&history, "p1"), None);
        assert_eq!(earliest_onboarding_activation(&history, "p3"), None);
        assert_eq!(earliest_onboarding_activation(&[], "p2"), None);
    }

    /// A re-add after a kick has an older, marker-free activation earlier in
    /// the history. The derived offset must land before the re-add, not before
    /// the original join, or the export finds the stale activation.
    #[test]
    fn ignores_an_earlier_marker_free_activation_of_the_same_participant() {
        let history = vec![
            (ts(100, 0), mapping(&[("p1", false), ("p2", false)])),
            (ts(200, 0), mapping(&[("p1", false)])),
            (ts(300, 0), mapping(&[("p1", false), ("p2", true)])),
        ];
        assert_eq!(
            earliest_onboarding_activation(&history, "p2"),
            Some(ts(300, 0))
        );
    }

    /// The offset has to be strictly before the activation, so the timestamp
    /// steps back by Canton's resolution rather than being used as-is.
    #[test]
    fn steps_back_one_microsecond() {
        assert_eq!(just_before(ts(10, 5_000)), ts(10, 4_000));
        assert_eq!(just_before(ts(10, 1_000)), ts(10, 0));
    }

    /// Stepping back across a second boundary must borrow, not underflow into
    /// a negative nanos field that Canton would reject.
    #[test]
    fn borrows_across_a_second_boundary() {
        assert_eq!(just_before(ts(10, 0)), ts(9, 999_999_000));
        assert_eq!(just_before(ts(10, 999)), ts(9, 999_999_999));

        let stepped = just_before(ts(10, 0));
        assert!(stepped.nanos >= 0 && stepped.nanos < 1_000_000_000);
    }
}
