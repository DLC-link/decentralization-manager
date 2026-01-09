use std::path::PathBuf;

use crate::{
    consts::{CURRENT_CONFIG_DIR, KICK_PROPOSALS_DIR, KICK_SIGNED_DIR, PARTICIPANT_IDS_DIR},
    error::Result,
    utils,
};

/// Kick workflow directory structure
#[derive(Clone, Debug)]
pub struct KickDirs {
    pub workflow_dir: PathBuf,
    pub kick_config_dir: PathBuf,
    pub kick_proposals_dir: PathBuf,
    pub kick_signed_dir: PathBuf,
    pub ids_dir: PathBuf,
}

impl KickDirs {
    /// Create new KickDirs with a base directory and instance name
    pub fn with_base(base_workflow_dir: PathBuf, instance_name: &str) -> Self {
        let workflow_dir = base_workflow_dir.join(instance_name);
        Self {
            workflow_dir: workflow_dir.clone(),
            kick_config_dir: workflow_dir.join(CURRENT_CONFIG_DIR),
            kick_proposals_dir: workflow_dir.join(KICK_PROPOSALS_DIR),
            kick_signed_dir: workflow_dir.join(KICK_SIGNED_DIR),
            ids_dir: workflow_dir.join(PARTICIPANT_IDS_DIR),
        }
    }

    /// Create required directories that don't exist
    pub async fn create_dirs(&self) -> Result {
        utils::create_directories(&[
            &self.workflow_dir,
            &self.kick_config_dir,
            &self.kick_proposals_dir,
            &self.kick_signed_dir,
        ])
        .await
    }
}
