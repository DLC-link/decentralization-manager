//! The API route list: every handler the HTTP server registers.

use actix_web::dev::{ServiceFactory, ServiceRequest};
use utoipa_actix_web::UtoipaApp;

use crate::server::handlers;

/// Register every API route. Each `.service()` call both registers the
/// actix route AND collects its OpenAPI path.
pub(super) fn api_routes<T>(app: UtoipaApp<T>) -> UtoipaApp<T>
where
    T: ServiceFactory<ServiceRequest, Config = (), Error = actix_web::Error, InitError = ()>,
{
    app.service(handlers::healthz)
        .service(handlers::get_network_config)
        .service(handlers::save_network_config)
        .service(handlers::get_node_config)
        .service(handlers::get_decentralized_parties)
        .service(handlers::get_participants_status)
        .service(handlers::get_node_health)
        .service(handlers::compare_peer_packages)
        .service(handlers::get_vetted_packages)
        .service(handlers::get_expected_versions)
        .service(handlers::clear_acs_import_quarantine)
        .service(handlers::list_former_parties)
        .service(handlers::preview_former_party_purge)
        .service(handlers::purge_former_party)
        .service(handlers::reconnect_after_purge)
        .service(handlers::dismiss_former_party)
        .service(handlers::start_kick)
        .service(handlers::get_kick_status)
        .service(handlers::cancel_kick)
        .service(handlers::start_add_party)
        .service(handlers::get_add_party_status)
        .service(handlers::cancel_add_party)
        .service(handlers::start_change_threshold)
        .service(handlers::get_change_threshold_status)
        .service(handlers::cancel_change_threshold)
        .service(handlers::list_external_parties)
        .service(handlers::unhost_external_party)
        .service(handlers::preview_unhost_external_party)
        .service(handlers::get_unhost_audit)
        .service(handlers::get_tenant_call_audit)
        .service(handlers::tenant_prepare)
        .service(handlers::tenant_execute)
        .service(handlers::tenant_add_hosts_prepare)
        .service(handlers::tenant_add_hosts_authorize)
        .service(handlers::tenant_add_hosts_execute)
        .service(handlers::tenant_acs_snapshot)
        .service(handlers::tenant_acs_import)
        .service(handlers::tenant_threshold_prepare)
        .service(handlers::tenant_threshold_authorize)
        .service(handlers::tenant_threshold_execute)
        .service(handlers::tenant_local_party_adopt_prepare)
        .service(handlers::tenant_local_party_adopt_execute)
        .service(handlers::tenant_party_state)
        .service(handlers::tenant_unhost)
        .service(handlers::tenant_status)
        .service(handlers::start_onboarding)
        .service(handlers::get_onboarding_status)
        .service(handlers::cancel_onboarding)
        .service(handlers::start_contracts)
        .service(handlers::get_contracts_status)
        .service(handlers::cancel_contracts)
        .service(handlers::upload_dars_local)
        .service(handlers::start_dars)
        .service(handlers::start_dars_for_package)
        .service(handlers::get_dars_status)
        .service(handlers::cancel_dars)
        .service(handlers::list_workflows)
        .service(handlers::dismiss_workflow)
        .service(handlers::retry_workflow)
        .service(handlers::cancel_workflow_instance)
        .service(handlers::get_key_status)
        .service(handlers::get_invitations)
        .service(handlers::list_handled_invitations)
        .service(handlers::get_invitation_dar)
        .service(handlers::accept_invitation)
        .service(handlers::decline_invitation)
        .service(handlers::get_auth_config)
        .service(handlers::get_auth_status)
        .service(handlers::test_auth)
        .service(handlers::grant_rights)
        .service(handlers::get_governance)
        .service(handlers::get_governance_state)
        .service(handlers::get_proposals_page)
        .service(handlers::get_known_members)
        .service(handlers::get_provider_services_handler)
        .service(handlers::get_user_services_handler)
        .service(handlers::get_credential_offers_handler)
        .service(handlers::get_credentials_handler)
        .service(handlers::get_registrar_service_requests_handler)
        .service(handlers::get_provider_configurations_handler)
        .service(handlers::get_block_list_shards_handler)
        .service(handlers::get_registrar_services_handler)
        .service(handlers::get_instruments_handler)
        .service(handlers::get_transfer_instructions_handler)
        .service(handlers::get_mint_requests_handler)
        .service(handlers::get_burn_requests_handler)
        .service(handlers::get_transfer_preapprovals_handler)
        .service(handlers::get_transfer_factories_handler)
        .service(handlers::get_holdings_handler)
        .service(handlers::query_contracts_handler)
        .service(handlers::get_packages)
        .service(handlers::propose_action)
        .service(handlers::confirm_action)
        .service(handlers::execute_action)
        .service(handlers::expire_confirmation)
        .service(handlers::cancel_confirmation)
        .service(handlers::cancel_proposal)
        .service(handlers::get_governance_audit)
        .service(handlers::get_governance_chain_audit)
        .service(handlers::get_token_standard_contracts)
        .service(handlers::get_coupon_reassignment_delegation)
        .service(handlers::get_reward_status)
        .service(handlers::get_network_info)
        .service(handlers::get_open_mining_round)
        .service(handlers::get_operator_info)
        .service(handlers::get_party_config)
        .service(handlers::save_party_config)
        .service(handlers::discover_member_party)
}

