//! Converting a **local** party into an externally-signed one.
//!
//! A local party's namespace is its participant's root key, so the participant
//! authorizes the party's own topology and the party has no key of its own: the
//! node submits for it. Plan B1 of the scoping study is giving such a party a
//! key its owner holds, after which it transacts through
//! `InteractiveSubmissionService` and can be co-validated across several hosts
//! like any external party.
//!
//! Two facts shape this, both established by running it against Canton rather
//! than by reading the spec:
//!
//! * **The write needs two signatures.** `topology.proto`'s authorization table
//!   requires "party namespace + all the new signing key" for adding a signing
//!   key. The namespace half is the participant's own key. The other half is
//!   proof the caller actually holds the key it is asking Canton to trust, and
//!   only that key can produce it. So the source node cannot do the conversion
//!   alone, however much it might want to: the owner has to sign too.
//! * **The converted mapping must be Confirmation.** A party that signs its own
//!   transactions is not submitted for by its host, and Canton refuses
//!   Submission once the party is externally signed.
//!
//! What this does **not** change is the namespace. The party id embeds it and it
//! is permanent, so a converted party keeps its identity, its contracts and its
//! featured-app status, and its source node keeps sole control of its topology
//! forever. That asymmetry is inherent to a local party and cannot be engineered
//! away; it can only be disclosed.

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{Signature, SignatureFormat, SigningAlgorithmSpec, SigningKeysWithThreshold},
    protocol::v30::{
        PartyToParticipant, SignedTopologyTransaction, TopologyMapping, TopologyTransaction,
        enums::{ParticipantPermission, TopologyChangeOp},
        topology_mapping,
    },
    topology::admin::v30::{
        AddTransactionsRequest, ForceFlag, GenerateTransactionsRequest, SignTransactionsRequest,
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
        external_party::{
            add_hosts::{AddHostsError, CurrentPartyTopology, read_party_to_participant},
            steps::party_signing_key,
        },
        topology,
    },
};

/// The unsigned conversion, plus the hash the adopting key must sign.
pub struct PreparedAdoption {
    /// The local party being converted.
    pub party_id: String,
    /// The serial the returned transaction carries.
    pub serial: u32,
    /// Canton's hash for each transaction, for the owner's key to sign.
    pub transaction_hashes: Vec<Vec<u8>>,
    /// The serialized topology transactions to submit.
    pub topology_transactions: Vec<Vec<u8>>,
}

/// The owner-signed conversion bundle.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LocalPartyAdoptionPayload {
    pub party_id: String,
    pub base_serial: u32,
    /// The raw Ed25519 public key being adopted, so the validator can rebuild
    /// what the mapping must carry rather than trusting the submitted bytes.
    pub public_key: [u8; 32],
    pub topology_transactions: Vec<Vec<u8>>,
    pub signatures: Vec<Vec<u8>>,
    pub signed_by: String,
}

/// Whether `party_id`'s namespace is this participant's own, which is what makes
/// a party local to it.
///
/// The check matters because this endpoint only makes sense for a party the node
/// actually controls. For any other party the node cannot authorize the write
/// anyway, and refusing here gives a comprehensible error instead of a Canton
/// authorization failure.
fn is_local_to(config: &NodeConfig, party_id: &str) -> bool {
    let Some((_, namespace)) = party_id.rsplit_once("::") else {
        return false;
    };
    namespace == config.participant_id().namespace.to_hex()
}

/// Build the conversion mapping: the party's hosts unchanged except that this
/// node moves to Confirmation, plus the adopted key.
fn adoption_mapping(
    config: &NodeConfig,
    current: &PartyToParticipant,
    public_key: &[u8; 32],
) -> anyhow::Result<PartyToParticipant> {
    if current.party_signing_keys.is_some() {
        anyhow::bail!(
            "{party} already carries party signing keys; this converts a local party once",
            party = current.party
        );
    }

    let self_uid = config.participant_id().to_string();
    let mut participants = current.participants.clone();
    let Some(host) = participants
        .iter_mut()
        .find(|p| p.participant_uid == self_uid)
    else {
        anyhow::bail!("{self_uid} does not host {party}", party = current.party);
    };
    // Submission is what a local party looks like before conversion, and what
    // Canton refuses after it.
    host.permission = ParticipantPermission::Confirmation as i32;

    Ok(PartyToParticipant {
        party: current.party.clone(),
        threshold: current.threshold,
        participants,
        party_signing_keys: Some(SigningKeysWithThreshold {
            keys: vec![party_signing_key(public_key)],
            threshold: 1,
        }),
    })
}

