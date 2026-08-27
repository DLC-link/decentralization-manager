use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
};

/// Drop the kicked participant from this node's cached membership.
///
/// `POST /add-party` and `POST /kick` validate against `dec_party_participants`,
/// which in production is only ever written by the `/decentralized-parties`
/// refresh. A completed kick therefore leaves the coordinator still reporting
/// the removed participant as a member until an unrelated refresh happens to
/// run: an immediate re-add is rejected with "already a member", and the
/// post-add threshold bound is computed one member too high.
pub async fn prune_cached_membership(
    db: &SqlitePool,
    party_id: &CantonId,
    kicked: &CantonId,
) -> Result {
    let kicked_uid = kicked.to_string();
    let cached = db.get_dec_party_participants(party_id).await?;
    if !cached.iter().any(|row| row.participant_uid == kicked_uid) {
        return Ok(());
    }

    let remaining: Vec<_> = cached
        .into_iter()
        .filter(|row| row.participant_uid != kicked_uid)
        .collect();

    let mut tx = db.begin_transaction().await?;
    tx.replace_dec_party_participants(party_id, &remaining)
        .await?;
    Commitable::commit(tx).await?;

    tracing::info!("Removed kicked participant {kicked} from cached membership of {party_id}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::db::{
        MIGRATOR,
        rows::{DecPartyParticipantRow, DecPartyRow},
    };

    use super::*;

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
                permission: "confirmation".to_string(),
                owner_key: None,
            })
            .collect();

        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&DecPartyRow {
            party_id: party_id_str,
            prefix: "net-a".to_string(),
            threshold: 2,
            updated_at: 1000,
            my_owner_key: None,
        })
        .await?;
        tx.replace_dec_party_participants(&party_id, &participants)
            .await?;
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

        prune_cached_membership(&pool, &party_id, &kicked).await?;

        let remaining = pool.get_dec_party_participants(&party_id).await?;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].participant_uid, NODE1);
        Ok(())
    }

    /// Kicks are retried and the coordinator can be restarted mid-workflow, so
    /// pruning a participant that is already gone must be a no-op rather than
    /// clearing the rest of the membership.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn pruning_an_absent_participant_leaves_the_cache_alone(pool: SqlitePool) -> Result {
        let party_id = seed(&pool, &[NODE1, NODE2]).await?;
        let never_a_member = CantonId::parse(NODE9)?;

        prune_cached_membership(&pool, &party_id, &never_a_member).await?;

        let remaining = pool.get_dec_party_participants(&party_id).await?;
        assert_eq!(remaining.len(), 2);
        Ok(())
    }
}
