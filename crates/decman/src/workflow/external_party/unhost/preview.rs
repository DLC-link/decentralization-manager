use common::api::ExternalPartyUnhostPreview;

use super::{
    attempt::{UnhostEnv, already_running, cleanup_party, read_hosted, relay_busy, relay_open},
    error::UnhostError,
    splice_cleanup::{SpliceContracts, find_splice_contracts, now_micros, operator_token},
    topology::{hosts, leave_mapping, refuse_a_decentralized_party},
};

/// What [`unhost`](super::unhost) would do for `party_id` right now. Nothing is written.
///
/// A precondition that would refuse the unhost is reported as `blocker` rather
/// than as an error, so the operator sees it before confirming.
///
/// # Errors
/// [`UnhostError::NotHosted`] when this node does not host the party, and
/// [`UnhostError::Internal`] when a read fails.
pub async fn preview(
    env: &UnhostEnv<'_>,
    party_id: &str,
) -> Result<ExternalPartyUnhostPreview, UnhostError> {
    let me = env.config.participant_id().to_string();
    let current = read_hosted(env.config, party_id).await?;
    refuse_a_decentralized_party(env.config, party_id).await?;
    if !hosts(&current.mapping, &me) {
        return Err(UnhostError::NotHosted {
            party: party_id.to_string(),
        });
    }

    let mut blocker = leave_mapping(&current.mapping, &me).err();
    if blocker.is_none() && env.relay_sessions.is_held(party_id) {
        blocker = Some(already_running(party_id));
    }
    if blocker.is_none() && relay_open(env.relay_sessions, party_id).await {
        blocker = Some(relay_busy(party_id));
    }

    let operator = env.config.validator_operator_party.as_ref();
    let mut contracts = SpliceContracts::default();
    if operator.is_some() {
        match cleanup_party(env) {
            Ok(operator) => {
                let token = operator_token(env, operator).await?;
                contracts = find_splice_contracts(env.config, party_id, operator, &token).await?;
            }
            Err(not_configured) => {
                blocker.get_or_insert(not_configured);
            }
        }
    }
    let now = now_micros();
    let (live, expired): (Vec<_>, Vec<_>) = contracts
        .preapprovals
        .into_iter()
        .partition(|p| p.expires_at > now);

    Ok(ExternalPartyUnhostPreview {
        party_id: party_id.to_string(),
        serial: current.serial,
        threshold: current.mapping.threshold,
        remaining_hosts: current
            .mapping
            .participants
            .iter()
            .filter(|p| p.participant_uid != me)
            .map(|p| p.participant_uid.clone())
            .collect(),
        validator_operator_party: operator.map(ToString::to_string),
        validator_rights: contracts.validator_rights,
        preapprovals_to_cancel: live.into_iter().map(|p| p.contract_id).collect(),
        preapprovals_expired: expired.into_iter().map(|p| p.contract_id).collect(),
        blocker: blocker.map(|b| b.to_string()),
    })
}
