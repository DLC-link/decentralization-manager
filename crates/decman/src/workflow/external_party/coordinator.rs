//! Coordinator driver for the decentrally-hosted external-party workflow.
//!
//! The coordinator holds the party's single Ed25519 key. It generates the key,
//! asks Canton to build the multi-host onboarding topology (naming every host),
//! signs the multi-hash once, authorizes hosting on its own participant, then
//! fans the party-signed bundle out to each hosting peer over Noise
//! (`AllocatePeers`); each peer authorizes hosting on its own participant. The
//! topology stays a proposal until the last host has signed.
//!
//! Each step is idempotent (load-or-create key, deterministic topology,
//! `ALREADY_EXISTS`-tolerant allocate), so a restart re-runs from the first
//! step and converges on the same party.

use std::{sync::Arc, time::Duration};

use anyhow::Context;
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig},
    db::schema::{Commitable, SchemaWrite},
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{WorkflowInstance, peer_status::LastSeen},
    workflow::{
        external_party::{
            ExternalPartyConfig, ExternalPartyStep,
            keys::ExternalKeyPair,
            steps::{ExternalPartyAllocatePayload, allocate_party, prepare_topology},
        },
        state::WorkflowState,
        storage::{WorkflowStorage, artifact_kinds, identity_kinds},
    },
};

/// Run the external-party workflow to completion and return the allocated
/// party id.
///
/// # Errors
/// Returns an error if key generation, topology preparation, signing, party
/// allocation, peer fan-out, or artifact persistence fails.
pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    config: ExternalPartyConfig,
    db: SqlitePool,
    last_seen: LastSeen,
    instance: Arc<WorkflowInstance>,
) -> Result<CantonId> {
    // Build the Noise server (its `WorkflowState` derives the expected-peer set
    // from the persisted run row the HTTP handler already inserted) and register
    // its handle so the always-on listener routes this run's peer traffic to it.
    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        db.clone(),
        config.instance_name.clone(),
        ExternalPartyStep::WaitingForPeers,
        None,
        last_seen,
    )
    .await?;
    let server = Arc::new(server);

    let workflow_state = server.get_workflow_state();
    let node_config_clone = node_config.clone();
    let config_clone = config.clone();
    let db_clone = db.clone();
    let workflow_handle = tokio::spawn(async move {
        run_workflow(workflow_state, node_config_clone, config_clone, db_clone).await
    });

    crate::workflow::run_workflow_with_handler(
        ActiveWorkflow::ExternalParty(server),
        instance,
        workflow_handle,
    )
    .await?;

    let party_id_bytes = db
        .read_artifact(
            &config.instance_name,
            artifact_kinds::EXTERNAL_PARTY_ID,
            None,
        )
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("EXTERNAL_PARTY_ID artifact missing — did PrepareTopology run?")
        })?;
    let party_id_str = String::from_utf8(party_id_bytes).context("Party ID is not valid UTF-8")?;
    let party_id = CantonId::parse(party_id_str.trim())?;

    // Persist the allocated party id on the run row. `workflow_artifacts` are
    // wiped when the run reaches a terminal state (see `set_workflow_run_status`),
    // so the EXTERNAL_PARTY_ID artifact can't be read after completion — the
    // run's durable `dec_party_id` column is what `GET /external-parties` reads.
    let mut tx = db.begin_transaction().await?;
    tx.set_workflow_run_dec_party_id(&config.instance_name, &party_id)
        .await?;
    Commitable::commit(tx).await?;

    // Move the client key out of the transient artifacts (about to be wiped on
    // completion) into the durable identity store so the sovereign party stays
    // recoverable and can later transact. Skipped in the wallet-driven flow —
    // there DPM never holds a seed (the wallet keeps it), so there is nothing to
    // persist.
    if config.prepared_bundle.is_none() {
        persist_external_party_identity(&db, &config.instance_name, &party_id).await?;
    }

    Ok(party_id)
}

/// Copy the external party's key material from the transient `workflow_artifacts`
/// (wiped when the run reaches a terminal state) into the durable, AES-GCM
/// encrypted `dec_party_identity` store, sub-keyed by the party's namespace
/// fingerprint. This is the v0 stand-in for a wallet holding the key: the seed
/// survives onboarding completion so the party remains recoverable and can later
/// transact via interactive submission.
///
/// # Errors
/// Returns an error if the stored fingerprint is not valid UTF-8 or a
/// read/write against the DB fails.
async fn persist_external_party_identity(
    db: &SqlitePool,
    instance_name: &str,
    party_id: &CantonId,
) -> Result<()> {
    let Some(fingerprint) = db
        .read_artifact(
            instance_name,
            artifact_kinds::EXTERNAL_PARTY_FINGERPRINT,
            None,
        )
        .await?
    else {
        // Nothing to copy (key material was never generated) — leave the run to
        // complete without a durable key rather than failing after allocation.
        return Ok(());
    };
    let fingerprint =
        String::from_utf8(fingerprint).context("external-party fingerprint is not valid UTF-8")?;

    for (artifact_kind, identity_kind) in [
        (
            artifact_kinds::EXTERNAL_PARTY_SEED,
            identity_kinds::EXTERNAL_PARTY_SEED,
        ),
        (
            artifact_kinds::EXTERNAL_PARTY_PUBLIC_KEY,
            identity_kinds::EXTERNAL_PARTY_PUBLIC_KEY,
        ),
    ] {
        if let Some(bytes) = db.read_artifact(instance_name, artifact_kind, None).await? {
            db.write_identity(party_id, identity_kind, &fingerprint, &bytes)
                .await?;
        }
    }

    Ok(())
}

