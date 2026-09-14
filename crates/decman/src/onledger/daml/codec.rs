//! Rust mirrors of every `decman-coordination-v1` template and choice
//! argument, with `Record` encode (create and exercise arguments) and decode
//! (verbose ACS reads).
//!
//! Field names and order follow design section 3 exactly. Every `Time` is an
//! `i64` of microseconds since the epoch, the Ledger API `Timestamp` unit.
//! Fingerprints, hashes, keys, and signatures are hex `String`s; the Daml
//! side only bounds their length, so the Rust callers validate hex.
//!
//! The decoders read fields by label, which a `verbose` read supplies. Every
//! decoder is strict: a missing or wrong-shaped field is an error, never a
//! default, because a silently defaulted field could make a node co-sign
//! against the wrong expectation.

use anyhow::{Context, Result, anyhow};
use canton_proto_rs::com::daml::ledger::api::v2::{Enum, Optional, Record, Value, value};
use common::canton_id::CantonId;
use decman_lib::framework::{
    encode::{
        field, make_bool, make_contract_id, make_int64, make_list, make_party, make_record,
        make_text,
    },
    record::{
        extract_contract_id, extract_int64, extract_party_id, extract_text, field_party_list,
        get_field, record_field,
    },
};

pub use common::types::WorkflowKind;

use super::templates::CoordinationTemplate;

// ---------------------------------------------------------------------------
// Encode helpers.
// TODO(decman-lib): these belong in `decman_lib::framework::encode` next to
// `make_optional_list`; kept private here until the lib grows them.
// ---------------------------------------------------------------------------

fn make_timestamp(micros: i64) -> Value {
    Value {
        sum: Some(value::Sum::Timestamp(micros)),
    }
}

fn make_enum(constructor: &str) -> Value {
    Value {
        sum: Some(value::Sum::Enum(Enum {
            enum_id: None,
            constructor: constructor.to_string(),
        })),
    }
}

fn make_optional(inner: Option<Value>) -> Value {
    Value {
        sum: Some(value::Sum::Optional(Box::new(Optional {
            value: inner.map(Box::new),
        }))),
    }
}

fn make_optional_text(v: &Option<String>) -> Value {
    make_optional(v.as_deref().map(make_text))
}

fn make_optional_int64(v: &Option<i64>) -> Value {
    make_optional(v.map(make_int64))
}

fn make_optional_party(v: &Option<CantonId>) -> Value {
    make_optional(v.as_ref().map(make_party))
}

fn make_text_list(items: &[String]) -> Value {
    make_list(items.iter().map(|s| make_text(s)).collect())
}

fn make_party_list(items: &[CantonId]) -> Value {
    make_list(items.iter().map(make_party).collect())
}

/// The empty record a choice without arguments takes.
pub fn unit_argument() -> Value {
    make_record(vec![])
}

// ---------------------------------------------------------------------------
// Decode helpers (strict).
// TODO(decman-lib): these belong in `decman_lib::framework::record`; kept
// private here until the lib grows them.
// ---------------------------------------------------------------------------

fn req_text(rec: &Record, label: &str) -> Result<String> {
    extract_text(get_field(rec, label)?).with_context(|| format!("field `{label}`"))
}

fn req_party(rec: &Record, label: &str) -> Result<CantonId> {
    extract_party_id(get_field(rec, label)?).with_context(|| format!("field `{label}`"))
}

fn req_int64(rec: &Record, label: &str) -> Result<i64> {
    extract_int64(get_field(rec, label)?).with_context(|| format!("field `{label}`"))
}

fn req_contract_id(rec: &Record, label: &str) -> Result<String> {
    extract_contract_id(get_field(rec, label)?).with_context(|| format!("field `{label}`"))
}

fn req_bool(rec: &Record, label: &str) -> Result<bool> {
    match record_field(rec, label) {
        Some(value::Sum::Bool(b)) => Ok(*b),
        _ => Err(anyhow!("field `{label}`: expected a Bool value")),
    }
}

fn req_timestamp(rec: &Record, label: &str) -> Result<i64> {
    match record_field(rec, label) {
        Some(value::Sum::Timestamp(t)) => Ok(*t),
        _ => Err(anyhow!("field `{label}`: expected a Time value")),
    }
}

fn req_enum(rec: &Record, label: &str) -> Result<String> {
    match record_field(rec, label) {
        Some(value::Sum::Enum(e)) => Ok(e.constructor.clone()),
        _ => Err(anyhow!("field `{label}`: expected an Enum value")),
    }
}

fn req_list<'a>(rec: &'a Record, label: &str) -> Result<&'a [Value]> {
    match record_field(rec, label) {
        Some(value::Sum::List(l)) => Ok(&l.elements),
        _ => Err(anyhow!("field `{label}`: expected a List value")),
    }
}

fn req_text_list(rec: &Record, label: &str) -> Result<Vec<String>> {
    req_list(rec, label)?
        .iter()
        .map(|v| extract_text(v).with_context(|| format!("field `{label}`: element")))
        .collect()
}

fn req_party_list(rec: &Record, label: &str) -> Result<Vec<CantonId>> {
    field_party_list(rec, label).with_context(|| format!("field `{label}`"))
}

fn req_record_list<'a>(rec: &'a Record, label: &str) -> Result<Vec<&'a Record>> {
    req_list(rec, label)?
        .iter()
        .map(|v| match &v.sum {
            Some(value::Sum::Record(r)) => Ok(r),
            _ => Err(anyhow!("field `{label}`: element is not a Record")),
        })
        .collect()
}

/// The inner value of an `Optional` field, or `None` for `Optional None`. A
/// missing field or a non-optional shape is an error.
fn opt_value<'a>(rec: &'a Record, label: &str) -> Result<Option<&'a Value>> {
    match record_field(rec, label) {
        Some(value::Sum::Optional(opt)) => Ok(opt.value.as_deref()),
        _ => Err(anyhow!("field `{label}`: expected an Optional value")),
    }
}

