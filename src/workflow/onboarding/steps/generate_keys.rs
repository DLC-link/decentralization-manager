use std::path::Path;

use canton_proto_rs::com::digitalasset::canton::{
    crypto::{
        admin::v30::{GenerateSigningKeyRequest, vault_service_client::VaultServiceClient},
        v30::{SigningKeySpec, SigningKeyUsage, SigningPublicKey},
    },
    protocol::v30::{
        NamespaceDelegation, TopologyMapping, enums::TopologyChangeOp, namespace_delegation,
        topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, StoreId, authorize_request, store_id,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};

use crate::{
    config::{NetworkConfig, NodeConfig},
    consts::{ATTESTOR_KEYS_PREFIX, DAML_KEY_NAME, NAMESPACE_KEY_NAME},
    error::Result,
    utils::{compute_fingerprint, write_messages_to_file},
    workflow::onboarding::OnboardingDirs,
};

/// Generate cryptographic keys and export them
///
/// Generates:
/// 1. Namespace signing key (for namespace delegation)
/// 2. DAML transaction key (for signing transactions)
/// 3. Exports both keys to attestor-public-keys.bin
///
/// This function generates signing keys and exports them,
/// and proposes a namespace delegation for the generated namespace key.
pub async fn generate_keys(
    config: &NodeConfig,
    dirs: &OnboardingDirs,
    network_config: &NetworkConfig,
) -> Result {
    tracing::info!("Generating cryptographic keys...");

    let mut vault_client = VaultServiceClient::connect(config.admin_api_url()).await?;

    // Generate namespace signing key
    tracing::debug!("Generating namespace signing key with name '{NAMESPACE_KEY_NAME}'");
    let namespace_key = generate_signing_key(
        &mut vault_client,
        NAMESPACE_KEY_NAME,
        vec![SigningKeyUsage::Namespace as i32],
    )
    .await?;

    let namespace_fingerprint = compute_fingerprint(&namespace_key);
    tracing::debug!("Namespace key fingerprint: {namespace_fingerprint}");

    // Propose namespace delegation
    propose_namespace_delegation(config, &namespace_key, &namespace_fingerprint).await?;

    // Generate DAML signing key for transactions
    tracing::debug!("Generating DAML signing key with name '{DAML_KEY_NAME}'");
    let daml_key = generate_signing_key(
        &mut vault_client,
        DAML_KEY_NAME,
        vec![SigningKeyUsage::Protocol as i32],
    )
    .await?;

    // Export keys with participant position from network config
    let participant_num = get_participant_position(&config.node.node_id, network_config)?;
    tracing::info!("Participant number: {participant_num}");

    export_keys(&dirs.keys_dir, &namespace_key, &daml_key, participant_num).await?;

    tracing::info!("Keys exported successfully");

    Ok(())
}

/// Helper: Generate a signing key via VaultService
async fn generate_signing_key(
    vault_client: &mut VaultServiceClient<tonic::transport::Channel>,
    name: &str,
    usage: Vec<i32>,
) -> Result<SigningPublicKey> {
    let request = tonic::Request::new(GenerateSigningKeyRequest {
        key_spec: SigningKeySpec::EcCurve25519 as i32,
        name: name.to_string(),
        usage,
    });

    let response = vault_client
        .generate_signing_key(request)
        .await?
        .into_inner();
    response
        .public_key
        .ok_or_else(|| anyhow::anyhow!("No public key returned from VaultService"))
}

/// Helper: Get participant position (1-based index) from network config
fn get_participant_position(node_id: &str, network_config: &NetworkConfig) -> Result<u32> {
    network_config
        .peers
        .iter()
        .position(|p| p.id == node_id)
        .map(|pos| (pos + 1) as u32)
        .ok_or_else(|| anyhow::anyhow!("Node ID '{node_id}' not found in network configuration"))
}

/// Helper: Export keys to file
async fn export_keys(
    keys_dir: &Path,
    namespace_key: &SigningPublicKey,
    daml_key: &SigningPublicKey,
    participant_num: u32,
) -> Result {
    let filename = format!("{ATTESTOR_KEYS_PREFIX}-{participant_num}.bin");
    let output_path = keys_dir.join(&filename);
    tracing::debug!("Exporting keys to {path}", path = output_path.display());
    write_messages_to_file(&[namespace_key.clone(), daml_key.clone()], &output_path).await
}

/// Propose namespace delegation for the generated namespace key
async fn propose_namespace_delegation(
    config: &NodeConfig,
    namespace_key: &SigningPublicKey,
    namespace_fingerprint: &str,
) -> Result {
    tracing::debug!("Proposing namespace delegation for {namespace_fingerprint}");

    let namespace_delegation = NamespaceDelegation {
        // fingerprint of the root key defining the namespace
        namespace: namespace_fingerprint.to_string(),

        // target key of getting full rights on the namespace (if target == namespace, it's a root CA)
        target_key: Some(namespace_key.clone()),

        #[allow(deprecated)]
        is_root_delegation: false,

        // restricts target_key to only sign transactions with the specified mapping types.
        // for backwards compatibility, only the following combinations are valid:
        //
        // * is_root_delegation = true,  restriction = empty: the key can sign all mappings
        // * is_root_delegation = false, restriction = empty: the key can sign all mappings but namespace delegations
        // * is_root_delegation = false, restriction = non-empty: the key can only sign the mappings according the restriction that is set
        restriction: Some(namespace_delegation::Restriction::CanSignAllMappings(
            namespace_delegation::CanSignAllMappings {},
        )),
    };

    let mut topology_client =
        TopologyManagerWriteServiceClient::connect(config.admin_api_url()).await?;

    let request = tonic::Request::new(AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            authorize_request::Proposal {
                change: TopologyChangeOp::AddReplace as i32,
                serial: 0,
                mapping: Some(TopologyMapping {
                    mapping: Some(topology_mapping::Mapping::NamespaceDelegation(
                        namespace_delegation,
                    )),
                }),
            },
        )),
        must_fully_authorize: true,
        force_changes: vec![],
        signed_by: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Authorized(store_id::Authorized {})),
        }),
        wait_to_become_effective: None,
    });

    topology_client.authorize(request).await?;
    tracing::debug!("Namespace delegation proposed successfully");
    Ok(())
}
