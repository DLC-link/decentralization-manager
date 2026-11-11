//! Decentralized parties, their owners, participants, contracts and identity.

use sqlx::SqlitePool;

use super::{
    audit::test_party_id,
    fixtures::{TEST_NS, test_dec_party},
};
use crate::{
    canton_id::{CantonId, Fingerprint},
    db::{
        rows::{
            DecPartyContractRow, DecPartyOwnerRow, DecPartyParticipantRow, DecPartyRow,
            OwnerKeyRefusal,
        },
        schema::{Commitable, SchemaRead, SchemaWrite},
        sqlite::connect::MIGRATOR,
    },
    error::Result,
};

// ====================================================================
// Decentralized Parties
// ====================================================================

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_insert_and_get_dec_parties(pool: SqlitePool) -> Result {
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&DecPartyRow {
        confirmation_threshold: Some(3),
        ..test_dec_party("net-a")
    })
    .await?;
    tx.upsert_dec_party(&test_dec_party("net-b")).await?;
    Commitable::commit(tx).await?;

    let all = pool.get_dec_parties_by_prefix("").await?;
    assert_eq!(all.len(), 2);

    let filtered = pool.get_dec_parties_by_prefix("net-a").await?;
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].threshold, 2);
    assert_eq!(filtered[0].confirmation_threshold, Some(3));
    assert_eq!(
        pool.get_dec_parties_by_prefix("net-b").await?[0].confirmation_threshold,
        None
    );

    assert!(
        pool.get_dec_parties_by_prefix("nonexistent")
            .await?
            .is_empty()
    );

    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_dec_party_owners(pool: SqlitePool) -> Result {
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&test_dec_party("net-a")).await?;
    let party_id_str = format!("net-a::{TEST_NS}");
    let party_id = CantonId::parse(&party_id_str).unwrap();
    let owners = vec!["owner-key-1".to_string(), "owner-key-2".to_string()];
    tx.replace_dec_party_owners(&party_id, &owners).await?;
    Commitable::commit(tx).await?;

    let result = pool.get_dec_party_owners(&party_id).await?;
    assert_eq!(result.len(), 2);
    assert!(result.iter().any(|o| o.owner_key == "owner-key-1"));
    assert!(result.iter().any(|o| o.owner_key == "owner-key-2"));

    let mut tx = pool.begin_transaction().await?;
    tx.replace_dec_party_owners(&party_id, &["only-owner".to_string()])
        .await?;
    Commitable::commit(tx).await?;

    let result = pool.get_dec_party_owners(&party_id).await?;
    assert_eq!(result.len(), 1);

    Ok(())
}

/// A party refresh rewrites the owner set from Canton, which knows
/// nothing of claims. An owner that stays must keep the participant that
/// claimed it, or every owner that does not host the party drops out of
/// the list until the next OwnerKeys round.
#[sqlx::test(migrator = "MIGRATOR")]
async fn an_owner_keeps_its_claim_across_a_refresh(pool: SqlitePool) -> Result {
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&test_dec_party("net-a")).await?;
    let party_id = CantonId::parse(&format!("net-a::{TEST_NS}"))?;
    tx.replace_dec_party_owners(&party_id, &["kept".to_string(), "dropped".to_string()])
        .await?;
    tx.update_owner_claim(&party_id, "kept", "p2::1220ab")
        .await?;
    tx.update_owner_claim(&party_id, "not-an-owner", "p3::1220cd")
        .await?;
    Commitable::commit(tx).await?;

    let mut tx = pool.begin_transaction().await?;
    tx.replace_dec_party_owners(&party_id, &["kept".to_string(), "new".to_string()])
        .await?;
    Commitable::commit(tx).await?;

    let mut owners = pool.get_dec_party_owners(&party_id).await?;
    owners.sort_by(|a, b| a.owner_key.cmp(&b.owner_key));
    let claims: Vec<(&str, Option<&str>)> = owners
        .iter()
        .map(|o| (o.owner_key.as_str(), o.participant_uid.as_deref()))
        .collect();
    assert_eq!(claims, vec![("kept", Some("p2::1220ab")), ("new", None)]);
    Ok(())
}

