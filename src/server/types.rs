use std::{collections::HashMap, sync::Arc, time::Duration};

use canton_proto_rs::com::digitalasset::canton::protocol::v30::enums::ParticipantPermission;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, RwLock};

use crate::{
    config::PackageConfig,
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
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
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
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct ParticipantInfo {
    pub participant_uid: CantonId,
    pub permission: Permission,
    /// Namespace key fingerprint for this participant (if they are an owner)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_key: Option<String>,
}

/// Contract information
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct ContractInfo {
    pub contract_id: String,
    pub template_id: String,
    pub package_id: String,
}

/// Vetted package information
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct VettedPackageInfo {
    pub package_id: String,
    pub package_name: String,
    pub package_version: String,
}

/// Package info for peer comparison
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct PackageInfo {
    pub package_id: String,
    pub name: String,
    pub version: String,
}

/// Result of querying packages from a single peer
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct PeerPackageResult {
    pub participant_id: String,
    pub name: String,
    pub reachable: bool,
    #[serde(default)]
    pub packages: Vec<PackageInfo>,
}

/// Response from the peer DAR comparison endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct PeerPackageComparison {
    pub local_packages: Vec<PackageInfo>,
    pub peers: Vec<PeerPackageResult>,
}

/// Party metadata from Ledger API
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct PartyMetadata {
    pub annotations: HashMap<String, String>,
}

/// Decentralized party information
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct DecentralizedParty {
    pub party_id: CantonId,
    pub threshold: i32,
    pub owners: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_owner_key: Option<String>,
    pub participants: Vec<ParticipantInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contracts: Vec<ContractInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_metadata: Option<PartyMetadata>,
}

/// Where the response data came from
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ResponseSource {
    /// Fresh data from Canton gRPC
    #[default]
    Live,
    /// Cached data from local database
    Cache,
}

/// Response for the decentralized parties endpoint
#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct DecentralizedPartiesResponse {
    pub parties: Vec<DecentralizedParty>,
    #[serde(default)]
    pub source: ResponseSource,
    /// Whether a background refresh from Canton is currently in progress
    #[serde(default)]
    pub refreshing: bool,
}

/// Connection status for a participant
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, utoipa::ToSchema)]
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
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct ParticipantStatus {
    pub id: String,
    pub status: ConnectionStatus,
}

/// Response for the participants status endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct ParticipantsStatusResponse {
    pub statuses: Vec<ParticipantStatus>,
}

/// Request to kick a participant from a decentralized party
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct KickRequest {
    pub decentralized_party_id: String,
    pub participant_id: String,
    pub namespace_fingerprint: String,
    pub new_threshold: i32,
}

/// Request to create a new decentralized party
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct OnboardingRequest {
    /// Party ID prefix for the decentralized party (e.g., "xyz-network")
    pub party_id_prefix: String,
    /// List of peer IDs to invite to the decentralized party
    pub peer_ids: Vec<String>,
}

/// Request to deploy contracts for a decentralized party
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct ContractsRequest {
    /// Decentralized party ID to deploy contracts for
    pub decentralized_party_id: CantonId,
    /// List of participant IDs that will sign submissions
    pub participant_ids: Vec<CantonId>,
    /// List of party IDs for each participant (must match participant_ids order)
    pub participant_parties: Vec<CantonId>,
    /// Operator party ID
    pub operator_party: CantonId,
    /// Contract definitions to create after decentralized party setup
    #[serde(default)]
    pub contracts: Vec<ContractDefinition>,
}

/// Request to upload DARs across all participants
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct DarsRequest {
    /// DAR files to upload (base64-encoded)
    pub dar_files: Vec<DarFile>,
}

/// Progress status of a workflow (kick, onboarding, etc.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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
#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkflowResponse {
    pub status: WorkflowProgress,
    pub message: String,
}

/// Type aliases for backwards compatibility
pub type KickResponse = WorkflowResponse;
pub type OnboardingResponse = WorkflowResponse;

/// Response for key status check
#[derive(Serialize, utoipa::ToSchema)]
pub struct KeyStatusResponse {
    pub has_keys: bool,
    pub public_key: Option<String>,
}

/// Type of workflow invitation
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "PascalCase")]
pub enum InvitationType {
    Onboarding,
    Kick,
    Contracts,
    Dars,
}

