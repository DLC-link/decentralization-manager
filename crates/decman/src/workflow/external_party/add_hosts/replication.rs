use sqlx::SqlitePool;

use super::error::AddHostsError;
use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    workflow::{
        party_replication::{
            ArtifactStore, ReplicationArtifacts, ReplicationTarget, capture_offset_once,
        },
        storage::artifact_kinds,
    },
};

/// The artifact keys the tenant add-hosts replication uses. Distinct from the
/// decparty add-party keys so the two flows cannot read each other's offsets.
pub const TENANT_ADD_HOSTS_ARTIFACTS: ReplicationArtifacts = ReplicationArtifacts {
    export_offset: artifact_kinds::TENANT_ADD_HOSTS_EXPORT_OFFSET,
    pre_activation_offset: artifact_kinds::TENANT_ADD_HOSTS_PRE_ACTIVATION_OFFSET,
    import_inflight: artifact_kinds::TENANT_ADD_HOSTS_IMPORT_INFLIGHT,
};

/// The run name a tenant add-hosts replication stores its artifacts under.
///
/// The tenant API has no workflow run to key artifacts by — it is stateless
/// HTTP, and Canton is the coordination store. This derives a stable name from
/// what the replication is about, so every host computes the same one without
/// coordinating, and two concurrent add-hosts for different joiners cannot
/// collide.
///
/// The base serial is part of the key, and that is load-bearing rather than
/// decorative. Tenant artefacts are never deleted and the offset capture is
/// once-only, so a key of `(party, target)` alone would have a target that was
/// removed and later re-added reuse the *first* attempt's offsets. `offset.rs`
/// explains why that is dangerous: an offset predating the earlier activation
/// makes `ExportPartyAcs` find the stale flag-less activation and abort with
/// INVALID_STATE. A re-add always happens at a different serial, so including it
/// gives the second attempt its own offsets.
pub fn replication_instance(party_id: &str, target: &CantonId, base_serial: u32) -> String {
    format!("tenant-add-hosts:{party_id}:{target}:{base_serial}")
}

/// Whether `instance` names a tenant add-hosts replication of `party_id`, as
/// built by [`replication_instance`].
///
/// The party id is followed by `:` and then the target's id, which never starts
/// with `:`. Checking that character keeps `alice::12` from matching an instance
/// of `alice::123`, and a party id cut short at its `::` from matching at all.
pub fn instance_is_for_party(instance: &str, party_id: &str) -> bool {
    instance
        .strip_prefix("tenant-add-hosts:")
        .and_then(|rest| rest.strip_prefix(party_id))
        .and_then(|rest| rest.strip_prefix(':'))
        .is_some_and(|target| !target.is_empty() && !target.starts_with(':'))
}

/// The replication this add-hosts implies: `party_id` moving onto `target`.
///
/// # Errors
/// Returns an error if `party_id` does not parse as a `CantonId`. A valid id
/// splits on `::` into a prefix and a valid namespace hex string.
pub fn replication_target(
    party_id: &str,
    target: &CantonId,
    base_serial: u32,
) -> Result<ReplicationTarget> {
    Ok(ReplicationTarget::new(
        CantonId::parse(party_id)?,
        target.clone(),
        replication_instance(party_id, target, base_serial),
        TENANT_ADD_HOSTS_ARTIFACTS,
        // No workflow run behind a tenant call, so the run-scoped artifact
        // table's foreign key cannot hold these.
        ArtifactStore::Tenant,
    ))
}
/// Capture the replication offsets for every joining host, before the topology
/// is submitted.
///
/// `ExportPartyAcs` and `ClearPartyOnboardingFlag` both search forward from an
/// offset that must predate the party's activation on the joiner, so a capture
/// taken any later would look for it in a window that no longer contains it.
/// Every node captures both keys, because whichever entry point a caller uses is
/// called on all of them and none knows yet which role it will play. The capture
/// is once-only, so a retry keeps the original value and calling this from both
/// entry points costs nothing.
///
/// # Errors
/// [`AddHostsError::Invalid`] for a host that is not a valid replication target,
/// [`AddHostsError::Canton`] if the offset read fails.
pub(super) async fn capture_replication_offsets(
    config: &NodeConfig,
    storage: &SqlitePool,
    party_id: &str,
    new_hosts: &[CantonId],
    base_serial: u32,
    ledger_token: Option<&str>,
) -> Result<(), AddHostsError> {
    for host in new_hosts {
        let target =
            replication_target(party_id, host, base_serial).map_err(AddHostsError::Invalid)?;
        capture_offset_once(
            config,
            storage,
            &target,
            target.artifacts.export_offset,
            None,
            ledger_token,
            "tenant add-hosts export",
        )
        .await
        .map_err(AddHostsError::Canton)?;
        capture_offset_once(
            config,
            storage,
            &target,
            target.artifacts.pre_activation_offset,
            Some(&config.participant_id().to_string()),
            ledger_token,
            "tenant add-hosts pre-activation",
        )
        .await
        .map_err(AddHostsError::Canton)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::external_party::add_hosts::test_support::*;

    /// An unhost refuses while a relay for the party is open, and finds that
    /// relay by its instance name. A match on a longer party id would block a
    /// party for another party's transfer.
    #[test]
    fn an_instance_name_matches_only_its_own_party() {
        let instance = replication_instance("alice::12", &participant(3), 4);
        assert!(instance_is_for_party(&instance, "alice::12"));
        assert!(!instance_is_for_party(&instance, "alice::1"));
        assert!(!instance_is_for_party(&instance, "alice"));
        assert!(!instance_is_for_party(&instance, "bob::12"));
        let longer = replication_instance("alice::123", &participant(3), 4);
        assert!(!instance_is_for_party(&longer, "alice::12"));
    }
}
