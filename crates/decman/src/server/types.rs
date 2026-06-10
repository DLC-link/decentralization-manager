use std::{
    collections::HashMap,
    sync::{Arc, RwLock as StdRwLock},
};

use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::digitalasset::canton::protocol::v30::enums::ParticipantPermission;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

// Wire DTOs that the `decman-cli` client and the frontend also need live in the
// shared `common` crate. They are re-exported here so existing
// `crate::server::types::X` (and the glob `pub use types::*` in `server/mod.rs`)
// keep resolving unchanged. `common::api` holds the HTTP request/response DTOs
// the frontend's TypeScript is generated from (see `decman/build.rs`).
pub use common::api::{
    AuditLogResponse, AuthStatus, AuthStatusResponse, AuthTestResponse, AuthTestResult,
    CancelConfirmationRequest, ChainAuditEntry, ChainAuditResponse, Claim, ContractQueryResponse,
    ContractWithBlob, ContractsInvitePayload, ContractsRequest, CredentialOfferInfo,
    CredentialOffersResponse, DarsInvitePayload, DarsRequest, DecentralizedPartiesResponse,
    DeclineInvitationPayload, DisclosedContractInput, DiscoverMemberPartyRequest,
    DiscoverMemberPartyResponse, ErrorResponse, ExpireConfirmationRequest, GovernanceState,
    GovernanceStateResponse, GovernanceType, GrantRightsRequest, GrantRightsResponse,
    InstrumentAllowance, InstrumentId, InstrumentIdentifier, InstrumentInfo, InstrumentsResponse,
    InvitationActionRequest, KeyStatusResponse, KickInvitePayload, KickRequest, KnownMember,
    KnownMembersResponse, MessageResponse, MissingEdgeKind, MissingPeerEdge, NetworkInfo,
    OnboardingInvitePayload, OnboardingMeshErrorResponse, OnboardingRequest, OperatorInfo,
    PartyAuthStatus, PartyConfigRequest, PartyConfigResponse, PendingInvitationsResponse,
    ProviderServiceInfo, ProviderServicesResponse, RegistrarServiceInfo, RegistrarServicesResponse,
    ResponseSource, RightsStatus, SuccessResponse, TransferFactoriesResponse, TransferFactoryInfo,
    TransferPreapprovalsResponse, UserServiceInfo, UserServicesResponse, VaultInfo, VaultsResponse,
    WorkflowResponse, WorkflowRunsResponse, WorkflowStatusResponse,
};
pub use common::types::{
    AuditLogEntry, AuthConfigResponse, ConnectionStatus, ContractInfo, DecentralizedParty,
    InvitationType, PackageInfo, ParticipantInfo, ParticipantStatus, ParticipantsStatusResponse,
    PartyMetadata, PeerErrorKind, PeerPackageComparison, PeerPackageResult, PendingInvitation,
    Permission, VettedPackageInfo, WorkflowInfo, WorkflowKind, WorkflowProgress, WorkflowRole,
    WorkflowRun,
};

use crate::{canton_id::CantonId, noise::server::ActiveWorkflow};

/// Liveness response for the `/healthz` ping endpoint. The body is
/// intentionally tiny: the frontend uses it to time its own round-trip to
/// this node, so the handler does no work beyond returning this. Named
/// `Liveness*` to avoid clashing with [`super::health::HealthResponse`], the
/// Noise health-probe payload.
///
/// Not generated into the frontend types: the frontend pings `/healthz` only to
/// time the round-trip and never reads the body — the latency is measured
/// client-side (`pingLatency`), not carried by this response.
#[derive(Serialize, utoipa::ToSchema)]
pub struct LivenessResponse {
    pub status: String,
}

/// Map a Canton proto `ParticipantPermission` discriminant to the wire
/// [`Permission`] DTO.
///
/// This conversion lives in the backend (not in `common` alongside the enum)
/// because it depends on the proto-generated `ParticipantPermission`, which is
/// a server-only dependency; the `Permission` enum itself is shared with the
/// `decman-cli` client and so must stay free of proto deps. Replaces the former
/// `impl From<i32> for Permission`, which the orphan rule no longer permits now
/// that `Permission` is a foreign type.
pub fn permission_from_proto(value: i32) -> Permission {
    match value {
        x if x == ParticipantPermission::Submission as i32 => Permission::Submission,
        x if x == ParticipantPermission::Confirmation as i32 => Permission::Confirmation,
        x if x == ParticipantPermission::Observation as i32 => Permission::Observation,
        _ => Permission::Unknown,
    }
}

/// Trait for workflow status types that can be used with HttpWorkflowState
pub trait WorkflowStatus: Default + Copy + Send + Sync {}

/// Generic state for tracking HTTP-triggered workflows. Holds enough context
/// for the matching `/cancel` endpoint to abort the spawn and notify the
/// peers that received an invite.
pub struct HttpWorkflowState<S: WorkflowStatus> {
    pub status: RwLock<S>,
    pub error: RwLock<Option<String>>,
    pub abort_handle: tokio::sync::Mutex<Option<tokio::task::AbortHandle>>,
    pub invited_peers: RwLock<Vec<CantonId>>,
}

impl<S: WorkflowStatus> Default for HttpWorkflowState<S> {
    fn default() -> Self {
        Self {
            status: RwLock::new(S::default()),
            error: RwLock::new(None),
            abort_handle: tokio::sync::Mutex::new(None),
            invited_peers: RwLock::new(Vec::new()),
        }
    }
}