/// A pending invitation from a coordinator
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct PendingInvitation {
    pub id: String,
    pub invitation_type: InvitationType,
    pub coordinator_pubkey: String,
    pub coordinator_name: Option<String>,
    pub received_at: i64,
}

/// Response for pending invitations endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct PendingInvitationsResponse {
    pub invitations: Vec<PendingInvitation>,
}

/// Request to accept or decline an invitation
#[derive(Deserialize, utoipa::ToSchema)]
pub struct InvitationActionRequest {
    pub id: String,
}

/// Authentication status for a party
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum AuthStatus {
    Authenticated,
    Mock,
    Failed { error: String },
    NotConfigured,
}

/// User rights validation result
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
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
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
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
#[derive(Serialize, utoipa::ToSchema)]
pub struct AuthStatusResponse {
    pub parties: Vec<PartyAuthStatus>,
}

/// Result of an authentication test
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct AuthTestResult {
    pub party_id: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response for the auth test endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct AuthTestResponse {
    pub results: Vec<AuthTestResult>,
}

// ============================================================================
// Governance Types (Structured Actions)
// ============================================================================

/// Instrument identifier (admin + id)
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct InstrumentId {
    pub admin: String,
    pub id: String,
}

/// Vault limits configuration (all fields are optional in DAML)
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct VaultLimits {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_total_deposit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_deposit_amount: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_withdrawal_amount: Option<String>,
}

/// Featured App Right beneficiary
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AppRewardBeneficiary {
    pub beneficiary: CantonId,
    pub weight: String,
}

/// Featured App Right configuration
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct FarConfig {
    pub featured_app_right_cid: String,
    pub beneficiaries: Vec<AppRewardBeneficiary>,
}

/// Structured action types for Vault governance
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
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

impl ActionType {
    /// Validate the action's fields. Returns an error message if invalid.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            ActionType::VaultDeployment {
                vault_far_config: Some(far),
                ..
            }
            | ActionType::ProcessorDeploymentRequest {
                processor_far_config: Some(far),
                ..
            } => validate_beneficiary_weights(&far.beneficiaries),
            ActionType::VaultUpdateFarBeneficiaries {
                new_beneficiaries, ..
            } => validate_beneficiary_weights(new_beneficiaries),
            _ => Ok(()),
        }
    }
}

fn validate_beneficiary_weights(beneficiaries: &[AppRewardBeneficiary]) -> Result<(), String> {
    if beneficiaries.is_empty() {
        return Ok(());
    }
    let sum: f64 = beneficiaries
        .iter()
        .map(|b| b.weight.parse::<f64>().unwrap_or(0.0))
        .sum();
    if (sum - 1.0).abs() > 1e-9 {
        return Err(format!(
            "FAR beneficiary weights must sum to exactly 1.0, got {sum}"
        ));
    }
    Ok(())
}

/// Credential claim (subject, property, value)
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Claim {
    pub subject: String,
    pub property: String,
    pub value: String,
}

/// Which governance system a request targets
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GovernanceType {
    /// VaultGovernanceRules (closed-enum inline actions)
    #[default]
    Vault,
    /// GovernanceRules self-management (GovernanceSelfAction)
    CoreSelf,
    /// GovernanceRules domain actions (GovernableAction proposals)
    CoreDomain,
}

/// Instrument allowance for token preapproval
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct InstrumentAllowance {
    pub id: String,
}

