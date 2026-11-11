//! Invitee health and version checks, run lookups, and the broadcasts to peers.

use std::collections::HashSet;

use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig},
    consts::MIN_PEER_VERSION,
    db::schema::SchemaRead,
    error::Result,
    noise::{
        Message, MessageType, NoiseKeypair, parse_public_key, send_noise_message,
        send_noise_message_with_retry,
    },
    server::{
        health::{HealthReply, classify_health_reply},
        types::WorkflowKind,
    },
    utils,
};

/// Look for an existing decentralized party whose human-readable prefix
/// equals `prefix`. Returns the matching `party_id` if found — used by the
/// onboarding pre-flight to refuse duplicate-prefix runs.
/// Probe every invitee's `Health` and return those that cannot POSITIVELY be
/// verified to run dec-party-manager >= [`MIN_PEER_VERSION`].
pub(super) async fn preflight_incompatible_peers(
    config: &NodeConfig,
    db: &SqlitePool,
    invitees: &[CantonId],
) -> Vec<(String, String)> {
    incompatible_versions(&probe_invitee_health(config, db, invitees).await)
}

/// One invitee's reply to a `Health` probe, or why no reply came.
type InviteeHealth = (String, std::result::Result<HealthReply, String>);

/// Probe every invitee's `Health` over Noise. An invitee missing from the
/// peer table, or with a public key that does not parse, is not probed.
pub(super) async fn probe_invitee_health(
    config: &NodeConfig,
    db: &SqlitePool,
    invitees: &[CantonId],
) -> Vec<InviteeHealth> {
    let Ok(peers) = db.get_all_peers().await else {
        return Vec::new();
    };
    let Ok(keypair) = NoiseKeypair::from_file(&config.key_file_path()).await else {
        return Vec::new();
    };
    let identity = config.participant_id().to_string();
    let invitee_set: HashSet<String> = invitees.iter().map(CantonId::to_string).collect();

    let mut probes = Vec::new();
    for peer in peers
        .into_iter()
        .filter(|p| invitee_set.contains(&p.participant_id.to_string()))
    {
        let Ok(pubkey) = parse_public_key(&peer.public_key) else {
            continue;
        };
        let psk = keypair.derive_psk(&pubkey);
        let id = peer.participant_id.to_string();
        let address = peer.address.clone();
        let port = peer.port;
        let identity = identity.clone();
        let retry = config.noise_retry.clone();
        probes.push(tokio::spawn(async move {
            let reply = send_noise_message_with_retry(
                &address,
                port,
                &psk,
                identity.as_bytes(),
                &Message::new_empty(MessageType::Health),
                &retry,
            )
            .await
            .map(|response| classify_health_reply(&response))
            .map_err(|e| e.to_string());
            (id, reply)
        }));
    }

    let mut replies = Vec::new();
    for probe in probes {
        if let Ok(reply) = probe.await {
            replies.push(reply);
        }
    }
    replies
}

/// The invitees that cannot POSITIVELY be verified to run at least
/// dec-party-manager [`MIN_PEER_VERSION`]. 0.1.9's Noise wire format is
/// breaking — an older peer cannot parse our frames at all, so inviting it
/// would strand the run. An old build answers the probe with a 503 (it can't
/// decode the request), so any failure to obtain a parseable version report
/// blocks the start: for this gate, unverifiable means incompatible, never
/// "assume OK".
pub(super) fn incompatible_versions(replies: &[InviteeHealth]) -> Vec<(String, String)> {
    replies
        .iter()
        .filter_map(|(id, reply)| match reply {
            Ok(reply) => match &reply.version {
                Some(v) if utils::version_at_least(v, MIN_PEER_VERSION) => None,
                Some(v) => Some((id.clone(), format!("running version {v}"))),
                None => Some((
                    id.clone(),
                    "reachable but reported no version (pre-0.1.9 build)".to_string(),
                )),
            },
            Err(e) => Some((id.clone(), format!("could not verify version: {e}"))),
        })
        .collect()
}

