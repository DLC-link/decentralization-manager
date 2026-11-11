use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::topology_mapping,
    topology::admin::v30::{AuthorizeRequest, authorize_request},
};
use common::api::HostPermission;
use sqlx::SqlitePool;

use super::{
    error::AddHostsError,
    head::read_party_to_participant,
    mapping::{add_hosts_mapping, hosts_present},
    replication::capture_replication_offsets,
};
use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    utils,
    workflow::{topology, topology_version::ProposalFormat},
};

/// Which half of Canton's "party namespace + the new participant namespace" this
/// node can sign, or `None` when it can sign neither.
///
/// A node that holds neither would spend its key on a signature Canton has no
/// use for, and the caller would read the resulting proposal as progress.
///
/// The party's namespace is the fingerprint half of its id. A node owns it when
/// the party is local to that node, or was before it adopted a signing key —
/// adopting one adds a `party_signing_keys` entry and never moves the namespace.
fn authorizing_role(
    config: &NodeConfig,
    party_id: &str,
    new_hosts: &[CantonId],
) -> Option<AuthorizingRole> {
    if owns_party_namespace(config, party_id) {
        return Some(AuthorizingRole::NamespaceOwner);
    }
    if new_hosts.contains(config.participant_id()) {
        return Some(AuthorizingRole::JoiningHost);
    }
    None
}

/// Is this party's namespace this node's own participant namespace?
///
/// True for a party that is local to this node, and for one that was before it
/// adopted a signing key — adopting one adds a `party_signing_keys` entry and
/// never moves the namespace, which the party id embeds. Such a namespace key
/// lives in Canton's vault, so this node is the only place its signature can
/// come from, and it can only come through `Authorize`.
pub fn owns_party_namespace(config: &NodeConfig, party_id: &str) -> bool {
    party_id
        .rsplit_once("::")
        .is_some_and(|(_, ns)| ns == config.participant_id().namespace.to_hex())
}

/// The half of the authorization a node contributes, for logging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthorizingRole {
    NamespaceOwner,
    JoiningHost,
}

impl AuthorizingRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::NamespaceOwner => "namespace-owner",
            Self::JoiningHost => "joining-host",
        }
    }
}