/// Ask Canton to build the conversion and return the hash the owner must sign.
///
/// # Errors
/// [`AddHostsError`] variants, so a caller can tell a stale pin from a refused
/// conversion from a Canton failure.
pub async fn prepare_adoption(
    config: &NodeConfig,
    party_id: &str,
    public_key: &[u8; 32],
    base_serial: u32,
) -> std::result::Result<PreparedAdoption, AddHostsError> {
    if !is_local_to(config, party_id) {
        return Err(AddHostsError::Invalid(anyhow::anyhow!(
            "{party_id} is not local to this participant; only the node whose namespace owns a \
             party can give it a signing key"
        )));
    }

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
        adoption_mapping(config, &current.mapping, public_key).map_err(AddHostsError::Invalid)?;
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
    let generated = client
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
        .into_inner()
        .generated_transactions;

    if generated.is_empty() {
        return Err(AddHostsError::Canton(anyhow::anyhow!(
            "GenerateTransactions returned no transactions for {party_id}"
        )));
    }

    let mut transaction_hashes = Vec::with_capacity(generated.len());
    let mut topology_transactions = Vec::with_capacity(generated.len());
    for tx in generated {
        transaction_hashes.push(tx.transaction_hash);
        topology_transactions.push(tx.serialized_transaction);
    }

    tracing::info!(
        %party_id,
        base_serial,
        next_serial,
        "local-party: generated key-adoption topology"
    );

    Ok(PreparedAdoption {
        party_id: party_id.to_string(),
        serial: next_serial,
        transaction_hashes,
        topology_transactions,
    })
}

/// Reject any submitted conversion that is not exactly this change.
///
/// The node signs this with its own namespace key, and that key *is* the party's
/// namespace, so an unvalidated bundle is the node authorizing whatever the
/// caller wrote against a party it controls. Every field is checked against this
/// node's own head-state read and a key rebuilt from the submitted public half.
///
/// # Errors
/// Returns an error naming the first field that does not match.
pub fn validate_adoption_topology(
    config: &NodeConfig,
    current: &CurrentPartyTopology,
    bundle: &LocalPartyAdoptionPayload,
) -> anyhow::Result<()> {
    if bundle.topology_transactions.is_empty() {
        anyhow::bail!("local-party adoption submitted no topology transactions");
    }
    if bundle.signatures.len() != bundle.topology_transactions.len() {
        anyhow::bail!(
            "local-party adoption submitted {t} transaction(s) and {s} signature(s); they must \
             be index-aligned",
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

    // Rebuilt locally from the submitted public key, so the comparison below is
    // against what this node would itself have written.
    let expected = adoption_mapping(config, &current.mapping, &bundle.public_key)?;

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
                 endpoint converts a local party and authorizes nothing else"
            );
        };

        // Canton rewrites the key's usage (it appends ProofOfOwnership), so the
        // key is compared on material rather than on equality, exactly as the
        // onboarding validator does.
        let (Some(submitted), Some(want)) = (&p2p.party_signing_keys, &expected.party_signing_keys)
        else {
            anyhow::bail!("topology transaction {index} carries no party signing keys");
        };
        if submitted.threshold != 1 || submitted.keys.len() != 1 {
            anyhow::bail!(
                "topology transaction {index} must carry exactly one signing key at threshold 1"
            );
        }
        let (got, want) = (&submitted.keys[0], &want.keys[0]);
        if got.public_key != want.public_key
            || got.format != want.format
            || got.key_spec != want.key_spec
        {
            anyhow::bail!(
                "topology transaction {index} does not carry the submitted public key as the \
                 party's signing key"
            );
        }

        let rebuilt = PartyToParticipant {
            party: expected.party.clone(),
            threshold: expected.threshold,
            participants: expected.participants.clone(),
            party_signing_keys: Some(SigningKeysWithThreshold {
                keys: vec![got.clone()],
                threshold: 1,
            }),
        };
        if p2p != rebuilt {
            anyhow::bail!(
                "topology transaction {index} changes more than adopting the key and will not be \
                 authorized"
            );
        }
    }

    Ok(())
}

