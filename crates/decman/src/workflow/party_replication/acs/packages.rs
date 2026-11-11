use std::collections::{BTreeSet, HashMap, HashSet};

use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        CumulativeFilter, EventFormat, Filters, GetActiveContractsRequest, GetLedgerEndRequest,
        WildcardFilter, cumulative_filter, get_active_contracts_response::ContractEntry,
    },
    digitalasset::canton::admin::participant::v30::{
        ListPackagesRequest, package_service_client::PackageServiceClient,
    },
};

use crate::{config::NodeConfig, error::Result, server::fetch_vetted_package_ids, utils};

/// Coordinator side: the distinct package ids referenced by the party's active
/// contracts. Shipped to the target so its ACS-import preflight can verify
/// it has every package the imported contracts need before disconnecting.
///
/// # Errors
///
/// Returns an error if the Ledger API state client cannot be created, or if
/// `GetLedgerEnd` fails. Returns an error if the `GetActiveContracts` stream
/// cannot be opened or fails while it is read.
pub async fn collect_party_package_ids(
    config: &NodeConfig,
    party_id: &str,
    ledger_token: Option<&str>,
) -> Result<Vec<String>> {
    let mut state = utils::create_state_client(config, ledger_token.map(str::to_string)).await?;
    let ledger_end = state
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {
            synchronizer_id: vec![],
        }))
        .await?
        .into_inner()
        .offset;

    let mut filters_by_party = HashMap::new();
    filters_by_party.insert(
        party_id.to_string(),
        Filters {
            cumulative: vec![CumulativeFilter {
                identifier_filter: Some(cumulative_filter::IdentifierFilter::WildcardFilter(
                    WildcardFilter {
                        include_created_event_blob: false,
                    },
                )),
            }],
        },
    );

    let request = GetActiveContractsRequest {
        include_incomplete_reassignments: false,
        active_at_offset: ledger_end,
        event_format: Some(EventFormat {
            filters_by_party,
            filters_for_any_party: None,
            verbose: false,
        }),
        stream_continuation_token: None,
    };

    let mut stream = state
        .get_active_contracts(tonic::Request::new(request))
        .await?
        .into_inner();

    let mut package_ids = BTreeSet::new();
    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
            && let Some(template_id) = created.template_id
        {
            package_ids.insert(template_id.package_id);
        }
    }
    Ok(package_ids.into_iter().collect())
}

/// New-member side: package ids currently known to this participant, via the
/// admin `PackageService.ListPackages`. Backs the ACS-import package preflight.
pub(super) async fn local_package_ids(config: &NodeConfig) -> Result<HashSet<String>> {
    let mut client = PackageServiceClient::new(config.admin_channel().await?);
    let descriptions = client
        .list_packages(tonic::Request::new(ListPackagesRequest {
            // 0 = no limit (the convention used across the codebase); a finite
            // cap could omit a required package on a participant with many
            // packages and wrongly fail the preflight.
            limit: 0,
            filter_name: String::new(),
        }))
        .await?
        .into_inner()
        .package_descriptions;
    Ok(descriptions.into_iter().map(|p| p.package_id).collect())
}

/// New-member side: refuse to join while this participant has vetted a
/// package it does not hold.
///
/// Vetting is synchronizer topology, but the package store is local, so the
/// two can disagree. An operator can vet a package that was never uploaded,
/// with `FORCE_FLAG_ALLOW_UNKNOWN_PACKAGE`. A restore from a backup taken
/// before a DAR upload brings back the old package store, while the vetting
/// stays on the synchronizer.
///
/// The coordinator checks the party's packages against this participant's
/// vetting only, and the import's own package preflight runs at `SyncAcs`,
/// after this member has left the synchronizer. Holding every vetted package,
/// together with the coordinator's check, puts every package the party needs
/// on this participant before it disconnects.
///
/// # Errors
///
/// Returns an error if the synchronizer ID cannot be resolved, or if the
/// topology read of this participant's vetting fails. Returns an error if
/// `ListPackages` fails, or if this participant has vetted a package it does
/// not hold.
pub async fn check_vetted_packages_held(config: &NodeConfig) -> Result {
    let vetted = fetch_vetted_package_ids(config, config.participant_id()).await?;
    let held = local_package_ids(config).await?;
    let missing = vetted_not_held(&vetted, &held);
    if !missing.is_empty() {
        anyhow::bail!(
            "this participant has vetted {n} package(s) it does not hold, as after a restore \
             from backup — re-upload the corresponding DAR(s), then retry the add. Missing \
             package ids: {missing:?}",
            n = missing.len()
        );
    }
    Ok(())
}

/// The vetted package ids missing from `held`, in vetting order.
fn vetted_not_held<'a>(vetted: &'a [String], held: &HashSet<String>) -> Vec<&'a str> {
    vetted
        .iter()
        .map(String::as_str)
        .filter(|id| !held.contains(*id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn vetted_not_held_names_each_missing_package() {
        let vetted = ids(&["a", "b", "c"]);
        let held: HashSet<String> = ids(&["b"]).into_iter().collect();

        assert_eq!(vetted_not_held(&vetted, &held), vec!["a", "c"]);
    }

    #[test]
    fn vetted_not_held_is_empty_when_every_vetted_package_is_held() {
        let vetted = ids(&["a", "b"]);
        let held: HashSet<String> = ids(&["a", "b"]).into_iter().collect();

        assert!(vetted_not_held(&vetted, &held).is_empty());
    }

    #[test]
    fn vetted_not_held_ignores_packages_held_but_not_vetted() {
        let vetted = ids(&["a"]);
        let held: HashSet<String> = ids(&["a", "unvetted"]).into_iter().collect();

        assert!(vetted_not_held(&vetted, &held).is_empty());
    }

    #[test]
    fn vetted_not_held_is_empty_when_nothing_is_vetted() {
        let held: HashSet<String> = ids(&["a"]).into_iter().collect();

        assert!(vetted_not_held(&[], &held).is_empty());
    }
}
