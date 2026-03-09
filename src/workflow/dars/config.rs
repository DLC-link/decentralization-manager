use serde::{Deserialize, Serialize};

use crate::workflow::contracts::DarFile;

/// Configuration for DARs upload workflow
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DarsConfig {
    /// DAR files to upload (base64-encoded)
    pub dar_files: Vec<DarFile>,
    /// Workflow instance name for directory organization
    pub instance_name: String,
}
