use bytes::{BufMut, BytesMut};
use canton_proto_rs::com::daml::ledger::api::v2::{
    Command, CreateCommand, GenMap, Identifier, Optional, Record, RecordField, Value, command,
    gen_map,
    interactive::{PrepareSubmissionRequest, PrepareSubmissionResponse},
    value,
};
use prost::Message;
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig},
    consts::MIN_PARTICIPANTS_CONTRACTS,
    error::Result,
    utils,
    workflow::{
        contracts::{ContractsConfig, FieldDefinition},
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// Prepare ledger submissions for governance contracts
///
/// This step must be run once by the coordinator with appropriate Ledger API credentials.
/// It prepares interactive submissions for creating the governance contracts.
///
/// Each prepared submission is persisted as a `PREPARED_SUBMISSION` artefact
/// keyed by a zero-padded ordinal (`"0000"`, `"0001"`, …) so subsequent reads
/// via `list_artifacts` return submissions sorted by their original creation
/// order — this matches the previous filesystem-based discovery loop, which
/// relied on lexicographic filename ordering.
///
/// # Arguments
/// * `config` - Configuration with Ledger API connection details
/// * `db` - Workflow storage backend (SqlitePool implementing `WorkflowStorage`)
/// * `instance_name` - Workflow run instance name (key for `workflow_artifacts`)
/// * `network_config` - Network configuration with peer settings
/// * `contracts_config` - Contracts workflow configuration with party ID
/// * `token` - Authentication token for Ledger API
/// * `user_id` - User ID for Ledger API operations
#[allow(clippy::too_many_arguments)]
pub async fn prepare_submissions(
    config: &NodeConfig,
    db: &SqlitePool,
    instance_name: &str,
    network_config: &NetworkConfig,
    contracts_config: &ContractsConfig,
    token: &str,
    user_id: &str,
) -> Result {
    tracing::info!("Preparing submissions...");

    // Use the decentralized party ID from config
    let decentralized_registrar = contracts_config.decentralized_party_id.clone();
    tracing::debug!("Using decentralized party: {decentralized_registrar}");

    let token_opt = Some(token.to_string());

    // Get participant parties from config (provided by API caller)
    let participant_parties: Vec<CantonId> = contracts_config.participant_parties.clone();

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
        parties = participant_parties
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Get operator party from config
    let operator = contracts_config.operator_party.clone();
    tracing::info!("Operator party: {operator}");

    // Build context for field value building
    let context = SubmissionContext {
        decentralized_party: decentralized_registrar.clone(),
        operator_party: operator.clone(),
        participant_parties: participant_parties.clone(),
        governance_threshold: network_config.governance_threshold() as i64,
    };

    let mut submission_client = utils::create_submission_client(config, token_opt.clone()).await?;

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
                act_as: vec![decentralized_registrar.to_string()],
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

        // Persist as `varint(len)||proto` so reads via
        // `utils::read_first_message_from_bytes` round-trip cleanly. This is
        // the exact byte shape `utils::write_messages_to_file(&[m], path)`
        // would have written for a single message.
        let payload = encode_length_prefixed_message(&prepared_submission);
        let ordinal = format!("{idx:04}");
        tracing::debug!(
            "Saving prepared submission {index} to artifact peer key {ordinal}",
            index = idx + 1,
        );
        db.write_artifact(
            instance_name,
            artifact_kinds::PREPARED_SUBMISSION,
            Some(&ordinal),
            &payload,
        )
        .await?;
    }

    tracing::info!(
        "{count} submissions prepared successfully",
        count = contracts_config.contracts.len()
    );
    Ok(())
}

/// Encode a single protobuf message as `varint(len)||proto`, matching the
/// byte layout produced by `utils::write_message_to_file`. Keeps the
/// downstream `utils::read_first_message_from_bytes` reader unchanged.
fn encode_length_prefixed_message(message: &PrepareSubmissionResponse) -> Vec<u8> {
    let encoded = message.encode_to_vec();
    let mut buffer = BytesMut::new();
    prost::encoding::encode_varint(encoded.len() as u64, &mut buffer);
    buffer.put_slice(&encoded);
    buffer.to_vec()
}

/// Context for building field values in contract submissions
struct SubmissionContext {
    decentralized_party: CantonId,
    operator_party: CantonId,
    participant_parties: Vec<CantonId>,
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
            value::Sum::Party(context.decentralized_party.to_string())
        }
        FieldDefinition::OperatorParty => value::Sum::Party(context.operator_party.to_string()),
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
                            sum: Some(value::Sum::Party(context.decentralized_party.to_string())),
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
                            sum: Some(value::Sum::Party(party.to_string())),
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
