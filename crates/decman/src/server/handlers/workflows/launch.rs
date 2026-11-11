//! Starting a coordinator run: the request that names its workflow and
//! carries that workflow's config.

use std::sync::Arc;

use sqlx::SqlitePool;

use crate::{
    auth::WorkflowAuth,
    config::NodeConfig,
    error::Result,
    server::{
        peer_status::LastSeen,
        types::{WorkflowInstance, WorkflowKind},
    },
    workflow::{
        self, AddPartyConfig, ChangeThresholdConfig, ContractsConfig, CoordinatorResult,
        DarsConfig, KickConfig, OnboardingConfig, WorkflowType,
    },
};

/// The workflow a coordinator run drives, with its config. Each variant
/// holds the config of its kind, so a start cannot pass the wrong one.
pub(crate) enum CoordinatorRequest {
    Onboarding(OnboardingConfig),
    Kick(KickConfig),
    Contracts(ContractsConfig),
    Dars(DarsConfig),
    AddParty(AddPartyConfig),
    ChangeThreshold(ChangeThresholdConfig),
}

impl CoordinatorRequest {
    /// Read a run's persisted `config_json` back as the request of `kind`.
    /// The row stores the bare config, as the start handlers wrote it.
    pub(crate) fn from_persisted(
        kind: WorkflowKind,
        config_json: &str,
    ) -> serde_json::Result<Self> {
        Ok(match kind {
            WorkflowKind::Onboarding => Self::Onboarding(serde_json::from_str(config_json)?),
            WorkflowKind::Kick => Self::Kick(serde_json::from_str(config_json)?),
            WorkflowKind::Contracts => Self::Contracts(serde_json::from_str(config_json)?),
            WorkflowKind::Dars => Self::Dars(serde_json::from_str(config_json)?),
            WorkflowKind::AddParty => Self::AddParty(serde_json::from_str(config_json)?),
            WorkflowKind::ChangeThreshold => {
                Self::ChangeThreshold(serde_json::from_str(config_json)?)
            }
        })
    }

