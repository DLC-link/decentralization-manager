//! Contracts workflow over `SubmissionRound` (design D7).
//!
//! The proposer prepares one interactive submission per contract, opens a
//! `SubmissionRound` per prepared transaction, collects and verifies
//! `SubmissionSignature`s, and executes each round once with exactly the
//! verified set. A member checks the round against its accepted proposal and
//! the head P2P, recomputes the hash, and signs with its per-party key.
//!
//! The Canton calls follow the shape of the kept step modules
//! (`workflow/contracts/steps/{prepare,sign,execute}.rs`): the same
//! `PrepareSubmission` request, the same local re-hash before a signature,
//! the same `select_signer` backend choice, and one
//! `ExecuteSubmissionAndWaitForTransaction` per prepared transaction. The
//! difference is the transport: signatures travel as Daml contracts, not over
//! a direct connection between nodes.
//!
//! Every function in this file is either pure (unit-tested below) or a thin
//! wrapper over one Canton or database call. The step machine lives in
//! `engine/contracts.rs`.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        Command, CreateCommand, GenMap, Identifier, Optional, Record, RecordField,
        Signature as LedgerSignature, Transaction, Value, command, event, gen_map,
        interactive::{
            ExecuteSubmissionAndWaitForTransactionRequest, PartySignatures,
            PrepareSubmissionRequest, PrepareSubmissionResponse, PreparedTransaction,
            SinglePartySignatures, daml_transaction, transaction::v1,
        },
        value,
    },
    digitalasset::canton::{
        admin::participant::v30::{
            ListPackagesRequest, package_service_client::PackageServiceClient,
        },
        crypto::{
            admin::v30::{
                ListKeysFilters, ListMyKeysRequest, private_key_metadata,
                vault_service_client::VaultServiceClient,
            },
            v30::{
                CryptoKeyFormat, Signature as CantonSignature, SignatureFormat,
                SigningAlgorithmSpec, SigningKeySpec, SigningPublicKey, public_key,
            },
        },
        protocol::v30::PartyToParticipant,
        topology::admin::v30::{
            ListSynchronizerParametersStateRequest,
            topology_manager_read_service_client::TopologyManagerReadServiceClient,
        },
    },
};
use common::{
    api::{ContractDefinition, FieldDefinition},
    canton_id::CantonId,
    types::WorkflowRun,
};
use prost::Message;
use prost_types::Timestamp;
use serde::Deserialize;
use sqlx::SqlitePool;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{
    auth::{AuthRegistry, MockAuthRegistry, PartyAuthCredentials, WorkflowAuth},
    canton_hash::{self, HashingScheme},
    config::{CredentialKind, NodeConfig},
    db::{
        rows::DecPartyContractRow,
        schema::{Commitable, SchemaRead, SchemaWrite},
    },
    signing::{PreparedTransactionHash, SigningKeyContext, select_signer},
    utils,
    workflow::topology as legacy_topology,
};

use super::{
    OnLedger,
    daml::{
        ActiveContract, CoordinationClient, CoordinationTemplate, choices,
        codec::{
            ChoiceArgument, CloseArgs, SignArgs, SubmissionRoundRecord, SubmissionSignatureRecord,
            WorkflowKind, WorkflowProposalRecord, unit_argument,
        },
    },
    keys, topology,
    validation::key_fingerprints,
};

/// `max_record_time = preparation time + 20 h`.
pub const MAX_RECORD_TIME_HORIZON_MICROS: i64 = 20 * 3600 * 1_000_000;

/// Signers must finish 30 minutes before the window closes.
pub const DEADLINE_SAFETY_MARGIN_MICROS: i64 = 30 * 60 * 1_000_000;

/// The Canton and Splice default `preparationTimeRecordTimeTolerance`, used
/// when the synchronizer parameters cannot be read (design D7).
pub const DEFAULT_RECORD_TIME_TOLERANCE_MICROS: i64 = 24 * 3600 * 1_000_000;

/// One prepared interactive submission, ready to become a `SubmissionRound`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedRound {
    pub index: i64,
    pub description: String,
    /// Lowercase hex of the serialized `PreparedTransaction`.
    pub prepared_transaction_hex: String,
    /// Lowercase hex of `prepared_transaction_hash`.
    pub prepared_hash_hex: String,
    pub hashing_scheme_version: i64,
    /// Micros since the epoch.
    pub preparation_time: i64,
    pub max_record_time: i64,
    pub deadline: i64,
}

/// A `SubmissionSignature` the proposer verified locally.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedSignature {
    pub signed_by: String,
    pub signature: Vec<u8>,
    pub format: String,
    pub algorithm: String,
    pub participant_id: String,
}

/// One contract an executed round created, in the shape of the
/// `dec_party_contract` cache.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedContract {
    pub contract_id: String,
    /// `Module:Entity`, as the parties refresh writes it.
    pub template_id: String,
    pub package_id: String,
    pub package_name: String,
    pub package_version: String,
    /// RFC 3339 text, as the parties refresh writes it.
    pub created_at: String,
}

/// What one `ExecuteSubmissionAndWaitForTransaction` committed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutedRound {
    pub update_id: String,
    pub created: Vec<CreatedContract>,
}

/// The contracts fields of a coordinator row's `config_json`
/// (`engine::coordinator_config_json` for `StartRequest::Contracts`).
#[derive(Clone, Debug, Deserialize)]
pub struct ContractsRunConfig {
    pub decentralized_party_id: CantonId,
    #[serde(default)]
    pub participant_ids: Vec<CantonId>,
    #[serde(default)]
    pub participant_parties: Vec<CantonId>,
    pub operator_party: CantonId,
    #[serde(default)]
    pub contracts: Vec<ContractDefinition>,
}

impl ContractsRunConfig {
    /// # Errors
    /// Returns an error when the row's `config_json` is not a contracts
    /// configuration.
    pub fn from_run(run: &WorkflowRun) -> Result<Self> {
        serde_json::from_str(&run.config_json).with_context(|| {
            format!(
                "config_json of {} is not a contracts run",
                run.instance_name
            )
        })
    }
}

// ---------------------------------------------------------------------------
// Time arithmetic
// ---------------------------------------------------------------------------

/// `deadline = min(max_record_time, preparation_time + tolerance) - 30 min`.
pub fn deadline_for(preparation_time: i64, max_record_time: i64, tolerance_micros: i64) -> i64 {
    max_record_time
        .min(preparation_time.saturating_add(tolerance_micros))
        .saturating_sub(DEADLINE_SAFETY_MARGIN_MICROS)
}

/// A protobuf `Duration` in micros. Negative or oversized values saturate.
pub fn duration_micros(duration: &prost_types::Duration) -> i64 {
    duration
        .seconds
        .saturating_mul(1_000_000)
        .saturating_add(i64::from(duration.nanos) / 1_000)
}

fn timestamp_from_micros(micros: i64) -> Timestamp {
    Timestamp {
        seconds: micros.div_euclid(1_000_000),
        nanos: i32::try_from(micros.rem_euclid(1_000_000) * 1_000).unwrap_or(0),
    }
}

/// RFC 3339 text of a Ledger API timestamp, for the contract cache.
fn rfc3339_of(ts: &Timestamp) -> String {
    chrono::DateTime::from_timestamp(ts.seconds, u32::try_from(ts.nanos).unwrap_or(0))
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

/// The live `preparationTimeRecordTimeTolerance` of the synchronizer, in
/// micros, from the head `SynchronizerParametersState`.
///
/// # Errors
/// Returns an error when the parameters cannot be read or carry no
/// tolerance.
pub async fn read_record_time_tolerance(config: &NodeConfig, synchronizer_id: &str) -> Result<i64> {
    let mut client = TopologyManagerReadServiceClient::new(config.admin_channel().await?)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);
    let response = client
        .list_synchronizer_parameters_state(tonic::Request::new(
            ListSynchronizerParametersStateRequest {
                base_query: Some(legacy_topology::head_state_query(synchronizer_id)),
                filter_synchronizer_id: String::new(),
            },
        ))
        .await
        .context("ListSynchronizerParametersState")?
        .into_inner();
    // Several results can coexist during a parameter change; the highest
    // serial is the state in force.
    let latest = response
        .results
        .into_iter()
        .max_by_key(|r| r.context.as_ref().map_or(0, |c| c.serial))
        .context("the synchronizer store holds no SynchronizerParametersState")?;
    let parameters = latest
        .item
        .context("SynchronizerParametersState without parameters")?;
    let tolerance = parameters
        .preparation_time_record_time_tolerance
        .context("preparationTimeRecordTimeTolerance is unset")?;
    Ok(duration_micros(&tolerance))
}

/// [`read_record_time_tolerance`], or the 24 h default when the read fails
/// or reports a non-positive value. A wrong tolerance only moves the
/// deadline, so a fallback is safe: Canton enforces the real bound.
pub async fn record_time_tolerance_or_default(config: &NodeConfig, synchronizer_id: &str) -> i64 {
    match read_record_time_tolerance(config, synchronizer_id).await {
        Ok(t) if t > 0 => t,
        Ok(t) => {
            tracing::warn!(
                tolerance = t,
                "non-positive record-time tolerance; using 24 h"
            );
            DEFAULT_RECORD_TIME_TOLERANCE_MICROS
        }
        Err(e) => {
            tracing::warn!(error = %e, "record-time tolerance unreadable; using 24 h");
            DEFAULT_RECORD_TIME_TOLERANCE_MICROS
        }
    }
}

// ---------------------------------------------------------------------------
// Dec-party credentials
// ---------------------------------------------------------------------------

