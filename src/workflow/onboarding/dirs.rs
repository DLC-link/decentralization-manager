use std::path::PathBuf;

use crate::{
    consts::{
        DNS_PROPOSALS_DIR, DNS_SUBMISSION_DIR, FINAL_PROPOSALS_SUBMISSION_DIR, PARTICIPANT_IDS_DIR,
        PARTICIPANT_KEYS_DIR, P2P_PROPOSALS_DIR, SIGNED_PROPOSALS_DIR, WORKFLOW_DATA_DIR,
    },
    error::Result,
    utils,
};

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
        Self::with_base(PathBuf::from(format!("./{WORKFLOW_DATA_DIR}")))
    }

    /// Create new OnboardingDirs with a custom base directory
    pub fn with_base(workflow_dir: PathBuf) -> Self {
        let dns_submission_dir = workflow_dir.join(DNS_SUBMISSION_DIR);
        let final_proposals_dir = workflow_dir.join(FINAL_PROPOSALS_SUBMISSION_DIR);

        Self {
            workflow_dir: workflow_dir.clone(),
            keys_dir: workflow_dir.join(PARTICIPANT_KEYS_DIR),
            ids_dir: workflow_dir.join(PARTICIPANT_IDS_DIR),
            dns_proposals_dir: workflow_dir.join(DNS_PROPOSALS_DIR),
            dns_submission_dir: dns_submission_dir.clone(),
            dns_signed_dir: dns_submission_dir.join(SIGNED_PROPOSALS_DIR),
            p2p_proposals_dir: workflow_dir.join(P2P_PROPOSALS_DIR),
            final_proposals_dir: final_proposals_dir.clone(),
            final_signed_dir: final_proposals_dir.join(SIGNED_PROPOSALS_DIR),
        }
    }

    /// Create required directories that don't exist
    pub async fn create_dirs(&self) -> Result {
        utils::create_directories(&[
            &self.workflow_dir,
            &self.keys_dir,
            &self.ids_dir,
            &self.dns_signed_dir,
            &self.final_signed_dir,
        ])
        .await
    }
}

impl Default for OnboardingDirs {
    fn default() -> Self {
        Self::new()
    }
}
