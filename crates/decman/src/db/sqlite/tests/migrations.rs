//! The migration checksum repair.

use sqlx::SqlitePool;

use crate::{
    db::sqlite::connect::{MIGRATION_CHECKSUM_REPAIRS, MIGRATOR, repair_migration_checksums},
    error::Result,
};

#[sqlx::test(migrator = "MIGRATOR")]
async fn repair_migration_checksums_unblocks_pre_edit_database(pool: SqlitePool) -> Result {
    let (version, old_checksum, _) = MIGRATION_CHECKSUM_REPAIRS[0];

    // Simulate a database whose migration was applied from the pre-edit
    // file: record the old checksum, as operator databases have it.
    sqlx::query(&format!(
        "UPDATE _sqlx_migrations SET checksum = X'{old_checksum}' WHERE version = ?"
    ))
    .bind(version)
    .execute(&pool)
    .await?;

    // Without the repair, the migrator refuses to start — this is the
    // operator-reported crash.
    let err = MIGRATOR
        .run(&pool)
        .await
        .expect_err("checksum must mismatch");
    assert!(
        err.to_string()
            .contains("previously applied but has been modified"),
        "unexpected error: {err}"
    );

    repair_migration_checksums(&pool).await?;
    MIGRATOR.run(&pool).await?;

    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn repair_migration_checksums_is_a_noop_on_current_database(pool: SqlitePool) -> Result {
    let (version, _, new_checksum) = MIGRATION_CHECKSUM_REPAIRS[0];

    repair_migration_checksums(&pool).await?;

    let recorded: (String,) =
        sqlx::query_as("SELECT lower(hex(checksum)) FROM _sqlx_migrations WHERE version = ?")
            .bind(version)
            .fetch_one(&pool)
            .await?;
    assert_eq!(recorded.0, new_checksum);
    MIGRATOR.run(&pool).await?;

    Ok(())
}

#[sqlx::test]
async fn repair_migration_checksums_tolerates_missing_table(pool: SqlitePool) -> Result {
    // A database that has never run migrations has no `_sqlx_migrations`
    // table; the repair must not fail on it.
    repair_migration_checksums(&pool).await?;
    Ok(())
}