fn opt_text(rec: &Record, label: &str) -> Result<Option<String>> {
    opt_value(rec, label)?
        .map(|v| extract_text(v).with_context(|| format!("field `{label}`")))
        .transpose()
}

fn opt_int64(rec: &Record, label: &str) -> Result<Option<i64>> {
    opt_value(rec, label)?
        .map(|v| extract_int64(v).with_context(|| format!("field `{label}`")))
        .transpose()
}

fn opt_party(rec: &Record, label: &str) -> Result<Option<CantonId>> {
    opt_value(rec, label)?
        .map(|v| extract_party_id(v).with_context(|| format!("field `{label}`")))
        .transpose()
}

fn parse_kind(constructor: &str) -> Result<WorkflowKind> {
    constructor
        .parse::<WorkflowKind>()
        .with_context(|| format!("field `kind`: unknown WorkflowKind `{constructor}`"))
}

// ---------------------------------------------------------------------------
// Traits
// ---------------------------------------------------------------------------

/// A template's create arguments: encode for `CreateCommand`, decode from a
/// verbose `CreatedEvent.create_arguments`.
pub trait TemplateRecord: Sized {
    /// Which template this record creates.
    const TEMPLATE: CoordinationTemplate;

    fn to_record(&self) -> Record;

    /// # Errors
    /// Returns an error when a field is missing or has the wrong shape.
    fn from_record(rec: &Record) -> Result<Self>;

    /// The record wrapped as a `Value`, the shape a choice argument takes.
    fn to_value(&self) -> Value {
        Value {
            sum: Some(value::Sum::Record(self.to_record())),
        }
    }
}

/// A choice's arguments: encode for `ExerciseCommand.choice_argument`.
pub trait ChoiceArgument {
    fn to_record(&self) -> Record;

    fn to_value(&self) -> Value {
        Value {
            sum: Some(value::Sum::Record(self.to_record())),
        }
    }
}

fn record(fields: Vec<canton_proto_rs::com::daml::ledger::api::v2::RecordField>) -> Record {
    Record {
        record_id: None,
        fields,
    }
}

// ---------------------------------------------------------------------------
// Decman.Coordination.Types
// ---------------------------------------------------------------------------

/// `DarPin`: one file of a `Dars` proposal. Bytes travel out of band; the
/// pin is what each invitee verifies the local upload against.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DarPin {
    pub filename: String,
    pub sha256_hex: String,
    pub main_package_id: String,
    pub size_bytes: i64,
}

impl DarPin {
    pub fn to_record(&self) -> Record {
        record(vec![
            field("filename", make_text(&self.filename)),
            field("sha256Hex", make_text(&self.sha256_hex)),
            field("mainPackageId", make_text(&self.main_package_id)),
            field("sizeBytes", make_int64(self.size_bytes)),
        ])
    }

    /// # Errors
    /// Returns an error when a field is missing or has the wrong shape.
    pub fn from_record(rec: &Record) -> Result<Self> {
        Ok(Self {
            filename: req_text(rec, "filename")?,
            sha256_hex: req_text(rec, "sha256Hex")?,
            main_package_id: req_text(rec, "mainPackageId")?,
            size_bytes: req_int64(rec, "sizeBytes")?,
        })
    }

    fn to_value(&self) -> Value {
        Value {
            sum: Some(value::Sum::Record(self.to_record())),
        }
    }
}

// ---------------------------------------------------------------------------
// Decman.Coordination.Node
// ---------------------------------------------------------------------------

/// `DecmanNode`: a node's self-signed registry entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecmanNodeRecord {
    /// Signatory.
    pub node: CantonId,
    /// Claimed hosting participant; readers cross-check it in topology.
    pub participant_id: String,
    pub display_name: String,
    pub version: String,
    pub build_version: String,
    pub coordination_version: i64,
    /// Observers.
    pub peers: Vec<CantonId>,
    /// Micros since the epoch.
    pub last_active_at: i64,
    pub heartbeat_interval_secs: i64,
    pub min_heartbeat_interval_secs: i64,
}

impl TemplateRecord for DecmanNodeRecord {
    const TEMPLATE: CoordinationTemplate = CoordinationTemplate::DecmanNode;

    fn to_record(&self) -> Record {
        record(vec![
            field("node", make_party(&self.node)),
            field("participantId", make_text(&self.participant_id)),
            field("displayName", make_text(&self.display_name)),
            field("version", make_text(&self.version)),
            field("buildVersion", make_text(&self.build_version)),
            field("coordinationVersion", make_int64(self.coordination_version)),
            field("peers", make_party_list(&self.peers)),
            field("lastActiveAt", make_timestamp(self.last_active_at)),
            field(
                "heartbeatIntervalSecs",
                make_int64(self.heartbeat_interval_secs),
            ),
            field(
                "minHeartbeatIntervalSecs",
                make_int64(self.min_heartbeat_interval_secs),
            ),
        ])
    }

    fn from_record(rec: &Record) -> Result<Self> {
        Ok(Self {
            node: req_party(rec, "node")?,
            participant_id: req_text(rec, "participantId")?,
            display_name: req_text(rec, "displayName")?,
            version: req_text(rec, "version")?,
            build_version: req_text(rec, "buildVersion")?,
            coordination_version: req_int64(rec, "coordinationVersion")?,
            peers: req_party_list(rec, "peers")?,
            last_active_at: req_timestamp(rec, "lastActiveAt")?,
            heartbeat_interval_secs: req_int64(rec, "heartbeatIntervalSecs")?,
            min_heartbeat_interval_secs: req_int64(rec, "minHeartbeatIntervalSecs")?,
        })
    }
}

