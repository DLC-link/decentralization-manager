use canton_proto_rs::com::digitalasset::canton::protocol::v30::PartyToParticipant;
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    db::schema::SchemaRead,
    server::{WorkflowKind, WorkflowRun},
    utils,
    workflow::{kick::KickConfig, signing_keys::requires_signing_key_adoption, topology},
};

/// Peer quorum: `m - 1` (the coordinator signs itself), clamped to
/// `[1, expected_peers]` so a peer event can always trip it (`0` if no peers).
fn peers_for_party_threshold(m: usize, expected_peers: usize) -> usize {
    if expected_peers == 0 {
        return 0;
    }
    m.saturating_sub(1).clamp(1, expected_peers)
}

/// Peer quorum for a run (`WorkflowState::peer_threshold`). `None` = require
/// every expected peer — also the fallback when the threshold can't be resolved.
pub(super) async fn resolve_peer_threshold(
    kind: WorkflowKind,
    config: &NodeConfig,
    run: Option<&WorkflowRun>,
    db: &SqlitePool,
    expected_peers: usize,
) -> Option<usize> {
    if expected_peers == 0 {
        return None;
    }
    match kind {
        // These define/extend the owner set or need every member — require all.
        WorkflowKind::Onboarding
        | WorkflowKind::Dars
        | WorkflowKind::AddParty
        | WorkflowKind::ChangeThreshold
        | WorkflowKind::RotateKey => None,
        // Existing M-of-N party: a quorum is enough.
        WorkflowKind::Kick | WorkflowKind::Contracts => {
            let run = run?;
            let party_id = run.dec_party_id.as_ref()?;
            if kind == WorkflowKind::Kick {
                let mapping = kick_party_mapping(config, party_id).await;
                return kick_peer_threshold_from_mapping(db, run, expected_peers, mapping.as_ref())
                    .await;
            }
            let m = lookup_party_threshold(db, party_id).await?;
            Some(peers_for_party_threshold(m, expected_peers))
        }
    }
}

/// A kick signs the DNS under the current threshold, then signs the P2P
/// update under the new one. Missing or invalid saved config requires all
/// peers rather than allowing the signature gate to advance too soon.
async fn lookup_kick_peer_threshold(
    db: &SqlitePool,
    run: &WorkflowRun,
    expected_peers: usize,
) -> Option<usize> {
    let party_id = run.dec_party_id.as_ref()?;
    let current = lookup_party_threshold(db, party_id).await?;
    let config: KickConfig = serde_json::from_str(&run.config_json).ok()?;
    let proposed = usize::try_from(config.new_threshold)
        .ok()
        .filter(|t| *t > 0)?;
    Some(peers_for_party_threshold(
        current.max(proposed),
        expected_peers,
    ))
}

/// Fully and partially legacy parties need each added key to sign. Requiring
/// every invitee prevents an ordinary namespace quorum from skipping one.
async fn kick_peer_threshold_from_mapping(
    db: &SqlitePool,
    run: &WorkflowRun,
    expected_peers: usize,
    mapping: Option<&PartyToParticipant>,
) -> Option<usize> {
    let mapping = mapping?;
    if requires_signing_key_adoption(mapping) {
        tracing::info!(
            "This kick must move legacy signing keys inline; every invited peer must sign"
        );
        return None;
    }
    lookup_kick_peer_threshold(db, run, expected_peers).await
}

/// Read the P2P mapping for the kick's signature gate. An unavailable mapping
/// requires every invitee rather than risking an unsigned key adoption.
async fn kick_party_mapping(
    config: &NodeConfig,
    party_id: &CantonId,
) -> Option<PartyToParticipant> {
    let synchronizer_id = match utils::get_synchronizer_id(config).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("Cannot resolve the synchronizer id to classify {party_id}: {e:#}");
            return None;
        }
    };
    match topology::fetch_p2p_mapping(config, &synchronizer_id, party_id).await {
        Ok(mapping) => Some(mapping),
        Err(e) => {
            tracing::warn!("Cannot read the P2P mapping for {party_id}: {e:#}");
            None
        }
    }
}