/// Ledger API credentials for the decentralized party, rebuilt from the
/// `party_credentials` table.
///
/// TODO(onledger/mod.rs): `OnLedger` holds the live `WorkflowAuth` in a
/// private field. Expose it as `OnLedger::auth()` and replace this rebuild,
/// which authenticates against Keycloak once per call.
pub(crate) async fn dec_party_credentials(
    ol: &OnLedger,
    dec_party_id: &CantonId,
) -> Result<PartyAuthCredentials> {
    let rows = ol.db().get_all_party_credentials().await?;
    let auth = if ol.test_mode() {
        WorkflowAuth::Mock(Arc::new(MockAuthRegistry::with_config(
            Arc::new(RwLock::new(rows)),
            &ol.config().insecure_auth,
        )))
    } else {
        let row = rows
            .iter()
            .find(|r| r.kind == CredentialKind::Decparty && r.dec_party_id == *dec_party_id)
            .with_context(|| format!("no party credentials configured for {dec_party_id}"))?;
        WorkflowAuth::Keycloak(Arc::new(
            AuthRegistry::new(std::slice::from_ref(row)).await?,
        ))
    };
    Ok(auth.get_credentials(dec_party_id).await?)
}

// ---------------------------------------------------------------------------
// Prepare (proposer)
// ---------------------------------------------------------------------------

/// Values the field builder substitutes into a create command. Same shape
/// as `SubmissionContext` in `workflow/contracts/steps/prepare.rs`.
struct SubmissionContext {
    decentralized_party: CantonId,
    operator_party: CantonId,
    participant_parties: Vec<CantonId>,
    governance_threshold: i64,
}

/// `PrepareSubmission` once per contract definition as the party, with
/// `max_record_time = now + 20 h`, and compute each round's deadline.
/// Indices are `0..contracts.len()`.
///
/// # Errors
/// Returns an error when a prepare call fails or a prepared transaction is
/// not one this node would sign itself.
pub async fn prepare_rounds(
    ol: &OnLedger,
    run: &WorkflowRun,
    dec_party_id: &CantonId,
    contracts: &[ContractDefinition],
) -> Result<Vec<PreparedRound>> {
    let indexed: Vec<(i64, &ContractDefinition)> = contracts
        .iter()
        .enumerate()
        .map(|(i, c)| (i64::try_from(i).unwrap_or(i64::MAX), c))
        .collect();
    prepare_rounds_at(ol, run, dec_party_id, &indexed).await
}

