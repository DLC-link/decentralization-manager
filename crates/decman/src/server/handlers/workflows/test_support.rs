//! Runs, party ids, app states, cached parties, kick fixtures and an admin
//! request shared by the workflow handler tests.

use std::{collections::HashSet, sync::Arc};

use actix_web::{http::StatusCode, test as actix_test, web};
use canton_proto_rs::com::digitalasset::canton::crypto::v30::{SigningKeyUsage, SigningPublicKey};
use sqlx::SqlitePool;

use crate::{
    auth::Principal,
    canton_id::CantonId,
    db::{
        rows::DecPartyParticipantRow,
        schema::{Commitable, SchemaWrite},
    },
    error::Result,
    server::{
        AppState,
        health::HealthReply,
        types::{WorkflowKind, WorkflowProgress, WorkflowRole, WorkflowRun},
    },
    utils,
};

/// A minimal coordinator run row for `party`, in progress.
pub fn in_progress_run(instance: &str, party: &CantonId) -> WorkflowRun {
    WorkflowRun {
        instance_name: instance.to_string(),
        kind: WorkflowKind::Contracts,
        role: WorkflowRole::Coordinator,
        status: WorkflowProgress::InProgress,
        current_step: "SubmitProposals".to_string(),
        step_index: 0,
        step_total: 1,
        config_json: "{}".to_string(),
        coordinator_pubkey: None,
        coordinator_instance: None,
        coordinator_name: None,
        expected_peers: Vec::new(),
        completed_peers: Vec::new(),
        connected_peers: Vec::new(),
        acs_progress: None,
        dec_party_id: Some(party.clone()),
        prefix: None,
        participants: Vec::new(),
        previous_threshold: None,
        new_threshold: None,
        kicked_participant: None,
        kicked_owner_key: None,
        added_participant: None,
        package_names: Vec::new(),
        dar_filenames: Vec::new(),
        error: None,
        dismissed: false,
        created_at: 0,
        updated_at: 0,
    }
}

pub fn pid(tag: u8) -> anyhow::Result<CantonId> {
    let ns = format!("1220{:0>64}", format!("{tag:02x}"));
    CantonId::parse(&format!("validator-{tag}::{ns}"))
}

pub fn host_row(uid: &CantonId, owner_key: Option<&str>) -> DecPartyParticipantRow {
    DecPartyParticipantRow {
        dec_party_id: "party".to_string(),
        participant_uid: uid.to_string(),
        permission: "confirmation".to_string(),
        owner_key: owner_key.map(str::to_string),
        signing_key: None,
    }
}

pub fn health_reply(version: &str, features: &[&str]) -> HealthReply {
    HealthReply {
        status: crate::server::ConnectionStatus::Connected,
        workflow: None,
        version: Some(version.to_string()),
        build_version: None,
        features: features.iter().map(ToString::to_string).collect(),
    }
}

pub fn ids(tags: &[u8]) -> anyhow::Result<HashSet<CantonId>> {
    tags.iter().map(|tag| pid(*tag)).collect()
}

/// Caches `party` at namespace threshold `threshold`, hosted by `hosts`
/// (this node is `pid(1)`), each under its own owner key `k{tag}`. The
/// keys in `owners_only` own the party without hosting it. Only `peers`
/// are configured peers of this node.
///
/// The party also gets a run in flight. A kick that passes every check
/// then stops at the in-flight guard with a 409, before it probes or
/// invites any peer.
pub async fn cache_kick_party(
    pool: &SqlitePool,
    party: &CantonId,
    threshold: i64,
    hosts: &[u8],
    owners_only: &[&str],
    peers: &[u8],
) -> Result {
    cache_party(pool, party, threshold, None).await?;
    let mut keys: Vec<String> = hosts.iter().map(|tag| format!("k{tag}")).collect();
    let host_rows = hosts
        .iter()
        .zip(&keys)
        .map(|(tag, key)| Ok(host_row(&pid(*tag)?, Some(key))))
        .collect::<anyhow::Result<Vec<_>>>()?;
    keys.extend(owners_only.iter().map(ToString::to_string));
    let mut tx = pool.begin_transaction().await?;
    tx.replace_dec_party_participants(party, &host_rows).await?;
    tx.replace_dec_party_owners(party, &keys).await?;
    for tag in peers {
        tx.insert_peer(&crate::config::Peer {
            participant_id: pid(*tag)?,
            name: format!("peer-{tag}"),
            address: "127.0.0.1".to_string(),
            port: 1,
            public_key: String::new(),
            party: None,
        })
        .await?;
    }
    Commitable::commit(tx).await?;
    insert_run(pool, &in_progress_run("contracts-in-flight", party)).await
}

pub fn kick_public_key(tag: u8) -> SigningPublicKey {
    SigningPublicKey {
        public_key: vec![tag; 32],
        usage: vec![SigningKeyUsage::Protocol as i32],
        ..Default::default()
    }
}

/// Records `kick_public_key(key)` as the signing key of host `pid(host)`.
pub async fn record_signing_keys(pool: &SqlitePool, party: &CantonId, keys: &[(u8, u8)]) -> Result {
    for (host, key) in keys {
        sqlx::query(
            "UPDATE dec_party_participant SET signing_key = ? \
             WHERE dec_party_id = ? AND participant_uid = ?",
        )
        .bind(utils::compute_fingerprint(&kick_public_key(*key)).to_string())
        .bind(party.to_string())
        .bind(pid(*host)?.to_string())
        .execute(pool)
        .await?;
    }
    Ok(())
}

pub async fn cache_party(
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

pub fn test_cid(prefix: &str) -> anyhow::Result<CantonId> {
    let ns = format!("1220{:0>64}", "a");
    CantonId::parse(&format!("{prefix}::{ns}"))
}

pub async fn insert_run(pool: &SqlitePool, run: &WorkflowRun) -> Result {
    let mut tx = pool.begin_transaction().await?;
    tx.upsert_workflow_run(run).await?;
    Commitable::commit(tx).await
}

/// `AppState::for_test` on a migrated database.
pub async fn state_on(pool: SqlitePool) -> Result<web::Data<AppState>> {
    let state = Arc::try_unwrap(AppState::for_test(None).await?.into_inner())
        .map_err(|_| anyhow::anyhow!("a fresh test state has one owner"))?;
    Ok(web::Data::new(AppState { db: pool, ..state }))
}

pub fn admin() -> Principal {
    Principal {
        sub: "admin".to_string(),
        issuer: "test".to_string(),
        roles: Vec::new(),
        email: None,
    }
}

pub async fn status_and_body(response: actix_web::dev::ServiceResponse) -> (StatusCode, String) {
    let status = response.status();
    let body = actix_test::read_body(response).await;
    (status, String::from_utf8_lossy(&body).into_owned())
}
