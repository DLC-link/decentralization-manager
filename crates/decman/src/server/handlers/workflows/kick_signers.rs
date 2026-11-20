//! Who signs a kick: the invitees, the quorum and the signing-key checks.

use std::collections::HashSet;

use canton_proto_rs::com::digitalasset::canton::protocol::v30::PartyToParticipant;
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    server::error::ApiError,
    utils,
    workflow::{
        signing_keys::{
            KeyClaimError, ensure_survivor_keys_preserved, kick_key_claims,
            known_signing_keys_by_member, members_requiring_key_adoption,
            recorded_signing_keys_by_member, requires_signing_key_adoption,
        },
        topology,
    },
};

/// The members that stay after a kick, split by whether this node can invite
/// them. Neither list holds this node or the kicked host.
#[derive(Debug, PartialEq)]
pub(super) struct KickSigners {
    /// Members that are configured peers. The kick invites these to sign.
    pub(super) invitees: Vec<CantonId>,
    /// Members that are not configured peers. The kick cannot reach these.
    pub(super) not_peers: Vec<CantonId>,
}

/// Split the members that stay after a kick into invitees and members that
/// are not peers. The kicked host never signs, so its peer entry plays no part.
pub(super) fn kick_signers(
    members: &HashSet<CantonId>,
    peer_ids: &HashSet<CantonId>,
    self_id: &CantonId,
    kicked_host: Option<&CantonId>,
) -> KickSigners {
    let (mut invitees, mut not_peers): (Vec<CantonId>, Vec<CantonId>) = members
        .iter()
        .filter(|member| *member != self_id && Some(*member) != kicked_host)
        .cloned()
        .partition(|member| peer_ids.contains(member));
    invitees.sort();
    not_peers.sort();
    KickSigners {
        invitees,
        not_peers,
    }
}

/// Why a kick cannot collect enough signatures, or `None` when it can.
///
/// This node and each invitee sign. Both the current namespace threshold
/// (`current`, `None` when the party is not cached) and the requested one
/// must be reachable, because the P2P mapping is submitted after the new
/// namespace takes effect.
pub(super) fn kick_quorum_refusal(
    current: Option<usize>,
    requested: usize,
    signers: &KickSigners,
) -> Option<String> {
    let peers = signers.invitees.len();
    let available = 1 + peers;
    let current_limits = current.is_some_and(|threshold| threshold > available);
    let requested_limits = requested > available;
    let mut message = if current_limits || requested_limits {
        let limit = if let Some(threshold) = current.filter(|_| current_limits) {
            format!("Cannot kick: the party's current namespace threshold is {threshold}.")
        } else {
            format!("Cannot kick: the requested new_threshold is {requested}.")
        };
        match peers {
            0 => format!("{limit} Only this node can sign."),
            1 => format!("{limit} Only 2 members can sign: this node and 1 configured peer."),
            _ => format!(
                "{limit} Only {available} members can sign: this node and {peers} configured \
                 peers."
            ),
        }
    } else if peers == 0 {
        "Cannot kick: this node has no other hosting member to co-sign the kick.".to_string()
    } else {
        return None;
    };
    for id in &signers.not_peers {
        message.push_str(&format!(
            " {id} is a member of the party but is not a configured peer of this node."
        ));
    }
    // Every member that stays signs once it is a peer. The run also needs a
    // co-signer besides this node.
    let reachable = available + signers.not_peers.len();
    let required = current.unwrap_or(1).max(requested).max(2);
    if !signers.not_peers.is_empty() && reachable >= required {
        message.push_str(match signers.not_peers.len() {
            1 => " Add it as a peer, then retry.",
            _ => " Add them as peers, then retry.",
        });
    } else if current_limits {
        message.push_str(
            " Lower the threshold with Change Threshold first, while every host can still sign.",
        );
        if requested_limits {
            message.push_str(&format!(
                " Then pick a new_threshold of {available} or lower."
            ));
        }
    } else if requested_limits {
        message.push_str(&format!(" Pick a new_threshold of {available} or lower."));
    }
    Some(message)
}

