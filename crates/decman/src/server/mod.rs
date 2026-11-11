//! HTTP server and always-on Noise listener.
//!
//! Builds the actix-web application (REST API + embedded React UI; a Swagger UI
//! is mounted only when the node runs in insecure mode, i.e. `--insecure`),
//! wires shared [`AppState`], and runs the long-lived Noise
//! listener that
//! handles inbound peer messages (invites, signing, health, cancellation)
//! independently of any coordinator-driven workflow.

mod assets;
mod audit;
mod chain_audit;
mod error;
mod event_filters;
mod external_parties;
mod former_parties;
mod governance_poller;
mod handlers;
pub(crate) mod ledger_paging;
mod middleware;
mod node_health;
mod owner_keys;
mod package_inventory;
mod peer_jobs;
mod peer_listener;
mod queries;
mod record;
mod recovery;
mod reward_automation;
mod scan;
mod startup;
mod state;
pub(crate) mod tenant_call_audit;
mod tenant_rate_limit;
mod transfer_context;
mod types;
mod workflow_registry;

#[cfg(test)]
mod peer_failure_tests;
#[cfg(test)]
mod serde_snapshots;

pub(crate) mod health;
pub(crate) mod peer_status;

pub(crate) use package_inventory::fetch_vetted_package_ids;
pub(crate) use queries::compare_versions;

// Reached externally as `dec_party_manager::server::NodeConfigResponse` by the
// `gen-types` binary (a separate crate from this lib), so it must stay `pub`.
pub use handlers::NodeConfigResponse;
pub(crate) use handlers::{CoordinatorRequest, mark_run_failed};
pub(crate) use types::*;
// These wire DTOs are likewise reached externally as `dec_party_manager::server::…`,
// by `gen-types` (TS generation) and, for `GovernanceResponse`, by the integration
// tests under `tests/` — both are separate crates that can only see `pub` items.
pub(crate) use node_health::HealthCache;
pub use node_health::{
    ComponentHealth, ComponentState, LinkHealth, NodeHealthResponse, NodeHealthStatus,
    ParticipantHealth, SynchronizerHealth, TopologyQueues,
};
pub(crate) use recovery::{mark_failed_via_pool, refire_peer, respawn_coordinator};
pub use startup::{metrics_server, start_server};
pub use state::AppState;
pub use types::{
    AcceptTransferDetails, ActionType, AppRewardBeneficiary, BillingParams, BurnRequestsResponse,
    ConfirmActionRequest, CredentialOfferInfo, CredentialOffersResponse, DomainConfirmation,
    DomainGovernanceAction, ExecuteActionRequest, GovernanceAction, GovernanceConfirmation,
    GovernanceResponse, HoldingInfo, HoldingsResponse, MintRequestsResponse, PendingAction,
    ProposalType, ProposeActionRequest, RewardStatusResponse, ServiceRequestDetails,
    TokenRequestInfo, TransferInstructionInfo, TransferInstructionStatus,
    TransferInstructionsResponse, TransferProposalDetails,
};
