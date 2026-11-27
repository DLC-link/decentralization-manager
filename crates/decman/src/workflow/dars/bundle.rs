//! The DAR bytes of a distribution run, kept in the run's `DARS_BUNDLE`
//! artefact.

use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    db::schema::{Commitable, SchemaRead, SchemaWrite},
    error::Result,
    server::{WorkflowProgress, WorkflowRun},
    utils,
    workflow::{
        contracts::DarFile,
        storage::{WorkflowStorage, artifact_kinds},
    },
};

use super::{DarListing, DarsConfig};

/// Encode DAR files as the `UploadDars` payload, sorted by name so that every
/// peer receives the same bytes. No files make an empty payload.
pub fn bundle_of(mut files: Vec<(String, Vec<u8>)>) -> Vec<u8> {
    if files.is_empty() {
        return Vec::new();
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    utils::encode_files(&files)
}

/// Read the DARs that run `instance_name` distributes.
///
/// The start handler checks the sizes of the set and of each DAR, and each
/// peer checks them again, so this read does not.
///
/// # Errors
///
/// When the run has no bundle.
pub async fn load_bundle(db: &SqlitePool, instance_name: &str) -> Result<Vec<u8>> {
    db.read_artifact(instance_name, artifact_kinds::DARS_BUNDLE, None)
        .await?
        .with_context(|| format!("run {instance_name} has no DARs stored"))
}

/// The persisted config of a DAR run from a release that kept the DAR bytes
/// in `config_json`.
#[derive(Deserialize)]
struct InlineDarsConfig {
    dar_files: Vec<DarFile>,
    instance_name: String,
    peer_ids: Vec<CantonId>,
}

/// Move the DAR bytes of older runs out of `config_json` into a bundle.
///
/// Every query that lists runs reads `config_json`, so bytes left there are
/// loaded on each health check and feed refresh. A run that can still send
/// DARs gets a bundle: one in progress, or a failed one the operator can
/// retry. Each row is moved in its own transaction, and a row that fails to
/// move is logged and left as it was. Returns how many rows moved.
///
/// # Errors
///
/// Only when the runs to move cannot be listed.
pub async fn move_inline_dars(db: &SqlitePool) -> Result<usize> {
    let mut moved = 0;
    for instance_name in db.get_dars_runs_with_inline_bytes().await? {
        match move_inline_dars_of(db, &instance_name).await {
            Ok(true) => moved += 1,
            Ok(false) => {}
            Err(e) => tracing::warn!(
                "Could not move the DARs of run {instance_name} out of its config: {e:#}"
            ),
        }
    }
    Ok(moved)
}

async fn move_inline_dars_of(db: &SqlitePool, instance_name: &str) -> Result<bool> {
    let Some(run) = db.get_workflow_run(instance_name).await? else {
        return Ok(false);
    };
    // A config without the bytes fails to parse here, and has nothing to move.
    let Ok(inline) = serde_json::from_str::<InlineDarsConfig>(&run.config_json) else {
        return Ok(false);
    };
    let files = inline
        .dar_files
        .into_iter()
        .map(|file| {
            let data = STANDARD
                .decode(&file.data)
                .with_context(|| format!("DAR {} does not decode", file.filename))?;
            Ok((file.filename, data))
        })
        .collect::<Result<Vec<_>>>()?;
    let config = DarsConfig {
        dar_files: files
            .iter()
            .map(|(filename, data)| DarListing::of(filename, data))
            .collect(),
        instance_name: inline.instance_name,
        peer_ids: inline.peer_ids,
    };

    let mut tx = db.begin_transaction().await?;
    if can_still_send(&run) {
        tx.write_workflow_artifact(
            instance_name,
            artifact_kinds::DARS_BUNDLE,
            None,
            &bundle_of(files),
        )
        .await?;
    }
    tx.set_workflow_run_config(instance_name, &serde_json::to_string(&config)?)
        .await?;
    Commitable::commit(tx).await?;
    Ok(true)
}

/// Whether the run may still send its DARs. The artefacts of every other run
/// are already deleted, so a bundle for it would only take space.
fn can_still_send(run: &WorkflowRun) -> bool {
    match run.status {
        WorkflowProgress::InProgress => true,
        WorkflowProgress::Failed => !run.dismissed,
        WorkflowProgress::Idle | WorkflowProgress::Completed | WorkflowProgress::Cancelled => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::MIGRATOR, noise::MAX_CHUNKED_TOTAL_SIZE};

    /// Insert a coordinator DAR run the way an older release wrote it: the DAR
    /// bytes inline in `config_json`.
    async fn insert_inline_run(
        db: &SqlitePool,
        instance: &str,
        status: &str,
        files: &[(&str, &[u8])],
    ) -> Result {
        let config = serde_json::json!({
            "dar_files": files
                .iter()
                .map(|(filename, data)| serde_json::json!({
                    "filename": filename,
                    "data": STANDARD.encode(data),
                }))
                .collect::<Vec<_>>(),
            "instance_name": instance,
            "peer_ids": [],
        });
        sqlx::query(
            "INSERT INTO workflow_runs
             (instance_name, kind, role, status, current_step, step_index, step_total,
              config_json, expected_peers_json, completed_peers_json, created_at, updated_at)
             VALUES (?, 'Dars', 'Coordinator', ?, 'WaitingForPeers', 0, 3, ?, '[]', '[]', 5, 7)",
        )
        .bind(instance)
        .bind(status)
        .bind(config.to_string())
        .execute(db)
        .await?;
        Ok(())
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_older_run_in_progress_moves_its_dars_into_a_bundle(db: SqlitePool) -> Result {
        insert_inline_run(
            &db,
            "dars-distribute-1",
            "inprogress",
            &[("b.dar", b"bee"), ("a.dar", b"ay")],
        )
        .await?;

        assert_eq!(move_inline_dars(&db).await?, 1);

        let run = db
            .get_workflow_run("dars-distribute-1")
            .await?
            .context("the run has no row")?;
        assert!(!run.config_json.contains("\"data\""), "{}", run.config_json);
        let config: DarsConfig = serde_json::from_str(&run.config_json)?;
        assert_eq!(
            config.dar_files,
            vec![
                DarListing::of("b.dar", b"bee"),
                DarListing::of("a.dar", b"ay")
            ]
        );
        assert_eq!(
            load_bundle(&db, "dars-distribute-1").await?,
            bundle_of(vec![
                ("b.dar".to_string(), b"bee".to_vec()),
                ("a.dar".to_string(), b"ay".to_vec()),
            ])
        );
        // The wait for peers counts from `updated_at`, so the move keeps it.
        assert_eq!(run.updated_at, 7);
        assert_eq!(move_inline_dars(&db).await?, 0);
        Ok(())
    }

    /// An ended run sends nothing more, so it loses the bytes and gets no
    /// bundle.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_older_ended_run_drops_its_dars(db: SqlitePool) -> Result {
        insert_inline_run(&db, "dars-distribute-1", "cancelled", &[("a.dar", b"ay")]).await?;

        assert_eq!(move_inline_dars(&db).await?, 1);

        let run = db
            .get_workflow_run("dars-distribute-1")
            .await?
            .context("the run has no row")?;
        assert!(!run.config_json.contains("\"data\""), "{}", run.config_json);
        assert!(
            db.read_artifact("dars-distribute-1", artifact_kinds::DARS_BUNDLE, None)
                .await?
                .is_none()
        );
        Ok(())
    }

    /// A peer that lists `per-dar-pull` reads each DAR on its own, so a run
    /// may hold more than one chunked transfer carries.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_bundle_above_the_chunked_limit_loads(db: SqlitePool) -> Result {
        let len = MAX_CHUNKED_TOTAL_SIZE + 1;
        insert_inline_run(&db, "above-limit", "inprogress", &[]).await?;
        db.write_artifact(
            "above-limit",
            artifact_kinds::DARS_BUNDLE,
            None,
            &vec![0; len],
        )
        .await?;

        assert_eq!(load_bundle(&db, "above-limit").await?.len(), len);
        Ok(())
    }
}