impl<S: WorkflowStatus> HttpWorkflowState<S> {
    pub fn new() -> Self {
        Self::default()
    }
}

/// One in-flight workflow this node owns (coordinator- or peer-side),
/// type-erased over kind so a single [`WorkflowRegistry`] + a single
/// [`PeerJob`] queue hold every concurrent run regardless of kind. Keyed in
/// the registry by `instance_name`.
pub struct WorkflowInstance {
    pub instance_name: String,
    pub kind: WorkflowKind,
    pub role: WorkflowRole,
    /// HTTP-facing state (status, error, abort handle, invitees) the
    /// `/workflows/{instance_name}/{status,cancel}` endpoints read and mutate.
    /// Uniform across kinds — every kind's status collapses to
    /// [`WorkflowProgress`].
    pub http: Arc<HttpWorkflowState<WorkflowProgress>>,
    /// Coordinator-only Noise handle the always-on listener routes a peer's
    /// workflow-command traffic to. `None` for peer-side runs (peers connect
    /// outbound as clients and are never routed to here). `std::sync::RwLock`
    /// so the listener clones the handle out without awaiting.
    pub active: StdRwLock<Option<ActiveWorkflow>>,
}

impl WorkflowInstance {
    /// Build a fresh instance with empty HTTP state and no Noise handle yet.
    pub fn new(instance_name: String, kind: WorkflowKind, role: WorkflowRole) -> Arc<Self> {
        Arc::new(Self {
            instance_name,
            kind,
            role,
            http: Arc::new(HttpWorkflowState::new()),
            active: StdRwLock::new(None),
        })
    }

    /// Register the coordinator's typed Noise server so the always-on listener
    /// can route this run's commands to it.
    pub fn set_active(&self, workflow: ActiveWorkflow) {
        *self.active.write().unwrap_or_else(|e| e.into_inner()) = Some(workflow);
    }
}

/// Instance-keyed registry of every in-flight workflow this node owns. Replaces
/// the single-tenant per-kind `HttpWorkflowState` singletons, the global
/// in-flight gate, and the single `active_workflow` routing slot: any number of
/// workflows (even of the same kind) run side-by-side, each addressed by
/// `instance_name`. The always-on Noise listener routes a peer's command via
/// [`route`](Self::route) using `Message::instance`.
///
/// Uses `std::sync::RwLock` (not tokio) so the listener holds the lock only
/// long enough to clone a handle out — never across an await. The inner
/// `HttpWorkflowState` locks are tokio and are awaited only after the `Arc`
/// has been cloned out and the registry lock released.
#[derive(Clone, Default)]
pub struct WorkflowRegistry {
    inner: Arc<StdRwLock<HashMap<String, Arc<WorkflowInstance>>>>,
}

impl WorkflowRegistry {
    /// Build an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a freshly-built instance. Returns `false` (and inserts nothing)
    /// if a run is already registered under that `instance_name` — the caller
    /// turns that into a 409.
    pub fn insert(&self, instance: Arc<WorkflowInstance>) -> bool {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if guard.contains_key(&instance.instance_name) {
            return false;
        }
        guard.insert(instance.instance_name.clone(), instance);
        true
    }

    /// Look up the instance registered under `instance_name`.
    pub fn get(&self, instance_name: &str) -> Option<Arc<WorkflowInstance>> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(instance_name)
            .cloned()
    }

    /// Remove and return the instance for `instance_name`. Called once a run
    /// reaches a terminal status so the registry doesn't accumulate stale
    /// entries (see also [`WorkflowGuard`], which does this on drop).
    pub fn remove(&self, instance_name: &str) -> Option<Arc<WorkflowInstance>> {
        self.inner
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(instance_name)
    }

    /// Clone out the coordinator Noise handle to route a workflow command to,
    /// without awaiting.
    ///
    /// When `instance_name` is non-empty (the peer learned the coordinator's run
    /// from the invite and stamps every command with it), route by **exact
    /// match only**. If that run isn't registered, or hasn't set its Noise
    /// handle yet (coordinator still spinning up), return `None` — the listener
    /// replies 503 and the peer's bounded retry waits for it. Never fall back to
    /// a *different* run for a peer that named its own: in rapid start/restart
    /// sequences a sole-active fallback would hand the command to a
    /// stale/completing workflow, which `Disconnect`s the peer and ends its run
    /// with no work done.
    ///
    /// Only an empty key — a peer that predates instance routing, or a resumed
    /// peer with no stored coordinator instance — falls back to the sole active
    /// run (if exactly one), since it has no key to match on.
    pub fn route(&self, instance_name: &str) -> Option<ActiveWorkflow> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        if !instance_name.is_empty() {
            return guard
                .get(instance_name)
                .and_then(|i| i.active.read().unwrap_or_else(|e| e.into_inner()).clone());
        }
        // Empty key: fall back to the sole active run, if exactly one.
        let mut actives = guard
            .values()
            .filter_map(|i| i.active.read().unwrap_or_else(|e| e.into_inner()).clone());
        let first = actives.next();
        match actives.next() {
            Some(_) => None,
            None => first,
        }
    }

    /// Snapshot every registered instance.
    pub fn snapshot(&self) -> Vec<Arc<WorkflowInstance>> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }
}

/// Removes a [`WorkflowInstance`] from the [`WorkflowRegistry`] on drop —
/// including on coordinator/peer task panic or abort — so a finished or
/// cancelled run can never linger and misroute later commands. Removing by the
/// guard's own `instance_name` means one finishing run never deregisters a
/// different concurrent run.
pub struct WorkflowGuard {
    registry: WorkflowRegistry,
    instance_name: String,
}

