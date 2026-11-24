use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::task::JoinHandle;

use crate::{
    auth::WorkflowAuth,
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig},
    db::schema::SchemaRead,
    error::Result,
    noise::server::ActiveWorkflow,
    server::{CoordinatorRequest, WorkflowInstance, peer_status::LastSeen},
    utils,
    workflow::{add_party, change_threshold, contracts, dars, kick, onboarding, rotate_key},
};

/// Result from a coordinator workflow, optionally containing the created party ID
pub struct CoordinatorResult {
    /// The created dec party ID (only set for onboarding workflows)
    pub created_party_id: Option<CantonId>,
}

/// The name the start log line gives the workflow of `request`.
fn workflow_name(request: &CoordinatorRequest) -> &'static str {
    match request {
        CoordinatorRequest::Onboarding(_) => "Onboarding",
        CoordinatorRequest::Contracts(_) => "Contracts",
        CoordinatorRequest::Dars(_) => "Dars",
        CoordinatorRequest::Kick(_) => "Kick",
        CoordinatorRequest::AddParty(_) => "AddParty",
        CoordinatorRequest::ChangeThreshold(_) => "ChangeThreshold",
        CoordinatorRequest::RotateKey(_) => "RotateKey",
    }
}

/// Start a coordinator workflow (called when this node initiates the workflow from UI)
///
/// # Errors
///
/// Returns an error if the peers cannot be read from the database. Returns an
/// error if the chosen workflow fails. Causes include an unreadable Noise
/// keypair, a failed step such as a Canton Admin API call, and peers that give
/// up or go silent.
pub(crate) async fn start_coordinator(
    node_config: NodeConfig,
    db: SqlitePool,
    request: CoordinatorRequest,
    workflow_auth: Option<WorkflowAuth>,
    last_seen: LastSeen,
    instance: Arc<WorkflowInstance>,
) -> Result<CoordinatorResult> {
    tracing::info!("Loading peers from database...");
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);

    let name = workflow_name(&request);
    tracing::info!("Starting {name} workflow as COORDINATOR");

    match request {
        CoordinatorRequest::Onboarding(config) => {
            let party_id = onboarding::coordinator::start_coordinator(
                node_config,
                network_config,
                config,
                db,
                last_seen,
                instance,
            )
            .await?;
            Ok(CoordinatorResult {
                created_party_id: Some(party_id),
            })
        }
        CoordinatorRequest::Contracts(config) => {
            contracts::coordinator::start_coordinator(
                node_config,
                network_config,
                config,
                workflow_auth,
                db,
                last_seen,
                instance,
            )
            .await?;
            Ok(CoordinatorResult {
                created_party_id: None,
            })
        }
        CoordinatorRequest::Dars(config) => {
            dars::coordinator::start_coordinator(
                node_config,
                network_config,
                config,
                db,
                last_seen,
                instance,
            )
            .await?;
            Ok(CoordinatorResult {
                created_party_id: None,
            })
        }
        CoordinatorRequest::Kick(config) => {
            kick::coordinator::start_coordinator(
                node_config,
                network_config,
                config,
                db,
                last_seen,
                instance,
            )
            .await?;
            Ok(CoordinatorResult {
                created_party_id: None,
            })
        }
        CoordinatorRequest::AddParty(config) => {
            add_party::coordinator::start_coordinator(
                node_config,
                network_config,
                config,
                workflow_auth,
                db,
                last_seen,
                instance,
            )
            .await?;
            Ok(CoordinatorResult {
                created_party_id: None,
            })
        }
        CoordinatorRequest::ChangeThreshold(config) => {
            change_threshold::coordinator::start_coordinator(
                node_config,
                network_config,
                config,
                db,
                last_seen,
                instance,
            )
            .await?;
            Ok(CoordinatorResult {
                created_party_id: None,
            })
        }
        CoordinatorRequest::RotateKey(config) => {
            rotate_key::coordinator::start_coordinator(
                node_config,
                network_config,
                config,
                db,
                last_seen,
                instance,
            )
            .await?;
            Ok(CoordinatorResult {
                created_party_id: None,
            })
        }
    }
}

