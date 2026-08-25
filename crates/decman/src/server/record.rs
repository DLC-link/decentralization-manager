//! Shared Ledger API `Record` field-reading helpers.
//!
//! Moved to `decman_lib::framework::record` (both the strict `Result` readers
//! and the lenient `Option` readers, plus the Set `Party`, `GenMap`, and
//! `RelTime` extractors); re-exported here so `crate::server::record::X` paths
//! (used by sibling modules like `reward_automation.rs` and
//! `transfer_context.rs`) keep resolving.
pub(crate) use decman_lib::framework::record::*;
