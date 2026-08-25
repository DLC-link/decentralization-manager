//! `ProposalType` payloads — one struct per governance proposal variant,
//! each implementing the capability traits (`TemplateInfo` /
//! `DamlProtoEncode` / `Validate`) `framework::traits` defines.
//!
//! Split by the package the proposal's template lives in: `core` for
//! `governance-core`, `utility` for `governance-utility-onboarding`.

pub mod core;
pub mod utility;