/// Types of governance domain action proposals
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProposalType {
    /// Set up Canton Coin TransferPreapproval
    SetupCcPreapproval {
        provider: CantonId,
        expected_dso: CantonId,
    },
    /// Set up utility token TransferPreapproval
    SetupTokenPreapproval {
        operator: CantonId,
        instrument_admin: CantonId,
        #[serde(default)]
        instrument_allowances: Vec<InstrumentAllowance>,
    },
    /// Transfer tokens via a TransferFactory
    Transfer {
        transfer_factory_cid: String,
        expected_admin: CantonId,
        receiver: CantonId,
        amount: String,
        instrument_id: InstrumentId,
        #[serde(default)]
        input_holding_cids: Vec<String>,
    },
    /// Accept an incoming token transfer
    AcceptTransfer { transfer_instruction_cid: String },
    /// Generic text-based vote (no on-chain effect beyond recording the result)
    GenericVote { description: String },
    /// Run the full Utility-Registry onboarding for an instrument issued by the
    /// governance party and produce an IssuanceConfig. One-shot per deployment.
    SetupIssuance {
        provider_service_cid: String,
        operator: CantonId,
        instrument_id_text: String,
        display_name: String,
        symbol: String,
        decimals: i64,
    },
    /// Offer a mint of `amount` tokens to `recipient` via AllocationFactory_OfferMint.
    /// The resulting MintOffer is accepted later by the recipient, outside this plugin.
    Mint {
        issuance_config_cid: String,
        recipient: CantonId,
        amount: String,
        description: String,
        /// Hours into the future after which the mint offer expires. Defaults to 24.
        #[serde(default)]
        execute_before_hours: Option<i64>,
    },
    /// Offer a burn of `amount` tokens held by `holder` via AllocationFactory_OfferBurn.
    /// Holdings are supplied by the holder at BurnOffer_Accept time, not here.
    Burn {
        issuance_config_cid: String,
        holder: CantonId,
        amount: String,
        description: String,
        /// Hours into the future after which the burn offer expires. Defaults to 24.
        #[serde(default)]
        execute_before_hours: Option<i64>,
    },
    /// Swap the AllocationFactory cid on an existing IssuanceConfig to `new_factory_cid`
    /// after validating that the new factory's admin matches the governance party.
    RotateFactory {
        issuance_config_cid: String,
        new_factory_cid: String,
    },
}

/// Request to propose a governance domain action (creates proposal contract)
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct ProposeActionRequest {
    pub party_id: CantonId,
    pub rules_contract_id: String,
    pub proposal: ProposalType,
}

/// A pending domain action proposal with its confirmations
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct DomainGovernanceAction {
    /// Contract ID of the proposal
    pub proposal_cid: String,
    /// Human-readable label (e.g., "SetupCcPreapproval")
    pub action_label: String,
    /// Human-readable description from the proposal's GovernableActionView
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Confirmations for this proposal
    pub confirmations: Vec<GovernanceConfirmation>,
    /// Number of unique confirmers
    pub confirmation_count: usize,
    /// Whether threshold is met for execution
    pub can_execute: bool,
}

/// Request to submit a confirmation for an action with structured type
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct ConfirmActionRequest {
    pub party_id: CantonId,
    pub rules_contract_id: String,
    pub action: ActionType,
    #[serde(default)]
    pub governance_type: GovernanceType,
    /// For CoreDomain: ContractId of the GovernableAction proposal
    #[serde(default)]
    pub proposal_cid: Option<String>,
}

/// A disclosed contract to include in the ledger submission
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct DisclosedContractInput {
    pub contract_id: String,
    pub blob: String, // base64-encoded created_event_blob
}

/// Request to execute a confirmed action with structured type
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct ExecuteActionRequest {
    pub party_id: CantonId,
    pub rules_contract_id: String,
    pub action: ActionType,
    pub confirmation_cids: Vec<String>,
    #[serde(default)]
    pub disclosed_contracts: Vec<DisclosedContractInput>,
    #[serde(default)]
    pub governance_type: GovernanceType,
    /// For CoreDomain: ContractId of the GovernableAction proposal
    #[serde(default)]
    pub proposal_cid: Option<String>,
}

/// A single governance confirmation with parsed action
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct GovernanceConfirmation {
    pub contract_id: String,
    pub action: ActionType,
    pub confirming_party: String,
}

/// A governance action with its confirmations, grouped by action hash
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
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
#[derive(Serialize, utoipa::ToSchema)]
pub struct GovernanceResponse {
    pub actions: Vec<GovernanceAction>,
    /// Pending domain action proposals (governance-core GovernableAction)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domain_actions: Vec<DomainGovernanceAction>,
    pub threshold: usize,
    /// The member party ID for the requesting party (used to identify own confirmations)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_party_id: Option<String>,
}

/// Request to expire a stale confirmation
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct ExpireConfirmationRequest {
    pub party_id: CantonId,
    pub rules_contract_id: String,
    pub confirmation_cid: String,
    #[serde(default)]
    pub governance_type: GovernanceType,
}

/// Request to cancel (revoke) own confirmation
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct CancelConfirmationRequest {
    pub party_id: CantonId,
    pub confirmation_cid: String,
    #[serde(default)]
    pub governance_type: GovernanceType,
}

/// State of a VaultGovernanceRules contract
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct GovernanceState {
    pub contract_id: String,
    pub vault_manager: CantonId,
    pub members: Vec<CantonId>,
    pub threshold: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_confirmation_timeout_microseconds: Option<i64>,
}

