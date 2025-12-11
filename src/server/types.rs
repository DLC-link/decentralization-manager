use std::collections::HashMap;

use canton_proto_rs::com::digitalasset::canton::protocol::v30::enums::ParticipantPermission;
use serde::{Deserialize, Serialize};

use crate::participant_id::CantonId;

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
}

/// Status of a kick workflow
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KickStatus {
    Idle,
    InProgress,
    Completed,
    Failed,
}

/// Response for kick workflow initiation
#[derive(Serialize)]
pub struct KickResponse {
    pub status: KickStatus,
    pub message: String,
}

/// Response for key status check
#[derive(Serialize)]
pub struct KeyStatusResponse {
    pub has_keys: bool,
    pub public_key: Option<String>,
}

/// Response for key generation
#[derive(Serialize)]
pub struct KeygenResponse {
    pub success: bool,
    pub public_key: String,
    pub message: String,
}

/// Status of an onboarding workflow
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnboardingStatus {
    Idle,
    InProgress,
    Completed,
    Failed,
}

/// Response for onboarding workflow initiation
#[derive(Serialize)]
pub struct OnboardingResponse {
    pub status: OnboardingStatus,
    pub message: String,
}
