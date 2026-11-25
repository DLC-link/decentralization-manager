//! The topology transactions that create a party, and the JSON Ledger API
//! requests that submit them.
//!
//! Every owner signs one hash: the multi-transaction hash of all the
//! transactions, which Canton counts as a signature on each of them. An
//! owner's key is both its root namespace key and one of the party's signing
//! keys, so that one signature authorizes:
//!
//! - the owner's root certificate, a `NamespaceDelegation` to its own key;
//! - the `DecentralizedNamespaceDefinition`, which every owner must sign when
//!   it is created;
//! - the `PartyToParticipant`, which adds the owner's key as a signing key and
//!   so needs that key's signature.
//!
//! Each host must also approve the `PartyToParticipant`. A participant does
//! that when it receives the transaction through `AllocateExternalParty`.

use std::collections::BTreeSet;

use base64::{Engine, engine::general_purpose::STANDARD};
use decman_lib::{
    canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::SigningKeysWithThreshold,
        protocol::v30::{
            DecentralizedNamespaceDefinition, NamespaceDelegation, PartyToParticipant,
            TopologyMapping, TopologyTransaction,
            enums::{ParticipantPermission, TopologyChangeOp},
            namespace_delegation::{CanSignAllMappings, Restriction},
            party_to_participant::HostingParticipant,
            topology_mapping::Mapping,
        },
    },
    topology::{self, TopologyFormat},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Error, PartyPlan,
    signature::{parse_public_key, verify_signature},
};

/// An onboarding transaction creates its mapping, so its serial is 1.
const FIRST_SERIAL: u32 = 1;

/// The party name in the format probe. The participant only builds that
/// topology and allocates nothing.
const PROBE_PARTY_HINT: &str = "decman-dapp-format-probe";

const PARTY_TO_PARTICIPANT: &str = "PartyToParticipant";

/// The transactions that create a party, and the hash every owner signs.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Onboarding {
    pub transactions: Vec<OnboardingTransaction>,
    /// The multi-transaction hash of all transactions, hex-encoded.
    pub combined_hash: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingTransaction {
    /// The kind of mapping the transaction creates.
    pub mapping: &'static str,
    /// Canton's hash of the transaction, hex-encoded.
    pub hash: String,
    /// The transaction in its versioned envelope, base64-encoded as the JSON
    /// Ledger API expects bytes.
    pub transaction: String,
}

/// One owner's Ed25519 signature on the combined hash.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerSignature {
    /// The signer's raw public key, hex-encoded.
    pub public_key: String,
    /// The 64-byte signature, hex-encoded.
    pub signature: String,
}

/// The two kinds of `POST /v2/parties/external/allocate` body.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AllocationRequests {
    /// Every host except the submitting one sends this first. Its one
    /// transaction is the unsigned `PartyToParticipant`, which the host signs
    /// as its approval to host the party.
    pub hosting_approval: Value,
    /// The submitting owner's participant sends this last: every transaction,
    /// with each owner's signature on the combined hash.
    pub allocate: Value,
}

/// The topology format with proto version `version`.
///
/// # Errors
///
/// Errors for any version other than 30 and 31.
pub fn topology_format(version: u32) -> Result<TopologyFormat, Error> {
    i32::try_from(version)
        .ok()
        .and_then(|version| TopologyFormat::from_proto_version(version).ok())
        .ok_or_else(|| Error::UnsupportedTopologyFormat(version.to_string()))
}

