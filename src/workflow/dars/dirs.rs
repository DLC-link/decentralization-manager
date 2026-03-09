use std::path::PathBuf;

use crate::{error::Result, utils};

/// DARs workflow directory structure
#[derive(Clone, Debug)]
pub struct DarsDirs {
    pub workflow_dir: PathBuf,
}

impl DarsDirs {
    /// Create new DarsDirs with base directory and instance name
    pub fn with_base(base_workflow_dir: PathBuf, instance_name: &str) -> Self {
        Self {
            workflow_dir: base_workflow_dir.join(instance_name),
        }
    }

    /// Create required directories that don't exist
    pub async fn create_dirs(&self) -> Result {
        utils::create_directories(&[&self.workflow_dir]).await
    }
}
