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
        AuthorizeRequest, StoreId, Synchronizer, authorize_request, store_id, synchronizer,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};

use crate::{
    config::NodeConfig,
    consts::{ATTESTOR_KEYS_PREFIX, PARTICIPANT_ID_PREFIX},
    error::Result,
    participant_id::CantonId,
    utils::{compute_fingerprint, get_participant_id, write_messages_to_file},
    workflow::add_party::{AddPartyConfig, AddPartyDirs},
};

/// Generate cryptographic keys for the new member being added
///
/// Generates:
/// 1. Namespace signing key (for namespace delegation)
/// 2. DAML transaction key (for signing transactions)
/// 3. Exports both keys to attestor-public-keys.bin
///
/// This is executed only by the new member joining the decentralized party.
pub async fn generate_keys(
    config: &NodeConfig,
    dirs: &AddPartyDirs,
    add_party_config: &AddPartyConfig,
) -> Result {
    tracing::info!("Generating cryptographic keys for new member...");

    let mut vault_client = VaultServiceClient::connect(config.admin_api_url()).await?;

    // Derive key names from party_id_prefix
    let namespace_key_name = add_party_config.namespace_key_name();
    let daml_key_name = add_party_config.daml_key_name();

    // Generate namespace signing key
    tracing::debug!("Generating namespace signing key with name '{namespace_key_name}'");
    let namespace_key = generate_signing_key(
        &mut vault_client,
        &namespace_key_name,
        vec![SigningKeyUsage::Namespace as i32],
    )
    .await?;

    let namespace_fingerprint = compute_fingerprint(&namespace_key);
    tracing::debug!("Namespace key fingerprint: {namespace_fingerprint}");

    // Propose namespace delegation
    propose_namespace_delegation(config, &namespace_key, &namespace_fingerprint).await?;

    // Generate DAML signing key for transactions
    tracing::debug!("Generating DAML signing key with name '{daml_key_name}'");
    let daml_key = generate_signing_key(
        &mut vault_client,
        &daml_key_name,
        vec![SigningKeyUsage::Protocol as i32],
    )
    .await?;

    // Export keys with participant ID
    let participant_id_str = config.participant_id().to_string();
    export_keys(
        &dirs.keys_dir,
        &namespace_key,
        &daml_key,
        &participant_id_str,
    )
    .await?;
    tracing::debug!("Keys exported successfully");

    // Get and export participant ID from Canton
    let participant_id = get_participant_id(config).await?;
    tracing::debug!("Participant ID: {participant_id}");
    export_participant_id(&dirs.ids_dir, &participant_id, &participant_id_str).await?;
    tracing::debug!("Participant ID exported successfully");

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

/// Helper: Export keys to file
async fn export_keys(
    keys_dir: &Path,
    namespace_key: &SigningPublicKey,
    daml_key: &SigningPublicKey,
    node_id: &str,
) -> Result {
    let filename = format!("{ATTESTOR_KEYS_PREFIX}-{node_id}.bin");
    let output_path = keys_dir.join(&filename);
    tracing::debug!("Exporting keys to {path}", path = output_path.display());
    write_messages_to_file(&[namespace_key.clone(), daml_key.clone()], &output_path).await
}

/// Helper: Export participant ID to file
async fn export_participant_id(ids_dir: &Path, participant_id: &CantonId, node_id: &str) -> Result {
    tokio::fs::create_dir_all(ids_dir).await?;
    let filename = format!("{PARTICIPANT_ID_PREFIX}-{node_id}.bin");
    let output_path = ids_dir.join(&filename);
    tracing::debug!(
        "Exporting participant ID to {path}",
        path = output_path.display()
    );
    tokio::fs::write(&output_path, participant_id.to_file_format()).await?;
    Ok(())
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

    // First, authorize in the local Authorized store
    let authorized_request = tonic::Request::new(AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            authorize_request::Proposal {
                change: TopologyChangeOp::AddReplace as i32,
                serial: 0,
                mapping: Some(TopologyMapping {
                    mapping: Some(topology_mapping::Mapping::NamespaceDelegation(
                        namespace_delegation.clone(),
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

    topology_client.authorize(authorized_request).await?;
    tracing::debug!("Namespace delegation authorized in local store");

    // Also submit to the synchronizer so it's visible for DNS updates
    let synchronizer_id = crate::utils::get_synchronizer_id(config).await?;
    let synchronizer_request = tonic::Request::new(AuthorizeRequest {
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
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id)),
            })),
        }),
        wait_to_become_effective: None,
    });

    match topology_client.authorize(synchronizer_request).await {
        Ok(_) => {
            tracing::debug!("Namespace delegation submitted to synchronizer");
        }
        Err(e) => {
            // Check if the error is because it already exists (which is fine)
            let status = e.to_string();
            if status.contains("ALREADY_EXISTS") || status.contains("already exists") {
                tracing::debug!("Namespace delegation already exists on synchronizer (this is OK)");
            } else {
                return Err(anyhow::anyhow!("Failed to submit namespace delegation to synchronizer: {e}"));
            }
        }
    }

    // Wait a moment for the namespace delegation to propagate
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    tracing::debug!("Waited for namespace delegation to propagate");

    Ok(())
}
