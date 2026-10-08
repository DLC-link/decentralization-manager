use serde::{Deserialize, Serialize};

use crate::{canton_id::CantonId, error::Result};

/// Wire DTOs for contract / DAR deployment. Defined in `common::api` (the
/// frontend's TypeScript is generated from them); re-exported so
/// `crate::workflow::contracts::{ContractDefinition, DarFile, FieldDefinition}`
/// resolve unchanged.
pub use common::api::{ContractDefinition, DarFile, FieldDefinition};

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
    /// Contract definitions to create after decentralized party setup
    #[serde(default)]
    pub contracts: Vec<ContractDefinition>,
    /// Workflow instance name for directory organization (e.g., "xyz-network-contracts-20260108-143052")
    #[serde(default)]
    pub instance_name: String,
    /// The creates this run committed to before it invited any peer
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_deployment: Option<CommittedDeployment>,
}

/// The creates a contracts run commits to when it starts. Peers verify every
/// prepared transaction against `intents`, so preparation must reproduce them
/// exactly, including the threshold read from the party cache at that moment.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct CommittedDeployment {
    /// The party threshold that unset `GovernanceThreshold` fields resolved to
    pub governance_threshold: i64,
    /// One create commitment per contract, in contract order
    pub intents: Vec<common::api::ContractDeploymentIntent>,
}

impl ContractsConfig {
    pub fn new(
        decentralized_party_id: CantonId,
        participant_ids: Vec<CantonId>,
        participant_parties: Vec<CantonId>,
        operator_party: CantonId,
        contracts: Vec<ContractDefinition>,
        instance_name: String,
    ) -> Self {
        Self {
            decentralized_party_id,
            participant_ids,
            participant_parties,
            operator_party,
            contracts,
            instance_name,
            committed_deployment: None,
        }
    }

    /// The creates this run committed to when it started.
    ///
    /// # Errors
    /// Fails for a run that an older version started without recording them.
    pub fn committed(&self) -> Result<&CommittedDeployment> {
        self.committed_deployment.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Contracts run {} has no recorded deployment commitments; start a new run",
                self.instance_name
            )
        })
    }
}
