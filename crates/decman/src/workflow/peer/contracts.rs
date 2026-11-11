//! The DARs and contracts commands: upload DARs and sign prepared
//! submissions.

use super::{
    payload::save_prepared_submissions_from_payload,
    run::{PeerRun, StepError, StepResult},
};
use crate::{noise::MessageType, utils, workflow::contracts};

impl PeerRun {
    /// UploadDars: upload the coordinator's DARs once they match the invitation.
    pub(super) async fn upload_dars(&mut self, payload: &[u8]) -> StepResult {
        tracing::info!("Executing: Upload DARs");

        // Decode DAR files from payload and upload directly
        let dar_files = if payload.is_empty() {
            Vec::new()
        } else {
            match utils::decode_files(payload) {
                Ok(files) => files,
                Err(e) => {
                    return Err(StepError::Retry(format!(
                        "Failed to decode DARs from coordinator: {e}"
                    )));
                }
            }
        };

        // Vetting a DAR makes this participant willing to execute its
        // code, so the files must be the ones the operator accepted.
        if let Err(e) = self.expectations.check_dars(&dar_files) {
            return Err(StepError::Retry(format!(
                "Refusing the coordinator's DARs: {e}"
            )));
        }

        if let Err(e) = contracts::upload_dars_from_bytes(&self.node_config, dar_files).await {
            return Err(StepError::Retry(format!("Step execution failed: {e}")));
        }
        self.consecutive_step_failures = 0;
        if let Err(e) = self
            .client
            .send(MessageType::StatusUpdate, b"UploadDars completed".to_vec())
            .await
        {
            tracing::error!("Failed to send completion status: {e}");
        }
        Ok(())
    }

    /// SignSubmissions: check and sign the contracts the coordinator prepared.
    pub(super) async fn sign_submissions(&mut self, payload: &[u8]) -> StepResult {
        tracing::info!("Executing: Sign submissions");

        if payload.is_empty() {
            return Err(StepError::Retry(
                "No submissions payload received from coordinator".to_string(),
            ));
        }

        // Decode config and files from payload: [config_json, files_payload]
        let items = match utils::decode_length_prefixed(payload, 2) {
            Ok(items) => items,
            Err(e) => {
                return Err(StepError::Retry(format!(
                    "Failed to decode SignSubmissions payload: {e}"
                )));
            }
        };

        let contracts_config: contracts::ContractsConfig = match serde_json::from_slice(&items[0]) {
            Ok(config) => config,
            Err(e) => {
                return Err(StepError::Retry(format!(
                    "Failed to deserialize contracts config: {e}"
                )));
            }
        };

        let dec_party_id = contracts_config.decentralized_party_id.clone();
        if let Err(e) = self.expectations.check_dec_party(&dec_party_id) {
            return Err(StepError::Retry(format!(
                "Refusing the coordinator's contracts config: {e}"
            )));
        }

        // Persist the prepared submissions sent by the coordinator into
        // this peer's workflow_artifacts so sign_submissions can
        // read them back via list_artifacts.
        if let Err(e) =
            save_prepared_submissions_from_payload(&items[1], &self.db, &self.instance_name).await
        {
            return Err(StepError::Retry(format!(
                "Failed to save prepared submissions from coordinator: {e}"
            )));
        }

        if let Err(e) = contracts::sign_submissions(
            &self.node_config,
            &self.db,
            &self.instance_name,
            &dec_party_id,
            &self.expectations.contract_intents,
        )
        .await
        {
            return Err(StepError::Retry(format!("Step execution failed: {e:#}")));
        }
        self.consecutive_step_failures = 0;
        if let Err(e) = contracts::peer::send_submission_signatures_to_coordinator(
            &self.client,
            &self.db,
            &self.instance_name,
            &self.node_config,
        )
        .await
        {
            tracing::error!("Failed to send submission signatures to coordinator: {e}");
        }
        Ok(())
    }
}