impl WorkflowGuard {
    /// Tie `instance_name`'s registry entry to the returned guard's lifetime.
    pub fn new(registry: WorkflowRegistry, instance_name: String) -> Self {
        Self {
            registry,
            instance_name,
        }
    }
}

impl Drop for WorkflowGuard {
    fn drop(&mut self) {
        self.registry.remove(&self.instance_name);
    }
}

/// A peer-side workflow job: emitted by `accept_invitation` / the
/// `RetryWorkflow` listener arm onto the single `mpsc::UnboundedSender` and
/// consumed by the peer listener, which spawns `workflow::start_peer` for it.
/// Carrying `kind`, `instance_name`, and `coordinator_pubkey` on the message
/// means concurrent accepts of any kind no longer race over a global slot.
#[derive(Clone, Debug)]
pub struct PeerJob {
    pub kind: WorkflowKind,
    /// The peer-side `workflow_runs` row primary key (local synthetic name).
    pub instance_name: String,
    /// The coordinator's own run `instance_name`, taken from the invite's
    /// `workflow_instance`. The peer tags every workflow command with it
    /// (`Message::instance`) so the coordinator's always-on listener routes the
    /// command to the right concurrent run. Empty if the invite predated
    /// instance routing — the coordinator then falls back to its sole run.
    pub coordinator_instance: String,
    pub coordinator_pubkey: String,
}

// `WorkflowProgress` is now defined in `common::types` and re-exported above.
// `WorkflowStatus` is a backend-local trait, so this impl on the (now foreign)
// `WorkflowProgress` is permitted by the orphan rule.
impl WorkflowStatus for WorkflowProgress {}

/// Type aliases for backwards compatibility
pub type KickStatus = WorkflowProgress;
pub type OnboardingStatus = WorkflowProgress;

/// Type aliases for backwards compatibility
pub type KickResponse = WorkflowResponse;
pub type OnboardingResponse = WorkflowResponse;

// ============================================================================
// Governance Types (Structured Actions)
// ============================================================================

/// Vault limits configuration (all fields are optional in DAML)
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct VaultLimits {
    #[schema(value_type = Option<String>)]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_deposit: Option<DamlDecimal>,
    #[schema(value_type = Option<String>)]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_deposit_amount: Option<DamlDecimal>,
    #[schema(value_type = Option<String>)]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_withdrawal_amount: Option<DamlDecimal>,
}

/// Featured App Right beneficiary
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AppRewardBeneficiary {
    pub beneficiary: CantonId,
    #[schema(value_type = String)]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub weight: DamlDecimal,
}

/// Featured App Right configuration
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct FarConfig {
    pub featured_app_right_cid: String,
    pub beneficiaries: Vec<AppRewardBeneficiary>,
}

