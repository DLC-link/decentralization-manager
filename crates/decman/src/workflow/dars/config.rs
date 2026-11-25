use serde::{Deserialize, Serialize};

use crate::{canton_id::CantonId, workflow::validation::hash_dar};

/// Configuration for DARs upload workflow, as `workflow_runs.config_json`
/// stores it.
///
/// It names the DARs but does not carry them. Their bytes are the run's
/// `DARS_BUNDLE` artefact, so the row stays small however large the DARs are.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DarsConfig {
    /// The DARs to distribute, in the order the operator gave them
    pub dar_files: Vec<DarListing>,
    /// Workflow instance name for directory organization
    pub instance_name: String,
    /// Selected peers — the only nodes the coordinator expects as peers
    pub peer_ids: Vec<CantonId>,
}

/// One DAR of a distribution run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DarListing {
    pub filename: String,
    /// SHA-256 of the DAR's bytes, as the invitation pins it
    pub hash: String,
}

impl DarListing {
    pub fn of(filename: &str, data: &[u8]) -> Self {
        Self {
            filename: filename.to_string(),
            hash: hash_dar(data),
        }
    }
}
