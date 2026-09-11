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

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{Signature, SignatureFormat, SigningAlgorithmSpec},
    protocol::v30::{
        PartyToParticipant, SignedTopologyTransaction, TopologyMapping, TopologyTransaction,
        enums::{ParticipantPermission, TopologyChangeOp},
        topology_mapping,
    },
    topology::admin::v30::{
        AddTransactionsRequest, GenerateTransactionsRequest, SignTransactionsRequest,
        generate_transactions_request,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
    version::v1::{UntypedVersionedMessage, untyped_versioned_message},
};
use prost::Message as _;
use serde::{Deserialize, Serialize};

use crate::{
    config::NodeConfig,
    utils,
    workflow::{
        external_party::add_hosts::{
            AddHostsError, CurrentPartyTopology, read_party_to_participant,
        },
        topology,
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
) -> std::result::Result<PreparedThreshold, AddHostsError> {
    let synchronizer_id = utils::get_synchronizer_id(config)
        .await
        .map_err(AddHostsError::Canton)?;

    let Some(current) = read_party_to_participant(config, party_id)
        .await
        .map_err(AddHostsError::Canton)?
    else {
        return Err(AddHostsError::UnknownParty {
            party: party_id.to_string(),
        });
    };
    if current.serial != base_serial {
        return Err(AddHostsError::StaleSerial {
            party: party_id.to_string(),
            pinned: base_serial,
            found: current.serial,
        });
    }

    let mapping =
        threshold_mapping(&current.mapping, new_threshold).map_err(AddHostsError::Invalid)?;
    let next_serial = current.serial.checked_add(1).ok_or_else(|| {
        AddHostsError::Invalid(anyhow::anyhow!(
            "{party_id} is at serial {s}, which cannot be advanced",
            s = current.serial
        ))
    })?;

    let mut client = TopologyManagerWriteServiceClient::new(
        config
            .admin_channel()
            .await
            .map_err(AddHostsError::Canton)?,
    );
    let response = client
        .generate_transactions(tonic::Request::new(GenerateTransactionsRequest {
            proposals: vec![generate_transactions_request::Proposal {
                operation: TopologyChangeOp::AddReplace as i32,
                serial: next_serial,
                mapping: Some(generate_transactions_request::proposal::Mapping::V30(
                    TopologyMapping {
                        mapping: Some(topology_mapping::Mapping::PartyToParticipant(mapping)),
                    },
                )),
                store: Some(topology::synchronizer_store_id(&synchronizer_id)),
            }],
            base_request: None,
        }))
        .await
        .map_err(|e| {
            AddHostsError::Canton(anyhow::Error::new(e).context("GenerateTransactions RPC failed"))
        })?
        .into_inner();

    if response.generated_transactions.is_empty() {
        return Err(AddHostsError::Canton(anyhow::anyhow!(
            "GenerateTransactions returned no transactions for {party_id}"
        )));
    }

    let mut transaction_hashes = Vec::with_capacity(response.generated_transactions.len());
    let mut topology_transactions = Vec::with_capacity(response.generated_transactions.len());
    for tx in response.generated_transactions {
        transaction_hashes.push(tx.transaction_hash);
        topology_transactions.push(tx.serialized_transaction);
    }

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
        transaction_hashes,
        topology_transactions,
    })
}

