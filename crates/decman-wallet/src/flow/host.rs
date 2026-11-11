//! The hosts a wallet drives and what each one reports.

use common::canton_id::CantonId;
use common::types::WorkflowProgress;
use serde::Serialize;

use crate::client::{HostStatus, TenantClient};
use crate::error::Error;

/// One participant that will host the party, and the DecMan instance in front of
/// it. A wallet configures these once — they are the hosting set it chose.
#[derive(Clone)]
pub struct WalletHost {
    pub client: TenantClient,
    /// This host's Canton participant id.
    pub participant_id: CantonId,
}

impl WalletHost {
    pub fn new(client: TenantClient, participant_id: CantonId) -> Self {
        Self {
            client,
            participant_id,
        }
    }
}

/// What one host reports about the party, or why it could not be reached.
#[derive(Clone, Debug, Serialize)]
pub struct HostReport {
    pub base_url: String,
    pub participant_id: String,
    /// `None` when the call to this host failed — see `error`.
    pub status: Option<HostStatus>,
    /// `None` when the call succeeded.
    pub error: Option<String>,
}

impl HostReport {
    pub(super) fn ok(host: &WalletHost, status: HostStatus) -> Self {
        Self {
            base_url: host.client.base_url().to_string(),
            participant_id: host.participant_id.to_string(),
            status: Some(status),
            error: None,
        }
    }

    pub(super) fn failed(host: &WalletHost, error: &Error) -> Self {
        Self {
            base_url: host.client.base_url().to_string(),
            participant_id: host.participant_id.to_string(),
            status: None,
            error: Some(error.to_string()),
        }
    }

    /// Whether this host has the party live.
    pub fn is_hosted(&self) -> bool {
        self.status.is_some_and(HostStatus::is_hosted)
    }
}

/// The outcome of driving onboarding across every host.
#[derive(Clone, Debug, Serialize)]
pub struct OnboardedParty {
    pub party_id: String,
    pub fingerprint: String,
    /// The party's public key, base64-encoded. Safe to publish — and the only
    /// half of the key any host ever sees.
    pub public_key: String,
    pub hosts: Vec<HostReport>,
}

impl OnboardedParty {
    /// Whether every host has authorized the party. Until this is true the
    /// topology is still a proposal somewhere and the party cannot transact.
    pub fn fully_hosted(&self) -> bool {
        !self.hosts.is_empty() && self.hosts.iter().all(HostReport::is_hosted)
    }
}

/// Map a host's own progress report onto the status vocabulary. A host answers
/// `Completed` once its authorized mapping names the party; anything else means
/// it is still working through the proposal.
pub(super) fn progress_to_status(progress: WorkflowProgress) -> HostStatus {
    if progress == WorkflowProgress::Completed {
        HostStatus::Hosted
    } else {
        HostStatus::Pending
    }
}

/// Ask every host where the party stands, one round. Authorization is not
/// instant — callers poll this until [`OnboardedParty::fully_hosted`]-equivalent
/// agreement, on whatever schedule suits them.
pub async fn statuses(hosts: &[WalletHost], party_id: &str) -> Vec<HostReport> {
    let mut reports = Vec::with_capacity(hosts.len());
    for host in hosts {
        let report = match host.client.host_status(party_id).await {
            Ok(status) => HostReport::ok(host, status),
            Err(e) => HostReport::failed(host, &e),
        };
        reports.push(report);
    }
    reports
}