/// Structured action types for Vault governance
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
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
    GovernanceAddAdditionalProposer {
        additional_proposer: CantonId,
    },
    GovernanceRemoveAdditionalProposer {
        additional_proposer: CantonId,
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
    ///
    /// Catches obviously-malformed inputs (negative thresholds, non-positive
    /// timeouts) before they reach Canton's DAML checks. Canton rejects bad
    /// values too, but here we surface a clear 400 rather than a generic
    /// submission error after the proposal contract is already on the wire.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            ActionType::GovernanceAddMember { new_threshold, .. }
            | ActionType::GovernanceRemoveMember { new_threshold, .. }
            | ActionType::GovernanceSetThreshold { new_threshold } => {
                validate_threshold(*new_threshold)
            }
            ActionType::GovernanceSetTimeout {
                new_timeout_microseconds,
            } => validate_timeout(*new_timeout_microseconds),
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

fn validate_threshold(new_threshold: i64) -> Result<(), String> {
    if new_threshold < 1 {
        return Err(format!(
            "new_threshold must be at least 1, got {new_threshold}"
        ));
    }
    Ok(())
}

fn validate_timeout(microseconds: i64) -> Result<(), String> {
    if microseconds <= 0 {
        return Err(format!(
            "new_timeout_microseconds must be positive, got {microseconds}"
        ));
    }
    Ok(())
}

fn validate_positive_amount(amount: &DamlDecimal, field: &str) -> Result<(), String> {
    // `DamlDecimal` itself doesn't implement `PartialOrd`; compare via the
    // inner `rust_decimal::Decimal` returned by `value()` against a parsed
    // zero so we don't need a direct dep on `rust_decimal`.
    let zero = "0"
        .parse::<DamlDecimal>()
        .expect("'0' is a valid DamlDecimal")
        .value();
    if amount.value() <= zero {
        return Err(format!("{field} must be strictly positive, got {amount}"));
    }
    Ok(())
}

fn validate_beneficiary_weights(beneficiaries: &[AppRewardBeneficiary]) -> Result<(), String> {
    if beneficiaries.is_empty() {
        return Ok(());
    }
    let sum: DamlDecimal = beneficiaries.iter().map(|b| b.weight).sum();
    let one: DamlDecimal = "1".parse().expect("'1' is a valid DamlDecimal");
    if sum != one {
        return Err(format!(
            "FAR beneficiary weights must sum to exactly 1.0, got {sum}"
        ));
    }
    Ok(())
}

/// Billing parameters for a paid credential.
/// Mirrors `Utility.Credential.App.V0.Types.BillingParams`.
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct BillingParams {
    /// The daily fee for the credential in USD (corresponds to RatePerDay record).
    #[schema(value_type = String)]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub fee_per_day_usd: DamlDecimal,
    /// Duration between fee charges, in minutes.
    pub billing_period_minutes: i64,
    /// Target deposit amount in USD.
    #[schema(value_type = String)]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub deposit_target_amount_usd: DamlDecimal,
    /// Holder's weight on the activity marker (0.0 - 1.0). None means 0.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub holder_activity_weight: Option<DamlDecimal>,
}

/// Types of governance domain action proposals
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
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
        #[schema(value_type = String)]
        #[cfg_attr(feature = "typegen", ts(type = "string"))]
        amount: DamlDecimal,
        instrument_id: InstrumentId,
        #[serde(default)]
        input_holding_cids: Vec<String>,
        /// How long the transfer (and, for two-step transfers, the resulting
        /// offer) stays valid, in hours. `None` uses the default window. A
        /// bounded window lets an unaccepted offer expire and release escrow.
        #[serde(default)]
        validity_window_hours: Option<u32>,
    },
    /// Accept an incoming token transfer
    AcceptTransfer { transfer_instruction_cid: String },
    /// Generic text-based vote (no on-chain effect beyond recording the result)
    GenericVote { description: String },
    /// Provision a Utility-Registry `ProviderService` with
    /// `operator = proposer` and `provider = governanceParty`. Produces the
    /// ProviderService cid consumed by `SetupUtility`.
    ProvisionProviderService,
    /// Run the full Utility-Registry onboarding in one vote. Flags control
    /// whether a `TransferRule` / `AllocationFactory` are created during the
    /// `RegistrarServiceRequest` accept.
    SetupUtility {
        provider_service_cid: String,
        operator: CantonId,
        instrument_id_text: String,
        #[serde(default)]
        additional_identifiers: Vec<InstrumentIdentifier>,
        create_transfer_rule: bool,
        create_allocation_factory: bool,
    },
    /// Create a `ProviderServiceRequest` for a given `operator` and `provider`.
    CreateProviderServiceRequest {
        operator: CantonId,
        provider: CantonId,
    },
    /// Create a `UserServiceRequest` for a given `operator` and `user`.
    CreateUserServiceRequest { operator: CantonId, user: CantonId },
    /// Set the provider-app reward beneficiaries on an `InstrumentConfiguration`.
    /// `providerAppRewardBeneficiaries = None` clears the current setting.
    SetProviderAppRewardBeneficiaries {
        instrument_configuration_cid: String,
        #[serde(default)]
        provider_app_reward_beneficiaries: Option<Vec<AppRewardBeneficiary>>,
    },
    /// Toggle result-contract emission on a `RegistrarService`.
    SetEnableResultContracts {
        registrar_service_cid: String,
        #[serde(default)]
        enable_result_contracts: Option<bool>,
    },
    /// Authorize the `operator` to create batched activity markers on behalf
    /// of the governance party via a `DelegatedBatchedMarkersProxy`.
    CreateDelegatedBatchedMarkersProxy { operator: CantonId },
    /// Offer a mint of `amount` tokens to `recipient` via
    /// `AllocationFactory_OfferMint`. The resulting `MintOffer` is accepted
    /// later by the recipient, outside this plugin.
    Mint {
        allocation_factory_cid: String,
        instrument_id: InstrumentId,
        instrument_configuration_cid: String,
        recipient: CantonId,
        #[schema(value_type = String)]
        #[cfg_attr(feature = "typegen", ts(type = "string"))]
        amount: DamlDecimal,
        description: String,
    },
    /// Offer a free credential to a holder via the governance party's
    /// `UserService`. Wraps `UserService_OfferFreeCredential` from the
    /// Utility Credential App.
    OfferFreeCredential {
        user_service_cid: String,
        holder: CantonId,
        id: String,
        description: String,
        claims: Vec<Claim>,
    },
    /// Offer a paid credential to a holder via the governance party's
    /// `UserService`. Wraps `UserService_OfferPaidCredential`.
    OfferPaidCredential {
        user_service_cid: String,
        holder: CantonId,
        id: String,
        description: String,
        claims: Vec<Claim>,
        billing_params: BillingParams,
        #[serde(default)]
        #[schema(value_type = Option<String>)]
        #[cfg_attr(feature = "typegen", ts(type = "string"))]
        deposit_initial_amount_usd: Option<DamlDecimal>,
    },
    /// Accept a free credential offered to the governance party. Wraps
    /// `UserService_AcceptFreeCredentialOffer`.
    AcceptFreeCredential {
        user_service_cid: String,
        credential_offer_cid: String,
    },
    /// Offer a burn of `amount` tokens held by `holder` via
    /// `AllocationFactory_OfferBurn`. Holdings are supplied by the holder at
    /// `BurnOffer_Accept` time, not here.
    Burn {
        allocation_factory_cid: String,
        instrument_id: InstrumentId,
        instrument_configuration_cid: String,
        holder: CantonId,
        #[schema(value_type = String)]
        #[cfg_attr(feature = "typegen", ts(type = "string"))]
        amount: DamlDecimal,
        description: String,
    },
    /// Accept a holder-initiated `MintRequest` via `MintRequest_Accept`. The
    /// `MintRequest` must already exist on-ledger (typically created by the
    /// holder by exercising `AllocationFactory_RequestMint`).
    AcceptMintRequest {
        mint_request_cid: String,
        instrument_configuration_cid: String,
        description: String,
    },
    /// Accept a holder-initiated `BurnRequest` via `BurnRequest_Accept`. The
    /// `BurnRequest` must already exist on-ledger (typically created by the
    /// holder by exercising `AllocationFactory_RequestBurn`).
    AcceptBurnRequest {
        burn_request_cid: String,
        instrument_configuration_cid: String,
        description: String,
    },
}