/// A namespace quorum cannot authorize a new inline signing key on behalf
/// of an unreachable member. Read topology only when a survivor is not a peer.
pub(super) async fn kick_key_adoption_refusal(
    config: &NodeConfig,
    db: &SqlitePool,
    party: &CantonId,
    not_peers: &[CantonId],
    removed: &str,
    kicked: Option<&CantonId>,
) -> Result<Option<String>> {
    if not_peers.is_empty() {
        return Ok(None);
    }
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let mapping = topology::fetch_p2p_mapping(config, &synchronizer_id, party).await?;
    kick_key_adoption_refusal_for_mapping(config, db, party, not_peers, removed, kicked, &mapping)
        .await
}

async fn kick_key_adoption_refusal_for_mapping(
    config: &NodeConfig,
    db: &SqlitePool,
    party: &CantonId,
    not_peers: &[CantonId],
    removed: &str,
    kicked: Option<&CantonId>,
    mapping: &PartyToParticipant,
) -> Result<Option<String>> {
    let inline = mapping
        .party_signing_keys
        .as_ref()
        .map_or(&[][..], |keys| keys.keys.as_slice());
    let kicked = kicked.map(ToString::to_string);
    let survivors: Vec<String> = mapping
        .participants
        .iter()
        .filter(|host| Some(&host.participant_uid) != kicked.as_ref())
        .map(|host| host.participant_uid.clone())
        .collect();
    let members: Vec<String> = not_peers
        .iter()
        .map(ToString::to_string)
        .filter(|member| survivors.contains(member))
        .collect();
    // With no inline keys every missing survivor must sign adoption. No
    // attribution lookup can make that kick safe to start without them.
    if inline.is_empty() {
        return Ok(kick_missing_adopters(&members));
    }
    let claims = known_signing_keys_by_member(config, db, party, inline).await?;
    let claims = kick_key_claims(Some(removed), &survivors, &claims, &members)?;
    if !requires_signing_key_adoption(mapping) {
        ensure_survivor_keys_preserved(inline, &survivors, &claims)?;
        return Ok(None);
    }
    let required = members_requiring_key_adoption(mapping, &members, &claims)?;
    Ok(kick_missing_adopters(&required))
}

fn kick_missing_adopters(required: &[String]) -> Option<String> {
    let (owner, add) = match required {
        [] => return None,
        [member] => (
            format!("{member} owns such a key but is not a configured peer of this node."),
            "Add it as a peer, then retry.",
        ),
        several => (
            format!(
                "{members} own such keys but are not configured peers of this node.",
                members = several.join(", ")
            ),
            "Add them as peers, then retry.",
        ),
    };
    Some(format!(
        "Cannot kick: the party still keeps signing keys in the older key mapping. The kick \
         moves them into the hosting list. The owner of each moved key must sign. {owner} {add}"
    ))
}

