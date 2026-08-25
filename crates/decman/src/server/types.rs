use std::{
    collections::HashMap,
    sync::{Arc, RwLock as StdRwLock},
};

use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::digitalasset::canton::protocol::v30::enums::ParticipantPermission;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

// Wire DTOs that the `decman-cli` client and the frontend also need live in the
// shared `common` crate. They are re-exported here so existing
// `crate::server::types::X` (and the glob `pub use types::*` in `server/mod.rs`)
// keep resolving unchanged. `common::api` holds the HTTP request/response DTOs
// the frontend's TypeScript is generated from (see `decman/build.rs`).
pub use common::api::PAGE_SIZE;
pub use common::api::{
    ActiveCouponReassignmentDelegation, AddPartyInvitePayload, AddPartyRequest, AuditLogResponse,
    AuthStatus, AuthStatusResponse, AuthTestResponse, AuthTestResult, CancelConfirmationRequest,
    CancelProposalRequest, ChainAuditEntry, ChainAuditResponse, ChangeThresholdInvitePayload,
    ChangeThresholdRequest, Claim, ContractQueryResponse, ContractWithBlob, ContractsInvitePayload,
    ContractsRequest, CouponReassignmentDelegationSummary, CredentialInfo, CredentialOfferInfo,
    CredentialOffersResponse, CredentialsResponse, DarsInvitePayload, DarsRequest,
    DecentralizedPartiesResponse, DeclineInvitationPayload, DisclosedContractInput,
    DiscoverMemberPartyRequest, DiscoverMemberPartyResponse, ErrorResponse,
    ExpireConfirmationRequest, ExternalPartiesResponse, ExternalPartyInfo, GovernanceState,
    GovernanceStateResponse, GovernanceType, GrantRightsRequest, GrantRightsResponse,
    InstrumentInfo, InstrumentsResponse, InvitationActionRequest, KeyStatusResponse,
    KickInvitePayload, KickRequest, KnownMember, KnownMembersResponse, MessageResponse,
    MissingEdgeKind, MissingPeerEdge, NetworkInfo, OnboardingInvitePayload,
    OnboardingMeshErrorResponse, OnboardingRequest, OperatorInfo, PartyAuthStatus,
    PartyConfigRequest, PartyConfigResponse, PendingInvitationsResponse, ProviderConfigurationInfo,
    ProviderConfigurationsResponse, ProviderServiceInfo, ProviderServicesResponse,
    RegistrarServiceInfo, RegistrarServiceRequestInfo, RegistrarServiceRequestsResponse,
    RegistrarServicesResponse, ResponseSource, RightsStatus, SuccessResponse, TenantOnboardRequest,
    TenantOnboardResponse, TenantPrepareRequest, TenantPrepareResponse, TransferFactoriesResponse,
    TransferFactoryInfo, TransferPreapprovalsResponse, UserServiceInfo, UserServicesResponse,
    VaultInfo, VaultsResponse, WorkflowResponse, WorkflowRunsResponse, WorkflowStatusResponse,
};
pub use common::types::{
    AuditLogEntry, AuthConfigResponse, ConnectionStatus, ContractInfo, DecentralizedParty,
    InvitationType, PackageInfo, ParticipantInfo, ParticipantStatus, ParticipantsStatusResponse,
    PartyMetadata, PeerErrorKind, PeerPackageComparison, PeerPackageResult, PendingInvitation,
    Permission, VettedPackageInfo, WorkflowKind, WorkflowProgress, WorkflowRole, WorkflowRun,
};
// The payload support types and their protocol validators now live in
// `decman-lib`, re-exported here so existing `crate::server::types::X`
// paths keep resolving unchanged.
pub use decman_lib::catalog::types::{AppRewardBeneficiary, BillingParams, FarConfig, VaultLimits};
use decman_lib::framework::Validate;

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
    /// run (if exactly one), since it has no key to match on. Note the
    /// fallback's inherent imprecision: if the no-key peer's true run is
    /// registered-but-not-yet-active while a sibling IS active, it gets routed
    /// to the sibling. Post-upgrade only pre-migration resumed rows produce
    /// no-key traffic (the wire break forces lockstep upgrades), so the
    /// exposure is one resume window per legacy row.
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

