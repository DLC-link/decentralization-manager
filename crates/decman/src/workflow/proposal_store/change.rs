//! Steps the create step of kick, change-threshold and add-party share: read
//! the namespace the export step saved, adopt the hosts' legacy signing keys,
//! and save the signed proposals.

use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::SigningPublicKey,
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToParticipant, SignedTopologyTransaction,
    },
};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{
        signing_keys::adopt_legacy_signing_keys, storage::WorkflowStorage,
        topology::DnsP2pArtifactKinds,
    },
};

/// The signed DNS and P2P proposals of a party change, and the namespace
/// definition its DNS proposes.
pub struct SignedPartyChange {
    pub dns: SignedTopologyTransaction,
    pub p2p: SignedTopologyTransaction,
    pub new_namespace_def: DecentralizedNamespaceDefinition,
}

/// Read the namespace definition the export step saved as `kind`.
///
/// # Errors
/// Returns an error naming `kind` in capitals when the artefact is missing,
/// or an error when it does not decode.
pub async fn read_exported_namespace(
    storage: &SqlitePool,
    instance_name: &str,
    kind: &str,
) -> Result<DecentralizedNamespaceDefinition> {
    let namespace_bytes = storage
        .read_artifact(instance_name, kind, None)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{name} artifact missing — did ExportState run?",
                name = kind.to_ascii_uppercase()
            )
        })?;
    utils::read_first_message_from_bytes(&namespace_bytes)
}

/// The party's signing keys for its current hosts.
///
/// A party with fewer inline keys than hosts keeps the rest in a legacy
/// `PartyToKeyMapping`, so the hosts' keys are adopted inline. Otherwise the
/// inline keys stay as they are.
///
/// # Errors
/// Returns an error if the legacy keys cannot be read or adopted.
pub async fn hosts_signing_keys(
    config: &NodeConfig,
    storage: &SqlitePool,
    synchronizer_id: &str,
    party_id: &CantonId,
    current: &PartyToParticipant,
) -> Result<Vec<SigningPublicKey>> {
    let current_signing_keys = current
        .party_signing_keys
        .clone()
        .map(|sk| sk.keys)
        .unwrap_or_default();
    if current_signing_keys.len() < current.participants.len() {
        let members: Vec<String> = current
            .participants
            .iter()
            .map(|p| p.participant_uid.clone())
            .collect();
        adopt_legacy_signing_keys(
            config,
            storage,
            synchronizer_id,
            party_id,
            &members,
            &current_signing_keys,
        )
        .await
    } else {
        Ok(current_signing_keys)
    }
}