/// Register the coordinator's Noise handle on its [`WorkflowInstance`] so the
/// always-on listener can route this run's peer command messages to it, then
/// drive the workflow loop to completion. No port is bound here — the single
/// always-on listener already owns port 9000. The instance's registry entry is
/// removed by the caller's [`WorkflowGuard`](crate::server::WorkflowGuard) on
/// return (success, error, or panic), so a finished or failed workflow stops
/// receiving routed commands.
///
/// The run fails, naming the peers and why, once peers it still waits on have
/// given up or gone silent and the rest cannot finish it.
///
/// # Errors
///
/// Returns an error if the peers the run still waits on give up or go silent
/// and the rest cannot finish it. Returns an error if the workflow task returns
/// an error, panics, or is cancelled.
pub async fn run_workflow_with_handler(
    active: ActiveWorkflow,
    instance: Arc<WorkflowInstance>,
    mut workflow_handle: JoinHandle<Result>,
) -> Result {
    instance.set_active(active.clone());
    let outcome = tokio::select! {
        outcome = &mut workflow_handle => outcome,
        reason = active.wait_until_stranded() => {
            workflow_handle.abort();
            utils::forget_synchronizer_id_on_unknown_store(&reason.to_string());
            tracing::error!("Workflow failed: {reason}");
            anyhow::bail!("{reason}");
        }
    };
    match outcome {
        Ok(Ok(())) => {
            tracing::info!("Workflow completed successfully, shutting down");
            Ok(())
        }
        Ok(Err(e)) => {
            // A retried run then resolves the successor synchronizer.
            utils::forget_synchronizer_id_on_unknown_store(&format!("{e:#}"));
            tracing::error!("Workflow failed: {e:#}");
            anyhow::bail!("Coordinator workflow failed: {e:#}");
        }
        Err(e) => {
            tracing::error!("Workflow task panicked: {e}");
            anyhow::bail!("Coordinator workflow task panicked: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::{
        AddPartyConfig, ChangeThresholdConfig, ContractsConfig, DarsConfig, KickConfig,
        OnboardingConfig, RotateKeyConfig,
    };

    /// The start line names each workflow as the old `WorkflowType` debug
    /// name did.
    #[test]
    fn the_start_line_keeps_each_workflow_name() -> anyhow::Result<()> {
        let party = CantonId::parse(&format!("dec::1220{}", "a".repeat(64)))?;
        let run = || "run-1".to_string();
        let requests = [
            CoordinatorRequest::Onboarding(OnboardingConfig::new(
                "newparty".to_string(),
                run(),
                None,
                None,
            )),
            CoordinatorRequest::Contracts(ContractsConfig::new(
                party.clone(),
                Vec::new(),
                Vec::new(),
                party.clone(),
                Vec::new(),
                run(),
            )),
            CoordinatorRequest::Dars(DarsConfig {
                dar_files: Vec::new(),
                instance_name: run(),
                peer_ids: Vec::new(),
            }),
            CoordinatorRequest::Kick(KickConfig::new(
                party.clone(),
                None,
                "k2".to_string(),
                1,
                None,
                2,
                run(),
            )),
            CoordinatorRequest::AddParty(AddPartyConfig::new(
                party.clone(),
                party.clone(),
                2,
                None,
                1,
                run(),
            )),
            CoordinatorRequest::ChangeThreshold(ChangeThresholdConfig::new(
                party.clone(),
                2,
                None,
                1,
                run(),
            )),
            CoordinatorRequest::RotateKey(RotateKeyConfig {
                decentralized_party_id: party,
                rotated_key: format!("1220{}", "c".repeat(64)).parse()?,
                threshold: 2,
                confirmation_threshold: 1,
                instance_name: run(),
            }),
        ];
        let names: Vec<&str> = requests.iter().map(workflow_name).collect();
        assert_eq!(
            names,
            [
                "Onboarding",
                "Contracts",
                "Dars",
                "Kick",
                "AddParty",
                "ChangeThreshold",
                "RotateKey"
            ]
        );
        Ok(())
    }
}
