//! Workflow building blocks that survive the move to on-ledger coordination.
//!
//! The coordinator and peer state machines of the 1.x transport are gone; the
//! on-ledger engine (`crate::onledger::engine`) drives every run from the
//! `workflow_runs` row. What stays here is Canton-facing code the engine and
//! the HTTP handlers reuse: topology readers and retry helpers, signing-key
//! lookup, artefact storage, party replication, external-party onboarding,
//! the contracts steps, and the per-kind config types the run cards render.

pub mod add_party;
pub mod change_threshold;
pub mod contracts;
pub mod dars;
pub mod external_party;
pub mod kick;
pub mod onboarding;
pub mod party_replication;
pub mod signing_keys;
pub mod state;
pub mod storage;
pub mod topology;
pub mod validation;

pub use add_party::{AddPartyConfig, AddPartyStep};
pub use change_threshold::{ChangeThresholdConfig, ChangeThresholdStep};
pub use contracts::{ContractsConfig, ContractsStep};
pub use dars::{DarsConfig, DarsStep};
pub use kick::{KickConfig, KickStep};
pub use onboarding::{OnboardingConfig, OnboardingStep};
pub use state::WorkflowState;
