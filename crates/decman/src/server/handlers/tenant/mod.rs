//! Wallet-facing tenant API (`/v0/tenant/*`) — external-party onboarding.
//!
//! These endpoints let a wallet provider drive external-party onboarding for a
//! party whose Ed25519 namespace key the wallet generates and holds. DPM only
//! relays: it prepares the onboarding topology, and submits the wallet-signed
//! bundle to its own participant. Every binary field on the wire is base64
//! (STANDARD engine).
//!
//! Hosting the party is all this API manages. Transacting as the party — reading its
//! contracts, preparing and executing submissions, moving token-standard assets —
//! is the wallet's own job against Canton, because it needs a Ledger-API
//! credential the node has no business holding on the party's behalf. Onboarding,
//! by contrast, writes topology over the tokenless Admin API and needs no ledger
//! token at all. `unhost` is the one call that uses a ledger token, and it is the
//! node's own: its Splice cleanup acts as the node's validator operator party,
//! never as the external party.
//!
//! Unlike the admin endpoints (Keycloak JWT + `require_admin`), tenant callers
//! authenticate with a provider-issued API key. The auth middleware checks the
//! key before the body is read, and every handler here takes the matched key
//! from [`tenant_caller`].
//!
//! Each key may make `DECPM_TENANT_RATE_LIMIT_PER_MINUTE` calls per minute, and
//! a call past that gets a 429 with `Retry-After`. The ACS block read does not
//! count, because one copy sends a call per block. An ACS block import counts
//! when it opens a copy, or when no open copy takes it. The other blocks of an
//! open copy, the final one included, never count, so a copy is never refused
//! partway.

mod acs;
mod add_hosts;
mod audit;
mod local_party;
mod onboard;
#[cfg(test)]
mod test_support;
mod threshold;
mod wire;

pub use acs::{tenant_acs_import, tenant_acs_snapshot};
pub use add_hosts::{
    tenant_add_hosts_authorize, tenant_add_hosts_execute, tenant_add_hosts_prepare,
};
pub use local_party::{tenant_local_party_adopt_execute, tenant_local_party_adopt_prepare};
pub use onboard::{tenant_execute, tenant_prepare, tenant_status};
pub use threshold::{
    tenant_party_state, tenant_threshold_authorize, tenant_threshold_execute,
    tenant_threshold_prepare, tenant_unhost,
};
