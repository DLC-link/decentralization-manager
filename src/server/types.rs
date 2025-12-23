use std::{collections::HashMap, sync::Arc, time::Duration};

use canton_proto_rs::com::digitalasset::canton::protocol::v30::enums::ParticipantPermission;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, RwLock};

use crate::{
    participant_id::CantonId,
    workflow::contracts::{ContractDefinition, DarFile},
};

use super::ListenerControl;

/// Trait for workflow status types that can be used with HttpWorkflowState
pub trait WorkflowStatus: Default + Copy + Send + Sync {}

/// Generic state for tracking HTTP-triggered workflows
pub struct HttpWorkflowState<S: WorkflowStatus> {
    pub status: RwLock<S>,
    pub error: RwLock<Option<String>>,
}

impl<S: WorkflowStatus> Default for HttpWorkflowState<S> {
    fn default() -> Self {
        Self {
            status: RwLock::new(S::default()),
            error: RwLock::new(None),
        }
    }
}

impl<S: WorkflowStatus> HttpWorkflowState<S> {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Guard that pauses the Noise listener while held and resumes it when dropped
pub struct ListenerPauseGuard {
    listener_control: Arc<RwLock<ListenerControl>>,
    listener_notify: Arc<Notify>,
}

impl ListenerPauseGuard {
    /// Pause the listener and return a guard that will resume it when dropped
    pub async fn pause(
        listener_control: Arc<RwLock<ListenerControl>>,
        listener_notify: Arc<Notify>,
    ) -> Self {
        {
            let mut control = listener_control.write().await;
            control.should_pause = true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        Self {
            listener_control,
            listener_notify,
        }
    }

    /// Resume the listener explicitly (also called automatically on drop)
    pub async fn resume(self) {
        self.resume_inner().await;
    }

    async fn resume_inner(&self) {
        {
            let mut control = self.listener_control.write().await;
            control.should_pause = false;
        }
        self.listener_notify.notify_one();
    }
}

/// Participant permission level
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    Submission,
    Confirmation,
    Observation,
    Unknown,
}

impl From<i32> for Permission {
    fn from(value: i32) -> Self {
        match value {
            x if x == ParticipantPermission::Submission as i32 => Permission::Submission,
            x if x == ParticipantPermission::Confirmation as i32 => Permission::Confirmation,
            x if x == ParticipantPermission::Observation as i32 => Permission::Observation,
            _ => Permission::Unknown,
        }
    }
}

/// Participant in a decentralized party
#[derive(Clone, Debug, Serialize)]
pub struct ParticipantInfo {
    pub participant_uid: CantonId,
    pub permission: Permission,
}

/// Contract information
#[derive(Clone, Debug, Serialize)]
pub struct ContractInfo {
    pub contract_id: String,
    pub template_id: String,
    pub package_id: String,
}

/// Party metadata from Ledger API
#[derive(Clone, Debug, Serialize)]
pub struct PartyMetadata {
    pub annotations: HashMap<String, String>,
}

/// Decentralized party information
#[derive(Clone, Debug, Serialize)]
pub struct DecentralizedParty {
    pub party_id: CantonId,
    pub threshold: i32,
    pub owners: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub my_owner_key: Option<String>,
    pub participants: Vec<ParticipantInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub contracts: Vec<ContractInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_metadata: Option<PartyMetadata>,
}

/// Response for the decentralized parties endpoint
#[derive(Serialize)]
pub struct DecentralizedPartiesResponse {
    pub parties: Vec<DecentralizedParty>,
}

/// Status of a single participant
#[derive(Clone, Debug, Serialize)]
pub struct ParticipantStatus {
    pub id: String,
    pub active: bool,
}

/// Response for the participants status endpoint
#[derive(Serialize)]
pub struct ParticipantsStatusResponse {
    pub statuses: Vec<ParticipantStatus>,
}

/// Request to kick a participant from a decentralized party
#[derive(Clone, Debug, Deserialize)]
pub struct KickRequest {
    pub decentralized_party_id: String,
    pub participant_id: String,
    pub namespace_fingerprint: String,
    pub new_threshold: i32,
}

/// Request to create a new decentralized party
#[derive(Clone, Debug, Deserialize)]
pub struct OnboardingRequest {
    /// Party ID prefix for the decentralized party (e.g., "xyz-network")
    pub party_id_prefix: String,
}

/// Request to deploy contracts for a decentralized party
#[derive(Clone, Debug, Deserialize)]
pub struct ContractsRequest {
    /// Decentralized party ID to deploy contracts for
    pub decentralized_party_id: CantonId,
    /// Operator party ID (optional, can be allocated dynamically if not provided)
    #[serde(default)]
    pub operator_party: Option<String>,
    /// Party hint for operator party allocation (used if operator_party not set)
    #[serde(default = "default_operator_party_hint")]
    pub operator_party_hint: String,
    /// DAR files to upload (base64-encoded)
    #[serde(default)]
    pub dar_files: Vec<DarFile>,
    /// Contract definitions to create after decentralized party setup
    #[serde(default)]
    pub contracts: Vec<ContractDefinition>,
}

fn default_operator_party_hint() -> String {
    "operator".to_string()
}

/// Progress status of a workflow (kick, onboarding, etc.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowProgress {
    #[default]
    Idle,
    InProgress,
    Completed,
    Failed,
}

impl WorkflowStatus for WorkflowProgress {}

/// Type aliases for backwards compatibility
pub type KickStatus = WorkflowProgress;
pub type OnboardingStatus = WorkflowProgress;

/// Response for workflow initiation (kick, onboarding, etc.)
#[derive(Serialize)]
pub struct WorkflowResponse {
    pub status: WorkflowProgress,
    pub message: String,
}

/// Type aliases for backwards compatibility
pub type KickResponse = WorkflowResponse;
pub type OnboardingResponse = WorkflowResponse;

/// Response for key status check
#[derive(Serialize)]
pub struct KeyStatusResponse {
    pub has_keys: bool,
    pub public_key: Option<String>,
}