/// Arguments of `DecmanNode_Update`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecmanNodeUpdateArgs {
    pub new_peers: Vec<CantonId>,
    pub new_display_name: String,
    pub new_version: String,
    pub new_build_version: String,
    pub new_coordination_version: i64,
    pub new_heartbeat_interval_secs: i64,
    pub new_min_heartbeat_interval_secs: i64,
}

impl DecmanNodeUpdateArgs {
    /// The update that turns any current entry into `desired`.
    pub fn from_desired(desired: &DecmanNodeRecord) -> Self {
        Self {
            new_peers: desired.peers.clone(),
            new_display_name: desired.display_name.clone(),
            new_version: desired.version.clone(),
            new_build_version: desired.build_version.clone(),
            new_coordination_version: desired.coordination_version,
            new_heartbeat_interval_secs: desired.heartbeat_interval_secs,
            new_min_heartbeat_interval_secs: desired.min_heartbeat_interval_secs,
        }
    }
}

impl ChoiceArgument for DecmanNodeUpdateArgs {
    fn to_record(&self) -> Record {
        record(vec![
            field("newPeers", make_party_list(&self.new_peers)),
            field("newDisplayName", make_text(&self.new_display_name)),
            field("newVersion", make_text(&self.new_version)),
            field("newBuildVersion", make_text(&self.new_build_version)),
            field(
                "newCoordinationVersion",
                make_int64(self.new_coordination_version),
            ),
            field(
                "newHeartbeatIntervalSecs",
                make_int64(self.new_heartbeat_interval_secs),
            ),
            field(
                "newMinHeartbeatIntervalSecs",
                make_int64(self.new_min_heartbeat_interval_secs),
            ),
        ])
    }
}

// ---------------------------------------------------------------------------
// Decman.Coordination.Workflow
// ---------------------------------------------------------------------------

/// `WorkflowProposal`: the intent and reference set of one run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowProposalRecord {
    /// Signatory.
    pub proposer: CantonId,
    pub proposer_participant: String,
    pub proposer_namespace_fingerprint: Option<String>,
    pub proposer_signing_public_key_hex: Option<String>,
    pub proposer_daml_key_fingerprint: Option<String>,
    pub run_id: String,
    pub kind: WorkflowKind,
    /// Observers.
    pub invitees: Vec<CantonId>,
    /// Participant ids of every member of the run, proposer included.
    pub participants: Vec<String>,
    pub dec_party_id: Option<String>,
    pub prefix: Option<String>,
    pub threshold: Option<i64>,
    pub previous_threshold: Option<i64>,
    pub dnd_base_serial: Option<i64>,
    pub p2p_base_serial: Option<i64>,
    pub new_participant: Option<String>,
    pub kicked_participant: Option<String>,
    pub dar_pins: Vec<DarPin>,
    pub package_names: Vec<String>,
    pub description: String,
    /// Micros since the epoch.
    pub created_at: i64,
    /// Micros since the epoch. The template requires `expires_at > created_at`.
    pub expires_at: i64,
}

impl TemplateRecord for WorkflowProposalRecord {
    const TEMPLATE: CoordinationTemplate = CoordinationTemplate::WorkflowProposal;

    fn to_record(&self) -> Record {
        record(vec![
            field("proposer", make_party(&self.proposer)),
            field("proposerParticipant", make_text(&self.proposer_participant)),
            field(
                "proposerNamespaceFingerprint",
                make_optional_text(&self.proposer_namespace_fingerprint),
            ),
            field(
                "proposerSigningPublicKeyHex",
                make_optional_text(&self.proposer_signing_public_key_hex),
            ),
            field(
                "proposerDamlKeyFingerprint",
                make_optional_text(&self.proposer_daml_key_fingerprint),
            ),
            field("runId", make_text(&self.run_id)),
            field("kind", make_enum(self.kind.as_str())),
            field("invitees", make_party_list(&self.invitees)),
            field("participants", make_text_list(&self.participants)),
            field("decPartyId", make_optional_text(&self.dec_party_id)),
            field("prefix", make_optional_text(&self.prefix)),
            field("threshold", make_optional_int64(&self.threshold)),
            field(
                "previousThreshold",
                make_optional_int64(&self.previous_threshold),
            ),
            field("dndBaseSerial", make_optional_int64(&self.dnd_base_serial)),
            field("p2pBaseSerial", make_optional_int64(&self.p2p_base_serial)),
            field("newParticipant", make_optional_text(&self.new_participant)),
            field(
                "kickedParticipant",
                make_optional_text(&self.kicked_participant),
            ),
            field(
                "darPins",
                make_list(self.dar_pins.iter().map(DarPin::to_value).collect()),
            ),
            field("packageNames", make_text_list(&self.package_names)),
            field("description", make_text(&self.description)),
            field("createdAt", make_timestamp(self.created_at)),
            field("expiresAt", make_timestamp(self.expires_at)),
        ])
    }