    /// Drive the coordinator for this request to its end.
    ///
    /// [`workflow::start_coordinator`] still takes the workflow type and six
    /// optional configs. This is the one place that fills them in.
    pub(crate) async fn start(
        self,
        node_config: NodeConfig,
        db: SqlitePool,
        workflow_auth: Option<WorkflowAuth>,
        last_seen: LastSeen,
        instance: Arc<WorkflowInstance>,
    ) -> Result<CoordinatorResult> {
        let (workflow_type, onboarding, kick, contracts, dars, add_party, change_threshold) =
            match self {
                Self::Onboarding(c) => (
                    WorkflowType::Onboarding,
                    Some(c),
                    None,
                    None,
                    None,
                    None,
                    None,
                ),
                Self::Kick(c) => (WorkflowType::Kick, None, Some(c), None, None, None, None),
                Self::Contracts(c) => (
                    WorkflowType::Contracts,
                    None,
                    None,
                    Some(c),
                    None,
                    None,
                    None,
                ),
                Self::Dars(c) => (WorkflowType::Dars, None, None, None, Some(c), None, None),
                Self::AddParty(c) => (
                    WorkflowType::AddParty,
                    None,
                    None,
                    None,
                    None,
                    Some(c),
                    None,
                ),
                Self::ChangeThreshold(c) => (
                    WorkflowType::ChangeThreshold,
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(c),
                ),
            };
        workflow::start_coordinator(
            node_config,
            db,
            workflow_type,
            onboarding,
            kick,
            contracts,
            dars,
            add_party,
            change_threshold,
            workflow_auth,
            last_seen,
            instance,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

    use super::*;

    fn persisted_again(request: &CoordinatorRequest) -> serde_json::Result<String> {
        fn json<T: Serialize>(config: &T) -> serde_json::Result<String> {
            serde_json::to_string(config)
        }
        match request {
            CoordinatorRequest::Onboarding(c) => json(c),
            CoordinatorRequest::Kick(c) => json(c),
            CoordinatorRequest::Contracts(c) => json(c),
            CoordinatorRequest::Dars(c) => json(c),
            CoordinatorRequest::AddParty(c) => json(c),
            CoordinatorRequest::ChangeThreshold(c) => json(c),
        }
    }

    fn kind_of(request: &CoordinatorRequest) -> WorkflowKind {
        match request {
            CoordinatorRequest::Onboarding(_) => WorkflowKind::Onboarding,
            CoordinatorRequest::Kick(_) => WorkflowKind::Kick,
            CoordinatorRequest::Contracts(_) => WorkflowKind::Contracts,
            CoordinatorRequest::Dars(_) => WorkflowKind::Dars,
            CoordinatorRequest::AddParty(_) => WorkflowKind::AddParty,
            CoordinatorRequest::ChangeThreshold(_) => WorkflowKind::ChangeThreshold,
        }
    }

    /// A run row stores the bare config of its kind. Reading it back gives
    /// the request of that kind, which writes the same JSON again.
    #[test]
    fn a_persisted_config_round_trips_through_its_request() -> anyhow::Result<()> {
        let party = crate::canton_id::CantonId::parse(&format!("dec::1220{}", "a".repeat(64)))?;
        let peer = crate::canton_id::CantonId::parse(&format!("p2::1220{}", "b".repeat(64)))?;
        let configs = [
            (
                WorkflowKind::Onboarding,
                serde_json::to_string(&OnboardingConfig::new(
                    "newparty".to_string(),
                    "newparty-creation".to_string(),
                    Some(2),
                    None,
                ))?,
            ),
            (
                WorkflowKind::Kick,
                serde_json::to_string(&KickConfig::new(
                    party.clone(),
                    Some(peer.clone()),
                    "k2".to_string(),
                    1,
                    None,
                    2,
                    "dec-kick-1".to_string(),
                ))?,
            ),
            (
                WorkflowKind::Contracts,
                serde_json::to_string(&ContractsConfig::new(
                    party.clone(),
                    vec![peer.clone()],
                    vec![peer.clone()],
                    party.clone(),
                    Vec::new(),
                    "dec-contracts-1".to_string(),
                ))?,
            ),
            (
                WorkflowKind::Dars,
                serde_json::to_string(&DarsConfig {
                    dar_files: Vec::new(),
                    instance_name: "dars-distribute-1".to_string(),
                    peer_ids: vec![peer.clone()],
                })?,
            ),
            (
                WorkflowKind::AddParty,
                serde_json::to_string(&AddPartyConfig::new(
                    party.clone(),
                    peer.clone(),
                    2,
                    None,
                    1,
                    "dec-add-party-1".to_string(),
                ))?,
            ),
            (
                WorkflowKind::ChangeThreshold,
                serde_json::to_string(&ChangeThresholdConfig::new(
                    party,
                    2,
                    Some(1),
                    1,
                    "dec-change-threshold-1".to_string(),
                ))?,
            ),
        ];
        for (kind, json) in configs {
            let request = CoordinatorRequest::from_persisted(kind, &json)?;
            assert_eq!(kind_of(&request), kind);
            assert_eq!(persisted_again(&request)?, json, "{kind:?}");
        }
        Ok(())
    }

    /// A row whose config does not fit its kind is an error, as it was when
    /// the resume parsed each kind by hand.
    #[test]
    fn a_config_of_another_kind_is_an_error() -> anyhow::Result<()> {
        let dars = serde_json::to_string(&DarsConfig {
            dar_files: Vec::new(),
            instance_name: "dars-distribute-1".to_string(),
            peer_ids: Vec::new(),
        })?;
        assert!(CoordinatorRequest::from_persisted(WorkflowKind::Kick, &dars).is_err());
        Ok(())
    }
}
