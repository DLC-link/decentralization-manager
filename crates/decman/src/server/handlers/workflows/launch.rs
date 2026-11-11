//! Starting a coordinator run: the request that names its workflow and
//! carries that workflow's config, and the task that drives it.

use std::{sync::Arc, time::Duration};

use actix_web::web;
use futures::future::BoxFuture;
use sqlx::SqlitePool;

use crate::{
    auth::WorkflowAuth,
    config::NodeConfig,
    error::Result,
    server::{
        AppState,
        peer_status::LastSeen,
        types::{
            WorkflowGuard, WorkflowInstance, WorkflowKind, WorkflowProgress, WorkflowRegistry,
        },
    },
    workflow::{
        self, AddPartyConfig, ChangeThresholdConfig, ContractsConfig, CoordinatorResult,
        DarsConfig, KickConfig, OnboardingConfig, WorkflowType,
    },
};

use super::runs::{mark_run_completed, mark_run_failed};

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

/// The order in which a coordinator task writes a run's end status: the
/// in-memory status, the log line and the run row. Each caller keeps the
/// order it had before the launch was shared.
#[derive(Clone, Copy, Debug)]
pub(crate) enum StatusOrder {
    /// In memory, then the log line, then the run row. Kick, onboarding and
    /// a resumed run.
    MemoryFirst,
    /// The run row, then in memory, then the log line. Contracts, add-party
    /// and change-threshold.
    RowFirst,
    /// As [`RowFirst`](Self::RowFirst), except that a failed invite holds
    /// the status locks while it writes the run row. DARs.
    RowFirstHoldingLocksOnInviteFailure,
}

/// How a coordinator task begins.
pub(crate) enum TaskStart {
    /// A new run: send its invites, then give the peers two seconds to start
    /// their own runs.
    Invite(BoxFuture<'static, Result>),
    /// A run resumed after a restart: no invites, and the log lines name the
    /// run.
    Resume,
}

/// What a coordinator task uses of the node.
pub(crate) struct NodeHandles {
    pub(crate) config: NodeConfig,
    pub(crate) db: SqlitePool,
    pub(crate) workflows: WorkflowRegistry,
    pub(crate) last_seen: LastSeen,
}

impl NodeHandles {
    fn of(data: &web::Data<AppState>) -> Self {
        Self {
            config: data.config.clone(),
            db: data.db.clone(),
            workflows: data.workflows.clone(),
            last_seen: data.last_seen.clone(),
        }
    }
}

/// The name a new run's log lines give its workflow.
fn workflow_title(kind: WorkflowKind) -> &'static str {
    match kind {
        WorkflowKind::Onboarding => "Onboarding",
        WorkflowKind::Kick => "Kick",
        WorkflowKind::Contracts => "Contracts",
        WorkflowKind::Dars => "DARs distribution",
        WorkflowKind::AddParty => "Add-party",
        WorkflowKind::ChangeThreshold => "Change-threshold",
    }
}

/// The name a failed invite's log line gives the invites.
fn invite_label(kind: WorkflowKind) -> &'static str {
    match kind {
        WorkflowKind::Onboarding => "onboarding",
        WorkflowKind::Kick => "kick",
        WorkflowKind::Contracts => "contracts",
        WorkflowKind::Dars => "DARs",
        WorkflowKind::AddParty => "add-party",
        WorkflowKind::ChangeThreshold => "change-threshold",
    }
}

