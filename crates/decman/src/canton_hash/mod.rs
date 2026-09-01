//! Canton's interactive-submission transaction hash, recomputed locally.
//!
//! A `PrepareSubmissionResponse` carries both the transaction and the hash to
//! sign, and the Ledger API is explicit that the hash is "provided for
//! convenience" — a client MUST recompute it from the transaction when the
//! preparing participant is not trusted. In DecMan the preparing participant
//! is the *coordinator*, which is exactly the party a peer is defending
//! against: signing the supplied hash blind lets a compromised coordinator
//! show one transaction and collect signatures over another.
//!
//! This module implements the hashing scheme so a peer can bind its signature
//! to the transaction it can actually inspect. It is a port of Canton's
//! `com.digitalasset.canton.protocol.hash` package (v3.5.8), cross-checked
//! against the reference Python implementation Canton ships with the
//! interactive-submission example, and pinned by the golden vectors from
//! Canton's own `NodeHashTest` / `MetadataHashTest` suites in the tests below.
//!
//! Unsupported input fails closed: an unknown scheme version, or a field a
//! scheme does not cover, produces an error rather than a hash that omits it.

mod encoding;
mod metadata;
mod nodes;

use canton_proto_rs::com::daml::ledger::api::v2::interactive::{
    HashingSchemeVersion, PrepareSubmissionResponse, PreparedTransaction,
};

use crate::{
    canton_hash::encoding::{Encoder, required},
    error::Result,
};

/// The hashing scheme versions this implementation covers.
///
/// Protocol version 34 supports V2; 35 supports V2 and V3. V4 is
/// development-protocol only and adds exercise external-call results to the
/// hash, so it is deliberately absent: a peer that cannot reproduce a hash
/// must refuse to sign it, not approximate it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashingScheme {
    V2,
    V3,
}

impl std::fmt::Display for HashingScheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::V2 => f.write_str("V2"),
            Self::V3 => f.write_str("V3"),
        }
    }
}

impl HashingScheme {
    /// The scheme's byte in the final hash preimage.
    fn version_byte(self) -> u8 {
        match self {
            Self::V2 => 0x02,
            Self::V3 => 0x03,
        }
    }

    /// Map the version the preparing participant reported.
    ///
    /// # Errors
    ///
    /// Errors for any version this implementation does not cover.
    pub fn from_proto(version: i32) -> Result<Self> {
        match HashingSchemeVersion::try_from(version) {
            Ok(HashingSchemeVersion::V2) => Ok(Self::V2),
            Ok(HashingSchemeVersion::V3) => Ok(Self::V3),
            Ok(other) => anyhow::bail!(
                "hashing scheme {name} is not supported; the transaction hash cannot be \
                 verified, so it will not be signed",
                name = other.as_str_name()
            ),
            Err(_) => anyhow::bail!(
                "unknown hashing scheme version {version}; the transaction hash cannot be \
                 verified, so it will not be signed"
            ),
        }
    }
}

/// Recompute the hash a peer is asked to sign from the transaction itself.
///
/// # Errors
///
/// Errors if the transaction is malformed, or if it uses a feature the given
/// scheme does not hash.
pub fn compute_prepared_transaction_hash(
    scheme: HashingScheme,
    prepared: &PreparedTransaction,
) -> Result<Vec<u8>> {
    let transaction = required(prepared.transaction.as_ref(), "transaction")?;
    let metadata = required(prepared.metadata.as_ref(), "metadata")?;

    let transaction_hash = nodes::hash_transaction(scheme, transaction)?;
    let metadata_hash = metadata::hash_metadata(scheme, metadata)?;

    let mut encoder = Encoder::with_purpose();
    encoder.byte(scheme.version_byte());
    encoder.add_hash(&transaction_hash);
    encoder.add_hash(&metadata_hash);
    Ok(encoder.digest().to_vec())
}