/// [`prepare_rounds`] for chosen indices, so an expired round is re-prepared
/// alone.
///
/// # Errors
/// As [`prepare_rounds`].
pub async fn prepare_rounds_at(
    ol: &OnLedger,
    run: &WorkflowRun,
    dec_party_id: &CantonId,
    contracts: &[(i64, &ContractDefinition)],
) -> Result<Vec<PreparedRound>> {
    if contracts.is_empty() {
        return Ok(Vec::new());
    }
    let config = ol.config();
    let run_config = ContractsRunConfig::from_run(run)?;
    let sync_id = utils::get_synchronizer_id(config).await?;
    let tolerance = record_time_tolerance_or_default(config, &sync_id).await;
    let context = SubmissionContext {
        decentralized_party: dec_party_id.clone(),
        operator_party: run_config.operator_party.clone(),
        participant_parties: run_config.participant_parties.clone(),
        governance_threshold: party_threshold(ol, &sync_id, dec_party_id).await?,
    };
    let credentials = dec_party_credentials(ol, dec_party_id).await?;
    let mut client = utils::create_submission_client(config, Some(credentials.token)).await?;

    let mut rounds = Vec::with_capacity(contracts.len());
    for (index, definition) in contracts {
        let command = create_command(definition, &context)?;
        let now = super::now_micros();
        let max_record_time = now.saturating_add(MAX_RECORD_TIME_HORIZON_MICROS);
        let response = client
            .prepare_submission(tonic::Request::new(PrepareSubmissionRequest {
                user_id: credentials.user_id.clone(),
                // A re-prepared round must not collide with the command
                // deduplication of an earlier attempt.
                command_id: format!("{}-{}", definition.id, Uuid::new_v4()),
                commands: vec![command],
                min_ledger_time: None,
                max_record_time: Some(timestamp_from_micros(max_record_time)),
                act_as: vec![dec_party_id.to_string()],
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
            .await
            .with_context(|| format!("PrepareSubmission for {}", definition.name))?
            .into_inner();
        rounds.push(prepared_round(
            *index,
            &definition.name,
            &response,
            dec_party_id,
            tolerance,
        )?);
    }
    Ok(rounds)
}

/// The party's DND threshold, for `GovernanceThreshold` fields without an
/// explicit value. The head DND is authoritative; the parties cache is the
/// fallback for a party whose namespace is not decentralized.
async fn party_threshold(ol: &OnLedger, sync_id: &str, dec_party_id: &CantonId) -> Result<i64> {
    let namespace = dec_party_id.namespace.to_hex();
    if let Some(dnd) = topology::read_accepted_dnd(ol.config(), sync_id, &namespace).await? {
        return Ok(i64::from(dnd.mapping.threshold));
    }
    ol.db()
        .get_dec_parties_by_prefix(&dec_party_id.prefix)
        .await?
        .into_iter()
        .find(|p| p.party_id == dec_party_id.to_string())
        .map(|p| p.threshold)
        .with_context(|| {
            format!(
                "{dec_party_id} has no DecentralizedNamespaceDefinition and is not in the parties \
                 cache; refresh /decentralized-parties before deploying contracts"
            )
        })
}

/// Turn a prepare response into a round: the hash is recomputed locally, the
/// transaction must be time-independent, and the signed `maxRecordTime`
/// must be present (design D7).
fn prepared_round(
    index: i64,
    description: &str,
    response: &PrepareSubmissionResponse,
    dec_party_id: &CantonId,
    tolerance_micros: i64,
) -> Result<PreparedRound> {
    canton_hash::verify_prepared_submission(response)?;
    let prepared = response
        .prepared_transaction
        .as_ref()
        .context("PrepareSubmission returned no transaction")?;
    let metadata = prepared
        .metadata
        .as_ref()
        .context("prepared transaction has no metadata")?;
    check_metadata(metadata, dec_party_id)?;
    let preparation_time =
        i64::try_from(metadata.preparation_time).context("preparationTime does not fit an i64")?;
    let max_record_time = metadata
        .max_record_time
        .context("PrepareSubmission dropped maxRecordTime")
        .and_then(|t| i64::try_from(t).context("maxRecordTime does not fit an i64"))?;
    Ok(PreparedRound {
        index,
        description: description.to_string(),
        prepared_transaction_hex: hex::encode(prepared.encode_to_vec()),
        prepared_hash_hex: hex::encode(&response.prepared_transaction_hash),
        hashing_scheme_version: i64::from(response.hashing_scheme_version),
        preparation_time,
        max_record_time,
        deadline: deadline_for(preparation_time, max_record_time, tolerance_micros),
    })
}

/// The metadata rules both sides apply: the transaction acts as the party
/// only, carries a `maxRecordTime`, and does not depend on ledger time (a
/// time-dependent transaction has a window of about a minute, which no
/// human approval round can meet).
fn check_metadata(
    metadata: &canton_proto_rs::com::daml::ledger::api::v2::interactive::Metadata,
    dec_party_id: &CantonId,
) -> Result<()> {
    let act_as = metadata
        .submitter_info
        .as_ref()
        .map(|s| s.act_as.as_slice())
        .unwrap_or_default();
    let expected = dec_party_id.to_string();
    ensure!(
        act_as == [expected.clone()],
        "the prepared transaction acts as {act_as:?}, not [{expected}]"
    );
    ensure!(
        metadata.max_record_time.is_some(),
        "the prepared transaction carries no maxRecordTime"
    );
    ensure!(
        metadata.min_ledger_effective_time.is_none()
            && metadata.max_ledger_effective_time.is_none(),
        "the prepared transaction depends on ledger time; its signing window is too short"
    );
    Ok(())
}

/// The create command of one contract definition.
fn create_command(definition: &ContractDefinition, context: &SubmissionContext) -> Result<Command> {
    let fields = definition
        .fields
        .iter()
        .map(|f| build_record_field(f, context))
        .collect::<Result<Vec<_>>>()?;
    Ok(Command {
        command: Some(command::Command::Create(CreateCommand {
            template_id: Some(Identifier {
                package_id: definition.package_id.clone(),
                module_name: definition.module_name.clone(),
                entity_name: definition.entity_name.clone(),
            }),
            create_arguments: Some(Record {
                record_id: None,
                fields,
            }),
        })),
    })
}

// TODO(workflow/contracts/steps/prepare.rs): `build_record_field` and
// `build_field_value` are private there. Make them `pub(crate)` and delete
// this copy.
fn build_record_field(field: &FieldDefinition, context: &SubmissionContext) -> Result<RecordField> {
    Ok(RecordField {
        label: String::new(),
        value: Some(build_field_value(field, context)?),
    })
}

fn party_value(party: &CantonId) -> Value {
    Value {
        sum: Some(value::Sum::Party(party.to_string())),
    }
}

fn party_set(parties: &[CantonId]) -> GenMap {
    let unit = Value {
        sum: Some(value::Sum::Unit(())),
    };
    GenMap {
        entries: parties
            .iter()
            .map(|party| gen_map::Entry {
                key: Some(party_value(party)),
                value: Some(unit.clone()),
            })
            .collect(),
    }
}

fn build_field_value(field: &FieldDefinition, context: &SubmissionContext) -> Result<Value> {
    let sum = match field {
        FieldDefinition::DecentralizedParty => {
            value::Sum::Party(context.decentralized_party.to_string())
        }
        FieldDefinition::OperatorParty => value::Sum::Party(context.operator_party.to_string()),
        FieldDefinition::ParticipantParty { id } => value::Sum::Party(id.to_string()),
        FieldDefinition::Text { value: text } => value::Sum::Text(text.clone()),
        FieldDefinition::Int64 { value: num } => value::Sum::Int64(*num),
        FieldDefinition::Bool { value: b } => value::Sum::Bool(*b),
        // Instrument record: { admin: Party, id: Text }
        FieldDefinition::Instrument { id } => value::Sum::Record(Record {
            record_id: None,
            fields: vec![
                RecordField {
                    label: String::new(),
                    value: Some(party_value(&context.decentralized_party)),
                },
                RecordField {
                    label: String::new(),
                    value: Some(Value {
                        sum: Some(value::Sum::Text(id.clone())),
                    }),
                },
            ],
        }),
        // Raw GenMap<Party, Unit> for CBTC-style contracts.
        FieldDefinition::AttestorsSet => {
            value::Sum::GenMap(party_set(&context.participant_parties))
        }
        // DA.Set.Types:Set Party is a record with one "map" field.
        FieldDefinition::PartySet { parties } => value::Sum::Record(Record {
            record_id: None,
            fields: vec![RecordField {
                label: "map".to_string(),
                value: Some(Value {
                    sum: Some(value::Sum::GenMap(party_set(parties))),
                }),
            }],
        }),
        // DA.Time.Types:RelTime is a record with one "microseconds" field.
        FieldDefinition::RelTime { microseconds } => value::Sum::Record(Record {
            record_id: None,
            fields: vec![RecordField {
                label: "microseconds".to_string(),
                value: Some(Value {
                    sum: Some(value::Sum::Int64(*microseconds)),
                }),
            }],
        }),
        FieldDefinition::Optional { inner } => value::Sum::Optional(Box::new(Optional {
            value: Some(Box::new(build_field_value(inner, context)?)),
        })),
        FieldDefinition::None => value::Sum::Optional(Box::new(Optional { value: None })),
        FieldDefinition::Record { fields } => value::Sum::Record(Record {
            record_id: None,
            fields: fields
                .iter()
                .map(|f| build_record_field(f, context))
                .collect::<Result<Vec<_>>>()?,
        }),
        FieldDefinition::GovernanceThreshold { value } => {
            value::Sum::Int64(value.unwrap_or(context.governance_threshold))
        }
    };
    Ok(Value { sum: Some(sum) })
}

// ---------------------------------------------------------------------------
// Rounds on the ledger
// ---------------------------------------------------------------------------

/// Create one `SubmissionRound` per prepared transaction, observed by
/// `signers`, and return the contract ids in the same order.
///
/// # Errors
/// Returns an error when a create fails; rounds created before the failure
/// stay on the ledger and the caller finds them on its next read.
pub async fn open_rounds(
    client: &CoordinationClient,
    run_id: &str,
    signers: &[CantonId],
    dec_party_id: &CantonId,
    act_as: &CantonId,
    rounds: &[PreparedRound],
) -> Result<Vec<String>> {
    let mut cids = Vec::with_capacity(rounds.len());
    for round in rounds {
        let record = SubmissionRoundRecord {
            proposer: client.node_party().clone(),
            run_id: run_id.to_string(),
            index: round.index,
            signers: signers.to_vec(),
            dec_party_id: dec_party_id.to_string(),
            act_as: act_as.to_string(),
            description: round.description.clone(),
            prepared_transaction_hex: round.prepared_transaction_hex.clone(),
            prepared_hash_hex: round.prepared_hash_hex.clone(),
            hashing_scheme_version: round.hashing_scheme_version,
            preparation_time: round.preparation_time,
            max_record_time: round.max_record_time,
            deadline: round.deadline,
        };
        let cid = client
            .create(&record)
            .await
            .with_context(|| format!("create SubmissionRound {}", round.index))?;
        tracing::info!(run_id, index = round.index, cid = %cid, deadline = round.deadline, "SubmissionRound opened");
        cids.push(cid);
    }
    Ok(cids)
}

/// Every active `SubmissionRound` of one run visible to this node.
///
/// # Errors
/// Returns an error when the read fails.
pub async fn read_rounds_for_run(
    client: &CoordinationClient,
    run_id: &str,
) -> Result<Vec<ActiveContract<SubmissionRoundRecord>>> {
    Ok(client
        .list_active::<SubmissionRoundRecord>()
        .await?
        .into_iter()
        .filter(|r| r.record.run_id == run_id)
        .collect())
}

/// Every active `SubmissionSignature` of one round.
///
/// # Errors
/// Returns an error when the read fails.
pub async fn read_signatures_for_round(
    client: &CoordinationClient,
    round_cid: &str,
) -> Result<Vec<ActiveContract<SubmissionSignatureRecord>>> {
    Ok(client
        .list_active::<SubmissionSignatureRecord>()
        .await?
        .into_iter()
        .filter(|s| s.record.round == round_cid)
        .collect())
}

/// Exercise `SubmissionRound_Close` with a result text.
///
/// # Errors
/// Returns an error when the submission fails.
pub async fn close_round(client: &CoordinationClient, round_cid: &str, result: &str) -> Result<()> {
    let args = CloseArgs {
        result: result.to_string(),
    };
    client
        .exercise(
            CoordinationTemplate::SubmissionRound,
            round_cid,
            choices::SUBMISSION_ROUND_CLOSE,
            args.to_value(),
        )
        .await
        .context("SubmissionRound_Close")?;
    Ok(())
}

/// Archive this node's `SubmissionSignature`s whose round is no longer
/// active (the housekeeping sweep of design D10).
///
/// # Errors
/// Returns an error when the read fails; a failed archive is logged.
pub async fn archive_own_signatures(
    client: &CoordinationClient,
    active_round_cids: &std::collections::HashSet<String>,
) -> Result<usize> {
    let me = client.node_party();
    let mut archived = 0;
    for s in client
        .list_active::<SubmissionSignatureRecord>()
        .await?
        .iter()
        .filter(|s| s.record.signer == *me && !active_round_cids.contains(&s.record.round))
    {
        match client
            .exercise(
                CoordinationTemplate::SubmissionSignature,
                &s.contract_id,
                choices::SUBMISSION_SIGNATURE_ARCHIVE,
                unit_argument(),
            )
            .await
        {
            Ok(_) => archived += 1,
            Err(e) => tracing::warn!(
                contract_id = %s.contract_id,
                error = %e,
                "archiving a stale SubmissionSignature failed; retrying next tick"
            ),
        }
    }
    Ok(archived)
}

// ---------------------------------------------------------------------------
// Signature verification (proposer)
// ---------------------------------------------------------------------------

fn format_name(value: i32) -> Result<String> {
    SignatureFormat::try_from(value)
        .map(|f| f.as_str_name().to_string())
        .map_err(|_| anyhow!("unknown SignatureFormat {value}"))
}

fn algorithm_name(value: i32) -> Result<String> {
    SigningAlgorithmSpec::try_from(value)
        .map(|a| a.as_str_name().to_string())
        .map_err(|_| anyhow!("unknown SigningAlgorithmSpec {value}"))
}

/// Proposer side: `signedBy` is one of the head P2P's party signing keys and
/// the signature verifies against that key over `hash` (Ed25519 CONCAT,
/// ECDSA DER).
///
/// # Errors
/// Returns an error when the key is unknown or the signature does not
/// verify.
pub fn verify_signature(
    head_p2p: &PartyToParticipant,
    signature: &SubmissionSignatureRecord,
    hash: &[u8],
) -> Result<VerifiedSignature> {
    let bytes = hex::decode(&signature.signature_hex).context("signatureHex is not hex")?;
    verify_parts(
        head_p2p,
        &signature.signed_by,
        &signature.format,
        &signature.algorithm,
        bytes,
        hash,
        &signature.participant_id,
    )
}

/// The proposer's own Canton signature, verified like a member's.
///
/// # Errors
/// As [`verify_signature`].
pub fn verify_canton_signature(
    head_p2p: &PartyToParticipant,
    signature: &CantonSignature,
    hash: &[u8],
    participant_id: &str,
) -> Result<VerifiedSignature> {
    verify_parts(
        head_p2p,
        &signature.signed_by,
        &format_name(signature.format)?,
        &algorithm_name(signature.signing_algorithm_spec)?,
        signature.signature.clone(),
        hash,
        participant_id,
    )
}

fn verify_parts(
    head_p2p: &PartyToParticipant,
    signed_by: &str,
    format: &str,
    algorithm: &str,
    signature: Vec<u8>,
    hash: &[u8],
    participant_id: &str,
) -> Result<VerifiedSignature> {
    let keys = head_p2p
        .party_signing_keys
        .as_ref()
        .context("the head PartyToParticipant carries no party_signing_keys")?;
    let key = keys
        .keys
        .iter()
        .find(|k| utils::compute_fingerprint(k) == signed_by)
        .with_context(|| format!("signedBy {signed_by} is not a party signing key"))?;
    let format = SignatureFormat::from_str_name(format)
        .with_context(|| format!("unknown signature format `{format}`"))?;
    let algorithm = SigningAlgorithmSpec::from_str_name(algorithm)
        .with_context(|| format!("unknown signing algorithm `{algorithm}`"))?;
    verify_raw(key, format, algorithm, &signature, hash)
        .with_context(|| format!("signature by {signed_by} does not verify"))?;
    Ok(VerifiedSignature {
        signed_by: signed_by.to_string(),
        signature,
        format: format.as_str_name().to_string(),
        algorithm: algorithm.as_str_name().to_string(),
        participant_id: participant_id.to_string(),
    })
}

/// Verify `signature` over `message` with `key`, in the format Canton
/// requires for the algorithm (`Signing.scala`: Ed25519 takes CONCAT only,
/// ECDSA takes DER only).
fn verify_raw(
    key: &SigningPublicKey,
    format: SignatureFormat,
    algorithm: SigningAlgorithmSpec,
    signature: &[u8],
    message: &[u8],
) -> Result<()> {
    let key_spec = SigningKeySpec::try_from(key.key_spec).unwrap_or(SigningKeySpec::Unspecified);
    match algorithm {
        SigningAlgorithmSpec::Ed25519 => {
            ensure!(
                format == SignatureFormat::Concat,
                "Ed25519 needs SIGNATURE_FORMAT_CONCAT"
            );
            ensure!(
                key_spec == SigningKeySpec::EcCurve25519,
                "key spec {} is not Ed25519",
                key_spec.as_str_name()
            );
            let raw = ed25519_raw_public_key(key)?;
            let verifying = ed25519_dalek::VerifyingKey::from_bytes(&raw)
                .map_err(|e| anyhow!("invalid Ed25519 public key: {e}"))?;
            let signature = ed25519_dalek::Signature::from_slice(signature)
                .map_err(|e| anyhow!("invalid Ed25519 signature: {e}"))?;
            verifying
                .verify_strict(message, &signature)
                .map_err(|e| anyhow!("Ed25519 verification failed: {e}"))
        }
        SigningAlgorithmSpec::EcDsaSha256 => {
            use p256::{ecdsa::signature::Verifier, pkcs8::DecodePublicKey};
            ensure!(
                format == SignatureFormat::Der,
                "ECDSA needs SIGNATURE_FORMAT_DER"
            );
            ensure!(
                key_spec == SigningKeySpec::EcP256,
                "key spec {} is not P-256",
                key_spec.as_str_name()
            );
            let verifying = p256::ecdsa::VerifyingKey::from_public_key_der(&key.public_key)
                .map_err(|e| anyhow!("invalid P-256 public key: {e}"))?;
            let signature = p256::ecdsa::DerSignature::from_bytes(signature)
                .map_err(|e| anyhow!("invalid DER signature: {e}"))?;
            verifying
                .verify(message, &signature)
                .map_err(|e| anyhow!("ECDSA verification failed: {e}"))
        }
        other => bail!(
            "signing algorithm {} is not supported by this node",
            other.as_str_name()
        ),
    }
}

/// The 32 raw bytes of an Ed25519 key, from its X.509 SPKI or raw form.
fn ed25519_raw_public_key(key: &SigningPublicKey) -> Result<[u8; 32]> {
    use x509_parser::prelude::*;
    let raw: Vec<u8> = match CryptoKeyFormat::try_from(key.format) {
        Ok(CryptoKeyFormat::DerX509SubjectPublicKeyInfo) => {
            let (_, spki) = SubjectPublicKeyInfo::from_der(&key.public_key)
                .map_err(|e| anyhow!("Ed25519 key is not an X.509 SubjectPublicKeyInfo: {e}"))?;
            spki.subject_public_key.data.to_vec()
        }
        Ok(CryptoKeyFormat::Raw) => key.public_key.clone(),
        _ => bail!("unsupported Ed25519 key format {}", key.format),
    };
    <[u8; 32]>::try_from(raw.as_slice())
        .map_err(|_| anyhow!("Ed25519 public key has {} bytes, not 32", raw.len()))
}

/// One verified signature per fingerprint; the first wins.
pub fn dedupe_verified(
    signatures: impl IntoIterator<Item = VerifiedSignature>,
) -> BTreeMap<String, VerifiedSignature> {
    let mut out = BTreeMap::new();
    for s in signatures {
        out.entry(s.signed_by.clone()).or_insert(s);
    }
    out
}

/// The distinct verified signatures of one round: every on-ledger
/// `SubmissionSignature` that verifies against the head P2P, plus the
/// proposer's own signature when given. Invalid signatures are logged and
/// skipped; they never count (design D7).
///
/// # Errors
/// Returns an error when the ledger read fails or the round's hash is not
/// hex.
pub async fn verified_signatures_for_round(
    client: &CoordinationClient,
    head_p2p: &PartyToParticipant,
    round: &ActiveContract<SubmissionRoundRecord>,
    own: Option<(&CantonSignature, &str)>,
) -> Result<BTreeMap<String, VerifiedSignature>> {
    let hash =
        hex::decode(&round.record.prepared_hash_hex).context("preparedHashHex is not hex")?;
    let mut verified = Vec::new();
    for signature in read_signatures_for_round(client, &round.contract_id).await? {
        match verify_signature(head_p2p, &signature.record, &hash) {
            Ok(v) => verified.push(v),
            Err(e) => tracing::warn!(
                round = %round.contract_id,
                signer = %signature.record.signer,
                signed_by = %signature.record.signed_by,
                error = %e,
                "SubmissionSignature does not verify; not counted"
            ),
        }
    }
    if let Some((signature, participant_id)) = own {
        match verify_canton_signature(head_p2p, signature, &hash, participant_id) {
            Ok(v) => verified.push(v),
            Err(e) => tracing::warn!(
                round = %round.contract_id,
                error = %e,
                "this node's own signature does not verify; not counted"
            ),
        }
    }
    Ok(dedupe_verified(verified))
}

/// The signature count a round needs: `party_signing_keys.threshold`.
///
/// # Errors
/// Returns an error when the head P2P has no signing keys (fail closed).
pub fn required_signatures(head_p2p: &PartyToParticipant) -> Result<usize> {
    let keys = head_p2p
        .party_signing_keys
        .as_ref()
        .context("the head PartyToParticipant carries no party_signing_keys; contracts need threshold signatures")?;
    ensure!(keys.threshold >= 1, "party_signing_keys.threshold is 0");
    Ok(usize::try_from(keys.threshold).unwrap_or(usize::MAX))
}

// ---------------------------------------------------------------------------
// Execute (proposer)
// ---------------------------------------------------------------------------

/// Decode the `PreparedTransaction` a round carries.
///
/// # Errors
/// Returns an error when the hex or the protobuf is malformed.
pub fn decode_prepared_transaction(round: &SubmissionRoundRecord) -> Result<PreparedTransaction> {
    let bytes = hex::decode(&round.prepared_transaction_hex)
        .context("preparedTransactionHex is not hex")?;
    PreparedTransaction::decode(bytes.as_slice())
        .context("preparedTransactionHex is not a PreparedTransaction")
}

fn ledger_signature(v: &VerifiedSignature) -> Result<LedgerSignature> {
    let format = SignatureFormat::from_str_name(&v.format)
        .with_context(|| format!("unknown signature format `{}`", v.format))?;
    let algorithm = SigningAlgorithmSpec::from_str_name(&v.algorithm)
        .with_context(|| format!("unknown signing algorithm `{}`", v.algorithm))?;
    Ok(LedgerSignature {
        format: format as i32,
        signature: v.signature.clone(),
        signed_by: v.signed_by.clone(),
        signing_algorithm_spec: algorithm as i32,
    })
}

/// `ExecuteSubmissionAndWaitForTransaction` with exactly the verified set.
/// Same request shape as `workflow/contracts/steps/execute.rs`.
///
/// # Errors
/// Returns an error when the execute call fails. The caller must not retry
/// blindly: the mediator rejects a second execution of the same prepared
/// transaction.
pub async fn execute_round_with_events(
    ol: &OnLedger,
    dec_party_id: &CantonId,
    round: &ActiveContract<SubmissionRoundRecord>,
    signatures: &BTreeMap<String, VerifiedSignature>,
) -> Result<ExecutedRound> {
    ensure!(
        !signatures.is_empty(),
        "no verified signatures to execute with"
    );
    let prepared = decode_prepared_transaction(&round.record)?;
    let ledger_signatures = signatures
        .values()
        .map(ledger_signature)
        .collect::<Result<Vec<_>>>()?;
    let credentials = dec_party_credentials(ol, dec_party_id).await?;
    let mut client = utils::create_submission_client(ol.config(), Some(credentials.token)).await?;
    let request = ExecuteSubmissionAndWaitForTransactionRequest {
        prepared_transaction: Some(prepared),
        party_signatures: Some(PartySignatures {
            signatures: vec![SinglePartySignatures {
                party: dec_party_id.to_string(),
                signatures: ledger_signatures,
            }],
        }),
        deduplication_period: None,
        submission_id: Uuid::new_v4().to_string(),
        user_id: credentials.user_id,
        hashing_scheme_version: i32::try_from(round.record.hashing_scheme_version)
            .context("hashingSchemeVersion does not fit an i32")?,
        min_ledger_time: None,
        transaction_format: None,
    };
    let response = client
        .execute_submission_and_wait_for_transaction(tonic::Request::new(request))
        .await
        .with_context(|| format!("ExecuteSubmission for round {}", round.record.index))?
        .into_inner();
    // Events come back only when the executing participant hosts the
    // party. The coordinator does (preflight), but an empty set is not an
    // error: the commit already happened.
    let versions = package_inventory_by_id(ol.config())
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "package inventory unreadable; cache rows get no version");
            HashMap::new()
        });
    Ok(match response.transaction {
        Some(tx) => ExecutedRound {
            update_id: tx.update_id.clone(),
            created: created_contracts_of(&tx, &versions),
        },
        None => ExecutedRound {
            update_id: String::new(),
            created: Vec::new(),
        },
    })
}

