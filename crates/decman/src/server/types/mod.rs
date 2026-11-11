//! HTTP wire types: the shared DTOs re-exported from `common`, and the server-only types by domain.

// Wire DTOs that the `decman-cli` client and the frontend also need live in the
// shared `common` crate. They are re-exported here so existing
// `crate::server::types::X` (and the glob `pub use types::*` in `server/mod.rs`)
// keep resolving unchanged. `common::api` holds the HTTP request/response DTOs
// the frontend's TypeScript is generated from (see `decman/build.rs`).
pub use common::api::{
    ActiveCouponReassignmentDelegation, AddPartyInvitePayload, AddPartyRequest, AuditLogResponse,
    AuthStatus, AuthStatusResponse, AuthTestResponse, AuthTestResult, BlockListShardInfo,
    BlockListShardsResponse, CancelConfirmationRequest, CancelProposalRequest, ChainAuditEntry,
    ChainAuditResponse, ChangeThresholdInvitePayload, ChangeThresholdRequest, Claim,
    ContractQueryResponse, ContractWithBlob, ContractsInvitePayload, ContractsRequest,
    CouponReassignmentDelegationSummary, CredentialInfo, CredentialsResponse, DarsInvitePayload,
    DarsRequest, DecentralizedPartiesResponse, DeclineInvitationPayload,
    DiscoverMemberPartyRequest, DiscoverMemberPartyResponse, DistributePackageRequest,
    ErrorResponse, ExpireConfirmationRequest, ExternalPartiesResponse, ExternalPartyHost,
    ExternalPartyInfo, ExternalPartyUnhostPreview, ExternalPartyUnhostRequest,
    ExternalPartyUnhostResponse, FormerPartiesResponse, FormerParty, GovernanceState,
    GovernanceStateResponse, GovernanceType, GrantRightsRequest, GrantRightsResponse,
    HandledInvitationsResponse, InstrumentInfo, InstrumentsResponse, InvitationActionRequest,
    KeyStatusResponse, KickInvitePayload, KickRequest, KnownMember, KnownMembersResponse,
    LocalPartyAdoptExecuteRequest, LocalPartyAdoptRequest, MessageResponse, MissingEdgeKind,
    MissingPeerEdge, NetworkInfo, OnboardingInvitePayload, OnboardingMeshErrorResponse,
    OnboardingRequest, OpenMiningRoundInfo, OperatorInfo, PartyAuthStatus, PartyConfigRequest,
    PartyConfigResponse, PendingInvitationsResponse, ProposalSummary, ProposalsPageResponse,
    ProviderConfigurationInfo, ProviderConfigurationsResponse, ProviderServiceInfo,
    ProviderServicesResponse, PurgeFormerPartyRequest, PurgePreview, PurgeStatus,
    ReconnectFormerPartyRequest, RegistrarServiceInfo, RegistrarServiceRequestInfo,
    RegistrarServiceRequestsResponse, RegistrarServicesResponse, RequestDarPayload, ResponseSource,
    RightsStatus, SuccessResponse, TenantAcsBlockResponse, TenantAcsImportRequest,
    TenantAcsImportResponse, TenantAddHostsExecuteRequest, TenantAddHostsExecuteResponse,
    TenantAddHostsPrepareResponse, TenantAddHostsRequest, TenantCallAuditResponse,
    TenantExecuteRequest, TenantExecuteResponse, TenantPartyStateResponse, TenantPrepareRequest,
    TenantPrepareResponse, TenantThresholdExecuteRequest, TenantThresholdRequest,
    TenantUnhostRequest, TransferFactoriesResponse, TransferFactoryInfo,
    TransferPreapprovalsResponse, UnhostAuditResponse, UserServiceInfo, UserServicesResponse,
    WorkflowResponse, WorkflowRunsResponse, WorkflowStatusResponse,
};
pub use common::types::{
    AcsTransferProgress, AuditLogEntry, AuthConfigResponse, ConnectionStatus, ContractInfo,
    DecentralizedParty, ExpectedPackageVersion, ExpectedVersionsResponse, HandledInvitation,
    InvitationOutcome, InvitationType, NonHostingOwner, PackageInfo, ParticipantInfo,
    ParticipantStatus, ParticipantsStatusResponse, PartyMetadata, PeerErrorKind,
    PeerPackageComparison, PeerPackageResult, PendingInvitation, Permission, VettedPackageInfo,
    WorkflowKind, WorkflowProgress, WorkflowRole, WorkflowRun,
};
pub use decman_lib::catalog::types::{
    AcceptTransferDetails, AppRewardBeneficiary, BillingParams, RewardBeneficiary,
    ServiceRequestDetails, TransferProposalDetails,
};

pub use decman_lib::catalog::action::ActionType;

mod governance;
mod node;
mod proposal;
#[cfg(test)]
mod test_support;
mod token_standard;

pub use super::workflow_registry::{PeerJob, WorkflowGuard, WorkflowInstance, WorkflowRegistry};
pub use governance::{
    AuditLogQuery, AuditScope, ChainAuditQuery, ConfirmActionRequest, DomainConfirmation,
    DomainGovernanceAction, ExecuteActionRequest, GovernanceAction, GovernanceConfirmation,
    GovernanceResponse, MAX_RECENT_AUDIT_LIMIT, ProposeActionRequest, RewardStatusResponse,
    chain_audit_entry_from_row,
};
pub use node::{
    KickResponse, KickStatus, LivenessResponse, OnboardingResponse, OnboardingStatus,
    permission_from_proto,
};
pub use proposal::ProposalType;
pub use token_standard::{
    BurnRequestsResponse, CredentialOfferInfo, CredentialOffersResponse, HoldingInfo,
    HoldingsResponse, MintRequestsResponse, PendingAction, TokenRequestInfo,
    TransferInstructionInfo, TransferInstructionStatus, TransferInstructionsResponse,
};
