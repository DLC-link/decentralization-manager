use std::collections::HashSet;

use super::AddPartyConfig;
use crate::{
    canton_id::CantonId, config::NodeConfig, error::Result, server::fetch_vetted_package_ids,
    workflow::party_replication::collect_party_package_ids,
};

/// A refusal to add a member that has not vetted every package the party's
/// contracts need. The message says what the operator must do, so it is safe
/// to show as it is.
#[derive(Debug, thiserror::Error)]
#[error(
    "the new member {participant} has not vetted {n} package(s) required by the party's \
     contracts — upload and vet the corresponding DAR(s) on it before adding it. Missing \
     package ids: {missing:?}",
    n = .missing.len()
)]
pub struct MissingPackages {
    pub(crate) participant: CantonId,
    pub(crate) missing: Vec<String>,
}

/// Refuse the add if the new member has not vetted a package the party's
/// contracts need.
///
/// The start request runs this before it sends any invite. `ExportState` runs
/// it again before any topology is written, because the peers can take hours
/// to connect and the party can gain contracts from a new package in that time.
///
/// `import_party_acs` checks this too, but only at `SyncAcs` — by then
/// `SubmitProposals` has hosted the party on the new participant with the
/// onboarding marker, so it is already receiving the party's ledger traffic
/// with no ACS behind it. Every archive of a contract created before that lands
/// in its ACS journal with no activation to derive a reassignment counter from,
/// and those orphans make the participant fail its next reconnect.
///
/// The new member's vetting comes from this node's own topology store, so no
/// peer is contacted. That store records vetting, not uploads, so this check is
/// stricter than the import's: a package the new member holds but has not
/// vetted is refused here too. A package it has vetted but no longer holds is
/// the new member's own check, `check_vetted_packages_held`, before it signs.
///
/// # Errors
/// Returns [`MissingPackages`] when the new member lacks a package, and any
/// other error when the party's contracts or the vetting cannot be read.
pub(crate) async fn check_new_member_packages(
    config: &NodeConfig,
    add_party_config: &AddPartyConfig,
    ledger_token: Option<&str>,
) -> Result {
    let party_id = add_party_config.decentralized_party_id.to_string();
    let required = collect_party_package_ids(config, &party_id, ledger_token).await?;
    if required.is_empty() {
        return Ok(());
    }

    let new_participant = &add_party_config.new_participant_id;
    let vetted: HashSet<String> = fetch_vetted_package_ids(config, new_participant)
        .await?
        .into_iter()
        .collect();

    let missing: Vec<String> = required
        .iter()
        .filter(|id| !vetted.contains(*id))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(MissingPackages {
            participant: new_participant.clone(),
            missing,
        }
        .into());
    }

    tracing::info!(
        "New member has vetted all {n} package(s) the party's contracts reference",
        n = required.len()
    );
    Ok(())
}
