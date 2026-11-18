use std::time::Duration;

use anyhow::{Context, ensure};
use canton_proto_rs::com::digitalasset::canton::{
    admin::participant::v30::{
        ListConnectedSynchronizersRequest, ListRegisteredSynchronizersRequest,
        synchronizer_connectivity_service_client::SynchronizerConnectivityServiceClient,
    },
    protocol::v30::{
        PartyToParticipant, SignedTopologyTransaction, TopologyMapping, enums::TopologyChangeOp,
        topology_mapping,
    },
    topology::admin::v30::{
        AddTransactionsRequest, ListAllRequest, SignTransactionsRequest,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
    version::v1::{UntypedVersionedMessage, untyped_versioned_message},
};
use common::api::HostPermission;
use prost::Message;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use super::{
    AddHostsError, CurrentPartyTopology, add_hosts_mapping, capture_replication_offsets,
    read_party_to_participant, replication_target, require_live_source,
};
use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    consts::{topology_retry_delay_secs, topology_retry_max_attempts},
    db::schema::SchemaRead,
    error::Result,
    utils,
    workflow::{
        party_replication::{ReplicationTarget, check_vetted_packages_held, open_import_window},
        storage::artifact_kinds,
        topology, topology_version,
    },
};

#[derive(Serialize, Deserialize)]
struct Staged {
    party_id: String,
    new_hosts: Vec<String>,
    permission: HostPermission,
    base_serial: u32,
    signed_transactions: Vec<Vec<u8>>,
}

fn host_ids(hosts: &[CantonId]) -> Vec<String> {
    let mut ids: Vec<_> = hosts.iter().map(ToString::to_string).collect();
    ids.sort();
    ids
}

fn next_serial(base_serial: u32) -> Result<u32> {
    base_serial
        .checked_add(1)
        .context("the topology serial cannot advance")
}

fn decode_signed(bytes: &[u8]) -> Result<SignedTopologyTransaction> {
    let envelope = UntypedVersionedMessage::decode(bytes)?;
    ensure!(
        envelope.version == 30,
        "unsupported signed topology envelope version"
    );
    let Some(untyped_versioned_message::Wrapper::Data(data)) = envelope.wrapper else {
        anyhow::bail!("signed topology envelope has no data");
    };
    let signed = SignedTopologyTransaction::decode(data.as_slice())?;
    ensure!(signed.proposal, "the relay accepts only topology proposals");
    ensure!(
        !signed.signatures.is_empty(),
        "a relayed proposal needs a signature"
    );
    ensure!(
        signed.multi_transaction_signatures.is_empty(),
        "the relay accepts only signatures on one topology transaction"
    );
    Ok(signed)
}

fn encode_signed(signed: &SignedTopologyTransaction) -> Vec<u8> {
    UntypedVersionedMessage {
        version: 30,
        wrapper: Some(untyped_versioned_message::Wrapper::Data(
            signed.encode_to_vec(),
        )),
    }
    .encode_to_vec()
}

fn exact_mapping(
    signed: &SignedTopologyTransaction,
    mapping: &PartyToParticipant,
    serial: u32,
) -> Result<()> {
    let transaction = topology_version::decode_transaction(&signed.transaction)?;
    ensure!(
        transaction.operation == TopologyChangeOp::AddReplace as i32
            && transaction.serial == serial
            && transaction.mapping
                == Some(TopologyMapping {
                    mapping: Some(topology_mapping::Mapping::PartyToParticipant(
                        mapping.clone()
                    )),
                }),
        "the relayed transaction is not exactly the pinned add-hosts proposal"
    );
    Ok(())
}

