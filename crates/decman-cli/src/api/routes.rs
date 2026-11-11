//! One client method per API route.

use std::time::Instant;

use anyhow::{Context, Result, bail};
use common::types::{AuditLogEntry, DecentralizedParty, ParticipantsStatusResponse};
use common::types::{PeerPackageComparison, VettedPackageInfo, WorkflowKind};
use reqwest::StatusCode;
use serde_json::{Value, json};

use super::client::DecmanClient;
use super::display::source_endpoint;
use super::display::{PeerView, merge_peers, mesh_error_hint, picker_option, sort_vetted};
use super::types::WorkflowRunsResponse;
use super::types::{AuditLogResponse, AuthStatusResponse, ChainAuditEntry, ChainAuditResponse};
use super::types::{DiscoverResult, ExecuteParams, FeedItem, GovState, GovStateResponse};
use super::types::{GovernanceConfirmations, Holding, HoldingsResponse, InvitationsResponse};
use super::types::{KeyStatus, KnownMember, KnownMembersResponse, NetworkConfigResponse};
use super::types::{NetworkInfo, NetworkPeersResponse, NodeConfigResponse, OperatorInfoResponse};
use super::types::{PackageConfig, PartiesResponse, PartyAuthStatus, PartyConfigView, PeerEntry};
use crate::composer::{PickerOption, PickerSource};

