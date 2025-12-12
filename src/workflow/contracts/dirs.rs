use std::{marker::PhantomData, path::PathBuf};

use crate::{
    consts::{DARS_DIR, DNS_SUBMISSION_DIR, PARTICIPANT_IDS_DIR, PARTICIPANT_KEYS_DIR, WORKFLOW_DATA_DIR},
    error::Result,
    utils,
};

/// Contracts workflow directory structure
#[derive(Clone, Debug)]
pub struct ContractsDirs {
    pub dars_dir: PathBuf,
    pub workflow_dir: PathBuf,
    pub dns_submission_dir: PathBuf,
    pub ids_dir: PathBuf,
    pub keys_dir: PathBuf,
    _p: PhantomData<()>,
}

impl ContractsDirs {
    /// Create new ContractsDirs with default paths
    pub fn new() -> Self {
        Self::with_base(
            PathBuf::from(format!("./{WORKFLOW_DATA_DIR}")),
            PathBuf::from(format!("./{DARS_DIR}")),
        )
    }

    /// Create new ContractsDirs with custom base directories
    pub fn with_base(workflow_dir: PathBuf, dars_dir: PathBuf) -> Self {
        Self {
            dars_dir,
            workflow_dir: workflow_dir.clone(),
            dns_submission_dir: workflow_dir.join(DNS_SUBMISSION_DIR),
            ids_dir: workflow_dir.join(PARTICIPANT_IDS_DIR),
            keys_dir: workflow_dir.join(PARTICIPANT_KEYS_DIR),
            _p: PhantomData,
        }
    }

    /// Create required directories that don't exist
    pub async fn create_dirs(&self) -> Result {
        utils::create_directories(&[&self.workflow_dir, &self.ids_dir, &self.keys_dir]).await
    }
}

impl Default for ContractsDirs {
    fn default() -> Self {
        Self::new()
    }
}
