//! The add-party commands that only the new member executes: generate its
//! keys, import the party's ACS and clear its onboarding flag. Every other
//! member replies skip. Also the members' signature on the clearing proposal.

use super::{
    payload::{decode_add_party_config, is_new_member, send_skip_status},
    run::{PeerRun, StepError, StepResult},
};
use crate::{
    noise::MessageType,
    utils,
    workflow::{add_party, party_replication},
};

impl PeerRun {
    /// GenerateAddPartyKeys: the new member creates its keys; every other
    /// member replies skip.
    pub(super) async fn generate_add_party_keys(&mut self, payload: &[u8]) -> StepResult {
        tracing::info!("Executing: Generate add-party keys");
        let add_party_config = match decode_add_party_config(payload) {
            Ok(config) => config,
            Err(e) => {
                return Err(StepError::Retry(format!(
                    "Failed to deserialize add-party config: {e:#}"
                )));
            }
        };
        // Both the party and the new member are pinned before
        // `is_new_member` reads `new_participant_id` to decide this
        // node's role: naming this peer there would otherwise make it
        // take the new member's part in a run it joined as an existing
        // member.
        if let Err(e) = self
            .expectations
            .check_dec_party(&add_party_config.decentralized_party_id)
            .and_then(|()| {
                self.expectations
                    .check_new_participant(&add_party_config.new_participant_id)
            })
        {
            return Err(StepError::Retry(format!(
                "Refusing the coordinator's add-party config: {e}"
            )));
        }
        if !is_new_member(&self.node_config, &add_party_config) {
            self.consecutive_step_failures = 0;
            send_skip_status(&self.client, "GenerateAddPartyKeys").await;
            return Ok(());
        }

        let ledger_token = add_party::resolve_ledger_token(
            &self.workflow_auth,
            &add_party_config.decentralized_party_id,
        )
        .await;
        if let Err(e) = add_party::generate_keys(
            &self.node_config,
            &self.db,
            &self.instance_name,
            &add_party_config,
            ledger_token.as_deref(),
        )
        .await
        {
            return Err(StepError::Retry(format!("Step execution failed: {e}")));
        }
        self.consecutive_step_failures = 0;
        if let Err(e) = add_party::peer::send_keys_to_coordinator(
            &self.client,
            &self.db,
            &self.instance_name,
            &self.node_config,
        )
        .await
        {
            tracing::error!("Failed to send add-party keys to coordinator: {e}");
        }
        Ok(())
    }

    /// ImportAcs: the new member imports the party's ACS; every other member
    /// replies skip.
    pub(super) async fn import_acs(&mut self, payload: &[u8]) -> StepResult {
        tracing::info!("Executing: Import party ACS");
        // [config, package_ids]. The snapshot is not in the payload: it
        // is pulled block by block straight into Canton's import, so
        // nothing proportional to the ACS is buffered or stored.
        let items = match utils::decode_length_prefixed(payload, 2) {
            Ok(items) => items,
            Err(e) => {
                return Err(StepError::Retry(format!(
                    "Failed to decode ImportAcs payload: {e}"
                )));
            }
        };
        let add_party_config = match decode_add_party_config(&items[0]) {
            Ok(config) => config,
            Err(e) => {
                return Err(StepError::Retry(format!(
                    "Failed to deserialize add-party config: {e:#}"
                )));
            }
        };
        // Both the party and the new member are pinned before
        // `is_new_member` reads `new_participant_id` to decide this
        // node's role: naming this peer there would otherwise make it
        // take the new member's part in a run it joined as an existing
        // member.
        if let Err(e) = self
            .expectations
            .check_dec_party(&add_party_config.decentralized_party_id)
            .and_then(|()| {
                self.expectations
                    .check_new_participant(&add_party_config.new_participant_id)
            })
        {
            return Err(StepError::Retry(format!(
                "Refusing the coordinator's ACS import: {e}"
            )));
        }
        if !is_new_member(&self.node_config, &add_party_config) {
            self.consecutive_step_failures = 0;
            send_skip_status(&self.client, "ImportAcs").await;
            return Ok(());
        }
        // Newline-joined package ids the party's contracts need; the
        // import preflight checks this participant has them all before
        // it disconnects anything.
        let required_package_ids: Vec<String> = String::from_utf8_lossy(&items[1])
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();

        let step_result = party_replication::import_party_acs(
            &self.node_config,
            &self.db,
            &add_party_config.replication_target(&self.instance_name),
            &required_package_ids,
            party_replication::TransferSource::Coordinator,
            |seq| {
                let client = &self.client;
                async move { client.request_next_acs_block(seq).await.map_err(Into::into) }
            },
        )
        .await;

        // The import owns its retries and runs them inside one disconnect
        // window. Retrying the step here would reconnect the participant
        // between attempts, which replays the ACS journal — so a failure
        // that reaches this point ends the peer.
        if let Err(e) = step_result {
            return Err(StepError::Abort(anyhow::anyhow!(
                "Aborting peer: the ACS import failed: {e:#}"
            )));
        }
        self.consecutive_step_failures = 0;
        if let Err(e) = self
            .client
            .send(MessageType::StatusUpdate, b"ImportAcs completed".to_vec())
            .await
        {
            tracing::error!("Failed to send completion status: {e}");
        }
        Ok(())
    }