impl DecmanClient {
    /// Fetch the decentralized parties this node is a member of.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_parties(&mut self) -> Result<Vec<DecentralizedParty>> {
        let parsed: PartiesResponse = self.get_json("/decentralized-parties")?;
        Ok(parsed.parties)
    }

    /// Fetch the network peers with their live connectivity status — the same
    /// data the web frontend's network panel shows.
    ///
    /// # Errors
    ///
    /// Returns an error if login or either request fails, the API returns a
    /// non-success status, or a response cannot be parsed.
    pub fn fetch_peers(&mut self) -> Result<Vec<PeerView>> {
        // Time one round-trip to our own API as the self-node latency (the web
        // frontend shows the same "you" latency), so this node's row is not blank.
        let start = Instant::now();
        let node: NodeConfigResponse = self.get_json("/node-config")?;
        let self_latency_ms = u64::try_from(start.elapsed().as_millis()).ok();
        let config: NetworkConfigResponse = self.get_json("/network-config")?;
        let statuses: ParticipantsStatusResponse = self.get_json("/participants-status")?;
        Ok(merge_peers(
            &node,
            &config.peers,
            &statuses.statuses,
            self_latency_ms,
        ))
    }

    /// Fetch the Daml packages (DARs) vetted on this node, sorted like the
    /// frontend (by name, then version).
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_dars(&mut self) -> Result<Vec<VettedPackageInfo>> {
        let mut packages: Vec<VettedPackageInfo> = self.get_json("/packages/vetted")?;
        sort_vetted(&mut packages);
        Ok(packages)
    }

    /// Fetch the Workflows feed: pending invitations followed by workflow runs
    /// — the same data the frontend's notifications panel shows.
    ///
    /// # Errors
    ///
    /// Returns an error if login or a request fails, the API returns a
    /// non-success status, or a response cannot be parsed.
    pub fn fetch_feed(&mut self) -> Result<Vec<FeedItem>> {
        let invitations: InvitationsResponse = self.get_json("/invitations")?;
        let runs: WorkflowRunsResponse = self.get_json("/workflows")?;

        let mut feed: Vec<FeedItem> = invitations
            .invitations
            .into_iter()
            .map(FeedItem::Invitation)
            .collect();
        feed.extend(runs.runs.into_iter().map(FeedItem::Run));
        Ok(feed)
    }

    /// Compare the local vetted packages against each peer's (the "checker").
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn compare_packages(&mut self) -> Result<PeerPackageComparison> {
        self.get_json("/packages/compare-peers")
    }

    /// Accept a pending invitation by id.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn accept_invitation(&mut self, id: &str) -> Result<()> {
        self.post("/invitations/accept", Some(json!({ "id": id })))
    }

    /// Decline a pending invitation by id.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn decline_invitation(&mut self, id: &str) -> Result<()> {
        self.post("/invitations/decline", Some(json!({ "id": id })))
    }

    /// Dismiss a finished workflow run by instance name.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it
    /// (e.g. the run is still in progress).
    pub fn dismiss_workflow(&mut self, instance_name: &str) -> Result<()> {
        self.post(&format!("/workflows/{instance_name}/dismiss"), None)
    }

    /// Upload a DAR to this node only (no distribution).
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn upload_dar(&mut self, filename: &str, data: &str) -> Result<()> {
        self.post(
            "/dars/upload",
            Some(json!({ "dar_files": [{ "filename": filename, "data": data }], "peer_ids": [] })),
        )
    }

    /// Distribute a DAR to the given peers (by participant id).
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it
    /// (e.g. no peers, or a workflow already in progress).
    pub fn distribute_dar(
        &mut self,
        filename: &str,
        data: &str,
        peer_ids: &[String],
    ) -> Result<()> {
        self.post(
            "/dars/distribute",
            Some(json!({
                "dar_files": [{ "filename": filename, "data": data }],
                "peer_ids": peer_ids,
            })),
        )
    }

    /// Start onboarding a new decentralized party with the given prefix and
    /// peer participant ids.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    /// A `422` mesh pre-flight failure is parsed into actionable remediation
    /// hints (which peers are unreachable or missing from each other's config).
    pub fn start_onboarding(&mut self, prefix: &str, peer_ids: &[String]) -> Result<()> {
        let body = json!({ "party_id_prefix": prefix, "peer_ids": peer_ids });
        let response = self.post_raw("/onboarding", Some(&body))?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let text = response.text().unwrap_or_default();
        if status == StatusCode::UNPROCESSABLE_ENTITY
            && let Some(hint) = mesh_error_hint(&text)
        {
            bail!("{hint}");
        }
        bail!("onboarding failed ({status}): {text}");
    }

    /// Kick a participant from a decentralized party, setting the new threshold.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn kick_participant(
        &mut self,
        party_id: &str,
        participant_id: &str,
        new_threshold: i32,
        previous_threshold: i32,
    ) -> Result<()> {
        self.post(
            "/kick",
            Some(json!({
                "decentralized_party_id": party_id,
                "participant_id": participant_id,
                "new_threshold": new_threshold,
                "previous_threshold": previous_threshold,
            })),
        )
    }

    /// Add a new member to an existing decentralized party, re-issuing the
    /// namespace + party mappings with `new_threshold`. The new participant must
    /// already be a configured peer of this node.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it
    /// (e.g. the participant is already a member, or a workflow is in progress).
    pub fn start_add_party(
        &mut self,
        party_id: &str,
        new_participant_id: &str,
        new_threshold: i32,
        previous_threshold: i32,
    ) -> Result<()> {
        self.post(
            "/add-party",
            Some(json!({
                "decentralized_party_id": party_id,
                "new_participant_id": new_participant_id,
                "new_threshold": new_threshold,
                "previous_threshold": previous_threshold,
            })),
        )
    }

    /// Change the signing threshold of an existing decentralized party
    /// (namespace + P2P), keeping its membership unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it
    /// (e.g. a workflow is already in progress, or the party is solo-owned).
    pub fn start_change_threshold(
        &mut self,
        party_id: &str,
        new_threshold: i32,
        previous_threshold: i32,
    ) -> Result<()> {
        self.post(
            "/change-threshold",
            Some(json!({
                "decentralized_party_id": party_id,
                "new_threshold": new_threshold,
                "previous_threshold": previous_threshold,
            })),
        )
    }

    /// Cancel an in-progress workflow run (coordinator side). The cancel
    /// endpoint is selected by the run's [`WorkflowKind`].
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn cancel_workflow(&mut self, kind: WorkflowKind) -> Result<()> {
        let path = match kind {
            WorkflowKind::Onboarding => "/onboarding/cancel",
            WorkflowKind::Kick => "/kick/cancel",
            WorkflowKind::Contracts => "/contracts/cancel",
            WorkflowKind::Dars => "/dars/cancel",
            WorkflowKind::AddParty => "/add-party/cancel",
            WorkflowKind::ChangeThreshold => "/change-threshold/cancel",
        };
        self.post(path, None)
    }

    /// Retry a failed workflow run (coordinator side) by instance name.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn retry_workflow(&mut self, instance_name: &str) -> Result<()> {
        self.post(&format!("/workflows/{instance_name}/retry"), None)
    }

    /// Fetch the governance audit log for a party (newest entries).
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_audit(&mut self, party_id: &str) -> Result<Vec<AuditLogEntry>> {
        let response: AuditLogResponse =
            self.get_json_query("/governance/audit", &[("party_id", party_id)])?;
        Ok(response.entries)
    }

    /// Fetch the token holdings for a party.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_holdings(&mut self, party_id: &str) -> Result<Vec<Holding>> {
        let response: HoldingsResponse =
            self.get_json_query("/holdings", &[("party_id", party_id)])?;
        Ok(response.holdings)
    }

    /// Fetch the per-party authentication status (rights + IdP status).
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_auth_status(&mut self) -> Result<Vec<PartyAuthStatus>> {
        let response: AuthStatusResponse = self.get_json("/auth/status")?;
        Ok(response.parties)
    }

    /// Re-test credentials for all parties (the backend re-mints tokens and
    /// re-checks rights). The refreshed state is read back via `auth/status`.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn test_auth(&mut self) -> Result<()> {
        self.post("/auth/test", None)
    }

    /// Grant actAs/readAs rights on a party's member and dec parties, using an
    /// admin (ParticipantAdmin) client. The secret is not stored server-side.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn grant_rights(
        &mut self,
        dec_party_id: &str,
        admin_client_id: &str,
        admin_client_secret: &str,
    ) -> Result<()> {
        self.post(
            "/auth/grant-rights",
            Some(json!({
                "dec_party_id": dec_party_id,
                "admin_client_id": admin_client_id,
                "admin_client_secret": admin_client_secret,
            })),
        )
    }

    /// Fetch the on-ledger governance state for a party (threshold, members,
    /// action timeout). `None` when the party has no governance rules contract.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_governance_state(&mut self, party_id: &str) -> Result<Option<GovState>> {
        let response: GovStateResponse =
            self.get_json_query("/governance/state", &[("party_id", party_id)])?;
        Ok(response.state)
    }

    /// Fetch the pending governance confirmations (off-chain actions and
    /// on-chain domain proposals) for a party.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_governance(&mut self, party_id: &str) -> Result<GovernanceConfirmations> {
        self.get_json_query("/governance/confirmations", &[("party_id", party_id)])
    }

    /// Add this node's confirmation to a governance action. `proposal_cid` is
    /// set for on-chain (`core_domain`) proposals.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn confirm_action(
        &mut self,
        party_id: &str,
        rules_contract_id: &str,
        action: &Value,
        governance_type: &str,
        proposal_cid: Option<&str>,
    ) -> Result<()> {
        let mut body = json!({
            "party_id": party_id,
            "rules_contract_id": rules_contract_id,
            "action": action,
            "governance_type": governance_type,
        });
        if let Some(cid) = proposal_cid {
            body["proposal_cid"] = json!(cid);
        }
        self.post("/governance/confirm", Some(body))
    }

    /// Execute a governance action once its threshold is met.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn execute_action(
        &mut self,
        party_id: &str,
        rules_contract_id: &str,
        execute: &ExecuteParams,
    ) -> Result<()> {
        let disclosed_json: Vec<Value> = execute
            .disclosed
            .iter()
            .map(|(cid, blob)| json!({ "contract_id": cid, "blob": blob }))
            .collect();
        let mut body = json!({
            "party_id": party_id,
            "rules_contract_id": rules_contract_id,
            "action": execute.action,
            "confirmation_cids": execute.confirmation_cids,
            "disclosed_contracts": disclosed_json,
            "governance_type": execute.governance_type,
        });
        if let Some(cid) = &execute.proposal_cid {
            body["proposal_cid"] = json!(cid);
        }
        self.post("/governance/execute", Some(body))
    }

    /// Propose a new on-chain (`core_domain`) governance action.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn propose_action(
        &mut self,
        party_id: &str,
        rules_contract_id: &str,
        proposal: &Value,
    ) -> Result<()> {
        self.post(
            "/governance/propose",
            Some(json!({
                "party_id": party_id,
                "rules_contract_id": rules_contract_id,
                "proposal": proposal,
            })),
        )
    }

    /// Fetch the operator party id for this node.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_operator_info(&mut self) -> Result<String> {
        let response: OperatorInfoResponse = self.get_json("/operator-info")?;
        Ok(response.party_id)
    }

    /// Fetch this node's Noise key status (existence + public key).
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_key_status(&mut self) -> Result<KeyStatus> {
        self.get_json("/keys/status")
    }

    /// Fetch the DSO network identity this node is bound to.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status (e.g. the DSO API is unreachable), or the response
    /// cannot be parsed.
    pub fn fetch_network_info(&mut self) -> Result<NetworkInfo> {
        self.get_json("/network-info")
    }

    /// Fetch the selectable contracts for a composer picker field: the same
    /// read endpoints the web frontend queries to populate its proposal-form
    /// dropdowns, mapped to `(contract_id, label)` options.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_picker_options(
        &mut self,
        source: PickerSource,
        party_id: &str,
    ) -> Result<Vec<PickerOption>> {
        let (path, key) = source_endpoint(source);
        let response: Value = self.get_json_query(path, &[("party_id", party_id)])?;
        Ok(response
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| picker_option(source, item))
            .collect())
    }

    /// Fetch the per-party IdP configuration for a dec party.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_party_config(&mut self, dec_party_id: &str) -> Result<PartyConfigView> {
        // `::` is path-safe and actix decodes the segment, so no encoding needed.
        self.get_json(&format!("/party-config/{dec_party_id}"))
    }

    /// Save the per-party IdP configuration. `body` is the full
    /// `PartyConfigRequest` (an absent secret keeps the existing one).
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn save_party_config(&mut self, body: Value) -> Result<()> {
        self.put("/party-config", body)
    }

    /// Discover the member party for the given IdP credentials.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn discover_member_party(&mut self, body: Value) -> Result<DiscoverResult> {
        let response = self.post_raw("/party-config/discover-member-party", Some(&body))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("discover-member-party failed ({status}): {text}");
        }
        response
            .json()
            .context("failed to parse discover-member-party response")
    }

    /// Fetch the configured peers with their full fields (for editing).
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_network_peers(&mut self) -> Result<Vec<PeerEntry>> {
        let response: NetworkPeersResponse = self.get_json("/network-config")?;
        Ok(response.peers)
    }

    /// Replace the configured peer list. `peers` must be a JSON array of peer
    /// objects (the endpoint takes a bare array, not an envelope).
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn save_network_peers(&mut self, peers: Value) -> Result<()> {
        self.post("/network-config", Some(peers))
    }

    /// Fetch the configured package ids for a party.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_packages(&mut self, party_id: &str) -> Result<PackageConfig> {
        self.get_json_query("/packages", &[("party_id", party_id)])
    }

    /// Fetch the governance members known for a party (participant uid → member
    /// party id), used to prefill the member set when deploying governance core.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_known_members(&mut self, party_id: &str) -> Result<Vec<KnownMember>> {
        let response: KnownMembersResponse =
            self.get_json_query("/governance/known-members", &[("party_id", party_id)])?;
        Ok(response.members)
    }

    /// Start the contracts-deployment workflow for a party.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn deploy_contracts(&mut self, body: Value) -> Result<()> {
        self.post("/contracts", Some(body))
    }

    /// Revoke this node's own confirmation on a governance action.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn cancel_confirmation(
        &mut self,
        party_id: &str,
        confirmation_cid: &str,
        governance_type: &str,
    ) -> Result<()> {
        self.post(
            "/governance/cancel",
            Some(json!({
                "party_id": party_id,
                "confirmation_cid": confirmation_cid,
                "governance_type": governance_type,
            })),
        )
    }

    /// Expire a stale confirmation on a governance action.
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, or the API rejects it.
    pub fn expire_confirmation(
        &mut self,
        party_id: &str,
        rules_contract_id: &str,
        confirmation_cid: &str,
        governance_type: &str,
    ) -> Result<()> {
        self.post(
            "/governance/expire",
            Some(json!({
                "party_id": party_id,
                "rules_contract_id": rules_contract_id,
                "confirmation_cid": confirmation_cid,
                "governance_type": governance_type,
            })),
        )
    }

    /// Fetch the on-chain governance audit trail for a party (newest first).
    ///
    /// # Errors
    ///
    /// Returns an error if login or the request fails, the API returns a
    /// non-success status, or the response cannot be parsed.
    pub fn fetch_chain_audit(&mut self, party_id: &str) -> Result<Vec<ChainAuditEntry>> {
        let response: ChainAuditResponse = self.get_json_query(
            "/governance/chain-audit",
            &[("party_id", party_id), ("limit", "200")],
        )?;
        Ok(response.entries)
    }
}
