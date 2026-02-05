use std::path::PathBuf;

use crate::{
    consts::{
        ADD_PARTY_PROPOSALS_DIR, ADD_PARTY_SIGNED_DIR, CURRENT_CONFIG_DIR, PARTICIPANT_IDS_DIR,
        PARTICIPANT_KEYS_DIR,
    },
    error::Result,
    utils,
};

/// Add party workflow directory structure
#[derive(Clone, Debug)]
pub struct AddPartyDirs {
    pub workflow_dir: PathBuf,
    pub current_config_dir: PathBuf,
    pub add_party_proposals_dir: PathBuf,
    pub add_party_signed_dir: PathBuf,
    pub keys_dir: PathBuf,
    pub ids_dir: PathBuf,
}

impl AddPartyDirs {
    /// Create new AddPartyDirs with a base directory and instance name
    pub fn with_base(base_workflow_dir: PathBuf, instance_name: &str) -> Self {
        let workflow_dir = base_workflow_dir.join(instance_name);
        Self {
            workflow_dir: workflow_dir.clone(),
            current_config_dir: workflow_dir.join(CURRENT_CONFIG_DIR),
            add_party_proposals_dir: workflow_dir.join(ADD_PARTY_PROPOSALS_DIR),
            add_party_signed_dir: workflow_dir.join(ADD_PARTY_SIGNED_DIR),
            keys_dir: workflow_dir.join(PARTICIPANT_KEYS_DIR),
            ids_dir: workflow_dir.join(PARTICIPANT_IDS_DIR),
        }
    }

    /// Create required directories that don't exist
    pub async fn create_dirs(&self) -> Result {
        utils::create_directories(&[
            &self.workflow_dir,
            &self.current_config_dir,
            &self.add_party_proposals_dir,
            &self.add_party_signed_dir,
            &self.keys_dir,
            &self.ids_dir,
        ])
        .await
    }
}