/// Response for the governance state endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct GovernanceStateResponse {
    pub state: Option<GovernanceState>,
}

/// Information about a deployed Vault contract
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct VaultInfo {
    pub contract_id: String,
    pub vault_name: String,
    pub share_symbol: String,
    pub is_paused: bool,
    pub vault_manager: CantonId,
}

/// Response for the vaults endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct VaultsResponse {
    pub vaults: Vec<VaultInfo>,
}

/// Information about a ProviderService contract
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct ProviderServiceInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub provider: CantonId,
}

/// Response for the provider services endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct ProviderServicesResponse {
    pub services: Vec<ProviderServiceInfo>,
}

/// Information about a UserService contract
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct UserServiceInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub user: CantonId,
}

/// Response for the user services endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct UserServicesResponse {
    pub services: Vec<UserServiceInfo>,
}

/// Information about a RegistrarService contract
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct RegistrarServiceInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub registrar: CantonId,
}

/// Response for the registrar services endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct RegistrarServicesResponse {
    pub services: Vec<RegistrarServiceInfo>,
}

/// A contract ID with its blob
#[derive(Serialize, utoipa::ToSchema)]
pub struct ContractWithBlob {
    pub contract_id: String,
    pub blob: String,
}

/// DSO network info (amulet rules + DSO party)
#[derive(Serialize, utoipa::ToSchema)]
pub struct NetworkInfo {
    pub dso_party_id: CantonId,
    pub amulet_rules_cid: String,
    pub amulet_rules_blob: String,
}

/// Response for the generic contract query endpoint
#[derive(Serialize, utoipa::ToSchema)]
pub struct ContractQueryResponse {
    pub contracts: Vec<ContractWithBlob>,
}

/// Request to save or update party configuration
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct PartyConfigRequest {
    /// The decentralized party ID
    pub dec_party_id: CantonId,
    /// The member party ID (local to this node)
    pub member_party_id: CantonId,
    /// Canton/Ledger API user ID
    pub user_id: String,
    /// Keycloak server URL
    pub keycloak_url: String,
    /// Keycloak realm name
    pub keycloak_realm: String,
    /// OAuth2 client ID
    pub keycloak_client_id: String,
    /// Client secret for M2M flow (None = keep existing, "" = clear)
    #[serde(default)]
    pub keycloak_client_secret: Option<String>,
    /// Username for password flow (None = keep existing, "" = clear)
    #[serde(default)]
    pub keycloak_username: Option<String>,
    /// Password for password flow (None = keep existing, "" = clear)
    #[serde(default)]
    pub keycloak_password: Option<String>,
    /// Package identifiers for deployed Daml contracts
    #[serde(default)]
    pub packages: PackageConfig,
}

/// Response with party configuration (secrets masked)
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct PartyConfigResponse {
    /// The decentralized party ID
    pub dec_party_id: CantonId,
    /// The member party ID (local to this node)
    pub member_party_id: CantonId,
    /// Canton/Ledger API user ID
    pub user_id: String,
    /// Keycloak server URL
    pub keycloak_url: String,
    /// Keycloak realm name
    pub keycloak_realm: String,
    /// OAuth2 client ID
    pub keycloak_client_id: String,
    /// Whether a client secret is configured
    pub has_client_secret: bool,
    /// Whether a username is configured
    pub has_username: bool,
    /// Whether a password is configured
    pub has_password: bool,
    /// Package identifiers for deployed Daml contracts
    pub packages: PackageConfig,
}

/// Frontend authentication configuration response
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct AuthConfigResponse {
    /// Whether Keycloak auth is required (false in test mode)
    pub auth_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keycloak_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keycloak_realm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keycloak_client_id: Option<String>,
}

/// Generic error response
#[derive(Serialize, utoipa::ToSchema)]
pub struct ErrorResponse {
    pub error: String,
}

/// Generic success response
#[derive(Serialize, utoipa::ToSchema)]
pub struct MessageResponse {
    pub message: String,
}

/// Generic success boolean response
#[derive(Serialize, utoipa::ToSchema)]
pub struct SuccessResponse {
    pub success: bool,
}

/// Response for workflow status check endpoints
#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkflowStatusResponse {
    pub status: WorkflowProgress,
    pub error: Option<String>,
}
