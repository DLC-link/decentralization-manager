use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use crate::canton_id::CantonId;

/// Configuration for kick workflow
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct KickConfig {
    /// Decentralized party ID to remove participant from
    pub decentralized_party_id: CantonId,

    /// Participant ID to kick
    pub participant_id: CantonId,

    /// Namespace fingerprint (DNS owner key) to remove
    pub namespace_fingerprint: String,

    /// New namespace threshold after the kick (configured by user)
    pub new_threshold: i32,

    /// Confirmation threshold after the kick, set when the party's two
    /// thresholds differ. `None` means the same as `new_threshold`, which is
    /// also what configs persisted before this field existed decode to.
    #[serde(default)]
    pub confirmation_threshold: Option<i32>,

    /// The party's threshold before the kick. Display-only — carried through
    /// to the workflow run card so the operator sees "old → new". Defaults to
    /// 0 for configs persisted before this field existed.
    #[serde(default)]
    pub previous_threshold: i32,

    /// Workflow instance name for directory organization (e.g., "xyz-network-kick-20260108-143052")
    pub instance_name: String,

    _p: PhantomData<()>,
}

impl KickConfig {
    pub fn new(
        decentralized_party_id: CantonId,
        participant_id: CantonId,
        namespace_fingerprint: String,
        new_threshold: i32,
        confirmation_threshold: Option<i32>,
        previous_threshold: i32,
        instance_name: String,
    ) -> Self {
        Self {
            decentralized_party_id,
            participant_id,
            namespace_fingerprint,
            new_threshold,
            confirmation_threshold,
            previous_threshold,
            instance_name,
            _p: PhantomData,
        }
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
        let config: KickConfig = serde_json::from_value(serde_json::json!({
            "decentralized_party_id": format!("xyz::1220{}", "ab".repeat(32)),
            "participant_id": format!("p2::1220{}", "cd".repeat(32)),
            "namespace_fingerprint": "1220aa",
            "new_threshold": 1,
            "previous_threshold": 2,
            "instance_name": "xyz-kick-1",
            "_p": null,
        }))?;
        assert_eq!(config.confirmation_threshold, None);
        Ok(())
    }
}
