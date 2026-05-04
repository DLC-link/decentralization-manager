use anyhow::Context;
use serde_json::json;

use super::Fixture;
use super::types::PendingInvitationsResponse;

/// Captured pending-invitation ids on P2 and P3, threaded through a
/// `Scenario` `Ctx` so a THEN that observes the invitation can hand its
/// id to a later WHEN that posts the accept.
#[derive(Default)]
pub struct InvitationIds {
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
    invitation_type: &str,
) -> Option<String> {
    let r: PendingInvitationsResponse = f.get_json(port, "/invitations").await.ok()?;
    r.invitations
        .into_iter()
        .find(|i| i.invitation_type == invitation_type)
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