/// Save `change` as the `kinds.dns_proposal`, `kinds.p2p_proposal` and
/// `new_namespace_kind` artefacts, each as a `varint(len)||proto` blob so
/// `read_first_message_from_bytes` reads it back. `label` names the change in
/// a log line after each write; `None` writes without logging.
///
/// # Errors
/// Returns an error if a write fails.
pub async fn save_party_proposals(
    storage: &SqlitePool,
    instance_name: &str,
    kinds: DnsP2pArtifactKinds<'_>,
    new_namespace_kind: &str,
    change: &SignedPartyChange,
    label: Option<&str>,
) -> Result {
    storage
        .write_artifact(
            instance_name,
            kinds.dns_proposal,
            None,
            &utils::encode_length_prefixed_message(&change.dns),
        )
        .await?;
    if let Some(label) = label {
        tracing::info!("Saved DNS {label} proposal to storage");
    }

    storage
        .write_artifact(
            instance_name,
            kinds.p2p_proposal,
            None,
            &utils::encode_length_prefixed_message(&change.p2p),
        )
        .await?;
    if let Some(label) = label {
        tracing::info!("Saved P2P {label} proposal to storage");
    }

    storage
        .write_artifact(
            instance_name,
            new_namespace_kind,
            None,
            &utils::encode_length_prefixed_message(&change.new_namespace_def),
        )
        .await?;
    if label.is_some() {
        tracing::info!("Saved new namespace definition to storage");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::TopologyTransaction;
    use prost::Message as _;

    use super::*;
    use crate::{
        db::MIGRATOR,
        workflow::{kick, storage::artifact_kinds},
    };

    const RUN: &str = "run-1";

    /// `workflow_artifacts.instance_name` is a foreign key onto
    /// `workflow_runs`, so an artefact needs its run row first.
    async fn insert_run(pool: &SqlitePool) -> Result {
        sqlx::query(
            "INSERT INTO workflow_runs (
                 instance_name, kind, role, status, current_step, step_index, step_total,
                 config_json, expected_peers_json, completed_peers_json,
                 created_at, updated_at
             ) VALUES (?1, 'Kick', 'Coordinator', 'inprogress', 'CreateProposals',
                       0, 1, '{}', '[]', '[]', 0, 0)",
        )
        .bind(RUN)
        .execute(pool)
        .await?;
        Ok(())
    }

    fn namespace(threshold: i32) -> DecentralizedNamespaceDefinition {
        DecentralizedNamespaceDefinition {
            decentralized_namespace: "1220ns".to_string(),
            threshold,
            owners: vec!["a".to_string(), "b".to_string()],
        }
    }

    fn signed(serial: u32) -> SignedTopologyTransaction {
        SignedTopologyTransaction {
            transaction: TopologyTransaction {
                serial,
                ..Default::default()
            }
            .encode_to_vec(),
            ..Default::default()
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_missing_export_names_the_kind_in_capitals(pool: SqlitePool) -> Result {
        insert_run(&pool).await?;

        let Err(e) =
            read_exported_namespace(&pool, RUN, artifact_kinds::CHANGE_THRESHOLD_NAMESPACE_DEF)
                .await
        else {
            panic!("nothing was exported");
        };
        assert_eq!(
            e.to_string(),
            "CHANGE_THRESHOLD_NAMESPACE_DEF artifact missing — did ExportState run?"
        );
        Ok(())
    }

    /// What the create step saves is what submit and the next run read back.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn saved_proposals_read_back_as_written(pool: SqlitePool) -> Result {
        insert_run(&pool).await?;
        let change = SignedPartyChange {
            dns: signed(3),
            p2p: signed(4),
            new_namespace_def: namespace(2),
        };

        save_party_proposals(
            &pool,
            RUN,
            kick::ARTIFACTS,
            artifact_kinds::KICK_NEW_NAMESPACE_DEF,
            &change,
            Some("kick"),
        )
        .await?;

        let read = |kind| pool.read_artifact(RUN, kind, None);
        let dns: SignedTopologyTransaction = utils::read_first_message_from_bytes(
            &read(artifact_kinds::KICK_DNS_PROPOSAL)
                .await?
                .unwrap_or_default(),
        )?;
        let p2p: SignedTopologyTransaction = utils::read_first_message_from_bytes(
            &read(artifact_kinds::KICK_P2P_PROPOSAL)
                .await?
                .unwrap_or_default(),
        )?;
        let new_namespace_def: DecentralizedNamespaceDefinition =
            utils::read_first_message_from_bytes(
                &read(artifact_kinds::KICK_NEW_NAMESPACE_DEF)
                    .await?
                    .unwrap_or_default(),
            )?;
        assert_eq!(dns, change.dns);
        assert_eq!(p2p, change.p2p);
        assert_eq!(new_namespace_def, change.new_namespace_def);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_exported_namespace_reads_back(pool: SqlitePool) -> Result {
        insert_run(&pool).await?;
        pool.write_artifact(
            RUN,
            artifact_kinds::ADD_PARTY_NAMESPACE_DEF,
            None,
            &utils::encode_length_prefixed_message(&namespace(1)),
        )
        .await?;

        assert_eq!(
            read_exported_namespace(&pool, RUN, artifact_kinds::ADD_PARTY_NAMESPACE_DEF).await?,
            namespace(1)
        );
        Ok(())
    }
}
