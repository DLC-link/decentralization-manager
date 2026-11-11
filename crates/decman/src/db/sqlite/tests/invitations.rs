//! Pending and handled invitations.

use sqlx::SqlitePool;

use super::fixtures::TEST_NS;
use crate::{
    canton_id::CantonId,
    db::{
        schema::{Commitable, SchemaRead, SchemaWrite},
        sqlite::connect::MIGRATOR,
    },
    error::Result,
    server::{InvitationOutcome, InvitationType, PendingInvitation},
};

// ====================================================================
// Pending invitations
// ====================================================================

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_pending_invitations_roundtrip(pool: SqlitePool) -> Result {
    assert!(pool.get_all_pending_invitations().await?.is_empty());

    let inv_a = PendingInvitation {
        id: "onboarding-aaaaaaaaaaaaaaaa".to_string(),
        invitation_type: InvitationType::Onboarding,
        coordinator_pubkey: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        coordinator_name: None,
        received_at: 1000,
        prefix: Some("my-party".to_string()),
        participants: vec![
            CantonId::parse(&format!("node1::{TEST_NS}")).unwrap(),
            CantonId::parse(&format!("node2::{TEST_NS}")).unwrap(),
        ],
        dar_filenames: Vec::new(),
        dar_hashes: Vec::new(),
        kicked_participant: None,
        kicked_owner_key: None,
        new_participant: None,
        new_threshold: None,
        previous_threshold: None,
        confirmation_threshold: None,
        dec_party_id: None,
        package_names: Vec::new(),
        contract_intents: Vec::new(),
        workflow_instance: Some("my-party-creation".to_string()),
    };
    let inv_b = PendingInvitation {
        id: "kick-bbbbbbbbbbbbbbbb".to_string(),
        invitation_type: InvitationType::Kick,
        coordinator_pubkey: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
        coordinator_name: None,
        received_at: 2000,
        prefix: None,
        participants: Vec::new(),
        dar_filenames: Vec::new(),
        dar_hashes: Vec::new(),
        kicked_participant: Some(CantonId::parse(&format!("kicked::{TEST_NS}")).unwrap()),
        kicked_owner_key: Some("1220kicked".to_string()),
        new_participant: None,
        new_threshold: Some(2),
        previous_threshold: Some(3),
        confirmation_threshold: Some(1),
        dec_party_id: Some(CantonId::parse(&format!("dec::{TEST_NS}")).unwrap()),
        package_names: Vec::new(),
        contract_intents: Vec::new(),
        workflow_instance: None,
    };

    let mut tx = pool.begin_transaction().await?;
    tx.upsert_pending_invitation(&inv_a).await?;
    tx.upsert_pending_invitation(&inv_b).await?;
    Commitable::commit(tx).await?;

    let inv_c = PendingInvitation {
        id: "dars-cccccccccccccccc".to_string(),
        invitation_type: InvitationType::Dars,
        coordinator_pubkey: "cccccccccccccccccccccccccccccccc".to_string(),
        coordinator_name: None,
        received_at: 3000,
        prefix: None,
        participants: Vec::new(),
        dar_filenames: vec!["app.dar".to_string(), "lib.dar".to_string()],
        dar_hashes: Vec::new(),
        kicked_participant: None,
        kicked_owner_key: None,
        new_participant: None,
        new_threshold: None,
        previous_threshold: None,
        confirmation_threshold: None,
        dec_party_id: None,
        package_names: Vec::new(),
        contract_intents: Vec::new(),
        workflow_instance: None,
    };
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_pending_invitation(&inv_c).await?;
    Commitable::commit(tx).await?;

    let loaded = pool.get_all_pending_invitations().await?;
    assert_eq!(loaded.len(), 3);
    assert_eq!(loaded[0].id, inv_a.id);
    assert_eq!(loaded[0].prefix.as_deref(), Some("my-party"));
    assert_eq!(loaded[0].participants.len(), 2);
    assert_eq!(loaded[0].confirmation_threshold, None);
    assert_eq!(loaded[1].invitation_type, InvitationType::Kick);
    assert_eq!(loaded[1].confirmation_threshold, Some(1));
    assert_eq!(loaded[1].kicked_owner_key.as_deref(), Some("1220kicked"));
    assert!(loaded[1].prefix.is_none());
    assert!(loaded[1].participants.is_empty());
    assert_eq!(loaded[2].invitation_type, InvitationType::Dars);
    assert_eq!(loaded[2].dar_filenames, vec!["app.dar", "lib.dar"]);

    let mut tx = pool.begin_transaction().await?;
    tx.delete_pending_invitation(&inv_a.id).await?;
    Commitable::commit(tx).await?;
    assert_eq!(pool.get_all_pending_invitations().await?.len(), 2);

    let mut tx = pool.begin_transaction().await?;
    tx.delete_pending_invitations_by_coordinator(&inv_b.coordinator_pubkey)
        .await?;
    Commitable::commit(tx).await?;
    assert_eq!(pool.get_all_pending_invitations().await?.len(), 1);

    let mut tx = pool.begin_transaction().await?;
    tx.delete_pending_invitations_by_coordinator(&inv_c.coordinator_pubkey)
        .await?;
    Commitable::commit(tx).await?;
    assert!(pool.get_all_pending_invitations().await?.is_empty());

    Ok(())
}

