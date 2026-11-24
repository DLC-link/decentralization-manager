//! Raising (or lowering) an existing external party's confirmation threshold.
//!
//! A separate serial bump from [`super::add_hosts`], and deliberately so: a
//! newly added host does not count toward the threshold until its onboarding
//! marker clears, so a write that added hosts and raised the threshold together
//! would leave the party needing more confirmations than it has hosts able to
//! give. Add first, replicate, then raise.
//!
//! Canton's authorization rules make this the simplest of the topology writes —
//! a threshold change needs the party namespace alone, so no participant
//! co-signs and the party's own key is the whole authorization. The host still
//! validates before it submits, because it is submitting to its own store.

use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{
        PartyToParticipant, TopologyMapping, enums::ParticipantPermission, topology_mapping,
    },
    topology::admin::v30::{AuthorizeRequest, authorize_request, generate_transactions_request},
};
use serde::{Deserialize, Serialize};

use crate::{
    config::NodeConfig,
    utils,
    workflow::{
        external_party::{
            add_hosts::{
                AddHostsError, CurrentPartyTopology, owns_party_namespace,
                read_party_to_participant,
            },
            keys::PartySignatureScheme,
            topology_bundle::{
                BundleSerial, check_bundle_envelope, check_signature_forms, decode_p2p_change,
                generate_p2p_change, party_signed, read_pinned, serial_after, submit_signed,
            },
        },
        topology,
        topology_version::ProposalFormat,
    },
};

/// The unsigned threshold change, plus the hash the party must sign.
pub struct PreparedThreshold {
    pub party_id: String,
    /// The serial the returned transactions carry (`base_serial + 1`).
    pub serial: u32,
    pub transaction_hashes: Vec<Vec<u8>>,
    pub topology_transactions: Vec<Vec<u8>>,
}

/// The party-signed threshold change a wallet submits to a host.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExternalPartyThresholdPayload {
    pub party_id: String,
    pub base_serial: u32,
    pub topology_transactions: Vec<Vec<u8>>,
    pub signatures: Vec<Vec<u8>>,
    pub signed_by: String,
}

/// How many hosts can actually confirm for the party right now.
///
/// Hosts still carrying the onboarding marker are assigned but suspended: they
/// hold none of the party's contracts and confirm nothing. A threshold above
/// this number is one the party cannot meet.
fn active_host_count(mapping: &PartyToParticipant) -> u32 {
    mapping
        .participants
        .iter()
        .filter(|p| {
            // Unmarked AND at Confirmation. An Observation host is listed and
            // live but confirms nothing, so counting it would authorize a
            // threshold the party cannot meet — the same mistake the add-hosts
            // guard originally made.
            p.onboarding.is_none() && p.permission == ParticipantPermission::Confirmation as i32
        })
        .count() as u32
}

/// Check `new_threshold` against what the party can actually field.
fn validate_threshold(mapping: &PartyToParticipant, new_threshold: u32) -> anyhow::Result<()> {
    if new_threshold < 1 {
        anyhow::bail!("confirmation threshold must be at least 1, got {new_threshold}");
    }
    let active = active_host_count(mapping);
    if new_threshold > active {
        anyhow::bail!(
            "confirmation threshold {new_threshold} exceeds the {active} host(s) able to confirm \
             for this party; hosts still carrying the onboarding marker do not count until their \
             ACS import completes"
        );
    }
    Ok(())
}

/// Build the serial-N+1 mapping that changes only the threshold.
fn threshold_mapping(
    current: &PartyToParticipant,
    new_threshold: u32,
) -> anyhow::Result<PartyToParticipant> {
    validate_threshold(current, new_threshold)?;
    if current.threshold == new_threshold {
        anyhow::bail!("the party's confirmation threshold is already {new_threshold}");
    }
    Ok(PartyToParticipant {
        party: current.party.clone(),
        threshold: new_threshold,
        // Untouched. This write moves the threshold and nothing else.
        participants: current.participants.clone(),
        party_signing_keys: current.party_signing_keys.clone(),
    })
}

