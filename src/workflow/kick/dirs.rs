use std::path::PathBuf;

use crate::error::Result;

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
        let workflow_dir = PathBuf::from("./workflow-data");

        Self {
            workflow_dir: workflow_dir.clone(),
            kick_config_dir: workflow_dir.join("current-config"),
            kick_proposals_dir: workflow_dir.join("kick-proposals"),
            kick_signed_dir: workflow_dir.join("kick-signed"),
            ids_dir: workflow_dir.join("participant-ids"),
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