/// [`execute_round_with_events`], returning the update id only.
///
/// # Errors
/// As [`execute_round_with_events`].
pub async fn execute_round(
    ol: &OnLedger,
    dec_party_id: &CantonId,
    round: &ActiveContract<SubmissionRoundRecord>,
    signatures: &BTreeMap<String, VerifiedSignature>,
) -> Result<String> {
    execute_round_with_events(ol, dec_party_id, round, signatures)
        .await
        .map(|e| e.update_id)
}

/// The contracts a committed transaction created, in cache shape.
/// `versions` maps a package id to `(name, version)`.
pub fn created_contracts_of(
    transaction: &Transaction,
    versions: &HashMap<String, (String, String)>,
) -> Vec<CreatedContract> {
    transaction
        .events
        .iter()
        .filter_map(|e| match e.event.as_ref()? {
            event::Event::Created(created) => Some(created),
            _ => None,
        })
        .map(|created| {
            let template = created.template_id.as_ref();
            let package_id = template.map(|t| t.package_id.clone()).unwrap_or_default();
            let inventory = versions.get(&package_id);
            let package_name = if created.package_name.is_empty() {
                inventory.map(|(name, _)| name.clone()).unwrap_or_default()
            } else {
                created.package_name.clone()
            };
            CreatedContract {
                contract_id: created.contract_id.clone(),
                template_id: template
                    .map(|t| format!("{}:{}", t.module_name, t.entity_name))
                    .unwrap_or_default(),
                package_id,
                package_name,
                package_version: inventory.map(|(_, v)| v.clone()).unwrap_or_default(),
                created_at: created
                    .created_at
                    .as_ref()
                    .map(rfc3339_of)
                    .unwrap_or_default(),
            }
        })
        .collect()
}

/// `package_id -> (name, version)` from the participant's package service.
///
/// TODO(server/package_inventory): `fetch_package_id_to_name` drops the
/// version; extend it and delete this copy.
async fn package_inventory_by_id(config: &NodeConfig) -> Result<HashMap<String, (String, String)>> {
    let mut client = PackageServiceClient::new(config.admin_channel().await?);
    let response = client
        .list_packages(tonic::Request::new(ListPackagesRequest {
            limit: 0,
            filter_name: String::new(),
        }))
        .await
        .context("ListPackages")?
        .into_inner();
    Ok(response
        .package_descriptions
        .into_iter()
        .map(|p| (p.package_id, (p.name, p.version)))
        .collect())
}