impl ProposalType {
    /// Validate the proposal's fields. Mirrors `ActionType::validate` —
    /// catches non-positive token amounts before they reach Canton's DAML
    /// checks so a 400 surfaces a precise reason rather than a generic
    /// submission error after a proposal contract is already created.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            ProposalType::Transfer {
                amount,
                validity_window_hours,
                ..
            } => {
                validate_positive_amount(amount, "amount")?;
                if *validity_window_hours == Some(0) {
                    return Err("validity_window_hours must be greater than 0".to_string());
                }
                Ok(())
            }
            ProposalType::Mint { amount, .. } | ProposalType::Burn { amount, .. } => {
                validate_positive_amount(amount, "amount")
            }
            ProposalType::OfferPaidCredential {
                deposit_initial_amount_usd: Some(d),
                ..
            } => validate_positive_amount(d, "deposit_initial_amount_usd"),
            ProposalType::SetProviderAppRewardBeneficiaries {
                provider_app_reward_beneficiaries: Some(beneficiaries),
                ..
            } => validate_beneficiary_weights(beneficiaries),
            _ => Ok(()),
        }
    }
}

/// Request to propose a governance domain action (creates proposal contract)
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ProposeActionRequest {
    pub party_id: CantonId,
    pub rules_contract_id: String,
    pub proposal: ProposalType,
}

/// A pending domain action proposal with its confirmations
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
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
    /// `true` when the underlying proposal contract was not found in this
    /// participant's ACS at query time. Confirmations referencing an archived
    /// proposal can't be confirmed/executed (the proposal cid is gone), but
    /// the Confirmation contracts themselves are still active and need to be
    /// expired explicitly to clear them off the ledger. The UI uses this
    /// flag to render a dismiss-only card instead of the normal Confirm /
    /// Execute affordances.
    #[serde(default)]
    pub orphaned: bool,
    /// Structured Transfer-proposal fields (recipient, amount, instrument)
    /// pulled from the on-chain `TransferProposal` contract so the
    /// notification card can display what's actually being transferred
    /// without the user having to inspect the contract CID. Only populated
    /// for `Transfer` proposals.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer_details: Option<TransferProposalDetails>,
    /// Sender / amount / instrument resolved from the `TransferInstruction`
    /// referenced by an `AcceptTransferProposal`. Lets the notification card
    /// show the operator what they're approving (who sent what) without a
    /// follow-up fetch from the UI. Only populated for `AcceptTransfer`
    /// proposals, and only when the linked instruction was readable at query
    /// time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accept_transfer_details: Option<AcceptTransferDetails>,
    /// Operator plus the counterparty (user or provider) pulled from a
    /// `CreateUserServiceRequest` / `CreateProviderServiceRequest` proposal so
    /// the notification card shows the full summary — proposal type (the
    /// `action_label`), operator party, and the user or provider party — without
    /// the operator having to inspect the contract. Only populated for those two
    /// proposal kinds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_request_details: Option<ServiceRequestDetails>,
}

/// Operator + counterparty parties extracted from a service-request proposal
/// (`CreateUserServiceRequest` / `CreateProviderServiceRequest`). Surfaced
/// inside `DomainGovernanceAction` so the pending-approval card can render who
/// the request onboards. Exactly one of `user` / `provider` is set, matching
/// the proposal kind.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ServiceRequestDetails {
    /// Operator party — present on both request kinds.
    pub operator: CantonId,
    /// User party — present for `CreateUserServiceRequest`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<CantonId>,
    /// Provider party — present for `CreateProviderServiceRequest`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<CantonId>,
}

/// Recipient/amount/instrument extracted from a `TransferProposal`'s
/// `transfer` field. Surfaced inside `DomainGovernanceAction` so the
/// notification queue card shows the meaningful parameters of the proposal.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TransferProposalDetails {
    pub receiver: CantonId,
    #[schema(value_type = String)]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub amount: DamlDecimal,
    pub instrument_admin: CantonId,
    pub instrument_id: String,
}

/// Sender/receiver/amount/instrument extracted from the `TransferInstruction`
/// referenced by an `AcceptTransferProposal`. Surfaced inside
/// `DomainGovernanceAction` so the pending-approval card for an Accept can
/// render who's transferring what to whom — the proposal contract itself
/// only carries the `TransferInstruction` cid, not these fields.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AcceptTransferDetails {
    pub sender: CantonId,
    pub receiver: CantonId,
    #[schema(value_type = String)]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub amount: DamlDecimal,
    pub instrument_admin: CantonId,
    pub instrument_id: String,
}

/// Request to submit a confirmation for an action with structured type
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
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

/// Request to execute a confirmed action with structured type
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
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
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct GovernanceConfirmation {
    pub contract_id: String,
    pub action: ActionType,
    pub confirming_party: CantonId,
    /// Unix seconds when the confirmation contract was created on the ledger.
    /// 0 if the timestamp could not be resolved.
    #[serde(default)]
    pub created_at: i64,
    /// Unix seconds of the confirmation's `expiresAt`. 0 if unresolved.
    #[serde(default)]
    pub expires_at: i64,
}

/// A governance action with its confirmations, grouped by action hash
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
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
    /// Unix seconds of the most recent confirmation (used for sorting in UI).
    #[serde(default)]
    pub last_confirmation_at: i64,
}

