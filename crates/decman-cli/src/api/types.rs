//! The response envelopes and view types the client parses from the API.

use common::types::{AuditLogEntry, DecentralizedParty, PendingInvitation, WorkflowRun};
use serde::Deserialize;
use serde_json::Value;

/// `/network-config` response: the configured peer list.
#[derive(Debug, Deserialize)]
pub(super) struct NetworkConfigResponse {
    #[serde(default)]
    pub(super) peers: Vec<PeerConfig>,
}

/// A configured peer (name, address) from `/network-config`.
#[derive(Debug, Deserialize)]
pub(super) struct PeerConfig {
    pub(super) participant_id: String,
    #[serde(default)]
    pub(super) name: String,
    #[serde(default)]
    pub(super) address: String,
    #[serde(default)]
    pub(super) port: u16,
}

/// `/node-config` response: this node's own identity and version. The backend
/// flattens its `NodeConfig`, so `node` and `version` are top-level fields.
#[derive(Debug, Deserialize)]
pub(super) struct NodeConfigResponse {
    pub(super) node: NodeIdentity,
    #[serde(default)]
    pub(super) version: Option<String>,
}

/// This node's identity, as reported by `/node-config`.
#[derive(Debug, Deserialize)]
pub(super) struct NodeIdentity {
    pub(super) participant_id: String,
    #[serde(default)]
    pub(super) public_address: Option<String>,
    #[serde(default)]
    pub(super) listen_address: String,
    #[serde(default)]
    pub(super) port: u16,
}

/// `/workflows` response envelope.
#[derive(Debug, Deserialize)]
pub(super) struct WorkflowRunsResponse {
    #[serde(default)]
    pub(super) runs: Vec<WorkflowRun>,
}

/// `/invitations` response envelope.
#[derive(Debug, Deserialize)]
pub(super) struct InvitationsResponse {
    #[serde(default)]
    pub(super) invitations: Vec<PendingInvitation>,
}

/// An item in the Workflows feed: an incoming invitation or a workflow run.
#[derive(Clone, Debug)]
pub enum FeedItem {
    Invitation(PendingInvitation),
    Run(WorkflowRun),
}

/// `/governance/audit` response envelope.
#[derive(Debug, Deserialize)]
pub(super) struct AuditLogResponse {
    #[serde(default)]
    pub(super) entries: Vec<AuditLogEntry>,
}

/// A token holding for a party, from `/holdings`. Decimal amounts are strings.
#[derive(Clone, Debug, Deserialize)]
pub struct Holding {
    #[serde(default)]
    pub instrument_admin: String,
    #[serde(default)]
    pub instrument_id: String,
    #[serde(default)]
    pub amount: String,
    #[serde(default)]
    pub locked_amount: String,
    #[serde(default)]
    pub preapproval_set_up: bool,
}

/// `/holdings` response envelope.
#[derive(Debug, Deserialize)]
pub(super) struct HoldingsResponse {
    #[serde(default)]
    pub(super) holdings: Vec<Holding>,
}

/// A single confirmation on a pending governance action.
#[derive(Clone, Debug, Deserialize)]
pub struct GovConfirmation {
    #[serde(default)]
    pub contract_id: String,
    #[serde(default)]
    pub confirming_party: String,
    #[serde(default)]
    pub expires_at: i64,
}

/// A pending off-chain governance action (core-self), grouped by hash.
#[derive(Clone, Debug, Deserialize)]
pub struct GovAction {
    /// The `ActionType` payload, echoed back verbatim on confirm / execute.
    #[serde(default)]
    pub action: Value,
    #[serde(default)]
    pub confirmations: Vec<GovConfirmation>,
    #[serde(default)]
    pub executable_confirmation_cids: Option<Vec<String>>,
    #[serde(default)]
    pub confirmation_count: i64,
    #[serde(default)]
    pub can_execute: bool,
}

/// A pending on-chain (core-domain) governance proposal.
#[derive(Clone, Debug, Deserialize)]
pub struct DomainGovAction {
    #[serde(default)]
    pub proposal_cid: String,
    #[serde(default)]
    pub action_label: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub confirmations: Vec<GovConfirmation>,
    #[serde(default)]
    pub executable_confirmation_cids: Option<Vec<String>>,
    #[serde(default)]
    pub confirmation_count: i64,
    #[serde(default)]
    pub can_execute: bool,
    #[serde(default)]
    pub orphaned: bool,
}

