use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ParticipantInfo {
    pub participant_uid: String,
    #[serde(default)]
    pub owner_key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ContractInfo {
    pub contract_id: String,
    pub template_id: String,
    pub package_id: String,
}

#[derive(Debug, Deserialize)]
pub struct DecentralizedParty {
    pub party_id: String,
    pub threshold: i32,
    #[serde(default)]
    pub participants: Vec<ParticipantInfo>,
    #[serde(default)]
    pub contracts: Vec<ContractInfo>,
}

#[derive(Debug, Deserialize)]
pub struct DecentralizedPartiesResponse {
    pub parties: Vec<DecentralizedParty>,
    #[serde(default)]
    pub refreshing: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GovernanceConfirmation {
    pub contract_id: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DomainAction {
    pub proposal_cid: String,
    pub action_label: String,
    #[serde(default)]
    pub confirmations: Vec<GovernanceConfirmation>,
    pub confirmation_count: usize,
    pub can_execute: bool,
}

#[derive(Debug, Deserialize)]
pub struct GovernanceState {
    #[serde(default)]
    pub domain_actions: Vec<DomainAction>,
    pub threshold: usize,
}

#[derive(Debug, Deserialize)]
pub struct ProviderServiceItem {
    pub contract_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ProviderServicesResponse {
    pub services: Vec<ProviderServiceItem>,
}

#[derive(Debug, Deserialize)]
pub struct ContractsQueryItem {
    pub contract_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ContractsQueryResponse {
    pub contracts: Vec<ContractsQueryItem>,
}

#[derive(Debug, Deserialize)]
pub struct PendingInvitation {
    pub id: String,
    pub invitation_type: String,
}

#[derive(Debug, Deserialize)]
pub struct PendingInvitationsResponse {
    pub invitations: Vec<PendingInvitation>,
}

#[derive(Debug, Deserialize)]
pub struct PartyDetails {
    pub party: String,
}

#[derive(Debug, Deserialize)]
pub struct AllocatePartyResponse {
    #[serde(rename = "partyDetails")]
    pub party_details: PartyDetails,
}

/// Response shape for `GET /governance/state?party_id=...`.
/// Used as a fallback in `deploy_gov_core` when `/decentralized-parties`
/// hasn't yet exposed the GovernanceRules contract.
#[derive(Debug, Deserialize)]
pub struct GovernanceStateContract {
    pub contract_id: String,
}

#[derive(Debug, Deserialize)]
pub struct GovernanceStateLookup {
    #[serde(default)]
    pub state: Option<GovernanceStateContract>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct WorkflowRun {
    pub instance_name: String,
    pub kind: String,
    pub role: String,
    pub status: String,
    #[serde(default)]
    pub current_step: String,
    #[serde(default)]
    pub step_index: usize,
    #[serde(default)]
    pub step_total: usize,
    #[serde(default)]
    pub expected_peers: Vec<String>,
    #[serde(default)]
    pub completed_peers: Vec<String>,
    #[serde(default)]
    pub coordinator_pubkey: Option<String>,
    #[serde(default)]
    pub coordinator_name: Option<String>,
    #[serde(default)]
    pub dismissed: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WorkflowRunsResponse {
    #[serde(default)]
    pub runs: Vec<WorkflowRun>,
}
