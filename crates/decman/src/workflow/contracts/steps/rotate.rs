//! The rotation that makes a run's one-shot keys the party's signing keys.
//!
//! Each signer adds one key. The rotation sets the party's signing keys to
//! exactly those keys, at a threshold equal to their number. A coordinator
//! that slips in a key of its own therefore gains nothing: every honest
//! signer's key must still sign each create. Every other field of the
//! party's mapping stays as it is.

use std::{collections::BTreeSet, time::Duration};

use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{Signature, SigningKeysWithThreshold, SigningPublicKey},
    protocol::v30::{
        PartyToParticipant, SignedTopologyTransaction, TopologyTransaction, enums, topology_mapping,
    },
    topology::admin::v30::SignTransactionsRequest,
};
use sqlx::SqlitePool;

use crate::{
    canton_id::{CantonId, Fingerprint},
    config::NodeConfig,
    consts::{topology_retry_delay_secs, topology_retry_max_attempts},
    error::Result,
    signing::{
        PreparedTransactionHash,
        one_shot::{self, OneShotKey},
    },
    utils,
    workflow::{
        contracts::steps::sign::{
            encode_messages_length_prefixed, read_all_messages_from_bytes, verified_hashes,
        },
        proposal_store, topology, topology_version,
    },
};

/// Build the rotation to `keys` and sign it with this node's namespace key.
///
/// The rotation is signed in a temporary store and is not published.
///
/// # Errors
///
/// Errors when the party's head cannot be read or Canton refuses to sign.
pub async fn build_rotation(
    config: &NodeConfig,
    synchronizer_id: &str,
    instance_name: &str,
    dec_party_id: &CantonId,
    keys: Vec<SigningPublicKey>,
) -> Result<SignedTopologyTransaction> {
    let head = proposal_store::fetch_party_head(config, synchronizer_id, dec_party_id).await?;
    let threshold = u32::try_from(keys.len())?;
    let rotated = PartyToParticipant {
        party_signing_keys: Some(SigningKeysWithThreshold { keys, threshold }),
        ..head.p2p.clone()
    };
    let signed = proposal_store::sign_party_proposals(
        config,
        synchronizer_id,
        instance_name,
        dec_party_id,
        &head,
        None,
        rotated,
        topology::party_proposal_force_flags(),
    )
    .await?;
    Ok(signed.p2p)
}

/// Check a rotation against this node's own view of the party, before this
/// node signs it. Returns the keys the rotation installs.
///
/// # Errors
///
/// Errors when the rotation changes anything but the signing keys, when it
/// leaves out `own`, or when its keys or threshold are not what a one-shot
/// deploy installs.
pub async fn check_rotation(
    config: &NodeConfig,
    synchronizer_id: &str,
    dec_party_id: &CantonId,
    rotation: &SignedTopologyTransaction,
    own: &Fingerprint,
) -> Result<Vec<SigningPublicKey>> {
    let proposed = topology_version::decode_transaction(&rotation.transaction)?;
    let (head_serial, head) =
        topology::fetch_p2p_mapping_at_head(config, synchronizer_id, dec_party_id).await?;
    let (_, namespace) = topology::fetch_namespace_definition_at_head(
        config,
        synchronizer_id,
        &dec_party_id.namespace.to_hex(),
    )
    .await?;
    check_rotation_against(
        dec_party_id,
        head_serial,
        &head,
        namespace.threshold,
        &proposed,
        own,
    )
}

fn check_rotation_against(
    dec_party_id: &CantonId,
    head_serial: u32,
    head: &PartyToParticipant,
    namespace_threshold: i32,
    proposed: &TopologyTransaction,
    own: &Fingerprint,
) -> Result<Vec<SigningPublicKey>> {
    anyhow::ensure!(
        proposed.operation == enums::TopologyChangeOp::AddReplace as i32,
        "The key rotation does not add or replace a mapping"
    );
    anyhow::ensure!(
        head_serial.checked_add(1) == Some(proposed.serial),
        "The key rotation is at serial {serial}, but this node holds serial {head_serial} for \
         {dec_party_id}",
        serial = proposed.serial
    );
    let Some(topology_mapping::Mapping::PartyToParticipant(mapping)) =
        proposed.mapping.as_ref().and_then(|m| m.mapping.as_ref())
    else {
        anyhow::bail!("The key rotation does not carry a PartyToParticipant mapping");
    };
    anyhow::ensure!(
        mapping.party == dec_party_id.to_string(),
        "The key rotation is for {party}, not {dec_party_id}",
        party = mapping.party
    );
    let unchanged = PartyToParticipant {
        party_signing_keys: head.party_signing_keys.clone(),
        ..mapping.clone()
    };
    anyhow::ensure!(
        unchanged == *head,
        "The key rotation changes more than the signing keys of {dec_party_id}"
    );

    let signing = mapping
        .party_signing_keys
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("The key rotation sets no signing keys"))?;
    let count = signing.keys.len();
    anyhow::ensure!(
        usize::try_from(signing.threshold)? == count,
        "The key rotation sets threshold {threshold} for {count} keys; a one-shot deploy needs \
         every key",
        threshold = signing.threshold
    );
    anyhow::ensure!(
        i64::try_from(count)? >= i64::from(namespace_threshold),
        "The key rotation installs {count} keys, fewer than the namespace threshold \
         {namespace_threshold}"
    );
    let mut fingerprints = BTreeSet::new();
    for key in &signing.keys {
        one_shot::protocol_verifying_key(key)?;
        anyhow::ensure!(
            fingerprints.insert(utils::compute_fingerprint(key)),
            "The key rotation repeats a key"
        );
    }
    anyhow::ensure!(
        fingerprints.contains(own),
        "The key rotation leaves out this node's one-shot key {own}"
    );
    Ok(signing.keys.clone())
}

