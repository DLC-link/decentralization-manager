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

pub use audit::{AuditLogResponse, ChainAuditEntry, ChainAuditResponse, ErrorResponse};
pub use audit::{MessageResponse, SuccessResponse, WorkflowStatusResponse};
pub use auth::RightsStatus;
pub use auth::{AuthStatus, AuthStatusResponse, AuthTestResponse, AuthTestResult};
pub use auth::{DiscoverMemberPartyRequest, DiscoverMemberPartyResponse, GrantRightsRequest};
pub use auth::{GrantRightsResponse, KnownMember, KnownMembersResponse, PartyAuthStatus};
pub use contracts::{ContractDefinition, DarFile, FieldDefinition, PackageConfig};
pub use governance::{ActiveCouponReassignmentDelegation, CancelConfirmationRequest};
pub use governance::{CancelProposalRequest, Claim, CouponReassignmentDelegationSummary};
pub use governance::{DisclosedContractInput, ExpireConfirmationRequest, GovernanceState};
pub use governance::{GovernanceStateResponse, GovernanceType, InstrumentAllowance};
pub use governance::{InstrumentId, InstrumentIdentifier, InstrumentIssuerCredentials};
pub use governance::{PartyCredentialRequirement, RequiredClaim};
pub use invitations::{AddPartyInvitePayload, ChangeThresholdInvitePayload};
pub use invitations::{ContractDeploymentIntent, ContractsInvitePayload, DarsInvitePayload};
pub use invitations::{DeclineInvitationPayload, HandledInvitationsResponse};
pub use invitations::{InvitationActionRequest, KeyStatusResponse, KickInvitePayload};
pub use invitations::{OnboardingInvitePayload, PendingInvitationsResponse, RequestDarPayload};
pub use parties::{DecentralizedPartiesResponse, FormerPartiesResponse, FormerParty};
pub use parties::{ProposalSummary, ProposalsPageResponse, PurgeFormerPartyRequest};
pub use parties::{PurgePreview, PurgeStatus, ReconnectFormerPartyRequest, ResponseSource};
pub use party_config::{PartyConfigRequest, PartyConfigResponse};
pub use registry::{BlockListShardInfo, BlockListShardsResponse, ContractQueryResponse};
pub use registry::{ContractWithBlob, CredentialInfo, CredentialsResponse};
pub use registry::{InstrumentConfigurationFields, InstrumentInfo, InstrumentsResponse};
pub use registry::{NetworkInfo, OpenMiningRoundInfo, OperatorInfo, ProviderConfigurationInfo};
pub use registry::{ProviderConfigurationsResponse, ProviderServiceInfo};
pub use registry::{ProviderServicesResponse, RegistrarServiceInfo, RegistrarServiceRequestInfo};
pub use registry::{RegistrarServiceRequestsResponse, RegistrarServicesResponse};
pub use registry::{TransferFactoriesResponse, TransferFactoryInfo};
pub use registry::{TransferPreapprovalsResponse, UserServiceInfo, UserServicesResponse};
pub use tenant::UnhostAuditResponse;
pub use tenant::{ExternalPartiesResponse, ExternalPartyHost, ExternalPartyInfo};
pub use tenant::{ExternalPartyUnhostPreview, ExternalPartyUnhostRequest};
pub use tenant::{ExternalPartyUnhostResponse, HostPermission, LocalPartyAdoptExecuteRequest};
pub use tenant::{LocalPartyAdoptRequest, TenantAcsBlockResponse, TenantAcsImportRequest};
pub use tenant::{TenantAcsImportResponse, TenantAddHostsExecuteRequest};
pub use tenant::{TenantAddHostsExecuteResponse, TenantAddHostsPrepareResponse};
pub use tenant::{TenantAddHostsRequest, TenantCallAuditEntry, TenantCallAuditResponse};
pub use tenant::{TenantExecuteRequest, TenantExecuteResponse, TenantPartyStateResponse};
pub use tenant::{TenantPrepareRequest, TenantPrepareResponse, TenantThresholdExecuteRequest};
pub use tenant::{TenantThresholdRequest, TenantUnhostRequest, UnhostAuditEntry};
pub use workflows::WorkflowRunsResponse;
pub use workflows::{AddPartyRequest, ChangeThresholdRequest, ContractsRequest, DarsRequest};
pub use workflows::{DistributePackageRequest, KickRequest, MissingEdgeKind, MissingPeerEdge};
pub use workflows::{OnboardingMeshErrorResponse, OnboardingRequest, WorkflowResponse};

/// Rows per page for every paginated list, on the wire and in the UI. Drives
/// the Canton `max_page_size` on ledger-API reads and the frontend's page size
/// (re-exported into `types.generated.ts` by `gen-types`, so the two cannot
/// drift). `i32` to match the ledger API's page-size fields.
pub const PAGE_SIZE: i32 = 25;
