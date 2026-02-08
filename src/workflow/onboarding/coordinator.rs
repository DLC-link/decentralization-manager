use std::{collections::HashSet, sync::Arc};

use anyhow::Context;

use crate::{
    config::{NetworkConfig, NodeConfig},
    consts::{
        ATTESTOR_KEYS_PREFIX, PARTICIPANT_ID_PREFIX, SIGNED_DNS_PROPOSAL_PREFIX,
        SIGNED_P2P_PROPOSALS_PREFIX,
    },
    error::Result,
    noise::server::NoiseServer,
    utils,
    workflow::state::WorkflowState,
};

use super::{
    OnboardingConfig, OnboardingDirs, OnboardingStep,
    steps::{create_proposals, generate_keys, submit_dns_proposals, submit_final_proposals},
};

/// Decode combined keys+id payload and save to separate directories
async fn save_keys_and_ids<S: crate::workflow::state::WorkflowStep + 'static>(
    workflow_state: &WorkflowState<S>,
    dirs: &OnboardingDirs,
) -> Result {
    let attestor_data = workflow_state.get_all_attestor_data().await;

    for (attestor_id, data) in attestor_data {
        let items = utils::decode_length_prefixed(&data, 2)
            .with_context(|| format!("Invalid payload from {attestor_id}"))?;

        let keys_data = &items[0];
        let id_data = &items[1];

        tracing::debug!(
            "Received from {attestor_id}: {keys_len} bytes keys + {id_len} bytes participant ID",
            keys_len = keys_data.len(),
            id_len = id_data.len()
        );

        // Save keys file
        let keys_path = dirs
            .keys_dir
            .join(format!("{ATTESTOR_KEYS_PREFIX}-{attestor_id}.bin"));
        tokio::fs::write(&keys_path, keys_data)
            .await
            .with_context(|| {
                format!(
                    "Failed to write keys to '{path}'",
                    path = keys_path.display()
                )
            })?;

        // Save participant ID file
        let id_path = dirs
            .ids_dir
            .join(format!("{PARTICIPANT_ID_PREFIX}-{attestor_id}.bin"));
        tokio::fs::write(&id_path, id_data).await.with_context(|| {
            format!(
                "Failed to write participant ID to '{path}'",
                path = id_path.display()
            )
        })?;
    }

    workflow_state.clear_attestor_data().await;
    Ok(())
}

pub async fn start_coordinator(
    node_config: NodeConfig,
    network_config: NetworkConfig,
    onboarding_config: OnboardingConfig,
) -> Result {
    tracing::info!("Initializing Noise server...");

    // Compute excluded peers: all peers not in peer_ids
    let peer_ids_set: HashSet<String> = onboarding_config.peer_ids.iter().cloned().collect();
    let excluded: Vec<String> = network_config
        .peers
        .iter()
        .map(|p| p.participant_id.to_string())
        .filter(|id| *id != node_config.participant_id().to_string() && !peer_ids_set.contains(id))
        .collect();

    let excluded = if excluded.is_empty() {
        None
    } else {
        Some(excluded)
    };

    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        OnboardingStep::WaitingForAttestors,
        excluded,
    )
    .await?;
    let server = Arc::new(server);

    let dirs = OnboardingDirs::with_base(
        node_config.workflow_data_dir(),
        &onboarding_config.instance_name,
    );
    dirs.create_dirs().await?;

    tracing::info!("Noise server initialized, listening for connections");

    let workflow_state = server.get_workflow_state();
    let node_config_clone = node_config.clone();
    let onboarding_config_clone = onboarding_config.clone();
    let dirs_clone = dirs.clone();
    let workflow_handle = tokio::spawn(async move {
        run_workflow(
            workflow_state,
            node_config_clone,
            onboarding_config_clone,
            dirs_clone,
        )
        .await
    });

    crate::workflow::run_server_with_workflow(server, workflow_handle).await
}

async fn run_workflow(
    workflow_state: Arc<WorkflowState<OnboardingStep>>,
    node_config: NodeConfig,
    onboarding_config: OnboardingConfig,
    dirs: OnboardingDirs,
) -> Result {
    let mut coordinator_completed_steps = HashSet::new();

    // Set the onboarding config as payload for GenerateKeys step
    let config_payload =
        serde_json::to_vec(&onboarding_config).context("Failed to serialize onboarding config")?;
    workflow_state.set_command_payload(config_payload).await;

    loop {
        let current_step = workflow_state.current_step().await;

        match current_step {
            OnboardingStep::WaitingForAttestors => {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            OnboardingStep::GenerateKeys => {
                if !coordinator_completed_steps.contains(&OnboardingStep::GenerateKeys) {
                    tracing::info!("Coordinator executing: Generate keys");
                    generate_keys(&node_config, &dirs, &onboarding_config).await?;
                    coordinator_completed_steps.insert(OnboardingStep::GenerateKeys);

                    tracing::info!(
                        "Waiting 3 seconds for Canton to process namespace delegations..."
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            OnboardingStep::CreateProposals => {
                tracing::info!("Coordinator executing: Create proposals");
                // Save attestor keys and participant IDs uploaded during GenerateKeys step
                save_keys_and_ids(&workflow_state, &dirs).await?;
                create_proposals(&node_config, &dirs, &onboarding_config).await?;

                // Load DNS proposal to send to attestors with SignDns command
                let dns_proposal_path = dirs.dns_proposals_dir.join("dns_proto.bin");
                let dns_proposal_data = tokio::fs::read(&dns_proposal_path).await?;
                workflow_state.set_command_payload(dns_proposal_data).await;

                workflow_state.advance_step().await;
            }
            OnboardingStep::SignDns => {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            OnboardingStep::SubmitDns => {
                tracing::info!("Coordinator executing: Submit DNS proposals");
                crate::workflow::save_attestor_data(
                    &workflow_state,
                    &dirs.dns_signed_dir,
                    SIGNED_DNS_PROPOSAL_PREFIX,
                )
                .await?;
                submit_dns_proposals(&node_config, &dirs).await?;

                // Load P2P proposal to send to attestors with SignP2p command
                let p2p_proposal_path = dirs.p2p_proposals_dir.join("p2p_proto.bin");
                let p2p_proposal_data = tokio::fs::read(&p2p_proposal_path).await?;
                workflow_state.set_command_payload(p2p_proposal_data).await;

                workflow_state.advance_step().await;
            }
            OnboardingStep::SignP2p => {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            OnboardingStep::SubmitFinal => {
                tracing::info!("Coordinator executing: Submit final proposals");
                crate::workflow::save_attestor_data(
                    &workflow_state,
                    &dirs.final_signed_dir,
                    SIGNED_P2P_PROPOSALS_PREFIX,
                )
                .await?;
                submit_final_proposals(&node_config, &dirs, &onboarding_config).await?;
                workflow_state.advance_step().await;
            }
            OnboardingStep::Complete => {
                tracing::info!("Onboarding workflow complete!");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                break;
            }
        }
    }

    Ok(())
}
