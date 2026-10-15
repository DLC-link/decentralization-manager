use anyhow::Context;
use common::{
    api::PendingInvitationsResponse,
    types::{InvitationType, PendingInvitation},
};
use serde_json::json;

use super::Fixture;

/// Captured pending-invitation ids on P2 and P3, threaded through a
/// `Scenario` `Ctx` so a THEN that observes the invitation can hand its
/// id to a later WHEN that posts the accept.
#[derive(Default)]
pub struct InvitationIds {
    /// The coordinator run that sent the invitations, for phases that probe
    /// with [`probe_run_invitation`].
    pub run: Option<String>,
    pub p2: Option<String>,
    pub p3: Option<String>,
}

/// Single-shot probe for a pending invitation of `invitation_type`.
///
/// Returns `Some(id)` when an invitation of that type is visible on the
/// participant; `None` while the participant has not yet observed it.
/// HTTP errors are treated as "not yet" so a transient blip retries.
pub async fn probe_pending_invitation(
    f: &Fixture,
    port: u16,
    invitation_type: InvitationType,
) -> Option<String> {
    let r: PendingInvitationsResponse = f.probe_get_json(port, "/invitations").await?;
    r.invitations
        .into_iter()
        .find(|i| i.invitation_type == invitation_type)
        .map(|i| i.id)
}

/// Single-shot probe for the pending invitation that coordinator run `run`
/// sent, with the same "not yet" semantics as [`probe_pending_invitation`].
///
/// A phase that follows a cancelled run of the same kind must use this one.
/// The cancel cascade can leave the old run's invitation pending, and a
/// probe by kind may then return it instead of the new one.
pub async fn probe_run_invitation(f: &Fixture, port: u16, run: &str) -> Option<String> {
    let r: PendingInvitationsResponse = f.probe_get_json(port, "/invitations").await?;
    invitation_for_run(r.invitations, run)
}

fn invitation_for_run(invitations: Vec<PendingInvitation>, run: &str) -> Option<String> {
    invitations
        .into_iter()
        .find(|i| i.workflow_instance.as_deref() == Some(run))
        .map(|i| i.id)
}

pub async fn post_accept_invitation(
    f: &Fixture,
    port: u16,
    invitation_id: &str,
) -> anyhow::Result<()> {
    let _: serde_json::Value = f
        .post_json(port, "/invitations/accept", &json!({ "id": invitation_id }))
        .await
        .with_context(|| format!("accepting invitation {invitation_id}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pending AddParty invitation as `/invitations` sends it.
    fn add_party_invitation(id: &str, run: Option<&str>) -> PendingInvitation {
        let mut wire = json!({
            "id": id,
            "invitation_type": "AddParty",
            "coordinator_pubkey": "c0ffee",
            "received_at": 1_700_000_000,
        });
        if let Some(run) = run {
            wire["workflow_instance"] = json!(run);
        }
        serde_json::from_value(wire).expect("wire-shaped PendingInvitation")
    }

    #[test]
    fn invitation_for_run_skips_a_stale_invitation_of_the_same_kind() {
        let pending = vec![
            add_party_invitation("stale", Some("test-network-1-add-party-100")),
            add_party_invitation("current", Some("test-network-1-add-party-200")),
        ];

        assert_eq!(
            invitation_for_run(pending, "test-network-1-add-party-200").as_deref(),
            Some("current"),
        );
    }

    #[test]
    fn invitation_for_run_waits_while_only_other_runs_are_pending() {
        let pending = vec![
            add_party_invitation("stale", Some("test-network-1-add-party-100")),
            add_party_invitation("legacy", None),
        ];

        assert_eq!(
            invitation_for_run(pending, "test-network-1-add-party-200"),
            None
        );
    }
}
