use serde::{Deserialize, Serialize};

use crate::participant_id::CantonId;

/// A DAR file to upload
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DarFile {
    /// Filename (used as description when uploading)
    pub filename: String,
    /// Base64-encoded DAR file contents
    pub data: String,
}

/// Configuration for contracts workflow
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ContractsConfig {
    /// Decentralized party ID to deploy contracts for
    pub decentralized_party_id: CantonId,
    /// List of participant IDs that will sign submissions
    #[serde(default)]
    pub participant_ids: Vec<CantonId>,
    /// List of party IDs for each participant (must match participant_ids order)
    #[serde(default)]
    pub participant_parties: Vec<CantonId>,
    /// Operator party ID
    pub operator_party: CantonId,
    /// DAR files to upload (base64-encoded)
    #[serde(default)]
    pub dar_files: Vec<DarFile>,
    /// Contract definitions to create after decentralized party setup
    #[serde(default)]
    pub contracts: Vec<ContractDefinition>,
    /// Workflow instance name for directory organization (e.g., "xyz-network-contracts-20260108-143052")
    #[serde(default)]
    pub instance_name: String,
}

impl ContractsConfig {
    pub fn new(
        decentralized_party_id: CantonId,
        participant_ids: Vec<CantonId>,
        participant_parties: Vec<CantonId>,
        operator_party: CantonId,
        dar_files: Vec<DarFile>,
        contracts: Vec<ContractDefinition>,
        instance_name: String,
    ) -> Self {
        Self {
            decentralized_party_id,
            participant_ids,
            participant_parties,
            operator_party,
            dar_files,
            contracts,
            instance_name,
        }
    }
}

/// Definition of a Daml contract to create on the ledger
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ContractDefinition {
    /// Unique identifier for this contract (used as command ID)
    pub id: String,
    /// Human-readable name for logging
    pub name: String,
    /// Package ID (can use # prefix for symbolic lookup)
    pub package_id: String,
    /// Module name (e.g., "CBTC.Governance")
    pub module_name: String,
    /// Entity/template name (e.g., "CBTCGovernanceRules")
    pub entity_name: String,
    /// Record fields for the create command
    pub fields: Vec<FieldDefinition>,
}

/// Definition of a field value in a Daml record
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldDefinition {
    /// The decentralized party ID
    DecentralizedParty,
    /// The operator party ID
    OperatorParty,
    /// A specific party ID
    ParticipantParty { id: CantonId },
    /// Static text value
    Text { value: String },
    /// Integer value
    Int64 { value: i64 },
    /// Boolean value
    Bool { value: bool },
    /// The instrument record (admin party + instrument id)
    Instrument { id: String },
    /// Set of all participant parties (as GenMap<Party, Unit>)
    AttestorsSet,
    /// Set of parties (as DA.Set.Types:Set Party - Record wrapped GenMap)
    PartySet { parties: Vec<CantonId> },
    /// Relative time value (as DA.Time.Types:RelTime - Record wrapped Int64 microseconds)
    RelTime { microseconds: i64 },
    /// Optional wrapper around another field
    Optional { inner: Box<FieldDefinition> },
    /// Nested record with fields
    Record { fields: Vec<FieldDefinition> },
    /// Governance threshold value
    GovernanceThreshold { value: Option<i64> },
}
