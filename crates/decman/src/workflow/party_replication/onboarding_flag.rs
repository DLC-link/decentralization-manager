//! Canton's `HostingParticipant.Onboarding` marker: clearing it, and waiting
//! for it to go.
//!
//! The marker keeps a replicated party suspended on its new host until the ACS
//! import lands. Clearing it is the last step of any replication and is the same
//! Canton call for every party type — only *who signs the resulting proposal*
//! differs, and that stays with each workflow.

use std::time::SystemTime;

use canton_proto_rs::com::digitalasset::canton::{
    admin::participant::v30::{
        ClearPartyOnboardingFlagRequest,
        party_management_service_client::PartyManagementServiceClient,
    },
    protocol::v30::PartyToParticipant,
};
use sqlx::SqlitePool;
use tokio::time;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{
        party_replication::ReplicationTarget, storage::WorkflowStorage, topology::fetch_p2p_mapping,
    },
};

const MAX_SAFE_TIME_WAIT_SECS: u64 = 600;

/// Outcome of the target participant's `ClearPartyOnboardingFlag` polling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClearOutcome {
    /// Canton reports the flag is gone — no signing round needed.
    Cleared,
    /// The safe time passed and the clearing transaction is proposed. Who must
    /// sign it next is party-type specific: a decentralized party needs
    /// threshold owner signatures, which its coordinator's sign round provides.
    Proposed,
}

/// Target-side step: drive `ClearPartyOnboardingFlag` until the flag is gone
/// or Canton has accepted the clearing proposal past its safe time.
///
/// Canton refuses to clear before its computed safe time (so no in-flight
/// transaction from the import window can be lost) — the endpoint returns
/// `onboarded = false` with `earliest_retry_timestamp` until then. After the
/// safe time, a call proposes the clearing topology transaction, which still
/// needs whatever signatures the party's namespace demands — collected by the
/// calling workflow, not here.
pub async fn clear_onboarding_flag(
    config: &NodeConfig,
    storage: &SqlitePool,
    target: &ReplicationTarget,
) -> Result<ClearOutcome> {
    // Logical synchronizer id — see `current_ledger_offset` for why the
    // physical id is rejected by PartyManagementService.
    let synchronizer_id =
        utils::extract_synchronizer_fingerprint(&utils::get_synchronizer_id(config).await?)?;
    let self_id = config.participant_id().to_string();

    // No zero fallback: ClearPartyOnboardingFlag rejects non-positive
    // offsets, and a missing artifact means GenerateNewMemberKeys never
    // persisted one — a real bug to surface, not paper over.
    let offset_bytes = storage
        .read_artifact(
            &target.instance_name,
            target.artifacts.pre_activation_offset,
            Some(&self_id),
        )
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{kind} artifact missing for {self_id} — this participant's pre-activation \
                 offset was never captured",
                kind = target.artifacts.pre_activation_offset
            )
        })?;
    let begin_offset_exclusive: i64 = String::from_utf8(offset_bytes)?
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("Failed to parse pre-activation offset: {e}"))?;

    let mut client = PartyManagementServiceClient::new(config.admin_channel().await?);
    let waited_start = time::Instant::now();

    loop {
        let request = tonic::Request::new(ClearPartyOnboardingFlagRequest {
            party_id: target.party_id.to_string(),
            synchronizer_id: synchronizer_id.clone(),
            begin_offset_exclusive,
            wait_for_activation_timeout: None,
        });

        let response = client
            .clear_party_onboarding_flag(request)
            .await?
            .into_inner();

        if response.onboarded {
            tracing::info!("Onboarding flag already cleared");
            return Ok(ClearOutcome::Cleared);
        }

        let now = SystemTime::now();
        let earliest_retry = response
            .earliest_retry_timestamp
            .and_then(|ts| SystemTime::try_from(ts).ok());

        match earliest_retry {
            Some(safe_time) if safe_time > now => {
                let wait = safe_time
                    .duration_since(now)
                    .unwrap_or(time::Duration::from_secs(1))
                    // Re-check at least every 10s so a moving safe time
                    // can't park the loop on one long sleep.
                    .min(time::Duration::from_secs(10));
                if waited_start.elapsed().as_secs() > MAX_SAFE_TIME_WAIT_SECS {
                    anyhow::bail!(
                        "Onboarding-flag safe time still {wait:?} away after waiting \
                         {MAX_SAFE_TIME_WAIT_SECS}s — synchronizer appears stuck"
                    );
                }
                tracing::info!(
                    "Safe time for clearing the onboarding flag not reached; waiting {wait:?}"
                );
                time::sleep(wait).await;
            }
            // Safe time reached (or Canton sent none): this call was made
            // past it, so the clearing transaction is proposed. The
            // coordinator's signing round takes it from here.
            _ => {
                tracing::info!(
                    "Safe time passed; clearing transaction proposed, awaiting owner signatures"
                );
                return Ok(ClearOutcome::Proposed);
            }
        }
    }
}

/// Poll head state until the target participant's onboarding marker is gone.
pub async fn wait_for_flag_cleared(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &CantonId,
    new_member: &CantonId,
) -> Result {
    let max_attempts = crate::consts::topology_retry_max_attempts();
    let retry_delay = time::Duration::from_secs(crate::consts::topology_retry_delay_secs());
    let new_member_str = new_member.to_string();

    for attempt in 1..=max_attempts {
        let p2p = fetch_p2p_mapping(config, synchronizer_id, party_id).await?;
        if !has_onboarding_marker(&p2p, &new_member_str) {
            tracing::info!("Onboarding flag cleared after {attempt} attempt(s)");
            return Ok(());
        }
        if attempt < max_attempts {
            tracing::debug!(
                "Onboarding flag still set, attempt {attempt}/{max_attempts}, \
                 retrying in {retry_delay:?}..."
            );
            time::sleep(retry_delay).await;
        }
    }

    anyhow::bail!("Onboarding flag was not cleared after {max_attempts} attempts")
}

/// Whether `participant` carries the Onboarding marker in `p2p`.
pub fn has_onboarding_marker(p2p: &PartyToParticipant, participant: &str) -> bool {
    p2p.participants
        .iter()
        .any(|p| p.participant_uid == participant && p.onboarding.is_some())
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::party_to_participant::{
        HostingParticipant, hosting_participant,
    };

    use super::*;

    fn p2p(participants: Vec<HostingParticipant>) -> PartyToParticipant {
        PartyToParticipant {
            party: "acme::1220abcd".to_string(),
            threshold: 2,
            participants,
            party_signing_keys: None,
        }
    }

    fn hosting(uid: &str, onboarding: bool) -> HostingParticipant {
        HostingParticipant {
            participant_uid: uid.to_string(),
            permission: 0,
            onboarding: onboarding.then_some(hosting_participant::Onboarding {}),
        }
    }

    #[test]
    fn detects_onboarding_marker_only_for_the_marked_participant() {
        let mapping = p2p(vec![hosting("PAR::a", false), hosting("PAR::b", true)]);

        assert!(!has_onboarding_marker(&mapping, "PAR::a"));
        assert!(has_onboarding_marker(&mapping, "PAR::b"));
        assert!(!has_onboarding_marker(&mapping, "PAR::missing"));
    }
}