/// A party with owners k1 to k3, hosted by p1 and p2 with no keys known.
async fn party_with_unkeyed_hosts(pool: &SqlitePool) -> Result<CantonId> {
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&test_dec_party("net-a")).await?;
    let party_id = CantonId::parse(&format!("net-a::{TEST_NS}"))?;
    let owners = ["k1", "k2", "k3"].map(str::to_string);
    tx.replace_dec_party_owners(&party_id, &owners).await?;
    let host = |uid: &str| DecPartyParticipantRow {
        dec_party_id: party_id.to_string(),
        participant_uid: uid.to_string(),
        permission: "confirmation".to_string(),
        owner_key: None,
        signing_key: None,
    };
    tx.replace_dec_party_participants(&party_id, &[host("p1::1220aa"), host("p2::1220bb")])
        .await?;
    Commitable::commit(tx).await?;
    Ok(party_id)
}

async fn host_key(pool: &SqlitePool, party_id: &CantonId, uid: &str) -> Result<Option<String>> {
    Ok(pool
        .get_dec_party_participants(party_id)
        .await?
        .into_iter()
        .find(|row| row.participant_uid == uid)
        .and_then(|row| row.owner_key))
}

async fn claimant(pool: &SqlitePool, party_id: &CantonId, key: &str) -> Result<Option<String>> {
    Ok(pool
        .get_dec_party_owners(party_id)
        .await?
        .into_iter()
        .find(|row| row.owner_key == key)
        .and_then(|row| row.participant_uid))
}

/// Any peer can report any key, so the first holder on record keeps it,
/// whether a host's key or a claim holds it.
#[sqlx::test(migrator = "MIGRATOR")]
async fn an_owner_key_has_one_holder_on_record(pool: SqlitePool) -> Result {
    let party_id = party_with_unkeyed_hosts(&pool).await?;
    let held_by = |uid: &str| Some(OwnerKeyRefusal::HeldBy(uid.to_string()));
    let mut tx = pool.begin_transaction().await?;
    assert_eq!(
        tx.update_participant_owner_key(&party_id, "p1::1220aa", "k1")
            .await?,
        None
    );
    assert_eq!(
        tx.update_participant_owner_key(&party_id, "p2::1220bb", "k1")
            .await?,
        held_by("p1::1220aa")
    );
    assert_eq!(
        tx.update_owner_claim(&party_id, "k1", "p3::1220cc").await?,
        held_by("p1::1220aa")
    );
    assert_eq!(
        tx.update_owner_claim(&party_id, "k3", "p3::1220cc").await?,
        None
    );
    assert_eq!(
        tx.update_owner_claim(&party_id, "k3", "p4::1220dd").await?,
        held_by("p3::1220cc")
    );
    assert_eq!(
        tx.update_participant_owner_key(&party_id, "p2::1220bb", "k3")
            .await?,
        held_by("p3::1220cc")
    );
    Commitable::commit(tx).await?;

    assert_eq!(host_key(&pool, &party_id, "p2::1220bb").await?, None);
    assert_eq!(claimant(&pool, &party_id, "k1").await?, None);
    assert_eq!(
        claimant(&pool, &party_id, "k3").await?.as_deref(),
        Some("p3::1220cc")
    );
    Ok(())
}

