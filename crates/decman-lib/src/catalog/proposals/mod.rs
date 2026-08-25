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
