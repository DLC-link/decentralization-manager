use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{SignedTopologyTransaction, topology_mapping},
    topology::admin::v30::{
        SignTransactionsRequest,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{
        add_party::{AddPartyConfig, steps::proposals::create::proposal_request},
        party_replication::{has_onboarding_marker, wait_for_flag_cleared},
        storage::{WorkflowStorage, artifact_kinds},
        topology::{
            add_transactions_request, authorize_with_topology_retry, dedupe_signatures,
            fetch_p2p_mapping, sign_transactions_with_topology_retry, synchronizer_store_id,
        },
    },
};

/// Hard cap on how long the new member waits for Canton's "earliest safe
/// time to clear the onboarding flag" to arrive. The safe time is normally
/// seconds away (decision timeouts); ten minutes flags a genuinely stuck
/// synchronizer instead of hanging the workflow forever.
/// New-member side: author the clearing proposal and return it encoded as
/// the `varint(len)||proto` blob the coordinator persists/ships. `None`
/// when the flag is already gone.
pub async fn author_clear_proposal(
    config: &NodeConfig,
    add_party_config: &AddPartyConfig,
) -> Result<Option<Vec<u8>>> {
    Ok(create_clear_proposal(config, add_party_config)
        .await?
        .map(|transaction| utils::encode_length_prefixed_message(&transaction)))
}

/// Build the onboarding-flag clearing proposal — the current P2P mapping
/// with the new member's `Onboarding` marker removed. MUST run on the new
/// member: Canton requires the onboarding participant itself to issue the
/// flag-clear transaction (the coordinator's authorize fails with
/// TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE — observed live). Returns
/// `None` when the flag is already gone from head state.
pub async fn create_clear_proposal(
    config: &NodeConfig,
    add_party_config: &AddPartyConfig,
) -> Result<Option<SignedTopologyTransaction>> {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let party_id = &add_party_config.decentralized_party_id;
    let new_member = add_party_config.new_participant_id.to_string();

    let current_p2p = fetch_p2p_mapping(config, &synchronizer_id, party_id).await?;
    if !has_onboarding_marker(&current_p2p, &new_member) {
        tracing::info!("Onboarding flag already cleared in head state — skipping sign round");
        return Ok(None);
    }

    let mut cleared_p2p = current_p2p;
    for participant in &mut cleared_p2p.participants {
        if participant.participant_uid == new_member {
            participant.onboarding = None;
        }
    }

    tracing::info!("Creating onboarding-flag clearing proposal...");
    let response = authorize_with_topology_retry(
        config,
        proposal_request(
            &synchronizer_id,
            topology_mapping::Mapping::PartyToParticipant(cleared_p2p),
        ),
        "add-party-clear",
    )
    .await?;

    response
        .transaction
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("No clearing transaction returned"))
}

/// All-peer step: sign the clearing proposal. `proposal_data` is the single
/// `varint(len)||proto` blob from the coordinator (config stripped by the
/// peer loop). Persists the per-peer `SIGNED_ADD_PARTY_CLEAR` artefact.
pub async fn sign_clear_proposal(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    proposal_data: &[u8],
) -> Result {
    tracing::info!("Signing onboarding-flag clearing proposal...");

    let node_id = config.participant_id().to_string();
    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    let transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(proposal_data)?;

    let request = SignTransactionsRequest {
        transactions: vec![transaction],
        signed_by: vec![],
        store: Some(synchronizer_store_id(&synchronizer_id)),
        force_flags: vec![],
    };

    let response =
        sign_transactions_with_topology_retry(config, request, "add-party-clear").await?;
    if response.transactions.len() != 1 {
        anyhow::bail!(
            "Expected 1 signed clearing transaction, got {count}",
            count = response.transactions.len()
        );
    }

    storage
        .write_artifact(
            instance_name,
            artifact_kinds::SIGNED_ADD_PARTY_CLEAR,
            Some(&node_id),
            &utils::encode_length_prefixed_message(&response.transactions[0]),
        )
        .await?;

    tracing::info!("Clearing proposal signed successfully");
    Ok(())
}

/// Coordinator: aggregate the peers' signatures onto the clearing proposal,
/// submit it, and wait for the onboarding marker to disappear from head
/// state.
pub async fn submit_clear_proposal(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    add_party_config: &AddPartyConfig,
) -> Result {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    let proposal_bytes = storage
        .read_artifact(
            instance_name,
            artifact_kinds::ADD_PARTY_CLEAR_PROPOSAL,
            None,
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("ADD_PARTY_CLEAR_PROPOSAL artifact missing"))?;
    let mut transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(&proposal_bytes)?;

    let signed = storage
        .list_artifacts(instance_name, artifact_kinds::SIGNED_ADD_PARTY_CLEAR)
        .await?;
    tracing::info!(
        "Aggregating clearing-proposal signatures from {count} peer(s)",
        count = signed.len()
    );
    for (peer_id, signed_bytes) in &signed {
        let peer_signed: SignedTopologyTransaction =
            utils::read_first_message_from_bytes(signed_bytes)?;
        tracing::debug!("Adding clearing signatures from {peer_id}");
        transaction.signatures.extend(peer_signed.signatures);
    }

    // The clearing proposal was AUTHORED BY THE NEW MEMBER, so — unlike the
    // add proposals, which the coordinator authors and thus self-signs — it
    // does not yet carry the coordinator's signature. The coordinator is a
    // namespace owner whose signature counts toward the authorization
    // threshold; without it, a party configured with `new_threshold` equal to
    // the owner count could never clear the flag (and every lower threshold
    // loses one otherwise-eligible signer). Add the coordinator's signature.
    let self_signed = sign_transactions_with_topology_retry(
        config,
        SignTransactionsRequest {
            transactions: vec![transaction.clone()],
            signed_by: vec![],
            store: Some(synchronizer_store_id(&synchronizer_id)),
            force_flags: vec![],
        },
        "add-party-clear coordinator",
    )
    .await?;
    if let Some(coordinator_signed) = self_signed.transactions.into_iter().next() {
        transaction.signatures.extend(coordinator_signed.signatures);
    }

    dedupe_signatures(&mut transaction);

    let mut topology_write_client =
        TopologyManagerWriteServiceClient::new(config.admin_channel().await?);
    tracing::info!("Submitting onboarding-flag clearing transaction...");
    topology_write_client
        .add_transactions(tonic::Request::new(add_transactions_request(
            &synchronizer_id,
            transaction,
        )))
        .await?;

    wait_for_flag_cleared(
        config,
        &synchronizer_id,
        &add_party_config.decentralized_party_id,
        &add_party_config.new_participant_id,
    )
    .await
}
