//! Package config and the contract and DAR definitions the workflows deploy.

use serde::{Deserialize, Serialize};

use crate::canton_id::CantonId;

/// Package identifiers used by the built-in Daml integrations
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct PackageConfig {
    pub governance_action: Option<String>,
    pub governance_core: Option<String>,
    pub governance_rewards: Option<String>,
    pub governance_token_custody: Option<String>,
    pub governance_utility_credential: Option<String>,
    pub governance_utility_onboarding: Option<String>,
    /// Base credential package; defines the `Credential` template.
    pub utility_credential: Option<String>,
    /// Credential app package (services, offers). Bundles the base package
    /// as a dependency but does not define its templates.
    pub utility_credential_app: Option<String>,
    pub utility_registry: Option<String>,
}

/// A DAR file to upload
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DarFile {
    /// Filename (used as description when uploading)
    pub filename: String,
    /// Base64-encoded DAR file contents
    pub data: String,
}

/// Definition of a Daml contract to create on the ledger
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(no_recursion))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
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
    /// `None` value for any `Optional<T>` field — emits a typed-empty Optional.
    None,
}
