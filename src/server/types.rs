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

/// Connection status for a participant
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum ConnectionStatus {
    /// Current node (always reachable)
    CurrentNode,
    /// Successfully connected via Noise protocol
    Connected,
    /// Failed to establish TCP connection (peer not reachable)
    Unreachable,
    /// Noise handshake/decryption failed (likely wrong public key configured)
    HandshakeFailed,
}

/// Status of a single participant
#[derive(Clone, Debug, Serialize)]
pub struct ParticipantStatus {
    pub id: String,
    pub status: ConnectionStatus,
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
    /// List of peer IDs to invite to the decentralized party
    pub peer_ids: Vec<String>,
}

/// Request to deploy contracts for a decentralized party
#[derive(Clone, Debug, Deserialize)]
pub struct ContractsRequest {
    /// Decentralized party ID to deploy contracts for
    pub decentralized_party_id: CantonId,
    /// List of participant IDs that will sign submissions
    pub participant_ids: Vec<CantonId>,
    /// List of party IDs for each participant (must match participant_ids order)
    pub participant_parties: Vec<CantonId>,
    /// Operator party ID
    pub operator_party: CantonId,
    /// DAR files to upload (base64-encoded)
    #[serde(default)]
    pub dar_files: Vec<DarFile>,
    /// Contract definitions to create after decentralized party setup
    #[serde(default)]
    pub contracts: Vec<ContractDefinition>,
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

/// Type of workflow invitation
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum InvitationType {
    Onboarding,
    Kick,
    Contracts,
}

/// A pending invitation from a coordinator
#[derive(Clone, Debug, Serialize)]
pub struct PendingInvitation {
    pub id: String,
    pub invitation_type: InvitationType,
    pub coordinator_pubkey: String,
    pub coordinator_name: Option<String>,
    pub received_at: i64,
}

/// Response for pending invitations endpoint
#[derive(Serialize)]
pub struct PendingInvitationsResponse {
    pub invitations: Vec<PendingInvitation>,
}

/// Request to accept or decline an invitation
#[derive(Deserialize)]
pub struct InvitationActionRequest {
    pub id: String,
}

/// Authentication status for a party
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum AuthStatus {
    Authenticated,
    Mock,
    Failed { error: String },
    NotConfigured,
}

/// User rights validation result
#[derive(Clone, Debug, Serialize)]
pub struct RightsStatus {
    /// Whether user can actAs the member party
    pub member_party_act_as: bool,
    /// Whether user can readAs the member party
    pub member_party_read_as: bool,
    /// Whether user can actAs the decentralized party
    pub dec_party_act_as: bool,
    /// Whether user can readAs the decentralized party
    pub dec_party_read_as: bool,
}

impl RightsStatus {
    /// Check if all required rights are present
    pub fn is_valid(&self) -> bool {
        self.member_party_act_as
            && self.member_party_read_as
            && self.dec_party_act_as
            && self.dec_party_read_as
    }
}

/// Authentication status for a single party
#[derive(Clone, Debug, Serialize)]
pub struct PartyAuthStatus {
    pub dec_party_id: String,
    pub member_party_id: String,
    pub user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keycloak_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keycloak_realm: Option<String>,
    pub status: AuthStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rights: Option<RightsStatus>,
}

/// Response for the auth status endpoint
#[derive(Serialize)]
pub struct AuthStatusResponse {
    pub parties: Vec<PartyAuthStatus>,
}

/// Result of an authentication test
#[derive(Clone, Debug, Serialize)]
pub struct AuthTestResult {
    pub party_id: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response for the auth test endpoint
#[derive(Serialize)]
pub struct AuthTestResponse {
    pub results: Vec<AuthTestResult>,
}

// ============================================================================
// Governance Types (Structured Actions)
// ============================================================================

/// Instrument identifier (admin + id)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstrumentId {
    pub admin: String,
    pub id: String,
}

/// Vault limits configuration (all fields are optional in DAML)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VaultLimits {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_total_deposit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_deposit_amount: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_withdrawal_amount: Option<String>,
}

/// Featured App Right beneficiary
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppRewardBeneficiary {
    pub beneficiary: CantonId,
    pub weight: String,
}

/// Featured App Right configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FarConfig {
    pub featured_app_right_cid: String,
    pub beneficiaries: Vec<AppRewardBeneficiary>,
}

