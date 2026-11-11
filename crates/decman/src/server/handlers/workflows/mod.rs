//! Workflow HTTP endpoints, one file per workflow, plus the shared run plumbing.

mod add_party;
mod cancel;
mod change_threshold;
mod contracts;
mod dars;
mod invites;
mod kick;
mod kick_target;
mod launch;
mod listing;
mod onboarding;
mod preflight;
mod runs;
#[cfg(test)]
mod test_support;

pub use add_party::{cancel_add_party, get_add_party_status, start_add_party};
pub use cancel::{
    cancel_contracts, cancel_dars, cancel_kick, cancel_onboarding, cancel_workflow_instance,
};
pub(crate) use change_threshold::validate_confirmation_threshold;
pub use change_threshold::{
    cancel_change_threshold, get_change_threshold_status, start_change_threshold,
};
pub use contracts::{get_contracts_status, start_contracts};
pub use dars::{get_dars_status, start_dars, start_dars_for_package, upload_dars_local};
pub use kick::{get_kick_status, start_kick};
pub(crate) use launch::{
    CoordinatorRequest, NodeHandles, StatusOrder, TaskStart, spawn_coordinator_task,
};
pub use listing::{dismiss_workflow, list_external_parties, list_workflows, retry_workflow};
pub use onboarding::{get_onboarding_status, start_onboarding};
pub(crate) use runs::{mark_run_failed, register_run_metrics};

// The onboarding and contracts handlers reach these as `super::…`.
use super::{Discovery, DiscoveryGate, discover_and_cache};
