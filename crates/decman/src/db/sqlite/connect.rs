//! Opening the SQLite pool and repairing migration checksums.

use std::{path::Path, str::FromStr, time::Duration};

use sqlx::{
    SqlitePool,
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use crate::error::Result;

pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Checksum rewrites for migration files that were edited in place after
/// operators had already applied them: (version, checksum recorded by the old
/// file, checksum of the current file). Every entry must be a semantically
/// neutral edit (comments, whitespace) — an edit that changes what the
/// migration does needs a new migration, never an entry here.
///
/// Version 4: #239 changed "DAML" to "Daml" in a comment of
/// `000004_workflow_runs.up.sql` after the migration had shipped, so databases
/// initialized before that commit fail sqlx's checksum validation with
/// "migration 4 was previously applied but has been modified".
pub(super) const MIGRATION_CHECKSUM_REPAIRS: &[(i64, &str, &str)] = &[(
    4,
    "137f46dbb8207525f71a15371f122ac9a22bddf6dfed826ff1d8dc1dd46e5b80965e1b8e46a0b7943b4a67ca9ddc5b33",
    "eda7576bac7480932ea8b0a8e3d6ddddb65d045a3cec2ccff85a425ee8b89baf64dc5ca8e5482f5670198aafe350eb9e",
)];

/// Rewrite the recorded checksums of known post-ship-edited migrations to
/// match the files this binary embeds. Run this before `MIGRATOR.run`, which
/// otherwise refuses to start on a database that applied the pre-edit file.
///
/// # Errors
///
/// Returns an error if the `_sqlx_migrations` table cannot be read or updated.
pub async fn repair_migration_checksums(pool: &SqlitePool) -> Result<()> {
    // A fresh database has no `_sqlx_migrations` table yet — nothing to repair.
    let table_exists = sqlx::query(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?
    .is_some();
    if !table_exists {
        return Ok(());
    }

    for (version, old_checksum, new_checksum) in MIGRATION_CHECKSUM_REPAIRS {
        // The checksums are compile-time hex constants, so inlining the blob
        // literal is safe; version and the old checksum are bound.
        let result = sqlx::query(&format!(
            "UPDATE _sqlx_migrations SET checksum = X'{new_checksum}' \
             WHERE version = ? AND lower(hex(checksum)) = ?"
        ))
        .bind(version)
        .bind(old_checksum)
        .execute(pool)
        .await?;
        if result.rows_affected() > 0 {
            tracing::info!(
                "Repaired the recorded checksum of migration {version} \
                 (applied by an older build from a since-edited file)"
            );
        }
    }

    Ok(())
}

/// Create a new SQLite connection pool
///
/// # Errors
///
/// Returns an error if the database file cannot be created or opened
pub async fn connect(db_path: &Path) -> Result<SqlitePool> {
    // Ensure parent directory exists
    if let Some(parent) = db_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let db_url = format!("sqlite:{}", db_path.display());

    let options = SqliteConnectOptions::from_str(&db_url)?
        .create_if_missing(true)
        // Enforce foreign keys explicitly. The schema relies on
        // `ON DELETE CASCADE` (e.g. dec_party -> dec_party_owner/participant);
        // sqlx defaults this pragma on, but pinning it here means a future
        // driver/default change can't silently disable cascade enforcement.
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(30));

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    Ok(pool)
}