/// Party signing threshold `M` from the `dec_parties` cache, or `None` (caller
/// requires all) when it's absent or invalid (`< 1`).
pub(crate) async fn lookup_party_threshold(db: &SqlitePool, party_id: &CantonId) -> Option<usize> {
    let party_id_str = party_id.to_string();
    let parties = match SchemaRead::get_dec_parties_by_prefix(db, &party_id.prefix).await {
        Ok(parties) => parties,
        Err(e) => {
            tracing::warn!(
                "Could not read dec party threshold for {party_id_str}: {e}; requiring all peers"
            );
            return None;
        }
    };
    let threshold = parties
        .into_iter()
        .find(|p| p.party_id == party_id_str)
        .map(|p| p.threshold)?;
    match usize::try_from(threshold) {
        Ok(m) if m >= 1 => Some(m),
        _ => {
            tracing::warn!(
                "Dec party {party_id_str} has invalid threshold {threshold}; requiring all peers"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::{SigningKeyUsage, SigningKeysWithThreshold, SigningPublicKey},
        protocol::v30::{enums::ParticipantPermission, party_to_participant::HostingParticipant},
    };

    use super::*;
    use crate::{
        canton_id::{NAMESPACE_LENGTH, Namespace},
        db::{
            MIGRATOR,
            rows::WorkflowRunRow,
            schema::{Commitable, SchemaWrite},
        },
        workflow::{WorkflowState, kick::KickStep, state::WorkflowStep},
    };

    fn kick_peer(tag: u8) -> CantonId {
        CantonId::new(
            format!("peer-{tag}"),
            Namespace::new([tag; NAMESPACE_LENGTH]),
        )
    }

    async fn saved_kick(
        pool: &SqlitePool,
        current: i64,
        proposed: i32,
    ) -> anyhow::Result<WorkflowRun> {
        let party = kick_peer(42);
        sqlx::query(
            "INSERT INTO dec_party (party_id, prefix, threshold, updated_at) VALUES (?, ?, ?, 0)",
        )
        .bind(party.to_string())
        .bind(&party.prefix)
        .bind(current)
        .execute(pool)
        .await?;
        let config = KickConfig::new(
            party.clone(),
            Some(kick_peer(9)),
            "owner-9".to_string(),
            proposed,
            None,
            0,
            "raised-threshold-kick".to_string(),
        );
        let run = WorkflowRunRow {
            instance_name: config.instance_name.clone(),
            kind: "Kick".to_string(),
            role: "Coordinator".to_string(),
            status: "inprogress".to_string(),
            current_step: "WaitingForPeers".to_string(),
            step_index: 0,
            step_total: KickStep::step_total(),
            config_json: serde_json::to_string(&config)?,
            coordinator_pubkey: None,
            coordinator_instance: None,
            expected_peers_json: serde_json::to_string(&vec![
                kick_peer(2),
                kick_peer(3),
                kick_peer(4),
            ])?,
            completed_peers_json: "[]".to_string(),
            dec_party_id: Some(party.to_string()),
            error: None,
            dismissed: 0,
            created_at: 0,
            updated_at: 0,
        }
        .into_domain()?;
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_workflow_run(&run).await?;
        Commitable::commit(tx).await?;
        pool.get_workflow_run(&config.instance_name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("saved kick missing"))
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_raised_kick_threshold_gates_connections_and_signatures(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let run = saved_kick(&pool, 2, 3).await?;
        // The display field is not stored; the quorum must read saved config.
        assert_eq!(run.new_threshold, None);
        let threshold = lookup_kick_peer_threshold(&pool, &run, 3).await;
        assert_eq!(threshold, Some(2));
        let state = WorkflowState::new(
            pool.clone(),
            run.instance_name.clone(),
            KickStep::WaitingForPeers,
            run.expected_peers,
            threshold,
        );

        state.peer_connected(kick_peer(2)).await;
        assert_eq!(state.current_step().await, KickStep::WaitingForPeers);
        state.peer_connected(kick_peer(3)).await;
        assert_eq!(state.current_step().await, KickStep::ExportState);
        state.advance_step().await;
        state.advance_step().await;
        assert_eq!(state.current_step().await, KickStep::SignProposals);

        state.peer_completed(kick_peer(2)).await;
        assert_eq!(state.current_step().await, KickStep::SignProposals);
        state.peer_completed(kick_peer(3)).await;
        assert_eq!(state.current_step().await, KickStep::SubmitKick);
        let saved = pool
            .get_workflow_run(&run.instance_name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("updated kick missing"))?;
        assert_eq!(saved.current_step, "SubmitKick");
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_lowered_kick_threshold_still_needs_the_current_quorum(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let mut run = saved_kick(&pool, 3, 2).await?;
        assert_eq!(lookup_kick_peer_threshold(&pool, &run, 3).await, Some(2));
        for invalid_config in ["{}", "not JSON"] {
            run.config_json = invalid_config.to_string();
            assert_eq!(lookup_kick_peer_threshold(&pool, &run, 3).await, None);
        }
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_kick_with_an_invalid_saved_threshold_requires_all_peers(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let mut run = saved_kick(&pool, 2, 0).await?;
        assert_eq!(lookup_kick_peer_threshold(&pool, &run, 3).await, None);
        let mut config: KickConfig = serde_json::from_str(&run.config_json)?;
        config.new_threshold = -1;
        run.config_json = serde_json::to_string(&config)?;
        assert_eq!(lookup_kick_peer_threshold(&pool, &run, 3).await, None);
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_partial_key_adoption_cannot_advance_at_just_namespace_quorum(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let run = saved_kick(&pool, 2, 2).await?;
        let mut mapping = PartyToParticipant {
            participants: [1, 2, 3, 4, 9]
                .into_iter()
                .map(|tag| HostingParticipant {
                    participant_uid: kick_peer(tag).to_string(),
                    permission: ParticipantPermission::Confirmation as i32,
                    ..Default::default()
                })
                .collect(),
            party_signing_keys: Some(SigningKeysWithThreshold {
                keys: vec![SigningPublicKey {
                    public_key: vec![1; 32],
                    usage: vec![SigningKeyUsage::Protocol as i32],
                    ..Default::default()
                }],
                threshold: 1,
            }),
            threshold: 2,
            ..Default::default()
        };
        let threshold = kick_peer_threshold_from_mapping(&pool, &run, 3, Some(&mapping)).await;
        assert_eq!(threshold, None);
        let state = WorkflowState::new(
            pool.clone(),
            run.instance_name.clone(),
            KickStep::SignProposals,
            run.expected_peers.clone(),
            threshold,
        );
        state.peer_completed(kick_peer(2)).await;
        assert_eq!(state.current_step().await, KickStep::SignProposals);
        state.peer_completed(kick_peer(3)).await;
        assert_eq!(state.current_step().await, KickStep::SignProposals);
        state.peer_completed(kick_peer(4)).await;
        assert_eq!(state.current_step().await, KickStep::SubmitKick);
        mapping.party_signing_keys = Some(SigningKeysWithThreshold {
            keys: (1..=5)
                .map(|seed| SigningPublicKey {
                    public_key: vec![seed; 32],
                    usage: vec![SigningKeyUsage::Protocol as i32],
                    ..Default::default()
                })
                .collect(),
            threshold: 2,
        });
        assert_eq!(
            kick_peer_threshold_from_mapping(&pool, &run, 3, Some(&mapping)).await,
            Some(1)
        );
        assert_eq!(
            kick_peer_threshold_from_mapping(&pool, &run, 3, None).await,
            None
        );
        Ok(())
    }

    #[test]
    fn party_threshold_subtracts_the_coordinators_own_signature() {
        assert_eq!(peers_for_party_threshold(2, 2), 1);
        assert_eq!(peers_for_party_threshold(3, 3), 2);
        assert_eq!(peers_for_party_threshold(4, 4), 3);
    }

    #[test]
    fn party_threshold_is_clamped_into_range() {
        assert_eq!(peers_for_party_threshold(1, 3), 1); // M=1 → 0 clamps up to 1
        assert_eq!(peers_for_party_threshold(9, 2), 2); // over-count clamps to all
        assert_eq!(peers_for_party_threshold(5, 0), 0); // no peers
    }
}
