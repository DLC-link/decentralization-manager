use anyhow::Context;
use tokio::fs;

use crate::{
    config::{NetworkConfig, NodeConfig},
    consts::{TOPOLOGY_RETRY_DELAY_SECS, TOPOLOGY_RETRY_MAX_ATTEMPTS},
    dirs::WorkflowDirs,
    error::Result,
    proto::com::{
        daml::ledger::api::v2::{
            Command, CreateCommand, GenMap, Identifier, Optional, Record, RecordField, Value,
            admin::{
                AllocatePartyRequest, CreateUserRequest, GrantUserRightsRequest,
                ListKnownPartiesRequest, ObjectMeta, Right, User,
                party_management_service_client::PartyManagementServiceClient,
                right::{CanActAs, CanReadAs, Kind},
            },
            command, gen_map,
            interactive::PrepareSubmissionRequest,
            value,
        },
        digitalasset::canton::protocol::v30::DecentralizedNamespaceDefinition,
    },
    utils,
};

/// Default page size for listing operations (parties, keys, etc.)
const DEFAULT_PAGE_SIZE: i32 = 1000;

/// Prepare ledger submissions for governance contracts
///
/// Corresponds to: 03b_PrepareSubmissions.sc
///
/// This step must be run once by the coordinator with appropriate Ledger API credentials.
/// It prepares interactive submissions for creating the governance contracts.
///
/// # Arguments
/// * `config` - Configuration with Ledger API connection details
/// * `dirs` - WorkflowDirs containing all directory paths
pub async fn prepare_submissions(config: &NodeConfig, dirs: &WorkflowDirs) -> Result {
    tracing::info!("Preparing submissions...");

    // Step 1: Construct decentralized registrar party ID from namespace definition
    let namespace_file = dirs.dns_submission_dir.join("namespaceDef.bin");
    tracing::debug!(
        "Reading namespace definition from {}",
        namespace_file.display()
    );
    let namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_file(&namespace_file).await?;

    let decentralized_registrar =
        format!("cbtc-network::{}", namespace_def.decentralized_namespace);
    tracing::debug!("Constructed decentralized registrar: {decentralized_registrar}");

    // Step 2: Wait for party to be visible in Ledger API
    tracing::info!("Waiting for decentralized party to be visible in Ledger API...");
    let mut party_client = utils::create_party_client(config).await?;

    let max_attempts = TOPOLOGY_RETRY_MAX_ATTEMPTS;
    let retry_delay = tokio::time::Duration::from_secs(TOPOLOGY_RETRY_DELAY_SECS);

    for attempt in 1..=max_attempts {
        let response = party_client
            .list_known_parties(tonic::Request::new(ListKnownPartiesRequest {
                identity_provider_id: String::new(),
                page_token: String::new(),
                page_size: DEFAULT_PAGE_SIZE,
            }))
            .await?
            .into_inner();

        let party_found = response
            .party_details
            .iter()
            .any(|pd| pd.party == decentralized_registrar);

        if party_found {
            tracing::info!("Decentralized party found in Ledger API after {attempt} attempt(s)");
            break;
        }

        if attempt < max_attempts {
            tracing::debug!(
                "Party not yet visible in Ledger API, attempt {attempt}/{max_attempts}, retrying in {retry_delay:?}..."
            );
            tokio::time::sleep(retry_delay).await;
        } else {
            anyhow::bail!(
                "Decentralized party not visible in Ledger API after {max_attempts} attempts"
            );
        }
    }

    // Step 3: Load network config and allocate parties for each participant
    let network_config_path = if std::path::PathBuf::from(&config.network_config).is_absolute() {
        std::path::PathBuf::from(&config.network_config)
    } else {
        // Resolve relative to test-configs directory
        std::env::current_dir()?
            .join("test-configs")
            .join(&config.network_config)
    };
    let network_config = NetworkConfig::from_file(&network_config_path).await?;

    tracing::info!("Getting parties for participants...");
    let mut participant_parties = Vec::new();

    for participant in &network_config.participants {
        let party = if let Some(party_id) = &participant.party {
            // Use party from config
            tracing::debug!("Using party from config for {}: {party_id}", participant.id);
            party_id.clone()
        } else {
            // Fallback to allocating/finding party
            tracing::debug!("Allocating/finding party for {}", participant.id);
            allocate_or_find_party(
                &mut party_client,
                &participant.id,
                &utils::get_synchronizer_id(config).await?,
            )
            .await?
        };
        tracing::debug!("Party for {}: {party}", participant.id);
        participant_parties.push(party);
    }

    if participant_parties.len() < 3 {
        anyhow::bail!(
            "Expected at least 3 participants, found {}",
            participant_parties.len()
        );
    }

    let attestor1 = participant_parties[0].clone();
    let attestor2 = participant_parties[1].clone();
    let attestor3 = participant_parties[2].clone();
    tracing::info!("Parties for participants: {attestor1}, {attestor2}, {attestor3}");

    // Get operator party from config or allocate
    let operator = if let Some(operator_party) = &network_config.network.operator_party {
        tracing::debug!("Using operator party from config: {operator_party}");
        operator_party.clone()
    } else {
        tracing::debug!("Allocating/finding operator party");
        allocate_or_find_party(
            &mut party_client,
            "operator",
            &utils::get_synchronizer_id(config).await?,
        )
        .await?
    };
    tracing::info!("Operator party: {operator}");

    // Step 4: Create ledger-api-user and grant rights
    // Note: User ID must match JWT token's "sub" claim
    tracing::info!("Setting up ledger-api-user...");
    let mut user_client = utils::create_user_client(config).await?;
    let user_id = "ledger-api-user";

    // Try to create user (may already exist)
    let create_user_result = user_client
        .create_user(tonic::Request::new(CreateUserRequest {
            user: Some(User {
                id: user_id.to_string(),
                primary_party: attestor1.clone(),
                is_deactivated: false,
                metadata: Some(ObjectMeta {
                    resource_version: String::new(),
                    annotations: [("description".to_string(), "Ledger API User".to_string())]
                        .into_iter()
                        .collect(),
                }),
                identity_provider_id: String::new(),
            }),
            rights: vec![
                Right {
                    kind: Some(Kind::CanActAs(CanActAs {
                        party: attestor1.clone(),
                    })),
                },
                Right {
                    kind: Some(Kind::CanReadAs(CanReadAs {
                        party: attestor1.clone(),
                    })),
                },
            ],
        }))
        .await;

    match create_user_result {
        Ok(_) => tracing::info!("Created {user_id}"),
        Err(e) if e.code() == tonic::Code::AlreadyExists => {
            tracing::debug!("{user_id} already exists");
        }
        Err(e) => return Err(e.into()),
    }

    // Grant rights for the decentralized registrar
    tracing::info!("Granting rights to {user_id} for decentralized party...");
    user_client
        .grant_user_rights(tonic::Request::new(GrantUserRightsRequest {
            user_id: user_id.to_string(),
            rights: vec![
                Right {
                    kind: Some(Kind::CanActAs(CanActAs {
                        party: decentralized_registrar.clone(),
                    })),
                },
                Right {
                    kind: Some(Kind::CanReadAs(CanReadAs {
                        party: decentralized_registrar.clone(),
                    })),
                },
            ],
            identity_provider_id: String::new(),
        }))
        .await?;

    tracing::info!("{user_id} setup complete");

    // Step 5: Build common structures
    let threshold = 2i64;

    // Instrument record (InstrumentId with admin and id fields)
    let instrument = Record {
        record_id: None,
        fields: vec![
            RecordField {
                label: String::new(),
                value: Some(Value {
                    sum: Some(value::Sum::Party(decentralized_registrar.clone())),
                }),
            },
            RecordField {
                label: String::new(),
                value: Some(Value {
                    sum: Some(value::Sum::Text("CBTC".to_string())),
                }),
            },
        ],
    };

    let unit = Value {
        sum: Some(value::Sum::Unit(())),
    };

    // Step 6: Prepare submission 1 - CBTCGovernanceRules
    tracing::info!("Preparing submission 1: CBTCGovernanceRules");

    let gov_template_id = Identifier {
        package_id: "#cbtc-governance".to_string(),
        module_name: "CBTC.Governance".to_string(),
        entity_name: "CBTCGovernanceRules".to_string(),
    };

    // Build attestors GenMap (representing Set Party in Daml)
    let attestors_map = GenMap {
        entries: vec![
            gen_map::Entry {
                key: Some(Value {
                    sum: Some(value::Sum::Party(attestor1.clone())),
                }),
                value: Some(unit.clone()),
            },
            gen_map::Entry {
                key: Some(Value {
                    sum: Some(value::Sum::Party(attestor2.clone())),
                }),
                value: Some(unit.clone()),
            },
            gen_map::Entry {
                key: Some(Value {
                    sum: Some(value::Sum::Party(attestor3.clone())),
                }),
                value: Some(unit.clone()),
            },
        ],
    };

    let create_gov_rules_command = Command {
        command: Some(command::Command::Create(CreateCommand {
            template_id: Some(gov_template_id),
            create_arguments: Some(Record {
                record_id: None,
                fields: vec![
                    RecordField {
                        label: String::new(),
                        value: Some(Value {
                            sum: Some(value::Sum::Party(decentralized_registrar.clone())),
                        }),
                    },
                    RecordField {
                        label: String::new(),
                        value: Some(Value {
                            sum: Some(value::Sum::Party(operator.clone())),
                        }),
                    },
                    RecordField {
                        label: String::new(),
                        value: Some(Value {
                            sum: Some(value::Sum::Record(instrument.clone())),
                        }),
                    },
                    RecordField {
                        label: String::new(),
                        value: Some(Value {
                            sum: Some(value::Sum::Record(Record {
                                record_id: None,
                                fields: vec![RecordField {
                                    label: String::new(),
                                    value: Some(Value {
                                        sum: Some(value::Sum::GenMap(attestors_map)),
                                    }),
                                }],
                            })),
                        }),
                    },
                    RecordField {
                        label: String::new(),
                        value: Some(Value {
                            sum: Some(value::Sum::Optional(Box::new(Optional {
                                value: Some(Box::new(Value {
                                    sum: Some(value::Sum::Int64(threshold)),
                                })),
                            }))),
                        }),
                    },
                ],
            }),
        })),
    };

    let mut submission_client = utils::create_submission_client(config).await?;

    let prepared_submission1 = submission_client
        .prepare_submission(tonic::Request::new(PrepareSubmissionRequest {
            user_id: user_id.to_string(),
            command_id: "create-govR".to_string(),
            commands: vec![create_gov_rules_command],
            min_ledger_time: None,
            max_record_time: None,
            act_as: vec![decentralized_registrar.clone()],
            read_as: vec![],
            disclosed_contracts: vec![],
            synchronizer_id: String::new(),
            package_id_selection_preference: vec![],
            verbose_hashing: false,
            prefetch_contract_keys: vec![],
            estimate_traffic_cost: None,
        }))
        .await?
        .into_inner();

    // Step 7: Prepare submission 2 - CBTCDepositAccountRules
    tracing::info!("Preparing submission 2: CBTCDepositAccountRules");

    let deposit_template_id = Identifier {
        package_id: "#cbtc".to_string(),
        module_name: "CBTC.DepositAccount".to_string(),
        entity_name: "CBTCDepositAccountRules".to_string(),
    };

    let create_deposit_rules_command = Command {
        command: Some(command::Command::Create(CreateCommand {
            template_id: Some(deposit_template_id),
            create_arguments: Some(Record {
                record_id: None,
                fields: vec![
                    RecordField {
                        label: String::new(),
                        value: Some(Value {
                            sum: Some(value::Sum::Party(decentralized_registrar.clone())),
                        }),
                    },
                    RecordField {
                        label: String::new(),
                        value: Some(Value {
                            sum: Some(value::Sum::Party(operator.clone())),
                        }),
                    },
                    RecordField {
                        label: String::new(),
                        value: Some(Value {
                            sum: Some(value::Sum::Record(instrument.clone())),
                        }),
                    },
                ],
            }),
        })),
    };

    let prepared_submission2 = submission_client
        .prepare_submission(tonic::Request::new(PrepareSubmissionRequest {
            user_id: user_id.to_string(),
            command_id: "create-daR".to_string(),
            commands: vec![create_deposit_rules_command],
            min_ledger_time: None,
            max_record_time: None,
            act_as: vec![decentralized_registrar.clone()],
            read_as: vec![],
            disclosed_contracts: vec![],
            synchronizer_id: String::new(),
            package_id_selection_preference: vec![],
            verbose_hashing: false,
            prefetch_contract_keys: vec![],
            estimate_traffic_cost: None,
        }))
        .await?
        .into_inner();

    // Step 8: Prepare submission 3 - CBTCWithdrawAccountRules
    tracing::info!("Preparing submission 3: CBTCWithdrawAccountRules");

    let withdraw_template_id = Identifier {
        package_id: "#cbtc".to_string(),
        module_name: "CBTC.WithdrawAccount".to_string(),
        entity_name: "CBTCWithdrawAccountRules".to_string(),
    };

    let create_withdraw_rules_command = Command {
        command: Some(command::Command::Create(CreateCommand {
            template_id: Some(withdraw_template_id),
            create_arguments: Some(Record {
                record_id: None,
                fields: vec![
                    RecordField {
                        label: String::new(),
                        value: Some(Value {
                            sum: Some(value::Sum::Party(decentralized_registrar.clone())),
                        }),
                    },
                    RecordField {
                        label: String::new(),
                        value: Some(Value {
                            sum: Some(value::Sum::Party(operator.clone())),
                        }),
                    },
                    RecordField {
                        label: String::new(),
                        value: Some(Value {
                            sum: Some(value::Sum::Record(instrument.clone())),
                        }),
                    },
                ],
            }),
        })),
    };

    let prepared_submission3 = submission_client
        .prepare_submission(tonic::Request::new(PrepareSubmissionRequest {
            user_id: user_id.to_string(),
            command_id: "create-waR".to_string(),
            commands: vec![create_withdraw_rules_command],
            min_ledger_time: None,
            max_record_time: None,
            act_as: vec![decentralized_registrar.clone()],
            read_as: vec![],
            disclosed_contracts: vec![],
            synchronizer_id: String::new(),
            package_id_selection_preference: vec![],
            verbose_hashing: false,
            prefetch_contract_keys: vec![],
            estimate_traffic_cost: None,
        }))
        .await?
        .into_inner();

    // Step 9: Save prepared submissions to files
    let ledger_submissions_dir = dirs.workflow_dir.join("ledger-submissions");
    let prepared_dir = ledger_submissions_dir.join("prepared");
    fs::create_dir_all(&prepared_dir).await?;

    let submission1_file = prepared_dir.join("prepared-submission-1.bin");
    tracing::debug!(
        "Saving prepared submission 1 to {}",
        submission1_file.display()
    );
    utils::write_messages_to_file(&[prepared_submission1], &submission1_file).await?;

    let submission2_file = prepared_dir.join("prepared-submission-2.bin");
    tracing::debug!(
        "Saving prepared submission 2 to {}",
        submission2_file.display()
    );
    utils::write_messages_to_file(&[prepared_submission2], &submission2_file).await?;

    let submission3_file = prepared_dir.join("prepared-submission-3.bin");
    tracing::debug!(
        "Saving prepared submission 3 to {}",
        submission3_file.display()
    );
    utils::write_messages_to_file(&[prepared_submission3], &submission3_file).await?;

    tracing::info!("Submissions prepared successfully");
    Ok(())
}