/// Check that the hash a coordinator asks a peer to sign is really the hash of
/// the transaction it sent alongside it.
///
/// # Errors
///
/// Errors if the response is malformed, if the scheme is one this
/// implementation cannot reproduce, or if the recomputed hash differs from the
/// supplied one — the case that means the coordinator is showing one
/// transaction and asking for a signature over another.
pub fn verify_prepared_submission(response: &PrepareSubmissionResponse) -> Result {
    let scheme = HashingScheme::from_proto(response.hashing_scheme_version)?;
    let prepared = required(
        response.prepared_transaction.as_ref(),
        "prepared transaction",
    )?;
    let recomputed = compute_prepared_transaction_hash(scheme, prepared)?;

    if recomputed != response.prepared_transaction_hash {
        anyhow::bail!(
            "prepared transaction hash mismatch: the coordinator asked for a signature over \
             {supplied} but the transaction it sent hashes to {recomputed} under scheme \
             {scheme}",
            supplied = hex::encode(&response.prepared_transaction_hash),
            recomputed = hex::encode(&recomputed),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::daml::ledger::api::v2::{
        Identifier, Value,
        interactive::{
            DamlTransaction, GlobalKey, GlobalKeyWithMaintainers, Metadata,
            daml_transaction::{self, NodeSeed},
            metadata::{InputContract, SubmitterInfo, input_contract},
            transaction::v1::{self, Create, Exercise, Fetch, Rollback},
        },
        value,
    };

    use super::*;

    // Fixtures mirroring Canton's `BaseNodeHashTest` / `HashUtilsTest`, so the
    // expected hashes below are Canton's own, lifted from its test suite at
    // tag v3.5.8. If this port drifts from Canton's encoding, these fail.
    const CONTRACT_ID_1: &str =
        "0007e7b5534931dfca8e1b485c105bae4e10808bd13ddc8e897f258015f9d921c5";
    const CONTRACT_ID_2: &str =
        "0059b59ad7a6b6066e77b91ced54b8282f0e24e7089944685cb8f22f32fcbc4e1b";
    const DUMMY_ARG_CONTRACT_ID: &str =
        "0097a092402108f5593bac7fb3c909cd316910197dd98d603042a45ab85c81e0fd";
    const SEED_CREATE: &str = "926bbb6f341bc0092ae65d06c6e284024907148cc29543ef6bff0930f5d52c19";
    const SEED_FETCH: &str = "4d2a522e9ee44e31b9bef2e3c8a07d43475db87463c6a13c4ea92f898ac8a930";
    const SEED_EXERCISE: &str = "a867edafa1277f46f879ab92c373a15c2d75c5d86fec741705cee1eb01ef8c9e";
    const SEED_ROLLBACK: &str = "5483d5df9b245e662c0e4368b8062e8a0fd24c17ce4ded1a0e452e4ee879dd81";
    const GLOBAL_KEY_HASH: &str =
        "4d1741d27564ab1f1e74873034760583a1a71edaa90dbad6be3a78c0bf3eec7c";
    const PACKAGE_NAME_0: &str = "package-name-0";

    fn parties(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    fn text(value: &str) -> Value {
        Value {
            sum: Some(value::Sum::Text(value.to_string())),
        }
    }

    fn identifier(module: &str, name: &str) -> Identifier {
        Identifier {
            package_id: "package".to_string(),
            module_name: module.to_string(),
            entity_name: name.to_string(),
        }
    }

    fn seed(node_id: i32, hex_seed: &str) -> Result<NodeSeed> {
        Ok(NodeSeed {
            node_id,
            seed: hex::decode(hex_seed)?,
        })
    }

    fn global_key() -> Result<GlobalKeyWithMaintainers> {
        Ok(GlobalKeyWithMaintainers {
            key: Some(GlobalKey {
                template_id: Some(identifier("module_key", "name")),
                package_name: "package_name_key".to_string(),
                key: Some(text("hello")),
                hash: hex::decode(GLOBAL_KEY_HASH)?,
            }),
            maintainers: parties(&["david"]),
        })
    }

    fn create_node(lf_version: &str, contract_id: &str) -> Create {
        Create {
            lf_version: lf_version.to_string(),
            contract_id: contract_id.to_string(),
            package_name: PACKAGE_NAME_0.to_string(),
            template_id: Some(identifier("module", "name")),
            argument: Some(text("hello")),
            signatories: parties(&["alice", "bob"]),
            stakeholders: parties(&["alice", "charlie"]),
            key: None,
        }
    }

    fn fetch_node(lf_version: &str, contract_id: &str) -> Fetch {
        Fetch {
            lf_version: lf_version.to_string(),
            contract_id: contract_id.to_string(),
            package_name: PACKAGE_NAME_0.to_string(),
            template_id: Some(identifier("module", "name")),
            signatories: parties(&["alice"]),
            stakeholders: parties(&["charlie"]),
            acting_parties: parties(&["alice", "bob"]),
            interface_id: None,
            key: None,
            by_key: false,
        }
    }

    fn exercise_node(lf_version: &str) -> Exercise {
        Exercise {
            lf_version: lf_version.to_string(),
            contract_id: CONTRACT_ID_1.to_string(),
            package_name: PACKAGE_NAME_0.to_string(),
            template_id: Some(identifier("module", "name")),
            signatories: parties(&["alice"]),
            stakeholders: parties(&["charlie"]),
            acting_parties: parties(&["alice", "bob"]),
            interface_id: Some(identifier("interface_module", "interface_name")),
            choice_id: "choice".to_string(),
            chosen_value: Some(Value {
                sum: Some(value::Sum::Int64(31380)),
            }),
            consuming: true,
            children: vec!["0".to_string(), "2".to_string()],
            exercise_result: Some(text("result")),
            choice_observers: parties(&["david"]),
            key: None,
            by_key: false,
            external_call_results: Vec::new(),
        }
    }

    fn node(node_id: &str, node_type: v1::node::NodeType) -> daml_transaction::Node {
        daml_transaction::Node {
            node_id: node_id.to_string(),
            versioned_node: Some(daml_transaction::node::VersionedNode::V1(v1::Node {
                node_type: Some(node_type),
            })),
        }
    }

    /// Canton's `subNodesMap`: create(0), create2(1), fetch(2), fetch2(3),
    /// exercise(4), rollback(5), with the exercise rooted at create+fetch and
    /// the rollback at fetch2+exercise.
    fn transaction(scheme: HashingScheme) -> Result<DamlTransaction> {
        let lf_version = match scheme {
            HashingScheme::V2 => "2.1",
            HashingScheme::V3 => "2",
        };
        let (create, fetch, exercise) = match scheme {
            HashingScheme::V2 => (
                create_node(lf_version, CONTRACT_ID_1),
                fetch_node(lf_version, CONTRACT_ID_1),
                exercise_node(lf_version),
            ),
            // V3's suite adds a contract key to create/fetch and exercises
            // by key.
            HashingScheme::V3 => {
                let mut create = create_node(lf_version, CONTRACT_ID_1);
                create.key = Some(global_key()?);
                let mut fetch = fetch_node(lf_version, CONTRACT_ID_1);
                fetch.key = Some(global_key()?);
                fetch.by_key = true;
                let mut exercise = exercise_node(lf_version);
                exercise.key = Some(global_key()?);
                exercise.by_key = true;
                (create, fetch, exercise)
            }
        };

        Ok(DamlTransaction {
            version: lf_version.to_string(),
            roots: vec!["0".to_string(), "5".to_string()],
            nodes: vec![
                node("0", v1::node::NodeType::Create(create)),
                node(
                    "1",
                    v1::node::NodeType::Create(create_node(lf_version, CONTRACT_ID_2)),
                ),
                node("2", v1::node::NodeType::Fetch(fetch)),
                node(
                    "3",
                    v1::node::NodeType::Fetch(fetch_node(lf_version, CONTRACT_ID_2)),
                ),
                node("4", v1::node::NodeType::Exercise(exercise)),
                node(
                    "5",
                    v1::node::NodeType::Rollback(Rollback {
                        children: vec!["2".to_string(), "4".to_string()],
                    }),
                ),
            ],
            node_seeds: vec![
                seed(0, SEED_CREATE)?,
                seed(2, SEED_FETCH)?,
                seed(4, SEED_EXERCISE)?,
                seed(5, SEED_ROLLBACK)?,
            ],
        })
    }

    /// Canton's `HashUtilsTest.metadata`.
    fn metadata(scheme: HashingScheme) -> Metadata {
        let lf_version = match scheme {
            HashingScheme::V2 => "2.1",
            HashingScheme::V3 => "2",
        };
        let disclosed = |contract_id: &str, party: &str, created_at: u64| InputContract {
            created_at,
            event_blob: Vec::new(),
            contract: Some(input_contract::Contract::V1(Create {
                lf_version: lf_version.to_string(),
                contract_id: contract_id.to_string(),
                package_name: "PkgName".to_string(),
                template_id: Some(Identifier {
                    package_id: "-dummyPkg-".to_string(),
                    module_name: "DummyModule".to_string(),
                    entity_name: "dummyName".to_string(),
                }),
                argument: Some(Value {
                    sum: Some(value::Sum::ContractId(DUMMY_ARG_CONTRACT_ID.to_string())),
                }),
                signatories: parties(&[party]),
                stakeholders: parties(&[party]),
                key: None,
            })),
        };

        Metadata {
            submitter_info: Some(SubmitterInfo {
                act_as: parties(&["alice", "bob"]),
                command_id: "command-id".to_string(),
            }),
            synchronizer_id: "synchronizer::id".to_string(),
            mediator_group: 0,
            transaction_uuid: "4c6471d3-4e09-49dd-addf-6cd90e19c583".to_string(),
            preparation_time: 0,
            input_contracts: vec![
                disclosed(CONTRACT_ID_1, "alice", 864_000_000_000),
                disclosed(CONTRACT_ID_2, "bob", 1_728_000_000_000),
            ],
            min_ledger_effective_time: Some(0xaaaa),
            max_ledger_effective_time: Some(0xbbbb),
            // Only hashed by V3; Canton's V3 suite sets it to 30 days.
            max_record_time: Some(2_592_000_000_000),
            ..Metadata::default()
        }
    }

    fn prepared(scheme: HashingScheme) -> Result<PreparedTransaction> {
        Ok(PreparedTransaction {
            transaction: Some(transaction(scheme)?),
            metadata: Some(metadata(scheme)),
        })
    }

    // ---------------------------------------------------------------------
    // Golden vectors from Canton v3.5.8's own hashing test suites.
    // ---------------------------------------------------------------------

    /// Per-node hashes, so a drift in one node kind names itself instead of
    /// only showing up as a different transaction hash.
    #[test]
    fn matches_canton_v2_node_hashes() -> Result {
        let transaction = transaction(HashingScheme::V2)?;
        let hasher = nodes::NodeHasher::new(HashingScheme::V2, &transaction)?;
        assert_eq!(
            hex::encode(hasher.hash_node_id("0")?),
            "6d2cfe58c2294000592034f4bdfe397fe246901bb8b63e3b9e041bb478e174b7",
            "create node"
        );
        assert_eq!(
            hex::encode(hasher.hash_node_id("2")?),
            "c962c6098394f3d11cd6f0c795de9517d32a8e3e1979cec76cd2f66254efc610",
            "fetch node"
        );
        assert_eq!(
            hex::encode(hasher.hash_node_id("4")?),
            "070970eb4b2de72561dafb67017ca33850650a8103e5134e16044ba78991f48c",
            "exercise node"
        );
        assert_eq!(
            hex::encode(hasher.hash_node_id("5")?),
            "7264d5da2fd714427453bedc0d1cdb21f52ac7aec8d4bb5ac0598d25c5fcaed9",
            "rollback node"
        );
        Ok(())
    }

    #[test]
    fn matches_canton_v3_node_hashes() -> Result {
        let transaction = transaction(HashingScheme::V3)?;
        let hasher = nodes::NodeHasher::new(HashingScheme::V3, &transaction)?;
        assert_eq!(
            hex::encode(hasher.hash_node_id("0")?),
            "0120d370509f54c07d8209f5af2d23f7f972997deb5fc5e0ebe886354395a3bc",
            "create node"
        );
        assert_eq!(
            hex::encode(hasher.hash_node_id("2")?),
            "20437df1980bb7d4d94ad53181dc6005bae37b5990719070c48a3960e8dda3a4",
            "fetch node"
        );
        assert_eq!(
            hex::encode(hasher.hash_node_id("4")?),
            "516c4689acabf9d1f0087a02a7dafad884d9080c48112a57a6d20b9f72d4b697",
            "exercise node"
        );
        assert_eq!(
            hex::encode(hasher.hash_node_id("5")?),
            "ea61ab05f9ab9769ab5c91493a601ccad9e784bd4c0b3bc4b7777f0da68eab4d",
            "rollback node"
        );
        Ok(())
    }

    #[test]
    fn matches_canton_v2_transaction_hash() -> Result {
        let transaction = transaction(HashingScheme::V2)?;
        let hash = nodes::hash_transaction(HashingScheme::V2, &transaction)?;
        assert_eq!(
            hex::encode(hash),
            "154f334d24a8a5e4d0ce51ac87d93821b3256f885f21d3f779a1640abf481983"
        );
        Ok(())
    }

    #[test]
    fn matches_canton_v2_metadata_hash() -> Result {
        let hash = metadata::hash_metadata(HashingScheme::V2, &metadata(HashingScheme::V2))?;
        assert_eq!(
            hex::encode(hash),
            "6e89fcbcc9605179a47919b5e65a864e470e7a133f4f9f39b1e4545b223db769"
        );
        Ok(())
    }

    #[test]
    fn matches_canton_v2_full_hash() -> Result {
        let hash =
            compute_prepared_transaction_hash(HashingScheme::V2, &prepared(HashingScheme::V2)?)?;
        assert_eq!(
            hex::encode(hash),
            "8c311c848db25d36b36fbd59f9483714a11688c2214e3c7cae3e028763520250"
        );
        Ok(())
    }

    #[test]
    fn matches_canton_v3_transaction_hash() -> Result {
        let transaction = transaction(HashingScheme::V3)?;
        let hash = nodes::hash_transaction(HashingScheme::V3, &transaction)?;
        assert_eq!(
            hex::encode(hash),
            "36f0d1f4e9742a5cf54795fd1633f46b8235fafa93da006dafe10eb358d465c4"
        );
        Ok(())
    }

    #[test]
    fn matches_canton_v3_metadata_hash() -> Result {
        let hash = metadata::hash_metadata(HashingScheme::V3, &metadata(HashingScheme::V3))?;
        assert_eq!(
            hex::encode(hash),
            "5a4c6aa89bb80d0af05a3f0614ba07bc7469795fff66d7b9e87c876eb0137e0a"
        );
        Ok(())
    }

    #[test]
    fn matches_canton_v3_full_hash() -> Result {
        let hash =
            compute_prepared_transaction_hash(HashingScheme::V3, &prepared(HashingScheme::V3)?)?;
        assert_eq!(
            hex::encode(hash),
            "db37e4e251b83196260900fd43aacf934e1893b7ac58db4c1bfa8ced432b3b84"
        );
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Adversarial cases: the tampering a compromised coordinator would try.
    // ---------------------------------------------------------------------

    fn response(scheme: HashingScheme) -> Result<PrepareSubmissionResponse> {
        let prepared = prepared(scheme)?;
        Ok(PrepareSubmissionResponse {
            prepared_transaction_hash: compute_prepared_transaction_hash(scheme, &prepared)?,
            prepared_transaction: Some(prepared),
            hashing_scheme_version: match scheme {
                HashingScheme::V2 => HashingSchemeVersion::V2 as i32,
                HashingScheme::V3 => HashingSchemeVersion::V3 as i32,
            },
            hashing_details: None,
            cost_estimation: None,
        })
    }

    #[test]
    fn accepts_a_faithful_response() -> Result {
        verify_prepared_submission(&response(HashingScheme::V2)?)?;
        verify_prepared_submission(&response(HashingScheme::V3)?)?;
        Ok(())
    }

    /// The core attack: show a benign transaction, ask for a signature over
    /// the hash of a different one.
    #[test]
    fn rejects_a_hash_that_does_not_belong_to_the_transaction() -> Result {
        let mut response = response(HashingScheme::V2)?;
        response.prepared_transaction_hash = vec![0x42; 32];
        assert!(verify_prepared_submission(&response).is_err());
        Ok(())
    }

    /// Swapping the payee on a create must change the hash.
    #[test]
    fn rejects_a_tampered_signatory() -> Result {
        let mut response = response(HashingScheme::V2)?;
        let prepared = required(response.prepared_transaction.as_mut(), "transaction")?;
        let transaction = required(prepared.transaction.as_mut(), "transaction")?;
        let node = required(transaction.nodes.first_mut(), "first node")?;
        let Some(daml_transaction::node::VersionedNode::V1(inner)) = node.versioned_node.as_mut()
        else {
            anyhow::bail!("expected a v1 node");
        };
        let Some(v1::node::NodeType::Create(create)) = inner.node_type.as_mut() else {
            anyhow::bail!("expected a create node");
        };
        create.signatories = parties(&["alice", "mallory"]);

        assert!(verify_prepared_submission(&response).is_err());
        Ok(())
    }

    /// Redirecting the transaction at a different synchronizer must change
    /// the hash — metadata is signed too.
    #[test]
    fn rejects_tampered_metadata() -> Result {
        let mut response = response(HashingScheme::V3)?;
        let prepared = required(response.prepared_transaction.as_mut(), "transaction")?;
        let metadata = required(prepared.metadata.as_mut(), "metadata")?;
        metadata.synchronizer_id = "other::synchronizer".to_string();

        assert!(verify_prepared_submission(&response).is_err());
        Ok(())
    }

    /// A coordinator must not be able to buy a weaker check by claiming a
    /// scheme we cannot reproduce.
    #[test]
    fn rejects_an_unsupported_scheme() -> Result {
        let mut response = response(HashingScheme::V3)?;
        response.hashing_scheme_version = HashingSchemeVersion::V4 as i32;
        assert!(verify_prepared_submission(&response).is_err());

        response.hashing_scheme_version = HashingSchemeVersion::Unspecified as i32;
        assert!(verify_prepared_submission(&response).is_err());
        Ok(())
    }

    /// Contract keys are not covered by V2, so a V2 payload carrying one must
    /// be refused rather than hashed without it.
    #[test]
    fn rejects_v3_only_fields_under_v2() -> Result {
        let mut prepared = prepared(HashingScheme::V2)?;
        let transaction = required(prepared.transaction.as_mut(), "transaction")?;
        let node = required(transaction.nodes.first_mut(), "first node")?;
        let Some(daml_transaction::node::VersionedNode::V1(inner)) = node.versioned_node.as_mut()
        else {
            anyhow::bail!("expected a v1 node");
        };
        let Some(v1::node::NodeType::Create(create)) = inner.node_type.as_mut() else {
            anyhow::bail!("expected a create node");
        };
        create.key = Some(global_key()?);

        assert!(compute_prepared_transaction_hash(HashingScheme::V2, &prepared).is_err());
        Ok(())
    }

    /// A cyclic tree must be reported, not followed until the stack runs out.
    #[test]
    fn rejects_a_cyclic_transaction_tree() -> Result {
        let mut prepared = prepared(HashingScheme::V2)?;
        let transaction = required(prepared.transaction.as_mut(), "transaction")?;
        transaction.roots = vec!["5".to_string()];
        transaction.nodes = vec![node(
            "5",
            v1::node::NodeType::Rollback(Rollback {
                children: vec!["5".to_string()],
            }),
        )];

        let error = compute_prepared_transaction_hash(HashingScheme::V2, &prepared)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("cyclically"), "unexpected error: {error}");
        Ok(())
    }

    #[test]
    fn rejects_a_missing_node() -> Result {
        let mut prepared = prepared(HashingScheme::V2)?;
        let transaction = required(prepared.transaction.as_mut(), "transaction")?;
        transaction.roots = vec!["nope".to_string()];
        assert!(compute_prepared_transaction_hash(HashingScheme::V2, &prepared).is_err());
        Ok(())
    }

    #[test]
    fn rejects_duplicate_node_ids() -> Result {
        let mut prepared = prepared(HashingScheme::V2)?;
        let transaction = required(prepared.transaction.as_mut(), "transaction")?;
        let duplicate = node(
            "0",
            v1::node::NodeType::Rollback(Rollback {
                children: Vec::new(),
            }),
        );
        transaction.nodes.push(duplicate);
        assert!(compute_prepared_transaction_hash(HashingScheme::V2, &prepared).is_err());
        Ok(())
    }

    /// An exercise node without its seed cannot be hashed the way Canton
    /// does, so it must not be signed.
    #[test]
    fn rejects_an_exercise_without_a_seed() -> Result {
        let mut prepared = prepared(HashingScheme::V2)?;
        let transaction = required(prepared.transaction.as_mut(), "transaction")?;
        transaction.node_seeds.retain(|seed| seed.node_id != 4);
        assert!(compute_prepared_transaction_hash(HashingScheme::V2, &prepared).is_err());
        Ok(())
    }
}
