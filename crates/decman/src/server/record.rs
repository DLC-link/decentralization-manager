//! Re-exports `decman_lib::framework::record`'s field-reading helpers so
//! `crate::server::record::X` paths (used by sibling modules like
//! `reward_automation.rs` and `transfer_context.rs`) keep resolving.
pub(crate) use decman_lib::framework::record::*;