/// Allocate a party with a given hint, or find if it already exists
async fn allocate_or_find_party<T>(
    client: &mut PartyManagementServiceClient<T>,
    party_id_hint: &str,
    synchronizer_id: &str,
) -> Result<String>
where
    T: tonic::client::GrpcService<tonic::body::Body> + Send,
    T::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    T::ResponseBody: tonic::codegen::Body<Data = tonic::codegen::Bytes> + Send + 'static,
    <T::ResponseBody as tonic::codegen::Body>::Error:
        Into<Box<dyn std::error::Error + Send + Sync>> + Send,
    T::Future: Send,
{
    // First try to find existing party with this hint
    let list_response = client
        .list_known_parties(tonic::Request::new(ListKnownPartiesRequest {
            identity_provider_id: String::new(),
            page_token: String::new(),
            page_size: DEFAULT_PAGE_SIZE,
        }))
        .await?
        .into_inner();

    // Check if party already exists (exact match or contains the hint)
    for party_details in list_response.party_details {
        if party_details.party.contains(party_id_hint) {
            tracing::debug!("Found existing party: {}", party_details.party);
            return Ok(party_details.party);
        }
    }

    // Party doesn't exist, allocate it
    tracing::debug!("Allocating new party with hint: {party_id_hint}");

    // Canton 3.4+ requires only the fingerprint portion of the synchronizer ID for party allocation
    // Extract fingerprint from full ID format: alias::fingerprint::version -> fingerprint
    let synchronizer_fingerprint = utils::extract_synchronizer_fingerprint(synchronizer_id)
        .context("Failed to extract synchronizer fingerprint for party allocation")?;
    tracing::debug!(
        "Using synchronizer fingerprint for party allocation: {synchronizer_fingerprint}"
    );

    let allocate_response = client
        .allocate_party(tonic::Request::new(AllocatePartyRequest {
            party_id_hint: party_id_hint.to_string(),
            local_metadata: Some(ObjectMeta {
                resource_version: String::new(),
                annotations: [(
                    "description".to_string(),
                    format!("Party for {party_id_hint}"),
                )]
                .into_iter()
                .collect(),
            }),
            identity_provider_id: String::new(),
            synchronizer_id: synchronizer_fingerprint,
            user_id: String::new(),
        }))
        .await?
        .into_inner();

    let party_id = allocate_response
        .party_details
        .ok_or_else(|| anyhow::anyhow!("AllocateParty returned no party details"))?
        .party;

    tracing::debug!("Allocated new party: {party_id}");
    Ok(party_id)
}
