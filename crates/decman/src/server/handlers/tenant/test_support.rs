//! Helpers shared by the tenant tests.

use crate::{
    db::MIGRATOR, server::AppState, workflow::external_party::add_hosts::replication_target,
};

/// A full namespace fingerprint — `CantonId` deserializes through
/// `Namespace::from_hex`, which refuses a truncated stub.
pub fn test_participant() -> String {
    format!("participant-3::1220{}", "bb".repeat(32))
}

#[cfg(test)]
pub async fn own_test_import(state: &AppState, party: &str) {
    MIGRATOR
        .run(&state.db)
        .await
        .expect("the test import uses the real schema");
    let target = replication_target(party, state.config.participant_id(), 1)
        .expect("the test party identifies a replication");
    sqlx::query("INSERT INTO tenant_import_window (participant_id, instance_name, party_id) VALUES (?, ?, ?)")
        .bind(state.config.participant_id().to_string())
        .bind(target.instance_name)
        .bind(party)
        .execute(&state.db).await.expect("the admitted test import owns its window");
}
