//! The wallet-driven flows: onboard a co-validated party, then transact as it.
//!
//! The shape of onboarding is deliberate and load-bearing: **one** host builds
//! the topology, the wallet signs it **locally**, and the wallet then submits the
//! same signed bundle to **every** host itself. No host relays to another, and no
//! host ever sees the private key. Canton keeps the topology a proposal until the
//! last host has signed, so a party is only live once every host reports it.

mod add_hosts;
mod host;
mod onboard;
mod sign;
mod threshold;

pub use add_hosts::{AddedHosts, add_hosts};
pub use host::{HostReport, OnboardedParty, WalletHost, statuses};
pub use onboard::execute_co_validated;
pub use threshold::raise_threshold;