#[allow(deprecated)]
async fn pending_proposal(
    config: &NodeConfig,
    mapping: &PartyToParticipant,
    serial: u32,
) -> Result<Option<SignedTopologyTransaction>> {
    let synchronizer = utils::get_synchronizer_id(config).await?;
    let mut query = topology::head_state_query(&synchronizer);
    query.proposals = true;
    let mut client = TopologyManagerReadServiceClient::new(config.admin_channel().await?)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);
    let response = client
        .list_all(tonic::Request::new(ListAllRequest {
            base_query: Some(query),
            exclude_mappings: vec![],
            filter_namespace: mapping
                .party
                .rsplit_once("::")
                .context("party has no namespace")?
                .1
                .to_string(),
        }))
        .await?
        .into_inner();
    for item in response.result.into_iter().flat_map(|result| result.items) {
        if item.rejection_reason.is_some() {
            continue;
        }
        let signed = decode_signed(&item.transaction)?;
        if exact_mapping(&signed, mapping, serial).is_ok() {
            return Ok(Some(signed));
        }
    }
    Ok(None)
}

async fn offline_manual(config: &NodeConfig) -> Result<bool> {
    let mut client = SynchronizerConnectivityServiceClient::new(config.admin_channel().await?);
    let connected = client
        .list_connected_synchronizers(tonic::Request::new(ListConnectedSynchronizersRequest {}))
        .await?
        .into_inner();
    let registered = client
        .list_registered_synchronizers(tonic::Request::new(ListRegisteredSynchronizersRequest {
            all_statuses: false,
        }))
        .await?
        .into_inner();
    Ok(connected.connected_synchronizers.is_empty()
        && registered
            .results
            .into_iter()
            .filter_map(|row| row.config)
            .any(|connection| {
                connection.synchronizer_alias == config.synchronizer() && connection.manual_connect
            }))
}

pub(super) async fn ensure_connected(
    config: &NodeConfig,
) -> std::result::Result<(), AddHostsError> {
    let mut client = SynchronizerConnectivityServiceClient::new(
        config
            .admin_channel()
            .await
            .map_err(AddHostsError::Canton)?,
    );
    let connected = client
        .list_connected_synchronizers(tonic::Request::new(ListConnectedSynchronizersRequest {}))
        .await
        .map_err(|error| AddHostsError::Canton(error.into()))?
        .into_inner();
    if !connected.connected_synchronizers.iter().any(|connection| {
        connection.synchronizer_alias == config.synchronizer() && connection.healthy
    }) {
        return Err(AddHostsError::Busy(
            "the participant must be connected and healthy before this add-hosts step".to_string(),
        ));
    }
    Ok(())
}

/// Read the participant-wide tenant import owner.
///
/// # Errors
/// Returns a database error when the durable owner cannot be read.
pub async fn import_window_owner(
    db: &SqlitePool,
    participant: &CantonId,
) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT instance_name FROM tenant_import_window WHERE participant_id = ?",
    )
    .bind(participant.to_string())
    .fetch_optional(db)
    .await?)
}

async fn claim_import_window(db: &SqlitePool, target: &ReplicationTarget) -> Result<bool> {
    sqlx::query(
        "INSERT INTO tenant_import_window (participant_id, instance_name, party_id) \
         SELECT ?1, ?2, ?3 \
         WHERE NOT EXISTS (SELECT 1 FROM workflow_runs WHERE status = 'inprogress') \
           AND NOT EXISTS (SELECT 1 FROM former_dec_party WHERE purge_status = 'running') \
         ON CONFLICT(participant_id) DO NOTHING",
    )
    .bind(target.target_participant_id.to_string())
    .bind(&target.instance_name)
    .bind(target.party_id.to_string())
    .execute(db)
    .await?;
    Ok(import_window_owner(db, &target.target_participant_id)
        .await?
        .as_deref()
        == Some(target.instance_name.as_str()))
}