#[cfg(test)]
mod tests {
    use actix_web::App;
    use utoipa::openapi::path::PathItem;
    use utoipa_actix_web::AppExt;

    use super::api_routes;

    /// Each method a path answers, as `METHOD /path`.
    fn routes_of(path: &str, item: &PathItem) -> Vec<String> {
        [
            ("GET", &item.get),
            ("PUT", &item.put),
            ("POST", &item.post),
            ("DELETE", &item.delete),
            ("OPTIONS", &item.options),
            ("HEAD", &item.head),
            ("PATCH", &item.patch),
            ("TRACE", &item.trace),
            ("QUERY", &item.query),
        ]
        .into_iter()
        .filter(|(_, operation)| operation.is_some())
        .map(|(method, _)| format!("{method} {path}"))
        .collect()
    }

    /// The OpenAPI document lists every API route with its method. A change
    /// to this list is a change to the HTTP interface.
    #[test]
    fn the_api_routes_are_stable() {
        let (_, api) = api_routes(App::new().into_utoipa_app()).split_for_parts();
        let routes: Vec<String> = api
            .paths
            .paths
            .iter()
            .flat_map(|(path, item)| routes_of(path, item))
            .collect();
        assert_eq!(routes, EXPECTED_ROUTES);
    }

    const EXPECTED_ROUTES: &[&str] = &[
        "DELETE /acs-import-quarantine",
        "POST /add-party",
        "POST /add-party/cancel",
        "GET /add-party/status",
        "GET /auth-config",
        "POST /auth/grant-rights",
        "GET /auth/status",
        "POST /auth/test",
        "GET /block-list-shards",
        "POST /change-threshold",
        "POST /change-threshold/cancel",
        "GET /change-threshold/status",
        "POST /contracts",
        "POST /contracts/cancel",
        "GET /contracts/query",
        "GET /contracts/status",
        "GET /coupon-reassignment-delegation",
        "GET /credential-offers",
        "GET /credentials",
        "POST /dars/cancel",
        "POST /dars/distribute",
        "POST /dars/distribute-package",
        "GET /dars/distribute/status",
        "POST /dars/upload",
        "GET /decentralized-parties",
        "GET /external-parties",
        "POST /external-parties/{party_id}/unhost",
        "GET /external-parties/{party_id}/unhost-audit",
        "GET /external-parties/{party_id}/unhost/preview",
        "GET /former-parties",
        "DELETE /former-parties",
        "POST /former-parties/purge",
        "GET /former-parties/purge-preview",
        "POST /former-parties/reconnect",
        "GET /governance/audit",
        "GET /governance/burn-requests",
        "POST /governance/cancel",
        "POST /governance/cancel-proposal",
        "GET /governance/chain-audit",
        "POST /governance/confirm",
        "GET /governance/confirmations",
        "POST /governance/execute",
        "POST /governance/expire",
        "GET /governance/known-members",
        "GET /governance/mint-requests",
        "GET /governance/proposals",
        "POST /governance/propose",
        "GET /governance/state",
        "GET /governance/transfer-instructions",
        "GET /healthz",
        "GET /holdings",
        "GET /instruments",
        "GET /invitations",
        "POST /invitations/accept",
        "POST /invitations/decline",
        "GET /invitations/handled",
        "GET /invitations/{id}/dars/{index}",
        "GET /keys/status",
        "POST /kick",
        "POST /kick/cancel",
        "GET /kick/status",
        "GET /network-config",
        "POST /network-config",
        "GET /network-info",
        "GET /node-config",
        "GET /node-health",
        "POST /onboarding",
        "POST /onboarding/cancel",
        "GET /onboarding/status",
        "GET /open-mining-round",
        "GET /operator-info",
        "GET /packages",
        "GET /packages/compare-peers",
        "GET /packages/expected-versions",
        "GET /packages/vetted",
        "GET /participants-status",
        "PUT /party-config",
        "POST /party-config/discover-member-party",
        "GET /party-config/{dec_party_id}",
        "GET /provider-configurations",
        "GET /registrar-service-requests",
        "GET /reward-status",
        "GET /services/provider",
        "GET /services/registrar",
        "GET /services/user",
        "GET /tenant-call-audit",
        "POST /token-standard-contracts",
        "GET /transfer-factories",
        "GET /transfer-preapprovals",
        "POST /v0/tenant/add-hosts/authorize",
        "POST /v0/tenant/add-hosts/execute",
        "POST /v0/tenant/add-hosts/import",
        "POST /v0/tenant/add-hosts/prepare",
        "POST /v0/tenant/execute",
        "POST /v0/tenant/local-party/adopt-key/execute",
        "POST /v0/tenant/local-party/adopt-key/prepare",
        "POST /v0/tenant/prepare",
        "POST /v0/tenant/threshold/authorize",
        "POST /v0/tenant/threshold/execute",
        "POST /v0/tenant/threshold/prepare",
        "POST /v0/tenant/unhost",
        "GET /v0/tenant/{party}/acs/{target}",
        "GET /v0/tenant/{party}/state",
        "GET /v0/tenant/{party}/status",
        "GET /workflows",
        "POST /workflows/{instance_name}/cancel",
        "POST /workflows/{instance_name}/dismiss",
        "POST /workflows/{instance_name}/retry",
    ];
}
