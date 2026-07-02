use canton_proto_rs::com::digitalasset::canton::topology::admin::v30::{
    BaseQuery, ListDecentralizedNamespaceDefinitionRequest, StoreId, Synchronizer, base_query,
    store_id, synchronizer, topology_manager_read_service_client::TopologyManagerReadServiceClient,
};
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{
        change_threshold::ChangeThresholdConfig,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// Export the current decentralized namespace state and validate the requested
/// threshold against the live owner set.
///
/// Saves the current `DecentralizedNamespaceDefinition`
/// (`CHANGE_THRESHOLD_NAMESPACE_DEF`) for `CreateProposals` to rewrite. Unlike
/// the kick workflow there is no member to remove — the owner/participant sets
/// are preserved and only the threshold changes.
pub async fn export_state(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    change_config: &ChangeThresholdConfig,
) -> Result {
    tracing::info!("Exporting current decentralized namespace state...");

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    let namespace_hex = change_config.decentralized_party_id.namespace.to_hex();
    tracing::info!("Querying namespace: {namespace_hex}");

    let mut topology_read_client =
        TopologyManagerReadServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(ListDecentralizedNamespaceDefinitionRequest {
        base_query: Some(BaseQuery {
            store: Some(StoreId {
                store: Some(store_id::Store::Synchronizer(Synchronizer {
                    kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.clone())),
                })),
            }),
            proposals: false,
            operation: 0,
            time_query: Some(base_query::TimeQuery::HeadState(())),
            filter_signed_key: String::new(),
            protocol_version: None,
        }),
        filter_namespace: namespace_hex.clone(),
    });

    let response = topology_read_client
        .list_decentralized_namespace_definition(request)
        .await?
        .into_inner();

    if response.results.is_empty() {
        anyhow::bail!("Namespace {namespace_hex} not found in topology");
    }

    let namespace_def = response
        .results
        .first()
        .and_then(|r| r.item.as_ref())
        .ok_or_else(|| anyhow::anyhow!("Namespace definition missing from response"))?;

    let owner_count = namespace_def.owners.len();
    tracing::info!(
        "Found namespace with {owner_count} owners, threshold {threshold}",
        threshold = namespace_def.threshold
    );

    // Validate the requested threshold against the *live* owner set. The HTTP
    // handler bounds it against cached membership, but the topology is the
    // source of truth — a threshold above the owner count can never be
    // satisfied and would leave a stuck proposal.
    if change_config.new_threshold < 1 || change_config.new_threshold > owner_count as i32 {
        anyhow::bail!(
            "new_threshold must be between 1 and {owner_count} (current namespace owner count); \
             got {got}",
            got = change_config.new_threshold,
        );
    }

    if change_config.new_threshold == namespace_def.threshold {
        anyhow::bail!(
            "new_threshold {t} is already the current threshold — nothing to change",
            t = change_config.new_threshold,
        );
    }

    // Save namespace definition as a length-prefixed protobuf — same byte
    // shape as the file written by `utils::write_message_to_file`.
    let namespace_bytes = utils::encode_length_prefixed_message(namespace_def);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::CHANGE_THRESHOLD_NAMESPACE_DEF,
            None,
            &namespace_bytes,
        )
        .await?;
    tracing::info!("Saved namespace definition to storage");

    tracing::info!("State exported successfully to workflow storage");
    Ok(())
}