/// Raise or lower the threshold with this node's own key, for a party whose
/// namespace nobody can sign for from outside Canton.
///
/// [`submit_threshold`] cannot serve such a party. It submits the caller's
/// signature untouched and no node co-signs, because for a party born external
/// the namespace is the wallet's key and a node asked to sign would fail with
/// `TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE`. A party that is, or once was,
/// local has a participant's root key for a namespace instead: it lives in
/// Canton's vault, nothing outside Canton can sign with it, and converting the
/// party does not move it — the adopted key authorizes transactions, not
/// topology.
///
/// A vault key signs only through `Authorize`. Canton wants the party namespace
/// alone for a threshold change, so unlike an add this needs exactly one node
/// and one call: `must_fully_authorize` holds, and no second signature is
/// awaited. The call supplies the whole authorization; it does not wait for the
/// change to become effective, so a caller that needs the new serial polls for
/// it.
///
/// # Errors
/// [`AddHostsError`] variants, so a caller answers a stale pin, a threshold this
/// party cannot field, and a Canton failure differently.
pub async fn authorize_threshold(
    config: &NodeConfig,
    party_id: &str,
    new_threshold: u32,
    base_serial: u32,
) -> Result<u32, AddHostsError> {
    let Some(current) = read_party_to_participant(config, party_id)
        .await
        .map_err(AddHostsError::Canton)?
    else {
        return Err(AddHostsError::UnknownParty {
            party: party_id.to_string(),
        });
    };
    // Before the serial is even looked at. A node that owns nothing here must
    // answer the same way whether or not the change has landed: once P1's write
    // is live, a bystander reading head state would otherwise match the
    // retry-as-success case and be told it authorized something.
    if !owns_party_namespace(config, party_id) {
        return Err(AddHostsError::Invalid(anyhow::anyhow!(
            "this node does not own {party_id}'s namespace, so it holds no key that can \
             authorize the party's topology"
        )));
    }

    if base_serial != current.serial {
        // A retry whose first attempt landed must not read as a conflict, or a
        // caller that lost the response could never learn it succeeded. Only
        // when head state is exactly one past the pin AND already carries this
        // threshold; anything else is a genuine stale pin.
        if current.serial == base_serial.saturating_add(1)
            && current.mapping.threshold == new_threshold
        {
            tracing::info!(
                party_id = %party_id,
                threshold = new_threshold,
                "threshold already authorized; treating the retry as success"
            );
            return Ok(base_serial);
        }
        return Err(AddHostsError::StaleSerial {
            party: party_id.to_string(),
            pinned: base_serial,
            found: current.serial,
        });
    }

    let mapping =
        threshold_mapping(&current.mapping, new_threshold).map_err(AddHostsError::Invalid)?;
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
            // This node holds the only signature Canton wants, so the change is
            // authorized outright rather than left as a proposal.
            must_fully_authorize: true,
            force_changes: vec![],
            signed_by: vec![],
            store: Some(topology::synchronizer_store_id(&synchronizer_id)),
            wait_to_become_effective: None,
        },
        "external-party threshold authorize",
    )
    .await
    .map_err(AddHostsError::Canton)?;

    tracing::info!(
        party_id = %party_id,
        base_serial = current.serial,
        threshold = new_threshold,
        "external-party: threshold authorized with this node's own key"
    );

    Ok(current.serial)
}

