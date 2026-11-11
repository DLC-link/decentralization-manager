use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{SigningKeyUsage, SigningPublicKey},
    protocol::v30::PartyToParticipant,
};
use sqlx::SqlitePool;

use super::expectations::PeerExpectations;
use crate::{
    canton_id::Fingerprint,
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{
        onboarding::steps::proposals::create::decode_keys_payload,
        signing_keys::{own_namespace_key, vault_holds},
        storage::{WorkflowStorage, artifact_kinds, identity_kinds},
    },
};

/// What an unrecorded local key bundle means for a check that needs it.
///
/// Onboarding generates the bundle on the run itself, so its absence there is
/// a broken run. Every later proposal falls back to the long-lived identity
/// row, which a party onboarded before that table existed does not have — the
/// same legacy gap the DNS owner-set check tolerates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WhenUnrecorded {
    Fail,
    Skip,
}

impl PeerExpectations {
    /// This node's Daml signing key must be among the party's signing keys, or
    /// it would be a member that cannot sign for the party it just authorized.
    pub(super) async fn check_own_daml_key(
        &self,
        config: &NodeConfig,
        storage: &SqlitePool,
        instance_name: &str,
        mapping: &PartyToParticipant,
        when_unrecorded: WhenUnrecorded,
    ) -> Result {
        let keys = match self.own_keys(storage, instance_name).await? {
            Some(keys) => keys,
            None if when_unrecorded == WhenUnrecorded::Fail => anyhow::bail!(
                "this node's public keys are not on the {instance_name} run, so the P2P \
                 proposal cannot be checked against them"
            ),
            None => {
                tracing::warn!(
                    "this node's keys are unrecorded for this party (onboarded before the \
                     identity table existed); checking the proposal against the vault instead"
                );
                return self.check_vault_holds_a_party_key(config, mapping).await;
            }
        };
        let own_daml_fingerprint = utils::compute_fingerprint(&keys[1]);
        let signing_keys = mapping
            .party_signing_keys
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("P2P proposal carries no party signing keys"))?;

