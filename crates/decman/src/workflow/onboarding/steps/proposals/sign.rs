use bytes::{BufMut, BytesMut};
use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::SignedTopologyTransaction,
    topology::admin::v30::{
        SignTransactionsRequest, StoreId, Synchronizer, store_id, synchronizer,
    },
};
use prost::Message;
use sqlx::SqlitePool;

use crate::{
    config::NodeConfig,
    error::Result,
    signing::custody,
    utils,
    workflow::{
        onboarding::steps::generate_keys::own_new_daml_key,
        storage::{WorkflowStorage, artifact_kinds},
        topology::sign_transactions_with_topology_retry,
    },
};

/// Sign a single topology proposal and persist the result.
///
/// `proposal_data` is the `varint(len)||SignedTopologyTransaction` blob the
/// coordinator sent (matches the original on-disk dns_proto.bin /
/// p2p_proto.bin format). The signed result is persisted as a per-peer
/// artefact under `(instance, kind, self_id)` using the same
/// length-prefixed framing.
///
/// `adds_own_key` marks the P2P proposal, which adds this node's new Daml
/// key. A key DecMan generated signs it here, because the vault does not
/// hold that key.
async fn sign_proposal(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    output_kind: &str,
    proposal_type: &str,
    proposal_data: &[u8],
    adds_own_key: bool,
) -> Result {
    tracing::info!("Signing {proposal_type} proposal...");

    let node_id = config.participant_id().to_string();
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    tracing::info!("Using {proposal_type} proposal from coordinator payload");
    let transaction: SignedTopologyTransaction =
        utils::read_first_message_from_bytes(proposal_data)?;

    let request = SignTransactionsRequest {
        transactions: vec![transaction],
        signed_by: vec![],
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id)),
            })),
        }),
        force_flags: vec![],
    };

    tracing::debug!("Calling SignTransactions RPC for {proposal_type}...");
    let mut response =
        sign_transactions_with_topology_retry(config, request, proposal_type).await?;

    if response.transactions.is_empty() {
        anyhow::bail!("No signed transaction returned for {proposal_type}");
    }
    if adds_own_key && let Some(own_key) = own_new_daml_key(config, storage, instance_name).await? {
        for transaction in &mut response.transactions {
            custody::sign_with_generated_key(config, storage, transaction, &own_key).await?;
        }
    }

    // Persist as a single per-peer artefact. Each transaction is written
    // as `varint(len)||proto`; concatenated, this matches the previous on-disk
    // format produced by `write_messages_to_file`.
    let mut buffer = BytesMut::new();
    for tx in &response.transactions {
        let encoded = tx.encode_to_vec();
        prost::encoding::encode_varint(encoded.len() as u64, &mut buffer);
        buffer.put_slice(&encoded);
    }

    storage
        .write_artifact(instance_name, output_kind, Some(&node_id), &buffer)
        .await?;

    tracing::info!("{proposal_type} proposal signed successfully");
    Ok(())
}

/// Sign DNS proposal with peer's key
///
/// This step must be run by each peer participant (except the coordinator who created the proposal).
/// Each peer signs the DNS proposal with their namespace key.
pub async fn sign_dns_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    proposal_data: &[u8],
) -> Result {
    sign_proposal(
        config,
        storage,
        instance_name,
        artifact_kinds::SIGNED_DNS_PROPOSAL,
        "DNS",
        proposal_data,
        false,
    )
    .await
}

/// Sign P2P proposals with peer's key
///
/// **Canton 3.4+**: Signing keys are now embedded in the P2P mapping.
/// This function signs the P2P proposal which contains both participant and key information.
pub async fn sign_p2p_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    proposal_data: &[u8],
) -> Result {
    sign_proposal(
        config,
        storage,
        instance_name,
        artifact_kinds::SIGNED_P2P_PROPOSAL,
        "P2P",
        proposal_data,
        true,
    )
    .await
}

/// An onboarding member whose Daml key DecMan generated, against a fake
/// Canton that adds no signature of its own.
#[cfg(test)]
mod with_a_generated_key {
    use canton_proto_rs::com::digitalasset::canton::{
        crypto::{
            admin::v30::vault_service_client::VaultServiceClient,
            v30::{SigningKeysWithThreshold, SigningPublicKey},
        },
        protocol::v30::{
            DecentralizedNamespaceDefinition, PartyToParticipant, TopologyMapping,
            TopologyTransaction, enums, topology_mapping,
        },
        version::v1::{UntypedVersionedMessage, untyped_versioned_message},
    };

    use super::*;
    use crate::{
        db::MIGRATOR,
        signing::{
            custody::{SoftwareKeys, new_daml_key},
            fake_admin_api::{FakeAdminApi, TestNode, VaultKey},
            key_store::topology_transaction_hash,
            vault_export::ed25519_public_key,
        },
        workflow::onboarding::steps::generate_keys::encode_keys_payload,
    };

