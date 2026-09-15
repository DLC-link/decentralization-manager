use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::digitalasset::canton::protocol::v30::enums::ParticipantPermission;
use chrono::Utc;
use serde::{Deserialize, Serialize};

// Wire DTOs that the `decman-cli` client and the frontend also need live in the
// shared `common` crate. They are re-exported here so existing
// `crate::server::types::X` (and the glob `pub use types::*` in `server/mod.rs`)
// keep resolving unchanged. `common::api` holds the HTTP request/response DTOs
// the frontend's TypeScript is generated from (see `decman/build.rs`).
pub use common::api::PAGE_SIZE;
pub use common::api::{
    ActiveCouponReassignmentDelegation, AddPartyRequest, AuditLogResponse, AuthStatus,
    AuthStatusResponse, AuthTestResponse, AuthTestResult, CancelConfirmationRequest,
    CancelProposalRequest, ChainAuditEntry, ChainAuditResponse, ChangeThresholdRequest, Claim,
    ContractQueryResponse, ContractWithBlob, ContractsRequest, CouponReassignmentDelegationSummary,
    CredentialInfo, CredentialOfferInfo, CredentialOffersResponse, CredentialsResponse,
    DarsRequest, DecentralizedPartiesResponse, DisclosedContractInput, DiscoverMemberPartyRequest,
    DiscoverMemberPartyResponse, ErrorResponse, ExpireConfirmationRequest, ExternalPartiesResponse,
    ExternalPartyHost, ExternalPartyInfo, GovernanceState, GovernanceStateResponse, GovernanceType,
    GrantRightsRequest, GrantRightsResponse, InstrumentInfo, InstrumentsResponse,
    InvitationActionRequest, KickRequest, KnownMember, KnownMembersResponse,
    LocalPartyAdoptOnboardRequest, LocalPartyAdoptRequest, MessageResponse, NetworkInfo,
    OnboardingRequest, OperatorInfo, PartyAuthStatus, PartyConfigRequest, PartyConfigResponse,
    PendingInvitationsResponse, ProposalSummary, ProposalsPageResponse, ProviderConfigurationInfo,
    ProviderConfigurationsResponse, ProviderServiceInfo, ProviderServicesResponse,
    RegistrarServiceInfo, RegistrarServiceRequestInfo, RegistrarServiceRequestsResponse,
    RegistrarServicesResponse, ResponseSource, RightsStatus, SuccessResponse,
    TenantAcsBlockResponse, TenantAcsImportRequest, TenantAcsImportResponse,
    TenantAddHostsOnboardRequest, TenantAddHostsOnboardResponse, TenantAddHostsPrepareResponse,
    TenantAddHostsRequest, TenantOnboardRequest, TenantOnboardResponse, TenantPartyStateResponse,
    TenantPrepareRequest, TenantPrepareResponse, TenantThresholdOnboardRequest,
    TenantThresholdRequest, TransferFactoriesResponse, TransferFactoryInfo,
    TransferPreapprovalsResponse, UserServiceInfo, UserServicesResponse, WorkflowResponse,
    WorkflowRunsResponse, WorkflowStatusResponse,
};
pub use common::types::{
    AcsTransferProgress, AuditLogEntry, AuthConfigResponse, ConnectionStatus, ContractInfo,
    DecentralizedParty, InvitationType, MemberVariant, PackageInfo, ParticipantInfo,
    ParticipantStatus, ParticipantsStatusResponse, PartyMetadata, PeerErrorKind,
    PeerPackageComparison, PeerPackageResult, PendingInvitation, Permission, VettedPackageInfo,
    WorkflowKind, WorkflowProgress, WorkflowRole, WorkflowRun,
};
pub use decman_lib::catalog::types::{
    AcceptTransferDetails, AppRewardBeneficiary, BillingParams, ServiceRequestDetails,
    TransferProposalDetails,
};

use crate::canton_id::CantonId;

/// Liveness response for the `/healthz` ping endpoint. The body is
/// intentionally tiny: the frontend uses it to time its own round-trip to
/// this node, so the handler does no work beyond returning this.
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

// ============================================================================
// Governance Types (Structured Actions)
// ============================================================================