/// A minimal onboarding invitation from `coordinator`.
fn test_invitation(id: &str, coordinator: &str, received_at: i64) -> PendingInvitation {
    PendingInvitation {
        id: id.to_string(),
        invitation_type: InvitationType::Onboarding,
        coordinator_pubkey: coordinator.to_string(),
        coordinator_name: None,
        received_at,
        prefix: Some(format!("party-{id}")),
        participants: Vec::new(),
        dar_filenames: Vec::new(),
        dar_hashes: Vec::new(),
        kicked_participant: None,
        kicked_owner_key: None,
        new_participant: None,
        new_threshold: None,
        previous_threshold: None,
        confirmation_threshold: None,
        dec_party_id: None,
        package_names: Vec::new(),
        contract_intents: Vec::new(),
        workflow_instance: Some(format!("run-{id}")),
    }
}

async fn save_invitations(pool: &SqlitePool, invitations: &[PendingInvitation]) -> Result {
    let mut tx = pool.begin_transaction().await?;
    for invitation in invitations {
        tx.upsert_pending_invitation(invitation).await?;
    }
    Commitable::commit(tx).await?;
    Ok(())
}

async fn mark_handled(
    pool: &SqlitePool,
    id: &str,
    outcome: InvitationOutcome,
    handled_at: i64,
) -> Result<bool> {
    let mut tx = pool.begin_transaction().await?;
    let marked = tx.mark_invitation_handled(id, outcome, handled_at).await?;
    Commitable::commit(tx).await?;
    Ok(marked)
}

fn ids<'a>(invitations: impl IntoIterator<Item = &'a PendingInvitation>) -> Vec<&'a str> {
    invitations.into_iter().map(|i| i.id.as_str()).collect()
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_handled_invitation_leaves_the_pending_list_as_history(pool: SqlitePool) -> Result {
    let accepted = test_invitation("a", "coord-1", 1000);
    let pending = test_invitation("b", "coord-1", 2000);
    save_invitations(&pool, &[accepted.clone(), pending.clone()]).await?;

    assert!(mark_handled(&pool, "a", InvitationOutcome::Accepted, 5000).await?);

    let still_pending = pool.get_all_pending_invitations().await?;
    assert_eq!(ids(&still_pending), vec!["b"]);
    let handled = pool.get_handled_invitations(50).await?;
    assert_eq!(handled.len(), 1);
    assert_eq!(handled[0].outcome, InvitationOutcome::Accepted);
    assert_eq!(handled[0].handled_at, 5000);
    assert_eq!(handled[0].invitation.id, "a");
    assert_eq!(handled[0].invitation.prefix, accepted.prefix);
    assert_eq!(handled[0].invitation.received_at, 1000);
    assert_eq!(
        handled[0].invitation.workflow_instance,
        accepted.workflow_instance
    );

    Ok(())
}

