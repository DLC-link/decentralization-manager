use anyhow::Context;
use bytes::{Buf, BufMut, BytesMut};
use canton_proto_rs::com::daml::ledger::api::v2::interactive::PrepareSubmissionResponse;
use sqlx::SqlitePool;

use crate::{
    canton_hash,
    canton_id::CantonId,
    error::Result,
    signing::PreparedTransactionHash,
    utils,
    workflow::storage::{WorkflowStorage, artifact_kinds},
};

/// Load this run's prepared submissions and return the hash of each, once it
/// is safe to sign.
///
/// The coordinator prepared the submissions, so a hash is only trustworthy
/// once this node has recomputed it from the transaction beside it.
/// Otherwise the coordinator could show a harmless transaction and collect a
/// signature over a different one. Each transaction must also act as the dec
/// party alone and be exactly the create the operator accepted.
///
/// # Errors
///
/// Errors when the submissions are missing, when their count differs from
/// the accepted invitation, or when one fails a check.
pub async fn verified_hashes(
    db: &SqlitePool,
    instance_name: &str,
    dec_party_id: &CantonId,
    intents: &[common::api::ContractDeploymentIntent],
) -> Result<Vec<PreparedTransactionHash>> {
    anyhow::ensure!(
        !intents.is_empty(),
        "Contracts invitation has no deployment commitments; restart this workflow with an upgraded coordinator"
    );
    let submission_rows = db
        .list_artifacts(instance_name, artifact_kinds::PREPARED_SUBMISSION)
        .await?;
    anyhow::ensure!(
        !submission_rows.is_empty(),
        "No PREPARED_SUBMISSION artifacts found for instance {instance_name} — did \
         PrepareSubmissions run?"
    );
    anyhow::ensure!(
        submission_rows.len() == intents.len(),
        "Prepared submission count differs from the accepted invitation"
    );

    let mut hashes = Vec::with_capacity(submission_rows.len());
    for ((ordinal, payload), intent) in submission_rows.iter().zip(intents) {
        let prepared: PrepareSubmissionResponse = utils::read_first_message_from_bytes(payload)?;
        verify_prepared_transaction(&prepared, dec_party_id)
            .with_context(|| format!("prepared submission {ordinal} failed validation"))?;
        verify_deployment_intent(&prepared, intent).with_context(|| {
            format!("prepared submission {ordinal} differs from the accepted create")
        })?;
        hashes.push(PreparedTransactionHash::new(
            prepared.prepared_transaction_hash,
        ));
    }
    tracing::info!(
        "Verified {count} prepared submission(s) against their transactions",
        count = hashes.len()
    );
    Ok(hashes)
}

fn verify_deployment_intent(
    prepared: &PrepareSubmissionResponse,
    intent: &common::api::ContractDeploymentIntent,
) -> Result {
    use canton_proto_rs::com::daml::ledger::api::v2::interactive::{
        daml_transaction::node::VersionedNode, transaction::v1::node::NodeType,
    };
    let transaction = prepared
        .prepared_transaction
        .as_ref()
        .and_then(|p| p.transaction.as_ref())
        .ok_or_else(|| anyhow::anyhow!("Missing transaction"))?;
    anyhow::ensure!(
        transaction.nodes.len() == 1 && transaction.roots.len() == 1,
        "Each accepted contract authorizes exactly one create node"
    );
    let node = &transaction.nodes[0];
    anyhow::ensure!(
        transaction.roots[0] == node.node_id,
        "Create must be the transaction root"
    );
    let Some(VersionedNode::V1(inner)) = &node.versioned_node else {
        anyhow::bail!("Unsupported transaction node version");
    };
    let Some(NodeType::Create(create)) = &inner.node_type else {
        anyhow::bail!(
            "Contract deployment only authorizes creates, never exercises or other node types"
        );
    };
    let template = create
        .template_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing create template"))?;
    anyhow::ensure!(
        template.package_id == intent.package_id
            && template.module_name == intent.module_name
            && template.entity_name == intent.entity_name,
        "Create template differs from the accepted invitation"
    );
    let argument = create
        .argument
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing create argument"))?;
    anyhow::ensure!(
        canton_hash::hash_value(argument)? == intent.argument_hash,
        "Create arguments differ from the accepted invitation"
    );
    Ok(())
}

