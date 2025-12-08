use std::collections::HashMap;

use colored::Colorize;

use canton_proto_rs::com::digitalasset::canton::{
    crypto::{
        admin::v30::{ListMyKeysRequest, vault_service_client::VaultServiceClient},
        v30::public_key,
    },
    topology::admin::v30::{
        BaseQuery, ListDecentralizedNamespaceDefinitionRequest, ListPartyToParticipantRequest,
        StoreId, Synchronizer, base_query, store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};

use crate::{config::NodeConfig, error::Result, utils};

/// Query and display decentralized parties from Canton topology
pub async fn query_parties(config: &NodeConfig, party_id_prefix: &str) -> Result {
    let admin_url = config.admin_api_url();
    tracing::info!("Connecting to {admin_url}");

    let mut client = TopologyManagerReadServiceClient::connect(admin_url.clone()).await?;
    let mut vault_client = VaultServiceClient::connect(admin_url).await?;

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::info!("Using synchronizer ID: {synchronizer_id}");

    // Get all namespace keys from this participant
    let keys_response = vault_client
        .list_my_keys(tonic::Request::new(ListMyKeysRequest { filters: None }))
        .await?
        .into_inner();

    let mut namespace_key_fingerprints = HashMap::new();
    for key_meta in keys_response.private_keys_metadata {
        if let Some(pub_key_with_name) = &key_meta.public_key_with_name
            && let Some(pub_key) = &pub_key_with_name.public_key
            && let Some(public_key::Key::SigningPublicKey(signing_key)) = &pub_key.key
            && signing_key.usage.contains(&1)
        {
            // SigningKeyUsage::Namespace = 1
            let fingerprint = utils::compute_fingerprint(signing_key);
            let name = if pub_key_with_name.name.is_empty() {
                "unnamed".to_string()
            } else {
                pub_key_with_name.name.clone()
            };
            namespace_key_fingerprints.insert(fingerprint, name);
        }
    }

    // List all decentralized namespaces
    println!(
        "\n{}",
        "=== Decentralized Namespaces ===".bold().bright_cyan()
    );

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
        filter_namespace: String::new(),
    });

    let response = client
        .list_decentralized_namespace_definition(request)
        .await?
        .into_inner();

    if response.results.is_empty() {
        println!("{}", "No decentralized namespaces found.".yellow());
        return Ok(());
    }

    for (idx, result) in response.results.iter().enumerate() {
        if let Some(item) = &result.item {
            println!(
                "\n{} {}",
                format!("[{idx}]").bright_white().bold(),
                "Namespace:".bright_blue()
            );
            println!("    {}", item.decentralized_namespace.cyan());
            println!(
                "    {}: {}",
                "Threshold".yellow(),
                item.threshold.to_string().bright_yellow().bold()
            );
            println!(
                "    {} ({}):",
                "Owners".green(),
                item.owners.len().to_string().bright_green().bold()
            );
            for (i, owner) in item.owners.iter().enumerate() {
                let marker = if namespace_key_fingerprints.contains_key(owner) {
                    let key_name = &namespace_key_fingerprints[owner];
                    format!(" <- THIS PARTICIPANT (key: {key_name})").bright_yellow()
                } else {
                    "".normal()
                };
                println!(
                    "      {} {}{}",
                    format!("[{i}]").dimmed(),
                    owner.green(),
                    marker
                );
            }

            // Query party mappings for this namespace
            let party_id = format!(
                "{party_id_prefix}::{namespace}",
                namespace = item.decentralized_namespace
            );
            println!(
                "\n    {} {}",
                "Party ID:".bright_magenta(),
                party_id.magenta()
            );

            let p2p_request = tonic::Request::new(ListPartyToParticipantRequest {
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
                filter_party: party_id.clone(),
                filter_participant: String::new(),
            });

            let p2p_response = client
                .list_party_to_participant(p2p_request)
                .await?
                .into_inner();

            if let Some(p2p_result) = p2p_response.results.first() {
                if let Some(p2p_item) = &p2p_result.item {
                    println!(
                        "    {} ({}):",
                        "Participants".bright_blue(),
                        p2p_item.participants.len().to_string().bright_blue().bold()
                    );
                    for (i, p) in p2p_item.participants.iter().enumerate() {
                        println!(
                            "      {} {}",
                            format!("[{i}]").dimmed(),
                            p.participant_uid.blue()
                        );
                    }
                }
            } else {
                println!("    {}", "No P2P mapping found".red().italic());
            }
        }
    }

    println!();
    Ok(())
}
