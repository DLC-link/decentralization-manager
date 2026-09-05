//! `ProposalType` payloads — one struct per governance proposal variant,
//! each implementing the capability traits (`TemplateInfo` /
//! `DamlProtoEncode` / `Validate`) `framework::traits` defines.
//!
//! Split by the package the proposal's template lives in: `core` for
//! `governance-core`, `credential` for `governance-utility-credential`,
//! `custody` for `governance-token-custody`, `utility` for
//! `governance-utility-onboarding`, `rewards` for `governance-rewards`.

pub mod core;
pub mod credential;
pub mod custody;
pub mod rewards;
pub mod utility;

/// Where the encode snapshots live, relative to the module that asserts them.
///
/// insta resolves a snapshot path against the directory of the file holding the
/// assertion, so this string only works for a module that sits directly in
/// `catalog/proposals/`. A module at another depth needs its own path.
#[cfg(test)]
pub(crate) const SNAPSHOT_PATH: &str = "../../../snapshots";