// `VaultLimits`, `AppRewardBeneficiary`, and `FarConfig` now live in
// `decman_lib::catalog::types` and are re-exported above so existing
// `crate::server::types::X` paths keep resolving. `RewardBeneficiary` moved
// there too, but since Task 13 has no non-test consumer under
// `crate::server::types::X` — `SetupCouponReassignmentDelegation`'s field
// lives on `decman_lib::catalog::proposals::rewards::SetupCouponReassignmentDelegation`,
// which references `decman_lib::catalog::types::RewardBeneficiary` directly
// — so the re-export was dropped; test fixtures import it straight from
// `decman_lib::catalog::types` instead.

/// `ActionType` — including its Daml `Value` codec (`to_vault_proto` /
/// `from_vault_proto` / `to_self_proto` / `from_self_proto`) and `validate`
/// — now lives in `decman_lib::catalog::action`, re-exported here so
/// existing `crate::server::types::ActionType` paths keep resolving.
pub use decman_lib::catalog::action::ActionType;

// The protocol validators (`validate_threshold`, `validate_timeout`,
// `validate_unique_issuers`, `validate_self_issued_requirements_have_claims`,
// `validate_future_micros`, `validate_positive_amount`,
// `validate_beneficiary_weights`, `validate_reward_beneficiaries`) now live
// only in `decman-lib` (`framework::validate`), called from each payload's
// own `Validate` impl — every `ProposalType::validate` arm below just
// delegates to `p.validate(&ctx)`, so this module no longer imports them
// directly. `BillingParams` lives in `decman-lib` (`catalog::types`),
// imported and re-exported above.

