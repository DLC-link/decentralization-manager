use std::{future::Future, sync::Arc, time::Duration};

use sqlx::SqlitePool;
use tokio::task::JoinHandle;

use crate::{
    auth::WorkflowAuth,
    canton_id::CantonId,
    config::{NetworkConfig, NodeConfig},
    db::schema::SchemaRead,
    error::Result,
    noise::server::{ActiveWorkflow, NoiseServer},
    server::{CoordinatorRequest, WorkflowInstance, peer_status::LastSeen},
    utils,
    workflow::{
        add_party, change_threshold, contracts, dars, kick, onboarding,
        state::{WorkflowState, WorkflowStep},
    },
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

/// How long the coordinator loop sleeps before it reads the step again, while
/// the peers or the listener move the run on.
const IDLE: Duration = Duration::from_secs(1);

/// What the coordinator loop does after one pass over the current step.
pub(crate) enum StepFlow {
    /// Read the current step again at once.
    Continue,
    /// Sleep one second, then read the current step again.
    Idle,
    /// The run is complete. Sleep this long, so that the peers can still
    /// fetch their Disconnect command, then end the task.
    Finish(Duration),
}

/// One workflow's share of the coordinator loop: the work this node does at
/// each step.
pub(crate) trait CoordinatorSteps<S: WorkflowStep>: Send + 'static {
    /// Do this node's work for `step` and say what the loop does next.
    ///
    /// # Errors
    ///
    /// Returns the error of the step's work. It fails the run.
    fn run_step(
        &mut self,
        workflow_state: &WorkflowState<S>,
        step: S,
    ) -> impl Future<Output = Result<StepFlow>> + Send;
}

/// Run a coordinator: spawn the task that drives `server`'s workflow state
/// through its steps, and wait for the run to end through
/// [`run_workflow_with_handler`].
///
/// The task first awaits `steps`. That future does the work the workflow
/// needs before its first step, such as seeding the first command payload,
/// and yields the workflow's step handler. The task then reads the current
/// step and runs the handler on it, until the handler finishes the run.
///
/// # Errors
///
/// Returns an error if `steps` or a step fails, if the peers the run still
/// waits on give up or go silent, or if the task panics or is cancelled.
pub(crate) async fn run_coordinator<S, C>(
    server: NoiseServer<S>,
    active: fn(Arc<NoiseServer<S>>) -> ActiveWorkflow,
    instance: Arc<WorkflowInstance>,
    steps: impl Future<Output = Result<C>> + Send + 'static,
) -> Result
where
    S: WorkflowStep + 'static,
    C: CoordinatorSteps<S>,
{
    let server = Arc::new(server);
    let workflow_handle = tokio::spawn(drive_steps(server.get_workflow_state(), steps, IDLE));
    run_workflow_with_handler(active(server), instance, workflow_handle).await
}

/// The coordinator task: await `steps`, then run the handler on the current
/// step until it finishes the run. An idle pass sleeps for `idle`.
async fn drive_steps<S, C>(
    workflow_state: Arc<WorkflowState<S>>,
    steps: impl Future<Output = Result<C>>,
    idle: Duration,
) -> Result
where
    S: WorkflowStep + 'static,
    C: CoordinatorSteps<S>,
{
    let mut steps = steps.await?;
    loop {
        let current_step = workflow_state.current_step().await;
        match steps.run_step(&workflow_state, current_step).await? {
            StepFlow::Continue => {}
            StepFlow::Idle => tokio::time::sleep(idle).await,
            StepFlow::Finish(wait) => {
                tokio::time::sleep(wait).await;
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use super::*;
    use crate::workflow::KickStep;

    /// A step handler that answers each pass from a script and records the
    /// steps it was given.
    struct Scripted {
        flows: VecDeque<Result<StepFlow>>,
        seen: Arc<Mutex<Vec<KickStep>>>,
    }

    impl CoordinatorSteps<KickStep> for Scripted {
        async fn run_step(
            &mut self,
            _workflow_state: &WorkflowState<KickStep>,
            step: KickStep,
        ) -> Result<StepFlow> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(step);
            }
            self.flows
                .pop_front()
                .unwrap_or_else(|| Err(anyhow::anyhow!("the script ran out")))
        }
    }

    fn scripted(flows: Vec<Result<StepFlow>>) -> (Scripted, Arc<Mutex<Vec<KickStep>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let steps = Scripted {
            flows: flows.into(),
            seen: seen.clone(),
        };
        (steps, seen)
    }

    fn seen_steps(seen: &Mutex<Vec<KickStep>>) -> anyhow::Result<Vec<KickStep>> {
        seen.lock()
            .map(|seen| seen.clone())
            .map_err(|_| anyhow::anyhow!("the seen steps lock is poisoned"))
    }

    async fn state_at(step: KickStep) -> anyhow::Result<Arc<WorkflowState<KickStep>>> {
        let db = SqlitePool::connect("sqlite::memory:").await?;
        Ok(WorkflowState::new(
            db,
            "kick-1".to_string(),
            step,
            Vec::new(),
            None,
        ))
    }

    /// The loop hands the handler the current step on every pass and ends
    /// once the handler finishes the run.
    #[tokio::test]
    async fn the_loop_runs_the_current_step_until_the_run_finishes() -> anyhow::Result<()> {
        let (steps, seen) = scripted(vec![
            Ok(StepFlow::Continue),
            Ok(StepFlow::Idle),
            Ok(StepFlow::Finish(Duration::ZERO)),
            Ok(StepFlow::Continue),
        ]);
        let state = state_at(KickStep::SignProposals).await?;

        drive_steps(state, async { Ok(steps) }, Duration::ZERO).await?;

        assert_eq!(seen_steps(&seen)?, vec![KickStep::SignProposals; 3]);
        Ok(())
    }

    /// A failed step ends the loop with that step's error.
    #[tokio::test]
    async fn a_failed_step_ends_the_loop_with_its_error() -> anyhow::Result<()> {
        let (steps, seen) = scripted(vec![
            Ok(StepFlow::Continue),
            Err(anyhow::anyhow!("submit failed")),
            Ok(StepFlow::Finish(Duration::ZERO)),
        ]);
        let state = state_at(KickStep::SubmitKick).await?;

        let result = drive_steps(state, async { Ok(steps) }, Duration::ZERO).await;

        let error = result
            .err()
            .ok_or_else(|| anyhow::anyhow!("the loop succeeded"))?;
        assert_eq!(error.to_string(), "submit failed");
        assert_eq!(seen_steps(&seen)?.len(), 2);
        Ok(())
    }

    /// A failed set-up ends the task with its error.
    #[tokio::test]
    async fn a_failed_setup_ends_the_task_with_its_error() -> anyhow::Result<()> {
        let state = state_at(KickStep::ExportState).await?;
        let steps = async { Err::<Scripted, _>(anyhow::anyhow!("no auth")) };

        let result = drive_steps(state, steps, Duration::ZERO).await;

        let error = result
            .err()
            .ok_or_else(|| anyhow::anyhow!("the loop succeeded"))?;
        assert_eq!(error.to_string(), "no auth");
        Ok(())
    }
}