/// A peer's part of a deploy: check the rotation, sign it with this node's
/// namespace key and `key`, and sign each prepared create with `key`. Returns
/// the reply for the coordinator. `key` is dropped before this returns.
///
/// # Errors
///
/// Errors when a prepared create or the rotation fails its checks, or when
/// Canton refuses to sign.
pub async fn sign_rotation(
    config: &NodeConfig,
    db: &SqlitePool,
    instance_name: &str,
    dec_party_id: &CantonId,
    intents: &[common::api::ContractDeploymentIntent],
    rotation_payload: &[u8],
    key: OneShotKey,
) -> Result<Vec<u8>> {
    let hashes = verified_hashes(db, instance_name, dec_party_id, intents).await?;
    let rotation: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(rotation_payload)?;
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    check_rotation(
        config,
        &synchronizer_id,
        dec_party_id,
        &rotation,
        key.fingerprint(),
    )
    .await?;

    let mut signed = countersign(config, &synchronizer_id, rotation).await?;
    signed
        .signatures
        .push(key.sign_topology(&signed.transaction));
    let daml = key.sign_prepared(&hashes);
    drop(key);

    Ok(utils::encode_length_prefixed(&[
        &utils::encode_length_prefixed_message(&signed),
        &encode_messages_length_prefixed(&daml),
    ]))
}

/// Add this node's namespace signature to the rotation.
async fn countersign(
    config: &NodeConfig,
    synchronizer_id: &str,
    rotation: SignedTopologyTransaction,
) -> Result<SignedTopologyTransaction> {
    let transaction = rotation.transaction.clone();
    let response = topology::sign_transactions_with_topology_retry(
        config,
        SignTransactionsRequest {
            transactions: vec![rotation],
            signed_by: vec![],
            store: Some(topology::synchronizer_store_id(synchronizer_id)),
            force_flags: vec![],
        },
        "contracts key rotation",
    )
    .await?;
    let [signed] = <[SignedTopologyTransaction; 1]>::try_from(response.transactions)
        .map_err(|_| anyhow::anyhow!("SignTransactions returned other than one transaction"))?;
    // The one-shot signatures cover these exact bytes.
    anyhow::ensure!(
        signed.transaction == transaction,
        "SignTransactions changed the bytes of the key rotation"
    );
    Ok(signed)
}

/// One signer's reply: its signed rotation and its signature on each create.
pub struct SignerReply {
    pub rotation: SignedTopologyTransaction,
    pub daml: Vec<Signature>,
}

impl SignerReply {
    /// # Errors
    ///
    /// Errors when the reply does not decode.
    pub fn decode(payload: &[u8]) -> Result<Self> {
        let items = utils::decode_length_prefixed(payload, 2)?;
        Ok(Self {
            rotation: utils::read_first_message_from_bytes(&items[0])?,
            daml: read_all_messages_from_bytes(&items[1])?,
        })
    }
}

/// Join the signers' copies of one rotation into one transaction.
///
/// # Errors
///
/// Errors when a copy is of a different transaction.
pub fn merge_rotations(
    mut rotation: SignedTopologyTransaction,
    copies: impl IntoIterator<Item = SignedTopologyTransaction>,
) -> Result<SignedTopologyTransaction> {
    for copy in copies {
        anyhow::ensure!(
            copy.transaction == rotation.transaction,
            "A signer returned a different key rotation"
        );
        rotation.signatures.extend(copy.signatures);
    }
    topology::dedupe_signatures(&mut rotation);
    Ok(rotation)
}