/// The transactions that create `plan`'s party, serialized in `format`.
///
/// One root certificate per owner, the decentralized namespace, and the
/// `PartyToParticipant`. The namespace and the party's signing keys share the
/// signing threshold. Every host confirms, and none may submit: the owners
/// sign every Daml transaction themselves.
pub fn onboarding_transactions(plan: &PartyPlan, format: TopologyFormat) -> Onboarding {
    let signing_keys: Vec<_> = plan
        .owners
        .iter()
        .map(|owner| topology::party_signing_key(owner.key.as_bytes()))
        .collect();

    let mut mappings: Vec<(&'static str, Mapping)> = plan
        .owners
        .iter()
        .zip(&signing_keys)
        .map(|(owner, key)| {
            let root_certificate = NamespaceDelegation {
                namespace: owner.namespace.clone(),
                target_key: Some(key.clone()),
                // A root certificate must be able to sign namespace delegations.
                restriction: Some(Restriction::CanSignAllMappings(CanSignAllMappings {})),
                ..Default::default()
            };
            (
                "NamespaceDelegation",
                Mapping::NamespaceDelegation(root_certificate),
            )
        })
        .collect();
    mappings.push((
        "DecentralizedNamespaceDefinition",
        Mapping::DecentralizedNamespaceDefinition(DecentralizedNamespaceDefinition {
            decentralized_namespace: plan.namespace.clone(),
            // plan_party caps the threshold at the owner count.
            threshold: i32::try_from(plan.signing_threshold).unwrap_or(i32::MAX),
            owners: plan
                .owners
                .iter()
                .map(|owner| owner.namespace.clone())
                .collect(),
        }),
    ));
    mappings.push((
        PARTY_TO_PARTICIPANT,
        Mapping::PartyToParticipant(PartyToParticipant {
            party: plan.party_id.clone(),
            threshold: plan.confirmation_threshold,
            participants: plan
                .hosts
                .iter()
                .map(|host| HostingParticipant {
                    participant_uid: host.clone(),
                    permission: ParticipantPermission::Confirmation as i32,
                    onboarding: None,
                })
                .collect(),
            party_signing_keys: Some(SigningKeysWithThreshold {
                keys: signing_keys,
                threshold: plan.signing_threshold,
            }),
        }),
    ));

    let serialized: Vec<(&'static str, Vec<u8>)> = mappings
        .into_iter()
        .map(|(kind, mapping)| {
            let transaction = TopologyTransaction {
                operation: TopologyChangeOp::AddReplace as i32,
                serial: FIRST_SERIAL,
                mapping: Some(TopologyMapping {
                    mapping: Some(mapping),
                }),
            };
            (kind, topology::encode_transaction(format, transaction))
        })
        .collect();
    let hashes: Vec<Vec<u8>> = serialized
        .iter()
        .map(|(_, bytes)| topology::topology_transaction_hash(bytes))
        .collect();
    let combined_hash = hex::encode(topology::multi_transaction_hash(&hashes));
    let transactions = serialized
        .into_iter()
        .zip(hashes)
        .map(|((mapping, bytes), hash)| OnboardingTransaction {
            mapping,
            hash: hex::encode(hash),
            transaction: STANDARD.encode(bytes),
        })
        .collect();
    Onboarding {
        transactions,
        combined_hash,
    }
}

/// Checks that every owner signed `onboarding`'s combined hash, once, and
/// that nobody else signed. Returns the signatures in owner order.
///
/// # Errors
///
/// Errors on a malformed key or signature, a signer that is not an owner, a
/// second signature from one owner, a signature over other bytes, or a
/// missing owner.
pub fn check_owner_signatures<'a>(
    plan: &PartyPlan,
    onboarding: &Onboarding,
    signatures: &'a [OwnerSignature],
) -> Result<Vec<&'a OwnerSignature>, Error> {
    let mut by_owner: Vec<Option<&OwnerSignature>> = vec![None; plan.owners.len()];
    for signature in signatures {
        let key = parse_public_key(signature.public_key.trim())?;
        let Some(index) = plan.owners.iter().position(|owner| owner.key == key) else {
            return Err(Error::SignerNotOwner {
                public_key: hex::encode(key.as_bytes()),
            });
        };
        let namespace = &plan.owners[index].namespace;
        if by_owner[index].is_some() {
            return Err(Error::DuplicateSignature {
                namespace: namespace.clone(),
            });
        }
        if !verify_signature(
            &plan.owners[index].public_key,
            &onboarding.combined_hash,
            signature.signature.trim(),
        )? {
            return Err(Error::OwnerSignatureMismatch {
                namespace: namespace.clone(),
            });
        }
        by_owner[index] = Some(signature);
    }
    plan.owners
        .iter()
        .zip(by_owner)
        .map(|(owner, signature)| {
            signature.ok_or_else(|| Error::MissingOwnerSignature {
                namespace: owner.namespace.clone(),
            })
        })
        .collect()
}

