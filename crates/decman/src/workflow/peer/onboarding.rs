//! The onboarding commands: generate keys and sign the DNS and P2P
//! proposals.

use super::{
    payload::extract_party_id_from_p2p_payload,
    run::{PeerRun, StepError, StepResult},
};
use crate::workflow::{
    onboarding,
    storage::{WorkflowStorage, artifact_kinds},
};

impl PeerRun {
    /// GenerateKeys: create this node's onboarding keys and send them to the
    /// coordinator.
    pub(super) async fn generate_keys(&mut self, payload: &[u8]) -> StepResult {
        tracing::info!("Executing: Generate keys");
        // Deserialize onboarding config from payload (we still need
        // the prefix for namespace_key_name / daml_key_name; the
        // config's `instance_name` is the coordinator's view and is
        // intentionally unused here).
        let onboarding_config: onboarding::OnboardingConfig = match serde_json::from_slice(payload)
        {
            Ok(config) => config,
            Err(e) => {
                return Err(StepError::Retry(format!(
                    "Failed to deserialize onboarding config: {e}"
                )));
            }
        };

        // The prefix names the vault keys this step creates or reuses,
        // so it is pinned before any key material is touched.
        if let Err(e) = self
            .expectations
            .check_onboarding_config(&onboarding_config.party_id_prefix)
        {
            return Err(StepError::Retry(format!(
                "Refusing the coordinator's onboarding config: {e}"
            )));
        }

        if let Err(e) = onboarding::generate_keys(
            &self.node_config,
            &self.db,
            &self.instance_name,
            &onboarding_config,
        )
        .await
        {
            return Err(StepError::Retry(format!("Step execution failed: {e}")));
        }
        self.consecutive_step_failures = 0;
        if let Err(e) = onboarding::peer::send_keys_to_coordinator(
            &self.client,
            &self.db,
            &self.instance_name,
            &self.node_config,
        )
        .await
        {
            tracing::error!("Failed to send keys to coordinator: {e}");
        }
        Ok(())
    }

    /// SignDns: check, record and sign the onboarding DNS proposal.
    pub(super) async fn sign_dns(&mut self, payload: &[u8]) -> StepResult {
        tracing::info!("Executing: Sign DNS proposal");
        if payload.is_empty() {
            return Err(StepError::Retry(
                "No DNS proposal payload received from coordinator".to_string(),
            ));
        }
        match self
            .expectations
            .check_onboarding_dns(&self.node_config, &self.db, &self.instance_name, payload)
            .await
        {
            // Remember the namespace we authorized so the P2P
            // proposal that follows can be pinned to the same party.
            Ok(namespace) => {
                // The P2P proposal is pinned to this namespace in the
                // next step. Signing DNS without recording it would
                // leave that cross-check silently disabled, so a write
                // failure fails the step instead.
                if let Err(e) = self
                    .db
                    .write_artifact(
                        &self.instance_name,
                        artifact_kinds::ACCEPTED_DNS_NAMESPACE,
                        None,
                        namespace.as_bytes(),
                    )
                    .await
                {
                    return Err(StepError::Retry(format!(
                        "Refusing to sign DNS: the accepted namespace could not be \
                         recorded, so the P2P cross-check would be lost: {e}"
                    )));
                }
            }
            Err(e) => {
                return Err(StepError::Retry(format!(
                    "Refusing the coordinator's DNS proposal: {e}"
                )));
            }
        }
        if let Err(e) = onboarding::sign_dns_proposals(
            &self.node_config,
            &self.db,
            &self.instance_name,
            payload,
        )
        .await
        {
            return Err(StepError::Retry(format!("Step execution failed: {e}")));
        }
        self.consecutive_step_failures = 0;
        if let Err(e) = onboarding::peer::send_dns_signature_to_coordinator(
            &self.client,
            &self.db,
            &self.instance_name,
            &self.node_config,
        )
        .await
        {
            tracing::error!("Failed to send DNS signature to coordinator: {e}");
        }
        Ok(())
    }