/// Check a coordinator-prepared submission before signing its hash.
///
/// Two things have to hold. The hash must be the hash of the transaction the
/// coordinator sent — the Ledger API is explicit that a client must recompute
/// it when the preparing participant is not trusted, and here it is the
/// coordinator we are guarding against. And the transaction must act as the
/// dec party this run is for, so a signature made with this node's key can
/// only ever authorize that party.
///
/// # Errors
///
/// Errors if the hash does not belong to the transaction, if the hashing
/// scheme is one this node cannot reproduce, or if the transaction acts as any
/// party other than `dec_party_id`.
fn verify_prepared_transaction(
    prepared: &PrepareSubmissionResponse,
    dec_party_id: &CantonId,
) -> Result {
    canton_hash::verify_prepared_submission(prepared)?;

    let act_as = prepared
        .prepared_transaction
        .as_ref()
        .and_then(|tx| tx.metadata.as_ref())
        .and_then(|metadata| metadata.submitter_info.as_ref())
        .map(|submitter| submitter.act_as.as_slice())
        .unwrap_or_default();

    let expected = dec_party_id.to_string();
    if act_as != [expected.clone()] {
        anyhow::bail!(
            "prepared submission acts as {act_as:?}, but this run is for {expected} only"
        );
    }
    Ok(())
}

/// Decode a sequence of `varint(len)||proto` messages from a byte slice. Mirrors
/// `utils::read_all_messages_from_file` but operates on in-memory data — used
/// to round-trip blobs we used to read from disk.
pub(crate) fn read_all_messages_from_bytes<M: prost::Message + Default>(
    data: &[u8],
) -> Result<Vec<M>> {
    let mut cursor = data;
    let mut messages = Vec::new();
    while cursor.has_remaining() {
        let len = prost::encoding::decode_varint(&mut cursor)? as usize;
        if cursor.remaining() < len {
            anyhow::bail!(
                "Incomplete message: expected {len} bytes, but only {remaining} remaining",
                remaining = cursor.remaining()
            );
        }
        let message_bytes = &cursor[..len];
        cursor.advance(len);
        messages.push(M::decode(message_bytes)?);
    }
    Ok(messages)
}

/// Encode a slice of protobuf messages as `varint(len)||proto` × N, matching the
/// byte layout produced by `utils::write_messages_to_file`. Round-trips with
/// `utils::read_all_messages_from_file` / `read_all_messages_from_bytes`.
pub(crate) fn encode_messages_length_prefixed<M: prost::Message>(messages: &[M]) -> Vec<u8> {
    let mut buffer = BytesMut::new();
    for message in messages {
        let encoded = message.encode_to_vec();
        prost::encoding::encode_varint(encoded.len() as u64, &mut buffer);
        buffer.put_slice(&encoded);
    }
    buffer.to_vec()
}

#[cfg(test)]
mod deployment_intent_tests {
    use super::*;
    use canton_proto_rs::com::daml::ledger::api::v2::{
        Identifier, Record, RecordField, Value,
        interactive::{
            DamlTransaction, PreparedTransaction, daml_transaction,
            transaction::v1::{self, node::NodeType},
        },
        value,
    };

    fn argument(text: &str) -> Value {
        Value {
            sum: Some(value::Sum::Record(Record {
                record_id: None,
                fields: vec![RecordField {
                    label: String::new(),
                    value: Some(Value {
                        sum: Some(value::Sum::Text(text.into())),
                    }),
                }],
            })),
        }
    }

    #[test]
    fn argument_commitments_match_lf_trailing_none_normalization() -> Result {
        let optional = |inner: Option<Value>| Value {
            sum: Some(value::Sum::Optional(Box::new(
                canton_proto_rs::com::daml::ledger::api::v2::Optional {
                    value: inner.map(Box::new),
                },
            ))),
        };
        let record = |values: Vec<Value>| Value {
            sum: Some(value::Sum::Record(Record {
                record_id: None,
                fields: values
                    .into_iter()
                    .map(|v| RecordField {
                        label: String::new(),
                        value: Some(v),
                    })
                    .collect(),
            })),
        };
        let original = record(vec![argument("accepted"), optional(None)]);
        let normalized = record(vec![argument("accepted")]);
        assert_eq!(
            canton_hash::hash_value(&original)?,
            canton_hash::hash_value(&normalized)?
        );
        // Normalize recursively, including the outer record's trailing None.
        assert_eq!(
            canton_hash::hash_value(&record(vec![original, optional(None)]))?,
            canton_hash::hash_value(&record(vec![normalized.clone()]))?
        );
        for changed in [
            record(vec![
                argument("accepted"),
                optional(Some(argument("injected"))),
            ]),
            record(vec![optional(None), argument("accepted")]),
            record(vec![argument("changed")]),
        ] {
            assert_ne!(
                canton_hash::hash_value(&changed)?,
                canton_hash::hash_value(&normalized)?
            );
        }
        Ok(())
    }

