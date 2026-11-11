//! Canton ledger query layer.
//!
//! Read-side helpers that query the Canton Ledger API (active contracts,
//! governance state, holdings, transfers, rewards, etc.) and shape the results
//! into the response types served by the HTTP handlers.

mod contracts;
mod governance;
mod governance_state;
mod holdings;
mod registry;
mod templates;
#[cfg(test)]
mod test_values;
mod transfers;
mod utility;

pub use contracts::{
    ContractQueryParams, get_contracts, get_party_metadata, query_contracts_by_template,
    sort_contracts,
};
pub(crate) use contracts::{compare_versions, fetch_package_versions};
pub use governance::{GovernanceQueue, get_governance_confirmations, page_proposal_summaries};
pub use governance_state::get_governance_state;
pub(crate) use governance_state::{
    fetch_contract_template_id, read_governance_state, resolve_contract_package_ref,
};
pub use holdings::{get_holdings, select_input_holdings};
pub use registry::{
    get_block_list_shards, get_instruments, get_provider_configurations,
    get_registrar_service_requests, get_registrar_services,
};
pub use templates::{contract_templates_all, rules_templates};
pub use transfers::{
    get_open_burn_requests, get_open_mint_requests, get_open_transfer_instructions,
    get_transfer_factories,
};
pub use utility::{
    get_credential_offers, get_credentials, get_provider_services, get_user_services,
};