/// Co-sign the owner-signed conversion with this node's namespace key and
/// submit it.
///
/// Both halves are required and neither is optional: `topology.proto` demands
/// "party namespace + all the new signing key" for adding a signing key. The
/// namespace half is this node's, because a local party's namespace is its
/// participant's. The other half is the owner's, and it is the only proof that
/// whoever asked for this conversion actually holds the key they want the party
/// to answer to.
///
/// `AllowUnvalidatedSigningKeys` is passed for the same reason the decparty
/// proposal builder passes it: the adopted key has no `NamespaceDelegation`
/// behind it, since it is the owner's rather than a participant's.
///
/// # Errors
/// [`AddHostsError`] variants, so a caller can answer a stale pin, a refused
/// bundle and a Canton failure differently.
pub async fn submit_adoption(
    config: &NodeConfig,
    bundle: &LocalPartyAdoptionPayload,
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
    validate_adoption_topology(config, &current, bundle).map_err(AddHostsError::Invalid)?;

    let synchronizer_id = utils::get_synchronizer_id(config)
        .await
        .map_err(AddHostsError::Canton)?;
    let store = topology::synchronizer_store_id(&synchronizer_id);
    let force = vec![ForceFlag::AllowUnvalidatedSigningKeys as i32];

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
            proposal: false,
            multi_transaction_signatures: vec![],
        })
        .collect();

    let co_signed = topology::sign_transactions_with_topology_retry(
        config,
        SignTransactionsRequest {
            transactions: signed,
            signed_by: vec![],
            store: Some(store.clone()),
            force_flags: force.clone(),
        },
        "local-party key adoption",
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
            force_changes: force,
            store: Some(store),
            wait_to_become_effective: None,
        }))
        .await
        .map_err(|e| {
            AddHostsError::Canton(
                anyhow::Error::new(e).context("AddTransactions RPC failed for key adoption"),
            )
        })?;

    tracing::info!(
        party_id = %bundle.party_id,
        base_serial = current.serial,
        "local-party: key adoption submitted"
    );

    Ok(current.serial)
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::party_to_participant::{
        HostingParticipant, hosting_participant,
    };

    use crate::canton_id::CantonId;

    use super::*;

    const OWNER_KEY: [u8; 32] = [4u8; 32];

    fn participant() -> CantonId {
        match CantonId::parse(&format!("participant-1::1220{}", "aa".repeat(32))) {
            Ok(id) => id,
            Err(e) => panic!("test participant must parse: {e}"),
        }
    }

    fn config() -> NodeConfig {
        let mut config = NodeConfig::default();
        config.node.participant_id = Some(participant());
        config
    }

    /// A local party: its namespace is the participant's own.
    fn local_party_id() -> String {
        format!("kairo::{ns}", ns = participant().namespace.to_hex())
    }

    fn local_mapping() -> PartyToParticipant {
        PartyToParticipant {
            party: local_party_id(),
            threshold: 1,
            participants: vec![HostingParticipant {
                participant_uid: participant().to_string(),
                permission: ParticipantPermission::Submission as i32,
                onboarding: None,
            }],
            party_signing_keys: None,
        }
    }

    /// The definition of local, and the reason the party can never be
    /// decentralized in place: its namespace is the participant's.
    #[test]
    fn recognises_a_party_local_to_this_node() {
        assert!(is_local_to(&config(), &local_party_id()));
        assert!(!is_local_to(
            &config(),
            &format!("alice::1220{}", "bb".repeat(32))
        ));
        assert!(!is_local_to(&config(), "malformed"));
    }

    /// Canton refuses Submission once a party signs its own transactions, so the
    /// conversion has to demote its host in the same write.
    #[test]
    fn demotes_the_host_to_confirmation() {
        let Ok(mapping) = adoption_mapping(&config(), &local_mapping(), &OWNER_KEY) else {
            panic!("converting a local party must succeed");
        };
        assert_eq!(
            mapping.participants[0].permission,
            ParticipantPermission::Confirmation as i32
        );
    }

    #[test]
    fn adopts_exactly_one_key_at_threshold_one() {
        let Ok(mapping) = adoption_mapping(&config(), &local_mapping(), &OWNER_KEY) else {
            panic!("converting a local party must succeed");
        };
        let Some(keys) = mapping.party_signing_keys else {
            panic!("the conversion must carry the adopted key");
        };
        assert_eq!(keys.threshold, 1);
        assert_eq!(keys.keys.len(), 1);
        assert_eq!(
            keys.keys[0].public_key,
            party_signing_key(&OWNER_KEY).public_key
        );
    }

    /// Converting twice would replace the owner's key with someone else's, which
    /// is the one way this endpoint could hand a party away.
    #[test]
    fn refuses_a_party_that_already_has_a_key() {
        let mut mapping = local_mapping();
        mapping.party_signing_keys = Some(SigningKeysWithThreshold {
            keys: vec![party_signing_key(&[9u8; 32])],
            threshold: 1,
        });
        let Err(e) = adoption_mapping(&config(), &mapping, &OWNER_KEY) else {
            panic!("a second conversion must be refused");
        };
        assert!(e.to_string().contains("already carries"), "{e}");
    }

    #[test]
    fn refuses_a_party_this_node_does_not_host() {
        let mut mapping = local_mapping();
        mapping.participants[0] = HostingParticipant {
            participant_uid: format!("participant-9::1220{}", "cc".repeat(32)),
            permission: ParticipantPermission::Submission as i32,
            onboarding: Some(hosting_participant::Onboarding {}),
        };
        let Err(e) = adoption_mapping(&config(), &mapping, &OWNER_KEY) else {
            panic!("a party this node does not host must be refused");
        };
        assert!(e.to_string().contains("does not host"), "{e}");
    }

    /// The threshold is not this write's business, and moving it here would let
    /// a conversion quietly change who has to confirm.
    #[test]
    fn leaves_the_threshold_and_the_host_set_alone() {
        let current = local_mapping();
        let Ok(mapping) = adoption_mapping(&config(), &current, &OWNER_KEY) else {
            panic!("converting must succeed");
        };
        assert_eq!(mapping.threshold, current.threshold);
        assert_eq!(mapping.participants.len(), current.participants.len());
        assert_eq!(mapping.party, current.party);
    }
}
