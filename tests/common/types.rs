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
