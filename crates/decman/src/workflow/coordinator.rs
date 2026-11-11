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
    server::{WorkflowInstance, peer_status::LastSeen},
    utils,
    workflow::{
        AddPartyConfig, ChangeThresholdConfig, ContractsConfig, DarsConfig, KickConfig,
        OnboardingConfig, add_party, change_threshold, contracts, dars, kick, onboarding,
    },
};

/// Workflow types that can be run
#[derive(Clone, Copy, Debug)]
pub enum WorkflowType {
    Onboarding,
    Contracts,
    Dars,
    Kick,
    AddParty,
    ChangeThreshold,
}

/// Result from a coordinator workflow, optionally containing the created party ID
pub struct CoordinatorResult {
    /// The created dec party ID (only set for onboarding workflows)
    pub created_party_id: Option<CantonId>,
}

/// Start a coordinator workflow (called when this node initiates the workflow from UI)
#[allow(clippy::too_many_arguments)]
pub async fn start_coordinator(
    node_config: NodeConfig,
    db: SqlitePool,
    workflow_type: WorkflowType,
    onboarding_config: Option<OnboardingConfig>,
    kick_config: Option<KickConfig>,
    contracts_config: Option<ContractsConfig>,
    dars_config: Option<DarsConfig>,
    add_party_config: Option<AddPartyConfig>,
    change_threshold_config: Option<ChangeThresholdConfig>,
    workflow_auth: Option<WorkflowAuth>,
    last_seen: LastSeen,
    instance: Arc<WorkflowInstance>,
) -> Result<CoordinatorResult> {
    tracing::info!("Loading peers from database...");
    let network_config = NetworkConfig::from_peers(db.get_all_peers().await?);

    tracing::info!("Starting {workflow_type:?} workflow as COORDINATOR");

    match workflow_type {
        WorkflowType::Onboarding => {
            let config = onboarding_config.ok_or_else(|| {
                anyhow::anyhow!("OnboardingConfig is required for Onboarding workflow")
            })?;
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
        WorkflowType::Contracts => {
            let config = contracts_config.ok_or_else(|| {
                anyhow::anyhow!("ContractsConfig is required for Contracts workflow")
            })?;
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
        WorkflowType::Dars => {
            let config = dars_config
                .ok_or_else(|| anyhow::anyhow!("DarsConfig is required for Dars workflow"))?;
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
        WorkflowType::Kick => {
            let config = kick_config
                .ok_or_else(|| anyhow::anyhow!("KickConfig is required for Kick workflow"))?;
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
        WorkflowType::AddParty => {
            let config = add_party_config.ok_or_else(|| {
                anyhow::anyhow!("AddPartyConfig is required for AddParty workflow")
            })?;
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
        WorkflowType::ChangeThreshold => {
            let config = change_threshold_config.ok_or_else(|| {
                anyhow::anyhow!("ChangeThresholdConfig is required for ChangeThreshold workflow")
            })?;
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
