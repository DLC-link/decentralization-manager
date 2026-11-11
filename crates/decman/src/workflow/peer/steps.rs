use sqlx::SqlitePool;

use crate::{
    db::schema::{Commitable, SchemaWrite},
    noise::MessageType,
    server::WorkflowKind,
    workflow::state::WorkflowStep,
};

use crate::workflow::{
    AddPartyStep, ChangeThresholdStep, ContractsStep, DarsStep, KickStep, OnboardingStep,
};

/// Whether a command belongs to the workflow kind the operator accepted.
///
/// `Wait`, `Ping` and `Disconnect` are protocol control messages that carry no
/// payload and drive no step, so they are allowed for every kind. Everything
/// else must map to a real step of `kind`: an accepted invitation authorizes
/// one workflow, not a channel the coordinator can drive anywhere.
pub(super) fn command_matches_kind(kind: WorkflowKind, command: MessageType) -> bool {
    matches!(
        command,
        MessageType::Wait | MessageType::Ping | MessageType::Disconnect
    ) || peer_step_for_command(kind, command).is_some()
}

/// Map an inbound coordinator command to the peer's view of step
/// progress for the given workflow kind. Returns `(step_name, step_index,
/// step_total)` for commands that correspond to a real step on the peer
/// side; returns `None` for `Wait` (no transition) and any unrelated
/// command.
pub(super) fn peer_step_for_command(
    kind: WorkflowKind,
    command: MessageType,
) -> Option<(&'static str, i64, i64)> {
    match kind {
        WorkflowKind::Onboarding => {
            let step = match command {
                MessageType::GenerateKeys => OnboardingStep::GenerateKeys,
                MessageType::SignDns => OnboardingStep::SignDns,
                MessageType::SignP2p => OnboardingStep::SignP2p,
                MessageType::Disconnect => OnboardingStep::Complete,
                _ => return None,
            };
            Some((
                step.step_name(),
                step.step_index(),
                OnboardingStep::step_total(),
            ))
        }
        WorkflowKind::Kick => {
            let step = match command {
                MessageType::SignKick => KickStep::SignProposals,
                MessageType::RetireLegacyKeys => KickStep::RetireLegacyKeys,
                MessageType::Disconnect => KickStep::Complete,
                _ => return None,
            };
            Some((step.step_name(), step.step_index(), KickStep::step_total()))
        }
        WorkflowKind::Contracts => {
            let step = match command {
                MessageType::SignSubmissions => ContractsStep::SignSubmissions,
                MessageType::Disconnect => ContractsStep::Complete,
                _ => return None,
            };
            Some((
                step.step_name(),
                step.step_index(),
                ContractsStep::step_total(),
            ))
        }
        WorkflowKind::Dars => {
            let step = match command {
                MessageType::UploadDars => DarsStep::UploadDars,
                MessageType::Disconnect => DarsStep::Complete,
                _ => return None,
            };
            Some((step.step_name(), step.step_index(), DarsStep::step_total()))
        }
        WorkflowKind::AddParty => {
            let step = match command {
                MessageType::GenerateAddPartyKeys => AddPartyStep::GenerateNewMemberKeys,
                MessageType::SignAddParty => AddPartyStep::SignProposals,
                MessageType::RetireLegacyKeys => AddPartyStep::RetireLegacyKeys,
                MessageType::ImportAcs => AddPartyStep::SyncAcs,
                MessageType::ClearOnboardingFlag => AddPartyStep::ProposeClearOnboarding,
                MessageType::SignClearOnboarding => AddPartyStep::SignClearOnboarding,
                MessageType::Disconnect => AddPartyStep::Complete,
                _ => return None,
            };
            Some((
                step.step_name(),
                step.step_index(),
                AddPartyStep::step_total(),
            ))
        }
        WorkflowKind::ChangeThreshold => {
            let step = match command {
                MessageType::SignChangeThreshold => ChangeThresholdStep::SignProposals,
                MessageType::RetireLegacyKeys => ChangeThresholdStep::RetireLegacyKeys,
                MessageType::Disconnect => ChangeThresholdStep::Complete,
                _ => return None,
            };
            Some((
                step.step_name(),
                step.step_index(),
                ChangeThresholdStep::step_total(),
            ))
        }
    }
}

