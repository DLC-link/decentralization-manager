mod auth;
mod config;
mod governance;
mod invitations;
mod keys;
mod parties;
mod party_config;
mod tenant;
mod token_standard;
mod workflows;

pub(crate) use auth::{get_auth_config, get_auth_status, grant_rights, test_auth};
// `NodeConfigResponse` is reached externally as `dec_party_manager::server::NodeConfigResponse`
// by the `gen-types` binary (a separate crate from this lib), so it must stay `pub`; the
// handler functions beside it have no such consumer.
pub use config::NodeConfigResponse;
pub(crate) use config::{get_network_config, get_node_config, healthz, save_network_config};
pub(crate) use governance::{
    cancel_confirmation, cancel_proposal, confirm_action, execute_action, expire_confirmation,
    get_coupon_reassignment_delegation, get_governance, get_governance_audit,
    get_governance_chain_audit, get_governance_state, get_known_members, propose_action,
};
// Crate-internal governance helpers reused by the reward-automation module,
// re-exported here so they are reachable through the private `governance`
// submodule.
pub(crate) use governance::{get_party_credentials, packages};
pub(crate) use invitations::{accept_invitation, decline_invitation, get_invitations};
pub(crate) use keys::get_key_status;
pub(crate) use parties::{
    compare_peer_packages, fetch_decentralized_parties, get_decentralized_parties,
    get_participants_status, get_vetted_packages, resolve_owner_keys_from_peers,
    store_parties_to_db,
};
pub(crate) use party_config::{discover_member_party, get_party_config, save_party_config};
pub(crate) use tenant::{
    tenant_add_hosts_onboard, tenant_add_hosts_prepare, tenant_onboard, tenant_prepare,
    tenant_status,
};
pub(crate) use token_standard::{
    get_burn_requests_handler, get_credential_offers_handler, get_credentials_handler,
    get_holdings_handler, get_instruments_handler, get_mint_requests_handler, get_network_info,
    get_operator_info, get_packages, get_provider_configurations_handler,
    get_provider_services_handler, get_registrar_service_requests_handler,
    get_registrar_services_handler, get_token_standard_contracts, get_transfer_factories_handler,
    get_transfer_instructions_handler, get_transfer_preapprovals_handler,
    get_user_services_handler, get_vaults_handler, query_contracts_handler
};
pub(crate) use workflows::{
    cancel_add_party, cancel_change_threshold, cancel_contracts, cancel_dars, cancel_kick,
    cancel_onboarding, cancel_workflow_instance, dismiss_workflow, get_add_party_status,
    get_change_threshold_status, get_contracts_status, get_dars_status, get_kick_status,
    get_onboarding_status, list_external_parties, list_workflows, retry_workflow, start_add_party,
    start_change_threshold, start_contracts, start_dars, start_kick, start_onboarding,
    upload_dars_local,
};
