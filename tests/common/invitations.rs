use std::time::Duration;

use anyhow::Context;
use serde_json::json;
use tracing::info;

use super::Fixture;
use super::http::poll_until;
use super::types::{PendingInvitation, PendingInvitationsResponse};

pub async fn accept_invitation(
    f: &Fixture,
    port: u16,
    name: &str,
    invitation_type: &str,
) -> anyhow::Result<()> {
    info!("Waiting for {invitation_type} invitation on {name} (port {port})");
    let inv_type = invitation_type.to_string();
    let label = format!("{invitation_type} invitation on {name}");

    let invitation: PendingInvitation = poll_until(
        Duration::from_secs(60),
        Duration::from_secs(1),
        &label,
        || {
            let inv_type = inv_type.clone();
            async move {
                let r: PendingInvitationsResponse = f.get_json(port, "/invitations").await.ok()?;
                r.invitations
                    .into_iter()
                    .find(|i| i.invitation_type == inv_type)
            }
        },
    )
    .await
    .with_context(|| format!("waiting for {invitation_type} on {name}"))?;

    info!(
        "Accepting {invitation_type} on {name} (id: {})",
        invitation.id
    );
    let _: serde_json::Value = f
        .post_json(port, "/invitations/accept", &json!({ "id": invitation.id }))
        .await
        .with_context(|| format!("accepting {invitation_type} on {name}"))?;
    Ok(())
}
