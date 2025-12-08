use anyhow::Context;
use tokio::fs;

use canton_proto_rs::com::{
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
};

use crate::{
    config::{FieldDefinition, NetworkConfig, NodeConfig},
    consts::{
        LEDGER_SUBMISSIONS_DIR, MIN_PARTICIPANTS_CONTRACTS, NAMESPACE_DEF_FILENAME, PREPARED_DIR,
        PREPARED_SUBMISSION_PREFIX, TOPOLOGY_RETRY_DELAY_SECS, TOPOLOGY_RETRY_MAX_ATTEMPTS,
    },
    error::Result,
    utils,
    workflow::contracts::ContractsDirs,
};

/// Default page size for listing operations (parties, keys, etc.)
const DEFAULT_PAGE_SIZE: i32 = 1000;

/// Prepare ledger submissions for governance contracts
///
/// This step must be run once by the coordinator with appropriate Ledger API credentials.
/// It prepares interactive submissions for creating the governance contracts.
///
/// # Arguments
/// * `config` - Configuration with Ledger API connection details
/// * `dirs` - WorkflowDirs containing all directory paths
/// * `network_config` - Network configuration with application settings
pub async fn prepare_submissions(
    config: &NodeConfig,
    dirs: &ContractsDirs,
    network_config: &NetworkConfig,
) -> Result {
    tracing::info!("Preparing submissions...");

    let app_config = &network_config.application;
    let party_id_prefix = &app_config.party_id_prefix;
    let user_id = &config.canton.ledger_api_user_id;

    // Step 1: Construct decentralized registrar party ID from namespace definition
    let namespace_file = dirs.dns_submission_dir.join(NAMESPACE_DEF_FILENAME);
    tracing::debug!(
        "Reading namespace definition from {path}",
        path = namespace_file.display()
    );
    let namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_file(&namespace_file).await?;

    let decentralized_registrar = format!(
        "{party_id_prefix}::{namespace}",
        namespace = namespace_def.decentralized_namespace
    );
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

    // Step 3: Allocate parties for each participant
    tracing::info!("Getting parties for participants...");
    let mut participant_parties = Vec::new();

    for participant in &network_config.participants {
        let party = if let Some(party_id) = &participant.party {
            // Use party from config
            tracing::debug!(
                "Using party from config for {id}: {party_id}",
                id = participant.id
            );
            party_id.clone()
        } else {
            // Fallback to allocating/finding party
            tracing::debug!("Allocating/finding party for {id}", id = participant.id);
            allocate_or_find_party(
                &mut party_client,
                &participant.id,
                &utils::get_synchronizer_id(config).await?,
            )
            .await?
        };
        tracing::debug!("Party for {id}: {party}", id = participant.id);
        participant_parties.push(party);
    }

    // Validate participant count
    if participant_parties.is_empty() {
        anyhow::bail!("No participants found in P2P mapping");
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

    // Get operator party from config or allocate
    let operator = if let Some(operator_party) = &network_config.network.operator_party {
        tracing::debug!("Using operator party from config: {operator_party}");
        operator_party.clone()
    } else {
        tracing::debug!("Allocating/finding operator party");
        allocate_or_find_party(
            &mut party_client,
            &app_config.operator_party_hint,
            &utils::get_synchronizer_id(config).await?,
        )
        .await?
    };
    tracing::info!("Operator party: {operator}");

    // Step 4: Create ledger-api-user and grant rights
    // Note: User ID must match JWT token's "sub" claim
    tracing::info!("Setting up {user_id}...");
    let mut user_client = utils::create_user_client(config).await?;

    // Try to create user (may already exist) - use first participant as primary party
    let primary_party = participant_parties
        .first()
        .ok_or_else(|| anyhow::anyhow!("No participants available for user creation"))?
        .clone();

    let create_user_result = user_client
        .create_user(tonic::Request::new(CreateUserRequest {
            user: Some(User {
                id: user_id.to_string(),
                primary_party: primary_party.clone(),
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
                        party: primary_party.clone(),
                    })),
                },
                Right {
                    kind: Some(Kind::CanReadAs(CanReadAs {
                        party: primary_party.clone(),
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

    // Step 5: Build context for field value building
    let context = SubmissionContext {
        decentralized_party: decentralized_registrar.clone(),
        operator_party: operator.clone(),
        participant_parties: participant_parties.clone(),
        governance_threshold: network_config.governance_threshold() as i64,
    };

    // Step 6: Prepare submissions for each contract defined in config
    let mut submission_client = utils::create_submission_client(config).await?;
    let ledger_submissions_dir = dirs.workflow_dir.join(LEDGER_SUBMISSIONS_DIR);
    let prepared_dir = ledger_submissions_dir.join(PREPARED_DIR);
    fs::create_dir_all(&prepared_dir).await?;

    if app_config.contracts.is_empty() {
        tracing::warn!(
            "No contracts defined in application config, skipping submission preparation"
        );
        return Ok(());
    }

    for (idx, contract_def) in app_config.contracts.iter().enumerate() {
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
        count = app_config.contracts.len()
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
        FieldDefinition::ParticipantParty { index } => {
            let party = context
                .participant_parties
                .get(*index)
                .ok_or_else(|| anyhow::anyhow!("Participant index {index} out of bounds"))?;
            value::Sum::Party(party.clone())
        }
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
            // Set Party represented as GenMap<Party, Unit>
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
        FieldDefinition::Optional { inner } => {
            let inner_value = build_field_value(inner, context)?;
            value::Sum::Optional(Box::new(Optional {
                value: Some(Box::new(inner_value)),
            }))
        }
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
        FieldDefinition::GovernanceThreshold => value::Sum::Int64(context.governance_threshold),
    };

    Ok(Value { sum: Some(sum) })
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
            tracing::debug!("Found existing party: {party}", party = party_details.party);
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
