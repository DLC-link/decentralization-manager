use std::path::PathBuf;

use crate::error::Result;

/// Onboarding workflow directory structure
#[derive(Clone, Debug)]
pub struct OnboardingDirs {
    pub workflow_dir: PathBuf,
    pub keys_dir: PathBuf,
    pub ids_dir: PathBuf,
    pub dns_proposals_dir: PathBuf,
    pub dns_submission_dir: PathBuf,
    pub dns_signed_dir: PathBuf,
    pub p2p_proposals_dir: PathBuf,
    pub final_proposals_dir: PathBuf,
    pub final_signed_dir: PathBuf,
}

impl OnboardingDirs {
    /// Create new OnboardingDirs with default paths
    pub fn new() -> Self {
        let workflow_dir = PathBuf::from("./workflow-data");
        let dns_submission_dir = workflow_dir.join("dns-submission");
        let final_proposals_dir = workflow_dir.join("final-proposals-submission");

        Self {
            workflow_dir: workflow_dir.clone(),
            keys_dir: workflow_dir.join("participant-keys"),
            ids_dir: workflow_dir.join("participant-ids"),
            dns_proposals_dir: workflow_dir.join("dns-proposals"),
            dns_submission_dir: dns_submission_dir.clone(),
            dns_signed_dir: dns_submission_dir.join("signed-proposals"),
            p2p_proposals_dir: workflow_dir.join("p2p-proposals"),
            final_proposals_dir: final_proposals_dir.clone(),
            final_signed_dir: final_proposals_dir.join("signed-proposals"),
        }
    }

    /// Create required directories that don't exist
    pub async fn create_dirs(&self) -> Result {
        tokio::fs::create_dir_all(&self.workflow_dir).await?;
        tokio::fs::create_dir_all(&self.keys_dir).await?;
        tokio::fs::create_dir_all(&self.ids_dir).await?;
        Ok(())
    }
}

impl Default for OnboardingDirs {
    fn default() -> Self {
        Self::new()
    }
}