/// Load a previously-persisted key seed (idempotent resume) or generate a fresh
/// keypair and persist its public material + seed.
async fn load_or_create_keypair(db: &SqlitePool, instance_name: &str) -> Result<ExternalKeyPair> {
    if let Some(bytes) = db
        .read_artifact(instance_name, artifact_kinds::EXTERNAL_PARTY_SEED, None)
        .await?
        && bytes.len() == 32
    {
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        tracing::info!("external-party: reusing persisted key seed for {instance_name}");
        return Ok(ExternalKeyPair::from_seed(seed));
    }

    let keypair = ExternalKeyPair::generate();
    db.write_artifact(
        instance_name,
        artifact_kinds::EXTERNAL_PARTY_PUBLIC_KEY,
        None,
        &keypair.public_key_bytes(),
    )
    .await?;
    db.write_artifact(
        instance_name,
        artifact_kinds::EXTERNAL_PARTY_FINGERPRINT,
        None,
        keypair.fingerprint().as_bytes(),
    )
    .await?;
    // v0 note: DPM generates and holds the party's Ed25519 key as a stand-in
    // for a wallet. The seed is AES-GCM encrypted at rest in `workflow_artifacts`
    // during the run, then copied into the durable `dec_party_identity` store at
    // completion (see `persist_external_party_identity`). In the production model
    // the wallet — not DPM — owns this key; DPM custody is a v0 simplification.
    tracing::info!(
        "external-party v0: DPM is generating and holding the Ed25519 key for {instance_name} \
         (v0 stand-in for a wallet-held key)"
    );
    db.write_artifact(
        instance_name,
        artifact_kinds::EXTERNAL_PARTY_SEED,
        None,
        &keypair.seed(),
    )
    .await?;
    Ok(keypair)
}

