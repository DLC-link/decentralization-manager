//! GovernanceRules state reads and contract package lookups.

use canton_proto_rs::com::daml::ledger::api::v2::{GetEventsByContractIdRequest, Identifier};
use decman_lib::catalog::interpret;

use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
    error::Result,
    utils,
};

use crate::server::{
    event_filters::{party_event_format, template_filter, wildcard_filter},
    ledger_paging::fetch_first_active_contract,
    package_inventory::{
        fetch_package_id_to_name, fetch_package_names, newest_matching_names, package_name_prefix,
    },
    types::GovernanceState,
};

// ============================================================================
// Governance State Query
// ============================================================================

/// Get the state of the GovernanceRules contract for a party
///
/// A failed read is logged and reads as no contract, which is what the feed
/// shows. [`read_governance_state`] keeps the failure for a caller that must
/// tell the two apart.
pub async fn get_governance_state(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
) -> Result<Option<GovernanceState>> {
    Ok(find_governance_state(config, party_id, token, packages, &mut None).await)
}

/// The GovernanceRules contract for a party. `Ok(None)` means every read
/// succeeded and found none; `Err` means a failed read left that unknown.
pub(crate) async fn read_governance_state(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
) -> Result<Option<GovernanceState>> {
    let mut failure = None;
    let found = find_governance_state(config, party_id, token, packages, &mut failure).await;
    match (found, failure) {
        (Some(state), _) => Ok(Some(state)),
        (None, Some(e)) => Err(e),
        (None, None) => Ok(None),
    }
}

/// Searches the configured package, then older ones. Keeps the first failed
/// read in `failure`.
async fn find_governance_state(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
    failure: &mut Option<anyhow::Error>,
) -> Option<GovernanceState> {
    // Try each governance template until we find a match
    for template in decman_lib::catalog::templates::governance_state_templates(packages) {
        match fetch_governance_state_for_template(config, party_id, token.clone(), &template).await
        {
            Ok(Some(mut state)) => {
                // Found under the configured package — not out of date.
                state.package_ref = Some(template.package_ref.clone());
                state.out_of_date = false;
                return Some(state);
            }
            Ok(None) => continue,
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("PACKAGE_NAMES_NOT_FOUND") {
                    continue;
                }
                tracing::warn!(
                    "Failed to query governance state for {}:{}: {e}",
                    template.module,
                    template.entity
                );
                failure.get_or_insert(e);
            }
        }
    }
    // Nothing under the configured packages — look for a GovernanceRules
    // contract under an older governance-core package version still
    // uploaded to the participant.
    fetch_governance_state_fallback(config, party_id, token, packages, failure).await
}

/// Look for a GovernanceRules contract under any OLDER governance-core
/// package version present on the participant. Runs only after the
/// configured templates yielded nothing; returns the newest match tagged
/// `out_of_date` with the package ref it actually lives under.
async fn fetch_governance_state_fallback(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
    failure: &mut Option<anyhow::Error>,
) -> Option<GovernanceState> {
    let configured = packages.governance_core.as_deref()?;
    let names = match fetch_package_names(config).await {
        Ok(names) => names,
        Err(e) => {
            tracing::warn!("Fallback gov-core discovery: cannot list packages: {e:#}");
            failure.get_or_insert(e);
            return None;
        }
    };
    let prefix = package_name_prefix(configured);
    let configured_name = configured.trim_start_matches('#');
    for name in newest_matching_names(&names, &prefix) {
        // The configured name was already tried by the caller.
        if name == configured_name {
            continue;
        }
        let template = decman_lib::framework::TemplateId::new(
            format!("#{name}"),
            "Governance.Rules",
            "GovernanceRules",
        );
        match fetch_governance_state_for_template(config, party_id, token.clone(), &template).await
        {
            Ok(Some(mut state)) => {
                tracing::warn!(
                    "GovernanceRules contract for {party_id} found under fallback package \
                     #{name} (configured {configured}); flagging as out of date"
                );
                state.package_ref = Some(template.package_ref);
                state.out_of_date = true;
                return Some(state);
            }
            Ok(None) => continue,
            Err(e) => {
                // An auth failure is about the party, not the package, so every
                // remaining ref fails the same way. Walking all of them cost
                // seconds per ref per party and is what left the approvals feed
                // spinning across a deployment of parties this node cannot read
                // (#424).
                if is_auth_failure(&e) {
                    tracing::debug!(
                        "Fallback gov-core query for #{name} not attempted further:                          no credentials for {party_id}"
                    );
                    failure.get_or_insert(e);
                    return None;
                }
                if !e.to_string().contains("PACKAGE_NAMES_NOT_FOUND") {
                    tracing::warn!("Fallback gov-core query for #{name} failed: {e}");
                    failure.get_or_insert(e);
                }
                continue;
            }
        }
    }
    None
}