/// Reject any submitted topology that is not exactly this threshold change.
///
/// Same reason the add-hosts validator exists: the host co-signs nothing here
/// (a threshold change needs only the party namespace) but it does submit to its
/// own store, and a bundle that moved the host set while claiming to move the
/// threshold would evict hosts under cover of a routine change.
pub fn validate_threshold_topology(
    current: &CurrentPartyTopology,
    bundle: &ExternalPartyThresholdPayload,
) -> anyhow::Result<()> {
    if bundle.topology_transactions.is_empty() {
        anyhow::bail!("external-party threshold change submitted no topology transactions");
    }
    if bundle.signatures.len() != bundle.topology_transactions.len() {
        anyhow::bail!(
            "external-party threshold change submitted {t} transaction(s) and {s} signature(s); \
             they must be index-aligned",
            t = bundle.topology_transactions.len(),
            s = bundle.signatures.len()
        );
    }
    let next_serial = current.serial.checked_add(1).with_context(|| {
        format!(
            "{party} is at serial {s}, which cannot be advanced",
            party = current.mapping.party,
            s = current.serial
        )
    })?;

    for (index, serialized) in bundle.topology_transactions.iter().enumerate() {
        let versioned =
            UntypedVersionedMessage::decode(serialized.as_slice()).with_context(|| {
                format!("topology transaction {index} is not a versioned Canton message")
            })?;
        let Some(untyped_versioned_message::Wrapper::Data(inner)) = versioned.wrapper else {
            anyhow::bail!("topology transaction {index} carries no payload");
        };
        let transaction = TopologyTransaction::decode(inner.as_slice()).with_context(|| {
            format!("topology transaction {index} is not a TopologyTransaction")
        })?;

        if transaction.operation != TopologyChangeOp::AddReplace as i32 {
            anyhow::bail!(
                "topology transaction {index} must be an ADD_REPLACE, got operation {op}",
                op = transaction.operation
            );
        }
        if transaction.serial != next_serial {
            anyhow::bail!(
                "topology transaction {index} has serial {serial}, which must be exactly one past \
                 the current {current_serial}",
                serial = transaction.serial,
                current_serial = current.serial
            );
        }

        let Some(TopologyMapping {
            mapping: Some(topology_mapping::Mapping::PartyToParticipant(p2p)),
        }) = transaction.mapping
        else {
            anyhow::bail!(
                "topology transaction {index} must carry a PartyToParticipant mapping; this \
                 endpoint changes a threshold and authorizes nothing else"
            );
        };

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
) -> std::result::Result<u32, AddHostsError> {
    let Some(current) = read_party_to_participant(config, &bundle.party_id)
        .await
        .map_err(AddHostsError::Canton)?
    else {
        return Err(AddHostsError::UnknownParty {
            party: bundle.party_id.clone(),
        });
    };
    if bundle.base_serial != current.serial {
        return Err(AddHostsError::StaleSerial {
            party: bundle.party_id.clone(),
            pinned: bundle.base_serial,
            found: current.serial,
        });
    }
    validate_threshold_topology(&current, bundle).map_err(AddHostsError::Invalid)?;

    let synchronizer_id = utils::get_synchronizer_id(config)
        .await
        .map_err(AddHostsError::Canton)?;
    let store = topology::synchronizer_store_id(&synchronizer_id);

    let signed: Vec<SignedTopologyTransaction> = bundle
        .topology_transactions
        .iter()
        .zip(&bundle.signatures)
        .map(|(transaction, signature)| SignedTopologyTransaction {
            transaction: transaction.clone(),
            signatures: vec![Signature {
                format: SignatureFormat::Concat as i32,
                signature: signature.clone(),
                signed_by: bundle.signed_by.clone(),
                signing_algorithm_spec: SigningAlgorithmSpec::Ed25519 as i32,
                signature_delegation: None,
            }],
            // A threshold change needs the party namespace alone, so the
            // party's signature is the complete authorization — no host has to
            // add one, and this is not a proposal awaiting others.
            proposal: false,
            multi_transaction_signatures: vec![],
        })
        .collect();

    // Still routed through the signing call so the node's own store accepts it
    // the same way every other topology write does; with a complete signature
    // set this adds nothing and changes nothing.
    let co_signed = topology::sign_transactions_with_topology_retry(
        config,
        SignTransactionsRequest {
            transactions: signed,
            signed_by: vec![],
            store: Some(store.clone()),
            force_flags: vec![],
        },
        "external-party threshold change",
    )
    .await
    .map_err(AddHostsError::Canton)?
    .transactions;

    let mut client = TopologyManagerWriteServiceClient::new(
        config
            .admin_channel()
            .await
            .map_err(AddHostsError::Canton)?,
    );
    client
        .add_transactions(tonic::Request::new(AddTransactionsRequest {
            transactions: co_signed,
            force_changes: vec![],
            store: Some(store),
            wait_to_become_effective: None,
        }))
        .await
        .map_err(|e| {
            AddHostsError::Canton(
                anyhow::Error::new(e)
                    .context("AddTransactions RPC failed for external-party threshold change"),
            )
        })?;

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
            participant_uid: format!("participant-{tag}::1220{}", "aa".repeat(32)),
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