/// Persist peer-side step progress for a command. Best-effort: any
/// failure is logged at WARN — the workflow itself doesn't depend on this
/// row staying in sync, only the notification feed UI does.
pub(super) async fn persist_peer_step(
    db: &SqlitePool,
    instance_name: &str,
    kind: WorkflowKind,
    command: MessageType,
) {
    let Some((step_name, step_index, _step_total)) = peer_step_for_command(kind, command) else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut tx = match db.begin_transaction().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("peer step persist: begin_transaction failed: {e}");
            return;
        }
    };
    if let Err(e) = tx
        .update_workflow_run_step(instance_name, step_name, step_index, &[], now)
        .await
    {
        tracing::warn!("peer step persist: update failed: {e}");
        return;
    }
    if let Err(e) = Commitable::commit(tx).await {
        tracing::warn!("peer step persist: commit failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_retirement_is_limited_to_membership_changes() {
        for kind in [
            WorkflowKind::Kick,
            WorkflowKind::AddParty,
            WorkflowKind::ChangeThreshold,
        ] {
            assert!(command_matches_kind(kind, MessageType::RetireLegacyKeys));
        }
        for kind in [
            WorkflowKind::Onboarding,
            WorkflowKind::Contracts,
            WorkflowKind::Dars,
        ] {
            assert!(!command_matches_kind(kind, MessageType::RetireLegacyKeys));
        }
    }

    /// An accepted invitation authorizes one workflow. A command belonging to
    /// another kind is the coordinator reaching past what the operator agreed
    /// to, and must be refused before any signing step sees its payload.
    #[test]
    fn refuses_commands_from_another_workflow_kind() {
        assert!(!command_matches_kind(
            WorkflowKind::Contracts,
            MessageType::SignKick
        ));
        assert!(!command_matches_kind(
            WorkflowKind::Dars,
            MessageType::SignSubmissions
        ));
        assert!(!command_matches_kind(
            WorkflowKind::Onboarding,
            MessageType::SignAddParty
        ));
        assert!(!command_matches_kind(
            WorkflowKind::ChangeThreshold,
            MessageType::UploadDars
        ));
    }

    #[test]
    fn allows_a_kinds_own_commands_and_the_control_messages() {
        assert!(command_matches_kind(
            WorkflowKind::Onboarding,
            MessageType::SignDns
        ));
        assert!(command_matches_kind(
            WorkflowKind::Kick,
            MessageType::SignKick
        ));
        for kind in [
            WorkflowKind::Onboarding,
            WorkflowKind::Kick,
            WorkflowKind::Contracts,
            WorkflowKind::Dars,
            WorkflowKind::AddParty,
            WorkflowKind::ChangeThreshold,
        ] {
            assert!(command_matches_kind(kind, MessageType::Wait));
            assert!(command_matches_kind(kind, MessageType::Ping));
            assert!(command_matches_kind(kind, MessageType::Disconnect));
        }
    }

    #[test]
    fn peer_step_maps_known_commands_and_rejects_the_rest() {
        // Commands that correspond to a real peer-side step for each kind.
        let mapped = [
            (WorkflowKind::Onboarding, MessageType::GenerateKeys),
            (WorkflowKind::Onboarding, MessageType::SignDns),
            (WorkflowKind::Onboarding, MessageType::SignP2p),
            (WorkflowKind::Onboarding, MessageType::Disconnect),
            (WorkflowKind::Kick, MessageType::SignKick),
            (WorkflowKind::Kick, MessageType::Disconnect),
            (WorkflowKind::Contracts, MessageType::SignSubmissions),
            (WorkflowKind::Contracts, MessageType::Disconnect),
            (WorkflowKind::Dars, MessageType::UploadDars),
            (WorkflowKind::Dars, MessageType::Disconnect),
            (WorkflowKind::AddParty, MessageType::GenerateAddPartyKeys),
            (WorkflowKind::AddParty, MessageType::SignAddParty),
            (WorkflowKind::AddParty, MessageType::ImportAcs),
            (WorkflowKind::AddParty, MessageType::ClearOnboardingFlag),
            (WorkflowKind::AddParty, MessageType::SignClearOnboarding),
            (WorkflowKind::AddParty, MessageType::Disconnect),
            (
                WorkflowKind::ChangeThreshold,
                MessageType::SignChangeThreshold,
            ),
            (WorkflowKind::ChangeThreshold, MessageType::Disconnect),
        ];
        for (kind, command) in mapped {
            assert!(
                peer_step_for_command(kind, command).is_some(),
                "{kind:?}/{command:?} should map to a step"
            );
        }

        // `Wait` (no transition) and an unrelated command never map, on any kind.
        for kind in [
            WorkflowKind::Onboarding,
            WorkflowKind::Kick,
            WorkflowKind::Contracts,
            WorkflowKind::Dars,
            WorkflowKind::AddParty,
            WorkflowKind::ChangeThreshold,
        ] {
            assert!(peer_step_for_command(kind, MessageType::Wait).is_none());
            assert!(peer_step_for_command(kind, MessageType::Ping).is_none());
        }

        // A command belonging to a different kind must not cross over.
        assert!(peer_step_for_command(WorkflowKind::Onboarding, MessageType::SignKick).is_none());
        assert!(peer_step_for_command(WorkflowKind::Kick, MessageType::GenerateKeys).is_none());
        assert!(peer_step_for_command(WorkflowKind::Contracts, MessageType::UploadDars).is_none());
        assert!(peer_step_for_command(WorkflowKind::Dars, MessageType::SignSubmissions).is_none());
        assert!(peer_step_for_command(WorkflowKind::AddParty, MessageType::GenerateKeys).is_none());
        assert!(peer_step_for_command(WorkflowKind::Kick, MessageType::SignAddParty).is_none());
        assert!(
            peer_step_for_command(WorkflowKind::ChangeThreshold, MessageType::SignKick).is_none()
        );
        assert!(
            peer_step_for_command(WorkflowKind::Kick, MessageType::SignChangeThreshold).is_none()
        );
    }

    #[test]
    fn peer_step_distinguishes_steps_within_a_kind() {
        // GenerateKeys and SignDns are different onboarding steps: distinct
        // indices, identical totals.
        match (
            peer_step_for_command(WorkflowKind::Onboarding, MessageType::GenerateKeys),
            peer_step_for_command(WorkflowKind::Onboarding, MessageType::SignDns),
        ) {
            (Some((_, gen_idx, total_a)), Some((_, dns_idx, total_b))) => {
                assert_ne!(gen_idx, dns_idx);
                assert_eq!(total_a, total_b);
                assert!(total_a >= 2);
            }
            _ => panic!("onboarding commands should map to steps"),
        }
    }
}
