//! Per-event parses over a Ledger API `CreatedEvent`: governance
//! confirmations, governance-rules state, and `GovernableAction` proposal
//! info.
//!
//! Each function here is a pure, single-event parse — no grouping, no map
//! insertion, no I/O. DecMan's `queries.rs` owns its own copies that
//! additionally group results into `HashMap`s and (for the on-chain action
//! hash) call `compute_action_hash`, which stays a decman concern.

mod confirmations;
#[cfg(test)]
mod fixtures;
mod proposals;
mod tally;

pub use confirmations::parse_domain_confirmation;
pub use confirmations::{ParsedConfirmation, ParsedDomainConfirmation, RulesState};
pub use confirmations::{extract_governance_state, parse_confirmation};
pub use proposals::extract_service_request_details;
pub use proposals::{DESCRIPTION_PREVIEW_BYTES, ProposalInfo, extract_transfer_proposal_details};
pub use proposals::{extract_accept_transfer_details_from_view, extract_proposal_info};
pub use tally::{DomainAction, EXECUTE_EXPIRY_MARGIN_SECONDS, FALLBACK_PROPOSAL_LABEL};
pub use tally::{assemble_domain_actions, counts_toward_threshold, dedupe_newest_per_party};
pub use tally::{is_executable, live_count};