async fn run_workflow(
    workflow_state: Arc<WorkflowState<ExternalPartyStep>>,
    node_config: NodeConfig,
    config: ExternalPartyConfig,
    db: SqlitePool,
) -> Result {
    let instance_name = config.instance_name.clone();
    let mut keypair: Option<ExternalKeyPair> = None;

    loop {
        match workflow_state.current_step().await {
            ExternalPartyStep::WaitingForPeers => {
                // Connection gate: advanced by peer_connected events once every
                // hosting peer is online.
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            ExternalPartyStep::GenerateKeys => {
                if config.prepared_bundle.is_some() {
                    // Wallet-driven: the wallet holds the key, so there is
                    // nothing for the coordinator to generate.
                    tracing::info!(
                        "external-party: wallet-provided key, skipping coordinator key generation"
                    );
                } else {
                    tracing::info!("external-party: generating client-side Ed25519 key");
                    keypair = Some(load_or_create_keypair(&db, &instance_name).await?);
                }
                workflow_state.advance_step().await;
            }
            ExternalPartyStep::PrepareTopology => {
                if let Some(bundle) = config.prepared_bundle.as_ref() {
                    // Wallet-driven: the wallet already generated the key, asked
                    // Canton to build the topology, and signed the multi-hash.
                    // Allocate directly from that bundle on the coordinator's own
                    // participant and fan the same bundle out to the hosts — the
                    // coordinator never touches the key or Canton generate/sign.
                    tracing::info!(
                        "external-party: allocating wallet-signed party on coordinator participant"
                    );
                    db.write_artifact(
                        &instance_name,
                        artifact_kinds::EXTERNAL_PARTY_ID,
                        None,
                        bundle.party_id.as_bytes(),
                    )
                    .await?;
                    allocate_party(&node_config, bundle).await?;
                    let payload = serde_json::to_vec(bundle)
                        .context("serialize external-party allocate bundle")?;
                    workflow_state.set_command_payload(payload).await;
                    workflow_state.advance_step().await;
                } else {
                    let kp = keypair
                        .as_ref()
                        .context("keypair missing before PrepareTopology")?;
                    tracing::info!(
                        "external-party: generating multi-host onboarding topology via Canton"
                    );
                    let prep =
                        prepare_topology(&node_config, &config, &kp.public_key_bytes()).await?;
                    db.write_artifact(
                        &instance_name,
                        artifact_kinds::EXTERNAL_PARTY_ID,
                        None,
                        prep.party_id.as_bytes(),
                    )
                    .await?;
                    db.write_artifact(
                        &instance_name,
                        artifact_kinds::EXTERNAL_PARTY_MULTI_HASH,
                        None,
                        &prep.multi_hash,
                    )
                    .await?;

                    // Party signs the multi-hash once; the coordinator (itself a
                    // host) authorizes hosting on its own participant.
                    let bundle = ExternalPartyAllocatePayload::sign(&prep, kp);
                    tracing::info!(
                        "external-party: authorizing hosting on coordinator participant"
                    );
                    allocate_party(&node_config, &bundle).await?;

                    // Fan the same party-signed bundle out to the hosting peers as
                    // the AllocatePeers command payload.
                    let payload = serde_json::to_vec(&bundle)
                        .context("serialize external-party allocate bundle")?;
                    workflow_state.set_command_payload(payload).await;
                    workflow_state.advance_step().await;
                }
            }
            ExternalPartyStep::AllocatePeers => {
                // Peer-gated: each hosting peer authorizes on its own participant;
                // advanced by peer_completed events once every host has signed.
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            ExternalPartyStep::Complete => {
                tracing::info!("external-party workflow complete for {instance_name}");
                // Keep the Noise handle registered briefly so every hosting peer
                // polls once more and receives the Disconnect (Complete's command)
                // to finish cleanly — otherwise a peer that polls after teardown
                // sees "no active workflow" and fails. Mirrors onboarding.
                tokio::time::sleep(Duration::from_secs(5)).await;
                break;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        db::MIGRATOR,
        server::{WorkflowKind, WorkflowProgress, WorkflowRole, WorkflowRun},
    };

    use super::*;

    // A valid `1220`-prefixed, 68-char namespace fingerprint for the test party.
    const TEST_FINGERPRINT: &str =
        "1220bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn external_party_key_material_survives_completion_wipe(pool: SqlitePool) -> Result {
        let instance = "alice-external";
        let party_id = CantonId::parse(&format!("alice::{TEST_FINGERPRINT}"))?;
        let seed = [7u8; 32];
        let public_key = [9u8; 32];

        // A run row must exist first: workflow_artifacts has a FK to workflow_runs
        // (the real flow inserts the run before any artifact write).
        let run = WorkflowRun {
            instance_name: instance.to_string(),
            kind: WorkflowKind::ExternalParty,
            role: WorkflowRole::Coordinator,
            status: WorkflowProgress::InProgress,
            current_step: "PrepareTopology".to_string(),
            step_index: 2,
            step_total: 5,
            config_json: "{}".to_string(),
            coordinator_pubkey: None,
            coordinator_instance: None,
            coordinator_name: None,
            expected_peers: Vec::new(),
            completed_peers: Vec::new(),
            dec_party_id: None,
            prefix: None,
            participants: Vec::new(),
            previous_threshold: None,
            new_threshold: None,
            kicked_participant: None,
            added_participant: None,
            package_names: Vec::new(),
            dar_filenames: Vec::new(),
            error: None,
            dismissed: false,
            created_at: 0,
            updated_at: 0,
        };
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_workflow_run(&run).await?;
        Commitable::commit(tx).await?;

        // Seed the transient artifacts a live GenerateKeys step would have written.
        pool.write_artifact(instance, artifact_kinds::EXTERNAL_PARTY_SEED, None, &seed)
            .await?;
        pool.write_artifact(
            instance,
            artifact_kinds::EXTERNAL_PARTY_PUBLIC_KEY,
            None,
            &public_key,
        )
        .await?;
        pool.write_artifact(
            instance,
            artifact_kinds::EXTERNAL_PARTY_FINGERPRINT,
            None,
            TEST_FINGERPRINT.as_bytes(),
        )
        .await?;

        persist_external_party_identity(&pool, instance, &party_id).await?;

        // Completing the run wipes `workflow_artifacts` — mirrors the HTTP task's
        // mark_run_completed call after start_coordinator returns.
        let mut tx = pool.begin_transaction().await?;
        tx.set_workflow_run_status(instance, WorkflowProgress::Completed, None, 1_000)
            .await?;
        Commitable::commit(tx).await?;

        // The transient seed artifact is gone...
        assert!(
            pool.read_artifact(instance, artifact_kinds::EXTERNAL_PARTY_SEED, None)
                .await?
                .is_none(),
            "workflow_artifacts must be wiped at completion"
        );

        // ...but the durable identity copy survives and is recoverable, so the
        // sovereign party can be reconstructed after onboarding finishes.
        let recovered_seed = pool
            .read_identity(
                &party_id,
                identity_kinds::EXTERNAL_PARTY_SEED,
                TEST_FINGERPRINT,
            )
            .await?;
        assert_eq!(recovered_seed.as_deref(), Some(&seed[..]));

        let recovered_pub = pool
            .read_identity(
                &party_id,
                identity_kinds::EXTERNAL_PARTY_PUBLIC_KEY,
                TEST_FINGERPRINT,
            )
            .await?;
        assert_eq!(recovered_pub.as_deref(), Some(&public_key[..]));

        Ok(())
    }
}
