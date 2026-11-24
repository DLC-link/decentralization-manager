//! `governance-utility-onboarding` proposal payloads.

mod instrument;
mod issuance;
mod registrar;
mod services;
mod setup;

/// The package key of every payload in this module.
const PACKAGE: &str = "governance_utility_onboarding";

pub use instrument::{
    MintsCredentials, OffboardInstrumentIssuers, OnboardInstrumentIssuers, ProvisionInstrument,
    UpdateInstrumentConfiguration, WithCredentialIdNonce,
};
pub use issuance::{AcceptBurnRequest, AcceptMintRequest, Burn, Mint};
pub use registrar::{CreateProviderConfiguration, CreateRegistrarServiceRequest, OnboardRegistrar};
pub use services::{
    CreateDelegatedBatchedMarkersProxy, CreateProviderServiceRequest, CreateUserServiceRequest,
    ProvisionProviderService, RequestDevNetFeaturedAppRight,
};
pub use setup::{
    CreateTransferRule, SetEnableResultContracts, SetProviderAppRewardBeneficiaries, SetupUtility,
};

#[cfg(test)]
mod tests;
