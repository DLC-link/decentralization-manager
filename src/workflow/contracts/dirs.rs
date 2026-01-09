use std::{marker::PhantomData, path::PathBuf};

use crate::{
    consts::{DNS_SUBMISSION_DIR, PARTICIPANT_IDS_DIR, PARTICIPANT_KEYS_DIR},
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
    /// Create new ContractsDirs with base directory, instance name, party prefix, and dars directory
    pub fn with_base(
        base_workflow_dir: PathBuf,
        instance_name: &str,
        party_prefix: &str,
        dars_dir: PathBuf,
    ) -> Self {
        let workflow_dir = base_workflow_dir.join(instance_name);
        // Keys and IDs are in the onboarding directory (party_prefix-creation)
        let onboarding_dir = base_workflow_dir.join(format!("{party_prefix}-creation"));
        Self {
            dars_dir,
            workflow_dir: workflow_dir.clone(),
            dns_submission_dir: workflow_dir.join(DNS_SUBMISSION_DIR),
            ids_dir: onboarding_dir.join(PARTICIPANT_IDS_DIR),
            keys_dir: onboarding_dir.join(PARTICIPANT_KEYS_DIR),
            _p: PhantomData,
        }
    }

    /// Create required directories that don't exist
    pub async fn create_dirs(&self) -> Result {
        utils::create_directories(&[&self.workflow_dir, &self.ids_dir, &self.keys_dir]).await
    }
}
