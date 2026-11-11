//! Resolving a kick target and its threshold bounds.

use std::collections::HashSet;

use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    db::{
        rows::{DecPartyOwnerRow, DecPartyParticipantRow},
        schema::SchemaRead,
    },
    error::Result,
    server::error::ApiError,
    workflow::thresholds::{PartyThresholds, carried_confirmation_threshold},
};

// ============================================================================
// Kick Workflow
// ============================================================================

/// A party's member count, as a threshold bound has to see it.
///
/// `members` is the cached membership and `self_id` is this node. The
/// coordinator signs the proposals, so it is necessarily a member — but it does
/// not appear in its own `peers` table, so a member set derived from that list
/// omits it. Counting it explicitly keeps the bound right however `members` was
/// built, including the fallback that substitutes the configured peer set when
/// nothing is cached.
pub(super) fn party_member_count(members: &HashSet<CantonId>, self_id: &CantonId) -> usize {
    members.len() + usize::from(!members.contains(self_id))
}

/// The highest threshold a kick may set.
///
/// The namespace threshold counts the owners left, and the signing threshold
/// that follows it counts the hosts' keys left. A host's kick takes one of
/// each. An owner that hosts nothing leaves every host in place.
pub(super) fn kick_threshold_bound(
    target_hosts: bool,
    member_count: usize,
    owner_count: usize,
) -> i32 {
    let hosts_left = member_count.saturating_sub(usize::from(target_hosts));
    let owners_left = if owner_count == 0 {
        // A party missing from the owner cache bounds by its hosts alone, as
        // the kick always did.
        hosts_left
    } else {
        owner_count - 1
    };
    i32::try_from(hosts_left.min(owners_left)).unwrap_or(i32::MAX)
}

/// The member a kick removes.
#[derive(Debug, PartialEq)]
pub(super) struct KickTarget {
    /// The namespace owner key the kick removes.
    pub(super) owner_key: String,
    /// The participant named for the member, when one is known.
    pub(super) participant: Option<CantonId>,
    /// Whether that participant hosts the party, and so leaves the mapping too.
    pub(super) hosts: bool,
}

#[derive(Debug, PartialEq)]
pub(super) enum KickTargetError {
    BadRequest(String),
    Conflict(String),
}

impl From<KickTargetError> for ApiError {
    fn from(error: KickTargetError) -> Self {
        match error {
            KickTargetError::BadRequest(error) => Self::BadRequest(error),
            KickTargetError::Conflict(error) => Self::Conflict(error),
        }
    }
}