/// Spawn the task that drives `instance`'s coordinator run: optionally send
/// the invites, run `request`, and write the end status in `order`. The
/// run leaves the registry when the task ends, however it ends.
/// `after_success` runs last, once a run that completed has written its
/// status.
pub(crate) fn spawn_coordinator_task(
    node: NodeHandles,
    instance: Arc<WorkflowInstance>,
    request: CoordinatorRequest,
    auth: Option<WorkflowAuth>,
    start: TaskStart,
    order: StatusOrder,
    after_success: Option<Box<dyn FnOnce() + Send>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let NodeHandles {
            config,
            db,
            workflows,
            last_seen,
        } = node;
        let instance_name = instance.instance_name.clone();
        let kind = instance.kind;
        let state = instance.http.clone();
        // Removes this run from the registry on return (success/failure/abort).
        let _workflow_guard = WorkflowGuard::new(workflows, instance_name.clone());

        let resumed = match start {
            TaskStart::Invite(invites) => {
                // Send the invites before starting the coordinator workflow
                if let Err(e) = invites.await {
                    tracing::error!("Failed to send {} invites: {e}", invite_label(kind));
                    let msg = format!("Failed to send invites: {e}");
                    match order {
                        StatusOrder::RowFirstHoldingLocksOnInviteFailure => {
                            let mut status = state.status.write().await;
                            let mut error = state.error.write().await;
                            *status = WorkflowProgress::Failed;
                            *error = Some(msg.clone());
                            mark_run_failed(&db, kind, &instance_name, &msg).await;
                        }
                        StatusOrder::MemoryFirst | StatusOrder::RowFirst => {
                            {
                                let mut status = state.status.write().await;
                                let mut error = state.error.write().await;
                                *status = WorkflowProgress::Failed;
                                *error = Some(msg.clone());
                            }
                            mark_run_failed(&db, kind, &instance_name, &msg).await;
                        }
                    }
                    return;
                }

                // Give peers time to start their peer workflows
                tokio::time::sleep(Duration::from_secs(2)).await;
                false
            }
            TaskStart::Resume => true,
        };

        let result = request
            .start(config, db.clone(), auth, last_seen, instance.clone())
            .await;

        // Update in-memory state in tight scopes — never hold the RwLock
        // across a DB await. The status endpoints take a read lock to serve
        // every poll; a writer holding it during the DB write would block
        // each concurrent read for that long on a slow runner.
        //
        // With `RowFirst` the run row is marked terminal BEFORE the in-memory
        // status flips. The two are read by different callers — `/…/status`
        // reads this lock, while the "already has a workflow in flight" guard
        // scans the run rows — so flipping memory first opens a window where
        // the API reports a run finished and refuses the next workflow for
        // that party in the same breath. Doing the durable write first can
        // only report a finished run as still running for an instant, which
        // costs a poll rather than a rejected request.
        match result {
            Ok(_) => {
                let log_completed = || {
                    if resumed {
                        tracing::info!("{kind:?} workflow {instance_name} completed");
                    } else {
                        tracing::info!("{} workflow completed successfully", workflow_title(kind));
                    }
                };
                match order {
                    StatusOrder::MemoryFirst => {
                        *state.status.write().await = WorkflowProgress::Completed;
                        log_completed();
                        mark_run_completed(&db, kind, &instance_name).await;
                    }
                    StatusOrder::RowFirst | StatusOrder::RowFirstHoldingLocksOnInviteFailure => {
                        mark_run_completed(&db, kind, &instance_name).await;
                        *state.status.write().await = WorkflowProgress::Completed;
                        log_completed();
                    }
                }
                if let Some(after_success) = after_success {
                    after_success();
                }
            }
            Err(e) => {
                let msg = format!("{e}");
                let log_failed = || {
                    if resumed {
                        tracing::error!("{kind:?} workflow {instance_name} failed: {e:#}");
                    } else {
                        tracing::error!("{} workflow failed: {e}", workflow_title(kind));
                    }
                };
                let write_failed = || async {
                    let mut status = state.status.write().await;
                    let mut error = state.error.write().await;
                    *status = WorkflowProgress::Failed;
                    *error = Some(msg.clone());
                };
                match order {
                    StatusOrder::MemoryFirst => {
                        write_failed().await;
                        log_failed();
                        mark_run_failed(&db, kind, &instance_name, &msg).await;
                    }
                    StatusOrder::RowFirst | StatusOrder::RowFirstHoldingLocksOnInviteFailure => {
                        mark_run_failed(&db, kind, &instance_name, &msg).await;
                        write_failed().await;
                        log_failed();
                    }
                }
            }
        }
    })
}

/// Start a new coordinator run from a start handler: spawn its task and
/// mark it in progress.
///
/// The abort handle, status and error locks are taken BEFORE the spawn and
/// flipped together. Held across the spawn, a concurrent cancel either runs
/// entirely before (status not InProgress, so 409 "no workflow in
/// progress") or entirely after (status InProgress and an abort handle, so
/// the cancel proceeds). Without this, a cancel landing between the status
/// flip and the abort handle would see "InProgress + None" and bail with a
/// spurious "still initializing" 409, which leaves the status pinned to
/// InProgress. Holding the status lock across the spawn also blocks the
/// task's terminal status write until the InProgress flip lands, so a
/// fast-failing task cannot publish its end before the start does.
pub(crate) async fn launch_coordinator(
    data: &web::Data<AppState>,
    instance: &Arc<WorkflowInstance>,
    request: CoordinatorRequest,
    auth: Option<WorkflowAuth>,
    invites: BoxFuture<'static, Result>,
    order: StatusOrder,
    after_success: Option<Box<dyn FnOnce() + Send>>,
) {
    let node = NodeHandles::of(data);
    let state = &instance.http;
    let mut abort_guard = state.abort_handle.lock().await;
    let mut status_guard = state.status.write().await;
    let mut error_guard = state.error.write().await;

    let join_handle = spawn_coordinator_task(
        node,
        instance.clone(),
        request,
        auth,
        TaskStart::Invite(invites),
        order,
        after_success,
    );
    *abort_guard = Some(join_handle.abort_handle());
    *status_guard = WorkflowProgress::InProgress;
    *error_guard = None;
    drop(error_guard);
    drop(status_guard);
    drop(abort_guard);
}