/// Merge created contracts into the `dec_party_contract` cache, so the
/// parties view shows them before the next refresh. Existing rows win on a
/// duplicate contract id.
///
/// # Errors
/// Returns an error when the read or the write fails.
pub async fn record_created_contracts(
    db: &SqlitePool,
    dec_party_id: &CantonId,
    created: &[CreatedContract],
) -> Result<usize> {
    if created.is_empty() {
        return Ok(0);
    }
    let mut rows = db.get_dec_party_contracts(dec_party_id).await?;
    let known: BTreeSet<String> = rows.iter().map(|r| r.contract_id.clone()).collect();
    let mut added = 0;
    for c in created.iter().filter(|c| !known.contains(&c.contract_id)) {
        rows.push(DecPartyContractRow {
            dec_party_id: dec_party_id.to_string(),
            contract_id: c.contract_id.clone(),
            template_id: c.template_id.clone(),
            package_id: c.package_id.clone(),
            package_name: c.package_name.clone(),
            package_version: c.package_version.clone(),
            created_at: c.created_at.clone(),
        });
        added += 1;
    }
    if added > 0 {
        let mut tx = db.begin_transaction().await?;
        tx.replace_dec_party_contracts(dec_party_id, &rows).await?;
        Commitable::commit(tx).await?;
    }
    Ok(added)
}

// ---------------------------------------------------------------------------
// Member checks (design D7)
// ---------------------------------------------------------------------------

/// The root nodes of a prepared transaction, which must all be `Create`s.
///
/// # Errors
/// Returns an error when a root is missing, is not a v1 node, or is not a
/// `Create`.
pub fn root_create_nodes(prepared: &PreparedTransaction) -> Result<Vec<&v1::Create>> {
    let transaction = prepared
        .transaction
        .as_ref()
        .context("prepared transaction has no Daml transaction")?;
    ensure!(
        !transaction.roots.is_empty(),
        "the transaction has no root node"
    );
    transaction
        .roots
        .iter()
        .map(|root| {
            let node = transaction
                .nodes
                .iter()
                .find(|n| n.node_id == *root)
                .with_context(|| format!("root node {root} is missing"))?;
            let Some(daml_transaction::node::VersionedNode::V1(inner)) =
                node.versioned_node.as_ref()
            else {
                bail!("root node {root} is not a v1 node");
            };
            match inner.node_type.as_ref() {
                Some(v1::node::NodeType::Create(create)) => Ok(create),
                _ => bail!("root node {root} is not a Create"),
            }
        })
        .collect()
}

/// The template package ids of every root `Create` of a round.
///
/// # Errors
/// As [`decode_prepared_transaction`] and [`root_create_nodes`].
pub fn root_create_package_ids(round: &SubmissionRoundRecord) -> Result<BTreeSet<String>> {
    let prepared = decode_prepared_transaction(round)?;
    root_create_nodes(&prepared)?
        .into_iter()
        .map(|create| {
            create
                .template_id
                .as_ref()
                .map(|t| t.package_id.clone())
                .context("a root Create has no template id")
        })
        .collect()
}

/// Every root package id resolves through the local inventory to a name the
/// accepted proposal lists. An unknown id fails: this node cannot judge a
/// package it does not hold.
///
/// # Errors
/// Returns an error naming the first package that does not resolve or is
/// not allowed.
pub fn check_root_packages(
    package_ids: &BTreeSet<String>,
    id_to_name: &HashMap<String, String>,
    allowed_names: &[String],
) -> Result<()> {
    for id in package_ids {
        let name = id_to_name
            .get(id)
            .with_context(|| format!("package {id} is not on this participant"))?;
        ensure!(
            allowed_names.contains(name),
            "package {id} ({name}) is not among the accepted package names {allowed_names:?}"
        );
    }
    Ok(())
}

