use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    db::schema::{Commitable, SchemaWrite},
    error::Result,
};

/// Drop the kicked member from this node's cached membership: its owner key,
/// and its participant row when it hosted the party.
///
/// `POST /add-party` and `POST /kick` validate against `dec_party_participants`,
/// which in production is only ever written by the `/decentralized-parties`
/// refresh. A completed kick therefore leaves the coordinator still reporting
/// the removed participant as a member until an unrelated refresh happens to
/// run: an immediate re-add is rejected with "already a member", and the
/// post-add threshold bound is computed one member too high.
///
/// Deletes the single row rather than rewriting the whole participant set. The
/// read-modify-write shape would revert a `permission` update landed by a
/// concurrent refresh, and there is nothing here that needs the other rows.
pub async fn prune_cached_membership(
    db: &SqlitePool,
    party_id: &CantonId,
    kicked: Option<&CantonId>,
    owner_key: &str,
) -> Result {
    let mut tx = db.begin_transaction().await?;
    if let Some(kicked) = kicked {
        tx.delete_dec_party_participant(party_id, &kicked.to_string())
            .await?;
    }
    tx.delete_dec_party_owner(party_id, owner_key).await?;
    Commitable::commit(tx).await?;

    tracing::info!(
        "Removed kicked member {kicked:?} (owner key {owner_key}) from cached membership of \
         {party_id}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        MIGRATOR,
        rows::{DecPartyParticipantRow, DecPartyRow},
        schema::SchemaRead,
    };

    const TEST_NS: &str = "1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    /// Participant uids round-trip through `CantonId::parse`, which requires a
    /// `prefix::<68-hex-char namespace>` shape.
    const NODE1: &str =
        "node1::12201111111111111111111111111111111111111111111111111111111111111111";
    const NODE2: &str =
        "node2::12202222222222222222222222222222222222222222222222222222222222222222";
    const NODE9: &str =
        "node9::12209999999999999999999999999999999999999999999999999999999999999999";

    async fn seed(pool: &SqlitePool, uids: &[&str]) -> Result<CantonId> {
        let party_id_str = format!("net-a::{TEST_NS}");
        let party_id = CantonId::parse(&party_id_str)?;
        let participants: Vec<_> = uids
            .iter()
            .map(|uid| DecPartyParticipantRow {
                dec_party_id: party_id_str.clone(),
                participant_uid: (*uid).to_string(),
                permission: "submission".to_string(),
                owner_key: None,
                signing_key: None,
            })
            .collect();

        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&DecPartyRow {
            party_id: party_id_str,
            prefix: "net-a".to_string(),
            threshold: 2,
            updated_at: 1000,
            my_owner_key: None,
            confirmation_threshold: None,
        })
        .await?;
        tx.replace_dec_party_participants(&party_id, &participants)
            .await?;
        let owners: Vec<String> = (1..=uids.len() + 1).map(|n| format!("owner-{n}")).collect();
        tx.replace_dec_party_owners(&party_id, &owners).await?;
        Commitable::commit(tx).await?;
        Ok(party_id)
    }

    /// The bug this exists for: a completed kick left the removed participant
    /// in the cache, so `POST /add-party` rejected an immediate re-add with
    /// "already a member".
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn kicked_participant_is_dropped_from_the_cache(pool: SqlitePool) -> Result {
        let party_id = seed(&pool, &[NODE1, NODE2]).await?;
        let kicked = CantonId::parse(NODE2)?;

        prune_cached_membership(&pool, &party_id, Some(&kicked), "owner-2").await?;

        let remaining = pool.get_dec_party_participants(&party_id).await?;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].participant_uid, NODE1);
        Ok(())
    }

    /// A member that owns the namespace without hosting the party leaves only
    /// the owner set, so every host row stays.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_owner_only_kick_drops_only_the_owner(pool: SqlitePool) -> Result {
        let party_id = seed(&pool, &[NODE1, NODE2]).await?;

        prune_cached_membership(&pool, &party_id, None, "owner-3").await?;

        assert_eq!(pool.get_dec_party_participants(&party_id).await?.len(), 2);
        let mut owners: Vec<String> = pool
            .get_dec_party_owners(&party_id)
            .await?
            .into_iter()
            .map(|owner| owner.owner_key)
            .collect();
        owners.sort();
        assert_eq!(owners, vec!["owner-1", "owner-2"]);
        Ok(())
    }

    /// Kicks are retried and the coordinator can be restarted mid-workflow, so
    /// pruning a participant that is already gone must be a no-op rather than
    /// clearing the rest of the membership.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn pruning_an_absent_participant_leaves_the_cache_alone(pool: SqlitePool) -> Result {
        let party_id = seed(&pool, &[NODE1, NODE2]).await?;
        let never_a_member = CantonId::parse(NODE9)?;

        prune_cached_membership(&pool, &party_id, Some(&never_a_member), "owner-9").await?;

        let remaining = pool.get_dec_party_participants(&party_id).await?;
        assert_eq!(remaining.len(), 2);
        Ok(())
    }

    /// Pruning must not rewrite the surviving rows. Rebuilding the whole
    /// participant set would push a stale `permission` back over one a
    /// concurrent `/decentralized-parties` refresh had just updated.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn pruning_leaves_the_surviving_rows_untouched(pool: SqlitePool) -> Result {
        let party_id = seed(&pool, &[NODE1, NODE2]).await?;

        // Stand in for a concurrent refresh promoting NODE1's permission.
        let mut tx = pool.begin_transaction().await?;
        tx.replace_dec_party_participants(
            &party_id,
            &[DecPartyParticipantRow {
                dec_party_id: party_id.to_string(),
                participant_uid: NODE1.to_string(),
                permission: "confirmation".to_string(),
                owner_key: Some("fingerprint-1".to_string()),
                signing_key: None,
            }],
        )
        .await?;
        Commitable::commit(tx).await?;

        prune_cached_membership(&pool, &party_id, Some(&CantonId::parse(NODE2)?), "owner-2")
            .await?;

        let remaining = pool.get_dec_party_participants(&party_id).await?;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].permission, "confirmation");
        assert_eq!(remaining[0].owner_key.as_deref(), Some("fingerprint-1"));
        Ok(())
    }
}