/// The invitees whose `Health` reply does not list `feature`, each with the
/// version it reported. Unverifiable means lacking, as for the version gate.
pub(super) fn lacking_feature(replies: &[InviteeHealth], feature: &str) -> Vec<(String, String)> {
    replies
        .iter()
        .filter_map(|(id, reply)| match reply {
            Ok(reply) if reply.features.iter().any(|f| f == feature) => None,
            Ok(reply) => Some((
                id.clone(),
                format!(
                    "running version {}",
                    reply.version.as_deref().unwrap_or("unknown")
                ),
            )),
            Err(e) => Some((id.clone(), format!("could not verify: {e}"))),
        })
        .collect()
}

/// Operator-facing 409 message naming each peer that cannot sign an
/// owner-only kick.
pub(super) fn format_peers_lacking_owner_only_kick(lacking: &[(String, String)]) -> String {
    let list = lacking
        .iter()
        .map(|(id, reason)| format!("{id} ({reason})"))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "Cannot start the kick: removing an owner that hosts no participant needs every \
         invited peer to support it, and older builds refuse to sign. Blocking peer(s): \
         {list}. Upgrade them and retry."
    )
}

/// Operator-facing 409 message naming each peer that blocks the start and why.
pub(super) fn format_incompatible_peers(incompatible: &[(String, String)]) -> String {
    let list = incompatible
        .iter()
        .map(|(id, reason)| format!("{id} ({reason})"))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "Cannot start the workflow: every invited peer must run dec-party-manager \
         >= {MIN_PEER_VERSION} (concurrent workflows changed the Noise wire format, \
         older builds cannot participate). Blocking peer(s): {list}. Upgrade or \
         deselect them and retry."
    )
}

/// Any in-progress run on this node (either role) already targeting the given
/// decentralized party. The old global one-at-a-time gate incidentally
/// prevented two workflows from mutating the same party's topology
/// concurrently (e.g. two kicks, or a kick racing a contracts deployment);
/// with concurrent workflows that protection must be explicit. Local-node
/// guard only — two DIFFERENT nodes coordinating conflicting workflows on the
/// same party is a distributed conflict Canton itself surfaces.
pub(super) async fn find_inprogress_run_for_party(
    db: &SqlitePool,
    party: &CantonId,
) -> Option<(String, WorkflowKind)> {
    SchemaRead::get_in_progress_workflow_runs(db)
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|r| r.dec_party_id.as_ref() == Some(party))
        .map(|r| (r.instance_name, r.kind))
}

pub(super) async fn find_party_with_prefix(
    db: &SqlitePool,
    prefix: &str,
) -> Result<Option<String>> {
    use crate::db::schema::SchemaRead;

    let parties = db.get_dec_parties_by_prefix(prefix).await?;
    Ok(parties.into_iter().next().map(|p| p.party_id))
}

/// `instance` is the cancelled run's `instance_name`, stamped onto the message
/// so peers cancel only that run's invite/peer-run — not every run they hold
/// from this coordinator (which may have other concurrent runs in flight).
pub(super) async fn send_cancel_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
    instance: &str,
) -> Result {
    broadcast_simple_message(
        config,
        db,
        peer_ids,
        Message::new_empty(MessageType::CancelInvite).with_instance(instance),
        "CancelInvite",
    )
    .await
}

/// Best-effort: notify previously-invited peers that the coordinator is
/// retrying the workflow so they flip their Failed row back to InProgress
/// and re-spin `start_peer`. `instance` scopes the retry to this run's peer
/// rows (see `send_cancel_invites`).
pub(super) async fn send_retry_workflow(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
    instance: &str,
) -> Result {
    broadcast_simple_message(
        config,
        db,
        peer_ids,
        Message::new_empty(MessageType::RetryWorkflow).with_instance(instance),
        "RetryWorkflow",
    )
    .await
}