        if !signing_keys
            .keys
            .iter()
            .any(|key| utils::compute_fingerprint(key) == own_daml_fingerprint)
        {
            anyhow::bail!(
                "P2P proposal does not carry this node's Daml signing key \
                 {own_daml_fingerprint}"
            );
        }
        Ok(())
    }

    /// The fallback for a party this node has no local key record for: the
    /// vault still knows whether this node holds the private half of one of
    /// the proposed keys.
    ///
    /// Without it a legacy party's members cannot check the attribution at
    /// all — the coordinator works out whose key is whose from its own
    /// records, and a stale one would take a member's authority away
    /// irreversibly, because inline keys override the mapping the member's
    /// key still sits in.
    ///
    /// # Errors
    ///
    /// Errors when the proposal carries no signing keys, when the vault holds
    /// none of them, or when the vault cannot be read.
    pub(super) async fn check_vault_holds_a_party_key(
        &self,
        config: &NodeConfig,
        mapping: &PartyToParticipant,
    ) -> Result {
        let Some(signing_keys) = mapping.party_signing_keys.as_ref() else {
            anyhow::bail!("P2P proposal carries no party signing keys");
        };

        for key in &signing_keys.keys {
            if vault_holds(config, &utils::compute_fingerprint(key)).await? {
                return Ok(());
            }
        }

        anyhow::bail!(
            "P2P proposal carries {count} party signing key(s) and this node holds the \
             private half of none of them, so it would be left a member that cannot sign \
             for the party",
            count = signing_keys.keys.len()
        )
    }

    /// This node's namespace fingerprint for the run: from the keys it
    /// generated during onboarding, falling back to the long-lived identity
    /// row when the run's artefacts are already gone.
    pub(super) async fn own_namespace(
        &self,
        config: &NodeConfig,
        storage: &SqlitePool,
        instance_name: &str,
    ) -> Result<Option<Fingerprint>> {
        // An identity row recovered from the chain can only carry the party's
        // Daml key, and older builds wrote a copy of it where the namespace
        // key belongs. Reading that as a namespace fingerprint would compare a
        // Daml key against the owner set and refuse every proposal, so such a
        // bundle does not answer the question.
        if let Some(keys) = self.own_keys(storage, instance_name).await?
            && let Some(fingerprint) = namespace_fingerprint(&keys)
        {
            return Ok(Some(fingerprint));
        }

        // No usable local record. For a party that already exists the vault
        // still answers it: the namespace key this node holds that the party's
        // current owner set names. Skipping the check instead would let a
        // coordinator swap this node's namespace out of the owner set with
        // every other check still passing, which signs the node's own
        // topology authority away.
        let Some(dec_party_id) = self.dec_party_id.as_ref() else {
            tracing::warn!(
                "this node's keys are unrecorded for this run and it targets no existing \
                 party, so its namespace cannot be resolved"
            );
            return Ok(None);
        };

        let synchronizer_id = utils::get_synchronizer_id(config).await?;
        let resolved = own_namespace_key(config, dec_party_id, &synchronizer_id)
            .await?
            .map(|key| utils::compute_fingerprint(&key));
        match &resolved {
            Some(fingerprint) => tracing::info!(
                "Resolved this node's namespace {fingerprint} for {dec_party_id} from the \
                 vault; no usable local key record"
            ),
            None => tracing::warn!(
                "this node holds no namespace key that {dec_party_id}'s owner set names, so \
                 the owner-set check cannot be applied"
            ),
        }
        Ok(resolved)
    }

    /// This node's `[namespace_key, daml_key]` bundle.
    pub(super) async fn own_keys(
        &self,
        storage: &SqlitePool,
        instance_name: &str,
    ) -> Result<Option<Vec<SigningPublicKey>>> {
        let self_id = self.self_id.to_string();
        let stored = match storage
            .read_artifact(
                instance_name,
                artifact_kinds::PEER_PUBLIC_KEYS,
                Some(&self_id),
            )
            .await?
        {
            Some(payload) => Some(payload),
            // No run artefact and no dec party to look under, or no identity
            // row for one: the keys are genuinely unrecorded. That is the
            // pre-identity-table party, and the only case a caller may treat
            // leniently. A read that *fails*, or a payload that will not
            // decode, is an error and stays one.
            None => match self.dec_party_id.as_ref() {
                Some(dec_party_id) => {
                    storage
                        .read_identity(dec_party_id, identity_kinds::PEER_PUBLIC_KEYS, &self_id)
                        .await?
                }
                None => None,
            },
        };

        let Some(payload) = stored else {
            return Ok(None);
        };

        let keys = decode_keys_payload(&payload)?;
        if keys.len() != 2 {
            anyhow::bail!(
                "expected 2 public keys for this node, found {count}",
                count = keys.len()
            );
        }
        Ok(Some(keys))
    }
}

/// The namespace fingerprint of a recorded `[namespace_key, daml_key]` bundle,
/// or `None` when index `[0]` is not a namespace key.
///
/// A bundle recovered from the chain cannot carry a namespace key — the chain
/// holds none — and older builds put a copy of the Daml key there instead.
/// Taking that at face value compares a Daml fingerprint against the party's
/// owner set, which refuses every proposal the node is asked to sign.
pub(super) fn namespace_fingerprint(keys: &[SigningPublicKey]) -> Option<Fingerprint> {
    let key = keys.first()?;
    if !key.usage.contains(&(SigningKeyUsage::Namespace as i32)) {
        tracing::warn!(
            "this node's recorded key bundle for this party carries no namespace key at [0]"
        );
        return None;
    }
    Some(utils::compute_fingerprint(key))
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::crypto::v30::SigningKeyUsage;

    use super::*;

    /// The bundle `backfill_peer_keys_from_chain` used to write: a copy of the
    /// Daml key where the namespace key belongs. Reading `[0]` as a namespace
    /// made every legacy member refuse the DNS proposal.
    #[test]
    fn placeholder_bundle_yields_no_namespace() {
        let daml = SigningPublicKey {
            public_key: vec![7; 32],
            usage: vec![SigningKeyUsage::Protocol as i32],
            ..Default::default()
        };

        assert!(namespace_fingerprint(&[daml.clone(), daml]).is_none());
    }

    #[test]
    fn real_bundle_yields_its_namespace_fingerprint() {
        let namespace = SigningPublicKey {
            public_key: vec![1; 32],
            usage: vec![SigningKeyUsage::Namespace as i32],
            ..Default::default()
        };
        let daml = SigningPublicKey {
            public_key: vec![2; 32],
            usage: vec![SigningKeyUsage::Protocol as i32],
            ..Default::default()
        };

        assert_eq!(
            namespace_fingerprint(&[namespace.clone(), daml]),
            Some(utils::compute_fingerprint(&namespace))
        );
    }
}
