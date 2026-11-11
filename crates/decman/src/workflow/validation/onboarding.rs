use std::collections::BTreeSet;

use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId, config::NodeConfig, error::Result,
    workflow::onboarding::steps::proposals::create::compute_decentralized_namespace,
};

use super::{
    decode::{decode_namespace_definition, decode_party_to_participant},
    expectations::PeerExpectations,
    own_keys::WhenUnrecorded,
};

impl PeerExpectations {
    /// Validate the onboarding config the coordinator sends before this node
    /// generates keys.
    ///
    /// `generate_keys` derives the vault key names from `party_id_prefix`, so
    /// an unchecked prefix lets a coordinator have this node create keys under
    /// a name the operator never accepted — or reuse the keys already sitting
    /// under another party's prefix, and authorize a namespace delegation for
    /// it. The prefix is pinned before any of that happens.
    ///
    /// # Errors
    ///
    /// Errors if the prefix differs from the accepted one, or if the accepted
    /// invitation carries none.
    pub fn check_onboarding_config(&self, party_id_prefix: &str) -> Result {
        let accepted = self.prefix.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "accepted onboarding invitation carries no party prefix, so the \
                 coordinator's config cannot be checked against it"
            )
        })?;
        if accepted != party_id_prefix {
            anyhow::bail!(
                "coordinator asked for keys under prefix {party_id_prefix} but the accepted \
                 invitation names {accepted}"
            );
        }
        Ok(())
    }

    /// Validate the DNS proposal of an onboarding run.
    ///
    /// The party does not exist yet, so there is no party id to pin against.
    /// What the peer can pin is the owner set it was invited into: one owner
    /// per accepted member, its own namespace among them, the threshold the
    /// invitation advertised, and a decentralized namespace that really is the
    /// hash of those owners (so the coordinator cannot name an unrelated
    /// namespace and have peers authorize it).
    ///
    /// # Errors
    ///
    /// Errors on any mismatch with the accepted invitation.
    pub async fn check_onboarding_dns(
        &self,
        config: &NodeConfig,
        storage: &SqlitePool,
        instance_name: &str,
        payload: &[u8],
    ) -> Result<String> {
        let namespace_def = decode_namespace_definition(payload)?;

        let owners: BTreeSet<String> = namespace_def.owners.iter().cloned().collect();
        if owners.len() != namespace_def.owners.len() {
            anyhow::bail!("DNS proposal repeats an owner namespace");
        }
        if owners.len() != self.members.len() {
            anyhow::bail!(
                "DNS proposal has {found} owners but the accepted invitation names \
                 {expected} members",
                found = owners.len(),
                expected = self.members.len()
            );
        }

        let own_namespace = self
            .own_namespace(config, storage, instance_name)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "this node's public keys are not on the {instance_name} run, so the DNS \
                     proposal cannot be checked against them"
                )
            })?;
        if !owners.iter().any(|owner| own_namespace == *owner) {
            anyhow::bail!(
                "DNS proposal does not include this node's namespace {own_namespace} \
                 among its owners"
            );
        }

        self.check_threshold("namespace", namespace_def.threshold, owners.len())?;

        let owner_set: std::collections::HashSet<String> = owners.iter().cloned().collect();
        let computed = compute_decentralized_namespace(&owner_set);
        if computed != namespace_def.decentralized_namespace {
            anyhow::bail!(
                "DNS proposal claims namespace {claimed} but its owner set hashes to \
                 {computed}",
                claimed = namespace_def.decentralized_namespace
            );
        }

        tracing::info!(
            "DNS proposal matches the accepted invitation: {count} owners, threshold {threshold}",
            count = owners.len(),
            threshold = namespace_def.threshold
        );
        Ok(namespace_def.decentralized_namespace)
    }

    /// Validate the P2P proposal of an onboarding run.
    ///
    /// `dns_namespace` is the namespace this peer validated and signed in the
    /// DNS step; the party id must be built from it, so the two proposals
    /// cannot describe different parties.
    ///
    /// # Errors
    ///
    /// Errors on any mismatch with the accepted invitation.
    pub async fn check_onboarding_p2p(
        &self,
        config: &NodeConfig,
        storage: &SqlitePool,
        instance_name: &str,
        payload: &[u8],
        dns_namespace: Option<&str>,
    ) -> Result {
        let mapping = decode_party_to_participant(payload)?;
        let party = CantonId::parse(&mapping.party)?;

        match &self.prefix {
            Some(prefix) if prefix != &party.prefix => anyhow::bail!(
                "P2P proposal is for party prefix {found} but the accepted invitation \
                 named {prefix}",
                found = party.prefix
            ),
            Some(_) => {}
            // `prefix` is required on every onboarding invite, so an absent one
            // means the invite never parsed — leaving the coordinator to choose
            // the party's prefix.
            None => anyhow::bail!(
                "accepted onboarding invitation carries no party prefix, so the proposed \
                 party cannot be checked against it"
            ),
        }

        // The command gate checks the workflow kind, not the step order, so a
        // coordinator can send SignP2p before SignDns. Absent therefore means
        // "this run has not signed a DNS proposal", which the coordinator
        // controls — not "an older build did not record it". Treating it as
        // legacy let a coordinator skip DNS and have the peer sign a party
        // with any namespace whose prefix happened to match.
        let Some(namespace) = dns_namespace else {
            anyhow::bail!(
                "no DNS namespace recorded for {instance_name}: this run has not signed a \
                 DNS proposal, so the P2P proposal cannot be pinned to one"
            );
        };
        if namespace != party.namespace.to_hex() {
            anyhow::bail!(
                "P2P proposal targets namespace {found} but this node signed DNS for \
                 {namespace}",
                found = party.namespace.to_hex()
            );
        }

        self.check_p2p_membership(&mapping)?;
        self.check_onboarding_markers(&mapping)?;
        self.check_p2p_thresholds(&mapping)?;
        self.check_own_daml_key(
            config,
            storage,
            instance_name,
            &mapping,
            WhenUnrecorded::Fail,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::topology_mapping;

    use super::*;
    use crate::{server::WorkflowKind, workflow::validation::test_support::*};

    /// `generate_keys` derives the vault key names from the prefix, so an
    /// unchecked one lets a coordinator have this node create keys under a name
    /// the operator never accepted — or reuse the keys already sitting under
    /// another party's prefix.
    #[test]
    fn rejects_key_generation_under_an_unaccepted_prefix() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a);
        expectations.kind = WorkflowKind::Onboarding;
        expectations.prefix = Some("treasury".to_string());

        assert!(
            expectations
                .check_onboarding_config("someone-elses-party")
                .is_err()
        );
        expectations.check_onboarding_config("treasury")?;

        // No accepted prefix at all is not a licence to pick one.
        expectations.prefix = None;
        assert!(expectations.check_onboarding_config("treasury").is_err());
        Ok(())
    }

    /// The command gate checks the workflow kind, not the step order, so a
    /// coordinator can send SignP2p before SignDns. With no namespace recorded
    /// the peer must refuse rather than fall back to the prefix check.
    #[tokio::test]
    async fn rejects_a_p2p_proposal_before_any_dns_was_signed() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a.clone());
        expectations.kind = WorkflowKind::Onboarding;
        expectations.prefix = Some("treasury".to_string());

        let party = format!("treasury::{}", canton_id("x", 9)?.namespace.to_hex());
        let payload = encode_proposal(topology_mapping::Mapping::PartyToParticipant(p2p(
            &party,
            &[a],
            1,
        )));

        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
        let error = expectations
            .check_onboarding_p2p(&NodeConfig::default(), &pool, "run", &payload, None)
            .await
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            error.contains("has not signed a DNS proposal"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    /// The prefix is required on every onboarding invite, so an absent one
    /// means the invite never parsed — and the coordinator would name the party.
    #[tokio::test]
    async fn rejects_an_onboarding_p2p_when_no_prefix_was_accepted() -> Result {
        let a = canton_id("p1", 1)?;
        let mut expectations = expectations(vec![a.clone()], a.clone());
        expectations.kind = WorkflowKind::Onboarding;
        assert!(expectations.prefix.is_none());

        let payload = encode_proposal(topology_mapping::Mapping::PartyToParticipant(p2p(
            &canton_id("anything", 5)?.to_string(),
            &[a],
            1,
        )));
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
        assert!(
            expectations
                .check_onboarding_p2p(&NodeConfig::default(), &pool, "run", &payload, None)
                .await
                .is_err()
        );
        Ok(())
    }
}
