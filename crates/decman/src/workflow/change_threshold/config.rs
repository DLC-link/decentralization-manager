use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use crate::{canton_id::CantonId, workflow::thresholds::PartyThresholds};

/// Configuration for the change-threshold workflow.
///
/// Changes the thresholds of an existing decentralized party without
/// altering its membership: the `DecentralizedNamespaceDefinition.threshold`
/// (how many namespace owners must sign topology transactions) and the
/// `PartyToParticipant.threshold` (party confirmation threshold) are
/// re-issued, keeping the same owners/participants.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChangeThresholdConfig {
    /// Decentralized party whose threshold is being changed.
    pub decentralized_party_id: CantonId,

    /// New namespace threshold, also set on the party's signing keys.
    pub new_threshold: i32,

    /// New confirmation threshold. `None` means the same as `new_threshold`,
    /// which is also what configs persisted before this field existed decode
    /// to.
    #[serde(default)]
    pub confirmation_threshold: Option<i32>,

    /// The party's threshold before the change. Display-only — carried through
    /// to the workflow run card so the operator sees "old → new". Defaults to
    /// 0 for configs persisted before this field existed.
    #[serde(default)]
    pub previous_threshold: i32,

    /// Workflow instance name for artefact organisation
    /// (e.g. "xyz-change-threshold-20260108-143052").
    pub instance_name: String,

    _p: PhantomData<()>,
}

impl ChangeThresholdConfig {
    pub fn new(
        decentralized_party_id: CantonId,
        new_threshold: i32,
        confirmation_threshold: Option<i32>,
        previous_threshold: i32,
        instance_name: String,
    ) -> Self {
        Self {
            decentralized_party_id,
            new_threshold,
            confirmation_threshold,
            previous_threshold,
            instance_name,
            _p: PhantomData,
        }
    }

    /// The thresholds the proposals write.
    pub fn thresholds(&self) -> PartyThresholds {
        PartyThresholds::new(self.new_threshold, self.confirmation_threshold)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run persisted before the confirmation threshold existed resumes
    /// with both thresholds equal, as it was started.
    #[test]
    fn a_config_persisted_before_the_confirmation_threshold_still_decodes() -> crate::error::Result
    {
        let config: ChangeThresholdConfig = serde_json::from_value(serde_json::json!({
            "decentralized_party_id": format!("xyz::1220{}", "ab".repeat(32)),
            "new_threshold": 2,
            "previous_threshold": 3,
            "instance_name": "xyz-change-threshold-1",
            "_p": null,
        }))?;
        assert_eq!(config.confirmation_threshold, None);
        assert_eq!(config.thresholds(), PartyThresholds::new(2, Some(2)));
        Ok(())
    }
}