/// A holder's recorded key changes only once it is no longer an owner.
/// Until then a second key would let a peer swap its record for another
/// participant's key.
#[sqlx::test(migrator = "MIGRATOR")]
async fn a_recorded_owner_key_changes_once_it_leaves_the_owners(pool: SqlitePool) -> Result {
    let party_id = party_with_unkeyed_hosts(&pool).await?;
    let keeps = |key: &str| Some(OwnerKeyRefusal::KeepsKey(key.to_string()));
    let mut tx = pool.begin_transaction().await?;
    tx.update_participant_owner_key(&party_id, "p1::1220aa", "k1")
        .await?;
    tx.update_owner_claim(&party_id, "k3", "p3::1220cc").await?;
    assert_eq!(
        tx.update_participant_owner_key(&party_id, "p1::1220aa", "k2")
            .await?,
        keeps("k1")
    );
    assert_eq!(
        tx.update_owner_claim(&party_id, "k2", "p1::1220aa").await?,
        keeps("k1")
    );
    assert_eq!(
        tx.update_owner_claim(&party_id, "k2", "p3::1220cc").await?,
        keeps("k3")
    );
    tx.replace_dec_party_owners(&party_id, &["k2".to_string(), "k3".to_string()])
        .await?;
    assert_eq!(
        tx.update_participant_owner_key(&party_id, "p1::1220aa", "k2")
            .await?,
        None
    );
    Commitable::commit(tx).await?;

    assert_eq!(
        host_key(&pool, &party_id, "p1::1220aa").await?.as_deref(),
        Some("k2")
    );
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn deleting_an_owner_leaves_the_others(pool: SqlitePool) -> Result {
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&test_dec_party("net-a")).await?;
    let party_id = CantonId::parse(&format!("net-a::{TEST_NS}"))?;
    tx.replace_dec_party_owners(&party_id, &["a".to_string(), "b".to_string()])
        .await?;
    tx.delete_dec_party_owner(&party_id, "b").await?;
    Commitable::commit(tx).await?;

    let owners = pool.get_dec_party_owners(&party_id).await?;
    assert_eq!(owners.len(), 1);
    assert_eq!(owners[0].owner_key, "a");
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_dec_party_participants(pool: SqlitePool) -> Result {
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&test_dec_party("net-a")).await?;
    let party_id_str = format!("net-a::{TEST_NS}");
    let party_id = CantonId::parse(&party_id_str).unwrap();
    let participants = vec![
        DecPartyParticipantRow {
            dec_party_id: party_id_str.clone(),
            participant_uid: "node1::1220aa".to_string(),
            permission: "submission".to_string(),
            owner_key: Some("fingerprint-1".to_string()),
            signing_key: None,
        },
        DecPartyParticipantRow {
            dec_party_id: party_id_str.clone(),
            participant_uid: "node2::1220bb".to_string(),
            permission: "confirmation".to_string(),
            owner_key: None,
            signing_key: None,
        },
    ];
    tx.replace_dec_party_participants(&party_id, &participants)
        .await?;
    Commitable::commit(tx).await?;

    let result = pool.get_dec_party_participants(&party_id).await?;
    assert_eq!(result.len(), 2);
    // Assert by participant_uid, not positional index: the backing query
    // has no ORDER BY, so row order is not contractual and must not be
    // relied on.
    let node1 = result.iter().find(|p| p.participant_uid == "node1::1220aa");
    let node2 = result.iter().find(|p| p.participant_uid == "node2::1220bb");
    assert!(
        matches!(node1, Some(p) if p.permission == "submission"
                && p.owner_key.as_deref() == Some("fingerprint-1")),
        "node1 row missing or has wrong permission/owner_key"
    );
    assert!(
        matches!(node2, Some(p) if p.permission == "confirmation" && p.owner_key.is_none()),
        "node2 row missing or has wrong permission/owner_key"
    );

    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_replace_preserves_owner_key_when_incoming_is_null(pool: SqlitePool) -> Result {
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&test_dec_party("net-a")).await?;
    let party_id_str = format!("net-a::{TEST_NS}");
    let party_id = CantonId::parse(&party_id_str).unwrap();

    // First write — owner_key is known.
    tx.replace_dec_party_participants(
        &party_id,
        &[DecPartyParticipantRow {
            dec_party_id: party_id_str.clone(),
            participant_uid: "node1::1220aa".to_string(),
            permission: "submission".to_string(),
            owner_key: Some("fingerprint-1".to_string()),
            signing_key: None,
        }],
    )
    .await?;
    Commitable::commit(tx).await?;

    // Second write — same row, owner_key NULL (simulates a cache refresh
    // that hasn't yet been followed by `resolve_owner_keys_from_peers`).
    let mut tx = pool.begin_transaction().await?;
    tx.replace_dec_party_participants(
        &party_id,
        &[DecPartyParticipantRow {
            dec_party_id: party_id_str.clone(),
            participant_uid: "node1::1220aa".to_string(),
            permission: "submission".to_string(),
            owner_key: None,
            signing_key: None,
        }],
    )
    .await?;
    Commitable::commit(tx).await?;

    let result = pool.get_dec_party_participants(&party_id).await?;
    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0].owner_key,
        Some("fingerprint-1".to_string()),
        "owner_key should be preserved across a refresh that brings a NULL value"
    );

    Ok(())
}