/// Work out which member a kick request names, from the cached membership.
///
/// A host is named by its participant, and its owner key comes from the cache.
/// An owner that hosts nothing is named by its owner key, or by the
/// participant that claimed the key. Such a kick waits until every host's own
/// key is known, because until then the key may belong to a host whose key
/// has not been resolved. A claim does not settle it: any peer can claim any
/// key, and every peer refuses to sign until it knows each host's key.
///
/// A host's cached key counts as known only while it is still an owner. A host
/// that rotated its key would otherwise have its new key read as unclaimed.
pub(super) fn resolve_kick_target(
    participant: Option<&CantonId>,
    owner_key: Option<&str>,
    hosts: &[DecPartyParticipantRow],
    owners: &[DecPartyOwnerRow],
) -> std::result::Result<KickTarget, KickTargetError> {
    const REFRESH: &str = "Try refreshing /decentralized-parties first.";
    let host_of = |uid: &CantonId| {
        let uid = uid.to_string();
        hosts.iter().find(|host| host.participant_uid == uid)
    };
    // A party missing from the owner cache has nothing to check a key
    // against, so its hosts' cached keys stand as they are.
    let known_key = |host: &DecPartyParticipantRow| {
        host.owner_key
            .clone()
            .filter(|key| owners.is_empty() || owners.iter().any(|o| o.owner_key == *key))
    };
    let unresolved_host = |key: &str| {
        hosts
            .iter()
            .find(|host| known_key(host).is_none())
            .map(|host| {
                KickTargetError::Conflict(format!(
                    "Cannot tell whether owner key {key} belongs to host {uid}, whose current \
                     owner key has not been resolved yet. {REFRESH}",
                    uid = host.participant_uid
                ))
            })
    };

    let Some(key) = owner_key else {
        let Some(participant) = participant else {
            return Err(KickTargetError::BadRequest(
                "Name the member to kick with participant_id, owner_key, or both".to_string(),
            ));
        };
        if let Some(host) = host_of(participant) {
            return match known_key(host) {
                Some(key) => Ok(KickTarget {
                    owner_key: key,
                    participant: Some(participant.clone()),
                    hosts: true,
                }),
                None => Err(KickTargetError::Conflict(format!(
                    "Participant {participant} hosts the party, but its current owner key has \
                     not been resolved yet. {REFRESH}"
                ))),
            };
        }
        let uid = participant.to_string();
        let claimed: Vec<&DecPartyOwnerRow> = owners
            .iter()
            .filter(|owner| owner.participant_uid.as_deref() == Some(uid.as_str()))
            .collect();
        return match claimed.as_slice() {
            [] => Err(KickTargetError::BadRequest(format!(
                "Cannot kick {participant}: not a member of this decentralized party"
            ))),
            [owner] => match unresolved_host(&owner.owner_key) {
                Some(error) => Err(error),
                None => Ok(KickTarget {
                    owner_key: owner.owner_key.clone(),
                    participant: Some(participant.clone()),
                    hosts: false,
                }),
            },
            several => Err(KickTargetError::Conflict(format!(
                "Participant {participant} claims {count} owner keys of this party. Name the \
                 one to remove with owner_key",
                count = several.len()
            ))),
        };
    };

    let Some(owner) = owners.iter().find(|owner| owner.owner_key == key) else {
        return Err(KickTargetError::BadRequest(format!(
            "Owner key {key} is not an owner of this decentralized party. {REFRESH}"
        )));
    };

    if let Some(host) = hosts
        .iter()
        .find(|host| host.owner_key.as_deref() == Some(key))
    {
        let uid = CantonId::parse(&host.participant_uid).map_err(|e| {
            KickTargetError::Conflict(format!(
                "Owner key {key} belongs to host {uid}, whose cached id does not parse: {e}",
                uid = host.participant_uid
            ))
        })?;
        if let Some(participant) = participant
            && *participant != uid
        {
            return Err(KickTargetError::BadRequest(format!(
                "Owner key {key} belongs to {uid}, not {participant}"
            )));
        }
        return Ok(KickTarget {
            owner_key: key.to_string(),
            participant: Some(uid),
            hosts: true,
        });
    }

    let claimant = owner
        .participant_uid
        .as_deref()
        .and_then(|uid| CantonId::parse(uid).ok());
    if let Some(claimant) = &claimant
        && host_of(claimant).is_some()
    {
        return Err(KickTargetError::Conflict(format!(
            "Owner key {key} was claimed by host {claimant}, but the cache holds another key \
             for it. {REFRESH}"
        )));
    }
    if let Some(error) = unresolved_host(key) {
        return Err(error);
    }
    if let Some(participant) = participant {
        if host_of(participant).is_some() {
            return Err(KickTargetError::BadRequest(format!(
                "Participant {participant} hosts the party under another owner key, not {key}"
            )));
        }
        match &claimant {
            Some(claimant) if claimant == participant => {}
            Some(claimant) => {
                return Err(KickTargetError::BadRequest(format!(
                    "Owner key {key} was claimed by {claimant}, not {participant}"
                )));
            }
            None => {
                return Err(KickTargetError::BadRequest(format!(
                    "No peer has claimed owner key {key}, so it cannot be tied to \
                     {participant}. Name only the owner key"
                )));
            }
        }
    }
    Ok(KickTarget {
        owner_key: key.to_string(),
        participant: claimant,
        hosts: false,
    })
}

/// The namespace threshold of the cached party, or `None` when the party is
/// not cached.
pub(super) async fn cached_namespace_threshold(
    db: &SqlitePool,
    party: &CantonId,
) -> Result<Option<usize>> {
    let party_id = party.to_string();
    let Some(row) = db
        .get_dec_parties_by_prefix(&party.prefix)
        .await?
        .into_iter()
        .find(|row| row.party_id == party_id)
    else {
        return Ok(None);
    };
    Ok(Some(usize::try_from(row.threshold)?))
}