/// Whether an error is the participant refusing the caller's credentials.
///
/// Distinguishable from a missing package: retrying a different package ref
/// cannot fix it, so a caller should stop rather than pay for every ref.
fn is_auth_failure(e: &anyhow::Error) -> bool {
    let text = e.to_string();
    text.contains("valid authentication credentials")
        || text.contains("UNAUTHENTICATED")
        || text.contains("PERMISSION_DENIED")
}

/// Fetch governance state for a specific template
async fn fetch_governance_state_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &decman_lib::framework::TemplateId,
) -> Result<Option<GovernanceState>> {
    let event_format = party_event_format(
        party_id,
        vec![template_filter(template.into(), false)],
        true,
    );

    Ok(fetch_first_active_contract(config, token, event_format)
        .await?
        .as_ref()
        .and_then(interpret::extract_governance_state)
        .map(|rules| GovernanceState {
            contract_id: rules.contract_id,
            governance_party: rules.governance_party,
            members: rules.members,
            threshold: rules.threshold,
            additional_proposers: rules.additional_proposers,
            disclosure_observers: rules.disclosure_observers,
            action_confirmation_timeout_microseconds: rules.timeout_micros,
            // Both callers overwrite these with the package the contract was
            // actually found under, so the parse says nothing about them.
            package_ref: None,
            out_of_date: false,
        }))
}

/// Resolve the package-name ref (`#name`) of the package an on-ledger
/// contract was actually created under. Used to exercise choices on
/// governance contracts that may live under an older package version than
/// the configured one. Returns `fallback` (the configured ref) on any
/// failure so callers degrade to the previous behavior instead of erroring.
pub(crate) async fn resolve_contract_package_ref(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    contract_id: &str,
    fallback: &str,
) -> String {
    match fetch_contract_package_ref(config, party_id, token, contract_id).await {
        Ok(Some(package_ref)) => package_ref,
        Ok(None) => {
            tracing::debug!(
                "Could not resolve package ref for {contract_id}; using configured {fallback}"
            );
            fallback.to_string()
        }
        Err(e) => {
            tracing::debug!(
                "Could not resolve package ref for {contract_id}: {e}; \
                 using configured {fallback}"
            );
            fallback.to_string()
        }
    }
}

/// Read the actual created template. Absence and RPC errors are distinct;
/// callers exercising delegated authority must not substitute a configured ref.
pub(crate) async fn fetch_contract_template_id(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    contract_id: &str,
) -> Result<Option<Identifier>> {
    let mut client = utils::create_event_query_client(config, token).await?;

    let request = GetEventsByContractIdRequest {
        contract_id: contract_id.to_string(),
        event_format: Some(party_event_format(
            party_id,
            vec![wildcard_filter(false)],
            false,
        )),
    };

    Ok(client
        .get_events_by_contract_id(tonic::Request::new(request))
        .await?
        .into_inner()
        .created
        .and_then(|c| c.created_event)
        .and_then(|e| e.template_id))
}

/// Look up a contract's created event and map its concrete package id back
/// to a `#name` ref via the participant's package inventory.
async fn fetch_contract_package_ref(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    contract_id: &str,
) -> Result<Option<String>> {
    let Some(template) = fetch_contract_template_id(config, party_id, token, contract_id).await?
    else {
        return Ok(None);
    };
    let package_id = template.package_id;
    // Already a `#name` ref — use it directly.
    if package_id.starts_with('#') {
        return Ok(Some(package_id));
    }
    let id_to_name = fetch_package_id_to_name(config).await?;
    Ok(id_to_name.get(&package_id).map(|name| format!("#{name}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With both APIs on port 1, where nothing listens, every rules read fails.
    /// The feed's read still shows no contract; the strict read reports the
    /// failure, so the poller cannot mistake an outage for a party without rules.
    #[tokio::test]
    async fn a_failed_rules_read_is_an_error_only_to_the_strict_read() -> Result {
        let mut config = NodeConfig::default();
        config.canton.ledger_api_port = 1;
        config.canton.admin_api_port = 1;
        let party = CantonId::parse(&format!("gov::{}", "0".repeat(68)))?;
        let packages = crate::config::default_package_config();

        assert!(
            get_governance_state(&config, &party, None, &packages)
                .await?
                .is_none()
        );
        assert!(
            read_governance_state(&config, &party, None, &packages)
                .await
                .is_err()
        );
        Ok(())
    }
}
