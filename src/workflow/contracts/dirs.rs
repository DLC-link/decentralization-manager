use std::{marker::PhantomData, path::PathBuf};

use crate::error::Result;

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
        let workflow_dir = PathBuf::from("./workflow-data");
        let dns_submission_dir = workflow_dir.join("dns-submission");

        Self {
            dars_dir: PathBuf::from("./dars"),
            workflow_dir: workflow_dir.clone(),
            dns_submission_dir,
            ids_dir: workflow_dir.join("participant-ids"),
            keys_dir: workflow_dir.join("participant-keys"),
            _p: PhantomData,
        }
    }

    /// Create required directories that don't exist
    pub async fn create_dirs(&self) -> Result {
        tokio::fs::create_dir_all(&self.workflow_dir).await?;
        tokio::fs::create_dir_all(&self.ids_dir).await?;
        tokio::fs::create_dir_all(&self.keys_dir).await?;
        Ok(())
    }
}

impl Default for ContractsDirs {
    fn default() -> Self {
        Self::new()
    }
}
