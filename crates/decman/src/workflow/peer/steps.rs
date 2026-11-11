use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::SqlitePool;

use crate::{
    db::schema::{Commitable, SchemaWrite},
    noise::MessageType,
    server::WorkflowKind,
    workflow::{
        AddPartyStep, ChangeThresholdStep, ContractsStep, DarsStep, KickStep, OnboardingStep,
        state::WorkflowStep,
    },
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
        WorkflowKind::Onboarding => step_progress::<OnboardingStep>(command),
        WorkflowKind::Kick => step_progress::<KickStep>(command),
        WorkflowKind::Contracts => step_progress::<ContractsStep>(command),
        WorkflowKind::Dars => step_progress::<DarsStep>(command),
        WorkflowKind::AddParty => step_progress::<AddPartyStep>(command),
        WorkflowKind::ChangeThreshold => step_progress::<ChangeThresholdStep>(command),
    }
}

/// `(step_name, step_index, step_total)` of the `S` step that `command`
/// drives.
fn step_progress<S: WorkflowStep>(command: MessageType) -> Option<(&'static str, i64, i64)> {
    let step = S::for_command(command)?;
    Some((step.step_name(), step.step_index(), S::step_total()))
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
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
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

    /// The full command table, pinned: every wire code against every kind.
    /// A peer writes the mapped step name and index to its run row.
    #[test]
    fn peer_step_table_is_pinned() {
        let expected: &[(WorkflowKind, MessageType, &str, i64, i64)] = &[
            (
                WorkflowKind::Onboarding,
                MessageType::GenerateKeys,
                "GenerateKeys",
                1,
                8,
            ),
            (
                WorkflowKind::Onboarding,
                MessageType::SignDns,
                "SignDns",
                3,
                8,
            ),
            (
                WorkflowKind::Onboarding,
                MessageType::SignP2p,
                "SignP2p",
                5,
                8,
            ),
            (
                WorkflowKind::Onboarding,
                MessageType::Disconnect,
                "Complete",
                7,
                8,
            ),
            (
                WorkflowKind::Kick,
                MessageType::SignKick,
                "SignProposals",
                3,
                7,
            ),
            (
                WorkflowKind::Kick,
                MessageType::RetireLegacyKeys,
                "RetireLegacyKeys",
                5,
                7,
            ),
            (
                WorkflowKind::Kick,
                MessageType::Disconnect,
                "Complete",
                6,
                7,
            ),
            (
                WorkflowKind::Contracts,
                MessageType::SignSubmissions,
                "SignSubmissions",
                2,
                5,
            ),
            (
                WorkflowKind::Contracts,
                MessageType::Disconnect,
                "Complete",
                4,
                5,
            ),
            (
                WorkflowKind::Dars,
                MessageType::UploadDars,
                "UploadDars",
                1,
                3,
            ),
            (
                WorkflowKind::Dars,
                MessageType::Disconnect,
                "Complete",
                2,
                3,
            ),
            (
                WorkflowKind::AddParty,
                MessageType::GenerateAddPartyKeys,
                "GenerateNewMemberKeys",
                1,
                14,
            ),
            (
                WorkflowKind::AddParty,
                MessageType::SignAddParty,
                "SignProposals",
                4,
                14,
            ),
            (
                WorkflowKind::AddParty,
                MessageType::ImportAcs,
                "SyncAcs",
                6,
                14,
            ),
            (
                WorkflowKind::AddParty,
                MessageType::ClearOnboardingFlag,
                "ProposeClearOnboarding",
                8,
                14,
            ),
            (
                WorkflowKind::AddParty,
                MessageType::SignClearOnboarding,
                "SignClearOnboarding",
                10,
                14,
            ),
            (
                WorkflowKind::AddParty,
                MessageType::RetireLegacyKeys,
                "RetireLegacyKeys",
                12,
                14,
            ),
            (
                WorkflowKind::AddParty,
                MessageType::Disconnect,
                "Complete",
                13,
                14,
            ),
            (
                WorkflowKind::ChangeThreshold,
                MessageType::SignChangeThreshold,
                "SignProposals",
                3,
                7,
            ),
            (
                WorkflowKind::ChangeThreshold,
                MessageType::RetireLegacyKeys,
                "RetireLegacyKeys",
                5,
                7,
            ),
            (
                WorkflowKind::ChangeThreshold,
                MessageType::Disconnect,
                "Complete",
                6,
                7,
            ),
        ];
        let kinds = [
            WorkflowKind::Onboarding,
            WorkflowKind::Kick,
            WorkflowKind::Contracts,
            WorkflowKind::Dars,
            WorkflowKind::AddParty,
            WorkflowKind::ChangeThreshold,
        ];
        for code in 0..=u16::MAX {
            let Ok(command) = MessageType::try_from(code) else {
                continue;
            };
            for kind in kinds {
                let want = expected
                    .iter()
                    .find(|(k, c, ..)| *k == kind && *c == command)
                    .map(|(_, _, name, index, total)| (*name, *index, *total));
                assert_eq!(
                    peer_step_for_command(kind, command),
                    want,
                    "{kind:?}/{command:?}"
                );
            }
        }
    }
}
