use std::path::PathBuf;

use crate::{
    consts::{
        CURRENT_CONFIG_DIR, KICK_PROPOSALS_DIR, KICK_SIGNED_DIR, PARTICIPANT_IDS_DIR,
        WORKFLOW_DATA_DIR,
    },
    error::Result,
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
    /// Create new KickDirs with default paths
    pub fn new() -> Self {
        Self::with_base(PathBuf::from(format!("./{WORKFLOW_DATA_DIR}")))
    }

    /// Create new KickDirs with a custom base directory
    pub fn with_base(workflow_dir: PathBuf) -> Self {
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
        use anyhow::Context;

        tokio::fs::create_dir_all(&self.workflow_dir)
            .await
            .with_context(|| format!("Failed to create dir '{}'", self.workflow_dir.display()))?;
        tokio::fs::create_dir_all(&self.kick_config_dir)
            .await
            .with_context(|| format!("Failed to create dir '{}'", self.kick_config_dir.display()))?;
        tokio::fs::create_dir_all(&self.kick_proposals_dir)
            .await
            .with_context(|| format!("Failed to create dir '{}'", self.kick_proposals_dir.display()))?;
        tokio::fs::create_dir_all(&self.kick_signed_dir)
            .await
            .with_context(|| format!("Failed to create dir '{}'", self.kick_signed_dir.display()))?;
        Ok(())
    }
}

impl Default for KickDirs {
    fn default() -> Self {
        Self::new()
    }
}
