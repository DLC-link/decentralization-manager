//! The governance audit and the chain audit cache.

use sqlx::SqlitePool;

use super::fixtures::TEST_NS;
use crate::{
    canton_id::CantonId,
    db::{
        rows::ChainAuditCacheRow,
        schema::SchemaRead,
        sqlite::{connect::MIGRATOR, read::RECENT_GOVERNANCE_AUDIT_SQL},
    },
    error::Result,
};

// ====================================================================
// Governance Audit
// ====================================================================

async fn insert_audit_entry(
    pool: &SqlitePool,
    party_id: &str,
    event_type: &str,
    status: &str,
    created_at: i64,
) -> Result {
    sqlx::query(
        r"
            INSERT INTO governance_audit (
                timestamp, event_type, party_id, member_party_id,
                governance_type, action_summary, details, status,
                error_message, created_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ",
    )
    .bind(created_at)
    .bind(event_type)
    .bind(party_id)
    .bind("member::1220aa")
    .bind("core_self")
    .bind("governance_add_member")
    .bind(r#"{"type":"governance_add_member"}"#)
    .bind(status)
    .bind(None::<String>)
    .bind(created_at)
    .execute(pool)
    .await?;

    Ok(())
}

pub(super) fn test_party_id(prefix: &str) -> CantonId {
    CantonId::parse(&format!("{prefix}::{TEST_NS}")).unwrap()
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_governance_audit_insert_and_query(pool: SqlitePool) -> Result {
    let party_id = test_party_id("party-a");
    let party_id_str = party_id.to_string();

    // Empty initially
    let entries = pool.get_governance_audit(&party_id, 50, 0).await?;
    assert!(entries.is_empty());

    // Insert entries
    insert_audit_entry(&pool, &party_id_str, "propose", "success", 1000).await?;
    insert_audit_entry(&pool, &party_id_str, "confirm", "success", 2000).await?;
    insert_audit_entry(&pool, &party_id_str, "execute", "failed", 3000).await?;

    // Query all
    let entries = pool.get_governance_audit(&party_id, 50, 0).await?;
    assert_eq!(entries.len(), 3);
    // Newest first
    assert_eq!(entries[0].event_type, "execute");
    assert_eq!(entries[1].event_type, "confirm");
    assert_eq!(entries[2].event_type, "propose");

    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_governance_audit_pagination(pool: SqlitePool) -> Result {
    let party_id = test_party_id("party-a");
    let party_id_str = party_id.to_string();

    for i in 0..5 {
        insert_audit_entry(&pool, &party_id_str, "confirm", "success", 1000 + i).await?;
    }

    // Limit
    let entries = pool.get_governance_audit(&party_id, 2, 0).await?;
    assert_eq!(entries.len(), 2);

    // Offset
    let entries = pool.get_governance_audit(&party_id, 2, 3).await?;
    assert_eq!(entries.len(), 2);

    // Beyond end
    let entries = pool.get_governance_audit(&party_id, 50, 5).await?;
    assert!(entries.is_empty());

    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_recent_governance_audit_spans_parties_newest_first(pool: SqlitePool) -> Result {
    let party_a = test_party_id("party-a").to_string();
    let party_b = test_party_id("party-b").to_string();
    let party_c = test_party_id("party-c").to_string();

    assert!(pool.get_recent_governance_audit(50, 0).await?.is_empty());

    // Inserted out of time order, so the result order comes from the query.
    insert_audit_entry(&pool, &party_b, "confirm", "success", 2000).await?;
    insert_audit_entry(&pool, &party_a, "propose", "success", 1000).await?;
    insert_audit_entry(&pool, &party_c, "execute", "failed", 3000).await?;
    insert_audit_entry(&pool, &party_a, "execute", "success", 4000).await?;

    let entries = pool.get_recent_governance_audit(50, 0).await?;
    let seen: Vec<(&str, &str, i64)> = entries
        .iter()
        .map(|e| (e.party_id.as_str(), e.event_type.as_str(), e.created_at))
        .collect();
    assert_eq!(
        seen,
        vec![
            (party_a.as_str(), "execute", 4000),
            (party_c.as_str(), "execute", 3000),
            (party_b.as_str(), "confirm", 2000),
            (party_a.as_str(), "propose", 1000),
        ]
    );

    Ok(())
}

/// Rows logged in the same second must still come back in a fixed order,
/// or a page boundary could repeat one row and skip another.
#[sqlx::test(migrator = "MIGRATOR")]
async fn test_recent_governance_audit_breaks_ties_by_insertion(pool: SqlitePool) -> Result {
    let party_a = test_party_id("party-a").to_string();
    let party_b = test_party_id("party-b").to_string();

    insert_audit_entry(&pool, &party_a, "propose", "success", 1000).await?;
    insert_audit_entry(&pool, &party_b, "confirm", "success", 1000).await?;
    insert_audit_entry(&pool, &party_a, "execute", "success", 1000).await?;

    let entries = pool.get_recent_governance_audit(50, 0).await?;
    let events: Vec<&str> = entries.iter().map(|e| e.event_type.as_str()).collect();
    assert_eq!(events, vec!["execute", "confirm", "propose"]);
    assert!(entries.windows(2).all(|w| w[0].id > w[1].id));

    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_recent_governance_audit_pagination(pool: SqlitePool) -> Result {
    for i in 0..5 {
        let party = test_party_id(&format!("party-{i}")).to_string();
        insert_audit_entry(&pool, &party, "confirm", "success", 1000 + i).await?;
    }

    let first = pool.get_recent_governance_audit(2, 0).await?;
    let created: Vec<i64> = first.iter().map(|e| e.created_at).collect();
    assert_eq!(created, vec![1004, 1003]);

    let middle = pool.get_recent_governance_audit(2, 2).await?;
    let created: Vec<i64> = middle.iter().map(|e| e.created_at).collect();
    assert_eq!(created, vec![1002, 1001]);

    let last = pool.get_recent_governance_audit(2, 4).await?;
    let created: Vec<i64> = last.iter().map(|e| e.created_at).collect();
    assert_eq!(created, vec![1000]);

    assert!(pool.get_recent_governance_audit(2, 5).await?.is_empty());

    Ok(())
}

/// The cross-party read runs on every feed poll, so it must walk the
/// `created_at` index rather than sort the whole table.
#[sqlx::test(migrator = "MIGRATOR")]
async fn test_recent_governance_audit_reads_the_created_index(pool: SqlitePool) -> Result {
    let rows = sqlx::query_as::<_, (i64, i64, i64, String)>(&format!(
        "EXPLAIN QUERY PLAN {RECENT_GOVERNANCE_AUDIT_SQL}"
    ))
    .bind(50_i64)
    .bind(0_i64)
    .fetch_all(&pool)
    .await?;
    let plan: Vec<String> = rows.into_iter().map(|(_, _, _, detail)| detail).collect();
    let plan = plan.join("\n");

    assert!(plan.contains("idx_governance_audit_created"), "{plan}");
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");

    Ok(())
}

// ====================================================================
// Chain audit cache
// ====================================================================

/// One cached chain-audit row. `contract_id` is part of the primary key, so
/// it is what lets several rows share an offset — the case the paging query
/// has to keep together.
async fn insert_chain_audit_row(
    pool: &SqlitePool,
    party_id: &str,
    offset: i64,
    contract_id: &str,
) -> Result {
    sqlx::query(
        r"
            INSERT INTO chain_audit_cache (
                party_id, offset, timestamp, event_type, contract_id,
                template_id, package_id, governance_type, action_summary,
                choice, acting_parties, update_id, details
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ",
    )
    .bind(party_id)
    .bind(offset)
    .bind(offset)
    .bind("propose")
    .bind(contract_id)
    .bind("Governance:Action")
    .bind("pkg-1")
    .bind("core_domain")
    .bind("propose_add_member")
    .bind(None::<String>)
    .bind("[]")
    .bind(format!("update-{offset}"))
    .bind("null")
    .execute(pool)
    .await?;

    Ok(())
}

/// Party A holds a trail whose middle offset carries three entries, plus a
/// second party's row at an interleaving offset to prove the party filter.
async fn seed_chain_audit_trail(pool: &SqlitePool, party_a: &str, party_b: &str) -> Result {
    for (offset, contract_id) in [
        (30, "c-30"),
        (20, "c-20-a"),
        (20, "c-20-b"),
        (20, "c-20-c"),
        (10, "c-10"),
    ] {
        insert_chain_audit_row(pool, party_a, offset, contract_id).await?;
    }
    insert_chain_audit_row(pool, party_b, 25, "other-25").await?;

    Ok(())
}

fn cached_offsets(rows: &[ChainAuditCacheRow]) -> Vec<i64> {
    rows.iter().map(|r| r.offset).collect()
}

/// The page is cut on whole offsets, not on rows: `limit = 2` reaches
/// offset 20 and then keeps all three of its entries. Cutting at two rows
/// would strand the rest of offset 20 — the next page asks for
/// `offset < 20` and would never return them.
#[sqlx::test(migrator = "MIGRATOR")]
async fn test_chain_audit_cache_keeps_offset_groups_whole(pool: SqlitePool) -> Result {
    let party_a = test_party_id("party-a");
    let party_b = test_party_id("party-b");
    seed_chain_audit_trail(&pool, &party_a.to_string(), &party_b.to_string()).await?;

    let rows = pool.get_chain_audit_cache(&party_a, 2, None).await?;
    assert_eq!(cached_offsets(&rows), vec![30, 20, 20, 20]);

    Ok(())
}

/// The cursor is exclusive, so paging from offset 20 returns what is
/// strictly older and never repeats that offset's group.
#[sqlx::test(migrator = "MIGRATOR")]
async fn test_chain_audit_cache_pages_before_a_cursor(pool: SqlitePool) -> Result {
    let party_a = test_party_id("party-a");
    let party_b = test_party_id("party-b");
    seed_chain_audit_trail(&pool, &party_a.to_string(), &party_b.to_string()).await?;

    let rows = pool.get_chain_audit_cache(&party_a, 2, Some(20)).await?;
    assert_eq!(cached_offsets(&rows), vec![10]);

    // Past the oldest cached offset there is nothing left to hand back —
    // the handler treats this empty result as a miss and reads Canton.
    let rows = pool.get_chain_audit_cache(&party_a, 2, Some(10)).await?;
    assert!(rows.is_empty());

    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_chain_audit_cache_returns_all_rows_for_an_oversized_limit(
    pool: SqlitePool,
) -> Result {
    let party_a = test_party_id("party-a");
    let party_b = test_party_id("party-b");
    seed_chain_audit_trail(&pool, &party_a.to_string(), &party_b.to_string()).await?;

    let rows = pool.get_chain_audit_cache(&party_a, 500, None).await?;
    assert_eq!(cached_offsets(&rows), vec![30, 20, 20, 20, 10]);

    Ok(())
}

/// A single offset bigger than the page still comes back whole, since the
/// cursor cannot address part of an offset group.
#[sqlx::test(migrator = "MIGRATOR")]
async fn test_chain_audit_cache_keeps_an_oversized_group_intact(pool: SqlitePool) -> Result {
    let party_a = test_party_id("party-a");
    let party_a_str = party_a.to_string();
    for contract_id in ["c-a", "c-b", "c-c"] {
        insert_chain_audit_row(&pool, &party_a_str, 20, contract_id).await?;
    }

    let rows = pool.get_chain_audit_cache(&party_a, 1, None).await?;
    assert_eq!(cached_offsets(&rows), vec![20, 20, 20]);

    Ok(())
}

/// Another party's rows never leak into a page, even at an offset that
/// interleaves with the queried party's trail.
#[sqlx::test(migrator = "MIGRATOR")]
async fn test_chain_audit_cache_filters_by_party(pool: SqlitePool) -> Result {
    let party_a = test_party_id("party-a");
    let party_b = test_party_id("party-b");
    seed_chain_audit_trail(&pool, &party_a.to_string(), &party_b.to_string()).await?;

    let rows = pool.get_chain_audit_cache(&party_b, 50, None).await?;
    assert_eq!(cached_offsets(&rows), vec![25]);

    let rows = pool.get_chain_audit_cache(&party_a, 50, None).await?;
    assert!(rows.iter().all(|r| r.offset != 25));

    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_governance_audit_filters_by_party(pool: SqlitePool) -> Result {
    let party_a = test_party_id("party-a");
    let party_b = test_party_id("party-b");

    insert_audit_entry(&pool, &party_a.to_string(), "propose", "success", 1000).await?;
    insert_audit_entry(&pool, &party_b.to_string(), "confirm", "success", 2000).await?;

    let entries = pool.get_governance_audit(&party_a, 50, 0).await?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].event_type, "propose");

    let entries = pool.get_governance_audit(&party_b, 50, 0).await?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].event_type, "confirm");

    Ok(())
}