/// `/governance/confirmations` response: pending actions awaiting signatures.
#[derive(Clone, Debug, Deserialize)]
pub struct GovernanceConfirmations {
    #[serde(default)]
    pub actions: Vec<GovAction>,
    #[serde(default)]
    pub domain_actions: Vec<DomainGovAction>,
    #[serde(default)]
    pub threshold: i32,
    #[serde(default)]
    pub rules_contract_id: Option<String>,
    #[serde(default)]
    pub member_party_id: Option<String>,
    /// Cursor of the next page of proposals, when there is one.
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// `/operator-info` response: this node's operator party id.
#[derive(Debug, Deserialize)]
pub(super) struct OperatorInfoResponse {
    #[serde(default)]
    pub(super) party_id: String,
}

/// `/keys/status` response: whether this node's Noise keypair exists and, if so,
/// its public key (the value peers configure to reach this node).
#[derive(Clone, Debug, Deserialize)]
pub struct KeyStatus {
    #[serde(default)]
    pub has_keys: bool,
    #[serde(default)]
    pub public_key: Option<String>,
}

/// `/network-info` response: the DSO identity this node is bound to. The
/// `amulet_rules` blob is intentionally not deserialized (it is large and only
/// needed server-side).
#[derive(Clone, Debug, Deserialize)]
pub struct NetworkInfo {
    #[serde(default)]
    pub dso_party_id: String,
    #[serde(default)]
    pub amulet_rules_cid: String,
}

/// Per-party IdP configuration, from `GET /party-config/{id}` (secrets are
/// returned only as `has_*` flags).
#[derive(Clone, Debug, Deserialize)]
pub struct PartyConfigView {
    #[serde(default)]
    pub member_party_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub keycloak_url: String,
    #[serde(default)]
    pub keycloak_realm: String,
    #[serde(default)]
    pub keycloak_client_id: String,
    #[serde(default)]
    pub has_client_secret: bool,
    #[serde(default)]
    pub auth0_domain: Option<String>,
    #[serde(default)]
    pub auth0_audience: Option<String>,
    #[serde(default)]
    pub auth0_client_id: Option<String>,
    #[serde(default)]
    pub has_auth0_client_secret: bool,
}

/// Result of `POST /party-config/discover-member-party`.
#[derive(Clone, Debug, Deserialize)]
pub struct DiscoverResult {
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub primary_party: Option<String>,
}

/// A fully-detailed configured peer, for the network-config editor (the
/// merged [`PeerView`] drops `public_key` / `party`, which editing needs).
#[derive(Clone, Debug, Deserialize)]
pub struct PeerEntry {
    #[serde(default)]
    pub participant_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub public_key: String,
    #[serde(default)]
    pub party: Option<String>,
}

/// `/network-config` response with the full peer fields.
#[derive(Debug, Deserialize)]
pub(super) struct NetworkPeersResponse {
    #[serde(default)]
    pub(super) peers: Vec<PeerEntry>,
}

/// Configured package ids for a party, from `/packages` (only the fields the
/// CLI needs for contract deployment).
#[derive(Clone, Debug, Deserialize)]
pub struct PackageConfig {
    #[serde(default)]
    pub governance_core: Option<String>,
}

/// A governance member discovered from peers, from `/governance/known-members`.
#[derive(Clone, Debug, Deserialize)]
pub struct KnownMember {
    #[serde(default)]
    pub participant_uid: String,
    #[serde(default)]
    pub member_party_id: Option<String>,
}

/// `/governance/known-members` response envelope.
#[derive(Debug, Deserialize)]
pub(super) struct KnownMembersResponse {
    #[serde(default)]
    pub(super) members: Vec<KnownMember>,
}

/// Arguments for [`DecmanClient::execute_action`]. `disclosed` carries any
/// `(contract_id, base64_blob)` pairs the action needs at execution time
/// (empty for most actions; deployment actions need their rules-contract blob).
pub struct ExecuteParams {
    pub action: Value,
    pub confirmation_cids: Vec<String>,
    pub governance_type: String,
    pub proposal_cid: Option<String>,
    pub disclosed: Vec<(String, String)>,
}

/// On-ledger governance state for a party, from `/governance/state`.
#[derive(Clone, Debug, Deserialize)]
pub struct GovState {
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub threshold: i32,
    #[serde(default)]
    pub action_confirmation_timeout_microseconds: Option<i64>,
    #[serde(default)]
    pub out_of_date: bool,
}

/// `/governance/state` response envelope (`state` is null when no rules
/// contract exists for the party).
#[derive(Debug, Deserialize)]
pub(super) struct GovStateResponse {
    #[serde(default)]
    pub(super) state: Option<GovState>,
}

/// One on-chain governance audit entry, from `/governance/chain-audit`.
#[derive(Clone, Debug, Deserialize)]
pub struct ChainAuditEntry {
    #[serde(default)]
    pub timestamp: i64,
    #[serde(default)]
    pub event_type: String,
    #[serde(default)]
    pub governance_type: String,
    #[serde(default)]
    pub action_summary: String,
    #[serde(default)]
    pub choice: Option<String>,
}

/// `/governance/chain-audit` response envelope.
#[derive(Debug, Deserialize)]
pub(super) struct ChainAuditResponse {
    #[serde(default)]
    pub(super) entries: Vec<ChainAuditEntry>,
}

/// One directed missing edge from the onboarding mesh pre-flight: peer `from`
/// does not have `to` configured (`mesh_hole`), or the coordinator could not
/// reach `to` at all (`unreachable_from_coordinator`).
#[derive(Debug, Deserialize)]
pub(super) struct MeshEdge {
    #[serde(default)]
    pub(super) from: String,
    #[serde(default)]
    pub(super) to: String,
    #[serde(default)]
    pub(super) kind: Option<String>,
}

/// The `422` body returned when onboarding's mesh pre-flight fails.
#[derive(Debug, Deserialize)]
pub(super) struct MeshErrorResponse {
    #[serde(default)]
    pub(super) error: String,
    #[serde(default)]
    pub(super) missing_edges: Vec<MeshEdge>,
}

/// Per-party authentication status, from `/auth/status`. Internally tagged on
/// the wire under `status`.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum AuthStatusKind {
    Authenticated,
    Mock,
    Failed {
        #[serde(default)]
        error: String,
    },
    Notconfigured,
}