    /// SignP2p: check and sign the onboarding P2P proposal, then keep this
    /// node's identity for the new party.
    pub(super) async fn sign_p2p(&mut self, payload: &[u8]) -> StepResult {
        tracing::info!("Executing: Sign P2P proposals");
        if payload.is_empty() {
            return Err(StepError::Retry(
                "No P2P proposal payload received from coordinator".to_string(),
            ));
        }
        // Absent means the DNS step ran on a build that did not record
        // the namespace yet — the mixed-version case, which degrades to
        // the prefix check with a warning inside check_onboarding_p2p.
        // A read error is different: it hides whether a namespace was
        // recorded at all, so it fails the step rather than skipping
        // the cross-check.
        let signed_namespace = match self
            .db
            .read_artifact(
                &self.instance_name,
                artifact_kinds::ACCEPTED_DNS_NAMESPACE,
                None,
            )
            .await
        {
            // Nothing recorded is the mixed-version case. Bytes that
            // are not valid UTF-8 are not: something WAS recorded and
            // cannot be read back, so treating it as absent would drop
            // the cross-check on corrupt state.
            Ok(None) => None,
            Ok(Some(bytes)) => match String::from_utf8(bytes) {
                Ok(namespace) => Some(namespace),
                Err(e) => {
                    return Err(StepError::Retry(format!(
                        "Refusing to sign P2P: the recorded DNS namespace is not \
                         readable, so the cross-check cannot be applied: {e}"
                    )));
                }
            },
            Err(e) => {
                return Err(StepError::Retry(format!(
                    "Refusing to sign P2P: cannot read the DNS namespace this run \
                     accepted, so the cross-check cannot be applied: {e}"
                )));
            }
        };
        if let Err(e) = self
            .expectations
            .check_onboarding_p2p(
                &self.node_config,
                &self.db,
                &self.instance_name,
                payload,
                signed_namespace.as_deref(),
            )
            .await
        {
            return Err(StepError::Retry(format!(
                "Refusing the coordinator's P2P proposal: {e}"
            )));
        }
        if let Err(e) = onboarding::sign_p2p_proposals(
            &self.node_config,
            &self.db,
            &self.instance_name,
            payload,
        )
        .await
        {
            return Err(StepError::Retry(format!("Step execution failed: {e}")));
        }
        self.consecutive_step_failures = 0;
        if let Err(e) = onboarding::peer::send_p2p_signatures_to_coordinator(
            &self.client,
            &self.db,
            &self.instance_name,
            &self.node_config,
        )
        .await
        {
            tracing::error!("Failed to send P2P signatures to coordinator: {e}");
        }

        // Identity hook (peer side): the SignP2p payload is a
        // SignedTopologyTransaction whose PartyToParticipant mapping
        // carries the resolved dec_party_id in its `party` field. By
        // now the namespace has been signed and submitted by the
        // coordinator, so we can persist this peer's keys +
        // participant id under the dec_party_identity table for use
        // by post-onboarding workflows on this node.
        match extract_party_id_from_p2p_payload(payload) {
            Ok(dec_party_id) => {
                if let Err(e) = self
                    .db
                    .write_run_party_id(&self.instance_name, &dec_party_id)
                    .await
                {
                    tracing::error!("Failed to persist party id {dec_party_id} on peer run: {e}");
                }
                if let Err(e) = onboarding::peer::copy_self_identity_for_party(
                    &self.db,
                    &self.instance_name,
                    &self.node_config,
                    &dec_party_id,
                )
                .await
                {
                    tracing::error!("Failed to copy self identity for {dec_party_id}: {e}");
                }
            }
            Err(e) => {
                tracing::error!("Failed to extract dec_party_id from P2P payload: {e}");
            }
        }
        Ok(())
    }
}
