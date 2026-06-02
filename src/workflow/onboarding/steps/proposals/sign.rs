use bytes::{BufMut, BytesMut};
use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::SignedTopologyTransaction,
    topology::admin::v30::{
        SignTransactionsRequest, StoreId, Synchronizer, store_id, synchronizer,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use prost::Message;
use sqlx::SqlitePool;
use tokio::time;

use crate::{
    config::NodeConfig,
    consts::{topology_retry_delay_secs, topology_retry_max_attempts},
    error::Result,
    utils,
    workflow::storage::{WorkflowStorage, artifact_kinds},
};

/// Sign a single topology proposal and persist the result.
///
/// `proposal_data` is the `varint(len)||SignedTopologyTransaction` blob the
/// coordinator sent (matches the original on-disk dns_proto.bin /
/// p2p_proto.bin format). The signed result is persisted as a per-peer
/// artefact under `(instance, kind, self_id)` using the same
/// length-prefixed framing.
async fn sign_proposal(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    output_kind: &str,
    proposal_type: &str,
    proposal_data: &[u8],
) -> Result {
    tracing::info!("Signing {proposal_type} proposal...");

    let node_id = config.participant_id().to_string();
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    tracing::info!("Using {proposal_type} proposal from coordinator payload");
    let transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(proposal_data)?;

    let mut topology_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    // Canton propagates a freshly-generated namespace key into the signing-key
    // store asynchronously — 10-20s on devnet's kubectl-tunneled cluster. A peer
    // that signs the DNS/P2P topology transaction before that completes is
    // rejected with TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE. That is
    // transient propagation lag, not a genuine failure, so retry on it using the
    // same budget the topology-submit steps use (configurable via the
    // DPM_TOPOLOGY_RETRY_* env vars). Any other error fails immediately.
    let max_attempts = topology_retry_max_attempts();
    let retry_delay = time::Duration::from_secs(topology_retry_delay_secs());

    let mut signed = None;
    for attempt in 1..=max_attempts {
        // `SignTransactionsRequest` takes ownership of its fields, so the
        // request is rebuilt on each attempt.
        let request = tonic::Request::new(SignTransactionsRequest {
            transactions: vec![transaction.clone()],
            signed_by: vec![],
            store: Some(StoreId {
                store: Some(store_id::Store::Synchronizer(Synchronizer {
                    kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.clone())),
                })),
            }),
            force_flags: vec![],
        });

        tracing::debug!(
            "Calling SignTransactions RPC for {proposal_type} (attempt {attempt}/{max_attempts})..."
        );
        match topology_client.sign_transactions(request).await {
            Ok(response) => {
                signed = Some(response.into_inner());
                break;
            }
            Err(status)
                if attempt < max_attempts
                    && status
                        .message()
                        .contains("TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY") =>
            {
                tracing::warn!(
                    "{proposal_type} signing key not yet propagated to Canton \
                     (attempt {attempt}/{max_attempts}), retrying in {retry_delay:?}..."
                );
                time::sleep(retry_delay).await;
            }
            Err(status) => return Err(status.into()),
        }
    }

    let response = signed.ok_or_else(|| {
        anyhow::anyhow!(
            "{proposal_type} signing key never propagated to Canton after {max_attempts} attempts"
        )
    })?;

    if response.transactions.is_empty() {
        anyhow::bail!("No signed transaction returned for {proposal_type}");
    }

    // Persist as a single per-peer artefact. Each transaction is written
    // as `varint(len)||proto`; concatenated, this matches the previous on-disk
    // format produced by `write_messages_to_file`.
    let mut buffer = BytesMut::new();
    for tx in &response.transactions {
        let encoded = tx.encode_to_vec();
        prost::encoding::encode_varint(encoded.len() as u64, &mut buffer);
        buffer.put_slice(&encoded);
    }

    storage
        .write_artifact(instance_name, output_kind, Some(&node_id), &buffer)
        .await?;

    tracing::info!("{proposal_type} proposal signed successfully");
    Ok(())
}

/// Sign DNS proposal with peer's key
///
/// This step must be run by each peer participant (except the coordinator who created the proposal).
/// Each peer signs the DNS proposal with their namespace key.
pub async fn sign_dns_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    proposal_data: &[u8],
) -> Result {
    sign_proposal(
        config,
        storage,
        instance_name,
        artifact_kinds::SIGNED_DNS_PROPOSAL,
        "DNS",
        proposal_data,
    )
    .await
}

/// Sign P2P proposals with peer's key
///
/// **Canton 3.4+**: Signing keys are now embedded in the P2P mapping.
/// This function signs the P2P proposal which contains both participant and key information.
pub async fn sign_p2p_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    proposal_data: &[u8],
) -> Result {
    sign_proposal(
        config,
        storage,
        instance_name,
        artifact_kinds::SIGNED_P2P_PROPOSAL,
        "P2P",
        proposal_data,
    )
    .await
}