    #[test]
    fn only_the_accepted_create_can_be_signed() -> Result {
        let intent = common::api::ContractDeploymentIntent {
            package_id: "a".repeat(64),
            module_name: "Governance".into(),
            entity_name: "Rules".into(),
            argument_hash: canton_hash::hash_value(&argument("accepted"))?,
        };
        let create = v1::Create {
            template_id: Some(Identifier {
                package_id: intent.package_id.clone(),
                module_name: intent.module_name.clone(),
                entity_name: intent.entity_name.clone(),
            }),
            argument: Some(argument("accepted")),
            ..Default::default()
        };
        let response = |node_type| PrepareSubmissionResponse {
            prepared_transaction: Some(PreparedTransaction {
                transaction: Some(DamlTransaction {
                    roots: vec!["0".into()],
                    nodes: vec![daml_transaction::Node {
                        node_id: "0".into(),
                        versioned_node: Some(daml_transaction::node::VersionedNode::V1(v1::Node {
                            node_type: Some(node_type),
                        })),
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        verify_deployment_intent(&response(NodeType::Create(create.clone())), &intent)?;
        for field in ["package", "module", "entity", "argument"] {
            let mut changed = create.clone();
            let template = changed
                .template_id
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("missing test template"))?;
            match field {
                "package" => template.package_id = "b".repeat(64),
                "module" => template.module_name = "Other".into(),
                "entity" => template.entity_name = "Other".into(),
                _ => changed.argument = Some(argument("substituted")),
            }
            assert!(
                verify_deployment_intent(&response(NodeType::Create(changed)), &intent).is_err(),
                "accepted changed {field}"
            );
        }
        for node in [
            NodeType::Exercise(Default::default()),
            NodeType::Fetch(Default::default()),
            NodeType::Rollback(Default::default()),
        ] {
            assert!(verify_deployment_intent(&response(node), &intent).is_err());
        }
        let mut extra = response(NodeType::Create(create));
        let tx = extra
            .prepared_transaction
            .as_mut()
            .and_then(|t| t.transaction.as_mut())
            .ok_or_else(|| anyhow::anyhow!("missing transaction"))?;
        tx.nodes.push(tx.nodes[0].clone());
        assert!(verify_deployment_intent(&extra, &intent).is_err());
        Ok(())
    }

    #[test]
    fn argument_commitments_ignore_ledger_annotations_and_map_order() -> Result {
        let original = argument("accepted");
        let mut enriched = original.clone();
        if let Some(value::Sum::Record(record)) = enriched.sum.as_mut() {
            record.record_id = Some(Identifier {
                package_id: "a".repeat(64),
                module_name: "M".into(),
                entity_name: "T".into(),
            });
            record.fields[0].label = "ledgerFieldName".into();
        }
        assert_eq!(
            canton_hash::hash_value(&original)?,
            canton_hash::hash_value(&enriched)?
        );
        let entries: Vec<_> = ["alice", "bob"]
            .into_iter()
            .map(
                |party| canton_proto_rs::com::daml::ledger::api::v2::gen_map::Entry {
                    key: Some(Value {
                        sum: Some(value::Sum::Party(party.into())),
                    }),
                    value: Some(enriched.clone()),
                },
            )
            .collect();
        let map = |entries| Value {
            sum: Some(value::Sum::GenMap(
                canton_proto_rs::com::daml::ledger::api::v2::GenMap { entries },
            )),
        };
        assert_eq!(
            canton_hash::hash_value(&map(entries.clone()))?,
            canton_hash::hash_value(&map(entries.into_iter().rev().collect()))?
        );
        Ok(())
    }
}
