use sqlx::SqlitePool;

use super::types::{StartClaim, WorkflowKind, WorkflowRegistry};
use crate::{canton_id::CantonId, db::schema::SchemaRead, error::Result};

#[derive(Debug, thiserror::Error)]
pub(super) enum PartyStartRefusal {
    #[error("Another request is starting a workflow for {party}; retry once it has started")]
    Starting { party: CantonId },
    #[error(
        "Party {party} already has a {kind} workflow in flight (run {run}); wait for it \
         to finish or cancel it from its coordinator"
    )]
    InFlight {
        party: CantonId,
        run: String,
        kind: WorkflowKind,
    },
    #[error("Cannot check running workflows for {party}; retry later")]
    Unavailable {
        party: CantonId,
        #[source]
        cause: anyhow::Error,
    },
}

/// Reserve a party until its start request registers an active run or returns.
/// Party-targeted starts, accepts, and retries use this gate before changing run state.
pub(super) async fn claim_party_for_workflow(
    registry: &WorkflowRegistry,
    db: &SqlitePool,
    party: &CantonId,
) -> Result<StartClaim, PartyStartRefusal> {
    let claim = registry
        .claim_start(party)
        .ok_or_else(|| PartyStartRefusal::Starting {
            party: party.clone(),
        })?;
    let existing = find_inprogress_run_for_party(db, party)
        .await
        .map_err(|cause| PartyStartRefusal::Unavailable {
            party: party.clone(),
            cause,
        })?;
    if let Some((run, kind)) = existing {
        return Err(PartyStartRefusal::InFlight {
            party: party.clone(),
            run,
            kind,
        });
    }
    Ok(claim)
}

/// Only local active rows exclude a party; cross-node conflicts remain Canton's domain.
pub(super) async fn find_inprogress_run_for_party(
    db: &SqlitePool,
    party: &CantonId,
) -> Result<Option<(String, WorkflowKind)>> {
    Ok(db
        .get_in_progress_workflow_runs()
        .await?
        .into_iter()
        .find(|run| run.dec_party_id.as_ref() == Some(party))
        .map(|run| (run.instance_name, run.kind)))
}
