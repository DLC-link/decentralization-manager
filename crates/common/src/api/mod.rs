//! HTTP wire DTOs for the `decman` server's REST API, shared with the
//! `decman-cli` client and used to generate the frontend's TypeScript types
//! (see the `gen-types` binary, which derives them from this module via ts-rs).
//!
//! Like [`crate::types`], these are pure data-transfer types: no server-only
//! dependencies. The OpenAPI (`utoipa`) derives are gated behind the `openapi`
//! feature so dependency-light clients don't inherit them.
//!
//! Types that carry on-chain decimal amounts (`DamlDecimal`) or feed Canton
//! ledger-command construction (e.g. `ActionType`, `ProposalType`) stay in the
//! `decman` crate for now — moving them would pull the heavy `canton_common`
//! dependency into this lightweight crate.

mod audit;
mod auth;
mod contracts;
#[cfg(test)]
mod fixtures;
mod governance;
mod invitations;
mod parties;
mod party_config;
mod registry;
mod tenant;
mod workflows;

pub use audit::{
    AuditLogResponse, ChainAuditEntry, ChainAuditResponse, ErrorResponse, MessageResponse,
    SuccessResponse, WorkflowStatusResponse,
};
pub use auth::{
    AuthStatus, AuthStatusResponse, AuthTestResponse, AuthTestResult, DiscoverMemberPartyRequest,
    DiscoverMemberPartyResponse, GrantRightsRequest, GrantRightsResponse, KnownMember,
    KnownMembersResponse, PartyAuthStatus, RightsStatus,
};
pub use contracts::{ContractDefinition, DarFile, FieldDefinition, PackageConfig};
pub use governance::{
    ActiveCouponReassignmentDelegation, CancelConfirmationRequest, CancelProposalRequest, Claim,
    CouponReassignmentDelegationSummary, DisclosedContractInput, ExpireConfirmationRequest,
    GovernanceState, GovernanceStateResponse, GovernanceType, InstrumentAllowance, InstrumentId,
    InstrumentIdentifier, InstrumentIssuerCredentials, PartyCredentialRequirement, RequiredClaim,
};
pub use invitations::{
    AddPartyInvitePayload, ChangeThresholdInvitePayload, ContractDeploymentIntent,
    ContractsInvitePayload, DarsInvitePayload, DeclineInvitationPayload,
    HandledInvitationsResponse, InvitationActionRequest, KeyStatusResponse, KickInvitePayload,
    OnboardingInvitePayload, PendingInvitationsResponse, RequestDarPayload,
};
pub use parties::{
    DecentralizedPartiesResponse, FormerPartiesResponse, FormerParty, ProposalSummary,
    ProposalsPageResponse, PurgeFormerPartyRequest, PurgePreview, PurgeStatus,
    ReconnectFormerPartyRequest, ResponseSource,
};
pub use party_config::{PartyConfigRequest, PartyConfigResponse};
pub use registry::{
    BlockListShardInfo, BlockListShardsResponse, ContractQueryResponse, ContractWithBlob,
    CredentialInfo, CredentialsResponse, InstrumentConfigurationFields, InstrumentInfo,
    InstrumentsResponse, NetworkInfo, OpenMiningRoundInfo, OperatorInfo, ProviderConfigurationInfo,
    ProviderConfigurationsResponse, ProviderServiceInfo, ProviderServicesResponse,
    RegistrarServiceInfo, RegistrarServiceRequestInfo, RegistrarServiceRequestsResponse,
    RegistrarServicesResponse, TransferFactoriesResponse, TransferFactoryInfo,
    TransferPreapprovalsResponse, UserServiceInfo, UserServicesResponse,
};
pub use tenant::{
    ExternalPartiesResponse, ExternalPartyHost, ExternalPartyInfo, ExternalPartyUnhostPreview,
    ExternalPartyUnhostRequest, ExternalPartyUnhostResponse, HostPermission,
    LocalPartyAdoptExecuteRequest, LocalPartyAdoptRequest, TenantAcsBlockResponse,
    TenantAcsImportRequest, TenantAcsImportResponse, TenantAddHostsExecuteRequest,
    TenantAddHostsExecuteResponse, TenantAddHostsPrepareResponse, TenantAddHostsRequest,
    TenantCallAuditEntry, TenantCallAuditResponse, TenantExecuteRequest, TenantExecuteResponse,
    TenantPartyStateResponse, TenantPrepareRequest, TenantPrepareResponse,
    TenantThresholdExecuteRequest, TenantThresholdRequest, TenantUnhostRequest, UnhostAuditEntry,
    UnhostAuditResponse,
};
pub use workflows::{
    AddPartyRequest, ChangeThresholdRequest, ContractsRequest, DarsRequest,
    DistributePackageRequest, KickRequest, MissingEdgeKind, MissingPeerEdge,
    OnboardingMeshErrorResponse, OnboardingRequest, WorkflowResponse, WorkflowRunsResponse,
};

/// Rows per page for every paginated list, on the wire and in the UI. Drives
/// the Canton `max_page_size` on ledger-API reads and the frontend's page size
/// (re-exported into `types.generated.ts` by `gen-types`, so the two cannot
/// drift). `i32` to match the ledger API's page-size fields.
pub const PAGE_SIZE: i32 = 25;