/// Response for governance confirmations endpoint
#[derive(Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct GovernanceResponse {
    pub actions: Vec<GovernanceAction>,
    /// Pending domain action proposals (governance-core GovernableAction)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domain_actions: Vec<DomainGovernanceAction>,
    pub threshold: usize,
    /// The member party ID for the requesting party (used to identify own confirmations)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_party_id: Option<CantonId>,
    /// Current contract id of the active GovernanceRules / VaultGovernanceRules
    /// contract for this party. The choice exercised when confirming an action
    /// is consuming, so this id changes after each confirm/execute — clients
    /// should use this field rather than a cached value to avoid
    /// `CONTRACT_NOT_FOUND` on stale ids.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rules_contract_id: Option<String>,
    /// True when the active governance-core rules contract is under an older
    /// package than configured (see `GovernanceState::out_of_date`).
    #[serde(default)]
    pub gov_core_out_of_date: bool,
    /// The package ref the rules contract actually lives under (for display
    /// in the out-of-date warning).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gov_core_package_ref: Option<String>,
}

/// An open `TransferInstruction` whose `receiver` is this party. Includes
/// offers waiting on an internal workflow (admin / registrar) so the dropdown
/// can surface them as "pending: X" rather than silently hide them.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TransferInstructionInfo {
    pub contract_id: String,
    pub sender: CantonId,
    pub receiver: CantonId,
    #[schema(value_type = String)]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub amount: DamlDecimal,
    pub instrument_admin: CantonId,
    pub instrument_id: String,
    pub status: TransferInstructionStatus,
    /// For `PendingInternalWorkflow`: the parties whose action is awaited and
    /// the human-readable label of what they need to do.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_actions: Vec<PendingAction>,
    /// Unix seconds of the offer's `executeBefore` deadline. Past-deadline
    /// rows are surfaced anyway (disabled in the UI) so the user can see they
    /// exist — DAML refuses to Accept them, but staying silent confused users.
    #[serde(default)]
    pub expires_at: i64,
}

/// One row of `TransferInstructionStatus.pendingActions`. The Daml type is
/// `Map Party Text`; the receiver can render "<party> — <action>" per row.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct PendingAction {
    pub party: CantonId,
    pub action: String,
}

/// Mirrors `Splice.Api.Token.TransferInstructionV1.TransferInstructionStatus`.
#[derive(Clone, Copy, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(rename_all = "snake_case")]
pub enum TransferInstructionStatus {
    PendingReceiverAcceptance,
    PendingInternalWorkflow,
}

/// Response for the transfer instructions endpoint.
#[derive(Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TransferInstructionsResponse {
    pub transfer_instructions: Vec<TransferInstructionInfo>,
}

/// An open `MintRequest`/`BurnRequest` (`Utility.Registry.App.V0.Model.{Mint,Burn}`)
/// the governance party can accept. The shape is identical for both kinds; the
/// containing endpoint disambiguates. `expires_at` is read off the inner
/// `mint`/`burn` payload's `executeBefore` field so the dropdown can disable
/// past-deadline rows.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TokenRequestInfo {
    pub contract_id: String,
    pub holder: CantonId,
    #[schema(value_type = String)]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub amount: DamlDecimal,
    pub instrument_admin: CantonId,
    pub instrument_id: String,
    /// Unix seconds of the request's `executeBefore` deadline.
    pub expires_at: i64,
}

/// Response for the mint-requests endpoint.
#[derive(Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct MintRequestsResponse {
    pub mint_requests: Vec<TokenRequestInfo>,
}

/// Response for the burn-requests endpoint.
#[derive(Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct BurnRequestsResponse {
    pub burn_requests: Vec<TokenRequestInfo>,
}

/// A token-standard Holding owned by a decentralized party, aggregated across
/// every active `Splice.Api.Token.HoldingV1:Holding` contract that shares the
/// same `(instrument_admin, instrument_id)` pair.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct HoldingInfo {
    pub instrument_admin: CantonId,
    pub instrument_id: String,
    /// Total amount held, summed across every active `Holding` contract for
    /// this instrument — including locked ones.
    #[schema(value_type = String)]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub amount: DamlDecimal,
    /// Portion of `amount` that is locked (escrowed for an in-flight
    /// transfer/allocation) and therefore not freely transferable. The
    /// available balance is `amount - locked_amount`.
    #[schema(value_type = String)]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub locked_amount: DamlDecimal,
    /// True if a `TransferPreapproval` is in place for this party for this
    /// instrument. CC (Amulet) holdings match when any
    /// `Splice.AmuletRules:TransferPreapproval` exists; utility-token holdings
    /// match by `(instrument_admin, instrument_id)` against
    /// `Utility.Registry.App.V0.Model.TransferPreapproval` contracts.
    pub preapproval_set_up: bool,
}

/// Response for the holdings endpoint.
#[derive(Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct HoldingsResponse {
    pub holdings: Vec<HoldingInfo>,
}

// ============================================================================
// Audit Trail Types
// ============================================================================

/// Query parameters for the governance audit endpoint
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct AuditLogQuery {
    /// Decentralized party ID to filter audit entries
    pub party_id: CantonId,
    /// Maximum number of entries to return (default 50)
    #[serde(default = "default_audit_limit")]
    pub limit: i64,
    /// Offset for pagination (default 0)
    #[serde(default)]
    pub offset: i64,
}

fn default_audit_limit() -> i64 {
    50
}

// ============================================================================
// Chain Audit Trail Types
// ============================================================================