/// Release ownership only after the tenant import reconnects and verifies health.
/// The completed marker prevents a later stage retry from opening another window.
///
/// # Errors
/// Returns a database error without partially releasing ownership.
pub async fn release_import_window_owner(
    db: &SqlitePool,
    target: &ReplicationTarget,
) -> Result<()> {
    let mut transaction = db.begin().await?;
    let deleted = sqlx::query(
        "DELETE FROM tenant_import_window WHERE participant_id = ? AND instance_name = ?",
    )
    .bind(target.target_participant_id.to_string())
    .bind(&target.instance_name)
    .execute(&mut *transaction)
    .await?;
    if deleted.rows_affected() > 0 {
        sqlx::query(
            "INSERT INTO tenant_replication_artifacts \
             (instance_name, artifact_kind, attestor_id, payload, created_at) \
             VALUES (?, ?, '', ?, CAST(strftime('%s', 'now') AS INTEGER)) \
             ON CONFLICT(instance_name, artifact_kind, attestor_id) DO NOTHING",
        )
        .bind(&target.instance_name)
        .bind(artifact_kinds::TENANT_ADD_HOSTS_IMPORT_COMPLETED)
        .bind(b"1".as_slice())
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(())
}

async fn return_staged(
    config: &NodeConfig,
    db: &SqlitePool,
    target: &ReplicationTarget,
    staged: Staged,
) -> std::result::Result<Vec<Vec<u8>>, AddHostsError> {
    if import_window_owner(db, config.participant_id())
        .await
        .map_err(AddHostsError::Canton)?
        .as_deref()
        != Some(target.instance_name.as_str())
    {
        return Err(AddHostsError::Invalid(anyhow::anyhow!(
            "this replication no longer owns the import window; a completed import is not staged again"
        )));
    }
    if target
        .read_artifact(db, artifact_kinds::TENANT_ADD_HOSTS_STAGE_READY, None)
        .await
        .map_err(AddHostsError::Canton)?
        .is_none()
    {
        open_import_window(config, db, target)
            .await
            .map_err(AddHostsError::Canton)?;
        ensure_offline(config).await?;
        target
            .write_artifact_if_absent(db, artifact_kinds::TENANT_ADD_HOSTS_STAGE_READY, None, b"1")
            .await
            .map_err(AddHostsError::Canton)?;
    } else {
        ensure_offline(config).await?;
    }
    Ok(staged.signed_transactions)
}

async fn ensure_offline(config: &NodeConfig) -> std::result::Result<(), AddHostsError> {
    if !offline_manual(config)
        .await
        .map_err(AddHostsError::Canton)?
    {
        return Err(AddHostsError::Busy(
            "the staged participant must remain disconnected on manual connection; inspect it before retrying"
                .to_string(),
        ));
    }
    Ok(())
}

/// Sign an exact local proposal and disconnect before revealing the signature.
/// The caller serializes staging against relay admission on this participant.
///
/// # Errors
/// Returns a validation, stale-serial, busy-window, or Canton/database error.
#[allow(clippy::too_many_arguments)]
pub async fn stage_add_hosts(
    config: &NodeConfig,
    db: &SqlitePool,
    party_id: &str,
    new_hosts: &[CantonId],
    permission: HostPermission,
    base_serial: u32,
) -> std::result::Result<Vec<Vec<u8>>, AddHostsError> {
    let target = replication_target(party_id, config.participant_id(), base_serial)
        .map_err(AddHostsError::Invalid)?;
    if db
        .get_acs_import_quarantine(&target.party_id, &target.target_participant_id)
        .await
        .map_err(AddHostsError::Canton)?
        .is_some()
    {
        return Err(AddHostsError::Invalid(anyhow::anyhow!(
            "this participant is quarantined for the party; an operator must repair it before staging"
        )));
    }
    if let Some(payload) = target
        .read_artifact(db, artifact_kinds::TENANT_ADD_HOSTS_STAGED_SIGNATURES, None)
        .await
        .map_err(AddHostsError::Canton)?
    {
        let staged: Staged = serde_json::from_slice(&payload)
            .map_err(|error| AddHostsError::Canton(error.into()))?;
        if staged.party_id != party_id
            || staged.new_hosts != host_ids(new_hosts)
            || staged.permission != permission
            || staged.base_serial != base_serial
        {
            return Err(AddHostsError::Invalid(anyhow::anyhow!(
                "the stage request differs from the request already signed for this replication"
            )));
        }
        return return_staged(config, db, &target, staged).await;
    }
    let current = read_current(config, party_id).await?;
    if current.serial != base_serial {
        return Err(AddHostsError::StaleSerial {
            party: party_id.to_string(),
            pinned: base_serial,
            found: current.serial,
        });
    }
    if !new_hosts.contains(config.participant_id()) {
        return Err(AddHostsError::Invalid(anyhow::anyhow!(
            "only a joining participant stages a signature"
        )));
    }
    ensure_connected(config).await?;
    let expected = add_hosts_mapping(&current.mapping, new_hosts, permission)
        .map_err(AddHostsError::Invalid)?;
    check_vetted_packages_held(config)
        .await
        .map_err(AddHostsError::Canton)?;
    capture_replication_offsets(config, db, party_id, new_hosts, base_serial, None).await?;
    let serial = next_serial(base_serial).map_err(AddHostsError::Invalid)?;
    let mut proposal = None;
    for attempt in 1..=topology_retry_max_attempts() {
        proposal = pending_proposal(config, &expected, serial)
            .await
            .map_err(AddHostsError::Canton)?;
        if proposal.is_some() {
            break;
        }
        if attempt < topology_retry_max_attempts() {
            tokio::time::sleep(Duration::from_secs(topology_retry_delay_secs())).await;
        }
    }
    let Some(proposal) = proposal else {
        return Err(AddHostsError::Invalid(anyhow::anyhow!(
            "the exact add-hosts proposal has not reached this participant; publish the party authorization on a current host first"
        )));
    };
    if !claim_import_window(db, &target)
        .await
        .map_err(AddHostsError::Canton)?
    {
        return Err(AddHostsError::Busy(
            "another import, workflow, or purge owns this participant".to_string(),
        ));
    }
    let synchronizer = utils::get_synchronizer_id(config)
        .await
        .map_err(AddHostsError::Canton)?;
    let signed = topology::sign_transactions_with_topology_retry(
        config,
        SignTransactionsRequest {
            transactions: vec![proposal],
            signed_by: vec![],
            store: Some(topology::synchronizer_store_id(&synchronizer)),
            force_flags: vec![],
        },
        "tenant add-hosts stage",
    )
    .await
    .map_err(AddHostsError::Canton)?
    .transactions;
    if signed.len() != 1 {
        return Err(AddHostsError::Canton(anyhow::anyhow!(
            "Canton returned an unexpected number of signed proposals"
        )));
    }
    for transaction in &signed {
        exact_mapping(transaction, &expected, serial).map_err(AddHostsError::Canton)?;
    }
    let staged = Staged {
        party_id: party_id.to_string(),
        new_hosts: host_ids(new_hosts),
        permission,
        base_serial,
        signed_transactions: signed.iter().map(encode_signed).collect(),
    };
    target
        .write_artifact_if_absent(
            db,
            artifact_kinds::TENANT_ADD_HOSTS_STAGED_SIGNATURES,
            None,
            &serde_json::to_vec(&staged).map_err(|error| AddHostsError::Canton(error.into()))?,
        )
        .await
        .map_err(AddHostsError::Canton)?;
    let persisted = target
        .read_artifact(db, artifact_kinds::TENANT_ADD_HOSTS_STAGED_SIGNATURES, None)
        .await
        .map_err(AddHostsError::Canton)?
        .context("the staged signature is not persisted")
        .map_err(AddHostsError::Canton)?;
    return_staged(
        config,
        db,
        &target,
        serde_json::from_slice(&persisted).map_err(|error| AddHostsError::Canton(error.into()))?,
    )
    .await
}

async fn read_current(
    config: &NodeConfig,
    party_id: &str,
) -> std::result::Result<CurrentPartyTopology, AddHostsError> {
    read_party_to_participant(config, party_id)
        .await
        .map_err(AddHostsError::Canton)?
        .ok_or_else(|| AddHostsError::UnknownParty {
            party: party_id.to_string(),
        })
}

/// Publish joining signatures from a connected current host after staging ends.
/// Canton verifies each signature and accumulates authorization for the mapping.
///
/// # Errors
/// Rejects altered mappings, unrelated transactions, stale pins, and Canton failures.
#[allow(clippy::too_many_arguments)]
pub async fn publish_add_hosts(
    config: &NodeConfig,
    _db: &SqlitePool,
    party_id: &str,
    new_hosts: &[CantonId],
    permission: HostPermission,
    base_serial: u32,
    signed_transactions: &[Vec<u8>],
) -> std::result::Result<u32, AddHostsError> {
    let current = read_current(config, party_id).await?;
    require_live_source(config, &current).map_err(AddHostsError::Invalid)?;
    if signed_transactions.is_empty() {
        return Err(AddHostsError::Invalid(anyhow::anyhow!(
            "no staged signatures were supplied"
        )));
    }
    let signed: Vec<_> = signed_transactions
        .iter()
        .map(|bytes| decode_signed(bytes))
        .collect::<Result<_>>()
        .map_err(AddHostsError::Invalid)?;
    let merged = merge_proposals(signed.clone()).map_err(AddHostsError::Invalid)?;
    let serial = next_serial(base_serial).map_err(AddHostsError::Invalid)?;
    if current.serial == serial {
        for transaction in &signed {
            exact_mapping(transaction, &current.mapping, serial).map_err(AddHostsError::Invalid)?;
        }
        return Ok(base_serial);
    }
    if current.serial != base_serial {
        return Err(AddHostsError::StaleSerial {
            party: party_id.to_string(),
            pinned: base_serial,
            found: current.serial,
        });
    }
    let expected = add_hosts_mapping(&current.mapping, new_hosts, permission)
        .map_err(AddHostsError::Invalid)?;
    for transaction in &signed {
        exact_mapping(transaction, &expected, serial).map_err(AddHostsError::Invalid)?;
    }
    ensure_connected(config).await?;
    let synchronizer = utils::get_synchronizer_id(config)
        .await
        .map_err(AddHostsError::Canton)?;
    let mut client = TopologyManagerWriteServiceClient::new(
        config
            .admin_channel()
            .await
            .map_err(AddHostsError::Canton)?,
    );
    client
        .add_transactions(tonic::Request::new(AddTransactionsRequest {
            transactions: vec![merged],
            force_changes: vec![],
            store: Some(topology::synchronizer_store_id(&synchronizer)),
            wait_to_become_effective: None,
        }))
        .await
        .map_err(|error| AddHostsError::Canton(error.into()))?;
    tracing::info!(%party_id, base_serial, "published add-hosts signatures from disconnected joiners");
    Ok(base_serial)
}

fn merge_proposals(
    transactions: Vec<SignedTopologyTransaction>,
) -> Result<SignedTopologyTransaction> {
    let mut transactions = transactions.into_iter();
    let mut merged = transactions
        .next()
        .context("no staged proposal to publish")?;
    for transaction in transactions {
        ensure!(
            transaction.transaction == merged.transaction,
            "every staged signature must sign byte-identical topology transactions"
        );
        merged.signatures.extend(transaction.signatures);
    }
    topology::dedupe_signatures(&mut merged);
    Ok(merged)
}

/// Admit an import only for the exact proposal this offline participant stages.
///
/// # Errors
/// Returns database or Canton errors while checking the staged proposal.
pub async fn import_onboarding_ready(
    config: &NodeConfig,
    db: &SqlitePool,
    target: &ReplicationTarget,
    base_serial: u32,
) -> Result<bool> {
    if target.target_participant_id != *config.participant_id()
        || target.instance_name
            != super::replication_instance(
                &target.party_id.to_string(),
                config.participant_id(),
                base_serial,
            )
    {
        return Ok(false);
    }
    if import_window_owner(db, config.participant_id())
        .await?
        .as_deref()
        != Some(target.instance_name.as_str())
        || target
            .read_artifact(db, artifact_kinds::TENANT_ADD_HOSTS_STAGE_READY, None)
            .await?
            .is_none()
        || target
            .read_artifact(db, target.artifacts.import_inflight, None)
            .await?
            .as_deref()
            != Some(b"disconnect-first")
        || !offline_manual(config).await?
    {
        return Ok(false);
    }
    let Some(payload) = target
        .read_artifact(db, artifact_kinds::TENANT_ADD_HOSTS_STAGED_SIGNATURES, None)
        .await?
    else {
        return Ok(false);
    };
    let staged: Staged = serde_json::from_slice(&payload)?;
    if staged.party_id != target.party_id.to_string()
        || staged.base_serial != base_serial
        || staged.signed_transactions.len() != 1
    {
        return Ok(false);
    }
    let signed = decode_signed(&staged.signed_transactions[0])?;
    let transaction = topology_version::decode_transaction(&signed.transaction)?;
    let Some(TopologyMapping {
        mapping: Some(topology_mapping::Mapping::PartyToParticipant(mapping)),
    }) = transaction.mapping
    else {
        return Ok(false);
    };
    if mapping.party != target.party_id.to_string()
        || !staged
            .new_hosts
            .contains(&config.participant_id().to_string())
        || !mapping.participants.iter().any(|host| {
            host.participant_uid == config.participant_id().to_string() && host.onboarding.is_some()
        })
    {
        return Ok(false);
    }
    exact_mapping(&signed, &mapping, next_serial(base_serial)?)?;
    Ok(
        pending_proposal(config, &mapping, next_serial(base_serial)?)
            .await?
            .is_some(),
    )
}

#[cfg(test)]
mod tests {
    use std::slice;

    use canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::Signature,
        protocol::v30::{
            TopologyTransaction,
            party_to_participant::{HostingParticipant, hosting_participant},
        },
    };

    use super::*;
    use crate::db::MIGRATOR;

    fn participant(tag: u8) -> CantonId {
        format!("participant-{tag}::1220{}", format!("{tag:02x}").repeat(32))
            .parse()
            .expect("the participant id is valid")
    }

    fn expected() -> PartyToParticipant {
        PartyToParticipant {
            party: format!("party::1220{}", "aa".repeat(32)),
            threshold: 1,
            participants: vec![
                HostingParticipant {
                    participant_uid: participant(1).to_string(),
                    permission: 2,
                    onboarding: None,
                },
                HostingParticipant {
                    participant_uid: participant(2).to_string(),
                    permission: 2,
                    onboarding: Some(hosting_participant::Onboarding {}),
                },
            ],
            party_signing_keys: None,
        }
    }

    fn signed(mapping: PartyToParticipant, serial: u32, signer: &str) -> SignedTopologyTransaction {
        let transaction = TopologyTransaction {
            serial,
            operation: TopologyChangeOp::AddReplace as i32,
            mapping: Some(TopologyMapping {
                mapping: Some(topology_mapping::Mapping::PartyToParticipant(mapping)),
            }),
        };
        SignedTopologyTransaction {
            transaction: UntypedVersionedMessage {
                version: 30,
                wrapper: Some(untyped_versioned_message::Wrapper::Data(
                    transaction.encode_to_vec(),
                )),
            }
            .encode_to_vec(),
            signatures: vec![Signature {
                signed_by: signer.to_string(),
                signature: vec![1; 64],
                ..Default::default()
            }],
            proposal: true,
            multi_transaction_signatures: vec![],
        }
    }

    #[test]
    fn relay_validates_the_entire_pinned_mapping_and_serial() -> Result<()> {
        let mapping = expected();
        let valid = signed(mapping.clone(), 5, "joiner");
        assert!(exact_mapping(&valid, &mapping, 5).is_ok());
        assert!(exact_mapping(&valid, &mapping, 6).is_err());
        let mut altered = mapping.clone();
        altered.threshold = 2;
        assert!(exact_mapping(&signed(altered, 5, "joiner"), &mapping, 5).is_err());
        let mut altered = mapping.clone();
        altered.participants[1].onboarding = None;
        assert!(exact_mapping(&signed(altered, 5, "joiner"), &mapping, 5).is_err());
        let mut altered = topology_version::decode_transaction(&valid.transaction)?;
        altered.operation = TopologyChangeOp::Remove as i32;
        let mut wrong_operation = valid;
        wrong_operation.transaction = UntypedVersionedMessage {
            version: 30,
            wrapper: Some(untyped_versioned_message::Wrapper::Data(
                altered.encode_to_vec(),
            )),
        }
        .encode_to_vec();
        assert!(exact_mapping(&wrong_operation, &mapping, 5).is_err());
        Ok(())
    }

    #[test]
    fn relay_merges_only_byte_identical_transactions_and_deduplicates_keys() -> Result<()> {
        let first = signed(expected(), 5, "source");
        let second = signed(expected(), 5, "joiner-a");
        let third = signed(expected(), 5, "joiner-b");
        let merged = merge_proposals(vec![first.clone(), second.clone(), first.clone(), third])?;
        assert_eq!(merged.signatures.len(), 3);
        assert_eq!(merged.transaction, first.transaction);
        let mut different_bytes = second;
        different_bytes
            .transaction
            .extend_from_slice(&[0x98, 0x06, 0x01]);
        assert!(merge_proposals(vec![first, different_bytes]).is_err());
        assert!(merge_proposals(vec![]).is_err());
        Ok(())
    }

    #[test]
    fn relay_refuses_unsigned_or_unsupported_signed_envelopes() {
        let mut proposal = signed(expected(), 5, "source");
        assert!(decode_signed(&encode_signed(&proposal)).is_ok());
        proposal.signatures.clear();
        assert!(decode_signed(&encode_signed(&proposal)).is_err());
        let unsupported = UntypedVersionedMessage {
            version: 31,
            wrapper: Some(untyped_versioned_message::Wrapper::Data(
                proposal.encode_to_vec(),
            )),
        }
        .encode_to_vec();
        assert!(decode_signed(&unsupported).is_err());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_participant_has_one_durable_window_across_parties(db: SqlitePool) -> Result<()> {
        let first = replication_target(&expected().party, &participant(2), 4)?;
        let second_party = format!("other::1220{}", "bb".repeat(32));
        let second = replication_target(&second_party, &participant(2), 8)?;
        assert!(claim_import_window(&db, &first).await?);
        assert!(claim_import_window(&db, &first).await?);
        assert!(!claim_import_window(&db, &second).await?);
        release_import_window_owner(&db, &second).await?;
        assert_eq!(
            import_window_owner(&db, &participant(2)).await?,
            Some(first.instance_name.clone())
        );
        release_import_window_owner(&db, &first).await?;
        assert!(
            first
                .read_artifact(&db, artifact_kinds::TENANT_ADD_HOSTS_IMPORT_COMPLETED, None)
                .await?
                .is_some()
        );
        assert!(claim_import_window(&db, &second).await?);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn completed_stage_retries_never_open_another_window(db: SqlitePool) -> Result<()> {
        let party = expected().party;
        let host = participant(2);
        let target = replication_target(&party, &host, 4)?;
        let staged = Staged {
            party_id: party.clone(),
            new_hosts: vec![host.to_string()],
            permission: HostPermission::Confirmation,
            base_serial: 4,
            signed_transactions: vec![encode_signed(&signed(expected(), 5, "joiner"))],
        };
        target
            .write_artifact(
                &db,
                artifact_kinds::TENANT_ADD_HOSTS_STAGED_SIGNATURES,
                None,
                &serde_json::to_vec(&staged)?,
            )
            .await?;
        assert!(claim_import_window(&db, &target).await?);
        release_import_window_owner(&db, &target).await?;
        let mut config = NodeConfig::default();
        config.node.participant_id = Some(host.clone());
        let result = stage_add_hosts(
            &config,
            &db,
            &party,
            slice::from_ref(&host),
            HostPermission::Confirmation,
            4,
        )
        .await;
        assert!(matches!(result, Err(AddHostsError::Invalid(_))));
        assert!(import_window_owner(&db, &host).await?.is_none());
        Ok(())
    }
}

#[cfg(test)]
#[path = "add_hosts_relay_audit_tests.rs"]
mod audit_tests;