/// Check that every signer's one-shot key signed the rotation and every
/// prepared create, so nothing reaches Canton that it would refuse.
///
/// # Errors
///
/// Errors when a key's signature is missing or does not verify.
pub fn check_signatures(
    rotation: &SignedTopologyTransaction,
    keys: &[SigningPublicKey],
    hashes: &[PreparedTransactionHash],
    daml: &[Vec<Signature>],
) -> Result {
    let topology_hash = one_shot::topology_hash(&rotation.transaction);
    for key in keys {
        let fingerprint = utils::compute_fingerprint(key);
        let signature = rotation
            .signatures
            .iter()
            .find(|s| s.signed_by == fingerprint.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!("One-shot key {fingerprint} did not sign the rotation")
            })?;
        one_shot::verify(key, &topology_hash, signature)?;
    }
    for (index, (hash, signatures)) in hashes.iter().zip(daml).enumerate() {
        anyhow::ensure!(
            signatures.len() == keys.len(),
            "Create {index} has {found} signatures for {expected} keys",
            found = signatures.len(),
            expected = keys.len()
        );
        for key in keys {
            let fingerprint = utils::compute_fingerprint(key);
            let signature = signatures
                .iter()
                .find(|s| s.signed_by == fingerprint.as_str())
                .ok_or_else(|| {
                    anyhow::anyhow!("One-shot key {fingerprint} did not sign create {index}")
                })?;
            one_shot::verify(key, hash.as_bytes(), signature)?;
        }
    }
    anyhow::ensure!(
        hashes.len() == daml.len(),
        "Signatures cover {signed} creates of {total}",
        signed = daml.len(),
        total = hashes.len()
    );
    Ok(())
}

/// Publish the rotation and wait until this node sees the new keys, so a
/// submission signed with them is accepted here.
///
/// # Errors
///
/// Errors when the party's head moved, when Canton refuses the rotation, or
/// when the keys do not take effect in time.
pub async fn publish_rotation(
    config: &NodeConfig,
    synchronizer_id: &str,
    dec_party_id: &CantonId,
    rotation: SignedTopologyTransaction,
    keys: &[SigningPublicKey],
) -> Result {
    let serial = topology_version::decode_transaction(&rotation.transaction)?.serial;
    let expected: BTreeSet<Fingerprint> = keys.iter().map(utils::compute_fingerprint).collect();
    topology::submit_p2p(
        config,
        synchronizer_id,
        "contracts key rotation",
        rotation,
        topology::party_proposal_force_flags(),
        || wait_for_keys(config, synchronizer_id, dec_party_id, serial, &expected),
    )
    .await
}

