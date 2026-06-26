use serde::{Deserialize, Serialize};

use crate::canton_id::CantonId;

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
        }
    }
}