#[cfg(test)]
mod launch_tests {
    use crate::{
        db::{MIGRATOR, schema::SchemaRead},
        server::types::WorkflowRole,
    };

    use super::super::test_support::{in_progress_run, insert_run, pid, test_node};
    use super::*;

    type TestResult = anyhow::Result<()>;

    const RUN: &str = "dars-distribute-1";

    /// A DARs run that this node registered and persisted, as a start
    /// handler leaves it before the launch.
    async fn registered_run(
        pool: &SqlitePool,
        data: &web::Data<AppState>,
    ) -> anyhow::Result<Arc<WorkflowInstance>> {
        let mut run = in_progress_run(RUN, &pid(9)?);
        run.kind = WorkflowKind::Dars;
        insert_run(pool, &run).await?;
        let instance = WorkflowInstance::new(
            RUN.to_string(),
            WorkflowKind::Dars,
            WorkflowRole::Coordinator,
        );
        assert!(data.workflows.insert(instance.clone()));
        Ok(instance)
    }

    fn request() -> CoordinatorRequest {
        CoordinatorRequest::Dars(DarsConfig {
            dar_files: Vec::new(),
            instance_name: RUN.to_string(),
            peer_ids: Vec::new(),
        })
    }

    async fn task_ended(data: &web::Data<AppState>) -> TestResult {
        for _ in 0..200 {
            if data.workflows.get(RUN).is_none() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        anyhow::bail!("the task did not end")
    }

    /// The launch returns with the run in progress and abortable, before the
    /// task can write its end; the task's end then replaces it.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn the_launch_marks_the_run_in_progress_first(pool: SqlitePool) -> TestResult {
        let node = test_node(pool.clone(), &pid(1)?).await?;
        let instance = registered_run(&pool, &node.data).await?;
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let invites = Box::pin(async move {
            let _ = released.await;
            Err(anyhow::anyhow!("no route to the peers"))
        });

        launch_coordinator(
            &node.data,
            &instance,
            request(),
            None,
            invites,
            StatusOrder::MemoryFirst,
            None,
        )
        .await;

        assert_eq!(
            *instance.http.status.read().await,
            WorkflowProgress::InProgress
        );
        assert_eq!(*instance.http.error.read().await, None);
        assert!(instance.http.abort_handle.lock().await.is_some());
        assert!(node.data.workflows.get(RUN).is_some());

        let _ = release.send(());
        task_ended(&node.data).await?;
        assert_eq!(*instance.http.status.read().await, WorkflowProgress::Failed);
        Ok(())
    }

    /// A failed invite fails the run in memory and in its row with the same
    /// text, and the run leaves the registry.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_failed_invite_fails_the_run(pool: SqlitePool) -> TestResult {
        let node = test_node(pool.clone(), &pid(1)?).await?;
        let instance = registered_run(&pool, &node.data).await?;

        launch_coordinator(
            &node.data,
            &instance,
            request(),
            None,
            Box::pin(async { Err(anyhow::anyhow!("no route to the peers")) }),
            StatusOrder::MemoryFirst,
            None,
        )
        .await;
        task_ended(&node.data).await?;

        let message = "Failed to send invites: no route to the peers";
        assert_eq!(*instance.http.status.read().await, WorkflowProgress::Failed);
        assert_eq!(instance.http.error.read().await.as_deref(), Some(message));
        let row = pool
            .get_workflow_run(RUN)
            .await?
            .ok_or_else(|| anyhow::anyhow!("the run row is gone"))?;
        assert_eq!(row.status, WorkflowProgress::Failed);
        assert_eq!(row.error.as_deref(), Some(message));
        Ok(())
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