/// Which ledger rights this node holds for a party's member and dec parties.
#[derive(Clone, Debug, Deserialize)]
pub struct AuthRights {
    #[serde(default)]
    pub member_party_act_as: bool,
    #[serde(default)]
    pub member_party_read_as: bool,
    #[serde(default)]
    pub dec_party_act_as: bool,
    #[serde(default)]
    pub dec_party_read_as: bool,
}

/// Authentication status for a single decentralized party.
#[derive(Clone, Debug, Deserialize)]
pub struct PartyAuthStatus {
    #[serde(default)]
    pub dec_party_id: String,
    #[serde(default)]
    pub member_party_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    pub status: AuthStatusKind,
    #[serde(default)]
    pub rights: Option<AuthRights>,
}

/// `/auth/status` response envelope.
#[derive(Debug, Deserialize)]
pub(super) struct AuthStatusResponse {
    #[serde(default)]
    pub(super) parties: Vec<PartyAuthStatus>,
}

/// `GET /decentralized-parties` response envelope.
#[derive(Debug, Deserialize)]
pub(super) struct PartiesResponse {
    #[serde(default)]
    pub(super) parties: Vec<DecentralizedParty>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::fixtures::NS;

    #[test]
    fn parties_response_ignores_unknown_fields() {
        // Arrange — the real envelope also carries `source` and `refreshing`.
        let json = format!(
            r#"{{"parties":[{{"party_id":"p::{NS}","threshold":2,"owners":["a"],
            "participants":[{{"participant_uid":"par::{NS}","permission":"submission"}}]}}],
            "source":"cache","refreshing":true}}"#
        );

        // Act
        let parsed: PartiesResponse = serde_json::from_str(&json).unwrap();

        // Assert
        assert_eq!(parsed.parties.len(), 1);
        assert_eq!(parsed.parties[0].threshold, 2);
        assert_eq!(parsed.parties[0].participants.len(), 1);
    }
}
