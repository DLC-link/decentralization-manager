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
        execute_submissions, prepare_submissions, rotate,
        sign::{encode_messages_length_prefixed, read_all_messages_from_bytes, verified_hashes},
    },
};

/// What the coordinator keeps from `BuildRotation` until it executes. Only in
/// memory: a restart in between announces fresh keys.
struct Round {
    /// The coordinator's one-shot key, until it signs the creates.
    key: Option<OneShotKey>,
    /// The rotation, until it is published.
    rotation: Option<SignedTopologyTransaction>,
    keys: Vec<SigningPublicKey>,
    signers: HashSet<CantonId>,
    /// Each create's hash with the coordinator's signature on it, once
    /// prepared.
    creates: Vec<(PreparedTransactionHash, Signature)>,
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
    let lost = || anyhow::anyhow!("The one-shot round of {instance_name} is gone");

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
            ContractsStep::AnnounceKeys
            | ContractsStep::SignRotation
            | ContractsStep::SignSubmissions => {
                if coordinator_only {
                    workflow_state.advance_step_if(current_step).await;
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            ContractsStep::BuildRotation => {
                tracing::info!("Coordinator executing: Build key rotation");
                let built = build_round(
                    &node_config,
                    &instance_name,
                    &dec_party_id,
                    workflow_state.get_all_peer_data().await,
                )
                .await
                .context("Failed to build the key rotation")?;
                workflow_state.clear_peer_data().await;
                let rotation = built.rotation.as_ref().ok_or_else(lost)?;
                workflow_state
                    .set_command_payload(utils::encode_length_prefixed(&[
                        &serde_json::to_vec(&config)?,
                        &utils::encode_length_prefixed_message(rotation),
                    ]))
                    .await;
                workflow_state
                    .limit_step_to(ContractsStep::SignRotation, built.signers.clone())
                    .await;
                round = Some(built);
                workflow_state.advance_step().await;
            }
            ContractsStep::PublishRotation => {
                tracing::info!("Coordinator executing: Publish key rotation");
                let built = round.as_mut().ok_or_else(lost)?;
                let replies = workflow_state.get_all_peer_data().await;
                workflow_state.clear_peer_data().await;
                publish_round(&node_config, &dec_party_id, built, replies)
                    .await
                    .context("Failed to publish the key rotation")?;
                workflow_state.advance_step().await;
            }
            ContractsStep::PrepareSubmissions => {
                tracing::info!("Coordinator executing: Prepare submissions");
                let built = round.as_mut().ok_or_else(lost)?;
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
                let hashes = verified_hashes(
                    &db,
                    &instance_name,
                    &dec_party_id,
                    &config.committed()?.intents,
                )
                .await?;
                let key = built.key.take().ok_or_else(lost)?;
                let own = key.sign_prepared(&hashes);
                drop(key);
                built.creates = hashes.into_iter().zip(own).collect();
                workflow_state
                    .set_command_payload(
                        load_prepared_submissions_payload(&db, &instance_name, &config).await?,
                    )
                    .await;
                workflow_state
                    .limit_step_to(ContractsStep::SignSubmissions, built.signers.clone())
                    .await;
                workflow_state.advance_step().await;
            }
            ContractsStep::ExecuteSubmissions => {
                tracing::info!("Coordinator executing: Execute submissions");
                // A run that resumed here stored the signatures before its
                // restart; only a run still holding its round has new ones.
                if let Some(built) = round.take() {
                    let replies = workflow_state.get_all_peer_data().await;
                    workflow_state.clear_peer_data().await;
                    store_create_signatures(&db, &instance_name, built, replies).await?;
                }
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

/// Choose the signers, make the coordinator's own one-shot key, and build the
/// rotation and sign it with that key.
///
/// The party's namespace threshold M sets the number of signers: the
/// coordinator and M-1 peers, taken in id order from those that announced a
/// key.
async fn build_round(
    node_config: &NodeConfig,
    instance_name: &str,
    dec_party_id: &CantonId,
    announced: HashMap<CantonId, Vec<u8>>,
) -> Result<Round> {
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
    tracing::info!(
        signers = keys.len(),
        "Built the key rotation for {dec_party_id}; waiting for the chosen peers to sign"
    );
    Ok(Round {
        key: Some(key),
        rotation: Some(rotation),
        keys,
        signers: announced.into_iter().map(|(peer, _)| peer).collect(),
        creates: Vec::new(),
    })
}

/// Merge the signers' copies of the rotation, check that every one-shot key
/// signed it, and publish it.
async fn publish_round(
    node_config: &NodeConfig,
    dec_party_id: &CantonId,
    round: &mut Round,
    replies: HashMap<CantonId, Vec<u8>>,
) -> Result {
    let mut copies = Vec::with_capacity(round.signers.len());
    for signer in &round.signers {
        let payload = replies
            .get(signer)
            .ok_or_else(|| anyhow::anyhow!("{signer} did not sign the rotation"))?;
        copies.push(
            utils::read_first_message_from_bytes(payload)
                .with_context(|| format!("{signer} sent a rotation that does not decode"))?,
        );
    }
    let own = round
        .rotation
        .take()
        .ok_or_else(|| anyhow::anyhow!("The rotation of {dec_party_id} was already published"))?;
    let rotation = rotate::merge_rotations(own, copies)?;
    rotate::check_rotation_signed(&rotation, &round.keys)?;

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

/// Join each signer's signatures on the creates to the coordinator's, check
/// them all, and store one row per create for `execute_submissions`.
async fn store_create_signatures(
    db: &sqlx::SqlitePool,
    instance_name: &str,
    round: Round,
    replies: HashMap<CantonId, Vec<u8>>,
) -> Result {
    let (hashes, own): (Vec<PreparedTransactionHash>, Vec<Signature>) =
        round.creates.into_iter().unzip();
    let mut by_create: Vec<Vec<Signature>> = own.into_iter().map(|s| vec![s]).collect();
    for signer in &round.signers {
        let payload = replies
            .get(signer)
            .ok_or_else(|| anyhow::anyhow!("{signer} did not sign the creates"))?;
        let signatures: Vec<Signature> = read_all_messages_from_bytes(payload)
            .with_context(|| format!("{signer} sent signatures that do not decode"))?;
        anyhow::ensure!(
            signatures.len() == by_create.len(),
            "{signer} signed {signed} creates of {total}",
            signed = signatures.len(),
            total = by_create.len()
        );
        for (on_create, signature) in by_create.iter_mut().zip(signatures) {
            on_create.push(signature);
        }
    }
    rotate::check_create_signatures(&round.keys, &hashes, &by_create)?;

    let ordinals = db
        .list_artifacts(instance_name, artifact_kinds::PREPARED_SUBMISSION)
        .await?;
    anyhow::ensure!(
        ordinals.len() == by_create.len(),
        "{instance_name} holds {stored} prepared submissions for {signed} signed creates",
        stored = ordinals.len(),
        signed = by_create.len()
    );
    for ((ordinal, _), signatures) in ordinals.iter().zip(&by_create) {
        db.write_artifact(
            instance_name,
            artifact_kinds::SUBMISSION_SIGNATURES,
            Some(ordinal),
            &encode_messages_length_prefixed(signatures),
        )
        .await?;
    }
    Ok(())
}

/// Encode what a chosen peer signs in `SignSubmissions`: the contracts config
/// and the prepared submissions. The submissions are keyed by zero-padded
/// ordinal so `list_artifacts` returns them in creation order, which the peer
/// keeps when it stores them on its side.
///
/// Returns `[config_json][files_payload]`, length-prefixed, where each file is
/// `(ordinal, blob)`.
async fn load_prepared_submissions_payload(
    db: &sqlx::SqlitePool,
    instance_name: &str,
    config: &ContractsConfig,
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
        &files_payload,
    ]))
}