/// Types of governance domain action proposals
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProposalType {
    /// Set up Canton Coin TransferPreapproval
    SetupCcPreapproval(decman_lib::catalog::proposals::custody::SetupCcPreapproval),
    /// Set up utility token TransferPreapproval
    SetupTokenPreapproval(decman_lib::catalog::proposals::custody::SetupTokenPreapproval),
    /// Transfer tokens via a TransferFactory
    Transfer(decman_lib::catalog::proposals::custody::Transfer),
    /// Accept an incoming token transfer
    AcceptTransfer(decman_lib::catalog::proposals::custody::AcceptTransfer),
    /// Generic text-based vote (no on-chain effect beyond recording the result)
    GenericVote(decman_lib::catalog::proposals::core::GenericVote),
    /// Provision a Utility-Registry `ProviderService` with
    /// `operator = proposer` and `provider = governanceParty`. Produces the
    /// ProviderService cid consumed by `SetupUtility`.
    ProvisionProviderService(decman_lib::catalog::proposals::utility::ProvisionProviderService),
    /// Run the full Utility-Registry onboarding in one vote. Flags control
    /// whether a `TransferRule` / `AllocationFactory` are created during the
    /// `RegistrarServiceRequest` accept.
    SetupUtility(decman_lib::catalog::proposals::utility::SetupUtility),
    /// Create a `ProviderServiceRequest` for a given `operator` and `provider`.
    CreateProviderServiceRequest(
        decman_lib::catalog::proposals::utility::CreateProviderServiceRequest,
    ),
    /// Create a `UserServiceRequest` for a given `operator` and `user`.
    CreateUserServiceRequest(decman_lib::catalog::proposals::utility::CreateUserServiceRequest),
    /// Set the provider-app reward beneficiaries on an `InstrumentConfiguration`.
    /// `providerAppRewardBeneficiaries = None` clears the current setting.
    SetProviderAppRewardBeneficiaries(
        decman_lib::catalog::proposals::utility::SetProviderAppRewardBeneficiaries,
    ),
    /// Create (or replace) the decparty's on-ledger CouponReassignmentDelegation.
    /// `prior_delegation` is the cid of the delegation being replaced (None for the first).
    SetupCouponReassignmentDelegation(
        decman_lib::catalog::proposals::rewards::SetupCouponReassignmentDelegation,
    ),
    /// Revoke (archive) the decparty's CouponReassignmentDelegation.
    RevokeCouponReassignmentDelegation(
        decman_lib::catalog::proposals::rewards::RevokeCouponReassignmentDelegation,
    ),
    /// Toggle result-contract emission on a `RegistrarService`.
    SetEnableResultContracts(decman_lib::catalog::proposals::utility::SetEnableResultContracts),
    /// Authorize the `operator` to create batched activity markers on behalf
    /// of the governance party via a `DelegatedBatchedMarkersProxy`.
    CreateDelegatedBatchedMarkersProxy(
        decman_lib::catalog::proposals::utility::CreateDelegatedBatchedMarkersProxy,
    ),
    /// Delegate minting of the governance party's CIP-104 reward coupons to a
    /// validator node's `delegate` party via a `MintingDelegationProposal`.
    /// The delegation beneficiary is always the governance party; the delegate
    /// accepts the proposal out-of-band via the wallet API.
    SetupMintingDelegation(decman_lib::catalog::proposals::rewards::SetupMintingDelegation),
    /// Accept a validator-created `ExternalPartySetupProposal` on behalf of the
    /// governance party, creating its `ValidatorRight` + `TransferPreapproval`.
    /// This is the missing prerequisite that makes the validator's built-in
    /// `MintingDelegationCollectRewardsTrigger` start collecting the party's
    /// CIP-104 reward coupons via the established `MintingDelegation`.
    AcceptExternalPartySetup(decman_lib::catalog::proposals::rewards::AcceptExternalPartySetup),
    /// Offer a mint of `amount` tokens to `recipient` via
    /// `AllocationFactory_OfferMint`. The resulting `MintOffer` is accepted
    /// later by the recipient, outside this plugin.
    Mint(decman_lib::catalog::proposals::utility::Mint),
    /// Offer a free credential to a holder via the governance party's
    /// `UserService`. Wraps `UserService_OfferFreeCredential` from the
    /// Utility Credential App.
    OfferFreeCredential(decman_lib::catalog::proposals::credential::OfferFreeCredential),
    /// Offer a paid credential to a holder via the governance party's
    /// `UserService`. Wraps `UserService_OfferPaidCredential`.
    OfferPaidCredential(decman_lib::catalog::proposals::credential::OfferPaidCredential),
    /// Accept a free credential offered to the governance party. Wraps
    /// `UserService_AcceptFreeCredentialOffer`.
    AcceptFreeCredential(decman_lib::catalog::proposals::credential::AcceptFreeCredential),
    /// Offer a burn of `amount` tokens held by `holder` via
    /// `AllocationFactory_OfferBurn`. Holdings are supplied by the holder at
    /// `BurnOffer_Accept` time, not here.
    Burn(decman_lib::catalog::proposals::utility::Burn),
    /// Accept a holder-initiated `MintRequest` via `MintRequest_Accept`. The
    /// `MintRequest` must already exist on-ledger (typically created by the
    /// holder by exercising `AllocationFactory_RequestMint`).
    AcceptMintRequest(decman_lib::catalog::proposals::utility::AcceptMintRequest),
    /// Accept a holder-initiated `BurnRequest` via `BurnRequest_Accept`. The
    /// `BurnRequest` must already exist on-ledger (typically created by the
    /// holder by exercising `AllocationFactory_RequestBurn`).
    AcceptBurnRequest(decman_lib::catalog::proposals::utility::AcceptBurnRequest),
    /// Create the provider decparty's `ProviderConfiguration` with
    /// credential requirements for registrars and holders. Executed once by
    /// the provider decparty at platform setup.
    CreateProviderConfiguration(
        decman_lib::catalog::proposals::utility::CreateProviderConfiguration,
    ),
    /// Create a `RegistrarServiceRequest` asking `provider` for registrar
    /// service, with the governance party as the registrar. The provider
    /// accepts later via `OnboardRegistrar` on its own decparty.
    CreateRegistrarServiceRequest(
        decman_lib::catalog::proposals::utility::CreateRegistrarServiceRequest,
    ),
    /// Accept a `RegistrarServiceRequest` on the provider decparty: mint the
    /// registrar credentials the governance party can self-issue against the
    /// `ProviderConfiguration`'s registrar requirements, then accept the
    /// request in the same vote.
    OnboardRegistrar(decman_lib::catalog::proposals::utility::OnboardRegistrar),
    /// Create an `InstrumentConfiguration` on the registrar decparty and
    /// credential the initial instrument issuers against its issuer
    /// requirements. Executed once per instrument.
    ProvisionInstrument(decman_lib::catalog::proposals::utility::ProvisionInstrument),
    /// Credential new instrument issuers against an existing
    /// `InstrumentConfiguration`'s issuer requirements.
    OnboardInstrumentIssuers(decman_lib::catalog::proposals::utility::OnboardInstrumentIssuers),
    /// Revoke the credentials the governance party issued for instrument
    /// issuers, removing their issuing privileges. Each row names one issuer
    /// and lists that issuer's credentials.
    OffboardInstrumentIssuers(decman_lib::catalog::proposals::utility::OffboardInstrumentIssuers),
}