/// The party's `party_signing_keys` record no owner per key, so the only
/// place the mapping from member to Daml key lives is what each member
/// reports about itself. A refresh that brings a NULL must not throw it
/// away, or a kick loses the record it needs to drop the right key (#428).
#[sqlx::test(migrator = "MIGRATOR")]
async fn test_participant_signing_key_survives_a_refresh(pool: SqlitePool) -> Result {
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&test_dec_party("net-a")).await?;
    let party_id_str = format!("net-a::{TEST_NS}");
    let party_id = CantonId::parse(&party_id_str)?;
    tx.replace_dec_party_participants(
        &party_id,
        &[DecPartyParticipantRow {
            dec_party_id: party_id_str.clone(),
            participant_uid: "node1::1220aa".to_string(),
            permission: "submission".to_string(),
            owner_key: None,
            signing_key: None,
        }],
    )
    .await?;
    // What `resolve_owner_keys_from_peers` writes once the peer answers.
    let signing_key: Fingerprint = TEST_NS.parse()?;
    tx.update_participant_signing_key(&party_id, "node1::1220aa", &signing_key)
        .await?;
    Commitable::commit(tx).await?;

    let seeded = pool.get_dec_party_participants(&party_id).await?;
    assert_eq!(
        seeded.first().and_then(|p| p.signing_key.clone()),
        Some(TEST_NS.to_string())
    );

    // A live Canton fetch never carries the fingerprint, so every refresh
    // brings a NULL for it.
    let mut tx = pool.begin_transaction().await?;
    tx.replace_dec_party_participants(
        &party_id,
        &[DecPartyParticipantRow {
            dec_party_id: party_id_str,
            participant_uid: "node1::1220aa".to_string(),
            permission: "confirmation".to_string(),
            owner_key: None,
            signing_key: None,
        }],
    )
    .await?;
    Commitable::commit(tx).await?;

    let result = pool.get_dec_party_participants(&party_id).await?;
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].permission, "confirmation");
    assert_eq!(
        result[0].signing_key,
        Some(TEST_NS.to_string()),
        "signing_key should be preserved across a refresh that brings a NULL value"
    );

    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_upsert_dec_party_preserves_participant_rows(pool: SqlitePool) -> Result {
    // Regression for the cascading-delete bug: INSERT OR REPLACE on the
    // parent `dec_party` row would fire ON DELETE CASCADE on
    // dec_party_participant, wiping owner_key rows before the participant
    // UPSERT could COALESCE them back.
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&test_dec_party("net-a")).await?;
    let party_id_str = format!("net-a::{TEST_NS}");
    let party_id = CantonId::parse(&party_id_str).unwrap();
    tx.replace_dec_party_participants(
        &party_id,
        &[DecPartyParticipantRow {
            dec_party_id: party_id_str.clone(),
            participant_uid: "node1::1220aa".to_string(),
            permission: "submission".to_string(),
            owner_key: Some("fingerprint-1".to_string()),
            signing_key: None,
        }],
    )
    .await?;
    Commitable::commit(tx).await?;

    // Re-upsert the parent dec_party row. With ON CONFLICT DO UPDATE
    // (not INSERT OR REPLACE), this must NOT cascade-delete the
    // participant row.
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&test_dec_party("net-a")).await?;
    Commitable::commit(tx).await?;

    let result = pool.get_dec_party_participants(&party_id).await?;
    assert_eq!(
        result.len(),
        1,
        "participant row was wiped by cascading delete on parent upsert — \
             INSERT OR REPLACE regression"
    );
    assert_eq!(
        result[0].owner_key,
        Some("fingerprint-1".to_string()),
        "owner_key was wiped despite participant row surviving"
    );

    // Now run the full sequence: upsert dec_party + replace participants
    // with NULL owner_key. End-to-end this is what `store_parties_to_db`
    // does on every cache refresh.
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&test_dec_party("net-a")).await?;
    tx.replace_dec_party_participants(
        &party_id,
        &[DecPartyParticipantRow {
            dec_party_id: party_id_str.clone(),
            participant_uid: "node1::1220aa".to_string(),
            permission: "submission".to_string(),
            owner_key: None,
            signing_key: None,
        }],
    )
    .await?;
    Commitable::commit(tx).await?;

    let result = pool.get_dec_party_participants(&party_id).await?;
    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0].owner_key,
        Some("fingerprint-1".to_string()),
        "owner_key not preserved through the full upsert-party + \
             replace-participants sequence"
    );

    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_replace_dec_party_participants_removes_stale(pool: SqlitePool) -> Result {
    // Covers the `NOT IN` delete branch and the empty-list branch of
    // `replace_dec_party_participants` — i.e. participants that have left
    // the party must be removed from the cache.
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&test_dec_party("net-a")).await?;
    let party_id_str = format!("net-a::{TEST_NS}");
    let party_id = CantonId::parse(&party_id_str).unwrap();
    let p1 = "node1::1220aa";
    let p2 = "node2::1220bb";
    tx.replace_dec_party_participants(
        &party_id,
        &[
            DecPartyParticipantRow {
                dec_party_id: party_id_str.clone(),
                participant_uid: p1.to_string(),
                permission: "submission".to_string(),
                owner_key: Some("fp-1".to_string()),
                signing_key: None,
            },
            DecPartyParticipantRow {
                dec_party_id: party_id_str.clone(),
                participant_uid: p2.to_string(),
                permission: "submission".to_string(),
                owner_key: Some("fp-2".to_string()),
                signing_key: None,
            },
        ],
    )
    .await?;
    Commitable::commit(tx).await?;
    assert_eq!(pool.get_dec_party_participants(&party_id).await?.len(), 2);

    // Replace with only p1 — p2 must be removed by the NOT IN branch.
    let mut tx = pool.begin_transaction().await?;
    tx.replace_dec_party_participants(
        &party_id,
        &[DecPartyParticipantRow {
            dec_party_id: party_id_str.clone(),
            participant_uid: p1.to_string(),
            permission: "submission".to_string(),
            owner_key: Some("fp-1".to_string()),
            signing_key: None,
        }],
    )
    .await?;
    Commitable::commit(tx).await?;
    let remaining = pool.get_dec_party_participants(&party_id).await?;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].participant_uid, p1);

    // Replace with empty list — p1 must be removed by the empty-list branch.
    let mut tx = pool.begin_transaction().await?;
    tx.replace_dec_party_participants(&party_id, &[]).await?;
    Commitable::commit(tx).await?;
    assert!(pool.get_dec_party_participants(&party_id).await?.is_empty());

    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_get_dec_party_participant_owner_key(pool: SqlitePool) -> Result {
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&test_dec_party("net-a")).await?;
    let party_id_str = format!("net-a::{TEST_NS}");
    let party_id = CantonId::parse(&party_id_str).unwrap();
    tx.replace_dec_party_participants(
        &party_id,
        &[
            DecPartyParticipantRow {
                dec_party_id: party_id_str.clone(),
                participant_uid: "node1::1220aa".to_string(),
                permission: "submission".to_string(),
                owner_key: Some("fingerprint-1".to_string()),
                signing_key: None,
            },
            DecPartyParticipantRow {
                dec_party_id: party_id_str.clone(),
                participant_uid: "node2::1220bb".to_string(),
                permission: "confirmation".to_string(),
                owner_key: None,
                signing_key: None,
            },
        ],
    )
    .await?;
    Commitable::commit(tx).await?;

    assert_eq!(
        pool.get_dec_party_participant_owner_key(&party_id, "node1::1220aa")
            .await?,
        Some("fingerprint-1".to_string())
    );
    assert_eq!(
        pool.get_dec_party_participant_owner_key(&party_id, "node2::1220bb")
            .await?,
        None
    );
    assert_eq!(
        pool.get_dec_party_participant_owner_key(&party_id, "missing::1220cc")
            .await?,
        None
    );

    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_dec_party_contracts(pool: SqlitePool) -> Result {
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&test_dec_party("net-a")).await?;
    let party_id_str = format!("net-a::{TEST_NS}");
    let party_id = CantonId::parse(&party_id_str).unwrap();
    let contracts = vec![
        DecPartyContractRow {
            dec_party_id: party_id_str.clone(),
            contract_id: "contract-1".to_string(),
            template_id: "Governance:GovernanceRules".to_string(),
            package_id: "#gov-core".to_string(),
            package_name: "governance-core-v0-rc3".to_string(),
            package_version: "0.1.0".to_string(),
            created_at: "2026-04-28T11:07:59.073177Z".to_string(),
        },
        DecPartyContractRow {
            dec_party_id: party_id_str.clone(),
            contract_id: "contract-2".to_string(),
            template_id: "CBTC.Governance:CBTCGovernanceRules".to_string(),
            package_id: "#cbtc-governance".to_string(),
            package_name: "cbtc-governance".to_string(),
            package_version: "0.1.0".to_string(),
            created_at: "2026-04-28T11:08:00.000000Z".to_string(),
        },
    ];
    tx.replace_dec_party_contracts(&party_id, &contracts)
        .await?;
    Commitable::commit(tx).await?;

    let result = pool.get_all_dec_party_contracts("net-a").await?;
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].template_id, "Governance:GovernanceRules");

    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_delete_dec_parties_cascades(pool: SqlitePool) -> Result {
    let party_id_str = format!("net-a::{TEST_NS}");
    let party_id = CantonId::parse(&party_id_str).unwrap();

    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&test_dec_party("net-a")).await?;
    tx.replace_dec_party_owners(&party_id, &["owner-1".to_string()])
        .await?;
    tx.replace_dec_party_participants(
        &party_id,
        &[DecPartyParticipantRow {
            dec_party_id: party_id_str.clone(),
            participant_uid: "node1".to_string(),
            permission: "submission".to_string(),
            owner_key: None,
            signing_key: None,
        }],
    )
    .await?;
    tx.replace_dec_party_contracts(
        &party_id,
        &[DecPartyContractRow {
            dec_party_id: party_id_str.clone(),
            contract_id: "c1".to_string(),
            template_id: "t1".to_string(),
            package_id: "p1".to_string(),
            package_name: String::new(),
            package_version: String::new(),
            created_at: String::new(),
        }],
    )
    .await?;
    Commitable::commit(tx).await?;

    assert_eq!(pool.get_dec_party_owners(&party_id).await?.len(), 1);
    assert_eq!(pool.get_dec_party_participants(&party_id).await?.len(), 1);
    assert_eq!(pool.get_all_dec_party_contracts("net-a").await?.len(), 1);

    let mut tx = pool.begin_transaction().await?;
    tx.delete_dec_parties_by_prefix("net-a").await?;
    Commitable::commit(tx).await?;

    assert!(pool.get_dec_parties_by_prefix("net-a").await?.is_empty());
    assert!(pool.get_dec_party_owners(&party_id).await?.is_empty());
    assert!(pool.get_dec_party_participants(&party_id).await?.is_empty());
    // Count the child rows directly: the bulk getter joins `dec_party`,
    // so it would read empty even if the contract rows stayed behind.
    let (contracts,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM dec_party_contract WHERE dec_party_id = ?")
            .bind(&party_id_str)
            .fetch_one(&pool)
            .await?;
    assert_eq!(contracts, 0);

    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_delete_all_dec_parties(pool: SqlitePool) -> Result {
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&test_dec_party("net-a")).await?;
    tx.upsert_dec_party(&test_dec_party("net-b")).await?;
    Commitable::commit(tx).await?;

    assert_eq!(pool.get_dec_parties_by_prefix("").await?.len(), 2);

    let mut tx = pool.begin_transaction().await?;
    tx.delete_dec_parties_by_prefix("").await?;
    Commitable::commit(tx).await?;

    assert!(pool.get_dec_parties_by_prefix("").await?.is_empty());

    Ok(())
}