    fn from_record(rec: &Record) -> Result<Self> {
        Ok(Self {
            proposer: req_party(rec, "proposer")?,
            proposer_participant: req_text(rec, "proposerParticipant")?,
            proposer_namespace_fingerprint: opt_text(rec, "proposerNamespaceFingerprint")?,
            proposer_signing_public_key_hex: opt_text(rec, "proposerSigningPublicKeyHex")?,
            proposer_daml_key_fingerprint: opt_text(rec, "proposerDamlKeyFingerprint")?,
            run_id: req_text(rec, "runId")?,
            kind: parse_kind(&req_enum(rec, "kind")?)?,
            invitees: req_party_list(rec, "invitees")?,
            participants: req_text_list(rec, "participants")?,
            dec_party_id: opt_text(rec, "decPartyId")?,
            prefix: opt_text(rec, "prefix")?,
            threshold: opt_int64(rec, "threshold")?,
            previous_threshold: opt_int64(rec, "previousThreshold")?,
            dnd_base_serial: opt_int64(rec, "dndBaseSerial")?,
            p2p_base_serial: opt_int64(rec, "p2pBaseSerial")?,
            new_participant: opt_text(rec, "newParticipant")?,
            kicked_participant: opt_text(rec, "kickedParticipant")?,
            dar_pins: req_record_list(rec, "darPins")?
                .into_iter()
                .map(DarPin::from_record)
                .collect::<Result<Vec<_>>>()
                .context("field `darPins`")?,
            package_names: req_text_list(rec, "packageNames")?,
            description: req_text(rec, "description")?,
            created_at: req_timestamp(rec, "createdAt")?,
            expires_at: req_timestamp(rec, "expiresAt")?,
        })
    }
}

/// Arguments of `WorkflowProposal_Accept`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptArgs {
    pub acceptor: CantonId,
    pub participant_id: String,
    pub namespace_fingerprint: Option<String>,
    pub signing_public_key_hex: Option<String>,
    pub daml_key_fingerprint: Option<String>,
    pub member_party: Option<CantonId>,
}

impl ChoiceArgument for AcceptArgs {
    fn to_record(&self) -> Record {
        record(vec![
            field("acceptor", make_party(&self.acceptor)),
            field("participantId", make_text(&self.participant_id)),
            field(
                "namespaceFingerprint",
                make_optional_text(&self.namespace_fingerprint),
            ),
            field(
                "signingPublicKeyHex",
                make_optional_text(&self.signing_public_key_hex),
            ),
            field(
                "damlKeyFingerprint",
                make_optional_text(&self.daml_key_fingerprint),
            ),
            field("memberParty", make_optional_party(&self.member_party)),
        ])
    }
}

/// Arguments of `WorkflowProposal_Decline`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclineArgs {
    pub decliner: CantonId,
    pub reason: String,
}

impl ChoiceArgument for DeclineArgs {
    fn to_record(&self) -> Record {
        record(vec![
            field("decliner", make_party(&self.decliner)),
            field("reason", make_text(&self.reason)),
        ])
    }
}

/// Arguments of `WorkflowProposal_Finish`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinishArgs {
    pub succeeded: bool,
    pub error: Option<String>,
}

impl ChoiceArgument for FinishArgs {
    fn to_record(&self) -> Record {
        record(vec![
            field("succeeded", make_bool(self.succeeded)),
            field("error", make_optional_text(&self.error)),
        ])
    }
}

/// `WorkflowAcceptance`: one invitee's consent plus its key material.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowAcceptanceRecord {
    /// The `WorkflowProposal` contract id this acceptance answers.
    pub proposal: String,
    pub proposer: CantonId,
    /// Signatory.
    pub acceptor: CantonId,
    pub observers: Vec<CantonId>,
    pub run_id: String,
    pub participant_id: String,
    pub namespace_fingerprint: Option<String>,
    pub signing_public_key_hex: Option<String>,
    pub daml_key_fingerprint: Option<String>,
    pub member_party: Option<CantonId>,
    /// Micros since the epoch.
    pub accepted_at: i64,
}

impl TemplateRecord for WorkflowAcceptanceRecord {
    const TEMPLATE: CoordinationTemplate = CoordinationTemplate::WorkflowAcceptance;

    fn to_record(&self) -> Record {
        record(vec![
            field("proposal", make_contract_id(&self.proposal)),
            field("proposer", make_party(&self.proposer)),
            field("acceptor", make_party(&self.acceptor)),
            field("observers", make_party_list(&self.observers)),
            field("runId", make_text(&self.run_id)),
            field("participantId", make_text(&self.participant_id)),
            field(
                "namespaceFingerprint",
                make_optional_text(&self.namespace_fingerprint),
            ),
            field(
                "signingPublicKeyHex",
                make_optional_text(&self.signing_public_key_hex),
            ),
            field(
                "damlKeyFingerprint",
                make_optional_text(&self.daml_key_fingerprint),
            ),
            field("memberParty", make_optional_party(&self.member_party)),
            field("acceptedAt", make_timestamp(self.accepted_at)),
        ])
    }

    fn from_record(rec: &Record) -> Result<Self> {
        Ok(Self {
            proposal: req_contract_id(rec, "proposal")?,
            proposer: req_party(rec, "proposer")?,
            acceptor: req_party(rec, "acceptor")?,
            observers: req_party_list(rec, "observers")?,
            run_id: req_text(rec, "runId")?,
            participant_id: req_text(rec, "participantId")?,
            namespace_fingerprint: opt_text(rec, "namespaceFingerprint")?,
            signing_public_key_hex: opt_text(rec, "signingPublicKeyHex")?,
            daml_key_fingerprint: opt_text(rec, "damlKeyFingerprint")?,
            member_party: opt_party(rec, "memberParty")?,
            accepted_at: req_timestamp(rec, "acceptedAt")?,
        })
    }
}

/// `WorkflowDecline`: one invitee's refusal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowDeclineRecord {
    pub proposal: String,
    pub proposer: CantonId,
    /// Signatory.
    pub decliner: CantonId,
    pub observers: Vec<CantonId>,
    pub run_id: String,
    pub reason: String,
    /// Micros since the epoch.
    pub declined_at: i64,
}

impl TemplateRecord for WorkflowDeclineRecord {
    const TEMPLATE: CoordinationTemplate = CoordinationTemplate::WorkflowDecline;