impl ProposalType {
    /// Validate the proposal's fields against the governance party the
    /// proposal targets. Mirrors `ActionType::validate` — catches bad input
    /// before it reaches Canton's Daml checks so a 400 surfaces a precise
    /// reason rather than a generic submission error.
    ///
    /// **Propose-path only.** The single production caller is
    /// `handlers::governance::propose_action`, and one arm
    /// ([`validate_future_micros`]) reads the clock. Re-using this to
    /// re-validate an already-stored proposal would reject it for nothing but
    /// having aged, so a new call site needs to split the time-dependent arms
    /// out first.
    pub fn validate(&self, governance_party: &CantonId) -> Result<(), String> {
        let ctx = decman_lib::framework::ValidationCtx {
            governance_party,
            now_micros: Utc::now().timestamp_micros(),
        };
        match self {
            ProposalType::GenericVote(p) => p.validate(&ctx).map_err(|e| e.to_string()),
            ProposalType::ProvisionProviderService(p) => {
                p.validate(&ctx).map_err(|e| e.to_string())
            }
            ProposalType::CreateProviderServiceRequest(p) => {
                p.validate(&ctx).map_err(|e| e.to_string())
            }
            ProposalType::CreateUserServiceRequest(p) => {
                p.validate(&ctx).map_err(|e| e.to_string())
            }
            ProposalType::CreateDelegatedBatchedMarkersProxy(p) => {
                p.validate(&ctx).map_err(|e| e.to_string())
            }
            ProposalType::SetupCcPreapproval(p) => p.validate(&ctx).map_err(|e| e.to_string()),
            ProposalType::SetupTokenPreapproval(p) => p.validate(&ctx).map_err(|e| e.to_string()),
            ProposalType::Transfer(p) => p.validate(&ctx).map_err(|e| e.to_string()),
            ProposalType::AcceptTransfer(p) => p.validate(&ctx).map_err(|e| e.to_string()),
            ProposalType::SetupCouponReassignmentDelegation(p) => {
                p.validate(&ctx).map_err(|e| e.to_string())
            }
            ProposalType::RevokeCouponReassignmentDelegation(p) => {
                p.validate(&ctx).map_err(|e| e.to_string())
            }
            ProposalType::SetupMintingDelegation(p) => p.validate(&ctx).map_err(|e| e.to_string()),
            ProposalType::AcceptExternalPartySetup(p) => {
                p.validate(&ctx).map_err(|e| e.to_string())
            }
            ProposalType::Mint(p) => p.validate(&ctx).map_err(|e| e.to_string()),
            ProposalType::Burn(p) => p.validate(&ctx).map_err(|e| e.to_string()),
            ProposalType::OfferFreeCredential(p) => p.validate(&ctx).map_err(|e| e.to_string()),
            ProposalType::OfferPaidCredential(p) => p.validate(&ctx).map_err(|e| e.to_string()),
            ProposalType::AcceptFreeCredential(p) => p.validate(&ctx).map_err(|e| e.to_string()),
            ProposalType::SetupUtility(p) => p.validate(&ctx).map_err(|e| e.to_string()),
            ProposalType::SetProviderAppRewardBeneficiaries(p) => {
                p.validate(&ctx).map_err(|e| e.to_string())
            }
            ProposalType::SetEnableResultContracts(p) => {
                p.validate(&ctx).map_err(|e| e.to_string())
            }
            ProposalType::AcceptMintRequest(p) => p.validate(&ctx).map_err(|e| e.to_string()),
            ProposalType::AcceptBurnRequest(p) => p.validate(&ctx).map_err(|e| e.to_string()),
            ProposalType::CreateProviderConfiguration(p) => {
                p.validate(&ctx).map_err(|e| e.to_string())
            }
            ProposalType::CreateRegistrarServiceRequest(p) => {
                p.validate(&ctx).map_err(|e| e.to_string())
            }
            ProposalType::OnboardRegistrar(p) => p.validate(&ctx).map_err(|e| e.to_string()),
            ProposalType::ProvisionInstrument(p) => p.validate(&ctx).map_err(|e| e.to_string()),
            ProposalType::OnboardInstrumentIssuers(p) => {
                p.validate(&ctx).map_err(|e| e.to_string())
            }
            ProposalType::OffboardInstrumentIssuers(p) => {
                p.validate(&ctx).map_err(|e| e.to_string())
            }
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
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DomainGovernanceAction {
    /// Contract ID of the proposal
    pub proposal_cid: String,
    /// Human-readable label (e.g., "SetupCcPreapproval")
    pub action_label: String,
    /// Human-readable description from the proposal's GovernableActionView
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer_details: Option<TransferProposalDetails>,
    /// Sender / amount / instrument resolved from the `TransferInstruction`
    /// referenced by an `AcceptTransferProposal`. Lets the notification card
    /// show the operator what they're approving (who sent what) without a
    /// follow-up fetch from the UI. Only populated for `AcceptTransfer`
    /// proposals, and only when the linked instruction was readable at query
    /// time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept_transfer_details: Option<AcceptTransferDetails>,
    /// Operator plus the counterparty (user or provider) pulled from a
    /// `CreateUserServiceRequest` / `CreateProviderServiceRequest` proposal so
    /// the notification card shows the full summary — proposal type (the
    /// `action_label`), operator party, and the user or provider party — without
    /// the operator having to inspect the contract. Only populated for those two
    /// proposal kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_request_details: Option<ServiceRequestDetails>,
    /// The member who created the proposal, read from the proposal contract.
    /// Only that member can retract it with `GovernableAction_ProposerCancel`,
    /// so the card shows the retract button when this equals the node's own
    /// member party. Absent on an orphaned card, where the proposal contract
    /// is no longer readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposer: Option<CantonId>,
    /// Ledger effective time of the proposal's create event, in seconds. The
    /// notification feed sorts on this, so a proposal holds its place between
    /// refreshes whether or not anyone has confirmed it. Absent on an orphaned
    /// card, where the proposal contract is no longer readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
}

/// Operator + counterparty parties extracted from a service-request proposal
/// (`CreateUserServiceRequest` / `CreateProviderServiceRequest`). Surfaced
/// inside `DomainGovernanceAction` so the pending-approval card can render who
/// the request onboards. Exactly one of `user` / `provider` is set, matching
/// the proposal kind.
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ServiceRequestDetails {
    /// Operator party — present on both request kinds.
    pub operator: CantonId,
    /// User party — present for `CreateUserServiceRequest`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<CantonId>,
    /// Provider party — present for `CreateProviderServiceRequest`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<CantonId>,
}

/// Recipient/amount/instrument extracted from a `TransferProposal`'s
/// `transfer` field. Surfaced inside `DomainGovernanceAction` so the
/// notification queue card shows the meaningful parameters of the proposal.
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
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
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
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
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
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
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
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
#[derive(Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct GovernanceResponse {
    pub actions: Vec<GovernanceAction>,
    /// Pending domain action proposals (governance-core GovernableAction)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domain_actions: Vec<DomainGovernanceAction>,
    pub threshold: usize,
    /// The member party ID for the requesting party (used to identify own confirmations)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_party_id: Option<CantonId>,
    /// Current contract id of the active GovernanceRules / VaultGovernanceRules
    /// contract for this party. The choice exercised when confirming an action
    /// is consuming, so this id changes after each confirm/execute — clients
    /// should use this field rather than a cached value to avoid
    /// `CONTRACT_NOT_FOUND` on stale ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules_contract_id: Option<String>,
    /// True when the active governance-core rules contract is under an older
    /// package than configured (see `GovernanceState::out_of_date`).
    #[serde(default)]
    pub gov_core_out_of_date: bool,
    /// The package ref the rules contract actually lives under (for display
    /// in the out-of-date warning).
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    /// exist — Daml refuses to Accept them, but staying silent confused users.
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
    /// Maximum number of entries to return (default [`PAGE_SIZE`], capped at
    /// [`MAX_CHAIN_AUDIT_LIMIT`])
    #[serde(default = "default_chain_audit_limit")]
    pub limit: usize,
    /// Cursor: return only entries strictly older than this ledger offset.
    /// Pass the previous response's `next_before_offset` to get the next page.
    #[serde(default)]
    pub before_offset: Option<i64>,
    /// When true, fetches fresh data from Canton and updates cache
    #[serde(default)]
    pub refresh: bool,
}

fn default_chain_audit_limit() -> usize {
    PAGE_SIZE as usize
}

/// Ceiling on a chain-audit page size.
///
/// `limit` arrives on the query string, so it is untrusted: left unbounded it
/// would drain the whole retained ledger into one response, and a value above
/// `i64::MAX` wraps negative when cast for SQLite — which reads a negative
/// `LIMIT` as no limit at all.
pub const MAX_CHAIN_AUDIT_LIMIT: usize = 1_000;

impl ChainAuditQuery {
    /// The requested page size, bounded by [`MAX_CHAIN_AUDIT_LIMIT`].
    pub fn clamped_limit(&self) -> usize {
        self.limit.min(MAX_CHAIN_AUDIT_LIMIT)
    }
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
            added_participant: None,
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

    // `proposal_transfer_rejects_non_positive_amount` moved to
    // `decman_lib::catalog::proposals::custody::tests` as
    // `transfer_rejects_non_positive_amount_and_zero_window`, testing
    // `Transfer::validate` directly.

    // `proposal_onboard_instrument_issuers_rejects_empty_issuer_list`,
    // `proposal_offboard_instrument_issuers_validates_rows`,
    // `proposal_onboard_instrument_issuers_rejects_duplicate_issuers`,
    // `proposal_provision_instrument_rejects_duplicate_initial_issuers`,
    // `proposal_create_provider_configuration_rejects_claimless_self_issued_requirement`,
    // and `proposal_provision_instrument_rejects_claimless_self_issued_requirement`
    // moved to `decman_lib::catalog::proposals::utility::tests` as
    // `onboard_instrument_issuers_rejects_empty_issuer_list`,
    // `offboard_instrument_issuers_validates_rows`,
    // `onboard_instrument_issuers_rejects_duplicate_issuers`,
    // `provision_instrument_rejects_duplicate_initial_issuers`,
    // `create_provider_configuration_rejects_claimless_self_issued_requirement`,
    // and `provision_instrument_rejects_claimless_self_issued_requirement`,
    // testing `OnboardInstrumentIssuers::validate` /
    // `OffboardInstrumentIssuers::validate` / `ProvisionInstrument::validate` /
    // `CreateProviderConfiguration::validate` directly.

    // `setup_delegation_validate` moved to
    // `decman_lib::catalog::proposals::rewards::tests`, testing
    // `SetupCouponReassignmentDelegation::validate` /
    // `RevokeCouponReassignmentDelegation::validate` directly.

    // `validate_reward_beneficiaries_edge_cases` moved to
    // `decman_lib::framework::validate::tests` with the validator itself.

    fn test_party(prefix: &str) -> anyhow::Result<CantonId> {
        CantonId::parse(&format!("{prefix}::1220{}", "ab".repeat(32)))
    }

    /// `/governance/confirmations` is deserialized by the integration-test
    /// harness, so the response has to survive a round trip in the shape the
    /// server actually emits — which omits every `skip_serializing_if` field.
    /// Without a matching `default` those come back as "missing field" errors.
    #[test]
    fn governance_response_round_trips_with_every_optional_field_omitted() -> anyhow::Result<()> {
        let response = GovernanceResponse {
            actions: vec![GovernanceAction {
                action_hash: "hash".to_owned(),
                action: ActionType::GovernanceSetThreshold { new_threshold: 2 },
                confirmations: vec![GovernanceConfirmation {
                    contract_id: "00conf".to_owned(),
                    action: ActionType::GovernanceSetThreshold { new_threshold: 2 },
                    confirming_party: test_party("m1")?,
                    created_at: 0,
                    expires_at: 0,
                }],
                confirmation_count: 1,
                can_execute: false,
                last_confirmation_at: 0,
            }],
            domain_actions: vec![DomainGovernanceAction {
                proposal_cid: "00prop".to_owned(),
                action_label: "SetThreshold".to_owned(),
                description: None,
                confirmations: Vec::new(),
                confirmation_count: 0,
                can_execute: false,
                orphaned: false,
                transfer_details: None,
                accept_transfer_details: None,
                service_request_details: None,
                proposer: None,
                created_at: None,
            }],
            threshold: 2,
            member_party_id: None,
            rules_contract_id: None,
            gov_core_out_of_date: false,
            gov_core_package_ref: None,
        };

        let json = serde_json::to_string(&response)?;
        assert!(
            !json.contains("member_party_id") && !json.contains("proposer"),
            "optional fields must be omitted on the wire: {json}"
        );

        let back: GovernanceResponse = serde_json::from_str(&json)?;
        assert_eq!(back.threshold, 2);
        assert_eq!(back.member_party_id, None);
        assert_eq!(
            back.domain_actions.first().map(|a| a.action_label.as_str()),
            Some("SetThreshold")
        );
        assert_eq!(
            back.actions
                .first()
                .and_then(|a| a.confirmations.first())
                .map(|c| c.confirming_party.clone()),
            Some(test_party("m1")?)
        );
        Ok(())
    }

    /// `ServiceRequestDetails` sets exactly one of `user` / `provider`; the
    /// unset one is omitted and must deserialize back as `None`.
    #[test]
    fn service_request_details_round_trips_with_one_side_unset() -> anyhow::Result<()> {
        let details = ServiceRequestDetails {
            operator: test_party("op")?,
            user: None,
            provider: Some(test_party("prov")?),
        };
        let json = serde_json::to_string(&details)?;
        let back: ServiceRequestDetails = serde_json::from_str(&json)?;
        assert_eq!(back.user, None);
        assert_eq!(back.provider, Some(test_party("prov")?));
        Ok(())
    }

    // `setup_minting_delegation_rejects_a_non_future_expiry` moved to
    // `decman_lib::catalog::proposals::rewards::tests`, testing
    // `SetupMintingDelegation::validate` directly with a fixed `now_micros`
    // in the `ValidationCtx` instead of the wall clock.
}
