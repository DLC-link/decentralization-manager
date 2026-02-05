use std::{collections::HashSet, sync::Arc};

use anyhow::Context;

use crate::{
    config::{NetworkConfig, NodeConfig},
    consts::{
        ATTESTOR_KEYS_PREFIX, DNS_ADD_PARTY_PROTO_FILENAME, P2P_ADD_PARTY_PROTO_FILENAME,
        PARTICIPANT_ID_PREFIX, SIGNED_ADD_PARTY_PROPOSALS_PREFIX,
    },
    error::Result,
    noise::server::NoiseServer,
    utils,
    workflow::state::WorkflowState,
};

use super::{
    AddPartyConfig, AddPartyDirs, AddPartyStep,
    steps::{create_proposals, export_state, submit_add_party},
};

/// Decode combined keys+id payload and save to separate directories
/// This mirrors the onboarding workflow's save_keys_and_ids function
async fn save_keys_and_ids<S: crate::workflow::state::WorkflowStep + 'static>(
    workflow_state: &WorkflowState<S>,
    dirs: &AddPartyDirs,
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
    add_party_config: AddPartyConfig,
) -> Result {
    tracing::info!("Initializing Noise server for add party workflow...");

    // All existing members plus the new member participate
    // No exclusions needed since we want everyone to join
    let server = NoiseServer::new(
        node_config.clone(),
        network_config.clone(),
        AddPartyStep::WaitingForAttestors,
        None, // No excluded participants
    )
    .await?;

    let workflow_state = server.get_workflow_state();
    let server = Arc::new(server);

    let dirs = AddPartyDirs::with_base(
        node_config.workflow_data_dir(),
        &add_party_config.instance_name,
    );
    dirs.create_dirs().await?;

    // Set the add party config as payload for GenerateNewMemberKeys step
    // This must be done BEFORE the workflow loop to avoid race conditions
    let config_payload =
        serde_json::to_vec(&add_party_config).context("Failed to serialize add party config")?;
    workflow_state.set_command_payload(config_payload).await;

    let coordinator_workflow = {
        let workflow_state = workflow_state.clone();
        let node_config = node_config.clone();
        let add_party_config = add_party_config.clone();
        let dirs = dirs.clone();

        tokio::spawn(async move {
            let mut coordinator_completed_steps = HashSet::new();

            loop {
                let current_step = workflow_state.current_step().await;
                tracing::debug!("Coordinator in step: {current_step:?}");

                match current_step {
                    AddPartyStep::WaitingForAttestors => {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    AddPartyStep::GenerateNewMemberKeys => {
                        // This step requires attestors (specifically the new member) to generate keys
                        // Config payload was already set before the loop started
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    AddPartyStep::ExportState => {
                        if !coordinator_completed_steps.contains(&AddPartyStep::ExportState) {
                            tracing::info!("Coordinator executing: Export state");

                            // Save new member's keys and participant IDs from attestor data
                            // This decodes the length-prefixed format and saves to separate files
                            save_keys_and_ids(&workflow_state, &dirs).await?;

                            export_state(&node_config, &dirs, &add_party_config).await?;
                            coordinator_completed_steps.insert(AddPartyStep::ExportState);
                            workflow_state.advance_step().await;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    AddPartyStep::CreateProposals => {
                        tracing::info!("Coordinator executing: Create proposals");
                        create_proposals(&node_config, &dirs, &add_party_config).await?;

                        // Load add party proposals to send to existing members with SignAddParty command
                        // Combine add party config + DNS and P2P proposals into a single payload
                        let dns_add_path = dirs
                            .add_party_proposals_dir
                            .join(DNS_ADD_PARTY_PROTO_FILENAME);
                        let p2p_add_path = dirs
                            .add_party_proposals_dir
                            .join(P2P_ADD_PARTY_PROTO_FILENAME);

                        let config_data = serde_json::to_vec(&add_party_config)
                            .context("Failed to serialize add party config")?;
                        let dns_data = tokio::fs::read(&dns_add_path).await?;
                        let p2p_data = tokio::fs::read(&p2p_add_path).await?;

                        let payload =
                            utils::encode_length_prefixed(&[&config_data, &dns_data, &p2p_data]);
                        workflow_state.set_command_payload(payload).await;
                        workflow_state.advance_step().await;
                    }
                    AddPartyStep::SignProposals => {
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    AddPartyStep::SubmitAddParty => {
                        tracing::info!("Coordinator executing: Submit add party");
                        crate::workflow::save_attestor_data(
                            &workflow_state,
                            &dirs.add_party_signed_dir,
                            SIGNED_ADD_PARTY_PROPOSALS_PREFIX,
                        )
                        .await?;
                        submit_add_party(&node_config, &dirs).await?;
                        workflow_state.advance_step().await;
                    }
                    AddPartyStep::Complete => {
                        tracing::info!("Add party workflow complete!");
                        tracing::debug!("Waiting for attestors to receive Disconnect command...");
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                        break;
                    }
                }
            }

            Ok(())
        })
    };

    crate::workflow::run_server_with_workflow(server, coordinator_workflow).await
}