/// Member side (design D7): the round names the accepted party, `act_as` is
/// that party, `maxRecordTime` is present, `deadline > now`, this node's key
/// is among `party_signing_keys`, the hash recomputes with `canton_hash`,
/// and every root node is a `Create` whose own package name is one of the
/// accepted names. The package id resolution through the local inventory
/// is [`check_root_packages`]; the driver runs both.
///
/// # Errors
/// Returns an error naming the first rule that fails.
pub fn check_round(
    round: &SubmissionRoundRecord,
    accepted: &WorkflowProposalRecord,
    head_p2p: &PartyToParticipant,
    own_key_fingerprint: &str,
    now_micros: i64,
) -> Result<()> {
    ensure!(
        accepted.kind == WorkflowKind::Contracts,
        "the accepted proposal is a {} run, not Contracts",
        accepted.kind
    );
    let party = accepted
        .dec_party_id
        .as_deref()
        .context("the accepted proposal names no decPartyId")?;
    let party_id = CantonId::parse(party).with_context(|| format!("decPartyId `{party}`"))?;
    ensure!(
        round.dec_party_id == party,
        "round names {} but the accepted proposal is for {party}",
        round.dec_party_id
    );
    ensure!(
        round.act_as == party,
        "round acts as {} but the accepted proposal is for {party}",
        round.act_as
    );
    ensure!(
        round.deadline > now_micros,
        "round {} expired at {}",
        round.index,
        round.deadline
    );
    ensure!(
        round.deadline <= round.max_record_time,
        "round deadline is after its maxRecordTime"
    );
    required_signatures(head_p2p)?;
    ensure!(
        key_fingerprints(head_p2p).contains(own_key_fingerprint),
        "this node's key {own_key_fingerprint} is not a party signing key of {party}"
    );

    let prepared = decode_prepared_transaction(round)?;
    let metadata = prepared
        .metadata
        .as_ref()
        .context("prepared transaction has no metadata")?;
    check_metadata(metadata, &party_id)?;
    ensure!(
        metadata.max_record_time.and_then(|t| i64::try_from(t).ok()) == Some(round.max_record_time),
        "the signed maxRecordTime differs from the round's"
    );
    ensure!(
        i64::try_from(metadata.preparation_time).ok() == Some(round.preparation_time),
        "the signed preparationTime differs from the round's"
    );

    let version = i32::try_from(round.hashing_scheme_version)
        .context("hashingSchemeVersion does not fit an i32")?;
    let recomputed = canton_hash::compute_prepared_transaction_hash(
        HashingScheme::from_proto(version)?,
        &prepared,
    )?;
    ensure!(
        hex::encode(&recomputed) == round.prepared_hash_hex.to_lowercase(),
        "preparedHashHex {} is not the hash of the transaction ({})",
        round.prepared_hash_hex,
        hex::encode(&recomputed)
    );

    for create in root_create_nodes(&prepared)? {
        ensure!(
            accepted.package_names.contains(&create.package_name),
            "root Create of package {} is not among the accepted package names {:?}",
            create.package_name,
            accepted.package_names
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Signing (both sides)
// ---------------------------------------------------------------------------

/// The public key and KMS id of a vault key by fingerprint, as
/// `workflow/contracts/steps/sign.rs` reads them.
async fn vault_key_by_fingerprint(
    config: &NodeConfig,
    fingerprint: &str,
) -> Result<(SigningPublicKey, Option<String>)> {
    let mut vault = VaultServiceClient::new(config.admin_channel().await?);
    let response = vault
        .list_my_keys(tonic::Request::new(ListMyKeysRequest {
            filters: Some(ListKeysFilters {
                fingerprint: fingerprint.to_string(),
                name: String::new(),
                purpose: vec![],
                usage_v30: vec![],
            }),
            base_request: None,
        }))
        .await
        .context("ListMyKeys")?
        .into_inner();
    let metadata = response
        .private_keys_metadata
        .into_iter()
        .next()
        .with_context(|| format!("signing key {fingerprint} is not in this node's vault"))?;
    let kms_key_id = metadata.kms_key_id.clone();
    let Some(private_key_metadata::PublicKeyWithName::V30(pkn)) = metadata.public_key_with_name
    else {
        bail!("vault key {fingerprint} has no public key");
    };
    let Some(public_key::Key::SigningPublicKey(key)) = pkn.public_key.and_then(|k| k.key) else {
        bail!("vault key {fingerprint} is not a signing key");
    };
    Ok((key, kms_key_id))
}

/// Sign a prepared-transaction hash with this node's Daml key for the party
/// (vault export or KMS, chosen by `select_signer`).
///
/// # Errors
/// Returns an error when the node holds no Daml key for the party or the
/// backend fails.
pub async fn sign_hash_with_party_key(
    ol: &OnLedger,
    dec_party_id: &CantonId,
    hash: &[u8],
) -> Result<CantonSignature> {
    let config = ol.config();
    let identity =
        keys::local_identity_for_party(config, ol.db(), Some(dec_party_id), None).await?;
    let fingerprint = identity
        .daml_key_fingerprint
        .with_context(|| format!("this node holds no Daml signing key for {dec_party_id}"))?;
    let (public_key, kms_key_id) = vault_key_by_fingerprint(config, &fingerprint).await?;
    let key = SigningKeyContext {
        fingerprint,
        public_key,
        kms_key_id,
    };
    let signer = select_signer(&key, config.admin_channel().await?).await?;
    signer
        .sign(&[PreparedTransactionHash::new(hash.to_vec())], &key)
        .await?
        .into_iter()
        .next()
        .context("the signing backend returned no signature")
}

/// Sign the round's hash with this node's per-party key (vault export or
/// KMS) and exercise `SubmissionRound_Sign`. Returns the signature cid.
///
/// # Errors
/// Returns an error when signing or the submission fails.
pub async fn sign_round(
    ol: &OnLedger,
    round: &ActiveContract<SubmissionRoundRecord>,
    dec_party_id: &CantonId,
) -> Result<String> {
    let hash =
        hex::decode(&round.record.prepared_hash_hex).context("preparedHashHex is not hex")?;
    let signature = sign_hash_with_party_key(ol, dec_party_id, &hash).await?;
    let client = ol.client().await?;
    let args = SignArgs {
        signer: client.node_party().clone(),
        participant_id: client.participant_id().to_string(),
        signed_by: signature.signed_by.clone(),
        signature_hex: hex::encode(&signature.signature),
        format: format_name(signature.format)?,
        algorithm: algorithm_name(signature.signing_algorithm_spec)?,
    };
    let outcome = client
        .exercise(
            CoordinationTemplate::SubmissionRound,
            &round.contract_id,
            choices::SUBMISSION_ROUND_SIGN,
            args.to_value(),
        )
        .await
        .context("SubmissionRound_Sign")?;
    outcome
        .created_contract_id
        .context("SubmissionRound_Sign created no SubmissionSignature")
}

#[cfg(test)]
pub(crate) mod tests {
    use canton_proto_rs::com::{
        daml::ledger::api::v2::{
            CreatedEvent, Event,
            interactive::{DamlTransaction, Metadata, metadata::SubmitterInfo},
        },
        digitalasset::canton::{
            crypto::v30::{SigningKeyUsage, SigningKeysWithThreshold},
            protocol::v30::{
                enums::ParticipantPermission, party_to_participant::HostingParticipant,
            },
        },
    };
    use ed25519_dalek::Signer as _;

    use super::*;
    use crate::onledger::daml::codec::tests::{party, proposal_full};

    const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
    const PACKAGE_ID: &str = "abc123def456";

    /// The X.509 SubjectPublicKeyInfo prefix of an Ed25519 key.
    const ED25519_SPKI_PREFIX: [u8; 12] = [
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];

    pub(crate) fn dec_party() -> CantonId {
        party("cbtc")
    }

    pub(crate) fn ed25519_key(seed: u8) -> (ed25519_dalek::SigningKey, SigningPublicKey) {
        let signing = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let mut spki = ED25519_SPKI_PREFIX.to_vec();
        spki.extend_from_slice(&signing.verifying_key().to_bytes());
        let key = SigningPublicKey {
            format: CryptoKeyFormat::DerX509SubjectPublicKeyInfo as i32,
            public_key: spki,
            key_spec: SigningKeySpec::EcCurve25519 as i32,
            usage: vec![SigningKeyUsage::Protocol as i32],
            ..Default::default()
        };
        (signing, key)
    }

    /// The X.509 SubjectPublicKeyInfo prefix of an uncompressed P-256 key.
    const P256_SPKI_PREFIX: [u8; 26] = [
        0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08,
        0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
    ];

    fn p256_key(seed: u8) -> (p256::ecdsa::SigningKey, SigningPublicKey) {
        let signing = p256::ecdsa::SigningKey::from_slice(&[seed; 32]).expect("p256 key");
        let point = signing.verifying_key().to_encoded_point(false);
        let mut der = P256_SPKI_PREFIX.to_vec();
        der.extend_from_slice(point.as_bytes());
        let key = SigningPublicKey {
            format: CryptoKeyFormat::DerX509SubjectPublicKeyInfo as i32,
            public_key: der,
            key_spec: SigningKeySpec::EcP256 as i32,
            usage: vec![SigningKeyUsage::Protocol as i32],
            ..Default::default()
        };
        (signing, key)
    }

    pub(crate) fn head_p2p(keys: Vec<SigningPublicKey>, threshold: u32) -> PartyToParticipant {
        PartyToParticipant {
            party: dec_party().to_string(),
            threshold: 1,
            participants: vec![HostingParticipant {
                participant_uid: format!("participant1::{NS}"),
                permission: ParticipantPermission::Confirmation as i32,
                onboarding: None,
            }],
            party_signing_keys: Some(SigningKeysWithThreshold { keys, threshold }),
        }
    }

    fn create_node(node_id: &str, package_name: &str) -> daml_transaction::Node {
        daml_transaction::Node {
            node_id: node_id.to_string(),
            versioned_node: Some(daml_transaction::node::VersionedNode::V1(v1::Node {
                node_type: Some(v1::node::NodeType::Create(v1::Create {
                    lf_version: "2.1".into(),
                    // The hasher hex-decodes contract ids: 34 bytes, like Canton's.
                    contract_id: format!(
                        "00{:0>66}",
                        node_id
                            .bytes()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>()
                    ),
                    package_name: package_name.into(),
                    template_id: Some(Identifier {
                        package_id: PACKAGE_ID.into(),
                        module_name: "Governance".into(),
                        entity_name: "Rules".into(),
                    }),
                    argument: Some(Value {
                        sum: Some(value::Sum::Text("hello".into())),
                    }),
                    signatories: vec![dec_party().to_string()],
                    stakeholders: vec![dec_party().to_string()],
                    key: None,
                })),
            })),
        }
    }

    fn rollback_node(node_id: &str) -> daml_transaction::Node {
        daml_transaction::Node {
            node_id: node_id.to_string(),
            versioned_node: Some(daml_transaction::node::VersionedNode::V1(v1::Node {
                node_type: Some(v1::node::NodeType::Rollback(v1::Rollback {
                    children: vec![],
                })),
            })),
        }
    }

    /// A time-independent create of `governance-core-v1` acting as the dec
    /// party, prepared at `prep` with `max_record_time = prep + 20 h`.
    pub(crate) fn prepared(prep: i64) -> PreparedTransaction {
        PreparedTransaction {
            transaction: Some(DamlTransaction {
                version: "2.1".into(),
                roots: vec!["0".into()],
                nodes: vec![create_node("0", "governance-core-v1")],
                node_seeds: vec![daml_transaction::NodeSeed {
                    node_id: 0,
                    seed: vec![7; 32],
                }],
            }),
            metadata: Some(Metadata {
                submitter_info: Some(SubmitterInfo {
                    act_as: vec![dec_party().to_string()],
                    command_id: "cmd".into(),
                }),
                synchronizer_id: "sync::id".into(),
                mediator_group: 0,
                transaction_uuid: "4c6471d3-4e09-49dd-addf-6cd90e19c583".into(),
                preparation_time: u64::try_from(prep).expect("positive"),
                input_contracts: vec![],
                min_ledger_effective_time: None,
                max_ledger_effective_time: None,
                max_record_time: Some(
                    u64::try_from(prep + MAX_RECORD_TIME_HORIZON_MICROS).expect("positive"),
                ),
                ..Metadata::default()
            }),
        }
    }

    /// A round the proposer would open for [`prepared`], with a correct hash.
    pub(crate) fn round_for(prepared: &PreparedTransaction, prep: i64) -> SubmissionRoundRecord {
        let hash = canton_hash::compute_prepared_transaction_hash(HashingScheme::V2, prepared)
            .expect("hash");
        let max_record_time = prep + MAX_RECORD_TIME_HORIZON_MICROS;
        SubmissionRoundRecord {
            proposer: party("node-a"),
            run_id: "cbtc-contracts".into(),
            index: 0,
            signers: vec![party("node-b")],
            dec_party_id: dec_party().to_string(),
            act_as: dec_party().to_string(),
            description: "Rules".into(),
            prepared_transaction_hex: hex::encode(prepared.encode_to_vec()),
            prepared_hash_hex: hex::encode(hash),
            hashing_scheme_version: 2,
            preparation_time: prep,
            max_record_time,
            deadline: deadline_for(prep, max_record_time, DEFAULT_RECORD_TIME_TOLERANCE_MICROS),
        }
    }

    pub(crate) fn accepted_contracts_proposal() -> WorkflowProposalRecord {
        WorkflowProposalRecord {
            kind: WorkflowKind::Contracts,
            dec_party_id: Some(dec_party().to_string()),
            package_names: vec!["governance-core-v1".into()],
            threshold: None,
            ..proposal_full()
        }
    }

    fn signature_record(
        signed_by: &str,
        sig: &[u8],
        format: &str,
        algorithm: &str,
    ) -> SubmissionSignatureRecord {
        SubmissionSignatureRecord {
            round: "00round".into(),
            proposer: party("node-a"),
            signer: party("node-b"),
            observers: vec![],
            run_id: "cbtc-contracts".into(),
            index: 0,
            participant_id: format!("participant2::{NS}"),
            signed_by: signed_by.into(),
            signature_hex: hex::encode(sig),
            format: format.into(),
            algorithm: algorithm.into(),
            signed_at: 0,
        }
    }

    // -- time --------------------------------------------------------------

    #[test]
    fn deadline_is_the_earlier_bound_minus_the_margin() {
        let prep = 1_000 * 1_000_000;
        let max_record = prep + MAX_RECORD_TIME_HORIZON_MICROS;
        // A 24 h tolerance: max_record_time is the earlier bound.
        let tolerance = 24 * 3600 * 1_000_000;
        assert_eq!(
            deadline_for(prep, max_record, tolerance),
            max_record - DEADLINE_SAFETY_MARGIN_MICROS
        );
        // A 1 h tolerance: preparation time + tolerance is the earlier bound.
        let tolerance = 3600 * 1_000_000;
        assert_eq!(
            deadline_for(prep, max_record, tolerance),
            prep + tolerance - DEADLINE_SAFETY_MARGIN_MICROS
        );
    }

    #[test]
    fn duration_and_timestamp_conversions() {
        let day = prost_types::Duration {
            seconds: 86_400,
            nanos: 500_000,
        };
        assert_eq!(duration_micros(&day), 86_400 * 1_000_000 + 500);
        let ts = timestamp_from_micros(1_700_000_000_123_456);
        assert_eq!(ts.seconds, 1_700_000_000);
        assert_eq!(ts.nanos, 123_456_000);
        assert_eq!(rfc3339_of(&ts), "2023-11-14T22:13:20Z");
    }

    // -- verification and dedupe ------------------------------------------

    #[test]
    fn dedupe_keeps_the_first_signature_per_fingerprint() {
        let sig = |fp: &str, n: u8| VerifiedSignature {
            signed_by: fp.into(),
            signature: vec![n],
            format: "SIGNATURE_FORMAT_CONCAT".into(),
            algorithm: "SIGNING_ALGORITHM_SPEC_ED25519".into(),
            participant_id: "p".into(),
        };
        let deduped = dedupe_verified([sig("a", 1), sig("b", 2), sig("a", 3)]);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped["a"].signature, vec![1]);
    }

    #[test]
    fn ed25519_concat_signature_verifies_against_the_p2p_key() {
        let (signing, key) = ed25519_key(1);
        let fp = utils::compute_fingerprint(&key);
        let hash = [0x42u8; 32];
        let sig = signing.sign(&hash).to_bytes();
        let head = head_p2p(vec![key], 1);
        let record = signature_record(
            &fp,
            &sig,
            "SIGNATURE_FORMAT_CONCAT",
            "SIGNING_ALGORITHM_SPEC_ED25519",
        );

        let verified = verify_signature(&head, &record, &hash).expect("verifies");
        assert_eq!(verified.signed_by, fp);
        assert_eq!(verified.signature, sig.to_vec());

        // The proposer's own Canton signature takes the same path.
        let own = CantonSignature {
            format: SignatureFormat::Concat as i32,
            signature: sig.to_vec(),
            signed_by: fp.clone(),
            signing_algorithm_spec: SigningAlgorithmSpec::Ed25519 as i32,
            signature_delegation: None,
        };
        assert!(verify_canton_signature(&head, &own, &hash, "p1").is_ok());
    }

    #[test]
    fn ecdsa_der_signature_verifies_against_the_p2p_key() {
        use p256::ecdsa::signature::Signer;
        let (signing, key) = p256_key(3);
        let fp = utils::compute_fingerprint(&key);
        let hash = [0x24u8; 32];
        let sig: p256::ecdsa::Signature = signing.sign(&hash);
        let der = sig.to_der();
        let head = head_p2p(vec![key], 1);
        let record = signature_record(
            &fp,
            der.as_bytes(),
            "SIGNATURE_FORMAT_DER",
            "SIGNING_ALGORITHM_SPEC_EC_DSA_SHA_256",
        );

        assert!(verify_signature(&head, &record, &hash).is_ok());
        // ECDSA must not be accepted in CONCAT form.
        let concat = signature_record(
            &fp,
            &sig.to_bytes(),
            "SIGNATURE_FORMAT_CONCAT",
            "SIGNING_ALGORITHM_SPEC_EC_DSA_SHA_256",
        );
        assert!(verify_signature(&head, &concat, &hash).is_err());
    }

    #[test]
    fn verification_rejects_foreign_keys_wrong_hashes_and_bad_formats() {
        let (signing, key) = ed25519_key(1);
        let (_, other_key) = ed25519_key(2);
        let fp = utils::compute_fingerprint(&key);
        let hash = [0x42u8; 32];
        let sig = signing.sign(&hash).to_bytes();
        let concat = "SIGNATURE_FORMAT_CONCAT";
        let ed = "SIGNING_ALGORITHM_SPEC_ED25519";

        // signedBy not in party_signing_keys.
        let head_other = head_p2p(vec![other_key.clone()], 1);
        let err = verify_signature(&head_other, &signature_record(&fp, &sig, concat, ed), &hash)
            .expect_err("foreign key");
        assert!(err.to_string().contains("not a party signing key"), "{err}");

        // Right fingerprint, wrong hash.
        let head = head_p2p(vec![key.clone(), other_key], 1);
        assert!(
            verify_signature(
                &head,
                &signature_record(&fp, &sig, concat, ed),
                &[0x43u8; 32]
            )
            .is_err()
        );

        // Tampered signature.
        let mut bad = sig;
        bad[0] ^= 0xff;
        assert!(verify_signature(&head, &signature_record(&fp, &bad, concat, ed), &hash).is_err());

        // Ed25519 in DER form, unknown format text, unsupported algorithm.
        assert!(
            verify_signature(
                &head,
                &signature_record(&fp, &sig, "SIGNATURE_FORMAT_DER", ed),
                &hash
            )
            .is_err()
        );
        assert!(
            verify_signature(
                &head,
                &signature_record(&fp, &sig, "SIGNATURE_FORMAT_PEM", ed),
                &hash
            )
            .is_err()
        );
        assert!(
            verify_signature(
                &head,
                &signature_record(&fp, &sig, concat, "SIGNING_ALGORITHM_SPEC_ML_DSA_65"),
                &hash
            )
            .is_err()
        );

        // A P2P without signing keys fails closed.
        let mut no_keys = head.clone();
        no_keys.party_signing_keys = None;
        assert!(
            verify_signature(&no_keys, &signature_record(&fp, &sig, concat, ed), &hash).is_err()
        );
        assert!(required_signatures(&no_keys).is_err());
        assert_eq!(required_signatures(&head).expect("threshold"), 1);
    }

    // -- member checks -----------------------------------------------------

    #[test]
    fn check_round_accepts_a_faithful_round() {
        let prep = 1_700_000_000_000_000;
        let prepared = prepared(prep);
        let round = round_for(&prepared, prep);
        let (_, key) = ed25519_key(1);
        let fp = utils::compute_fingerprint(&key);
        let head = head_p2p(vec![key], 1);

        check_round(
            &round,
            &accepted_contracts_proposal(),
            &head,
            &fp,
            prep + 60,
        )
        .expect("accepted");
        assert_eq!(
            root_create_package_ids(&round).expect("ids"),
            BTreeSet::from([PACKAGE_ID.to_string()])
        );
    }

    #[test]
    fn check_round_rejects_every_d7_violation() {
        let prep = 1_700_000_000_000_000;
        let prepared = prepared(prep);
        let round = round_for(&prepared, prep);
        let accepted = accepted_contracts_proposal();
        let (_, key) = ed25519_key(1);
        let (_, other) = ed25519_key(2);
        let fp = utils::compute_fingerprint(&key);
        let head = head_p2p(vec![key.clone()], 1);
        let now = prep + 60;
        let fails = |round: &SubmissionRoundRecord,
                     accepted: &WorkflowProposalRecord,
                     head: &PartyToParticipant,
                     fp: &str,
                     now: i64,
                     needle: &str| {
            let err = check_round(round, accepted, head, fp, now).expect_err(needle);
            assert!(
                err.to_string().contains(needle),
                "expected `{needle}` in `{err}`"
            );
        };

        // Another party on the round.
        let mut r = round.clone();
        r.dec_party_id = party("other").to_string();
        fails(&r, &accepted, &head, &fp, now, "round names");
        // act_as differs from the party.
        let mut r = round.clone();
        r.act_as = party("other").to_string();
        fails(&r, &accepted, &head, &fp, now, "round acts as");
        // Expired.
        fails(&round, &accepted, &head, &fp, round.deadline, "expired");
        // Own key not a signing key.
        fails(
            &round,
            &accepted,
            &head_p2p(vec![other], 1),
            &fp,
            now,
            "not a party signing key",
        );
        // No signing keys at all.
        let mut no_keys = head.clone();
        no_keys.party_signing_keys = None;
        fails(&round, &accepted, &no_keys, &fp, now, "party_signing_keys");
        // Proposal of another kind, or without a party.
        let mut a = accepted.clone();
        a.kind = WorkflowKind::Kick;
        fails(&round, &a, &head, &fp, now, "not Contracts");
        let mut a = accepted.clone();
        a.dec_party_id = None;
        fails(&round, &a, &head, &fp, now, "no decPartyId");
        // Hash does not belong to the transaction.
        let mut r = round.clone();
        r.prepared_hash_hex = hex::encode([0x11u8; 32]);
        fails(
            &r,
            &accepted,
            &head,
            &fp,
            now,
            "not the hash of the transaction",
        );
        // Unsupported hashing scheme.
        let mut r = round.clone();
        r.hashing_scheme_version = 4;
        assert!(check_round(&r, &accepted, &head, &fp, now).is_err());
        // Round metadata differs from the signed metadata.
        let mut r = round.clone();
        r.max_record_time += 1;
        r.deadline = r.max_record_time - DEADLINE_SAFETY_MARGIN_MICROS;
        fails(
            &r,
            &accepted,
            &head,
            &fp,
            now,
            "signed maxRecordTime differs",
        );
        let mut r = round.clone();
        r.preparation_time += 1;
        fails(
            &r,
            &accepted,
            &head,
            &fp,
            now,
            "signed preparationTime differs",
        );
        // Deadline past the signed bound.
        let mut r = round.clone();
        r.deadline = r.max_record_time + 1;
        fails(&r, &accepted, &head, &fp, now, "after its maxRecordTime");
        // Package not accepted.
        let mut a = accepted.clone();
        a.package_names = vec!["something-else".into()];
        fails(
            &round,
            &a,
            &head,
            &fp,
            now,
            "not among the accepted package names",
        );
        // Garbage transaction bytes.
        let mut r = round.clone();
        r.prepared_transaction_hex = "zz".into();
        fails(&r, &accepted, &head, &fp, now, "not hex");
    }

    #[test]
    fn check_round_rejects_transactions_this_node_must_not_sign() {
        let prep = 1_700_000_000_000_000;
        let accepted = accepted_contracts_proposal();
        let (_, key) = ed25519_key(1);
        let fp = utils::compute_fingerprint(&key);
        let head = head_p2p(vec![key], 1);
        let now = prep + 60;
        let with = |edit: &dyn Fn(&mut PreparedTransaction)| {
            let mut p = prepared(prep);
            edit(&mut p);
            let mut r = round_for(&p, prep);
            // Keep the round's own copy of the times consistent with the
            // metadata under test.
            if let Some(m) = p.metadata.as_ref() {
                r.max_record_time = m
                    .max_record_time
                    .and_then(|t| i64::try_from(t).ok())
                    .unwrap_or(r.max_record_time);
            }
            r
        };
        let expect = |r: &SubmissionRoundRecord, needle: &str| {
            let err = check_round(r, &accepted, &head, &fp, now).expect_err(needle);
            assert!(
                err.to_string().contains(needle),
                "expected `{needle}` in `{err}`"
            );
        };

        // Acting as a second party.
        expect(
            &with(&|p| {
                let info = p
                    .metadata
                    .as_mut()
                    .and_then(|m| m.submitter_info.as_mut())
                    .expect("info");
                info.act_as.push(party("mallory").to_string());
            }),
            "acts as",
        );
        // No signed maxRecordTime.
        expect(
            &with(&|p| p.metadata.as_mut().expect("meta").max_record_time = None),
            "no maxRecordTime",
        );
        // Time-dependent transaction.
        expect(
            &with(&|p| p.metadata.as_mut().expect("meta").min_ledger_effective_time = Some(1)),
            "depends on ledger time",
        );
        // A root that is not a Create.
        expect(
            &with(&|p| {
                let tx = p.transaction.as_mut().expect("tx");
                tx.nodes.push(rollback_node("1"));
                tx.roots.push("1".into());
            }),
            "not a Create",
        );
        // A root Create of a package the proposal does not list.
        expect(
            &with(&|p| {
                let tx = p.transaction.as_mut().expect("tx");
                tx.nodes = vec![create_node("0", "evil-v1")];
            }),
            "not among the accepted package names",
        );
    }

    #[test]
    fn root_packages_must_resolve_to_an_accepted_name() {
        let ids = BTreeSet::from([PACKAGE_ID.to_string()]);
        let allowed = vec!["governance-core-v1".to_string()];
        let known: HashMap<String, String> =
            [(PACKAGE_ID.to_string(), "governance-core-v1".to_string())]
                .into_iter()
                .collect();
        assert!(check_root_packages(&ids, &known, &allowed).is_ok());

        let err = check_root_packages(&ids, &HashMap::new(), &allowed).expect_err("unknown");
        assert!(err.to_string().contains("not on this participant"), "{err}");

        let other: HashMap<String, String> = [(PACKAGE_ID.to_string(), "other-v1".to_string())]
            .into_iter()
            .collect();
        let err = check_root_packages(&ids, &other, &allowed).expect_err("wrong name");
        assert!(
            err.to_string()
                .contains("not among the accepted package names"),
            "{err}"
        );
    }

    // -- prepare and execute plumbing ---------------------------------------

    #[test]
    fn prepared_round_takes_times_from_the_signed_metadata() {
        let prep = 1_700_000_000_000_000;
        let prepared_tx = prepared(prep);
        let hash = canton_hash::compute_prepared_transaction_hash(HashingScheme::V2, &prepared_tx)
            .expect("hash");
        let response = PrepareSubmissionResponse {
            prepared_transaction: Some(prepared_tx.clone()),
            prepared_transaction_hash: hash.clone(),
            hashing_scheme_version: 2,
            hashing_details: None,
            cost_estimation: None,
        };
        let tolerance = 3600 * 1_000_000;

        let round = prepared_round(3, "Rules", &response, &dec_party(), tolerance).expect("round");
        assert_eq!(round.index, 3);
        assert_eq!(round.preparation_time, prep);
        assert_eq!(round.max_record_time, prep + MAX_RECORD_TIME_HORIZON_MICROS);
        assert_eq!(
            round.deadline,
            prep + tolerance - DEADLINE_SAFETY_MARGIN_MICROS
        );
        assert_eq!(round.prepared_hash_hex, hex::encode(&hash));
        assert_eq!(round.hashing_scheme_version, 2);
        assert_eq!(
            decode_prepared_transaction(&round_for(&prepared_tx, prep)).expect("decodes"),
            prepared_tx
        );

        // A hash that does not match the transaction is refused before any
        // round is opened.
        let mut bad = response.clone();
        bad.prepared_transaction_hash = vec![0; 32];
        assert!(prepared_round(0, "Rules", &bad, &dec_party(), tolerance).is_err());
        // So is a transaction acting as another party.
        assert!(prepared_round(0, "Rules", &response, &party("other"), tolerance).is_err());
    }

    #[test]
    fn create_command_substitutes_context_values() {
        let context = SubmissionContext {
            decentralized_party: dec_party(),
            operator_party: party("op"),
            participant_parties: vec![party("m1"), party("m2")],
            governance_threshold: 2,
        };
        let definition = ContractDefinition {
            id: "rules".into(),
            name: "Rules".into(),
            package_id: "#governance-core-v1".into(),
            module_name: "Governance".into(),
            entity_name: "Rules".into(),
            fields: vec![
                FieldDefinition::DecentralizedParty,
                FieldDefinition::OperatorParty,
                FieldDefinition::AttestorsSet,
                FieldDefinition::GovernanceThreshold { value: None },
                FieldDefinition::GovernanceThreshold { value: Some(5) },
                FieldDefinition::Optional {
                    inner: Box::new(FieldDefinition::RelTime { microseconds: 7 }),
                },
                FieldDefinition::None,
            ],
        };

        let command = create_command(&definition, &context).expect("command");
        let Some(command::Command::Create(create)) = command.command else {
            panic!("not a create");
        };
        assert_eq!(
            create.template_id.expect("id").package_id,
            "#governance-core-v1"
        );
        let fields = create.create_arguments.expect("args").fields;
        assert_eq!(fields.len(), 7);
        assert_eq!(
            fields[0].value.as_ref().and_then(|v| v.sum.clone()),
            Some(value::Sum::Party(dec_party().to_string()))
        );
        assert_eq!(
            fields[1].value.as_ref().and_then(|v| v.sum.clone()),
            Some(value::Sum::Party(party("op").to_string()))
        );
        match fields[2].value.as_ref().and_then(|v| v.sum.as_ref()) {
            Some(value::Sum::GenMap(map)) => assert_eq!(map.entries.len(), 2),
            other => panic!("attestors set is {other:?}"),
        }
        assert_eq!(
            fields[3].value.as_ref().and_then(|v| v.sum.clone()),
            Some(value::Sum::Int64(2))
        );
        assert_eq!(
            fields[4].value.as_ref().and_then(|v| v.sum.clone()),
            Some(value::Sum::Int64(5))
        );
        match fields[5].value.as_ref().and_then(|v| v.sum.as_ref()) {
            Some(value::Sum::Optional(opt)) => assert!(opt.value.is_some()),
            other => panic!("optional is {other:?}"),
        }
        match fields[6].value.as_ref().and_then(|v| v.sum.as_ref()) {
            Some(value::Sum::Optional(opt)) => assert!(opt.value.is_none()),
            other => panic!("none is {other:?}"),
        }
    }

    #[test]
    fn created_contracts_take_the_cache_shape() {
        let created = |cid: &str| Event {
            event: Some(event::Event::Created(CreatedEvent {
                contract_id: cid.into(),
                template_id: Some(Identifier {
                    package_id: PACKAGE_ID.into(),
                    module_name: "Governance".into(),
                    entity_name: "Rules".into(),
                }),
                package_name: "governance-core-v1".into(),
                created_at: Some(Timestamp {
                    seconds: 1_700_000_000,
                    nanos: 0,
                }),
                ..CreatedEvent::default()
            })),
        };
        let tx = Transaction {
            update_id: "u1".into(),
            events: vec![
                created("c1"),
                Event {
                    event: Some(event::Event::Archived(Default::default())),
                },
                created("c2"),
            ],
            ..Transaction::default()
        };
        let versions: HashMap<String, (String, String)> = [(
            PACKAGE_ID.to_string(),
            ("governance-core-v1".to_string(), "0.1.0".to_string()),
        )]
        .into_iter()
        .collect();

        let contracts = created_contracts_of(&tx, &versions);
        assert_eq!(contracts.len(), 2);
        assert_eq!(contracts[0].contract_id, "c1");
        assert_eq!(contracts[0].template_id, "Governance:Rules");
        assert_eq!(contracts[0].package_id, PACKAGE_ID);
        assert_eq!(contracts[0].package_name, "governance-core-v1");
        assert_eq!(contracts[0].package_version, "0.1.0");
        assert_eq!(contracts[0].created_at, "2023-11-14T22:13:20Z");
        assert!(
            created_contracts_of(&tx, &HashMap::new())[1]
                .package_version
                .is_empty()
        );
    }

    #[test]
    fn ledger_signature_carries_the_enum_values() {
        let sig = ledger_signature(&VerifiedSignature {
            signed_by: "1220ab".into(),
            signature: vec![1, 2],
            format: "SIGNATURE_FORMAT_DER".into(),
            algorithm: "SIGNING_ALGORITHM_SPEC_EC_DSA_SHA_256".into(),
            participant_id: "p".into(),
        })
        .expect("signature");
        assert_eq!(sig.format, SignatureFormat::Der as i32);
        assert_eq!(
            sig.signing_algorithm_spec,
            SigningAlgorithmSpec::EcDsaSha256 as i32
        );
        assert_eq!(sig.signed_by, "1220ab");
        assert_eq!(
            format_name(SignatureFormat::Concat as i32).expect("name"),
            "SIGNATURE_FORMAT_CONCAT"
        );
        assert!(format_name(99).is_err());
        assert!(algorithm_name(99).is_err());
    }

    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn created_contracts_merge_into_the_cache(pool: SqlitePool) -> Result<()> {
        use crate::db::rows::DecPartyRow;
        let party_id = dec_party();
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&DecPartyRow {
            party_id: party_id.to_string(),
            prefix: party_id.prefix.clone(),
            threshold: 1,
            updated_at: 0,
            my_owner_key: None,
        })
        .await?;
        Commitable::commit(tx).await?;
        let contract = |cid: &str| CreatedContract {
            contract_id: cid.into(),
            template_id: "Governance:Rules".into(),
            package_id: PACKAGE_ID.into(),
            package_name: "governance-core-v1".into(),
            package_version: "0.1.0".into(),
            created_at: "2023-11-14T22:13:20Z".into(),
        };

        assert_eq!(
            record_created_contracts(&pool, &party_id, &[contract("c1")]).await?,
            1
        );
        // A second write with an overlap adds only the new row.
        assert_eq!(
            record_created_contracts(&pool, &party_id, &[contract("c1"), contract("c2")]).await?,
            1
        );
        let rows = pool.get_dec_party_contracts(&party_id).await?;
        let ids: BTreeSet<String> = rows.iter().map(|r| r.contract_id.clone()).collect();
        assert_eq!(ids, BTreeSet::from(["c1".to_string(), "c2".to_string()]));
        assert_eq!(rows[0].template_id, "Governance:Rules");
        Ok(())
    }
}
