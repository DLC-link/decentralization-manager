//! `governance-utility-onboarding` proposal payloads.

mod instrument;
mod issuance;
mod registrar;
mod services;
mod setup;

/// The package key of every payload in this module.
const PACKAGE: &str = "governance_utility_onboarding";

pub use instrument::{MintsCredentials, OffboardInstrumentIssuers, OnboardInstrumentIssuers};
pub use instrument::{ProvisionInstrument, UpdateInstrumentConfiguration, WithCredentialIdNonce};
pub use issuance::{AcceptBurnRequest, AcceptMintRequest, Burn, Mint};
pub use registrar::{CreateProviderConfiguration, CreateRegistrarServiceRequest, OnboardRegistrar};
pub use services::RequestDevNetFeaturedAppRight;
pub use services::{CreateDelegatedBatchedMarkersProxy, CreateProviderServiceRequest};
pub use services::{CreateUserServiceRequest, ProvisionProviderService};
pub use setup::SetupUtility;
pub use setup::{CreateTransferRule, SetEnableResultContracts, SetProviderAppRewardBeneficiaries};

#[cfg(test)]
mod tests;