// ====================================================================
// delete_stale_dec_parties
// ====================================================================

/// Build a `DecPartyRow` with a caller-chosen `party_id` while keeping a
/// fixed `prefix`. `test_dec_party` ties the party_id to the prefix, so it
/// cannot produce several distinct parties under the same prefix — which is
/// exactly what the stale-delete prefix-scoping test needs.
fn dec_party_row(prefix: &str, party_id: &str) -> DecPartyRow {
    DecPartyRow {
        party_id: party_id.to_string(),
        prefix: prefix.to_string(),
        threshold: 2,
        updated_at: 1000,
        my_owner_key: None,
        confirmation_threshold: None,
    }
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_delete_stale_dec_parties_prefix_scoped(pool: SqlitePool) -> Result {
    // Three parties under `net-a`, one under `net-b`.
    let a1 = format!("a1::{TEST_NS}");
    let a2 = format!("a2::{TEST_NS}");
    let a3 = format!("a3::{TEST_NS}");
    let b1 = format!("b1::{TEST_NS}");

    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&dec_party_row("net-a", &a1)).await?;
    tx.upsert_dec_party(&dec_party_row("net-a", &a2)).await?;
    tx.upsert_dec_party(&dec_party_row("net-a", &a3)).await?;
    tx.upsert_dec_party(&dec_party_row("net-b", &b1)).await?;
    Commitable::commit(tx).await?;

    assert_eq!(pool.get_dec_parties_by_prefix("net-a").await?.len(), 3);
    assert_eq!(pool.get_dec_parties_by_prefix("net-b").await?.len(), 1);

    // Keep only a1 fresh under net-a → a2/a3 must go, a1 stays, and the
    // net-b party must be untouched (proves the `prefix = ?` scoping).
    let mut tx = pool.begin_transaction().await?;
    tx.delete_stale_dec_parties("net-a", std::slice::from_ref(&a1))
        .await?;
    Commitable::commit(tx).await?;

    let net_a = pool.get_dec_parties_by_prefix("net-a").await?;
    assert_eq!(net_a.len(), 1);
    assert_eq!(net_a[0].party_id, a1);
    let net_b = pool.get_dec_parties_by_prefix("net-b").await?;
    assert_eq!(net_b.len(), 1);
    assert_eq!(net_b[0].party_id, b1);

    // Empty fresh set for net-a → the early-return prefix-wide delete fires;
    // every net-a party goes while net-b still survives.
    let mut tx = pool.begin_transaction().await?;
    tx.delete_stale_dec_parties("net-a", &[]).await?;
    Commitable::commit(tx).await?;

    assert!(pool.get_dec_parties_by_prefix("net-a").await?.is_empty());
    let net_b = pool.get_dec_parties_by_prefix("net-b").await?;
    assert_eq!(net_b.len(), 1);
    assert_eq!(net_b[0].party_id, b1);

    Ok(())
}