/// Ask Canton to build the threshold change and the hash the party must sign.
///
/// # Errors
/// [`AddHostsError`] variants, so a host answers a stale pin, a refused
/// threshold and a Canton failure differently — see
/// [`AddHostsError`](crate::workflow::external_party::add_hosts::AddHostsError).
pub async fn prepare_threshold(
    config: &NodeConfig,
    party_id: &str,
    new_threshold: u32,
    base_serial: u32,
) -> Result<PreparedThreshold, AddHostsError> {
    let synchronizer_id = utils::get_synchronizer_id(config)
        .await
        .map_err(AddHostsError::Canton)?;

    let current = read_pinned(config, party_id, base_serial).await?;

    let mapping =
        threshold_mapping(&current.mapping, new_threshold).map_err(AddHostsError::Invalid)?;
    let generated = generate_p2p_change(
        config,
        party_id,
        current.serial,
        &synchronizer_id,
        generate_transactions_request::proposal::Mapping::V30(TopologyMapping {
            mapping: Some(topology_mapping::Mapping::PartyToParticipant(mapping)),
        }),
    )
    .await?;
    let next_serial = generated.serial;

    tracing::info!(
        %party_id,
        base_serial,
        next_serial,
        new_threshold,
        "external-party: generated threshold change"
    );

    Ok(PreparedThreshold {
        party_id: party_id.to_string(),
        serial: next_serial,
        transaction_hashes: generated.transaction_hashes,
        topology_transactions: generated.topology_transactions,
    })
}

/// Reject any submitted topology that is not exactly this threshold change.
///
/// Same reason the add-hosts validator exists: the host co-signs nothing here
/// (a threshold change needs only the party namespace) but it does submit to its
/// own store, and a bundle that moved the host set while claiming to move the
/// threshold would evict hosts under cover of a routine change.
///
/// # Errors
/// Returns an error if the bundle has no transactions or not one signature per
/// transaction. Returns an error if a signature has a form the party's key
/// cannot produce, or if the current serial cannot be advanced. Returns an
/// error if a transaction does not decode, is not an `ADD_REPLACE` at the next
/// serial, or carries no `PartyToParticipant`. Returns an error if a
/// transaction names another party or leaves the threshold unchanged. It also
/// fails if the new threshold is below 1, exceeds the number of hosts able to
/// confirm, or comes with any other change.
pub fn validate_threshold_topology(
    current: &CurrentPartyTopology,
    bundle: &ExternalPartyThresholdPayload,
) -> anyhow::Result<()> {
    check_bundle_envelope(
        "external-party threshold change",
        &bundle.topology_transactions,
        &bundle.signatures,
    )?;
    check_signature_forms(&current.mapping, &bundle.signed_by, &bundle.signatures)?;
    let next_serial = serial_after(current)?;

    for (index, serialized) in bundle.topology_transactions.iter().enumerate() {
        let p2p = decode_p2p_change(
            index,
            serialized,
            BundleSerial::Next {
                current: current.serial,
                next: next_serial,
            },
            "changes a threshold",
        )?;

        if p2p.party != current.mapping.party {
            anyhow::bail!(
                "topology transaction {index} is for party {found}, not {expected}",
                found = p2p.party,
                expected = current.mapping.party
            );
        }
        if p2p.threshold == current.mapping.threshold {
            anyhow::bail!(
                "topology transaction {index} does not change the threshold; it is already {t}",
                t = current.mapping.threshold
            );
        }
        validate_threshold(&current.mapping, p2p.threshold)?;

        // Everything except the threshold must be byte-identical to head state.
        // This is the check that stops an eviction riding along on a threshold
        // change, and the struct equality also refuses any field the per-field
        // checks above do not know about.
        let expected = PartyToParticipant {
            party: current.mapping.party.clone(),
            threshold: p2p.threshold,
            participants: current.mapping.participants.clone(),
            party_signing_keys: current.mapping.party_signing_keys.clone(),
        };
        if p2p != expected {
            anyhow::bail!(
                "topology transaction {index} changes more than the threshold and will not be \
                 submitted"
            );
        }
    }

    Ok(())
}

