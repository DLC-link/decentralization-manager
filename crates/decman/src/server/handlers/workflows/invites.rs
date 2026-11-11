//! Sending a workflow's invites to its peers over Noise.

use std::collections::HashSet;

use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig, Peer},
    db::schema::SchemaRead,
    error::Result,
    noise::{Message, NoiseKeypair, parse_public_key, send_noise_message},
};

use super::onboarding::interpret_invite_reply;

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

/// Load the peers and this node's Noise key, build the invite with
/// `message`, and send it to each recipient. A peer that refuses the invite
/// fails the whole send; a peer that cannot be reached is logged and
/// skipped.
pub(super) async fn send_invites(
    config: &NodeConfig,
    db: &SqlitePool,
    recipients: Recipients<'_>,
    labels: InviteLabels,
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
                send_invite(config, &keypair, peer, &invite_message, &labels).await?;
            }
        }
        Recipients::KickSurvivors { invitees, kicked } => {
            let current_participant_id = config.participant_id();
            let invitee_set: HashSet<&CantonId> = invitees.iter().collect();
            for peer in &network_config.peers {
                tracing::debug!(
                    "Checking peer {}: self_match={}, kicked_match={}",
                    peer.participant_id,
                    peer.participant_id == *current_participant_id,
                    Some(&peer.participant_id) == kicked
                );

                // Only invite this run's surviving party members (see start_kick). The
                // self/kicked skips below are now subsumed by this but kept for clarity.
                if !invitee_set.contains(&peer.participant_id) {
                    continue;
                }

                // Skip self
                if peer.participant_id == *current_participant_id {
                    tracing::debug!("Skipping {} - this is self", peer.participant_id);
                    continue;
                }

                // Skip the peer being kicked (they won't participate in the kick workflow)
                if Some(&peer.participant_id) == kicked {
                    tracing::info!(
                        "Skipping invite to {} - they are being kicked",
                        peer.participant_id
                    );
                    continue;
                }

                send_invite(config, &keypair, peer, &invite_message, &labels).await?;
            }
        }
        Recipients::Listed(peer_ids) => {
            for peer_id in peer_ids {
                let Some(peer) = network_config
                    .peers
                    .iter()
                    .find(|p| &p.participant_id == peer_id)
                else {
                    tracing::warn!(
                        "Skipping invite to {peer_id} - peer not found in network config"
                    );
                    continue;
                };
                send_invite(config, &keypair, peer, &invite_message, &labels).await?;
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
) -> Result {
    if peer.public_key.is_empty() {
        tracing::warn!(
            "Skipping {}invite to {} - no public key configured",
            labels.skip,
            peer.participant_id
        );
        return Ok(());
    }

    let peer_pub_key = match parse_public_key(&peer.public_key) {
        Ok(pk) => pk,
        Err(e) => {
            tracing::warn!(
                "Skipping {}invite to {} - invalid public key: {e}",
                labels.skip,
                peer.participant_id
            );
            return Ok(());
        }
    };

    let psk = keypair.derive_psk(&peer_pub_key);
    // Use participant_id as identity (must match what server expects in peer_keys lookup)
    let identity = config.participant_id().to_string();

    tracing::info!(
        "Sending {} invite to {} at {}:{}",
        labels.kind,
        peer.participant_id,
        peer.address,
        peer.port
    );

    match send_noise_message(&peer.address, peer.port, &psk, identity.as_bytes(), message).await {
        Ok(response) => {
            interpret_invite_reply(&peer.participant_id, labels.kind, &response)?;
        }
        Err(e) => {
            tracing::error!(
                "Failed to send {}invite to {}: {e}",
                labels.failed,
                peer.participant_id
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Pins the encoded invite of each workflow kind: its message type and
    //! the exact payload bytes a peer decodes.

    use common::api::{ContractDefinition, ContractDeploymentIntent};

    use super::super::{
        add_party::add_party_invite, change_threshold::change_threshold_invite,
        contracts::contracts_invite, dars::dars_invite, kick::kick_invite,
        onboarding::onboarding_invite,
    };
    use super::*;
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
        use super::super::test_support::{closed_port, insert_peers, peer, pid, test_node};

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
            send_invites(&node.data.config, &pool, recipients, labels(), invite).await?;
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
