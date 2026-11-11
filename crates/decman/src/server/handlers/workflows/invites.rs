//! Sending a workflow's invites to its peers over Noise.

use std::collections::HashSet;

use sqlx::SqlitePool;

use super::onboarding::interpret_invite_reply;
use crate::{
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig, Peer},
    db::schema::SchemaRead,
    error::Result,
    noise::{Message, NoiseError, NoiseKeypair, parse_public_key, send_noise_message},
};

/// Which peers an invite goes to, and in which order.
pub(super) enum Recipients<'a> {
    /// The configured peers, in peer-table order, that are in the list and
    /// are not this node.
    Members(&'a [CantonId]),
    /// As [`Members`](Self::Members), and never the member being kicked.
    /// Each configured peer is logged as it is checked.
    KickSurvivors {
        invitees: &'a [CantonId],
        kicked: Option<&'a CantonId>,
    },
    /// The listed peers, in list order. A peer missing from the peer table
    /// is skipped with a warning.
    Listed(&'a [CantonId]),
}

/// The words an invite's log lines use.
pub(super) struct InviteLabels {
    /// Names the invite in "Sending <kind> invite" and in the reply check.
    pub(super) kind: &'static str,
    /// Goes before "invite" in the warnings about a skipped peer.
    pub(super) skip: &'static str,
    /// Goes before "invite" in the error about a failed send.
    pub(super) failed: &'static str,
}

/// One log line of an invite send. [`send_invites`] hands each line to the
/// function that [`invite_log!`] builds in the workflow's module, so the
/// line keeps that module as its tracing target.
pub(super) enum InviteLog<'a> {
    /// A configured peer is checked against the kick's recipients.
    CheckingPeer {
        peer: &'a CantonId,
        self_match: bool,
        kicked_match: bool,
    },
    /// The peer is this node.
    SkippingSelf { peer: &'a CantonId },
    /// The peer is the member being kicked.
    SkippingKicked { peer: &'a CantonId },
    /// A listed peer is missing from the peer table.
    NotInPeerTable { peer: &'a CantonId },
    /// The peer has no Noise key; `skip` is [`InviteLabels::skip`].
    NoPublicKey {
        skip: &'static str,
        peer: &'a CantonId,
    },
    /// The peer's Noise key does not parse.
    InvalidPublicKey {
        skip: &'static str,
        peer: &'a CantonId,
        error: &'a NoiseError,
    },
    /// The invite goes out; `kind` is [`InviteLabels::kind`].
    Sending { kind: &'static str, peer: &'a Peer },
    /// The peer could not be reached; `failed` is [`InviteLabels::failed`].
    SendFailed {
        failed: &'static str,
        peer: &'a CantonId,
        error: &'a NoiseError,
    },
}

/// Build the function that writes [`send_invites`]' [`InviteLog`] lines.
/// The function expands where the macro is called, so each line has the
/// calling module as its tracing target.
macro_rules! invite_log {
    () => {
        |line: $crate::server::handlers::workflows::invites::InviteLog<'_>| {
            use $crate::server::handlers::workflows::invites::InviteLog;
            match line {
                InviteLog::CheckingPeer {
                    peer,
                    self_match,
                    kicked_match,
                } => ::tracing::debug!(
                    "Checking peer {}: self_match={}, kicked_match={}",
                    peer,
                    self_match,
                    kicked_match
                ),
                InviteLog::SkippingSelf { peer } => {
                    ::tracing::debug!("Skipping {} - this is self", peer)
                }
                InviteLog::SkippingKicked { peer } => {
                    ::tracing::info!("Skipping invite to {} - they are being kicked", peer)
                }
                InviteLog::NotInPeerTable { peer } => ::tracing::warn!(
                    "Skipping invite to {} - peer not found in network config",
                    peer
                ),
                InviteLog::NoPublicKey { skip, peer } => ::tracing::warn!(
                    "Skipping {}invite to {} - no public key configured",
                    skip,
                    peer
                ),
                InviteLog::InvalidPublicKey { skip, peer, error } => ::tracing::warn!(
                    "Skipping {}invite to {} - invalid public key: {}",
                    skip,
                    peer,
                    error
                ),
                InviteLog::Sending { kind, peer } => ::tracing::info!(
                    "Sending {} invite to {} at {}:{}",
                    kind,
                    peer.participant_id,
                    peer.address,
                    peer.port
                ),
                InviteLog::SendFailed {
                    failed,
                    peer,
                    error,
                } => ::tracing::error!("Failed to send {}invite to {}: {}", failed, peer, error),
            }
        }
    };
}
pub(super) use invite_log;

/// Load the peers and this node's Noise key, build the invite with
/// `message`, and send it to each recipient. A peer that refuses the invite
/// fails the whole send; a peer that cannot be reached is logged and
/// skipped. `log` writes the log lines; build it with [`invite_log!`].
pub(super) async fn send_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    recipients: Recipients<'_>,
    labels: InviteLabels,
    log: fn(InviteLog<'_>),
    message: impl FnOnce() -> Result<Message>,
) -> Result {
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&config.key_file_path()).await?;
    let invite_message = message()?;

    match recipients {
        Recipients::Members(invitees) => {
            let current_participant_id = config.participant_id();
            let invitee_set: HashSet<&CantonId> = invitees.iter().collect();
            for peer in &network_config.peers {
                if !invitee_set.contains(&peer.participant_id)
                    || peer.participant_id == *current_participant_id
                {
                    continue;
                }
                send_invite(config, &keypair, peer, &invite_message, &labels, log).await?;
            }
        }
        Recipients::KickSurvivors { invitees, kicked } => {
            let current_participant_id = config.participant_id();
            let invitee_set: HashSet<&CantonId> = invitees.iter().collect();
            for peer in &network_config.peers {
                log(InviteLog::CheckingPeer {
                    peer: &peer.participant_id,
                    self_match: peer.participant_id == *current_participant_id,
                    kicked_match: Some(&peer.participant_id) == kicked,
                });

                // Only invite this run's surviving party members (see start_kick). The
                // self/kicked skips below are now subsumed by this but kept for clarity.
                if !invitee_set.contains(&peer.participant_id) {
                    continue;
                }

                // Skip self
                if peer.participant_id == *current_participant_id {
                    log(InviteLog::SkippingSelf {
                        peer: &peer.participant_id,
                    });
                    continue;
                }

                // Skip the peer being kicked (they won't participate in the kick workflow)
                if Some(&peer.participant_id) == kicked {
                    log(InviteLog::SkippingKicked {
                        peer: &peer.participant_id,
                    });
                    continue;
                }

                send_invite(config, &keypair, peer, &invite_message, &labels, log).await?;
            }
        }
        Recipients::Listed(peer_ids) => {
            for peer_id in peer_ids {
                let Some(peer) = network_config
                    .peers
                    .iter()
                    .find(|p| &p.participant_id == peer_id)
                else {
                    log(InviteLog::NotInPeerTable { peer: peer_id });
                    continue;
                };
                send_invite(config, &keypair, peer, &invite_message, &labels, log).await?;
            }
        }
    }

    Ok(())
}

/// Send `message` to one peer and read its reply. A peer without a usable
/// Noise key is skipped with a warning.
async fn send_invite(
    config: &NodeConfig,
    keypair: &NoiseKeypair,
    peer: &Peer,
    message: &Message,
    labels: &InviteLabels,
    log: fn(InviteLog<'_>),
) -> Result {
    if peer.public_key.is_empty() {
        log(InviteLog::NoPublicKey {
            skip: labels.skip,
            peer: &peer.participant_id,
        });
        return Ok(());
    }

    let peer_pub_key = match parse_public_key(&peer.public_key) {
        Ok(pk) => pk,
        Err(e) => {
            log(InviteLog::InvalidPublicKey {
                skip: labels.skip,
                peer: &peer.participant_id,
                error: &e,
            });
            return Ok(());
        }
    };

    let psk = keypair.derive_psk(&peer_pub_key);
    // Use participant_id as identity (must match what server expects in peer_keys lookup)
    let identity = config.participant_id().to_string();

    log(InviteLog::Sending {
        kind: labels.kind,
        peer,
    });

    match send_noise_message(&peer.address, peer.port, &psk, identity.as_bytes(), message).await {
        Ok(response) => {
            interpret_invite_reply(&peer.participant_id, labels.kind, &response)?;
        }
        Err(e) => {
            log(InviteLog::SendFailed {
                failed: labels.failed,
                peer: &peer.participant_id,
                error: &e,
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Pins the encoded invite of each workflow kind: its message type and
    //! the exact payload bytes a peer decodes.

    use common::api::{ContractDefinition, ContractDeploymentIntent};

    use super::{
        super::{
            add_party::add_party_invite,
            change_threshold::change_threshold_invite,
            contracts::contracts_invite,
            dars::dars_invite,
            kick::kick_invite,
            onboarding::onboarding_invite,
            test_support::{CapturedLogs, closed_port, insert_peers, peer, pid, test_node},
        },
        *,
    };
    use crate::{
        noise::MessageType,
        workflow::{
            AddPartyConfig, ChangeThresholdConfig, ContractsConfig, KickConfig,
            contracts::CommittedDeployment,
        },
    };

    fn cid(prefix: &str) -> anyhow::Result<CantonId> {
        CantonId::parse(&format!("{prefix}::1220{}", "a".repeat(64)))
    }

    fn id(prefix: &str) -> String {
        format!("{prefix}::1220{}", "a".repeat(64))
    }

    /// Assert `message` is a `msg_type` invite whose payload is exactly
    /// `payload`, with no routing instance.
    #[track_caller]
    fn assert_invite(message: &Message, msg_type: MessageType, payload: &str) {
        assert_eq!(message.msg_type, msg_type);
        assert_eq!(message.instance, "");
        assert_eq!(String::from_utf8_lossy(&message.payload), payload);
    }

    #[test]
    fn the_kick_invite_is_stable() -> anyhow::Result<()> {
        let config = KickConfig::new(
            cid("dec")?,
            Some(cid("p2")?),
            "k2".to_string(),
            1,
            Some(1),
            2,
            "dec-kick-1".to_string(),
        );
        let message = kick_invite(&config, &[cid("p3")?])?;
        assert_invite(
            &message,
            MessageType::InviteKick,
            &format!(
                r#"{{"dec_party_id":"{}","kicked_participant":"{}","kicked_owner_key":"k2","new_threshold":1,"previous_threshold":2,"confirmation_threshold":1,"participants":["{}"],"workflow_instance":"dec-kick-1"}}"#,
                id("dec"),
                id("p2"),
                id("p3")
            ),
        );
        Ok(())
    }

    #[test]
    fn the_add_party_invite_is_stable() -> anyhow::Result<()> {
        let config = AddPartyConfig::new(
            cid("dec")?,
            cid("p4")?,
            2,
            None,
            1,
            "dec-add-party-1".to_string(),
        );
        let message = add_party_invite(&cid("p1")?, &config, &[cid("p2")?, cid("p4")?])?;
        assert_invite(
            &message,
            MessageType::InviteAddParty,
            &format!(
                r#"{{"dec_party_id":"{}","new_participant":"{}","new_threshold":2,"previous_threshold":1,"participants":["{}","{}","{}"],"workflow_instance":"dec-add-party-1"}}"#,
                id("dec"),
                id("p4"),
                id("p2"),
                id("p4"),
                id("p1")
            ),
        );
        Ok(())
    }

    #[test]
    fn the_change_threshold_invite_is_stable() -> anyhow::Result<()> {
        let config = ChangeThresholdConfig::new(
            cid("dec")?,
            3,
            Some(2),
            2,
            "dec-change-threshold-1".to_string(),
        );
        let message = change_threshold_invite(&cid("p1")?, &config, &[cid("p2")?])?;
        assert_invite(
            &message,
            MessageType::InviteChangeThreshold,
            &format!(
                r#"{{"dec_party_id":"{}","new_threshold":3,"previous_threshold":2,"confirmation_threshold":2,"participants":["{}","{}"],"workflow_instance":"dec-change-threshold-1"}}"#,
                id("dec"),
                id("p2"),
                id("p1")
            ),
        );
        Ok(())
    }

    #[test]
    fn the_contracts_invite_is_stable() -> anyhow::Result<()> {
        let contract = |name: &str| ContractDefinition {
            id: name.to_string(),
            name: name.to_string(),
            package_id: "pkg".to_string(),
            module_name: "Governance.Rules".to_string(),
            entity_name: "GovernanceRules".to_string(),
            fields: Vec::new(),
        };
        let mut config = ContractsConfig::new(
            cid("dec")?,
            vec![cid("p1")?, cid("p2")?],
            vec![cid("p1")?, cid("p2")?],
            cid("dec")?,
            vec![
                contract("Rules"),
                contract(""),
                contract("Rules"),
                contract("Core"),
            ],
            "dec-contracts-1".to_string(),
        );
        config.committed_deployment = Some(CommittedDeployment {
            governance_threshold: 1,
            intents: vec![ContractDeploymentIntent {
                package_id: "pkg".to_string(),
                module_name: "Governance.Rules".to_string(),
                entity_name: "GovernanceRules".to_string(),
                argument_hash: "hash".to_string(),
            }],
        });
        let message = contracts_invite(&config, &[cid("p2")?])?;
        assert_invite(
            &message,
            MessageType::InviteContracts,
            &format!(
                r#"{{"dec_party_id":"{}","participants":["{}"],"package_names":["Core","Rules"],"contract_intents":[{{"package_id":"pkg","module_name":"Governance.Rules","entity_name":"GovernanceRules","argument_hash":"hash"}}],"workflow_instance":"dec-contracts-1"}}"#,
                id("dec"),
                id("p2")
            ),
        );
        Ok(())
    }

    /// A contracts run without recorded commitments has no invite.
    #[test]
    fn a_contracts_invite_needs_the_commitments() -> anyhow::Result<()> {
        let config = ContractsConfig::new(
            cid("dec")?,
            Vec::new(),
            Vec::new(),
            cid("dec")?,
            Vec::new(),
            "dec-contracts-1".to_string(),
        );
        let error = contracts_invite(&config, &[])
            .err()
            .ok_or_else(|| anyhow::anyhow!("an uncommitted run must have no invite"))?;
        assert_eq!(
            error.to_string(),
            "Contracts run dec-contracts-1 has no recorded deployment commitments; start a new run"
        );
        Ok(())
    }

    #[test]
    fn the_onboarding_invite_is_stable() -> anyhow::Result<()> {
        let peers = [cid("p2")?, cid("p3")?];
        // Without a chosen threshold the invite shows the majority default:
        // three owners, so two.
        let message = onboarding_invite(&peers, "newparty", None, None, "newparty-creation")?;
        assert_invite(
            &message,
            MessageType::InviteOnboarding,
            &format!(
                r#"{{"prefix":"newparty","participants":["{}","{}"],"threshold":2,"workflow_instance":"newparty-creation"}}"#,
                id("p2"),
                id("p3")
            ),
        );
        let message = onboarding_invite(&peers, "newparty", Some(3), Some(1), "newparty-creation")?;
        assert_invite(
            &message,
            MessageType::InviteOnboarding,
            &format!(
                r#"{{"prefix":"newparty","participants":["{}","{}"],"threshold":3,"confirmation_threshold":1,"workflow_instance":"newparty-creation"}}"#,
                id("p2"),
                id("p3")
            ),
        );
        Ok(())
    }

    /// Peers that cannot be reached, or that have no usable key, or that
    /// are missing from the peer table, are skipped; the send still
    /// succeeds, for every kind of recipient list.
    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn unreachable_and_unknown_peers_are_skipped(pool: SqlitePool) -> anyhow::Result<()> {
        let (me, p2, p3, p4) = (pid(1)?, pid(2)?, pid(3)?, pid(4)?);
        let port = closed_port()?;
        insert_peers(&pool, &[peer(&p2, port, true), peer(&p3, port, false)]).await?;
        let node = test_node(pool.clone(), &me).await?;
        let labels = || InviteLabels {
            kind: "test",
            skip: "",
            failed: "",
        };
        let invite = || Ok(Message::new(MessageType::InviteDars, Vec::new()));
        let everyone = [me.clone(), p2.clone(), p3.clone(), p4.clone()];

        for recipients in [
            Recipients::Members(&everyone),
            Recipients::KickSurvivors {
                invitees: &everyone,
                kicked: Some(&p3),
            },
            Recipients::Listed(&everyone[1..]),
        ] {
            send_invites(
                &node.data.config,
                &pool,
                recipients,
                labels(),
                invite_log!(),
                invite,
            )
            .await?;
        }
        Ok(())
    }

    /// `send_invites` writes each log line through the caller's
    /// `invite_log!`, so the line has the caller's module as its target. Each
    /// line reads as the workflow's own send loop wrote it.
    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn invite_lines_keep_the_callers_target_and_text(pool: SqlitePool) -> anyhow::Result<()> {
        let (me, p2, p3, p4, p5) = (pid(1)?, pid(2)?, pid(3)?, pid(4)?, pid(5)?);
        let port = closed_port()?;
        let mut bad_key = peer(&p5, port, false);
        bad_key.public_key = "not hex".to_string();
        insert_peers(
            &pool,
            &[
                peer(&me, port, false),
                peer(&p2, port, true),
                peer(&p3, port, false),
                bad_key,
            ],
        )
        .await?;
        let node = test_node(pool.clone(), &me).await?;
        let invite = || Ok(Message::new(MessageType::InviteDars, Vec::new()));
        let logs = CapturedLogs::default();
        let _default = tracing::subscriber::set_default(logs.subscriber());

        let listed = [p2.clone(), p3.clone(), p5.clone(), p4.clone()];
        send_invites(
            &node.data.config,
            &pool,
            Recipients::Listed(&listed),
            InviteLabels {
                kind: "add-party",
                skip: "add-party ",
                failed: "add-party ",
            },
            invite_log!(),
            invite,
        )
        .await?;
        let survivors = [me.clone(), p2.clone(), p3.clone()];
        send_invites(
            &node.data.config,
            &pool,
            Recipients::KickSurvivors {
                invitees: &survivors,
                kicked: Some(&p3),
            },
            InviteLabels {
                kind: "kick",
                skip: "",
                failed: "kick ",
            },
            invite_log!(),
            invite,
        )
        .await?;

        let here = module_path!();
        let lines = logs.lines()?;
        for expected in [
            format!(" INFO {here}: Sending add-party invite to {p2} at 127.0.0.1:{port}"),
            format!(" WARN {here}: Skipping add-party invite to {p3} - no public key configured"),
            format!(" WARN {here}: Skipping add-party invite to {p5} - invalid public key: "),
            format!(" WARN {here}: Skipping invite to {p4} - peer not found in network config"),
            format!("DEBUG {here}: Checking peer {me}: self_match=true, kicked_match=false"),
            format!("DEBUG {here}: Skipping {me} - this is self"),
            format!("DEBUG {here}: Checking peer {p3}: self_match=false, kicked_match=true"),
            format!(" INFO {here}: Skipping invite to {p3} - they are being kicked"),
            format!(" INFO {here}: Sending kick invite to {p2} at 127.0.0.1:{port}"),
            format!("ERROR {here}: Failed to send add-party invite to {p2}: "),
            format!("ERROR {here}: Failed to send kick invite to {p2}: "),
        ] {
            assert!(
                lines.iter().any(|line| line.starts_with(&expected)),
                "no line starts with {expected:?}: {lines:#?}"
            );
        }
        Ok(())
    }

    #[test]
    fn the_dars_invite_is_stable() -> anyhow::Result<()> {
        let message = dars_invite(
            &[cid("p2")?],
            &["app.dar".to_string()],
            &["hash".to_string()],
            "dars-distribute-1",
        )?;
        assert_invite(
            &message,
            MessageType::InviteDars,
            &format!(
                r#"{{"dar_filenames":["app.dar"],"dar_hashes":["hash"],"participants":["{}"],"workflow_instance":"dars-distribute-1"}}"#,
                id("p2")
            ),
        );
        Ok(())
    }
}
