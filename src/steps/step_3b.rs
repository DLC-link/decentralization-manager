use std::path::Path;

use prost::Message;
use tokio::fs;

use crate::{
    config::Config,
    error::Result,
    proto::com::{
        daml::ledger::api::v2::{
            Command, CreateCommand, GenMap, Identifier, Optional, Record, RecordField, Value,
            admin::{
                CreateUserRequest, GrantUserRightsRequest, ListKnownPartiesRequest, ObjectMeta,
                Right, User,
                party_management_service_client::PartyManagementServiceClient,
                right::{CanActAs, CanReadAs, Kind},
                user_management_service_client::UserManagementServiceClient,
            },
            command, gen_map,
            interactive::{
                PrepareSubmissionRequest,
                interactive_submission_service_client::InteractiveSubmissionServiceClient,
            },
            value,
        },
        digitalasset::canton::protocol::v30::DecentralizedNamespaceDefinition,
    },
    utils,
};

/// Prepare ledger submissions for governance contracts
///
/// Corresponds to: 03b_PrepareSubmissions.sc
///
/// This step must be run once by the coordinator with appropriate Ledger API credentials.
/// It prepares interactive submissions for creating the governance contracts.
///
/// # Arguments
/// * `config` - Configuration with Ledger API connection details
/// * `out_dir` - Base output directory (usually ./out)
pub async fn prepare_submissions(config: &Config, out_dir: &Path) -> Result {
    tracing::info!("Preparing submissions...");

    // Step 1: Construct decentralized registrar party ID from namespace definition
    let namespace_file = out_dir.join("step_2a").join("namespaceDef.bin");
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
    let mut party_client = PartyManagementServiceClient::connect(config.ledger_api_url()).await?;

    let max_attempts = 30;
    let retry_delay = tokio::time::Duration::from_secs(2);

    for attempt in 1..=max_attempts {
        let response = party_client
            .list_known_parties(tonic::Request::new(ListKnownPartiesRequest {
                identity_provider_id: String::new(),
                page_token: String::new(),
                page_size: 1000,
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

    // Step 3: Look up other parties

    // Look up attestor parties
    let attestor1 = find_party(&mut party_client, "attestor-1").await?;
    let attestor2 = find_party(&mut party_client, "attestor-2").await?;
    let attestor3 = find_party(&mut party_client, "attestor-3").await?;
    tracing::debug!("Found attestors: {attestor1}, {attestor2}, {attestor3}");

    // Look up operator party
    let operator = find_party(&mut party_client, "operator").await?;
    tracing::debug!("Found operator: {operator}");

    // Step 4: Create CoordinatorUser and grant rights
    tracing::info!("Setting up CoordinatorUser...");
    let mut user_client = UserManagementServiceClient::connect(config.ledger_api_url()).await?;

    // Try to create user (may already exist)
    let create_user_result = user_client
        .create_user(tonic::Request::new(CreateUserRequest {
            user: Some(User {
                id: "CoordinatorUser".to_string(),
                primary_party: attestor1.clone(),
                is_deactivated: false,
                metadata: Some(ObjectMeta {
                    resource_version: String::new(),
                    annotations: [("description".to_string(), "Coordinator User".to_string())]
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
        Ok(_) => tracing::info!("Created CoordinatorUser"),
        Err(e) if e.code() == tonic::Code::AlreadyExists => {
            tracing::debug!("CoordinatorUser already exists");
        }
        Err(e) => return Err(e.into()),
    }

    // Grant rights for the decentralized registrar
    tracing::info!("Granting rights to CoordinatorUser for decentralized party...");
    user_client
        .grant_user_rights(tonic::Request::new(GrantUserRightsRequest {
            user_id: "CoordinatorUser".to_string(),
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

    tracing::info!("CoordinatorUser setup complete");

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

    let mut submission_client =
        InteractiveSubmissionServiceClient::connect(config.ledger_api_url()).await?;

    let prepared_submission1 = submission_client
        .prepare_submission(tonic::Request::new(PrepareSubmissionRequest {
            user_id: "CoordinatorUser".to_string(),
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
            user_id: "CoordinatorUser".to_string(),
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
            user_id: "CoordinatorUser".to_string(),
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
        }))
        .await?
        .into_inner();

    // Step 9: Save prepared submissions to files
    let step_4_dir = out_dir.join("step_4");
    let subs_dir = step_4_dir.join("subs");
    fs::create_dir_all(&subs_dir).await?;

    let submission1_file = subs_dir.join("prepared-submission-1.bin");
    tracing::debug!(
        "Saving prepared submission 1 to {}",
        submission1_file.display()
    );
    let prepared_tx1 = prepared_submission1
        .prepared_transaction
        .ok_or_else(|| anyhow::anyhow!("No prepared transaction in response"))?;
    let encoded1 = prepared_tx1.encode_to_vec();
    utils::write_bytes_to_file(&encoded1, &submission1_file).await?;

    let submission2_file = subs_dir.join("prepared-submission-2.bin");
    tracing::debug!(
        "Saving prepared submission 2 to {}",
        submission2_file.display()
    );
    let prepared_tx2 = prepared_submission2
        .prepared_transaction
        .ok_or_else(|| anyhow::anyhow!("No prepared transaction in response"))?;
    let encoded2 = prepared_tx2.encode_to_vec();
    utils::write_bytes_to_file(&encoded2, &submission2_file).await?;

    let submission3_file = subs_dir.join("prepared-submission-3.bin");
    tracing::debug!(
        "Saving prepared submission 3 to {}",
        submission3_file.display()
    );
    let prepared_tx3 = prepared_submission3
        .prepared_transaction
        .ok_or_else(|| anyhow::anyhow!("No prepared transaction in response"))?;
    let encoded3 = prepared_tx3.encode_to_vec();
    utils::write_bytes_to_file(&encoded3, &submission3_file).await?;

    tracing::info!("Submissions prepared successfully");
    Ok(())
}

/// Find a party by party ID prefix
async fn find_party(
    client: &mut PartyManagementServiceClient<tonic::transport::Channel>,
    party_prefix: &str,
) -> Result<String> {
    let response = client
        .list_known_parties(tonic::Request::new(ListKnownPartiesRequest {
            identity_provider_id: String::new(),
            page_token: String::new(),
            page_size: 1000,
        }))
        .await?
        .into_inner();

    for party_details in response.party_details {
        if party_details.party.contains(party_prefix) {
            return Ok(party_details.party);
        }
    }

    anyhow::bail!("Party with prefix '{party_prefix}' not found")
}
