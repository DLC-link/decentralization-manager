use canton_proto_rs::com::daml::ledger::api::v2::{
    Command, CreateCommand, GenMap, Identifier, Optional, Record, RecordField, Value, command,
    gen_map, interactive::PrepareSubmissionRequest, value,
};
use tokio::fs;

use crate::{
    config::{NetworkConfig, NodeConfig},
    consts::{
        LEDGER_SUBMISSIONS_DIR, MIN_PARTICIPANTS_CONTRACTS, PREPARED_DIR,
        PREPARED_SUBMISSION_PREFIX,
    },
    error::Result,
    utils,
    workflow::contracts::{ContractsConfig, ContractsDirs, FieldDefinition},
};

/// Prepare ledger submissions for governance contracts
///
/// This step must be run once by the coordinator with appropriate Ledger API credentials.
/// It prepares interactive submissions for creating the governance contracts.
///
/// # Arguments
/// * `config` - Configuration with Ledger API connection details
/// * `dirs` - WorkflowDirs containing all directory paths
/// * `network_config` - Network configuration with peer settings
/// * `contracts_config` - Contracts workflow configuration with party ID
/// * `token` - Authentication token for Ledger API
/// * `user_id` - User ID for Ledger API operations
pub async fn prepare_submissions(
    config: &NodeConfig,
    dirs: &ContractsDirs,
    network_config: &NetworkConfig,
    contracts_config: &ContractsConfig,
    token: &str,
    user_id: &str,
) -> Result {
    tracing::info!("Preparing submissions...");

    // Use the decentralized party ID from config
    let decentralized_registrar = contracts_config.decentralized_party_id.to_string();
    tracing::debug!("Using decentralized party: {decentralized_registrar}");

    let token_opt = Some(token.to_string());

    // Get participant parties from config (provided by API caller)
    let participant_parties: Vec<String> = contracts_config
        .participant_parties
        .iter()
        .map(|p| p.to_string())
        .collect();

    // Validate participant count
    if participant_parties.is_empty() {
        anyhow::bail!("No participant parties provided in contracts config");
    }

    if participant_parties.len() < MIN_PARTICIPANTS_CONTRACTS {
        anyhow::bail!(
            "At least {MIN_PARTICIPANTS_CONTRACTS} participants required for contract operations, found {count}",
            count = participant_parties.len()
        );
    }

    tracing::info!(
        "Parties for {count} participants: {parties}",
        count = participant_parties.len(),
        parties = participant_parties.join(", ")
    );

    // Get operator party from config
    let operator = contracts_config.operator_party.to_string();
    tracing::info!("Operator party: {operator}");

    // Build context for field value building
    let context = SubmissionContext {
        decentralized_party: decentralized_registrar.clone(),
        operator_party: operator.clone(),
        participant_parties: participant_parties.clone(),
        governance_threshold: network_config.governance_threshold() as i64,
    };

    // Step 6: Prepare submissions for each contract defined in config
    let mut submission_client = utils::create_submission_client(config, token_opt.clone()).await?;
    let ledger_submissions_dir = dirs.workflow_dir.join(LEDGER_SUBMISSIONS_DIR);
    let prepared_dir = ledger_submissions_dir.join(PREPARED_DIR);
    fs::create_dir_all(&prepared_dir).await?;

    if contracts_config.contracts.is_empty() {
        tracing::warn!(
            "No contracts defined in application config, skipping submission preparation"
        );
        return Ok(());
    }

    for (idx, contract_def) in contracts_config.contracts.iter().enumerate() {
        tracing::info!(
            "Preparing submission {idx}: {contract_name} ({contract_id})",
            idx = idx + 1,
            contract_name = contract_def.name,
            contract_id = contract_def.id
        );

        let template_id = Identifier {
            package_id: contract_def.package_id.clone(),
            module_name: contract_def.module_name.clone(),
            entity_name: contract_def.entity_name.clone(),
        };

        let fields = contract_def
            .fields
            .iter()
            .map(|field_def| build_record_field(field_def, &context))
            .collect::<Result<Vec<_>>>()?;

        let create_command = Command {
            command: Some(command::Command::Create(CreateCommand {
                template_id: Some(template_id),
                create_arguments: Some(Record {
                    record_id: None,
                    fields,
                }),
            })),
        };

        let prepared_submission = submission_client
            .prepare_submission(tonic::Request::new(PrepareSubmissionRequest {
                user_id: user_id.to_string(),
                command_id: contract_def.id.clone(),
                commands: vec![create_command],
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

        let submission_file = prepared_dir.join(format!(
            "{PREPARED_SUBMISSION_PREFIX}-{index}.bin",
            index = idx + 1
        ));
        tracing::debug!(
            "Saving prepared submission {index} to {path}",
            index = idx + 1,
            path = submission_file.display()
        );
        utils::write_messages_to_file(&[prepared_submission], &submission_file).await?;
    }

    tracing::info!(
        "{count} submissions prepared successfully",
        count = contracts_config.contracts.len()
    );
    Ok(())
}

/// Context for building field values in contract submissions
struct SubmissionContext {
    decentralized_party: String,
    operator_party: String,
    participant_parties: Vec<String>,
    governance_threshold: i64,
}

/// Build a RecordField from a FieldDefinition
fn build_record_field(
    field_def: &FieldDefinition,
    context: &SubmissionContext,
) -> Result<RecordField> {
    Ok(RecordField {
        label: String::new(),
        value: Some(build_field_value(field_def, context)?),
    })
}

/// Build a Daml Value from a FieldDefinition
fn build_field_value(field_def: &FieldDefinition, context: &SubmissionContext) -> Result<Value> {
    let sum = match field_def {
        FieldDefinition::DecentralizedParty => {
            value::Sum::Party(context.decentralized_party.clone())
        }
        FieldDefinition::OperatorParty => value::Sum::Party(context.operator_party.clone()),
        FieldDefinition::ParticipantParty { id } => value::Sum::Party(id.to_string()),
        FieldDefinition::Text { value: text } => value::Sum::Text(text.clone()),
        FieldDefinition::Int64 { value: num } => value::Sum::Int64(*num),
        FieldDefinition::Bool { value: b } => value::Sum::Bool(*b),
        FieldDefinition::Instrument { id } => {
            // Instrument record: { admin: Party, id: Text }
            value::Sum::Record(Record {
                record_id: None,
                fields: vec![
                    RecordField {
                        label: String::new(),
                        value: Some(Value {
                            sum: Some(value::Sum::Party(context.decentralized_party.clone())),
                        }),
                    },
                    RecordField {
                        label: String::new(),
                        value: Some(Value {
                            sum: Some(value::Sum::Text(id.clone())),
                        }),
                    },
                ],
            })
        }
        FieldDefinition::AttestorsSet => {
            // Raw GenMap<Party, Unit> for CBTC-style contracts
            let unit = Value {
                sum: Some(value::Sum::Unit(())),
            };
            value::Sum::GenMap(GenMap {
                entries: context
                    .participant_parties
                    .iter()
                    .map(|party| gen_map::Entry {
                        key: Some(Value {
                            sum: Some(value::Sum::Party(party.clone())),
                        }),
                        value: Some(unit.clone()),
                    })
                    .collect(),
            })
        }
        FieldDefinition::PartySet { parties } => {
            // DA.Set.Types:Set Party is a record containing a "map" field with GenMap<Party, Unit>
            let unit = Value {
                sum: Some(value::Sum::Unit(())),
            };
            let gen_map = GenMap {
                entries: parties
                    .iter()
                    .map(|party| gen_map::Entry {
                        key: Some(Value {
                            sum: Some(value::Sum::Party(party.to_string())),
                        }),
                        value: Some(unit.clone()),
                    })
                    .collect(),
            };
            value::Sum::Record(Record {
                record_id: None,
                fields: vec![RecordField {
                    label: "map".to_string(),
                    value: Some(Value {
                        sum: Some(value::Sum::GenMap(gen_map)),
                    }),
                }],
            })
        }
        FieldDefinition::RelTime { microseconds } => {
            // DA.Time.Types:RelTime is a record containing a "microseconds" field (Int64)
            value::Sum::Record(Record {
                record_id: None,
                fields: vec![RecordField {
                    label: "microseconds".to_string(),
                    value: Some(Value {
                        sum: Some(value::Sum::Int64(*microseconds)),
                    }),
                }],
            })
        }
        FieldDefinition::Optional { inner } => {
            let inner_value = build_field_value(inner, context)?;
            value::Sum::Optional(Box::new(Optional {
                value: Some(Box::new(inner_value)),
            }))
        }
        FieldDefinition::None => value::Sum::Optional(Box::new(Optional { value: None })),
        FieldDefinition::Record { fields } => {
            let record_fields = fields
                .iter()
                .map(|f| build_record_field(f, context))
                .collect::<Result<Vec<_>>>()?;
            value::Sum::Record(Record {
                record_id: None,
                fields: record_fields,
            })
        }
        FieldDefinition::GovernanceThreshold { value } => {
            // Use provided value or fall back to calculated threshold
            value::Sum::Int64(value.unwrap_or(context.governance_threshold))
        }
    };

    Ok(Value { sum: Some(sum) })
}
