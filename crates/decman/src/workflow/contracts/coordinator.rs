use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{Signature, SigningPublicKey},
    protocol::v30::SignedTopologyTransaction,
};

use crate::{
    auth::WorkflowAuth,
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig},
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{WorkflowInstance, peer_status::LastSeen},
    signing::{
        PreparedTransactionHash,
        one_shot::{self, OneShotKey},
    },
    utils,
    workflow::{
        state::WorkflowState,
        storage::{WorkflowStorage, artifact_kinds},
        topology,
    },
};

use super::{
    ContractsConfig, ContractsStep,
    steps::{
        execute_submissions, prepare_submissions,
        rotate::{self, SignerReply},
        sign::{encode_messages_length_prefixed, verified_hashes},
    },
};

/// What the coordinator keeps from `BuildRotation` until `PublishRotation`.
/// Only in memory: a restart in between announces fresh keys.
struct Round {
    rotation: SignedTopologyTransaction,
    keys: Vec<SigningPublicKey>,
    hashes: Vec<PreparedTransactionHash>,
    /// The coordinator's signature on each create.
    daml: Vec<Signature>,
    signers: HashSet<CantonId>,
}

pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    config: ContractsConfig,
    workflow_auth: Option<WorkflowAuth>,
    db: sqlx::SqlitePool,
    last_seen: LastSeen,
    instance: Arc<WorkflowInstance>,
) -> Result {
    tracing::info!("Initializing Noise server...");

    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        db.clone(),
        config.instance_name.clone(),
        ContractsStep::WaitingForPeers,
        None, // No excluded participants
        last_seen,
    )
    .await?;
    let server = Arc::new(server);

    tracing::info!("Noise server initialized, listening for connections");

    let workflow_state = server.get_workflow_state();
    let node_config_clone = node_config.clone();
    let db_clone = db.clone();
    let workflow_handle = tokio::spawn(async move {
        run_workflow(
            workflow_state,
            node_config_clone,
            db_clone,
            config,
            workflow_auth,
        )
        .await
    });

    crate::workflow::run_workflow_with_handler(
        ActiveWorkflow::Contracts(server),
        instance,
        workflow_handle,
    )
    .await
}