// ====================================================================
// dec_party_identity
// ====================================================================

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_dec_party_identity_roundtrip(pool: SqlitePool) -> Result {
    let party_id = test_party_id("net-a");
    let kind = "peer_public_keys";

    // Peers deliberately written out of order; ids sort as a, b, c.
    let mut tx = pool.begin_transaction().await?;
    tx.write_dec_party_identity(&party_id, kind, "c::1220cc", b"payload-c")
        .await?;
    tx.write_dec_party_identity(&party_id, kind, "a::1220aa", b"payload-a")
        .await?;
    tx.write_dec_party_identity(&party_id, kind, "b::1220bb", b"payload-b")
        .await?;
    Commitable::commit(tx).await?;

    // read returns each peer's exact bytes.
    assert_eq!(
        pool.read_dec_party_identity(&party_id, kind, "a::1220aa")
            .await?,
        Some(b"payload-a".to_vec())
    );
    assert_eq!(
        pool.read_dec_party_identity(&party_id, kind, "b::1220bb")
            .await?,
        Some(b"payload-b".to_vec())
    );
    assert_eq!(
        pool.read_dec_party_identity(&party_id, kind, "c::1220cc")
            .await?,
        Some(b"payload-c".to_vec())
    );

    // A missing peer → None.
    assert_eq!(
        pool.read_dec_party_identity(&party_id, kind, "missing::1220dd")
            .await?,
        None
    );
    // A different artifact_kind on a known peer → None.
    assert_eq!(
        pool.read_dec_party_identity(&party_id, "other_kind", "a::1220aa")
            .await?,
        None
    );

    // list returns rows ordered a, b, c with payloads paired correctly.
    let listed = pool.list_dec_party_identity(&party_id, kind).await?;
    assert_eq!(
        listed,
        vec![
            ("a::1220aa".to_string(), b"payload-a".to_vec()),
            ("b::1220bb".to_string(), b"payload-b".to_vec()),
            ("c::1220cc".to_string(), b"payload-c".to_vec()),
        ]
    );

    // INSERT OR REPLACE: re-writing the same (party, kind, peer) replaces
    // the payload rather than inserting a duplicate row.
    let mut tx = pool.begin_transaction().await?;
    tx.write_dec_party_identity(&party_id, kind, "a::1220aa", b"payload-a-v2")
        .await?;
    Commitable::commit(tx).await?;

    assert_eq!(
        pool.read_dec_party_identity(&party_id, kind, "a::1220aa")
            .await?,
        Some(b"payload-a-v2".to_vec())
    );
    let listed = pool.list_dec_party_identity(&party_id, kind).await?;
    assert_eq!(listed.len(), 3);
    assert_eq!(
        listed[0],
        ("a::1220aa".to_string(), b"payload-a-v2".to_vec())
    );

    Ok(())
}

