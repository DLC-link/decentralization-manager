use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use super::NoiseServer;
use crate::{canton_id::CantonId, consts::PEER_SILENCE_LIMIT, workflow::state::WorkflowStep};

/// How often a coordinator checks whether its peers can still finish the run.
const PEER_CHECK_INTERVAL: Duration = Duration::from_secs(5);

impl<S: WorkflowStep + 'static> NoiseServer<S> {
    /// Resolve once this run can no longer finish, with the reason: the
    /// current step waits on peers that gave up or went silent, and the rest
    /// cannot reach its quorum.
    ///
    /// A step that waits on peers advances only when they report, so without
    /// this a peer that dies mid-step parks the run forever (#442).
    pub async fn wait_until_stranded(&self) -> String {
        let mut check = tokio::time::interval(PEER_CHECK_INTERVAL);
        loop {
            check.tick().await;
            if let Some(reason) = self.stranded_reason(Instant::now()).await {
                return reason;
            }
        }
    }

    /// Why this run can no longer finish as of `now`, or `None` while it can.
    pub(crate) async fn stranded_reason(&self, now: Instant) -> Option<String> {
        let unavailable = self.unavailable_peers(now).await;
        if unavailable.is_empty() {
            return None;
        }
        let (step, missing) = self.workflow_state.stranded_by(&unavailable).await?;
        let peers = missing
            .iter()
            .map(|(peer, why)| format!("peer {peer} {why}"))
            .collect::<Vec<_>>()
            .join("; ");
        Some(format!("{step} cannot complete: {peers}"))
    }

    /// The peers this run can no longer count on, with why: those that reported
    /// giving up, and those not heard from for [`PEER_SILENCE_LIMIT`] as of
    /// `now`. `last_seen` counts heartbeats as well as workflow traffic, so a
    /// peer busy in a long step stays available.
    async fn unavailable_peers(&self, now: Instant) -> HashMap<CantonId, String> {
        let mut unavailable = self.departed.read().await.clone();
        let last_seen = self.last_seen.read().await;
        for peer in self.workflow_state.expected_peers() {
            if unavailable.contains_key(peer) {
                continue;
            }
            let heard = last_seen
                .get(&peer.to_string())
                .map_or(self.serving_since, |seen| (*seen).max(self.serving_since));
            let silent = now.saturating_duration_since(heard);
            if silent >= PEER_SILENCE_LIMIT {
                unavailable.insert(
                    peer.clone(),
                    format!("has been unreachable for {}s", silent.as_secs()),
                );
            }
        }
        unavailable
    }
}