pub use decman_lib::catalog::action::ActionType;

/// Types of governance domain action proposals
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, utoipa::ToSchema)]
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
    /// Self-grant a `FeaturedAppRight` to the governance party on DevNet by
    /// exercising `AmuletRules_DevNet_FeatureApp`.
    RequestDevNetFeaturedAppRight(
        decman_lib::catalog::proposals::utility::RequestDevNetFeaturedAppRight,
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
    /// (`decman_lib::framework::validate::validate_future_micros`) reads the
    /// clock. Re-using this to
    /// re-validate an already-stored proposal would reject it for nothing but
    /// having aged, so a new call site needs to split the time-dependent arms
    /// out first.
    pub fn validate(&self, governance_party: &CantonId) -> Result<(), String> {
        let ctx = decman_lib::framework::ValidationCtx {
            governance_party,
            now_micros: Utc::now().timestamp_micros(),
        };
        let payload: &dyn decman_lib::framework::Validate = match self {
            Self::SetupCcPreapproval(p) => p,
            Self::SetupTokenPreapproval(p) => p,
            Self::Transfer(p) => p,
            Self::AcceptTransfer(p) => p,
            Self::GenericVote(p) => p,
            Self::ProvisionProviderService(p) => p,
            Self::SetupUtility(p) => p,
            Self::CreateProviderServiceRequest(p) => p,
            Self::CreateUserServiceRequest(p) => p,
            Self::SetProviderAppRewardBeneficiaries(p) => p,
            Self::SetupCouponReassignmentDelegation(p) => p,
            Self::RevokeCouponReassignmentDelegation(p) => p,
            Self::SetEnableResultContracts(p) => p,
            Self::CreateDelegatedBatchedMarkersProxy(p) => p,
            Self::RequestDevNetFeaturedAppRight(p) => p,
            Self::SetupMintingDelegation(p) => p,
            Self::AcceptExternalPartySetup(p) => p,
            Self::Mint(p) => p,
            Self::OfferFreeCredential(p) => p,
            Self::OfferPaidCredential(p) => p,
            Self::AcceptFreeCredential(p) => p,
            Self::Burn(p) => p,
            Self::AcceptMintRequest(p) => p,
            Self::AcceptBurnRequest(p) => p,
            Self::CreateProviderConfiguration(p) => p,
            Self::CreateRegistrarServiceRequest(p) => p,
            Self::OnboardRegistrar(p) => p,
            Self::ProvisionInstrument(p) => p,
            Self::OnboardInstrumentIssuers(p) => p,
            Self::OffboardInstrumentIssuers(p) => p,
        };
        payload.validate(&ctx).map_err(|e| e.to_string())
    }

    /// The generic propose payload — `None` for the two transfer variants,
    /// which need runtime context (the registry choice context, the validity
    /// window, and the on-chain sender party) and so go through their
    /// wrapper structs (`TransferWithContext` /
    /// `AcceptTransferWithContext`) rather than the payload itself.
    pub fn grpc_payload(&self) -> Option<&dyn decman_lib::framework::GrpcPayload> {
        match self {
            Self::Transfer(_) | Self::AcceptTransfer(_) => None,
            Self::SetupCcPreapproval(p) => Some(p),
            Self::SetupTokenPreapproval(p) => Some(p),
            Self::GenericVote(p) => Some(p),
            Self::ProvisionProviderService(p) => Some(p),
            Self::SetupUtility(p) => Some(p),
            Self::CreateProviderServiceRequest(p) => Some(p),
            Self::CreateUserServiceRequest(p) => Some(p),
            Self::SetProviderAppRewardBeneficiaries(p) => Some(p),
            Self::SetupCouponReassignmentDelegation(p) => Some(p),
            Self::RevokeCouponReassignmentDelegation(p) => Some(p),
            Self::SetEnableResultContracts(p) => Some(p),
            Self::CreateDelegatedBatchedMarkersProxy(p) => Some(p),
            Self::RequestDevNetFeaturedAppRight(p) => Some(p),
            Self::SetupMintingDelegation(p) => Some(p),
            Self::AcceptExternalPartySetup(p) => Some(p),
            Self::Mint(p) => Some(p),
            Self::OfferFreeCredential(p) => Some(p),
            Self::OfferPaidCredential(p) => Some(p),
            Self::AcceptFreeCredential(p) => Some(p),
            Self::Burn(p) => Some(p),
            Self::AcceptMintRequest(p) => Some(p),
            Self::AcceptBurnRequest(p) => Some(p),
            Self::CreateProviderConfiguration(p) => Some(p),
            Self::CreateRegistrarServiceRequest(p) => Some(p),
            Self::OnboardRegistrar(p) => Some(p),
            Self::ProvisionInstrument(p) => Some(p),
            Self::OnboardInstrumentIssuers(p) => Some(p),
            Self::OffboardInstrumentIssuers(p) => Some(p),
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
    pub confirmations: Vec<DomainConfirmation>,
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

/// A single confirmation of a domain-action proposal (governance-core
/// `Governance.Confirmation`). Unlike [`GovernanceConfirmation`] (which
/// backs core-self-management confirmations, each carrying its own
/// real inline `action`), the on-chain `Confirmation` contract carries no
/// action at all — only `actionProposalCid` and `actionLabel`, surfaced at
/// the parent [`DomainGovernanceAction`] level. There is no meaningful
/// per-confirmation action to serialize, so this type has no `action` field
/// rather than papering over the gap with a placeholder.
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DomainConfirmation {
    pub contract_id: String,
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
    /// Opaque resume token for the next batch of proposals. Absent when the
    /// party has no more. Hand it back as `cursor` to continue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Pending domain action proposals (governance-core GovernableAction)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domain_actions: Vec<DomainGovernanceAction>,
    pub threshold: usize,
    /// The member party ID for the requesting party (used to identify own confirmations)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_party_id: Option<CantonId>,
    /// Current contract id of the active GovernanceRules contract for this
    /// party. The choice exercised when confirming an action
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

/// Which ledger events a chain-audit read returns.
///
/// `Governance` filters Canton-side to the governance packages and keeps only
/// proposals, confirmations, executions and their outcomes. `All` drops both
/// filters and returns every event the party witnesses, so a party whose
/// activity lives in its own application packages — an app or oracle party
/// that never deploys governance contracts — is not reported as having done
/// nothing.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AuditScope {
    #[default]
    Governance,
    All,
}

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
    /// Which events to return: `governance` (default) or `all`.
    #[serde(default)]
    pub scope: AuditScope,
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
    use common::api::InstrumentId;
    use common::types::AcsTransferDirection;
    use decman_lib::catalog::proposals::core::GenericVote;
    use decman_lib::catalog::proposals::credential::{
        AcceptFreeCredential, OfferFreeCredential, OfferPaidCredential,
    };
    use decman_lib::catalog::proposals::custody::{
        AcceptTransfer, SetupCcPreapproval, SetupTokenPreapproval, Transfer,
    };
    use decman_lib::catalog::proposals::rewards::{
        AcceptExternalPartySetup, RevokeCouponReassignmentDelegation,
        SetupCouponReassignmentDelegation, SetupMintingDelegation,
    };
    use decman_lib::catalog::proposals::utility::{
        AcceptBurnRequest, AcceptMintRequest, Burn, CreateDelegatedBatchedMarkersProxy,
        CreateProviderConfiguration, CreateProviderServiceRequest, CreateRegistrarServiceRequest,
        CreateUserServiceRequest, Mint, OffboardInstrumentIssuers, OnboardInstrumentIssuers,
        OnboardRegistrar, ProvisionInstrument, ProvisionProviderService,
        RequestDevNetFeaturedAppRight, SetEnableResultContracts, SetProviderAppRewardBeneficiaries,
        SetupUtility,
    };
    use serde_json::Value;

    use super::*;
    use crate::error::Result;

    /// The card reads these field names and the lowercase direction straight
    /// off the wire, and `acs_progress` must vanish rather than appear as
    /// `null` when nothing is transferring, or every idle run renders a meter.
    #[test]
    fn acs_transfer_progress_wire_shape_is_stable() -> Result {
        let progress = AcsTransferProgress {
            direction: AcsTransferDirection::Import,
            bytes: 1_369_579_398,
            block: 1306,
            started_at_ms: 1_788_968_788_000,
            updated_at_ms: 1_788_969_093_000,
        };
        let json = serde_json::to_value(&progress)?;

        assert_eq!(
            json.get("direction").and_then(Value::as_str),
            Some("import")
        );
        assert_eq!(
            json.get("bytes").and_then(Value::as_i64),
            Some(1_369_579_398)
        );
        assert_eq!(json.get("block").and_then(Value::as_i64), Some(1306));
        assert_eq!(
            json.get("started_at_ms").and_then(Value::as_i64),
            Some(1_788_968_788_000)
        );
        assert_eq!(
            json.get("updated_at_ms").and_then(Value::as_i64),
            Some(1_788_969_093_000)
        );

        let exported = serde_json::to_value(AcsTransferProgress {
            direction: AcsTransferDirection::Export,
            ..progress
        })?;
        assert_eq!(
            exported.get("direction").and_then(Value::as_str),
            Some("export")
        );
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
            coordinator_participant: None,
            coordinator_party: None,
            proposal_cid: None,
            member_variant: None,
            topology_hashes: Default::default(),
            coordinator_instance: None,
            coordinator_name: None,
            expected_peers: vec![peer_a.clone(), peer_b.clone()],
            completed_peers: vec![peer_a],
            connected_peers: vec![peer_b],
            acs_progress: None,
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

        let connected = json
            .get("connected_peers")
            .and_then(Value::as_array)
            .expect("connected_peers must be a JSON array");
        assert_eq!(connected.len(), 1);
        assert!(
            connected[0].is_string(),
            "connected_peers entry must be a string, got {}",
            connected[0]
        );

        // dec_party_id (Option<CantonId>) must serialize as a plain string,
        // not as a nested object with prefix/namespace fields.
        let dec_party = json.get("dec_party_id").expect("dec_party_id key present");
        assert!(
            dec_party.is_string(),
            "dec_party_id must be a JSON string when set, got {dec_party}"
        );
        assert_eq!(dec_party.as_str().unwrap(), dec_party_id_str);
    }

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
            next_cursor: None,
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

    /// A domain-action confirmation (`DomainConfirmation`, backing
    /// governance-core `Confirmation` contracts) has no real inline action —
    /// only the parent `DomainGovernanceAction.action_label` describes it —
    /// so it must never serialize an `"action"` key at all, placeholder or
    /// otherwise. The sibling self-management confirmation
    /// (`GovernanceConfirmation`) genuinely does carry its own action and
    /// must keep serializing the real one.
    #[test]
    fn domain_confirmation_omits_action_self_confirmation_keeps_it() -> anyhow::Result<()> {
        let response = GovernanceResponse {
            next_cursor: None,
            actions: vec![GovernanceAction {
                action_hash: "hash".to_owned(),
                action: ActionType::GovernanceSetThreshold { new_threshold: 7 },
                confirmations: vec![GovernanceConfirmation {
                    contract_id: "self-conf".to_owned(),
                    action: ActionType::GovernanceSetThreshold { new_threshold: 7 },
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
                action_label: "WithdrawPending".to_owned(),
                description: None,
                confirmations: vec![DomainConfirmation {
                    contract_id: "domain-conf".to_owned(),
                    confirming_party: test_party("m2")?,
                    created_at: 0,
                    expires_at: 0,
                }],
                confirmation_count: 1,
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

        let value = serde_json::to_value(&response)?;

        let domain_confirmation = &value["domain_actions"][0]["confirmations"][0];
        assert!(
            domain_confirmation.get("action").is_none(),
            "domain confirmation must not carry an action field: {domain_confirmation}"
        );
        // Sanity: not just an empty object — the fields we do expect are there.
        assert_eq!(domain_confirmation["contract_id"], "domain-conf");

        let self_confirmation = &value["actions"][0]["confirmations"][0];
        assert_eq!(
            self_confirmation["action"]["type"], "governance_set_threshold",
            "self-management confirmation must keep its real action: {self_confirmation}"
        );
        assert_eq!(self_confirmation["action"]["new_threshold"], 7);

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

    /// One minimal instance per `ProposalType` variant, in declaration order.
    /// Field values are placeholders — this only exercises which variants
    /// `grpc_payload` treats as `Some`/`None`, not the payload contents (see
    /// `serde_snapshots.rs` for the fully-populated wire-shape fixtures).
    fn one_of_each_proposal_type() -> Vec<ProposalType> {
        let instrument_id = || InstrumentId {
            admin: "admin-party".into(),
            id: "TOK".into(),
        };
        vec![
            ProposalType::SetupCcPreapproval(SetupCcPreapproval {
                provider: test_party("prov").unwrap(),
                expected_dso: test_party("dso").unwrap(),
            }),
            ProposalType::SetupTokenPreapproval(SetupTokenPreapproval {
                operator: test_party("op").unwrap(),
                instrument_admin: test_party("iadmin").unwrap(),
                instrument_allowances: vec![],
            }),
            ProposalType::Transfer(Transfer {
                transfer_factory_cid: "00tf".into(),
                expected_admin: test_party("iadmin").unwrap(),
                receiver: test_party("recv").unwrap(),
                amount: "1".parse().unwrap(),
                instrument_id: instrument_id(),
                input_holding_cids: vec![],
                validity_window_hours: None,
            }),
            ProposalType::AcceptTransfer(AcceptTransfer {
                transfer_instruction_cid: "00ti".into(),
            }),
            ProposalType::GenericVote(GenericVote {
                description: "a vote".into(),
            }),
            ProposalType::ProvisionProviderService(ProvisionProviderService {}),
            ProposalType::SetupUtility(SetupUtility {
                provider_service_cid: "00psc".into(),
                operator: test_party("op").unwrap(),
                instrument_id_text: "uuid-1".into(),
                additional_identifiers: vec![],
                create_transfer_rule: true,
                create_allocation_factory: true,
            }),
            ProposalType::CreateProviderServiceRequest(CreateProviderServiceRequest {
                operator: test_party("op").unwrap(),
                provider: test_party("prov").unwrap(),
            }),
            ProposalType::CreateUserServiceRequest(CreateUserServiceRequest {
                operator: test_party("op").unwrap(),
                user: test_party("user").unwrap(),
            }),
            ProposalType::SetProviderAppRewardBeneficiaries(SetProviderAppRewardBeneficiaries {
                instrument_configuration_cid: "00icc".into(),
                provider_app_reward_beneficiaries: None,
            }),
            ProposalType::SetupCouponReassignmentDelegation(SetupCouponReassignmentDelegation {
                dso: test_party("dso").unwrap(),
                assigners: vec![],
                new_beneficiaries: vec![],
                prior_delegation: None,
            }),
            ProposalType::RevokeCouponReassignmentDelegation(RevokeCouponReassignmentDelegation {
                delegation: "00deleg".into(),
            }),
            ProposalType::SetEnableResultContracts(SetEnableResultContracts {
                registrar_service_cid: "00rsc".into(),
                enable_result_contracts: None,
            }),
            ProposalType::CreateDelegatedBatchedMarkersProxy(CreateDelegatedBatchedMarkersProxy {
                operator: test_party("op").unwrap(),
            }),
            ProposalType::RequestDevNetFeaturedAppRight(RequestDevNetFeaturedAppRight {
                amulet_rules_cid: "00amulet".into(),
            }),
            ProposalType::SetupMintingDelegation(SetupMintingDelegation {
                delegate: test_party("delegate").unwrap(),
                dso: test_party("dso").unwrap(),
                expires_at_micros: 4_000_000_000_000_000,
                amulet_merge_limit: 10,
                description: "delegate minting".into(),
            }),
            ProposalType::AcceptExternalPartySetup(AcceptExternalPartySetup {
                proposal_cid: "00eps".into(),
            }),
            ProposalType::Mint(Mint {
                allocation_factory_cid: "00alloc".into(),
                instrument_id: instrument_id(),
                instrument_configuration_cid: "00icc".into(),
                recipient: test_party("recv").unwrap(),
                amount: "5".parse().unwrap(),
                description: "mint".into(),
            }),
            ProposalType::OfferFreeCredential(OfferFreeCredential {
                user_service_cid: "00usc".into(),
                holder: test_party("holder").unwrap(),
                id: "cred-1".into(),
                description: "free cred".into(),
                claims: vec![],
            }),
            ProposalType::OfferPaidCredential(OfferPaidCredential {
                user_service_cid: "00usc".into(),
                holder: test_party("holder").unwrap(),
                id: "cred-2".into(),
                description: "paid cred".into(),
                claims: vec![],
                billing_params: BillingParams {
                    fee_per_day_usd: "1.5".parse().unwrap(),
                    billing_period_minutes: 60,
                    deposit_target_amount_usd: "30".parse().unwrap(),
                    holder_activity_weight: None,
                },
                deposit_initial_amount_usd: None,
            }),
            ProposalType::AcceptFreeCredential(AcceptFreeCredential {
                user_service_cid: "00usc".into(),
                credential_offer_cid: "00offer".into(),
            }),
            ProposalType::Burn(Burn {
                allocation_factory_cid: "00alloc".into(),
                instrument_id: instrument_id(),
                instrument_configuration_cid: "00icc".into(),
                holder: test_party("holder").unwrap(),
                amount: "3".parse().unwrap(),
                description: "burn".into(),
            }),
            ProposalType::AcceptMintRequest(AcceptMintRequest {
                mint_request_cid: "00mr".into(),
                instrument_configuration_cid: "00icc".into(),
                issuer_credential_cids: vec![],
                description: "accept mint".into(),
            }),
            ProposalType::AcceptBurnRequest(AcceptBurnRequest {
                burn_request_cid: "00br".into(),
                instrument_configuration_cid: "00icc".into(),
                issuer_credential_cids: vec![],
                description: "accept burn".into(),
            }),
            ProposalType::CreateProviderConfiguration(CreateProviderConfiguration {
                provider_service_cid: "00psc".into(),
                registrar_requirements: vec![],
                holder_requirements: vec![],
            }),
            ProposalType::CreateRegistrarServiceRequest(CreateRegistrarServiceRequest {
                operator: test_party("op").unwrap(),
                provider: test_party("prov").unwrap(),
                create_transfer_rule: false,
                create_allocation_factory: true,
            }),
            ProposalType::OnboardRegistrar(OnboardRegistrar {
                provider_service_cid: "00psc".into(),
                registrar_service_request_cid: "00rsr".into(),
                provider_configuration_cid: "00pcc".into(),
            }),
            ProposalType::ProvisionInstrument(ProvisionInstrument {
                registrar_service_cid: "00rsc".into(),
                instrument_id_text: "uuid-2".into(),
                additional_identifiers: vec![],
                issuer_requirements: vec![],
                holder_requirements: vec![],
                initial_instrument_issuers: vec![],
            }),
            ProposalType::OnboardInstrumentIssuers(OnboardInstrumentIssuers {
                instrument_configuration_cid: "00icc".into(),
                instrument_issuers: vec![],
            }),
            ProposalType::OffboardInstrumentIssuers(OffboardInstrumentIssuers {
                instrument_issuers: vec![],
            }),
        ]
    }

    /// Guards the split the payload-projection macro used to generate: every
    /// `ProposalType` variant carries a `GrpcPayload` except `Transfer` and
    /// `AcceptTransfer`, which need runtime context and go through wrapper
    /// structs instead (see `grpc_payload`'s doc comment). A variant landing
    /// in the wrong match arm — Transfer wrongly returning `Some`, or a
    /// plain variant wrongly returning `None` — fails this test instead of
    /// surfacing as a silent gap deep in `propose_action`.
    #[test]
    fn grpc_payload_is_none_only_for_transfer_and_accept_transfer() {
        for proposal in one_of_each_proposal_type() {
            let is_transfer_variant = matches!(
                proposal,
                ProposalType::Transfer(_) | ProposalType::AcceptTransfer(_)
            );
            assert_eq!(
                proposal.grpc_payload().is_none(),
                is_transfer_variant,
                "{proposal:?}: grpc_payload() Some/None must match transfer-variant status"
            );
        }
    }
}