/// Authorize the add-hosts change with this node's own key, for a party whose
/// namespace nobody can sign for from outside Canton.
///
/// The wallet-signed path cannot serve such a party. `validate_add_hosts_topology`
/// requires `signed_by` to be the party's namespace, and for a party that is (or
/// once was) local that namespace is a participant's root key: it lives in
/// Canton's vault, and no admin RPC signs a hash with a vault key. Converting the
/// party does not move the namespace either — the adopted key is a
/// `party_signing_keys` entry with no `NamespaceDelegation` behind it, so it
/// signs transactions and authorizes no topology at all.
///
/// A vault key signs only through `Authorize`, so that is what this uses. Canton
/// wants "party namespace + the new participant namespace" for an add, which no
/// single node holds: the source node contributes the first half and each joining
/// node the second. `must_fully_authorize` is therefore false — the write stays a
/// proposal until the last node calls this, and Canton assembles it.
///
/// Every node builds the mapping itself from its own head-state read, exactly as
/// [`prepare_add_hosts`](super::prepare_add_hosts) does, so a caller cannot hand one node a different
/// change from another.
///
/// # Errors
/// [`AddHostsError`] variants, so a caller can answer a stale pin, a bundle this
/// node may not authorize, and a Canton failure differently.
pub async fn authorize_add_hosts(
    config: &NodeConfig,
    storage: &SqlitePool,
    party_id: &str,
    new_hosts: &[CantonId],
    permission: HostPermission,
    base_serial: u32,
    ledger_token: Option<&str>,
) -> std::result::Result<u32, AddHostsError> {
    let Some(current) = read_party_to_participant(config, party_id)
        .await
        .map_err(AddHostsError::Canton)?
    else {
        return Err(AddHostsError::UnknownParty {
            party: party_id.to_string(),
        });
    };
    // Before the serial is even looked at. A node that holds neither half must
    // answer the same way whether or not the add has landed: once it is live, a
    // bystander reading head state would otherwise match the retry-as-success
    // case below and be told it authorized something.
    let Some(role) = authorizing_role(config, party_id, new_hosts) else {
        return Err(AddHostsError::Invalid(anyhow::anyhow!(
            "this node neither owns {party_id}'s namespace nor is joining it, so it holds no \
             key Canton needs for the add"
        )));
    };

    if base_serial != current.serial {
        // The second node's call is what makes the change live, so by the time a
        // driver retries either call the serial may already have advanced. A
        // retry whose first attempt landed must not read as a conflict, or a
        // driver that lost the response could never learn it succeeded.
        //
        // Only when head state is exactly one past the pin AND already hosts
        // what this call asked for. Anything else is a genuine stale pin: some
        // other write could have moved the party, and calling that success would
        // report a change that is not live.
        if current.serial == base_serial.saturating_add(1)
            && hosts_present(&current.mapping, new_hosts, permission)
        {
            tracing::info!(
                party_id = %party_id,
                serial = current.serial,
                "add-hosts already authorized; treating the retry as success"
            );
            return Ok(base_serial);
        }
        return Err(AddHostsError::StaleSerial {
            party: party_id.to_string(),
            pinned: base_serial,
            found: current.serial,
        });
    }

    let mapping = add_hosts_mapping(&current.mapping, new_hosts, permission)
        .map_err(AddHostsError::Invalid)?;
    capture_replication_offsets(
        config,
        storage,
        party_id,
        new_hosts,
        base_serial,
        ledger_token,
    )
    .await?;
    let synchronizer_id = utils::get_synchronizer_id(config)
        .await
        .map_err(AddHostsError::Canton)?;
    let next_serial = current.serial.checked_add(1).ok_or_else(|| {
        AddHostsError::Invalid(anyhow::anyhow!(
            "{party_id} is at serial {s}, which cannot be advanced",
            s = current.serial
        ))
    })?;

    topology::authorize_with_topology_retry(
        config,
        AuthorizeRequest {
            r#type: Some(authorize_request::Type::Proposal(
                ProposalFormat::for_synchronizer(&synchronizer_id).proposal(
                    next_serial,
                    topology_mapping::Mapping::PartyToParticipant(mapping),
                ),
            )),
            // Neither node holds both halves, so demanding full authorization
            // would fail on whichever one calls first.
            must_fully_authorize: false,
            force_changes: vec![],
            signed_by: vec![],
            store: Some(topology::synchronizer_store_id(&synchronizer_id)),
            wait_to_become_effective: None,
        },
        "external-party add-hosts authorize",
    )
    .await
    .map_err(AddHostsError::Canton)?;

    tracing::info!(
        party_id = %party_id,
        base_serial = current.serial,
        role = role.as_str(),
        "external-party: add-hosts authorized with this node's own key"
    );

    Ok(current.serial)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::external_party::add_hosts::test_support::*;

    /// The node whose root key IS the party's namespace signs the "party
    /// namespace" half. This is the case the wallet-signed path cannot serve:
    /// that key lives in Canton's vault and no RPC signs a hash with it.
    #[test]
    fn the_namespace_owner_authorizes_as_owner() {
        let config = test_config(1);
        let party = format!("alice::{ns}", ns = participant(1).namespace.to_hex());
        assert_eq!(
            authorizing_role(&config, &party, &[participant(2)]),
            Some(AuthorizingRole::NamespaceOwner)
        );
    }

    /// A joining node signs the "new participant" half, whoever owns the
    /// namespace. Both calls are needed, which is why neither may be skipped.
    #[test]
    fn a_joining_node_authorizes_as_joiner() {
        let config = test_config(2);
        let party = format!("alice::{ns}", ns = participant(1).namespace.to_hex());
        assert_eq!(
            authorizing_role(&config, &party, &[participant(2)]),
            Some(AuthorizingRole::JoiningHost)
        );
    }

    /// A node holding neither half would spend its key on a signature Canton has
    /// no use for, and the caller would read the proposal it leaves behind as
    /// progress.
    #[test]
    fn a_bystander_node_cannot_authorize() {
        let config = test_config(3);
        let party = format!("alice::{ns}", ns = participant(1).namespace.to_hex());
        assert_eq!(authorizing_role(&config, &party, &[participant(2)]), None);
    }

    /// An id with no namespace half is not a party id, and treating its missing
    /// namespace as a match would let any node authorize anything.
    #[test]
    fn a_party_id_without_a_namespace_cannot_authorize() {
        assert_eq!(
            authorizing_role(&test_config(1), "alice", &[participant(2)]),
            None
        );
    }
}