    fn to_record(&self) -> Record {
        record(vec![
            field("proposal", make_contract_id(&self.proposal)),
            field("proposer", make_party(&self.proposer)),
            field("decliner", make_party(&self.decliner)),
            field("observers", make_party_list(&self.observers)),
            field("runId", make_text(&self.run_id)),
            field("reason", make_text(&self.reason)),
            field("declinedAt", make_timestamp(self.declined_at)),
        ])
    }

    fn from_record(rec: &Record) -> Result<Self> {
        Ok(Self {
            proposal: req_contract_id(rec, "proposal")?,
            proposer: req_party(rec, "proposer")?,
            decliner: req_party(rec, "decliner")?,
            observers: req_party_list(rec, "observers")?,
            run_id: req_text(rec, "runId")?,
            reason: req_text(rec, "reason")?,
            declined_at: req_timestamp(rec, "declinedAt")?,
        })
    }
}

/// `WorkflowOutcome`: the proposer's final word on a run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowOutcomeRecord {
    /// Signatory.
    pub proposer: CantonId,
    pub run_id: String,
    pub kind: WorkflowKind,
    pub observers: Vec<CantonId>,
    pub succeeded: bool,
    pub error: Option<String>,
    /// Micros since the epoch.
    pub finished_at: i64,
}

impl TemplateRecord for WorkflowOutcomeRecord {
    const TEMPLATE: CoordinationTemplate = CoordinationTemplate::WorkflowOutcome;

    fn to_record(&self) -> Record {
        record(vec![
            field("proposer", make_party(&self.proposer)),
            field("runId", make_text(&self.run_id)),
            field("kind", make_enum(self.kind.as_str())),
            field("observers", make_party_list(&self.observers)),
            field("succeeded", make_bool(self.succeeded)),
            field("error", make_optional_text(&self.error)),
            field("finishedAt", make_timestamp(self.finished_at)),
        ])
    }

    fn from_record(rec: &Record) -> Result<Self> {
        Ok(Self {
            proposer: req_party(rec, "proposer")?,
            run_id: req_text(rec, "runId")?,
            kind: parse_kind(&req_enum(rec, "kind")?)?,
            observers: req_party_list(rec, "observers")?,
            succeeded: req_bool(rec, "succeeded")?,
            error: opt_text(rec, "error")?,
            finished_at: req_timestamp(rec, "finishedAt")?,
        })
    }
}

// ---------------------------------------------------------------------------
// Decman.Coordination.Submission
// ---------------------------------------------------------------------------

/// `SubmissionRound`: one prepared Daml transaction awaiting signatures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmissionRoundRecord {
    /// Signatory.
    pub proposer: CantonId,
    pub run_id: String,
    pub index: i64,
    /// Observers.
    pub signers: Vec<CantonId>,
    pub dec_party_id: String,
    pub act_as: String,
    pub description: String,
    pub prepared_transaction_hex: String,
    pub prepared_hash_hex: String,
    pub hashing_scheme_version: i64,
    /// Micros since the epoch.
    pub preparation_time: i64,
    /// Micros since the epoch.
    pub max_record_time: i64,
    /// Micros since the epoch. The template requires `deadline <= max_record_time`.
    pub deadline: i64,
}

impl TemplateRecord for SubmissionRoundRecord {
    const TEMPLATE: CoordinationTemplate = CoordinationTemplate::SubmissionRound;

    fn to_record(&self) -> Record {
        record(vec![
            field("proposer", make_party(&self.proposer)),
            field("runId", make_text(&self.run_id)),
            field("index", make_int64(self.index)),
            field("signers", make_party_list(&self.signers)),
            field("decPartyId", make_text(&self.dec_party_id)),
            field("actAs", make_text(&self.act_as)),
            field("description", make_text(&self.description)),
            field(
                "preparedTransactionHex",
                make_text(&self.prepared_transaction_hex),
            ),
            field("preparedHashHex", make_text(&self.prepared_hash_hex)),
            field(
                "hashingSchemeVersion",
                make_int64(self.hashing_scheme_version),
            ),
            field("preparationTime", make_timestamp(self.preparation_time)),
            field("maxRecordTime", make_timestamp(self.max_record_time)),
            field("deadline", make_timestamp(self.deadline)),
        ])
    }

    fn from_record(rec: &Record) -> Result<Self> {
        Ok(Self {
            proposer: req_party(rec, "proposer")?,
            run_id: req_text(rec, "runId")?,
            index: req_int64(rec, "index")?,
            signers: req_party_list(rec, "signers")?,
            dec_party_id: req_text(rec, "decPartyId")?,
            act_as: req_text(rec, "actAs")?,
            description: req_text(rec, "description")?,
            prepared_transaction_hex: req_text(rec, "preparedTransactionHex")?,
            prepared_hash_hex: req_text(rec, "preparedHashHex")?,
            hashing_scheme_version: req_int64(rec, "hashingSchemeVersion")?,
            preparation_time: req_timestamp(rec, "preparationTime")?,
            max_record_time: req_timestamp(rec, "maxRecordTime")?,
            deadline: req_timestamp(rec, "deadline")?,
        })
    }
}

/// Arguments of `SubmissionRound_Sign`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignArgs {
    pub signer: CantonId,
    pub participant_id: String,
    pub signed_by: String,
    pub signature_hex: String,
    pub format: String,
    pub algorithm: String,
}

impl ChoiceArgument for SignArgs {
    fn to_record(&self) -> Record {
        record(vec![
            field("signer", make_party(&self.signer)),
            field("participantId", make_text(&self.participant_id)),
            field("signedBy", make_text(&self.signed_by)),
            field("signatureHex", make_text(&self.signature_hex)),
            field("format", make_text(&self.format)),
            field("algorithm", make_text(&self.algorithm)),
        ])
    }
}

/// Arguments of `SubmissionRound_Close`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CloseArgs {
    pub result: String,
}