async fn run_workflow(
    workflow_state: Arc<WorkflowState<ContractsStep>>,
    node_config: NodeConfig,
    db: sqlx::SqlitePool,
    config: ContractsConfig,
    workflow_auth: Option<WorkflowAuth>,
) -> Result {
    let instance_name = config.instance_name.clone();
    let dec_party_id = config.decentralized_party_id.clone();

    // 1-of-N party: the coordinator's own signature already meets the threshold,
    // so no peer signatures are required. The peer gates (WaitingForPeers /
    // SignSubmissions) would otherwise wait for a peer that may never connect;
    // the coordinator advances them itself below. Any other threshold (or an
    // unresolved one) falls back to the normal peer wait.
    let coordinator_only =
        crate::noise::server::lookup_party_threshold(&db, &dec_party_id).await == Some(1);

    // Auth handle for the decentralized party. We deliberately fetch a fresh
    // token at each ledger-touching step rather than caching one snapshot up
    // front: a workflow can sit in `WaitingForPeers` for an arbitrarily long
    // time, and a token captured before that wait would be expired by the time
    // Prepare/Execute run automatically once peers accept. `get_credentials`
    // returns a cached-with-refresh token, so per-step calls are cheap.
    let auth = workflow_auth
        .ok_or_else(|| anyhow::anyhow!("Auth not configured, cannot run contracts workflow"))?;

    // The peers' one-shot keys reached this process only in memory. A run
    // that resumes at AnnounceKeys therefore asks every peer again.
    if workflow_state.current_step().await == ContractsStep::AnnounceKeys {
        workflow_state.restart_step().await;
    }
    let mut round: Option<Round> = None;

    loop {
        let current_step = workflow_state.current_step().await;

        match current_step {
            ContractsStep::WaitingForPeers => {
                if coordinator_only {
                    workflow_state
                        .advance_step_if(ContractsStep::WaitingForPeers)
                        .await;
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            ContractsStep::PrepareSubmissions => {
                tracing::info!("Coordinator executing: Prepare submissions");
                let creds = auth.get_credentials(&config.decentralized_party_id).await?;
                prepare_submissions(
                    &node_config,
                    &db,
                    &instance_name,
                    &config,
                    &creds.token,
                    &creds.user_id,
                )
                .await?;
                workflow_state.clear_command_payload().await;
                workflow_state.advance_step().await;
            }
            ContractsStep::AnnounceKeys | ContractsStep::SignSubmissions => {
                if coordinator_only {
                    workflow_state.advance_step_if(current_step).await;
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            ContractsStep::BuildRotation => {
                tracing::info!("Coordinator executing: Build key rotation");
                let built = build_round(
                    &node_config,
                    &db,
                    &instance_name,
                    &config,
                    workflow_state.get_all_peer_data().await,
                )
                .await
                .context("Failed to build the key rotation")?;
                workflow_state.clear_peer_data().await;
                workflow_state
                    .set_command_payload(
                        load_signing_payload(&db, &instance_name, &config, &built.rotation).await?,
                    )
                    .await;
                workflow_state
                    .limit_step_to(ContractsStep::SignSubmissions, built.signers.clone())
                    .await;
                round = Some(built);
                workflow_state.advance_step().await;
            }
            ContractsStep::PublishRotation => {
                tracing::info!("Coordinator executing: Publish key rotation");
                let built = round.take().ok_or_else(|| {
                    anyhow::anyhow!("PublishRotation reached without a built rotation")
                })?;
                let replies = workflow_state.get_all_peer_data().await;
                workflow_state.clear_peer_data().await;
                publish_round(
                    &node_config,
                    &db,
                    &instance_name,
                    &dec_party_id,
                    built,
                    replies,
                )
                .await
                .context("Failed to publish the key rotation")?;
                workflow_state.advance_step().await;
            }
            ContractsStep::ExecuteSubmissions => {
                tracing::info!("Coordinator executing: Execute submissions");
                let creds = auth.get_credentials(&config.decentralized_party_id).await?;
                execute_submissions(
                    &node_config,
                    &db,
                    &instance_name,
                    &config,
                    &creds.token,
                    &creds.user_id,
                )
                .await?;
                workflow_state.advance_step().await;
            }
            ContractsStep::Complete => {
                tracing::info!("Contracts workflow complete!");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                break;
            }
        }
    }

    Ok(())
}

/// Choose the signers, make the coordinator's own one-shot key, and build and
/// sign the rotation and the creates with it.
///
/// The party's namespace threshold M sets the number of signers: the
/// coordinator and M-1 peers, taken in id order from those that announced a
/// key. The coordinator's key is dropped before this returns.
async fn build_round(
    node_config: &NodeConfig,
    db: &sqlx::SqlitePool,
    instance_name: &str,
    config: &ContractsConfig,
    announced: HashMap<CantonId, Vec<u8>>,
) -> Result<Round> {
    let dec_party_id = &config.decentralized_party_id;
    let synchronizer_id = utils::get_synchronizer_id(node_config).await?;
    let (_, namespace) = topology::fetch_namespace_definition_at_head(
        node_config,
        &synchronizer_id,
        &dec_party_id.namespace.to_hex(),
    )
    .await?;
    let needed = usize::try_from(namespace.threshold)?.saturating_sub(1);

    let mut announced: Vec<(CantonId, SigningPublicKey)> = announced
        .into_iter()
        .map(|(peer, payload)| {
            let key: SigningPublicKey = prost::Message::decode(payload.as_slice())?;
            one_shot::protocol_verifying_key(&key)
                .with_context(|| format!("{peer} announced an unusable key"))?;
            Ok((peer, key))
        })
        .collect::<Result<_>>()?;
    announced.sort_by(|a, b| a.0.cmp(&b.0));
    anyhow::ensure!(
        announced.len() >= needed,
        "{count} peer(s) announced a one-shot key, but the namespace threshold needs {needed}",
        count = announced.len()
    );
    announced.truncate(needed);

    let hashes = verified_hashes(
        db,
        instance_name,
        dec_party_id,
        &config.committed()?.intents,
    )
    .await?;
    let key = OneShotKey::generate();
    let keys: Vec<SigningPublicKey> = std::iter::once(key.public_key().clone())
        .chain(announced.iter().map(|(_, k)| k.clone()))
        .collect();
    let mut rotation = rotate::build_rotation(
        node_config,
        &synchronizer_id,
        instance_name,
        dec_party_id,
        keys.clone(),
    )
    .await?;
    rotate::check_rotation(
        node_config,
        &synchronizer_id,
        dec_party_id,
        &rotation,
        key.fingerprint(),
    )
    .await?;
    rotation
        .signatures
        .push(key.sign_topology(&rotation.transaction));
    let daml = key.sign_prepared(&hashes);
    tracing::info!(
        signers = keys.len(),
        "Built the key rotation for {dec_party_id}; waiting for the chosen peers to sign"
    );
    Ok(Round {
        rotation,
        keys,
        hashes,
        daml,
        signers: announced.into_iter().map(|(peer, _)| peer).collect(),
    })
}

/// Merge the signers' replies, check every signature, store each create's
/// signatures for `ExecuteSubmissions`, and publish the rotation.
async fn publish_round(
    node_config: &NodeConfig,
    db: &sqlx::SqlitePool,
    instance_name: &str,
    dec_party_id: &CantonId,
    round: Round,
    replies: HashMap<CantonId, Vec<u8>>,
) -> Result {
    let mut copies = Vec::with_capacity(round.signers.len());
    let mut by_create: Vec<Vec<Signature>> = round.daml.into_iter().map(|s| vec![s]).collect();
    for signer in &round.signers {
        let payload = replies
            .get(signer)
            .ok_or_else(|| anyhow::anyhow!("{signer} sent no signatures"))?;
        let reply = SignerReply::decode(payload)
            .with_context(|| format!("{signer} sent signatures that do not decode"))?;
        anyhow::ensure!(
            reply.daml.len() == by_create.len(),
            "{signer} signed {signed} creates of {total}",
            signed = reply.daml.len(),
            total = by_create.len()
        );
        for (signatures, signature) in by_create.iter_mut().zip(reply.daml) {
            signatures.push(signature);
        }
        copies.push(reply.rotation);
    }
    let rotation = rotate::merge_rotations(round.rotation, copies)?;
    rotate::check_signatures(&rotation, &round.keys, &round.hashes, &by_create)?;

    let ordinals = db
        .list_artifacts(instance_name, artifact_kinds::PREPARED_SUBMISSION)
        .await?;
    for ((ordinal, _), signatures) in ordinals.iter().zip(&by_create) {
        db.write_artifact(
            instance_name,
            artifact_kinds::SUBMISSION_SIGNATURES,
            Some(ordinal),
            &encode_messages_length_prefixed(signatures),
        )
        .await?;
    }

    let synchronizer_id = utils::get_synchronizer_id(node_config).await?;
    rotate::publish_rotation(
        node_config,
        &synchronizer_id,
        dec_party_id,
        rotation,
        &round.keys,
    )
    .await
}

/// Encode what a chosen peer signs: the contracts config, the rotation, and
/// the prepared submissions. The submissions are keyed by zero-padded ordinal
/// so `list_artifacts` returns them in creation order, which the peer keeps
/// when it stores them on its side.
///
/// Returns `[config_json][rotation][files_payload]`, length-prefixed, where
/// each file is `(ordinal, blob)`.
async fn load_signing_payload(
    db: &sqlx::SqlitePool,
    instance_name: &str,
    config: &ContractsConfig,
    rotation: &SignedTopologyTransaction,
) -> Result<Vec<u8>> {
    tracing::info!("Loading prepared submissions from storage for distribution");

    let submission_rows = db
        .list_artifacts(instance_name, artifact_kinds::PREPARED_SUBMISSION)
        .await?;

    if submission_rows.is_empty() {
        anyhow::bail!("No PREPARED_SUBMISSION artifacts found for instance {instance_name}");
    }

    tracing::info!(
        "Loaded {count} prepared submission artefact(s) for distribution to peers",
        count = submission_rows.len()
    );

    let config_data = serde_json::to_vec(config).context("Failed to serialize contracts config")?;
    let files_payload = utils::encode_files(&submission_rows);
    Ok(utils::encode_length_prefixed(&[
        &config_data,
        &utils::encode_length_prefixed_message(rotation),
        &files_payload,
    ]))
}