/// Query parameters for the on-chain governance audit endpoint
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ChainAuditQuery {
    /// Decentralized party ID to query chain events for
    pub party_id: CantonId,
    /// Maximum number of entries to return (default 100)
    #[serde(default = "default_chain_audit_limit")]
    pub limit: usize,
    /// When true, fetches fresh data from Canton and updates cache
    #[serde(default)]
    pub refresh: bool,
}

fn default_chain_audit_limit() -> usize {
    100
}

/// Build a [`ChainAuditEntry`] wire DTO from a cached DB row.
///
/// A free function rather than `impl From` because `ChainAuditEntry` now lives
/// in the `common` crate; the orphan rule forbids implementing the foreign
/// `From` trait for a foreign type here. Mirrors [`permission_from_proto`].
pub fn chain_audit_entry_from_row(row: crate::db::rows::ChainAuditCacheRow) -> ChainAuditEntry {
    ChainAuditEntry {
        offset: row.offset,
        timestamp: row.timestamp,
        event_type: row.event_type,
        contract_id: row.contract_id,
        template_id: row.template_id,
        package_id: row.package_id,
        governance_type: row.governance_type,
        action_summary: row.action_summary,
        choice: row.choice,
        acting_parties: serde_json::from_str(&row.acting_parties).unwrap_or_default(),
        update_id: row.update_id,
        details: serde_json::from_str(&row.details).unwrap_or(serde_json::Value::Null),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use sqlx::SqlitePool;

    use super::*;
    use crate::{
        config::{NetworkConfig, NodeConfig},
        db::MIGRATOR,
        error::Result,
        noise::{load_or_generate_keypair, server::NoiseServer},
        server::peer_status::LastSeen,
        workflow::OnboardingStep,
    };

    /// Build a real `ActiveWorkflow` (the enum is over `NoiseServer<S>`, which
    /// has no test double) for registry routing tests. `dir` must outlive the
    /// call — `NoiseServer::new` reads the keypair from it.
    async fn test_active_workflow(
        pool: &SqlitePool,
        dir: &tempfile::TempDir,
        instance: &str,
    ) -> Result<ActiveWorkflow> {
        let config = NodeConfig::default().with_root_dir(dir.path());
        tokio::fs::create_dir_all(config.data_dir()).await?;
        load_or_generate_keypair(config.key_file_path()).await?;
        let last_seen: LastSeen = Arc::new(RwLock::new(HashMap::new()));
        let server = NoiseServer::new(
            config,
            NetworkConfig::from_peers(Vec::new()),
            pool.clone(),
            instance.to_string(),
            OnboardingStep::WaitingForPeers,
            None,
            last_seen,
        )
        .await
        .map_err(|e| anyhow::anyhow!("NoiseServer::new: {e}"))?;
        Ok(ActiveWorkflow::Onboarding(Arc::new(server)))
    }

    #[test]
    fn registry_rejects_duplicate_instance_and_guard_removes_own_entry() {
        let registry = WorkflowRegistry::new();
        let a = WorkflowInstance::new(
            "a-creation".to_string(),
            WorkflowKind::Onboarding,
            WorkflowRole::Coordinator,
        );
        let a_dup = WorkflowInstance::new(
            "a-creation".to_string(),
            WorkflowKind::Onboarding,
            WorkflowRole::Coordinator,
        );
        let b = WorkflowInstance::new(
            "b-creation".to_string(),
            WorkflowKind::Onboarding,
            WorkflowRole::Coordinator,
        );

        assert!(registry.insert(a));
        // Same-instance registration must be rejected (the start handlers turn
        // this into a 409) while a distinct sibling registers fine.
        assert!(!registry.insert(a_dup));
        assert!(registry.insert(b));

        // A guard dropping removes exactly its own entry — never a sibling's.
        let guard = WorkflowGuard::new(registry.clone(), "a-creation".to_string());
        drop(guard);
        assert!(registry.get("a-creation").is_none());
        assert!(registry.get("b-creation").is_some());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn route_is_exact_for_keyed_peers_with_sole_active_fallback_for_legacy(
        pool: SqlitePool,
    ) -> Result {
        let dir = tempfile::tempdir()?;
        let registry = WorkflowRegistry::new();

        // Run A is fully live (registered + Noise handle set); run B is
        // registered but still spinning up (no handle yet) — the exact window
        // the routing rules exist for.
        let a = WorkflowInstance::new(
            "a-creation".to_string(),
            WorkflowKind::Onboarding,
            WorkflowRole::Coordinator,
        );
        let b = WorkflowInstance::new(
            "b-creation".to_string(),
            WorkflowKind::Onboarding,
            WorkflowRole::Coordinator,
        );
        assert!(registry.insert(a.clone()));
        assert!(registry.insert(b.clone()));
        a.set_active(test_active_workflow(&pool, &dir, "a-creation").await?);

        // Keyed peers route exactly: A's traffic reaches A.
        assert!(registry.route("a-creation").is_some());
        // A peer naming run B must get None (503 -> bounded retry) while B
        // spins up — NEVER a fallback onto sibling A, which would Disconnect
        // it (the G3 regression this rule fixed).
        assert!(registry.route("b-creation").is_none());
        // An unknown key (cancelled/dismissed run) also gets None.
        assert!(registry.route("no-such-run").is_none());
        // A legacy/resumed peer with no key falls back to the sole active run.
        assert!(registry.route("").is_some());

        // Once B is live too, exact keys both route, but an empty key is
        // ambiguous and must refuse rather than guess.
        b.set_active(test_active_workflow(&pool, &dir, "b-creation").await?);
        assert!(registry.route("a-creation").is_some());
        assert!(registry.route("b-creation").is_some());
        assert!(registry.route("").is_none());

        Ok(())
    }

    /// P3: locks the wire shape of `WorkflowRun` so the `String → CantonId`
    /// typing change for participant-id fields cannot silently switch from
    /// plain strings to nested objects on the JSON the frontend consumes.
    #[test]
    fn workflow_run_serializes_canton_ids_as_plain_strings() {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        let participant_id_str = format!("participant::{ns}");
        let dec_party_id_str = format!("test-network-1::{ns}");

        let peer_a = CantonId::parse(&format!("participant::{ns}")).unwrap();
        let peer_b = CantonId::parse(&format!(
            "participant::1220{0}{0}",
            "abcdefabcdefabcdefabcdefabcdef00"
        ))
        .unwrap();

        let run = WorkflowRun {
            instance_name: "test-network-1-creation".to_string(),
            kind: WorkflowKind::Onboarding,
            role: WorkflowRole::Coordinator,
            status: WorkflowProgress::InProgress,
            current_step: "WaitingForPeers".to_string(),
            step_index: 0,
            step_total: 7,
            config_json: r#"{"prefix":"test-network-1"}"#.to_string(),
            coordinator_pubkey: None,
            coordinator_instance: None,
            coordinator_name: None,
            expected_peers: vec![peer_a.clone(), peer_b.clone()],
            completed_peers: vec![peer_a],
            dec_party_id: Some(CantonId::parse(&dec_party_id_str).unwrap()),
            prefix: None,
            participants: Vec::new(),
            previous_threshold: None,
            new_threshold: None,
            kicked_participant: None,
            package_names: Vec::new(),
            dar_filenames: Vec::new(),
            error: None,
            dismissed: false,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_001,
        };

        let json = serde_json::to_value(&run).expect("serialize WorkflowRun");

        // expected_peers and completed_peers must be JSON arrays of
        // plain strings — never objects with prefix/namespace fields.
        let expected = json
            .get("expected_peers")
            .and_then(Value::as_array)
            .expect("expected_peers must be a JSON array");
        assert_eq!(expected.len(), 2);
        for v in expected {
            assert!(
                v.is_string(),
                "expected_peers entry must be a string, got {v}"
            );
        }
        assert_eq!(expected[0].as_str().unwrap(), participant_id_str);

        let completed = json
            .get("completed_peers")
            .and_then(Value::as_array)
            .expect("completed_peers must be a JSON array");
        assert_eq!(completed.len(), 1);
        assert!(completed[0].is_string());

        // dec_party_id (Option<CantonId>) must serialize as a plain string,
        // not as a nested object with prefix/namespace fields.
        let dec_party = json.get("dec_party_id").expect("dec_party_id key present");
        assert!(
            dec_party.is_string(),
            "dec_party_id must be a JSON string when set, got {dec_party}"
        );
        assert_eq!(dec_party.as_str().unwrap(), dec_party_id_str);
    }

    #[test]
    fn action_threshold_rejects_zero_and_negative() {
        let action = ActionType::GovernanceSetThreshold { new_threshold: 0 };
        assert!(action.validate().is_err());
        let action = ActionType::GovernanceSetThreshold { new_threshold: -3 };
        assert!(action.validate().is_err());
        let action = ActionType::GovernanceSetThreshold { new_threshold: 1 };
        assert!(action.validate().is_ok());
    }

    #[test]
    fn action_threshold_rejects_in_add_remove_member() {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        let member = CantonId::parse(&format!("member::{ns}")).unwrap();
        let action = ActionType::GovernanceAddMember {
            member: member.clone(),
            new_threshold: 0,
        };
        assert!(action.validate().is_err());
        let action = ActionType::GovernanceRemoveMember {
            member,
            new_threshold: -1,
        };
        assert!(action.validate().is_err());
    }

    #[test]
    fn action_timeout_rejects_zero_and_negative() {
        let action = ActionType::GovernanceSetTimeout {
            new_timeout_microseconds: 0,
        };
        assert!(action.validate().is_err());
        let action = ActionType::GovernanceSetTimeout {
            new_timeout_microseconds: -1_000_000,
        };
        assert!(action.validate().is_err());
        let action = ActionType::GovernanceSetTimeout {
            new_timeout_microseconds: 60_000_000,
        };
        assert!(action.validate().is_ok());
    }

    #[test]
    fn proposal_transfer_rejects_non_positive_amount() {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        let to = CantonId::parse(&format!("recv::{ns}")).unwrap();
        let admin = CantonId::parse(&format!("admin::{ns}")).unwrap();
        let mk = |amount: &str, window: Option<u32>| ProposalType::Transfer {
            transfer_factory_cid: "tf".to_string(),
            expected_admin: admin.clone(),
            receiver: to.clone(),
            amount: amount.parse().expect("valid decimal"),
            instrument_id: InstrumentId {
                admin: "a".into(),
                id: "i".into(),
            },
            input_holding_cids: Vec::new(),
            validity_window_hours: window,
        };
        assert!(mk("0", None).validate().is_err());
        assert!(mk("-1.5", None).validate().is_err());
        assert!(mk("0.0001", None).validate().is_ok());
        // A custom (positive) window is accepted; a zero-hour window is rejected.
        assert!(mk("1.0", Some(48)).validate().is_ok());
        assert!(mk("1.0", Some(0)).validate().is_err());
    }
}