impl ChoiceArgument for CloseArgs {
    fn to_record(&self) -> Record {
        record(vec![field("result", make_text(&self.result))])
    }
}

/// `SubmissionSignature`: one signer's signature over a round.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmissionSignatureRecord {
    /// The `SubmissionRound` contract id.
    pub round: String,
    pub proposer: CantonId,
    /// Signatory.
    pub signer: CantonId,
    pub observers: Vec<CantonId>,
    pub run_id: String,
    pub index: i64,
    pub participant_id: String,
    pub signed_by: String,
    pub signature_hex: String,
    pub format: String,
    pub algorithm: String,
    /// Micros since the epoch.
    pub signed_at: i64,
}

impl TemplateRecord for SubmissionSignatureRecord {
    const TEMPLATE: CoordinationTemplate = CoordinationTemplate::SubmissionSignature;

    fn to_record(&self) -> Record {
        record(vec![
            field("round", make_contract_id(&self.round)),
            field("proposer", make_party(&self.proposer)),
            field("signer", make_party(&self.signer)),
            field("observers", make_party_list(&self.observers)),
            field("runId", make_text(&self.run_id)),
            field("index", make_int64(self.index)),
            field("participantId", make_text(&self.participant_id)),
            field("signedBy", make_text(&self.signed_by)),
            field("signatureHex", make_text(&self.signature_hex)),
            field("format", make_text(&self.format)),
            field("algorithm", make_text(&self.algorithm)),
            field("signedAt", make_timestamp(self.signed_at)),
        ])
    }

    fn from_record(rec: &Record) -> Result<Self> {
        Ok(Self {
            round: req_contract_id(rec, "round")?,
            proposer: req_party(rec, "proposer")?,
            signer: req_party(rec, "signer")?,
            observers: req_party_list(rec, "observers")?,
            run_id: req_text(rec, "runId")?,
            index: req_int64(rec, "index")?,
            participant_id: req_text(rec, "participantId")?,
            signed_by: req_text(rec, "signedBy")?,
            signature_hex: req_text(rec, "signatureHex")?,
            format: req_text(rec, "format")?,
            algorithm: req_text(rec, "algorithm")?,
            signed_at: req_timestamp(rec, "signedAt")?,
        })
    }
}

// ---------------------------------------------------------------------------
// Decman.Coordination.Acs
// ---------------------------------------------------------------------------

/// `AcsManifest`: pins an offline ACS snapshot the operator moves by hand.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcsManifestRecord {
    /// Signatory.
    pub exporter: CantonId,
    pub exporter_participant: String,
    pub observers: Vec<CantonId>,
    pub dec_party_id: String,
    pub target_participant: String,
    pub activation_serial: i64,
    pub size_bytes: i64,
    pub sha256_hex: String,
    pub package_ids: Vec<String>,
    /// Micros since the epoch.
    pub exported_at: i64,
}

impl TemplateRecord for AcsManifestRecord {
    const TEMPLATE: CoordinationTemplate = CoordinationTemplate::AcsManifest;

    fn to_record(&self) -> Record {
        record(vec![
            field("exporter", make_party(&self.exporter)),
            field("exporterParticipant", make_text(&self.exporter_participant)),
            field("observers", make_party_list(&self.observers)),
            field("decPartyId", make_text(&self.dec_party_id)),
            field("targetParticipant", make_text(&self.target_participant)),
            field("activationSerial", make_int64(self.activation_serial)),
            field("sizeBytes", make_int64(self.size_bytes)),
            field("sha256Hex", make_text(&self.sha256_hex)),
            field("packageIds", make_text_list(&self.package_ids)),
            field("exportedAt", make_timestamp(self.exported_at)),
        ])
    }

