use serde::{Deserialize, Serialize};

use crate::canton_id::CantonId;

/// Configuration for the external-party onboarding workflow.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExternalPartyConfig {
    /// Human-readable hint that becomes the identifier segment of the party id
    /// (`{party_hint}::{namespace_fingerprint}`).
    pub party_hint: String,
    /// Workflow run instance name (primary key of the `workflow_runs` row).
    pub instance_name: String,
    /// The other participants that will host the party (beyond the coordinator
    /// node). Each authorizes hosting on its own participant.
    #[serde(default)]
    pub hosting_peers: Vec<CantonId>,
    /// Confirmation threshold for the hosting participant set. `None` lets
    /// Canton default it to the number of hosting participants.
    #[serde(default)]
    pub confirmation_threshold: Option<u32>,
    /// Wallet-driven onboarding: the party-signed onboarding bundle the wallet
    /// produced (key generated + multi-hash signed client-side). When `Some`,
    /// the coordinator skips key generation, Canton topology generation, and
    /// signing — it allocates directly from this bundle and fans it out. `None`
    /// is the UI/DPM-custody flow where the coordinator holds the key.
    #[serde(default)]
    pub prepared_bundle:
        Option<crate::workflow::external_party::steps::ExternalPartyAllocatePayload>,
}

impl ExternalPartyConfig {
    /// Build a config for a run identified by `instance_name`.
    pub fn new(
        party_hint: String,
        instance_name: String,
        hosting_peers: Vec<CantonId>,
        confirmation_threshold: Option<u32>,
        prepared_bundle: Option<
            crate::workflow::external_party::steps::ExternalPartyAllocatePayload,
        >,
    ) -> Self {
        Self {
            party_hint,
            instance_name,
            hosting_peers,
            confirmation_threshold,
            prepared_bundle,
        }
    }
}