    const RUN: &str = "party-creation";
    const NAMESPACE: &str = "1220bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// A member on its onboarding run, with the Daml key GenerateKeys had
    /// DecMan generate. Returns the member and its key.
    async fn member(fake: &FakeAdminApi, db: &SqlitePool) -> Result<(TestNode, SigningPublicKey)> {
        let node = fake.node(SoftwareKeys::Stored, true).await?;
        sqlx::query(
            "INSERT INTO workflow_runs (
                 instance_name, kind, role, status, current_step, step_index, step_total,
                 config_json, expected_peers_json, completed_peers_json,
                 created_at, updated_at
             ) VALUES (?1, 'Onboarding', 'Peer', 'inprogress', 'SignP2p',
                       0, 1, '{}', '[]', '[]', 0, 0)",
        )
        .bind(RUN)
        .execute(db)
        .await?;
        let key = new_daml_key(
            &node.config,
            db,
            &mut VaultServiceClient::new(node.config.admin_channel().await?),
            "party-daml-transactions-run-1",
            None,
        )
        .await?
        .public_key;
        db.write_artifact(
            RUN,
            artifact_kinds::PEER_PUBLIC_KEYS,
            Some(&node.config.participant_id().to_string()),
            &encode_keys_payload(&VaultKey::software("party-namespace", 1).public_key, &key),
        )
        .await?;
        Ok((node, key))
    }

    fn payload(mapping: topology_mapping::Mapping) -> Vec<u8> {
        let transaction = TopologyTransaction {
            operation: enums::TopologyChangeOp::AddReplace as i32,
            serial: 1,
            mapping: Some(TopologyMapping {
                mapping: Some(mapping),
            }),
        };
        utils::encode_length_prefixed_message(&SignedTopologyTransaction {
            transaction: UntypedVersionedMessage {
                version: 30,
                wrapper: Some(untyped_versioned_message::Wrapper::Data(
                    transaction.encode_to_vec(),
                )),
            }
            .encode_to_vec(),
            ..Default::default()
        })
    }

    async fn signed(
        node: &TestNode,
        db: &SqlitePool,
        kind: &str,
    ) -> Result<SignedTopologyTransaction> {
        let bytes = db
            .read_artifact(RUN, kind, Some(&node.config.participant_id().to_string()))
            .await?
            .ok_or_else(|| anyhow::anyhow!("no {kind} was saved"))?;
        utils::read_first_message_from_bytes(&bytes)
    }

    /// Canton requires each new party key to sign the mapping that adds it.
    /// The vault does not hold a key DecMan generated, so the member adds
    /// that signature over the transaction's hash itself.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_member_signs_the_party_mapping_with_its_generated_key(db: SqlitePool) -> Result {
        let fake = FakeAdminApi::start(vec![]).await?;
        let (node, key) = member(&fake, &db).await?;
        let p2p = payload(topology_mapping::Mapping::PartyToParticipant(
            PartyToParticipant {
                party: format!("party::{NAMESPACE}"),
                threshold: 1,
                participants: Vec::new(),
                party_signing_keys: Some(SigningKeysWithThreshold {
                    keys: vec![VaultKey::software("peer", 7).public_key, key.clone()],
                    threshold: 1,
                }),
            },
        ));

        sign_p2p_proposals(&node.config, &db, RUN, &p2p).await?;

        let transaction = signed(&node, &db, artifact_kinds::SIGNED_P2P_PROPOSAL).await?;
        let [signature] = transaction.signatures.as_slice() else {
            anyhow::bail!(
                "expected DecMan's one signature, got {:?}",
                transaction.signatures
            );
        };
        assert_eq!(
            signature.signed_by,
            utils::compute_fingerprint(&key).to_string()
        );
        let verifying = ed25519_dalek::VerifyingKey::from_bytes(&ed25519_public_key(&key)?)?;
        let bytes: [u8; 64] = signature.signature.as_slice().try_into()?;
        verifying.verify_strict(
            &topology_transaction_hash(&transaction.transaction),
            &ed25519_dalek::Signature::from_bytes(&bytes),
        )?;
        Ok(())
    }

    /// The Daml key signs only the mapping that adds it. The namespace
    /// proposal gets no signature from it.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_member_does_not_sign_the_namespace_with_its_daml_key(db: SqlitePool) -> Result {
        let fake = FakeAdminApi::start(vec![]).await?;
        let (node, _) = member(&fake, &db).await?;
        let dns = payload(topology_mapping::Mapping::DecentralizedNamespaceDefinition(
            DecentralizedNamespaceDefinition {
                decentralized_namespace: NAMESPACE.to_string(),
                threshold: 1,
                owners: vec![NAMESPACE.to_string()],
            },
        ));

        sign_dns_proposals(&node.config, &db, RUN, &dns).await?;

        let transaction = signed(&node, &db, artifact_kinds::SIGNED_DNS_PROPOSAL).await?;
        assert!(transaction.signatures.is_empty());
        Ok(())
    }
}