/// The confirmation threshold a kick or add-party carries over, worked out
/// from the cached party. A party missing from the cache counts as one whose
/// thresholds match; the coordinator checks the chain before it signs.
pub(super) async fn cached_confirmation_threshold(
    db: &SqlitePool,
    party: &CantonId,
    host_count: usize,
) -> Result<Option<i32>> {
    let party_id = party.to_string();
    let Some(row) = db
        .get_dec_parties_by_prefix(&party.prefix)
        .await?
        .into_iter()
        .find(|row| row.party_id == party_id)
    else {
        return Ok(None);
    };
    let current = PartyThresholds::new(
        i32::try_from(row.threshold)?,
        row.confirmation_threshold.map(i32::try_from).transpose()?,
    );
    Ok(carried_confirmation_threshold(current, host_count))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::pid;
    use super::*;
    use crate::db::MIGRATOR;
    use crate::db::schema::{Commitable, SchemaWrite};

    /// The coordinator signs the proposals, so it is a member of the party it
    /// is changing — but it is absent from its own `peers` table. A member set
    /// derived from that list must still count it, or every threshold bound
    /// computed from it is one too low.
    #[test]
    fn party_member_count_counts_this_node_when_the_set_omits_it() -> anyhow::Result<()> {
        let me = pid(1)?;
        let others: HashSet<CantonId> = [pid(2)?, pid(3)?, pid(4)?].into_iter().collect();

        // The shape that caused the bug: three other members cached, self absent.
        assert_eq!(party_member_count(&others, &me), 4);
        Ok(())
    }

    /// When the cache does contain this node — the normal case, since the
    /// refresh writes the chain's participant list verbatim — it must not be
    /// counted twice.
    #[test]
    fn party_member_count_does_not_double_count_a_present_self() -> anyhow::Result<()> {
        let me = pid(1)?;
        let all: HashSet<CantonId> = [me.clone(), pid(2)?, pid(3)?, pid(4)?]
            .into_iter()
            .collect();

        assert_eq!(party_member_count(&all, &me), 4);
        Ok(())
    }

    /// A one-member party is this node alone, whichever way the set was built.
    #[test]
    fn party_member_count_handles_a_lone_member() -> anyhow::Result<()> {
        let me = pid(1)?;
        assert_eq!(party_member_count(&HashSet::new(), &me), 1);
        assert_eq!(
            party_member_count(&[me.clone()].into_iter().collect(), &me),
            1
        );
        Ok(())
    }

    fn host_row(uid: &CantonId, owner_key: Option<&str>) -> DecPartyParticipantRow {
        DecPartyParticipantRow {
            dec_party_id: "party".to_string(),
            participant_uid: uid.to_string(),
            permission: "confirmation".to_string(),
            owner_key: owner_key.map(str::to_string),
            signing_key: None,
        }
    }

    fn owner_row(owner_key: &str, claimant: Option<&CantonId>) -> DecPartyOwnerRow {
        DecPartyOwnerRow {
            dec_party_id: "party".to_string(),
            owner_key: owner_key.to_string(),
            participant_uid: claimant.map(ToString::to_string),
        }
    }

    /// Hosts p1 and p2 hold keys "k1" and "k2"; "k3" is owned by p3, which
    /// hosts nothing, and "k4" by nobody that has said so.
    fn owner_only_party() -> anyhow::Result<(Vec<DecPartyParticipantRow>, Vec<DecPartyOwnerRow>)> {
        let (p1, p2, p3) = (pid(1)?, pid(2)?, pid(3)?);
        Ok((
            vec![host_row(&p1, Some("k1")), host_row(&p2, Some("k2"))],
            vec![
                owner_row("k1", Some(&p1)),
                owner_row("k2", None),
                owner_row("k3", Some(&p3)),
                owner_row("k4", None),
            ],
        ))
    }

    #[test]
    fn a_host_is_named_by_its_participant() -> anyhow::Result<()> {
        let (hosts, owners) = owner_only_party()?;
        let target = resolve_kick_target(Some(&pid(2)?), None, &hosts, &owners);
        assert_eq!(
            target,
            Ok(KickTarget {
                owner_key: "k2".to_string(),
                participant: Some(pid(2)?),
                hosts: true,
            })
        );
        Ok(())
    }

    /// An owner key a host holds kicks that host, whoever is named for it.
    #[test]
    fn a_host_key_kicks_its_host() -> anyhow::Result<()> {
        let (hosts, owners) = owner_only_party()?;
        let target = resolve_kick_target(None, Some("k2"), &hosts, &owners);
        assert_eq!(
            target,
            Ok(KickTarget {
                owner_key: "k2".to_string(),
                participant: Some(pid(2)?),
                hosts: true,
            })
        );
        let mismatch = resolve_kick_target(Some(&pid(1)?), Some("k2"), &hosts, &owners);
        assert!(
            matches!(&mismatch, Err(KickTargetError::BadRequest(e)) if e.contains("belongs to")),
            "unexpected result: {mismatch:?}"
        );
        Ok(())
    }

    #[test]
    fn an_owner_that_hosts_nothing_is_named_by_its_claimant_or_its_key() -> anyhow::Result<()> {
        let (hosts, owners) = owner_only_party()?;
        let expected = KickTarget {
            owner_key: "k3".to_string(),
            participant: Some(pid(3)?),
            hosts: false,
        };
        assert_eq!(
            resolve_kick_target(Some(&pid(3)?), None, &hosts, &owners).as_ref(),
            Ok(&expected)
        );
        assert_eq!(
            resolve_kick_target(None, Some("k3"), &hosts, &owners),
            Ok(expected)
        );
        assert_eq!(
            resolve_kick_target(None, Some("k4"), &hosts, &owners),
            Ok(KickTarget {
                owner_key: "k4".to_string(),
                participant: None,
                hosts: false,
            })
        );
        Ok(())
    }

    /// Until every host's key is known, an owner key may be the key of a
    /// host, and kicking it would strip that host of its ownership. A claim
    /// does not settle it, because any peer can claim any key.
    #[test]
    fn an_owner_only_kick_waits_for_every_host_key() -> anyhow::Result<()> {
        let (mut hosts, owners) = owner_only_party()?;
        hosts[1].owner_key = None;
        for (participant, key) in [
            (None, Some("k4")),
            (None, Some("k3")),
            (Some(pid(3)?), None),
        ] {
            let target = resolve_kick_target(participant.as_ref(), key, &hosts, &owners);
            assert!(
                matches!(&target, Err(KickTargetError::Conflict(e)) if e.contains("not been resolved")),
                "{participant:?} / {key:?}: unexpected result: {target:?}"
            );
        }
        Ok(())
    }

    /// Host 2 rotated its owner key to k4, and the cache still holds k2. The
    /// unclaimed k4 may be host 2's new key, so neither k4 nor host 2 can be
    /// kicked until a refresh resolves it.
    #[test]
    fn a_host_key_that_is_no_longer_an_owner_counts_as_unresolved() -> anyhow::Result<()> {
        let (hosts, mut owners) = owner_only_party()?;
        owners.retain(|owner| owner.owner_key != "k2");
        for (participant, key) in [(None, Some("k4")), (Some(pid(2)?), None)] {
            let target = resolve_kick_target(participant.as_ref(), key, &hosts, &owners);
            assert!(
                matches!(&target, Err(KickTargetError::Conflict(e)) if e.contains("not been resolved")),
                "{participant:?} / {key:?}: unexpected result: {target:?}"
            );
        }
        Ok(())
    }

    /// A participant label must come from a claim, or the run card would name
    /// a participant nobody tied to the key.
    #[test]
    fn a_participant_must_match_the_claim_on_the_key() -> anyhow::Result<()> {
        let (hosts, owners) = owner_only_party()?;
        for (participant, key, expected) in [
            (pid(4)?, "k3", "was claimed by"),
            (pid(4)?, "k4", "No peer has claimed"),
            (pid(1)?, "k4", "under another owner key"),
        ] {
            let target = resolve_kick_target(Some(&participant), Some(key), &hosts, &owners);
            assert!(
                matches!(&target, Err(KickTargetError::BadRequest(e)) if e.contains(expected)),
                "{participant} / {key}: unexpected result: {target:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn a_kick_must_name_a_member() -> anyhow::Result<()> {
        let (hosts, owners) = owner_only_party()?;
        for (participant, key) in [(None, None), (Some(pid(5)?), None), (None, Some("k9"))] {
            let target = resolve_kick_target(participant.as_ref(), key, &hosts, &owners);
            assert!(
                matches!(target, Err(KickTargetError::BadRequest(_))),
                "unexpected result: {target:?}"
            );
        }
        Ok(())
    }

    /// Three hosts and four owners. A host's kick leaves two keys to sign
    /// with; an owner-only kick leaves all three.
    #[test]
    fn the_kick_bound_counts_the_hosts_and_the_owners_left() {
        assert_eq!(kick_threshold_bound(true, 3, 4), 2);
        assert_eq!(kick_threshold_bound(false, 3, 4), 3);
        // Two owners hosting nothing: the hosts left bound it below the owners.
        assert_eq!(kick_threshold_bound(false, 2, 4), 2);
        // An uncached owner set bounds by the hosts, as before.
        assert_eq!(kick_threshold_bound(true, 4, 0), 3);
    }

    /// The bug in the numbers that produced it: a 4-member party at threshold
    /// 3 could not be kicked, because the bound said 2 while the peers demanded
    /// 3. With self counted the bound is 3 and the kick is expressible.
    #[test]
    fn kick_bound_admits_the_threshold_the_peers_require() -> anyhow::Result<()> {
        let me = pid(1)?;
        let cached_without_self: HashSet<CantonId> =
            [pid(2)?, pid(3)?, pid(4)?].into_iter().collect();

        let bound = party_member_count(&cached_without_self, &me) as i32 - 1;
        assert_eq!(bound, 3, "a 4-member party leaves 3 after a kick");
        assert!(bound >= 3, "threshold 3 must be expressible");
        Ok(())
    }

    async fn cache_party(
        pool: &SqlitePool,
        party: &CantonId,
        threshold: i64,
        confirmation_threshold: Option<i64>,
    ) -> Result {
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&crate::db::rows::DecPartyRow {
            party_id: party.to_string(),
            prefix: party.prefix.clone(),
            threshold,
            updated_at: 0,
            my_owner_key: None,
            confirmation_threshold,
        })
        .await?;
        Commitable::commit(tx).await
    }

    /// Kick and add-party read the party's two thresholds from the cache.
    /// Matching ones stay matched, so the run asks for no confirmation
    /// threshold of its own and the invite is what an older peer expects.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_cached_party_with_matching_thresholds_carries_none(pool: SqlitePool) -> Result {
        let party = pid(9)?;
        cache_party(&pool, &party, 2, Some(2)).await?;
        assert_eq!(cached_confirmation_threshold(&pool, &party, 4).await?, None);

        // A row cached before the column existed reads the same way.
        cache_party(&pool, &party, 2, None).await?;
        assert_eq!(cached_confirmation_threshold(&pool, &party, 4).await?, None);

        // So does a party that is not cached at all.
        assert_eq!(
            cached_confirmation_threshold(&pool, &pid(10)?, 4).await?,
            None
        );
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_cached_party_with_distinct_thresholds_keeps_its_confirmation_threshold(
        pool: SqlitePool,
    ) -> Result {
        let party = pid(11)?;
        cache_party(&pool, &party, 1, Some(3)).await?;
        // Add-party grows the host set, so the threshold stays as it is.
        assert_eq!(
            cached_confirmation_threshold(&pool, &party, 4).await?,
            Some(3)
        );
        // A kick down to two hosts caps it at two.
        assert_eq!(
            cached_confirmation_threshold(&pool, &party, 2).await?,
            Some(2)
        );
        Ok(())
    }
}
