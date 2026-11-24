//! The rotate-key commands: sign the coordinator's rotation proposal, and
//! record the coordinator's new key once the rotation is in force.

use super::run::{PeerRun, StepError, StepResult};
use crate::{
    canton_id::Fingerprint,
    noise::MessageType,
    utils,
    workflow::{
        rotate_key,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

impl PeerRun {
    /// SignRotateKey: check the coordinator's rotation proposal, record the new
    /// key it names, sign the proposal and send the signature.
    pub(super) async fn sign_rotate_key(&mut self, payload: &[u8]) -> StepResult {
        tracing::info!("Executing: Sign rotate-key proposal");
        let items = match utils::decode_length_prefixed(payload, 2) {
            Ok(items) => items,
            Err(e) => {
                return Err(StepError::Retry(format!(
                    "Failed to decode SignRotateKey payload: {e}"
                )));
            }
        };
        if let Err(e) = serde_json::from_slice::<rotate_key::RotateKeyConfig>(&items[0]) {
            return Err(StepError::Retry(format!(
                "Failed to deserialize rotate-key config: {e}"
            )));
        }
        let new_key = match self
            .expectations
            .check_rotation_proposal(&self.node_config, &self.db, &self.instance_name, &items[1])
            .await
        {
            Ok(new_key) => new_key,
            Err(e) => {
                return Err(StepError::Retry(format!(
                    "Refusing the coordinator's rotate-key proposal: {e}"
                )));
            }
        };
        // RecordKey later records only the key checked here.
        if let Err(e) = self
            .db
            .write_artifact(
                &self.instance_name,
                artifact_kinds::ROTATE_KEY_ACCEPTED_NEW_KEY,
                None,
                new_key.as_str().as_bytes(),
            )
            .await
        {
            return Err(StepError::Retry(format!(
                "Failed to record the checked rotation key: {e}"
            )));
        }
        if let Err(e) = rotate_key::steps::sign_proposal(
            &self.node_config,
            &self.db,
            &self.instance_name,
            &items[1],
        )
        .await
        {
            return Err(StepError::Retry(format!("Step execution failed: {e}")));
        }
        self.consecutive_step_failures = 0;
        if let Err(e) = rotate_key::peer::send_rotate_key_signatures_to_coordinator(
            &self.client,
            &self.db,
            &self.instance_name,
            &self.node_config,
        )
        .await
        {
            tracing::error!("Failed to send rotate-key signatures to coordinator: {e}");
        }
        Ok(())
    }

    /// RecordRotatedKey: record the coordinator's new Daml key, once it is the
    /// key this node checked when it signed.
    pub(super) async fn record_rotated_key(&mut self, payload: &[u8]) -> StepResult {
        tracing::info!("Executing: Record the coordinator's new Daml key");
        // The party, the coordinator and the replaced key all come
        // from the accepted invitation. The payload names the new
        // key, which must be the one this node checked in
        // SignProposal, and which the topology must already carry.
        let accepted = self
            .db
            .read_artifact(
                &self.instance_name,
                artifact_kinds::ROTATE_KEY_ACCEPTED_NEW_KEY,
                None,
            )
            .await;
        let recorded = match (
            self.expectations.dec_party_id.as_ref(),
            self.expectations.rotated_key.as_ref(),
            accepted,
        ) {
            (_, _, Err(e)) => Err(e),
            (_, _, Ok(None)) => Err(anyhow::anyhow!(
                "this node checked no proposal of this run, so it records no key"
            )),
            (Some(party), Some(rotated_key), Ok(Some(accepted))) => {
                match (
                    rotate_key::steps::decode_recorded_key(payload),
                    String::from_utf8(accepted)
                        .map_err(anyhow::Error::from)
                        .and_then(|key| key.parse::<Fingerprint>()),
                ) {
                    (Ok(new_key), Ok(accepted)) => {
                        rotate_key::steps::record_peer_key(
                            &self.node_config,
                            &self.db,
                            party,
                            &self.expectations.coordinator_id,
                            (rotated_key, &accepted),
                            new_key,
                        )
                        .await
                    }
                    (Err(e), _) | (_, Err(e)) => Err(e),
                }
            }
            (_, _, Ok(Some(_))) => Err(anyhow::anyhow!(
                "the accepted rotate-key invitation names no party or no replaced key"
            )),
        };
        if let Err(e) = recorded {
            return Err(StepError::Retry(format!(
                "Failed to record the coordinator's new key: {e}"
            )));
        }
        self.consecutive_step_failures = 0;
        self.client
            .send(
                MessageType::StatusUpdate,
                rotate_key::RECORDED_KEY_STATUS.to_vec(),
            )
            .await?;
        Ok(())
    }
}
