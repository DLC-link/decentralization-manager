//! Runs, party ids and an admin request shared by the workflow handler tests.

use crate::{
    canton_id::CantonId,
    server::types::{WorkflowKind, WorkflowProgress, WorkflowRole, WorkflowRun},
};

use crate::auth::Principal;

/// A minimal coordinator run row for `party`, in progress.
pub fn in_progress_run(instance: &str, party: &CantonId) -> WorkflowRun {
    WorkflowRun {
        instance_name: instance.to_string(),
        kind: WorkflowKind::Contracts,
        role: WorkflowRole::Coordinator,
        status: WorkflowProgress::InProgress,
        current_step: "SubmitProposals".to_string(),
        step_index: 0,
        step_total: 1,
        config_json: "{}".to_string(),
        coordinator_pubkey: None,
        coordinator_instance: None,
        coordinator_name: None,
        expected_peers: Vec::new(),
        completed_peers: Vec::new(),
        connected_peers: Vec::new(),
        acs_progress: None,
        dec_party_id: Some(party.clone()),
        prefix: None,
        participants: Vec::new(),
        previous_threshold: None,
        new_threshold: None,
        kicked_participant: None,
        kicked_owner_key: None,
        added_participant: None,
        package_names: Vec::new(),
        dar_filenames: Vec::new(),
        error: None,
        dismissed: false,
        created_at: 0,
        updated_at: 0,
    }
}

pub fn pid(tag: u8) -> anyhow::Result<CantonId> {
    let ns = format!("1220{:0>64}", format!("{tag:02x}"));
    CantonId::parse(&format!("validator-{tag}::{ns}"))
}

pub fn test_cid(prefix: &str) -> anyhow::Result<CantonId> {
    let ns = format!("1220{:0>64}", "a");
    CantonId::parse(&format!("{prefix}::{ns}"))
}

pub fn admin() -> Principal {
    Principal {
        sub: "admin".to_string(),
        issuer: "test".to_string(),
        roles: Vec::new(),
        email: None,
    }
}