/// Structured action types for Vault governance
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionType {
    // Governance (4)
    GovernanceAddMember {
        member: CantonId,
        new_threshold: i64,
    },
    GovernanceRemoveMember {
        member: CantonId,
        new_threshold: i64,
    },
    GovernanceSetThreshold {
        new_threshold: i64,
    },
    GovernanceSetTimeout {
        new_timeout_microseconds: i64,
    },

    // Vault Deployment (2)
    VaultDeployment {
        vault_rules_cid: String,
        vault_name: String,
        share_symbol: String,
        asset_instrument_id: InstrumentId,
        limits: VaultLimits,
        vault_backend_signatory: CantonId,
        #[serde(default)]
        vault_far_config: Option<FarConfig>,
        allocation_factory_cid: String,
        registrar_service_cid: String,
    },
    YieldEpochDeployment {
        vault_rules_cid: String,
        vault_cid: String,
        asset_instrument_id: InstrumentId,
        vault_backend_signatory: CantonId,
    },

    // Vault Operations (5)
    VaultPause {
        vault_id: String,
    },
    VaultUnpause {
        vault_id: String,
    },
    VaultUpdateLimits {
        vault_id: String,
        new_limits: VaultLimits,
    },
    VaultUpdateBackend {
        vault_id: String,
        new_backend_signatory: CantonId,
    },
    VaultUpdateFarBeneficiaries {
        vault_id: String,
        new_beneficiaries: Vec<AppRewardBeneficiary>,
    },

    // Processor (1)
    ProcessorDeploymentRequest {
        vault_processor_rules_cid: String,
        vault_backend_signatory: CantonId,
        allocation_factory_cid: String,
        #[serde(default)]
        processor_far_config: Option<FarConfig>,
        initial_supported_vaults: Vec<String>,
    },

    // Utility Onboarding (4)
    UtilityCreateProviderRequest {
        operator: CantonId,
    },
    UtilityCreateUserRequest {
        operator: CantonId,
    },
    UtilitySetup {
        operator: CantonId,
        provider_service_cid: String,
        user_service_cid: String,
    },
    UtilityAcceptHolderServiceRequest {
        operator: CantonId,
        provider_service_cid: String,
        holder_service_request_cid: String,
        holder: CantonId,
    },
    // Credential Actions (2)
    CredentialOfferFree {
        operator: CantonId,
        user_service_cid: String,
        holder: CantonId,
        id: String,
        description: String,
        claims: Vec<Claim>,
    },
    CredentialAcceptFree {
        operator: CantonId,
        user_service_cid: String,
        credential_offer_cid: String,
    },

    // DevNet (1)
    DevNetFeatureApp {
        amulet_rules_cid: String,
    },
}

/// Credential claim (subject, property, value)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Claim {
    pub subject: String,
    pub property: String,
    pub value: String,
}

/// Request to submit a confirmation for an action with structured type
#[derive(Clone, Debug, Deserialize)]
pub struct ConfirmActionRequest {
    pub party_id: CantonId,
    pub rules_contract_id: String,
    pub action: ActionType,
}

/// A disclosed contract to include in the ledger submission
#[derive(Clone, Debug, Deserialize)]
pub struct DisclosedContractInput {
    pub contract_id: String,
    pub blob: String, // base64-encoded created_event_blob
}

/// Request to execute a confirmed action with structured type
#[derive(Clone, Debug, Deserialize)]
pub struct ExecuteActionRequest {
    pub party_id: CantonId,
    pub rules_contract_id: String,
    pub action: ActionType,
    pub confirmation_cids: Vec<String>,
    #[serde(default)]
    pub disclosed_contracts: Vec<DisclosedContractInput>,
}

/// A single governance confirmation with parsed action
#[derive(Clone, Debug, Serialize)]
pub struct GovernanceConfirmation {
    pub contract_id: String,
    pub action: ActionType,
    pub confirming_party: String,
}

/// A governance action with its confirmations, grouped by action hash
#[derive(Clone, Debug, Serialize)]
pub struct GovernanceAction {
    /// Deterministic hash of the serialized action for grouping
    pub action_hash: String,
    /// The parsed action type
    pub action: ActionType,
    /// List of confirmations for this action
    pub confirmations: Vec<GovernanceConfirmation>,
    /// Number of confirmations
    pub confirmation_count: usize,
    /// Whether threshold is met for execution
    pub can_execute: bool,
}

/// Response for governance confirmations endpoint
#[derive(Serialize)]
pub struct GovernanceResponse {
    pub actions: Vec<GovernanceAction>,
    pub threshold: usize,
    /// The member party ID for the requesting party (used to identify own confirmations)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_party_id: Option<String>,
}

/// Request to expire a stale confirmation
#[derive(Clone, Debug, Deserialize)]
pub struct ExpireConfirmationRequest {
    pub party_id: CantonId,
    pub rules_contract_id: String,
    pub confirmation_cid: String,
}

/// State of a VaultGovernanceRules contract
#[derive(Clone, Debug, Serialize)]
pub struct GovernanceState {
    pub contract_id: String,
    pub vault_manager: CantonId,
    pub members: Vec<CantonId>,
    pub threshold: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_confirmation_timeout_microseconds: Option<i64>,
}

/// Response for the governance state endpoint
#[derive(Serialize)]
pub struct GovernanceStateResponse {
    pub state: Option<GovernanceState>,
}

/// Information about a deployed Vault contract
#[derive(Clone, Debug, Serialize)]
pub struct VaultInfo {
    pub contract_id: String,
    pub vault_name: String,
    pub share_symbol: String,
    pub is_paused: bool,
    pub vault_manager: CantonId,
}

/// Response for the vaults endpoint
#[derive(Serialize)]
pub struct VaultsResponse {
    pub vaults: Vec<VaultInfo>,
}

/// Information about a ProviderService contract
#[derive(Clone, Debug, Serialize)]
pub struct ProviderServiceInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub provider: CantonId,
}

/// Response for the provider services endpoint
#[derive(Serialize)]
pub struct ProviderServicesResponse {
    pub services: Vec<ProviderServiceInfo>,
}

/// Information about a UserService contract
#[derive(Clone, Debug, Serialize)]
pub struct UserServiceInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub user: CantonId,
}

/// Response for the user services endpoint
#[derive(Serialize)]
pub struct UserServicesResponse {
    pub services: Vec<UserServiceInfo>,
}