/// The `AllocateExternalParty` bodies that create `plan`'s party on
/// `synchronizer_id`, once every owner has signed.
///
/// # Errors
///
/// Errors when the synchronizer id is empty or [`check_owner_signatures`]
/// refuses the signatures.
pub fn allocation_requests(
    plan: &PartyPlan,
    onboarding: &Onboarding,
    synchronizer_id: &str,
    signatures: &[OwnerSignature],
) -> Result<AllocationRequests, Error> {
    let synchronizer_id = synchronizer_id.trim();
    if synchronizer_id.is_empty() {
        return Err(Error::MissingSynchronizer);
    }
    let signatures = check_owner_signatures(plan, onboarding, signatures)?;
    let multi_hash_signatures = plan
        .owners
        .iter()
        .zip(signatures)
        .map(|(owner, signature)| {
            let bytes =
                hex::decode(signature.signature.trim()).map_err(|_| Error::InvalidSignature)?;
            Ok(json!({
                "format": "SIGNATURE_FORMAT_CONCAT",
                "signature": STANDARD.encode(bytes),
                "signedBy": owner.namespace,
                "signingAlgorithmSpec": "SIGNING_ALGORITHM_SPEC_ED25519",
            }))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let unsigned = |transaction: &OnboardingTransaction| json!({ "transaction": transaction.transaction, "signatures": [] });
    let hosting = onboarding
        .transactions
        .iter()
        .filter(|transaction| transaction.mapping == PARTY_TO_PARTICIPANT)
        .map(unsigned)
        .collect::<Vec<_>>();
    Ok(AllocationRequests {
        hosting_approval: json!({
            "synchronizer": synchronizer_id,
            "onboardingTransactions": hosting,
            "multiHashSignatures": [],
            "identityProviderId": "",
        }),
        allocate: json!({
            "synchronizer": synchronizer_id,
            "onboardingTransactions": onboarding.transactions.iter().map(unsigned).collect::<Vec<_>>(),
            "multiHashSignatures": multi_hash_signatures,
            "identityProviderId": "",
        }),
    })
}

/// The `POST /v2/parties/external/generate-topology` body that probes a
/// participant's topology format. The participant builds a one-key party's
/// topology for `public_key_hex` in the synchronizer's format and allocates
/// nothing.
///
/// # Errors
///
/// Errors when the public key is not a valid Ed25519 key.
pub fn generate_topology_request(
    synchronizer_id: &str,
    public_key_hex: &str,
) -> Result<Value, Error> {
    let key = parse_public_key(public_key_hex.trim())?;
    let signing_key = topology::party_signing_key(key.as_bytes());
    Ok(json!({
        "synchronizer": synchronizer_id.trim(),
        "partyHint": PROBE_PARTY_HINT,
        "publicKey": {
            "format": "CRYPTO_KEY_FORMAT_DER_X509_SUBJECT_PUBLIC_KEY_INFO",
            "keyData": STANDARD.encode(signing_key.public_key),
            "keySpec": "SIGNING_KEY_SPEC_EC_CURVE25519",
        },
        "localParticipantObservationOnly": false,
        "otherConfirmingParticipantUids": [],
        "confirmationThreshold": 0,
        "observingParticipantUids": [],
    }))
}

/// The topology format of the transactions in a generate-topology answer.
///
/// The participant serializes them at the synchronizer's protocol version,
/// so their envelope names the format that `AllocateExternalParty` accepts.
///
/// # Errors
///
/// Errors when the answer holds no transactions, a transaction does not
/// decode, the transactions disagree, or the format is unknown.
pub fn topology_format_of_generated(response: &Value) -> Result<TopologyFormat, Error> {
    let transactions = response
        .get("topologyTransactions")
        .and_then(Value::as_array)
        .filter(|transactions| !transactions.is_empty())
        .ok_or(Error::NoTopologyToProbe)?;
    let versions = transactions
        .iter()
        .map(|transaction| {
            let encoded = transaction.as_str().ok_or_else(|| {
                Error::InvalidParticipantAnswer("a topology transaction is not a string".into())
            })?;
            let bytes = STANDARD
                .decode(encoded)
                .map_err(|e| Error::InvalidParticipantAnswer(e.to_string()))?;
            topology::envelope_version(&bytes)
                .map_err(|e| Error::InvalidParticipantAnswer(e.to_string()))
        })
        .collect::<Result<BTreeSet<i32>, Error>>()?;
    let mut versions = versions.into_iter();
    let (Some(version), None) = (versions.next(), versions.next()) else {
        return Err(Error::MixedTopologyFormats);
    };
    TopologyFormat::from_proto_version(version)
        .map_err(|_| Error::UnsupportedTopologyFormat(version.to_string()))
}

/// Whether a `GET /v2/parties/{party}` answer says the participant hosts
/// `party_id`.
pub fn party_hosted_locally(response: &Value, party_id: &str) -> bool {
    response
        .get("partyDetails")
        .and_then(Value::as_array)
        .is_some_and(|details| {
            details.iter().any(|detail| {
                detail.get("party").and_then(Value::as_str) == Some(party_id)
                    && detail.get("isLocal").and_then(Value::as_bool) == Some(true)
            })
        })
}

#[cfg(test)]
mod tests {
    use decman_lib::canton_proto_rs::com::digitalasset::canton::crypto::v30::SigningPublicKey;
    use ed25519_dalek::{Signer, SigningKey};

    use super::*;
    use crate::{PartyProposal, ProposedOwner, plan_party};

    const SYNCHRONIZER: &str = "global-domain::1220abcd";

    fn signing_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn proposal_with(seeds: &[u8]) -> PartyProposal {
        PartyProposal {
            party_hint: "treasury".to_owned(),
            owners: seeds
                .iter()
                .map(|seed| ProposedOwner {
                    public_key: hex::encode(signing_key(*seed).verifying_key().as_bytes()),
                    participant: format!("participant-{seed}::1220{seed:02x}"),
                })
                .collect(),
            signing_threshold: 2,
            confirmation_threshold: 2,
            covalidators: vec!["covalidator::1220cc".to_owned()],
        }
    }

    fn plan_with(seeds: &[u8]) -> PartyPlan {
        plan_party(&proposal_with(seeds)).expect("the test proposal is valid")
    }

    fn sign(seed: u8, onboarding: &Onboarding) -> OwnerSignature {
        let key = signing_key(seed);
        let message = hex::decode(&onboarding.combined_hash).expect("the combined hash is hex");
        OwnerSignature {
            public_key: hex::encode(key.verifying_key().as_bytes()),
            signature: hex::encode(key.sign(&message).to_bytes()),
        }
    }

    fn decoded(transaction: &OnboardingTransaction) -> Result<Mapping, Error> {
        let bytes = STANDARD
            .decode(&transaction.transaction)
            .map_err(|e| Error::InvalidParticipantAnswer(e.to_string()))?;
        let decoded = topology::decode_transaction(&bytes)
            .map_err(|e| Error::InvalidParticipantAnswer(e.to_string()))?;
        assert_eq!(decoded.operation, TopologyChangeOp::AddReplace as i32);
        assert_eq!(decoded.serial, 1);
        decoded
            .mapping
            .and_then(|mapping| mapping.mapping)
            .ok_or_else(|| Error::InvalidParticipantAnswer("no mapping".into()))
    }

    fn party_to_participant(onboarding: &Onboarding) -> Result<PartyToParticipant, Error> {
        let last = onboarding
            .transactions
            .last()
            .ok_or_else(|| Error::InvalidParticipantAnswer("no transactions".into()))?;
        match decoded(last)? {
            Mapping::PartyToParticipant(p2p) => Ok(p2p),
            other => Err(Error::InvalidParticipantAnswer(format!("{other:?}"))),
        }
    }

    fn owner_signing_key(seed: u8) -> SigningPublicKey {
        topology::party_signing_key(signing_key(seed).verifying_key().as_bytes())
    }

    #[test]
    fn builds_a_root_certificate_per_owner_then_the_namespace_then_the_hosting() -> Result<(), Error>
    {
        let plan = plan_with(&[1, 2]);
        for format in [TopologyFormat::V30, TopologyFormat::V31] {
            let onboarding = onboarding_transactions(&plan, format);
            let kinds: Vec<_> = onboarding.transactions.iter().map(|t| t.mapping).collect();
            assert_eq!(
                kinds,
                [
                    "NamespaceDelegation",
                    "NamespaceDelegation",
                    "DecentralizedNamespaceDefinition",
                    "PartyToParticipant"
                ]
            );

            for (owner, transaction) in plan.owners.iter().zip(&onboarding.transactions) {
                let Mapping::NamespaceDelegation(delegation) = decoded(transaction)? else {
                    panic!("expected a namespace delegation");
                };
                assert_eq!(delegation.namespace, owner.namespace);
                assert_eq!(
                    delegation.target_key,
                    Some(topology::party_signing_key(owner.key.as_bytes()))
                );
                assert_eq!(
                    delegation.restriction,
                    Some(Restriction::CanSignAllMappings(CanSignAllMappings {}))
                );
            }

            let Mapping::DecentralizedNamespaceDefinition(namespace) =
                decoded(&onboarding.transactions[2])?
            else {
                panic!("expected a decentralized namespace");
            };
            assert_eq!(namespace.decentralized_namespace, plan.namespace);
            assert_eq!(namespace.threshold, 2);
            let owners: Vec<_> = plan.owners.iter().map(|o| o.namespace.clone()).collect();
            assert_eq!(namespace.owners, owners);

            let p2p = party_to_participant(&onboarding)?;
            assert_eq!(p2p.party, plan.party_id);
            assert_eq!(p2p.threshold, 2);
            let hosts: Vec<_> = p2p
                .participants
                .iter()
                .map(|p| p.participant_uid.clone())
                .collect();
            assert_eq!(hosts, plan.hosts);
            assert!(p2p.participants.iter().all(|p| p.onboarding.is_none()));
            let keys = p2p.party_signing_keys.expect("the party has signing keys");
            assert_eq!(keys.threshold, 2);
            let mut expected_keys = vec![owner_signing_key(1), owner_signing_key(2)];
            let mut actual_keys = keys.keys;
            expected_keys.sort_by(|a, b| a.public_key.cmp(&b.public_key));
            actual_keys.sort_by(|a, b| a.public_key.cmp(&b.public_key));
            assert_eq!(actual_keys, expected_keys);
        }
        Ok(())
    }

    #[test]
    fn each_hash_covers_the_serialized_envelope_and_the_combined_hash_covers_all()
    -> Result<(), Error> {
        let plan = plan_with(&[1, 2]);
        for format in [TopologyFormat::V30, TopologyFormat::V31] {
            let onboarding = onboarding_transactions(&plan, format);
            let mut hashes = Vec::new();
            for transaction in &onboarding.transactions {
                let bytes = STANDARD
                    .decode(&transaction.transaction)
                    .map_err(|e| Error::InvalidParticipantAnswer(e.to_string()))?;
                assert_eq!(
                    topology::envelope_version(&bytes)
                        .map_err(|e| Error::InvalidParticipantAnswer(e.to_string()))?,
                    format.proto_version()
                );
                let hash = topology::topology_transaction_hash(&bytes);
                assert_eq!(transaction.hash, hex::encode(&hash));
                hashes.push(hash);
            }
            assert_eq!(
                onboarding.combined_hash,
                hex::encode(topology::multi_transaction_hash(&hashes))
            );
        }
        Ok(())
    }

    /// Canton 3.7's own code, from the Splice 0.10 bundle, parsed these
    /// transactions at protocol versions 36 and 37 and computed these hashes
    /// and combined hashes. The vectors pin our bytes and both hash rules.
    #[test]
    fn hashes_match_what_canton_computed_for_the_same_transactions() -> Result<(), Error> {
        let uid = |name: &str, byte: &str| format!("{name}::1220{}", byte.repeat(32));
        let mut proposal = proposal_with(&[1, 2]);
        proposal.owners[0].participant = uid("participant-1", "a1");
        proposal.owners[1].participant = uid("participant-2", "b2");
        proposal.covalidators = vec![uid("participant-3", "c3")];
        let plan = plan_party(&proposal)?;
        assert_eq!(
            plan.party_id,
            "treasury::1220c73aa2db7c2ad4a12867f55cdfbf67fc1832983b16677583c014fb39a9bfaa33"
        );

        let cases = [
            (
                TopologyFormat::V30,
                [
                    "1220dc939f79c45436990f97c0bde262023812a5d40b386d0e1decd8588d522e64a6",
                    "122016960b46f54aa713092897f77fa4bc12bed7ee44b62ed4371139d55fdcf20553",
                    "1220e63ecfcf1bb83ecdc8909d7b0f69b1aa4aef11eacbd230957271e6993beef2c2",
                    "1220acc485e5b7a8b3d37dbd8ab9d300157fa55ef991798aec17a7903f5bafc59bb9",
                ],
                "12204dfafc2777feec7fb1792cb682c030d2c42e4b5fd2619ab9b19c0c7be9575c06",
            ),
            (
                TopologyFormat::V31,
                [
                    "12202ec5c7c54ca90a9d2e30cf38d5cb4ef92712ade0ec0fa923d5bf2c86fd284dc1",
                    "1220bae82fab497564c5819726e69b53b7348cf51d00bce6d00671148c0b48fb7544",
                    "12201e097bc227e827d545e82e0790382a12e5f40ecb86a6c03d9540b3addbc31a94",
                    "1220272683001121416b544880a7476fc3a90eaafd14bded9af5b2c6274f21e98007",
                ],
                "122099f6dac8533356527b00aab69e344f9ebe001596a05db01a572e401f730def6d",
            ),
        ];
        for (format, hashes, combined) in cases {
            let onboarding = onboarding_transactions(&plan, format);
            let actual: Vec<&str> = onboarding
                .transactions
                .iter()
                .map(|t| t.hash.as_str())
                .collect();
            assert_eq!(actual, hashes, "{format:?}");
            assert_eq!(onboarding.combined_hash, combined, "{format:?}");
        }
        Ok(())
    }

    #[test]
    fn no_host_ever_gets_submission_permission() -> Result<(), Error> {
        let mut proposal = proposal_with(&[1, 2, 3]);
        proposal.covalidators = vec!["c1::1220c1".into(), "c2::1220c2".into()];
        let plan = plan_party(&proposal)?;
        for format in [TopologyFormat::V30, TopologyFormat::V31] {
            let p2p = party_to_participant(&onboarding_transactions(&plan, format))?;
            assert_eq!(p2p.participants.len(), 5);
            assert!(
                p2p.participants
                    .iter()
                    .all(|p| p.permission == ParticipantPermission::Confirmation as i32)
            );
        }
        Ok(())
    }

    /// The other tests use one value for both thresholds, so a swap would
    /// pass them. Here each threshold has its own value.
    #[test]
    fn each_threshold_lands_in_its_own_field() -> Result<(), Error> {
        let mut proposal = proposal_with(&[1, 2, 3]);
        proposal.signing_threshold = 2;
        proposal.confirmation_threshold = 3;
        let plan = plan_party(&proposal)?;
        for format in [TopologyFormat::V30, TopologyFormat::V31] {
            let onboarding = onboarding_transactions(&plan, format);
            let Mapping::DecentralizedNamespaceDefinition(namespace) =
                decoded(&onboarding.transactions[3])?
            else {
                panic!("expected a decentralized namespace");
            };
            assert_eq!(namespace.threshold, 2, "{format:?}");

            let p2p = party_to_participant(&onboarding)?;
            assert_eq!(p2p.threshold, 3, "{format:?}");
            let keys = p2p.party_signing_keys.expect("the party has signing keys");
            assert_eq!(keys.threshold, 2, "{format:?}");
            assert_eq!(keys.keys.len(), 3, "{format:?}");
        }
        Ok(())
    }

    #[test]
    fn changing_the_hosting_changes_only_its_hash() {
        let plan = plan_with(&[1, 2]);
        let mut proposal = proposal_with(&[1, 2]);
        proposal.confirmation_threshold = 3;
        let changed = plan_party(&proposal).expect("valid");

        let before = onboarding_transactions(&plan, TopologyFormat::V31);
        let after = onboarding_transactions(&changed, TopologyFormat::V31);

        for index in 0..3 {
            assert_eq!(
                before.transactions[index].hash,
                after.transactions[index].hash
            );
        }
        assert_ne!(before.transactions[3].hash, after.transactions[3].hash);
        assert_ne!(before.combined_hash, after.combined_hash);
    }

    #[test]
    fn the_format_changes_every_hash() {
        let plan = plan_with(&[1, 2]);
        let v30 = onboarding_transactions(&plan, TopologyFormat::V30);
        let v31 = onboarding_transactions(&plan, TopologyFormat::V31);
        for (a, b) in v30.transactions.iter().zip(&v31.transactions) {
            assert_ne!(a.hash, b.hash);
        }
    }

    #[test]
    fn ten_owners_get_ten_root_certificates() {
        let seeds: Vec<u8> = (1..=10).collect();
        let onboarding = onboarding_transactions(&plan_with(&seeds), TopologyFormat::V31);
        let roots = onboarding
            .transactions
            .iter()
            .filter(|t| t.mapping == "NamespaceDelegation")
            .count();
        assert_eq!(roots, 10);
        assert_eq!(onboarding.transactions.len(), 12);
    }

    #[test]
    fn signatures_from_every_owner_produce_both_requests() -> Result<(), Error> {
        let plan = plan_with(&[1, 2]);
        let onboarding = onboarding_transactions(&plan, TopologyFormat::V31);
        // Order of the signatures does not matter.
        let signatures = [sign(2, &onboarding), sign(1, &onboarding)];

        let requests = allocation_requests(&plan, &onboarding, SYNCHRONIZER, &signatures)?;

        let approval = &requests.hosting_approval;
        assert_eq!(approval["synchronizer"], SYNCHRONIZER);
        assert_eq!(approval["multiHashSignatures"], json!([]));
        assert_eq!(
            approval["onboardingTransactions"],
            json!([{ "transaction": onboarding.transactions[3].transaction, "signatures": [] }])
        );

        let allocate = &requests.allocate;
        assert_eq!(allocate["synchronizer"], SYNCHRONIZER);
        let sent: Vec<&str> = allocate["onboardingTransactions"]
            .as_array()
            .expect("an array")
            .iter()
            .map(|t| t["transaction"].as_str().expect("base64"))
            .collect();
        let built: Vec<&str> = onboarding
            .transactions
            .iter()
            .map(|t| t.transaction.as_str())
            .collect();
        assert_eq!(sent, built);

        let multi = allocate["multiHashSignatures"]
            .as_array()
            .expect("an array");
        assert_eq!(multi.len(), 2);
        let message = hex::decode(&onboarding.combined_hash).expect("hex");
        for (owner, signature) in plan.owners.iter().zip(multi) {
            assert_eq!(signature["signedBy"], owner.namespace.as_str());
            assert_eq!(signature["format"], "SIGNATURE_FORMAT_CONCAT");
            assert_eq!(
                signature["signingAlgorithmSpec"],
                "SIGNING_ALGORITHM_SPEC_ED25519"
            );
            let bytes: [u8; 64] = STANDARD
                .decode(signature["signature"].as_str().expect("base64"))
                .expect("base64")
                .try_into()
                .expect("64 bytes");
            owner
                .key
                .verify_strict(&message, &ed25519_dalek::Signature::from_bytes(&bytes))
                .expect("the signature covers the combined hash");
        }
        Ok(())
    }

    #[test]
    fn refuses_a_signature_from_a_key_that_is_not_an_owner() {
        let plan = plan_with(&[1, 2]);
        let onboarding = onboarding_transactions(&plan, TopologyFormat::V31);
        let signatures = [
            sign(1, &onboarding),
            sign(2, &onboarding),
            sign(3, &onboarding),
        ];
        assert!(matches!(
            allocation_requests(&plan, &onboarding, SYNCHRONIZER, &signatures),
            Err(Error::SignerNotOwner { .. })
        ));
    }

    #[test]
    fn refuses_when_an_owner_has_not_signed() {
        let plan = plan_with(&[1, 2]);
        let onboarding = onboarding_transactions(&plan, TopologyFormat::V31);
        let missing = plan
            .owners
            .iter()
            .find(|owner| owner.key == signing_key(2).verifying_key())
            .expect("owner 2")
            .namespace
            .clone();
        match allocation_requests(&plan, &onboarding, SYNCHRONIZER, &[sign(1, &onboarding)]) {
            Err(Error::MissingOwnerSignature { namespace }) => assert_eq!(namespace, missing),
            other => panic!("expected a missing signature, got {other:?}"),
        }
    }

    #[test]
    fn refuses_a_signature_over_another_combined_hash() {
        let plan = plan_with(&[1, 2]);
        let onboarding = onboarding_transactions(&plan, TopologyFormat::V31);
        let mut proposal = proposal_with(&[1, 2]);
        proposal.confirmation_threshold = 1;
        let other =
            onboarding_transactions(&plan_party(&proposal).expect("valid"), TopologyFormat::V31);
        let signatures = [sign(1, &onboarding), sign(2, &other)];
        assert!(matches!(
            allocation_requests(&plan, &onboarding, SYNCHRONIZER, &signatures),
            Err(Error::OwnerSignatureMismatch { .. })
        ));
    }

    #[test]
    fn refuses_a_second_signature_from_one_owner() {
        let plan = plan_with(&[1, 2]);
        let onboarding = onboarding_transactions(&plan, TopologyFormat::V31);
        let signatures = [
            sign(1, &onboarding),
            sign(1, &onboarding),
            sign(2, &onboarding),
        ];
        assert!(matches!(
            allocation_requests(&plan, &onboarding, SYNCHRONIZER, &signatures),
            Err(Error::DuplicateSignature { .. })
        ));
    }

    #[test]
    fn refuses_an_empty_synchronizer() {
        let plan = plan_with(&[1, 2]);
        let onboarding = onboarding_transactions(&plan, TopologyFormat::V31);
        let signatures = [sign(1, &onboarding), sign(2, &onboarding)];
        assert!(matches!(
            allocation_requests(&plan, &onboarding, " ", &signatures),
            Err(Error::MissingSynchronizer)
        ));
    }

    #[test]
    fn only_formats_30_and_31_are_known() {
        assert_eq!(topology_format(30).ok(), Some(TopologyFormat::V30));
        assert_eq!(topology_format(31).ok(), Some(TopologyFormat::V31));
        for version in [0, 29, 32, u32::MAX] {
            assert!(matches!(
                topology_format(version),
                Err(Error::UnsupportedTopologyFormat(_))
            ));
        }
    }

    fn generated(transactions: &[Vec<u8>]) -> Value {
        json!({
            "partyId": "probe::1220aa",
            "topologyTransactions": transactions.iter().map(|t| STANDARD.encode(t)).collect::<Vec<_>>(),
        })
    }

    /// A transaction's envelope with its proto version replaced.
    fn envelope(version: i32) -> Vec<u8> {
        use decman_lib::canton_proto_rs::com::digitalasset::canton::version::v1::UntypedVersionedMessage;
        use prost::Message;

        let bytes =
            topology::encode_transaction(TopologyFormat::V30, TopologyTransaction::default());
        let mut message =
            UntypedVersionedMessage::decode(bytes.as_slice()).expect("the envelope decodes");
        message.version = version;
        message.encode_to_vec()
    }

    #[test]
    fn the_probe_reads_the_format_of_the_generated_topology() -> Result<(), Error> {
        for format in [TopologyFormat::V30, TopologyFormat::V31] {
            let transaction = topology::encode_transaction(format, TopologyTransaction::default());
            assert_eq!(
                topology_format_of_generated(&generated(&[transaction]))?,
                format
            );
        }
        Ok(())
    }

    #[test]
    fn the_probe_refuses_an_unknown_format() {
        assert!(matches!(
            topology_format_of_generated(&generated(&[envelope(32)])),
            Err(Error::UnsupportedTopologyFormat(version)) if version == "32"
        ));
    }

    #[test]
    fn the_probe_refuses_an_answer_without_transactions_or_with_two_formats() {
        assert!(matches!(
            topology_format_of_generated(&generated(&[])),
            Err(Error::NoTopologyToProbe)
        ));
        assert!(matches!(
            topology_format_of_generated(&json!({ "partyId": "x" })),
            Err(Error::NoTopologyToProbe)
        ));
        assert!(matches!(
            topology_format_of_generated(&generated(&[envelope(30), envelope(31)])),
            Err(Error::MixedTopologyFormats)
        ));
        assert!(matches!(
            topology_format_of_generated(&json!({ "topologyTransactions": ["not base64!"] })),
            Err(Error::InvalidParticipantAnswer(_))
        ));
    }

    #[test]
    fn the_probe_request_carries_the_key_as_canton_parses_it() -> Result<(), Error> {
        let key = signing_key(1).verifying_key();
        let request = generate_topology_request(SYNCHRONIZER, &hex::encode(key.as_bytes()))?;
        assert_eq!(request["synchronizer"], SYNCHRONIZER);
        assert_eq!(
            request["publicKey"]["format"],
            "CRYPTO_KEY_FORMAT_DER_X509_SUBJECT_PUBLIC_KEY_INFO"
        );
        assert_eq!(
            request["publicKey"]["keySpec"],
            "SIGNING_KEY_SPEC_EC_CURVE25519"
        );
        let key_data = STANDARD
            .decode(request["publicKey"]["keyData"].as_str().expect("base64"))
            .expect("base64");
        assert_eq!(
            key_data,
            topology::party_signing_key(key.as_bytes()).public_key
        );
        assert!(matches!(
            generate_topology_request(SYNCHRONIZER, "abcd"),
            Err(Error::InvalidPublicKey)
        ));
        Ok(())
    }

    #[test]
    fn a_party_is_hosted_only_when_the_participant_says_it_is_local() {
        let party = "treasury::1220aa";
        let answer = |is_local: bool| json!({ "partyDetails": [{ "party": party, "isLocal": is_local, "identityProviderId": "" }] });
        assert!(party_hosted_locally(&answer(true), party));
        assert!(!party_hosted_locally(&answer(false), party));
        assert!(!party_hosted_locally(&answer(true), "other::1220bb"));
        assert!(!party_hosted_locally(&json!({ "partyDetails": [] }), party));
    }
}
