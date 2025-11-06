use std::path::PathBuf;

use crate::error::Result;

/// Holds all workflow directory paths
#[derive(Debug, Clone)]
pub struct WorkflowDirs {
    pub dars_dir: PathBuf,
    pub workflow_dir: PathBuf,
    pub keys_dir: PathBuf,
    pub ids_dir: PathBuf,
    pub dns_proposals_dir: PathBuf,
    pub dns_submission_dir: PathBuf,
    pub dns_signed_dir: PathBuf,
    pub p2p_ptk_proposals_dir: PathBuf,
    pub final_proposals_dir: PathBuf,
    pub final_signed_dir: PathBuf,
}

impl WorkflowDirs {
    /// Create new WorkflowDirs with default paths
    pub fn new() -> Self {
        let workflow_dir = PathBuf::from("./workflow-data");
        let dns_submission_dir = workflow_dir.join("dns-submission");
        let final_proposals_dir = workflow_dir.join("final-proposals-submission");

        Self {
            dars_dir: PathBuf::from("./dars"),
            workflow_dir: workflow_dir.clone(),
            keys_dir: workflow_dir.join("participant-keys"),
            ids_dir: workflow_dir.join("participant-ids"),
            dns_proposals_dir: workflow_dir.join("dns-proposals"),
            dns_submission_dir: dns_submission_dir.clone(),
            dns_signed_dir: dns_submission_dir.join("signed-proposals"),
            p2p_ptk_proposals_dir: workflow_dir.join("p2p-ptk-proposals"),
            final_proposals_dir: final_proposals_dir.clone(),
            final_signed_dir: final_proposals_dir.join("signed-proposals"),
        }
    }

    /// Create required directories that don't exist
    pub async fn create_required_dirs(&self) -> Result {
        if !self.workflow_dir.exists() {
            tokio::fs::create_dir_all(&self.workflow_dir).await?;
        }
        if !self.keys_dir.exists() {
            tokio::fs::create_dir_all(&self.keys_dir).await?;
        }
        if !self.ids_dir.exists() {
            tokio::fs::create_dir_all(&self.ids_dir).await?;
        }
        Ok(())
    }
}

impl Default for WorkflowDirs {
    fn default() -> Self {
        Self::new()
    }
}
