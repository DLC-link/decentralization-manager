use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    db::{
        rows::DecPartyTopologyVersion,
        schema::{Commitable, SchemaWrite},
    },
    error::Result,
    workflow::thresholds::PartyThresholds,
};

/// Drop the kicked member from this node's cached membership: its owner key,
/// its participant row when it hosted the party, and the confirmed thresholds.
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
    confirmed_version: DecPartyTopologyVersion,
    confirmed_thresholds: PartyThresholds,
) -> Result {
    let mut tx = db.begin_transaction().await?;
    let updated = tx
        .update_dec_party_thresholds_after_kick(
            party_id,
            confirmed_version,
            i64::from(confirmed_thresholds.namespace),
            (confirmed_thresholds.confirmation != confirmed_thresholds.namespace)
                .then_some(i64::from(confirmed_thresholds.confirmation)),
        )
        .await?;
    if !updated {
        tracing::info!("Leaving a newer cached topology snapshot of {party_id} unchanged");
        return Commitable::commit(tx).await;
    }
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
    use crate::db::{
        MIGRATOR,
        rows::{DecPartyParticipantRow, DecPartyRow},
        schema::SchemaRead,
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

    fn completed_version() -> DecPartyTopologyVersion {
        DecPartyTopologyVersion {
            namespace_serial: 2,
            p2p_serial: 2,
        }
    }

    async fn record_version(
        pool: &SqlitePool,
        party: &CantonId,
        namespace_serial: i64,
        p2p_serial: i64,
    ) -> Result {
        let mut tx = pool.begin_transaction().await?;
        assert!(
            tx.advance_dec_party_topology_version(
                party,
                DecPartyTopologyVersion {
                    namespace_serial,
                    p2p_serial
                },
            )
            .await?
        );
        Commitable::commit(tx).await
    }

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

        prune_cached_membership(
            &pool,
            &party_id,
            Some(&kicked),
            "owner-2",
            completed_version(),
            PartyThresholds::new(2, None),
        )
        .await?;

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

        prune_cached_membership(
            &pool,
            &party_id,
            None,
            "owner-3",
            completed_version(),
            PartyThresholds::new(2, None),
        )
        .await?;

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

        prune_cached_membership(
            &pool,
            &party_id,
            Some(&never_a_member),
            "owner-9",
            completed_version(),
            PartyThresholds::new(2, None),
        )
        .await?;

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

        prune_cached_membership(
            &pool,
            &party_id,
            Some(&CantonId::parse(NODE2)?),
            "owner-2",
            completed_version(),
            PartyThresholds::new(2, None),
        )
        .await?;

        let remaining = pool.get_dec_party_participants(&party_id).await?;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].permission, "confirmation");
        assert_eq!(remaining[0].owner_key.as_deref(), Some("fingerprint-1"));
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_completed_kick_updates_both_cached_thresholds(pool: SqlitePool) -> Result {
        let party = seed(&pool, &[NODE1, NODE2, NODE9]).await?;
        sqlx::query("UPDATE dec_party SET threshold = 3 WHERE party_id = ?")
            .bind(party.to_string())
            .execute(&pool)
            .await?;

        prune_cached_membership(
            &pool,
            &party,
            Some(&CantonId::parse(NODE2)?),
            "owner-2",
            completed_version(),
            PartyThresholds::new(2, Some(1)),
        )
        .await?;

        let cached = pool.get_dec_parties_by_prefix(&party.prefix).await?;
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].threshold, 2);
        assert_eq!(cached[0].confirmation_threshold, Some(1));
        assert_eq!(cached[0].updated_at, 1000);
        assert_eq!(pool.get_dec_party_participants(&party).await?.len(), 2);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn pruning_cannot_overwrite_a_newer_refreshed_threshold(pool: SqlitePool) -> Result {
        let party = seed(&pool, &[NODE1, NODE2]).await?;
        sqlx::query(
            "UPDATE dec_party SET threshold = 4, confirmation_threshold = 3, updated_at = 2000 \
             WHERE party_id = ?",
        )
        .bind(party.to_string())
        .execute(&pool)
        .await?;
        record_version(&pool, &party, 3, 3).await?;

        prune_cached_membership(
            &pool,
            &party,
            Some(&CantonId::parse(NODE2)?),
            "owner-2",
            completed_version(),
            PartyThresholds::new(1, None),
        )
        .await?;

        let cached = pool.get_dec_parties_by_prefix(&party.prefix).await?;
        assert_eq!(cached[0].threshold, 4);
        assert_eq!(cached[0].confirmation_threshold, Some(3));
        assert_eq!(cached[0].updated_at, 2000);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn retrying_pruning_cannot_rewind_a_later_refresh(pool: SqlitePool) -> Result {
        let party = seed(&pool, &[NODE1, NODE2]).await?;
        let kicked = CantonId::parse(NODE2)?;
        prune_cached_membership(
            &pool,
            &party,
            Some(&kicked),
            "owner-2",
            completed_version(),
            PartyThresholds::new(1, None),
        )
        .await?;
        // A later topology update returned to the old threshold without
        // re-adding the kicked member. A retry still must not rewind it.
        sqlx::query("UPDATE dec_party SET threshold = 2 WHERE party_id = ?")
            .bind(party.to_string())
            .execute(&pool)
            .await?;
        record_version(&pool, &party, 3, 3).await?;

        prune_cached_membership(
            &pool,
            &party,
            Some(&kicked),
            "owner-2",
            completed_version(),
            PartyThresholds::new(1, None),
        )
        .await?;

        let cached = pool.get_dec_parties_by_prefix(&party.prefix).await?;
        assert_eq!(cached[0].threshold, 2);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_prune_rolls_back_threshold_and_membership_changes(
        pool: SqlitePool,
    ) -> Result {
        let party = seed(&pool, &[NODE1, NODE2]).await?;
        sqlx::query(
            "CREATE TRIGGER refuse_owner_removal BEFORE DELETE ON dec_party_owner \
             BEGIN SELECT RAISE(ABORT, 'owner removal failed'); END",
        )
        .execute(&pool)
        .await?;

        let result = prune_cached_membership(
            &pool,
            &party,
            Some(&CantonId::parse(NODE2)?),
            "owner-2",
            completed_version(),
            PartyThresholds::new(1, None),
        )
        .await;

        assert!(result.is_err());
        let cached = pool.get_dec_parties_by_prefix(&party.prefix).await?;
        assert_eq!(cached[0].threshold, 2);
        assert_eq!(pool.get_dec_party_participants(&party).await?.len(), 2);
        assert_eq!(pool.get_dec_party_owners(&party).await?.len(), 3);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn completion_repairs_a_host_kick_snapshot_between_dns_and_p2p(
        pool: SqlitePool,
    ) -> Result {
        let party = seed(&pool, &[NODE1, NODE2, NODE9]).await?;
        sqlx::query(
            "UPDATE dec_party SET threshold = 2, confirmation_threshold = 3 WHERE party_id = ?",
        )
        .bind(party.to_string())
        .execute(&pool)
        .await?;
        sqlx::query("DELETE FROM dec_party_owner WHERE dec_party_id = ? AND owner_key = 'owner-2'")
            .bind(party.to_string())
            .execute(&pool)
            .await?;
        record_version(&pool, &party, 2, 1).await?;

        prune_cached_membership(
            &pool,
            &party,
            Some(&CantonId::parse(NODE2)?),
            "owner-2",
            completed_version(),
            PartyThresholds::new(2, None),
        )
        .await?;

        let cached = pool.get_dec_parties_by_prefix(&party.prefix).await?;
        assert_eq!(cached[0].threshold, 2);
        assert_eq!(cached[0].confirmation_threshold, None);
        assert_eq!(pool.get_dec_party_participants(&party).await?.len(), 2);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn completion_repairs_an_owner_only_snapshot_without_a_removed_host(
        pool: SqlitePool,
    ) -> Result {
        let party = seed(&pool, &[NODE1, NODE2, NODE9]).await?;
        sqlx::query(
            "UPDATE dec_party SET threshold = 2, confirmation_threshold = 3 WHERE party_id = ?",
        )
        .bind(party.to_string())
        .execute(&pool)
        .await?;
        sqlx::query("DELETE FROM dec_party_owner WHERE dec_party_id = ? AND owner_key = 'owner-4'")
            .bind(party.to_string())
            .execute(&pool)
            .await?;
        record_version(&pool, &party, 2, 1).await?;

        prune_cached_membership(
            &pool,
            &party,
            None,
            "owner-4",
            completed_version(),
            PartyThresholds::new(2, None),
        )
        .await?;

        let cached = pool.get_dec_parties_by_prefix(&party.prefix).await?;
        assert_eq!(cached[0].threshold, 2);
        assert_eq!(cached[0].confirmation_threshold, None);
        assert_eq!(pool.get_dec_party_participants(&party).await?.len(), 3);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_newer_confirmation_change_with_the_same_namespace_wins(pool: SqlitePool) -> Result {
        let party = seed(&pool, &[NODE1, NODE2, NODE9]).await?;
        sqlx::query(
            "UPDATE dec_party SET threshold = 2, confirmation_threshold = 3 WHERE party_id = ?",
        )
        .bind(party.to_string())
        .execute(&pool)
        .await?;
        record_version(&pool, &party, 2, 3).await?;

        prune_cached_membership(
            &pool,
            &party,
            None,
            "owner-4",
            completed_version(),
            PartyThresholds::new(2, None),
        )
        .await?;

        let cached = pool.get_dec_parties_by_prefix(&party.prefix).await?;
        assert_eq!(cached[0].threshold, 2);
        assert_eq!(cached[0].confirmation_threshold, Some(3));
        // A later topology snapshot also protects membership from old retries.
        assert_eq!(pool.get_dec_party_owners(&party).await?.len(), 4);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn upgrading_an_existing_cache_keeps_attribution_and_initializes_versions(
        pool: SqlitePool,
    ) -> Result {
        // Recreate the upgrade path with real cached rows predating metadata.
        sqlx::query("DROP TABLE dec_party_topology_version")
            .execute(&pool)
            .await?;
        let party = seed(&pool, &[NODE1, NODE2]).await?;
        sqlx::query("UPDATE dec_party_participant SET owner_key = 'owner-1', signing_key = 'known-key' WHERE participant_uid = ?")
            .bind(NODE1).execute(&pool).await?;
        sqlx::raw_sql(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/000030_dec_party_topology_version.up.sql"
        )))
        .execute(&pool)
        .await?;
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dec_party_topology_version")
            .fetch_one(&pool)
            .await?;
        assert_eq!(before, 0);

        prune_cached_membership(
            &pool,
            &party,
            Some(&CantonId::parse(NODE2)?),
            "owner-2",
            completed_version(),
            PartyThresholds::new(1, None),
        )
        .await?;

        let cached = pool.get_dec_parties_by_prefix(&party.prefix).await?;
        assert_eq!(cached[0].threshold, 1);
        let surviving = pool.get_dec_party_participants(&party).await?;
        assert_eq!(surviving.len(), 1);
        assert_eq!(surviving[0].owner_key.as_deref(), Some("owner-1"));
        assert_eq!(surviving[0].signing_key.as_deref(), Some("known-key"));
        let version: DecPartyTopologyVersion = sqlx::query_as(
            "SELECT namespace_serial, p2p_serial FROM dec_party_topology_version WHERE party_id = ?",
        ).bind(party.to_string()).fetch_one(&pool).await?;
        assert_eq!(version, completed_version());
        Ok(())
    }
}
