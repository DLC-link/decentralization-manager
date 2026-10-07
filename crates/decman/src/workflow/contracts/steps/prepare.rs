use canton_proto_rs::com::daml::ledger::api::v2::{
    Command, CreateCommand, GenMap, Identifier, Optional, Record, RecordField, Value, command,
    gen_map, interactive::PrepareSubmissionRequest, value,
};
use canton_proto_rs::com::digitalasset::canton::admin::participant::v30::{
    ListPackagesRequest, PackageDescription, package_service_client::PackageServiceClient,
};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    db::schema::SchemaRead,
    error::Result,
    utils,
    workflow::{
        contracts::{ContractDefinition, ContractsConfig, FieldDefinition},
        storage::{WorkflowStorage, artifact_kinds},
    },
};

/// Resolve package-name selectors once, before an invitation commits to a create.
/// Persisting the result prevents later package uploads changing the approved deployment.
pub async fn resolve_package_ids(
    config: &NodeConfig,
    contracts: &mut [ContractDefinition],
) -> Result {
    let packages = if contracts.iter().any(|c| c.package_id.starts_with('#')) {
        PackageServiceClient::new(config.admin_channel().await?)
            .list_packages(ListPackagesRequest {
                limit: 0,
                filter_name: String::new(),
            })
            .await?
            .into_inner()
            .package_descriptions
    } else {
        Vec::new()
    };
    for contract in contracts {
        contract.package_id = resolve_package_id(&contract.package_id, &packages)?;
    }
    Ok(())
}

fn resolve_package_id(reference: &str, packages: &[PackageDescription]) -> Result<String> {
    let Some(name) = reference.strip_prefix('#') else {
        anyhow::ensure!(
            reference.len() == 64 && reference.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid package ID {reference}"
        );
        return Ok(reference.to_ascii_lowercase());
    };
    let mut matches: Vec<_> = packages.iter().filter(|p| p.name == name).collect();
    matches.sort_by(|a, b| crate::server::compare_versions(&b.version, &a.version));
    let selected = matches
        .first()
        .ok_or_else(|| anyhow::anyhow!("Package {reference} is not installed"))?;
    anyhow::ensure!(
        !matches
            .iter()
            .skip(1)
            .any(|p| p.version == selected.version && p.package_id != selected.package_id),
        "Package {reference} has ambiguous IDs at version {}; specify an exact package ID",
        selected.version
    );
    resolve_package_id(&selected.package_id, &[])
}

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
/// * `contracts_config` - Contracts workflow configuration with party ID
/// * `token` - Authentication token for Ledger API
/// * `user_id` - User ID for Ledger API operations
pub async fn prepare_submissions(
    config: &NodeConfig,
    db: &SqlitePool,
    instance_name: &str,
    contracts_config: &ContractsConfig,
    token: &str,
    user_id: &str,
) -> Result {
    tracing::info!("Preparing submissions...");

    let context = submission_context(db, contracts_config).await?;
    let decentralized_registrar = &context.decentralized_party;
    let token_opt = Some(token.to_string());

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
                hashing_scheme_version: None,
                taps_max_passes: None,
            }))
            .await?
            .into_inner();

        // Persist as `varint(len)||proto` so reads via
        // `utils::read_first_message_from_bytes` round-trip cleanly. This is
        // the exact byte shape `utils::write_messages_to_file(&[m], path)`
        // would have written for a single message.
        let payload = utils::encode_length_prefixed_message(&prepared_submission);
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

async fn submission_context(
    db: &SqlitePool,
    contracts_config: &ContractsConfig,
) -> Result<SubmissionContext> {
    // Use the decentralized party ID from config
    let decentralized_registrar = contracts_config.decentralized_party_id.clone();
    tracing::debug!("Using decentralized party: {decentralized_registrar}");

    // Get participant parties from config (provided by API caller)
    let participant_parties: Vec<CantonId> = contracts_config.participant_parties.clone();

    // Use the dec party's OWN threshold (set at onboarding, baked into its
    // namespace definition), not a recomputed mesh majority. Contract
    // deployment is signed by the party, so it needs at least `threshold` of
    // its owners to sign — and the deployed governance contract should carry
    // that same threshold. This also replaces the former hardcoded
    // 3-participant minimum.
    let party_threshold = db
        .get_dec_parties_by_prefix(&decentralized_registrar.prefix)
        .await?
        .into_iter()
        .find(|p| p.party_id == decentralized_registrar.to_string())
        .map(|p| p.threshold)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Decentralized party {decentralized_registrar} not found in cache; \
                 refresh /decentralized-parties before deploying contracts"
            )
        })?;

    if participant_parties.is_empty() {
        anyhow::bail!("No participant parties provided in contracts config");
    }
    if (participant_parties.len() as i64) < party_threshold {
        anyhow::bail!(
            "Need at least {party_threshold} participant(s) to meet the party's threshold \
             for contract operations, found {count}",
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
    Ok(SubmissionContext {
        decentralized_party: decentralized_registrar.clone(),
        operator_party: operator.clone(),
        participant_parties: participant_parties.clone(),
        governance_threshold: party_threshold,
    })
}

/// Commit to every requested create before an operator accepts the invitation.
pub async fn deployment_intents(
    db: &SqlitePool,
    config: &ContractsConfig,
) -> Result<Vec<common::api::ContractDeploymentIntent>> {
    let context = submission_context(db, config).await?;
    config
        .contracts
        .iter()
        .map(|contract| {
            anyhow::ensure!(
                contract.package_id.len() == 64
                    && contract.package_id.bytes().all(|b| b.is_ascii_hexdigit()),
                "Contract {} must name a resolved package ID before invitation",
                contract.name
            );
            let argument = Value {
                sum: Some(value::Sum::Record(Record {
                    record_id: None,
                    fields: contract
                        .fields
                        .iter()
                        .map(|field| build_record_field(field, &context))
                        .collect::<Result<Vec<_>>>()?,
                })),
            };
            Ok(common::api::ContractDeploymentIntent {
                package_id: contract.package_id.clone(),
                module_name: contract.module_name.clone(),
                entity_name: contract.entity_name.clone(),
                argument_hash: crate::canton_hash::hash_value(&argument)?,
            })
        })
        .collect()
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

#[cfg(test)]
mod package_resolution_tests {
    use super::*;

    #[test]
    fn aliases_pin_the_newest_exact_package_and_reject_ambiguity() -> Result {
        let package = |name: &str, version: &str, id: &str| PackageDescription {
            name: name.into(),
            version: version.into(),
            package_id: id.repeat(64),
            ..Default::default()
        };
        let mut packages = vec![
            package("governance", "1.9.0", "a"),
            package("governance", "1.10.0", "b"),
            package("governance-other", "9.0.0", "c"),
        ];
        let pinned = resolve_package_id("#governance", &packages)?;
        assert_eq!(pinned, "b".repeat(64));
        packages.push(package("governance", "2.0.0", "d"));
        assert_eq!(resolve_package_id(&pinned, &packages)?, pinned);
        packages.push(package("governance", "2.0.0", "e"));
        assert!(resolve_package_id("#governance", &packages).is_err());
        assert!(resolve_package_id("#missing", &packages).is_err());
        assert!(resolve_package_id("not-a-package", &packages).is_err());
        Ok(())
    }
}