    /// ClearOnboardingFlag: the new member clears its onboarding flag, or
    /// authors the clearing proposal; every other member replies skip.
    pub(super) async fn clear_onboarding_flag(&mut self, payload: &[u8]) -> StepResult {
        tracing::info!("Executing: Clear onboarding flag");
        let add_party_config = match decode_add_party_config(payload) {
            Ok(config) => config,
            Err(e) => {
                return Err(StepError::Retry(format!(
                    "Failed to deserialize add-party config: {e:#}"
                )));
            }
        };
        // Both the party and the new member are pinned before
        // `is_new_member` reads `new_participant_id` to decide this
        // node's role: naming this peer there would otherwise make it
        // take the new member's part in a run it joined as an existing
        // member.
        if let Err(e) = self
            .expectations
            .check_dec_party(&add_party_config.decentralized_party_id)
            .and_then(|()| {
                self.expectations
                    .check_new_participant(&add_party_config.new_participant_id)
            })
        {
            return Err(StepError::Retry(format!(
                "Refusing the coordinator's clearing request: {e}"
            )));
        }
        if !is_new_member(&self.node_config, &add_party_config) {
            self.consecutive_step_failures = 0;
            send_skip_status(&self.client, "ClearOnboardingFlag").await;
            return Ok(());
        }

        match party_replication::clear_onboarding_flag(
            &self.node_config,
            &self.db,
            &add_party_config.replication_target(&self.instance_name),
        )
        .await
        {
            Ok(party_replication::ClearOutcome::Proposed) => {
                // Canton requires the ONBOARDING PARTICIPANT to issue
                // the flag-clear transaction — author it here and ship
                // it to the coordinator for the threshold-signing
                // round (the coordinator lacks an appropriate key).
                match add_party::author_clear_proposal(&self.node_config, &add_party_config).await {
                    Ok(Some(proposal)) => {
                        self.consecutive_step_failures = 0;
                        if let Err(e) = self
                            .client
                            .send(MessageType::AddPartyClearProposal, proposal)
                            .await
                        {
                            tracing::error!("Failed to send clearing proposal to coordinator: {e}");
                        }
                    }
                    Ok(None) => {
                        // Flag dropped between the poll and the
                        // authoring — report as cleared.
                        self.consecutive_step_failures = 0;
                        if let Err(e) = self
                            .client
                            .send(
                                MessageType::StatusUpdate,
                                b"ClearOnboardingFlag: Cleared".to_vec(),
                            )
                            .await
                        {
                            tracing::error!("Failed to send completion status: {e}");
                        }
                    }
                    Err(e) => {
                        return Err(StepError::Retry(format!("Step execution failed: {e}")));
                    }
                }
            }
            Ok(party_replication::ClearOutcome::Cleared) => {
                self.consecutive_step_failures = 0;
                if let Err(e) = self
                    .client
                    .send(
                        MessageType::StatusUpdate,
                        b"ClearOnboardingFlag: Cleared".to_vec(),
                    )
                    .await
                {
                    tracing::error!("Failed to send completion status: {e}");
                }
            }
            Err(e) => {
                return Err(StepError::Retry(format!("Step execution failed: {e}")));
            }
        }
        Ok(())
    }

    /// SignClearOnboarding: check and sign the new member's clearing proposal.
    pub(super) async fn sign_clear_onboarding(&mut self, payload: &[u8]) -> StepResult {
        tracing::info!("Executing: Sign onboarding-flag clearing proposal");
        let items = match utils::decode_length_prefixed(payload, 2) {
            Ok(items) => items,
            Err(e) => {
                return Err(StepError::Retry(format!(
                    "Failed to decode SignClearOnboarding payload: {e}"
                )));
            }
        };
        if items[1].is_empty() {
            // Skip marker: the flag already cleared without a
            // signing round (e.g. a single-owner-threshold party).
            self.consecutive_step_failures = 0;
            send_skip_status(&self.client, "SignClearOnboarding").await;
            return Ok(());
        }

        if let Err(e) = self.expectations.check_clear_onboarding(&items[1]) {
            return Err(StepError::Retry(format!(
                "Refusing the coordinator's clearing proposal: {e}"
            )));
        }

        if let Err(e) = add_party::sign_clear_proposal(
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
        if let Err(e) = add_party::peer::send_clear_signature_to_coordinator(
            &self.client,
            &self.db,
            &self.instance_name,
            &self.node_config,
        )
        .await
        {
            tracing::error!("Failed to send clearing signature to coordinator: {e}");
        }
        Ok(())
    }
}