async fn wait_for_keys(
    config: &NodeConfig,
    synchronizer_id: &str,
    dec_party_id: &CantonId,
    serial: u32,
    expected: &BTreeSet<Fingerprint>,
) -> Result {
    let attempts = topology_retry_max_attempts();
    let delay = Duration::from_secs(topology_retry_delay_secs());
    for attempt in 1..=attempts {
        let (head_serial, head) =
            topology::fetch_p2p_mapping_at_head(config, synchronizer_id, dec_party_id).await?;
        let installed: BTreeSet<Fingerprint> = head
            .party_signing_keys
            .map(|k| k.keys)
            .unwrap_or_default()
            .iter()
            .map(utils::compute_fingerprint)
            .collect();
        if head_serial >= serial && installed == *expected {
            return Ok(());
        }
        if attempt < attempts {
            tokio::time::sleep(delay).await;
        }
    }
    anyhow::bail!("The one-shot keys of {dec_party_id} did not take effect after {attempts} checks")
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::{
        TopologyMapping, party_to_participant::HostingParticipant,
    };

    use super::*;
    use crate::canton_id::{NAMESPACE_LENGTH, Namespace};

    struct Fixture {
        party: CantonId,
        head: PartyToParticipant,
        signers: Vec<OneShotKey>,
    }

    impl Fixture {
        fn new() -> Self {
            let party = CantonId::new("dec".to_string(), Namespace::new([1; NAMESPACE_LENGTH]));
            let head = PartyToParticipant {
                party: party.to_string(),
                threshold: 2,
                participants: vec![HostingParticipant {
                    participant_uid: "p1::1220".into(),
                    permission: 2,
                    onboarding: None,
                }],
                party_signing_keys: Some(SigningKeysWithThreshold {
                    keys: vec![OneShotKey::generate().public_key().clone()],
                    threshold: 1,
                }),
            };
            Self {
                party,
                head,
                signers: vec![OneShotKey::generate(), OneShotKey::generate()],
            }
        }

        fn keys(&self) -> Vec<SigningPublicKey> {
            self.signers
                .iter()
                .map(|k| k.public_key().clone())
                .collect()
        }

        fn rotation(&self, change: impl FnOnce(&mut PartyToParticipant)) -> TopologyTransaction {
            let keys = self.keys();
            let mut mapping = PartyToParticipant {
                party_signing_keys: Some(SigningKeysWithThreshold {
                    threshold: u32::try_from(keys.len()).unwrap_or(u32::MAX),
                    keys,
                }),
                ..self.head.clone()
            };
            change(&mut mapping);
            TopologyTransaction {
                operation: enums::TopologyChangeOp::AddReplace as i32,
                serial: 8,
                mapping: Some(TopologyMapping {
                    mapping: Some(topology_mapping::Mapping::PartyToParticipant(mapping)),
                }),
            }
        }

        fn check(&self, proposed: &TopologyTransaction) -> Result<Vec<SigningPublicKey>> {
            check_rotation_against(
                &self.party,
                7,
                &self.head,
                2,
                proposed,
                self.signers[0].fingerprint(),
            )
        }
    }

    #[test]
    fn a_rotation_to_every_signers_key_is_accepted() -> Result {
        let f = Fixture::new();
        assert_eq!(f.check(&f.rotation(|_| {}))?, f.keys());
        Ok(())
    }

    #[test]
    fn a_rotation_that_changes_anything_else_is_refused() {
        let f = Fixture::new();
        for change in [
            (|m: &mut PartyToParticipant| m.threshold = 1) as fn(&mut PartyToParticipant),
            |m| m.participants.clear(),
            |m| m.participants[0].permission = 1,
            |m| m.party = "other::1220".into(),
        ] {
            assert!(f.check(&f.rotation(change)).is_err());
        }
    }

    #[test]
    fn a_rotation_below_every_key_or_the_namespace_threshold_is_refused() {
        let f = Fixture::new();
        let lower = f.rotation(|m| {
            if let Some(k) = m.party_signing_keys.as_mut() {
                k.threshold = 1;
            }
        });
        assert!(f.check(&lower).is_err());

        let fewer = f.rotation(|m| {
            if let Some(k) = m.party_signing_keys.as_mut() {
                k.keys.truncate(1);
                k.threshold = 1;
            }
        });
        assert!(f.check(&fewer).is_err());
    }

    #[test]
    fn a_rotation_without_this_nodes_key_is_refused() {
        let f = Fixture::new();
        let replaced = f.rotation(|m| {
            if let Some(k) = m.party_signing_keys.as_mut() {
                k.keys[0] = OneShotKey::generate().public_key().clone();
            }
        });
        assert!(f.check(&replaced).is_err());
    }

    #[test]
    fn a_rotation_with_a_repeated_or_namespace_key_is_refused() {
        let f = Fixture::new();
        let repeated = f.rotation(|m| {
            if let Some(k) = m.party_signing_keys.as_mut() {
                k.keys[1] = k.keys[0].clone();
            }
        });
        assert!(f.check(&repeated).is_err());

        let namespace = f.rotation(|m| {
            if let Some(k) = m.party_signing_keys.as_mut() {
                k.keys[1].usage.push(1);
            }
        });
        assert!(f.check(&namespace).is_err());
    }

    #[test]
    fn a_rotation_at_a_stale_serial_is_refused() {
        let f = Fixture::new();
        let mut stale = f.rotation(|_| {});
        stale.serial = 7;
        assert!(f.check(&stale).is_err());
    }

    fn signed(transaction: &[u8], keys: &[OneShotKey]) -> SignedTopologyTransaction {
        SignedTopologyTransaction {
            transaction: transaction.to_vec(),
            signatures: keys.iter().map(|k| k.sign_topology(transaction)).collect(),
            proposal: true,
            multi_transaction_signatures: vec![],
        }
    }

    #[test]
    fn signatures_from_every_key_pass_and_a_missing_one_fails() -> Result {
        let f = Fixture::new();
        let hashes = vec![PreparedTransactionHash::new(vec![3; 32])];
        let daml: Vec<Vec<Signature>> = vec![
            f.signers
                .iter()
                .flat_map(|k| k.sign_prepared(&hashes))
                .collect(),
        ];
        let merged = merge_rotations(
            signed(b"rotation", &f.signers[..1]),
            [signed(b"rotation", &f.signers[1..])],
        )?;
        check_signatures(&merged, &f.keys(), &hashes, &daml)?;

        let partial = signed(b"rotation", &f.signers[..1]);
        assert!(check_signatures(&partial, &f.keys(), &hashes, &daml).is_err());

        let short = vec![daml[0][..1].to_vec()];
        assert!(check_signatures(&merged, &f.keys(), &hashes, &short).is_err());
        Ok(())
    }

    #[test]
    fn copies_of_another_rotation_are_not_merged() {
        let f = Fixture::new();
        assert!(
            merge_rotations(
                signed(b"rotation", &f.signers[..1]),
                [signed(b"other", &f.signers[1..])],
            )
            .is_err()
        );
    }
}