async fn broadcast_simple_message(
    config: &NodeConfig,
    db: &SqlitePool,
    peer_ids: &[CantonId],
    message: Message,
    label: &str,
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;
    let identity = config.participant_id().to_string();

    for peer_id in peer_ids {
        let Some(peer) = network_config
            .peers
            .iter()
            .find(|p| &p.participant_id == peer_id)
        else {
            continue;
        };
        if peer.public_key.is_empty() {
            continue;
        }
        let Ok(peer_pub_key) = parse_public_key(&peer.public_key) else {
            continue;
        };
        let psk = keypair.derive_psk(&peer_pub_key);

        if let Err(e) = send_noise_message(
            &peer.address,
            peer.port,
            &psk,
            identity.as_bytes(),
            &message,
        )
        .await
        {
            tracing::warn!("Failed to send {label} to {peer_id}: {e}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        super::{
            runs::{mark_run_completed, mark_run_failed},
            test_support::{in_progress_run, pid},
        },
        *,
    };
    use crate::{
        consts::OWNER_ONLY_KICK,
        db::{
            MIGRATOR,
            schema::{Commitable, SchemaWrite},
        },
    };

    /// The invariant the completion ordering rests on: once a run row is
    /// terminal, the in-flight guard must not see it.
    ///
    /// This is the half that made the ordering bug reachable. `/…/status` reads
    /// the in-memory status while this guard reads the run rows, so marking the
    /// row terminal *after* flipping memory left a window where the API reported
    /// a run finished and refused the next workflow for the same party in the
    /// same breath. The handlers now write the row first; this pins what that
    /// write has to achieve.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_completed_run_is_not_in_flight_for_its_party(pool: SqlitePool) -> Result {
        let party = pid(7)?;
        let run = in_progress_run("contracts-1", &party);
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_workflow_run(&run).await?;
        Commitable::commit(tx).await?;

        // While it runs, a second workflow for the same party is refused — the
        // guard's whole purpose.
        let found = find_inprogress_run_for_party(&pool, &party).await;
        assert_eq!(
            found.map(|(run, _)| run),
            Some("contracts-1".to_string()),
            "an in-progress run must be reported as in flight"
        );

        mark_run_completed(&pool, WorkflowKind::Contracts, "contracts-1").await;

        assert!(
            find_inprogress_run_for_party(&pool, &party).await.is_none(),
            "a completed run must not block the next workflow for its party"
        );
        Ok(())
    }

    /// A failed run must not block the party either: the operator's next move
    /// after reading Failed is to retry, and a guard that still counted it would
    /// refuse that retry.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_run_is_not_in_flight_for_its_party(pool: SqlitePool) -> Result {
        let party = pid(8)?;
        let run = in_progress_run("contracts-2", &party);
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_workflow_run(&run).await?;
        Commitable::commit(tx).await?;

        mark_run_failed(&pool, WorkflowKind::Contracts, "contracts-2", "boom").await;

        assert!(
            find_inprogress_run_for_party(&pool, &party).await.is_none(),
            "a failed run must not block the retry it invites"
        );
        Ok(())
    }

    fn health_reply(version: &str, features: &[&str]) -> HealthReply {
        HealthReply {
            status: crate::server::ConnectionStatus::Connected,
            workflow: None,
            version: Some(version.to_string()),
            build_version: None,
            features: features.iter().map(ToString::to_string).collect(),
        }
    }

    /// Every build reports one version until a release bumps it, so only the
    /// feature list tells a peer that can sign an owner-only kick. A peer that
    /// lists no features, or that did not answer, cannot.
    #[test]
    fn an_owner_only_kick_needs_every_peer_to_list_the_feature() {
        let replies: Vec<InviteeHealth> = vec![
            (
                "p2".to_string(),
                Ok(health_reply("1.14.0", &[OWNER_ONLY_KICK])),
            ),
            ("p3".to_string(), Ok(health_reply("1.14.0", &[]))),
            ("p4".to_string(), Err("timed out".to_string())),
        ];
        assert_eq!(
            lacking_feature(&replies, OWNER_ONLY_KICK),
            vec![
                ("p3".to_string(), "running version 1.14.0".to_string()),
                ("p4".to_string(), "could not verify: timed out".to_string()),
            ]
        );
        assert!(incompatible_versions(&replies[..2]).is_empty());
    }
}
