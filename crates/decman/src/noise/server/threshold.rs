use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId, config::NodeConfig, db::schema::SchemaRead, server::WorkflowKind, utils,
    workflow::topology,
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
    dec_party_id: Option<&CantonId>,
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
        | WorkflowKind::ChangeThreshold => None,
        // Existing M-of-N party: a quorum is enough.
        WorkflowKind::Kick | WorkflowKind::Contracts => {
            let party_id = dec_party_id?;
            // A kick on a party whose keys are still in the deprecated
            // `PartyToKeyMapping` moves every survivor's key inline, and
            // Canton makes each added key sign the transaction that adds it.
            // A quorum would leave the proposal short of signatures, so that
            // kick needs every survivor, not a quorum.
            if kind == WorkflowKind::Kick && party_keys_are_legacy(config, party_id).await {
                tracing::info!(
                    "{party_id} keeps its signing keys in a legacy PartyToKeyMapping; this \
                     kick needs every surviving peer to sign, not a quorum"
                );
                return None;
            }
            let m = lookup_party_threshold(db, party_id).await?;
            Some(peers_for_party_threshold(m, expected_peers))
        }
    }
}

/// Whether a party still keeps its signing keys in the deprecated
/// `PartyToKeyMapping` instead of inline on its `PartyToParticipant`.
///
/// Answers `true` when the mapping cannot be read at all. The cost of being
/// wrong that way is a run that waits for every peer; the cost of the other
/// way is a run that advances without the signatures Canton demands.
async fn party_keys_are_legacy(config: &NodeConfig, party_id: &CantonId) -> bool {
    let synchronizer_id = match utils::get_synchronizer_id(config).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("Cannot resolve the synchronizer id to classify {party_id}: {e:#}");
            return true;
        }
    };
    match topology::fetch_p2p_mapping(config, &synchronizer_id, party_id).await {
        Ok(mapping) => mapping
            .party_signing_keys
            .is_none_or(|keys| keys.keys.is_empty()),
        Err(e) => {
            tracing::warn!("Cannot read the P2P mapping for {party_id}: {e:#}");
            true
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
    use super::*;

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