/// Refuse a kick whose proposal could not keep each remaining member's
/// signing key. Only a record says which key is whose, so every remaining
/// host other than this node needs one. This node reads its own key from its
/// vault when it builds the proposal. Reads only the database.
///
/// # Errors
///
/// [`KeyClaimError`] when a key is not on record or two remaining members
/// claim one key. Any other error is a database failure.
pub(super) async fn check_kick_survivor_keys(
    db: &SqlitePool,
    party: &CantonId,
    self_id: &CantonId,
    signers: &KickSigners,
    removed: &str,
) -> Result {
    let claims = recorded_signing_keys_by_member(db, party).await?;
    let survivors: Vec<String> = signers
        .invitees
        .iter()
        .chain(&signers.not_peers)
        .chain(std::iter::once(self_id))
        .map(ToString::to_string)
        .collect();
    kick_key_claims(Some(removed), &survivors, &claims, &[])?;

    let unknown = |members: &[CantonId]| -> Vec<String> {
        members
            .iter()
            .map(ToString::to_string)
            .filter(|member| !claims.contains_key(member))
            .collect()
    };
    let peers = unknown(&signers.invitees);
    let not_peers = unknown(&signers.not_peers);
    if peers.is_empty() && not_peers.is_empty() {
        return Ok(());
    }
    let mut message = format!(
        "Cannot kick: this node has no record of the signing key of {members}. The kick must \
         keep the key of each member that stays.",
        members = peers
            .iter()
            .chain(&not_peers)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
    match not_peers.as_slice() {
        [] => message
            .push_str(" Refresh /decentralized-parties so each peer reports its key, then retry."),
        [member] => message.push_str(&format!(
            " {member} is not a configured peer of this node, so a refresh cannot read its \
             key. Add it as a peer, refresh /decentralized-parties, then retry."
        )),
        several => message.push_str(&format!(
            " {members} are not configured peers of this node, so a refresh cannot read their \
             keys. Add them as peers, refresh /decentralized-parties, then retry.",
            members = several.join(", ")
        )),
    }
    Err(KeyClaimError::Unresolved(message).into())
}

/// The answer to a failed signing-key check of a kick. A key-claim refusal
/// says what to do, so it goes back as it is. Any other failure is logged and
/// answered with generic text.
pub(super) fn kick_key_check_failure(party: &CantonId, e: &anyhow::Error) -> ApiError {
    match e.downcast_ref::<KeyClaimError>() {
        Some(KeyClaimError::Conflict(error)) => {
            tracing::warn!("Kick of {party} refused: {error}");
            ApiError::Conflict(error.clone())
        }
        Some(KeyClaimError::Unresolved(error)) => {
            tracing::info!("Kick of {party} refused: {error}");
            ApiError::ServiceUnavailable(error.clone())
        }
        None => {
            tracing::error!("Cannot check the signing keys of {party} for a kick: {e:#}");
            ApiError::ServiceUnavailable(
                "Cannot check the party's signing keys right now. Retry later.".to_string(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use anyhow::Context;
    use canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::SigningKeysWithThreshold,
        protocol::v30::{enums::ParticipantPermission, party_to_participant::HostingParticipant},
    };

    use crate::{db::MIGRATOR, workflow::signing_keys::signing_keys_after_kick};

    use super::super::test_support::{
        cache_kick_party, ids, kick_public_key, pid, record_signing_keys,
    };
    use super::*;

    fn signers(invitees: &[u8], not_peers: &[u8]) -> anyhow::Result<KickSigners> {
        Ok(KickSigners {
            invitees: invitees
                .iter()
                .map(|tag| pid(*tag))
                .collect::<anyhow::Result<_>>()?,
            not_peers: not_peers
                .iter()
                .map(|tag| pid(*tag))
                .collect::<anyhow::Result<_>>()?,
        })
    }

    #[test]
    fn kick_signers_invites_only_members_that_are_peers() -> anyhow::Result<()> {
        let split = kick_signers(
            &ids(&[1, 2, 3, 4])?,
            &ids(&[1, 2, 3, 9])?,
            &pid(1)?,
            Some(&pid(4)?),
        );
        assert_eq!(split, signers(&[2, 3], &[])?);
        Ok(())
    }

    #[test]
    fn a_kicked_host_that_is_not_a_peer_is_neither_invited_nor_reported() -> anyhow::Result<()> {
        let split = kick_signers(&ids(&[1, 2, 3])?, &ids(&[2])?, &pid(1)?, Some(&pid(3)?));
        assert_eq!(split, signers(&[2], &[])?);
        Ok(())
    }

    #[test]
    fn kick_signers_names_surviving_members_that_are_not_peers() -> anyhow::Result<()> {
        let split = kick_signers(&ids(&[1, 2, 3, 4])?, &ids(&[2])?, &pid(1)?, Some(&pid(4)?));
        assert_eq!(split, signers(&[2], &[3])?);
        Ok(())
    }

    #[test]
    fn a_kick_below_quorum_names_the_members_that_are_not_peers() -> anyhow::Result<()> {
        let refusal = kick_quorum_refusal(Some(3), 2, &signers(&[2], &[3, 5])?)
            .context("a kick two signers short of threshold 3 must be refused")?;
        assert!(
            refusal.contains(
                "the party's current namespace threshold is 3. Only 2 members can sign: this \
                 node and 1 configured peer."
            ),
            "{refusal}"
        );
        for tag in [3, 5] {
            assert!(
                refusal.contains(&format!(
                    "{} is a member of the party but is not a configured peer of this node.",
                    pid(tag)?
                )),
                "{refusal}"
            );
        }
        assert!(
            refusal.ends_with(" Add them as peers, then retry."),
            "{refusal}"
        );
        assert!(!refusal.contains(&pid(2)?.to_string()), "{refusal}");
        Ok(())
    }

    /// Adding the members that are not peers would still leave the kick
    /// short, so the advice is about the threshold instead.
    #[test]
    fn a_kick_that_peers_cannot_save_points_to_the_threshold() -> anyhow::Result<()> {
        let refusal = kick_quorum_refusal(Some(4), 2, &signers(&[2], &[3])?)
            .context("three reachable signers cannot meet threshold 4")?;
        assert!(refusal.contains(&pid(3)?.to_string()), "{refusal}");
        assert!(!refusal.contains("Add it as a peer"), "{refusal}");
        assert!(
            refusal.ends_with(
                " Lower the threshold with Change Threshold first, while every host can still \
                 sign."
            ),
            "{refusal}"
        );
        Ok(())
    }

    #[test]
    fn a_short_kick_says_which_threshold_is_the_limit() -> anyhow::Result<()> {
        let current = kick_quorum_refusal(Some(3), 2, &signers(&[2], &[])?)
            .context("the current threshold 3 is out of reach")?;
        assert!(
            current.starts_with("Cannot kick: the party's current namespace threshold is 3."),
            "{current}"
        );
        assert!(current.contains("Change Threshold"), "{current}");
        assert!(!current.contains("Pick a new_threshold"), "{current}");

        let requested = kick_quorum_refusal(Some(2), 3, &signers(&[2], &[])?)
            .context("the requested threshold 3 is out of reach")?;
        assert!(
            requested.starts_with("Cannot kick: the requested new_threshold is 3."),
            "{requested}"
        );
        assert!(
            requested.ends_with(" Pick a new_threshold of 2 or lower."),
            "{requested}"
        );
        assert!(!requested.contains("Change Threshold"), "{requested}");

        let both = kick_quorum_refusal(Some(4), 3, &signers(&[2], &[])?)
            .context("both thresholds are out of reach")?;
        assert!(both.contains("current namespace threshold is 4."), "{both}");
        assert!(
            both.ends_with(" Then pick a new_threshold of 2 or lower."),
            "{both}"
        );
        Ok(())
    }

    #[test]
    fn the_key_adoption_refusal_names_each_owner_and_the_next_step() -> anyhow::Result<()> {
        assert_eq!(kick_missing_adopters(&[]), None);
        let one = kick_missing_adopters(&[pid(3)?.to_string()])
            .context("one owner that is not a peer must sign")?;
        assert_eq!(
            one,
            format!(
                "Cannot kick: the party still keeps signing keys in the older key mapping. The \
                 kick moves them into the hosting list. The owner of each moved key must sign. \
                 {p3} owns such a key but is not a configured peer of this node. Add it as a \
                 peer, then retry.",
                p3 = pid(3)?
            )
        );
        let two = kick_missing_adopters(&[pid(3)?.to_string(), pid(4)?.to_string()])
            .context("two owners that are not peers must sign")?;
        assert!(
            two.ends_with(
                " own such keys but are not configured peers of this node. Add them as peers, \
                 then retry."
            ),
            "{two}"
        );
        assert!(!two.contains("inline"), "{two}");
        Ok(())
    }

    #[test]
    fn a_kick_at_quorum_passes_with_a_member_that_is_not_a_peer() -> anyhow::Result<()> {
        assert_eq!(kick_quorum_refusal(Some(2), 2, &signers(&[2], &[3])?), None);
        Ok(())
    }

    #[test]
    fn an_uncached_threshold_skips_the_quorum_check() -> anyhow::Result<()> {
        assert_eq!(kick_quorum_refusal(None, 2, &signers(&[2], &[3])?), None);
        Ok(())
    }

    #[test]
    fn a_kick_with_no_invitee_is_refused() -> anyhow::Result<()> {
        let alone = kick_quorum_refusal(None, 1, &signers(&[], &[])?)
            .context("a kick with no co-signer must be refused")?;
        assert_eq!(
            alone,
            "Cannot kick: this node has no other hosting member to co-sign the kick."
        );

        let refusal = kick_quorum_refusal(Some(1), 1, &signers(&[], &[3])?)
            .context("a kick whose only survivor is not a peer must be refused")?;
        assert!(
            refusal.contains(&format!("{} is a member of the party", pid(3)?)),
            "{refusal}"
        );
        assert!(
            refusal.ends_with(" Add it as a peer, then retry."),
            "{refusal}"
        );
        Ok(())
    }

    /// The kicked member reports a remaining member's key as its own. That
    /// claim is ignored, so it cannot block its own kick.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_target_that_claims_a_survivors_key_passes_the_key_check(pool: SqlitePool) -> Result {
        let party = pid(53)?;
        cache_kick_party(&pool, &party, 2, &[1, 2, 3, 4], &[], &[2, 3]).await?;
        record_signing_keys(&pool, &party, &[(2, 2), (3, 3), (4, 2)]).await?;
        let signers = kick_signers(
            &ids(&[1, 2, 3, 4])?,
            &ids(&[2, 3])?,
            &pid(1)?,
            Some(&pid(4)?),
        );

        check_kick_survivor_keys(&pool, &party, &pid(1)?, &signers, &pid(4)?.to_string()).await
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_kick_with_no_missing_peer_skips_the_extra_topology_read(pool: SqlitePool) -> Result {
        let mut config = NodeConfig::default();
        config.canton.admin_api_host = "[".to_string();
        assert_eq!(
            kick_key_adoption_refusal(&config, &pool, &pid(48)?, &[], "unused", None).await?,
            None
        );
        Ok(())
    }

    async fn fully_inline_kick_keys(
        pool: &SqlitePool,
    ) -> Result<(NodeConfig, CantonId, PartyToParticipant)> {
        let party = pid(49)?;
        cache_kick_party(pool, &party, 2, &[1, 2, 3, 4], &[], &[2]).await?;
        record_signing_keys(pool, &party, &[(1, 1), (2, 2), (3, 3), (4, 4)]).await?;
        let mut config = NodeConfig::default();
        config.node.participant_id = Some(pid(1)?);
        config.canton.admin_api_host = "[".to_string();
        let mapping = PartyToParticipant {
            party: party.to_string(),
            threshold: 2,
            participants: (1..=4)
                .map(|tag| -> Result<HostingParticipant> {
                    Ok(HostingParticipant {
                        participant_uid: pid(tag)?.to_string(),
                        permission: ParticipantPermission::Confirmation as i32,
                        ..Default::default()
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            party_signing_keys: Some(SigningKeysWithThreshold {
                keys: (1..=4).map(kick_public_key).collect(),
                threshold: 2,
            }),
        };
        Ok((config, party, mapping))
    }

    /// The kicked member claims the key of a remaining member that is not a
    /// peer. The preflight ignores that claim, and the kick removes the
    /// target's own key by elimination. A remaining member that claims the
    /// same key as another one is still refused.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn fully_inline_preflight_ignores_a_target_claiming_a_non_peers_key(
        pool: SqlitePool,
    ) -> Result {
        let (config, party, mapping) = fully_inline_kick_keys(&pool).await?;
        record_signing_keys(&pool, &party, &[(4, 3)]).await?;
        let preflight = |pool: SqlitePool| {
            let (config, party, mapping) = (config.clone(), party.clone(), mapping.clone());
            async move {
                kick_key_adoption_refusal_for_mapping(
                    &config,
                    &pool,
                    &party,
                    &[pid(3)?],
                    &pid(4)?.to_string(),
                    Some(&pid(4)?),
                    &mapping,
                )
                .await
            }
        };

        assert_eq!(preflight(pool.clone()).await?, None);
        let keys = &mapping
            .party_signing_keys
            .as_ref()
            .context("inline keys")?
            .keys;
        let attributed = known_signing_keys_by_member(&config, &pool, &party, keys).await?;
        let kept = signing_keys_after_kick(
            keys,
            &pid(4)?.to_string(),
            &[
                pid(1)?.to_string(),
                pid(2)?.to_string(),
                pid(3)?.to_string(),
            ],
            &attributed,
        )?;
        assert_eq!(kept, keys[..3].to_vec());

        record_signing_keys(&pool, &party, &[(2, 3)]).await?;
        let error = preflight(pool)
            .await
            .expect_err("two remaining members claim one key");
        assert!(
            matches!(
                error.downcast_ref::<KeyClaimError>(),
                Some(KeyClaimError::Conflict(_))
            ),
            "{error:#}"
        );
        let response = kick_key_check_failure(&party, &error).into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_non_peer_needs_a_unique_known_key_even_on_a_fully_inline_party(
        pool: SqlitePool,
    ) -> Result {
        let (config, party, mut mapping) = fully_inline_kick_keys(&pool).await?;
        sqlx::query(
            "UPDATE dec_party_participant SET signing_key = NULL WHERE participant_uid = ?",
        )
        .bind(pid(3)?.to_string())
        .execute(&pool)
        .await?;
        let error = kick_key_adoption_refusal_for_mapping(
            &config,
            &pool,
            &party,
            &[pid(3)?],
            &pid(4)?.to_string(),
            Some(&pid(4)?),
            &mapping,
        )
        .await
        .expect_err("unknown non-peer ownership must request a refresh")
        .to_string();
        assert!(error.contains(&pid(3)?.to_string()), "{error}");
        assert!(error.contains("no record of the signing key"), "{error}");
        assert!(
            error.contains("A member that is not a peer must first be added as one."),
            "{error}"
        );
        sqlx::query("UPDATE dec_party_participant SET signing_key = ? WHERE participant_uid = ?")
            .bind(utils::compute_fingerprint(&kick_public_key(3)).to_string())
            .bind(pid(3)?.to_string())
            .execute(&pool)
            .await?;
        assert_eq!(
            kick_key_adoption_refusal_for_mapping(
                &config,
                &pool,
                &party,
                &[pid(3)?],
                &pid(4)?.to_string(),
                Some(&pid(4)?),
                &mapping,
            )
            .await?,
            None
        );
        // A reachable p2 needs adoption, but non-peer p3 is already inline.
        mapping.party_signing_keys = Some(SigningKeysWithThreshold {
            keys: vec![kick_public_key(1), kick_public_key(3)],
            threshold: 2,
        });
        assert_eq!(
            kick_key_adoption_refusal_for_mapping(
                &config,
                &pool,
                &party,
                &[pid(3)?],
                &pid(4)?.to_string(),
                Some(&pid(4)?),
                &mapping,
            )
            .await?,
            None
        );
        Ok(())
    }
}