/// Submit the wallet-signed threshold change on this host.
///
/// # Errors
/// See [`prepare_threshold`].
pub async fn submit_threshold(
    config: &NodeConfig,
    bundle: &ExternalPartyThresholdPayload,
) -> Result<u32, AddHostsError> {
    let current = read_pinned(config, &bundle.party_id, bundle.base_serial).await?;
    validate_threshold_topology(&current, bundle).map_err(AddHostsError::Invalid)?;

    let synchronizer_id = utils::get_synchronizer_id(config)
        .await
        .map_err(AddHostsError::Canton)?;
    let store = topology::synchronizer_store_id(&synchronizer_id);

    let scheme = PartySignatureScheme::for_party(&current.mapping, &bundle.signed_by);
    let signed = party_signed(
        &bundle.topology_transactions,
        &bundle.signatures,
        // A threshold change needs the party namespace alone, so the
        // party's signature is the complete authorization — no host has to
        // add one, and this is not a proposal awaiting others.
        false,
        |signature| scheme.canton_signature(signature, &bundle.signed_by),
    );

    // Submitted as-is. Unlike an add, this host does NOT co-sign: Canton's rules
    // want the party namespace alone for a threshold change, and for an external
    // party that namespace is the wallet's key. Asking the node to sign fails
    // with TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE, because it correctly
    // has no key that can authorize this — the plan says the same thing in one
    // line: "Only the wallet signs."
    let co_signed = signed;

    submit_signed(
        config,
        co_signed,
        vec![],
        store,
        "external-party threshold change",
    )
    .await?;

    tracing::info!(
        party_id = %bundle.party_id,
        base_serial = current.serial,
        "external-party: threshold change submitted on this host"
    );

    Ok(current.serial)
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::{
        enums::ParticipantPermission,
        party_to_participant::{HostingParticipant, hosting_participant},
    };

    use super::*;

    fn host(tag: u8, onboarding: bool) -> HostingParticipant {
        HostingParticipant {
            participant_uid: format!("participant-{tag}::1220{digest}", digest = "aa".repeat(32)),
            permission: ParticipantPermission::Confirmation as i32,
            onboarding: onboarding.then_some(hosting_participant::Onboarding {}),
        }
    }

    /// Three hosts, but only two can confirm — the third is still onboarding.
    fn mapping(threshold: u32) -> PartyToParticipant {
        PartyToParticipant {
            party: "alice::1220aa".to_string(),
            threshold,
            participants: vec![host(1, false), host(2, false), host(3, true)],
            party_signing_keys: None,
        }
    }

    /// The whole reason the threshold raise is a separate write: a marked host
    /// is assigned but suspended, so counting it would set a threshold the
    /// party cannot meet until its ACS import finishes.
    #[test]
    fn a_marked_host_does_not_count_toward_the_threshold() {
        assert_eq!(active_host_count(&mapping(1)), 2);
        if let Err(e) = validate_threshold(&mapping(1), 2) {
            panic!("the two confirming hosts must support a threshold of 2: {e}");
        }
        let Err(e) = validate_threshold(&mapping(1), 3) else {
            panic!("a threshold of 3 must be refused while only 2 hosts can confirm");
        };
        assert!(e.to_string().contains("able to confirm"), "{e}");
    }

    #[test]
    fn rejects_a_threshold_below_one() {
        let Err(e) = validate_threshold(&mapping(1), 0) else {
            panic!("a threshold of 0 must be refused");
        };
        assert!(e.to_string().contains("at least 1"), "{e}");
    }

    #[test]
    fn rejects_a_change_that_changes_nothing() {
        let Err(e) = threshold_mapping(&mapping(2), 2) else {
            panic!("a no-op threshold change must be refused");
        };
        assert!(e.to_string().contains("already"), "{e}");
    }

    /// The host set must survive a threshold change byte-for-byte — otherwise
    /// an eviction could ride along on a routine-looking change.
    #[test]
    fn moves_the_threshold_and_nothing_else() {
        let current = mapping(1);
        let Ok(next) = threshold_mapping(&current, 2) else {
            panic!("raising to a supported threshold must succeed");
        };
        assert_eq!(next.threshold, 2);
        assert_eq!(next.participants, current.participants);
        assert_eq!(next.party, current.party);
        assert_eq!(next.party_signing_keys, current.party_signing_keys);
    }
}