// ====================================================================
// get_all_dec_party_* — INNER JOIN + prefix scoping
// ====================================================================

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_get_all_dec_party_joins_and_prefix(pool: SqlitePool) -> Result {
    let party_a = test_party_id("net-a");
    let party_a_str = party_a.to_string();
    let party_b = test_party_id("net-b");
    let party_b_str = party_b.to_string();

    let mut tx = pool.begin_transaction().await?;
    tx.upsert_dec_party(&test_dec_party("net-a")).await?;
    tx.upsert_dec_party(&test_dec_party("net-b")).await?;

    // net-a: 2 owners, 1 participant. net-b: 1 owner, 2 participants.
    tx.replace_dec_party_owners(
        &party_a,
        &["a-owner-1".to_string(), "a-owner-2".to_string()],
    )
    .await?;
    tx.replace_dec_party_owners(&party_b, &["b-owner-1".to_string()])
        .await?;

    tx.replace_dec_party_participants(
        &party_a,
        &[DecPartyParticipantRow {
            dec_party_id: party_a_str.clone(),
            participant_uid: "a-node1::1220aa".to_string(),
            permission: "submission".to_string(),
            owner_key: None,
            signing_key: None,
        }],
    )
    .await?;
    tx.replace_dec_party_participants(
        &party_b,
        &[
            DecPartyParticipantRow {
                dec_party_id: party_b_str.clone(),
                participant_uid: "b-node1::1220bb".to_string(),
                permission: "submission".to_string(),
                owner_key: None,
                signing_key: None,
            },
            DecPartyParticipantRow {
                dec_party_id: party_b_str.clone(),
                participant_uid: "b-node2::1220cc".to_string(),
                permission: "confirmation".to_string(),
                owner_key: None,
                signing_key: None,
            },
        ],
    )
    .await?;
    Commitable::commit(tx).await?;

    // Empty prefix → every JOIN-matched row across both parties.
    let all_owners = pool.get_all_dec_party_owners("").await?;
    assert_eq!(all_owners.len(), 3);
    let all_participants = pool.get_all_dec_party_participants("").await?;
    assert_eq!(all_participants.len(), 3);

    // Specific prefix → only that party's rows.
    let a_owners = pool.get_all_dec_party_owners("net-a").await?;
    assert_eq!(a_owners.len(), 2);
    assert!(a_owners.iter().all(|o| o.dec_party_id == party_a_str));
    let b_owners = pool.get_all_dec_party_owners("net-b").await?;
    assert_eq!(b_owners.len(), 1);
    assert_eq!(
        b_owners[0],
        DecPartyOwnerRow {
            dec_party_id: party_b_str.clone(),
            owner_key: "b-owner-1".to_string(),
            participant_uid: None,
        }
    );

    let a_participants = pool.get_all_dec_party_participants("net-a").await?;
    assert_eq!(a_participants.len(), 1);
    assert_eq!(a_participants[0].participant_uid, "a-node1::1220aa");
    let b_participants = pool.get_all_dec_party_participants("net-b").await?;
    assert_eq!(b_participants.len(), 2);
    assert!(b_participants.iter().all(|p| p.dec_party_id == party_b_str));

    Ok(())
}