    fn from_record(rec: &Record) -> Result<Self> {
        Ok(Self {
            exporter: req_party(rec, "exporter")?,
            exporter_participant: req_text(rec, "exporterParticipant")?,
            observers: req_party_list(rec, "observers")?,
            dec_party_id: req_text(rec, "decPartyId")?,
            target_participant: req_text(rec, "targetParticipant")?,
            activation_serial: req_int64(rec, "activationSerial")?,
            size_bytes: req_int64(rec, "sizeBytes")?,
            sha256_hex: req_text(rec, "sha256Hex")?,
            package_ids: req_text_list(rec, "packageIds")?,
            exported_at: req_timestamp(rec, "exportedAt")?,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

    pub(crate) fn party(prefix: &str) -> CantonId {
        CantonId::parse(&format!("{prefix}::{NS}")).expect("valid canton id")
    }

    fn participant(n: u8) -> String {
        format!("participant{n}::{NS}")
    }

    fn labels(rec: &Record) -> Vec<&str> {
        rec.fields.iter().map(|f| f.label.as_str()).collect()
    }

    fn round_trip<T: TemplateRecord + PartialEq + std::fmt::Debug>(value: &T) {
        let decoded = T::from_record(&value.to_record()).expect("decodes");
        assert_eq!(&decoded, value);
    }

    pub(crate) fn node_record(prefix: &str, peers: &[&str]) -> DecmanNodeRecord {
        DecmanNodeRecord {
            node: party(prefix),
            participant_id: participant(1),
            display_name: "Node One".into(),
            version: "2.0.0".into(),
            build_version: "2.0.0-dev".into(),
            coordination_version: 1,
            peers: peers.iter().map(|p| party(p)).collect(),
            last_active_at: 1_700_000_000_000_000,
            heartbeat_interval_secs: 3600,
            min_heartbeat_interval_secs: 60,
        }
    }

    pub(crate) fn proposal_full() -> WorkflowProposalRecord {
        WorkflowProposalRecord {
            proposer: party("node-a"),
            proposer_participant: participant(1),
            proposer_namespace_fingerprint: Some("1220aa".into()),
            proposer_signing_public_key_hex: Some("0a1b2c".into()),
            proposer_daml_key_fingerprint: Some("1220bb".into()),
            run_id: "cbtc-creation".into(),
            kind: WorkflowKind::Onboarding,
            invitees: vec![party("node-b"), party("node-c")],
            participants: vec![participant(1), participant(2), participant(3)],
            dec_party_id: Some(format!("cbtc::{NS}")),
            prefix: Some("cbtc".into()),
            threshold: Some(2),
            previous_threshold: Some(3),
            dnd_base_serial: Some(4),
            p2p_base_serial: Some(5),
            new_participant: Some(participant(4)),
            kicked_participant: Some(participant(3)),
            dar_pins: vec![DarPin {
                filename: "governance-core-v1-0.1.0.dar".into(),
                sha256_hex: "deadbeef".into(),
                main_package_id: "abc123".into(),
                size_bytes: 368_723,
            }],
            package_names: vec!["governance-core-v1".into()],
            description: "Create the cbtc party".into(),
            created_at: 1_700_000_000_000_000,
            expires_at: 1_700_604_800_000_000,
        }
    }

    pub(crate) fn proposal_minimal() -> WorkflowProposalRecord {
        WorkflowProposalRecord {
            proposer_namespace_fingerprint: None,
            proposer_signing_public_key_hex: None,
            proposer_daml_key_fingerprint: None,
            dec_party_id: None,
            prefix: None,
            threshold: None,
            previous_threshold: None,
            dnd_base_serial: None,
            p2p_base_serial: None,
            new_participant: None,
            kicked_participant: None,
            dar_pins: vec![],
            package_names: vec![],
            kind: WorkflowKind::Dars,
            ..proposal_full()
        }
    }

    pub(crate) fn acceptance_full(proposal: &str, acceptor: &str) -> WorkflowAcceptanceRecord {
        WorkflowAcceptanceRecord {
            proposal: proposal.into(),
            proposer: party("node-a"),
            acceptor: party(acceptor),
            observers: vec![party("node-b"), party("node-c")],
            run_id: "cbtc-creation".into(),
            participant_id: participant(2),
            namespace_fingerprint: Some("1220cc".into()),
            signing_public_key_hex: Some("0a0b".into()),
            daml_key_fingerprint: Some("1220dd".into()),
            member_party: Some(party("member-b")),
            accepted_at: 1_700_000_100_000_000,
        }
    }

    fn acceptance_minimal() -> WorkflowAcceptanceRecord {
        WorkflowAcceptanceRecord {
            namespace_fingerprint: None,
            signing_public_key_hex: None,
            daml_key_fingerprint: None,
            member_party: None,
            ..acceptance_full("00proposal", "node-b")
        }
    }

    fn decline() -> WorkflowDeclineRecord {
        WorkflowDeclineRecord {
            proposal: "00proposal".into(),
            proposer: party("node-a"),
            decliner: party("node-c"),
            observers: vec![party("node-b"), party("node-c")],
            run_id: "cbtc-creation".into(),
            reason: "not now".into(),
            declined_at: 1_700_000_200_000_000,
        }
    }

    fn outcome(error: Option<&str>) -> WorkflowOutcomeRecord {
        WorkflowOutcomeRecord {
            proposer: party("node-a"),
            run_id: "cbtc-creation".into(),
            kind: WorkflowKind::Kick,
            observers: vec![party("node-b")],
            succeeded: error.is_none(),
            error: error.map(str::to_string),
            finished_at: 1_700_000_300_000_000,
        }
    }

    fn round() -> SubmissionRoundRecord {
        SubmissionRoundRecord {
            proposer: party("node-a"),
            run_id: "cbtc-contracts-1".into(),
            index: 0,
            signers: vec![party("node-b"), party("node-c")],
            dec_party_id: format!("cbtc::{NS}"),
            act_as: format!("cbtc::{NS}"),
            description: "GovernanceRules".into(),
            prepared_transaction_hex: "0a0b0c".into(),
            prepared_hash_hex: "1220ee".into(),
            hashing_scheme_version: 2,
            preparation_time: 1_700_000_000_000_000,
            max_record_time: 1_700_072_000_000_000,
            deadline: 1_700_070_200_000_000,
        }
    }

    fn signature() -> SubmissionSignatureRecord {
        SubmissionSignatureRecord {
            round: "00round".into(),
            proposer: party("node-a"),
            signer: party("node-b"),
            observers: vec![party("node-b"), party("node-c")],
            run_id: "cbtc-contracts-1".into(),
            index: 0,
            participant_id: participant(2),
            signed_by: "1220ff".into(),
            signature_hex: "abcd".into(),
            format: "SIGNATURE_FORMAT_CONCAT".into(),
            algorithm: "SIGNING_ALGORITHM_SPEC_ED25519".into(),
            signed_at: 1_700_000_400_000_000,
        }
    }

    fn manifest() -> AcsManifestRecord {
        AcsManifestRecord {
            exporter: party("node-a"),
            exporter_participant: participant(1),
            observers: vec![party("node-d")],
            dec_party_id: format!("cbtc::{NS}"),
            target_participant: participant(4),
            activation_serial: 7,
            size_bytes: 1024,
            sha256_hex: "00ff".into(),
            package_ids: vec!["abc".into(), "def".into()],
            exported_at: 1_700_000_500_000_000,
        }
    }

    #[test]
    fn decman_node_round_trips_with_and_without_peers() {
        round_trip(&node_record("node-a", &["node-b", "node-c"]));
        round_trip(&node_record("node-a", &[]));
    }

    #[test]
    fn decman_node_field_order_matches_the_template() {
        let rec = node_record("node-a", &[]).to_record();
        assert_eq!(
            labels(&rec),
            [
                "node",
                "participantId",
                "displayName",
                "version",
                "buildVersion",
                "coordinationVersion",
                "peers",
                "lastActiveAt",
                "heartbeatIntervalSecs",
                "minHeartbeatIntervalSecs",
            ]
        );
    }

    #[test]
    fn workflow_proposal_round_trips_all_some_and_all_none() {
        round_trip(&proposal_full());
        round_trip(&proposal_minimal());
    }

    #[test]
    fn workflow_proposal_field_order_matches_the_template() {
        let rec = proposal_full().to_record();
        assert_eq!(
            labels(&rec),
            [
                "proposer",
                "proposerParticipant",
                "proposerNamespaceFingerprint",
                "proposerSigningPublicKeyHex",
                "proposerDamlKeyFingerprint",
                "runId",
                "kind",
                "invitees",
                "participants",
                "decPartyId",
                "prefix",
                "threshold",
                "previousThreshold",
                "dndBaseSerial",
                "p2pBaseSerial",
                "newParticipant",
                "kickedParticipant",
                "darPins",
                "packageNames",
                "description",
                "createdAt",
                "expiresAt",
            ]
        );
    }

    #[test]
    fn every_workflow_kind_encodes_as_its_daml_constructor() {
        for kind in [
            WorkflowKind::Onboarding,
            WorkflowKind::AddParty,
            WorkflowKind::Kick,
            WorkflowKind::ChangeThreshold,
            WorkflowKind::Contracts,
            WorkflowKind::Dars,
        ] {
            let proposal = WorkflowProposalRecord {
                kind,
                ..proposal_minimal()
            };
            let rec = proposal.to_record();
            assert_eq!(req_enum(&rec, "kind").expect("enum"), kind.as_str());
            round_trip(&proposal);
        }
    }

    #[test]
    fn workflow_acceptance_round_trips_all_some_and_all_none() {
        round_trip(&acceptance_full("00proposal", "node-b"));
        round_trip(&acceptance_minimal());
    }

    #[test]
    fn workflow_decline_round_trips() {
        round_trip(&decline());
    }

    #[test]
    fn workflow_outcome_round_trips_with_and_without_error() {
        round_trip(&outcome(None));
        round_trip(&outcome(Some("Peer declined")));
    }

    #[test]
    fn submission_round_and_signature_round_trip() {
        round_trip(&round());
        round_trip(&signature());
    }

    #[test]
    fn acs_manifest_round_trips() {
        round_trip(&manifest());
        round_trip(&AcsManifestRecord {
            package_ids: vec![],
            observers: vec![],
            ..manifest()
        });
    }

    #[test]
    fn choice_arguments_carry_the_daml_labels() {
        let update = DecmanNodeUpdateArgs::from_desired(&node_record("node-a", &["node-b"]));
        assert_eq!(
            labels(&update.to_record()),
            [
                "newPeers",
                "newDisplayName",
                "newVersion",
                "newBuildVersion",
                "newCoordinationVersion",
                "newHeartbeatIntervalSecs",
                "newMinHeartbeatIntervalSecs",
            ]
        );
        let accept = AcceptArgs {
            acceptor: party("node-b"),
            participant_id: participant(2),
            namespace_fingerprint: None,
            signing_public_key_hex: None,
            daml_key_fingerprint: None,
            member_party: None,
        };
        assert_eq!(
            labels(&accept.to_record()),
            [
                "acceptor",
                "participantId",
                "namespaceFingerprint",
                "signingPublicKeyHex",
                "damlKeyFingerprint",
                "memberParty",
            ]
        );
        let decline = DeclineArgs {
            decliner: party("node-c"),
            reason: "no".into(),
        };
        assert_eq!(labels(&decline.to_record()), ["decliner", "reason"]);
        let finish = FinishArgs {
            succeeded: false,
            error: Some("x".into()),
        };
        assert_eq!(labels(&finish.to_record()), ["succeeded", "error"]);
        let sign = SignArgs {
            signer: party("node-b"),
            participant_id: participant(2),
            signed_by: "1220ff".into(),
            signature_hex: "ab".into(),
            format: "f".into(),
            algorithm: "a".into(),
        };
        assert_eq!(
            labels(&sign.to_record()),
            [
                "signer",
                "participantId",
                "signedBy",
                "signatureHex",
                "format",
                "algorithm",
            ]
        );
        assert_eq!(
            labels(
                &CloseArgs {
                    result: "ok".into()
                }
                .to_record()
            ),
            ["result"]
        );
        assert!(matches!(
            unit_argument().sum,
            Some(value::Sum::Record(r)) if r.fields.is_empty()
        ));
    }

    #[test]
    fn a_missing_field_is_an_error_not_a_default() {
        let mut rec = node_record("node-a", &[]).to_record();
        rec.fields.retain(|f| f.label != "coordinationVersion");
        let err = DecmanNodeRecord::from_record(&rec).expect_err("must fail");
        assert!(err.to_string().contains("coordinationVersion"), "{err}");
    }

    #[test]
    fn a_wrong_shaped_optional_is_an_error() {
        let mut rec = proposal_minimal().to_record();
        for f in &mut rec.fields {
            if f.label == "threshold" {
                f.value = Some(make_int64(3));
            }
        }
        let err = WorkflowProposalRecord::from_record(&rec).expect_err("must fail");
        assert!(err.to_string().contains("threshold"), "{err}");
    }

    #[test]
    fn an_unknown_kind_constructor_is_an_error() {
        let mut rec = proposal_minimal().to_record();
        for f in &mut rec.fields {
            if f.label == "kind" {
                f.value = Some(make_enum("Teleport"));
            }
        }
        let err = WorkflowProposalRecord::from_record(&rec).expect_err("must fail");
        assert!(err.to_string().contains("Teleport"), "{err}");
    }
}