/// Accept and decline consume the pending row. A second attempt must
/// change nothing, as it did when the row was deleted.
#[sqlx::test(migrator = "MIGRATOR")]
async fn test_handled_invitation_cannot_be_handled_again(pool: SqlitePool) -> Result {
    save_invitations(&pool, &[test_invitation("a", "coord-1", 1000)]).await?;

    assert!(mark_handled(&pool, "a", InvitationOutcome::Declined, 5000).await?);
    assert!(!mark_handled(&pool, "a", InvitationOutcome::Accepted, 6000).await?);
    assert!(!mark_handled(&pool, "a", InvitationOutcome::Declined, 7000).await?);
    assert!(!mark_handled(&pool, "missing", InvitationOutcome::Accepted, 8000).await?);

    let handled = pool.get_handled_invitations(50).await?;
    assert_eq!(handled.len(), 1);
    assert_eq!(handled[0].outcome, InvitationOutcome::Declined);
    assert_eq!(handled[0].handled_at, 5000);
    assert!(pool.get_all_pending_invitations().await?.is_empty());

    Ok(())
}

/// A coordinator that sends the same invitation again (same run, so same
/// id) after a decline gets a fresh pending card, as before history.
/// The fallback for a card that was never persisted: the answered
/// invitation is written in full, replacing any older row with its id.
#[sqlx::test(migrator = "MIGRATOR")]
async fn test_insert_handled_invitation_writes_the_answer(pool: SqlitePool) -> Result {
    save_invitations(&pool, &[test_invitation("old", "coord-1", 1000)]).await?;
    mark_handled(&pool, "old", InvitationOutcome::Declined, 2000).await?;

    let mut tx = pool.begin_transaction().await?;
    tx.insert_handled_invitation(
        &test_invitation("new", "coord-1", 3000),
        InvitationOutcome::Accepted,
        3500,
    )
    .await?;
    tx.insert_handled_invitation(
        &test_invitation("old", "coord-1", 4000),
        InvitationOutcome::Accepted,
        4500,
    )
    .await?;
    Commitable::commit(tx).await?;

    let handled = pool.get_handled_invitations(50).await?;
    let summary: Vec<(&str, InvitationOutcome, i64, i64)> = handled
        .iter()
        .map(|h| {
            (
                h.invitation.id.as_str(),
                h.outcome,
                h.invitation.received_at,
                h.handled_at,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("old", InvitationOutcome::Accepted, 4000, 4500),
            ("new", InvitationOutcome::Accepted, 3000, 3500),
        ]
    );
    assert_eq!(handled[1].invitation.prefix.as_deref(), Some("party-new"));
    assert!(pool.get_all_pending_invitations().await?.is_empty());
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_reinvite_after_decline_is_pending_again(pool: SqlitePool) -> Result {
    save_invitations(&pool, &[test_invitation("a", "coord-1", 1000)]).await?;
    mark_handled(&pool, "a", InvitationOutcome::Declined, 5000).await?;

    save_invitations(&pool, &[test_invitation("a", "coord-1", 9000)]).await?;

    let pending = pool.get_all_pending_invitations().await?;
    assert_eq!(ids(&pending), vec!["a"]);
    assert_eq!(pending[0].received_at, 9000);
    assert!(pool.get_handled_invitations(50).await?.is_empty());
    // And it can be answered again.
    assert!(mark_handled(&pool, "a", InvitationOutcome::Accepted, 9500).await?);

    Ok(())
}

/// Cancel and cap eviction delete pending invitations only: a coordinator
/// cancelling its run must not erase what this operator already decided.
#[sqlx::test(migrator = "MIGRATOR")]
async fn test_pending_deletes_keep_handled_history(pool: SqlitePool) -> Result {
    save_invitations(
        &pool,
        &[
            test_invitation("a", "coord-1", 1000),
            test_invitation("b", "coord-1", 2000),
        ],
    )
    .await?;
    mark_handled(&pool, "a", InvitationOutcome::Declined, 5000).await?;

    let mut tx = pool.begin_transaction().await?;
    tx.delete_pending_invitation("a").await?;
    tx.delete_pending_invitations_by_coordinator("coord-1")
        .await?;
    Commitable::commit(tx).await?;

    assert!(pool.get_all_pending_invitations().await?.is_empty());
    let handled = pool.get_handled_invitations(50).await?;
    assert_eq!(ids(handled.iter().map(|h| &h.invitation)), vec!["a"]);

    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_handled_invitations_list_newest_first_up_to_the_limit(pool: SqlitePool) -> Result {
    let invitations: Vec<_> = ["a", "b", "c"]
        .iter()
        .map(|id| test_invitation(id, "coord-1", 1000))
        .collect();
    save_invitations(&pool, &invitations).await?;
    mark_handled(&pool, "b", InvitationOutcome::Accepted, 5001).await?;
    mark_handled(&pool, "a", InvitationOutcome::Declined, 5003).await?;
    mark_handled(&pool, "c", InvitationOutcome::Accepted, 5002).await?;

    let handled = pool.get_handled_invitations(2).await?;

    assert_eq!(ids(handled.iter().map(|h| &h.invitation)), vec!["a", "c"]);
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_prune_keeps_the_newest_handled_and_every_pending(pool: SqlitePool) -> Result {
    let invitations: Vec<_> = (0..6)
        .map(|i| test_invitation(&format!("inv-{i}"), "coord-1", 1000))
        .collect();
    save_invitations(&pool, &invitations).await?;
    for i in 0..5 {
        mark_handled(
            &pool,
            &format!("inv-{i}"),
            InvitationOutcome::Declined,
            5000 + i,
        )
        .await?;
    }

    let mut tx = pool.begin_transaction().await?;
    let pruned = tx.prune_handled_invitations(3).await?;
    Commitable::commit(tx).await?;

    assert_eq!(pruned, 2);
    let handled = pool.get_handled_invitations(50).await?;
    assert_eq!(
        ids(handled.iter().map(|h| &h.invitation)),
        vec!["inv-4", "inv-3", "inv-2"]
    );
    assert_eq!(
        ids(&pool.get_all_pending_invitations().await?),
        vec!["inv-5"]
    );
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn test_contracts_pending_invitation_roundtrip(pool: SqlitePool) -> Result {
    // A Contracts invite carries dec party + member set + package names so
    // the peer card is as rich as the coordinator's. Confirm all three
    // round-trip through the DB (validates migration 000011).
    let inv = PendingInvitation {
        id: "contracts-dddddddddddddddd".to_string(),
        invitation_type: InvitationType::Contracts,
        coordinator_pubkey: "dddddddddddddddddddddddddddddddd".to_string(),
        coordinator_name: None,
        received_at: 4000,
        prefix: None,
        participants: vec![
            CantonId::parse(&format!("node1::{TEST_NS}")).unwrap(),
            CantonId::parse(&format!("node2::{TEST_NS}")).unwrap(),
        ],
        dar_filenames: Vec::new(),
        dar_hashes: Vec::new(),
        kicked_participant: None,
        kicked_owner_key: None,
        new_participant: None,
        new_threshold: None,
        previous_threshold: None,
        confirmation_threshold: None,
        dec_party_id: Some(CantonId::parse(&format!("dec::{TEST_NS}")).unwrap()),
        package_names: vec!["Governance Core".to_string(), "Token Custody".to_string()],
        contract_intents: vec![common::api::ContractDeploymentIntent {
            package_id: "ab".repeat(32),
            module_name: "Governance".to_string(),
            entity_name: "Settings".to_string(),
            argument_hash: "cd".repeat(32),
        }],
        workflow_instance: Some("dec-contracts-4000".to_string()),
    };

    let mut tx = pool.begin_transaction().await?;
    tx.upsert_pending_invitation(&inv).await?;
    Commitable::commit(tx).await?;

    let loaded = pool.get_all_pending_invitations().await?;
    assert_eq!(loaded.len(), 1);
    let got = &loaded[0];
    assert_eq!(got.invitation_type, InvitationType::Contracts);
    assert_eq!(got.participants.len(), 2);
    assert_eq!(
        got.dec_party_id.as_ref().map(CantonId::to_string),
        Some(format!("dec::{TEST_NS}"))
    );
    assert_eq!(got.package_names, vec!["Governance Core", "Token Custody"]);
    assert_eq!(got.contract_intents, inv.contract_intents);
    assert_eq!(got.workflow_instance.as_deref(), Some("dec-contracts-4000"));

    Ok(())
}
