//! The decman HTTP API client: wire types, display helpers, the HTTP core,
//! login and one method per route.

mod client;
mod display;
#[cfg(test)]
mod fixtures;
mod login;
mod routes;
mod types;

pub use client::{ApiError, AuthSettings, DecmanClient, for_terminal};
pub use display::run_name;
pub use display::{PeerView, audit_action, invitation_name, mesh_error_hint, party_name};
pub use types::{AuthStatusKind, ChainAuditEntry, DiscoverResult, DomainGovAction};
pub use types::{ExecuteParams, FeedItem, GovAction, GovConfirmation, GovState};
pub use types::{GovernanceConfirmations, Holding, KeyStatus, KnownMember, NetworkInfo};
pub use types::{PartyAuthStatus, PartyConfigView, PeerEntry};

#[cfg(test)]
pub use types::AuthRights;
