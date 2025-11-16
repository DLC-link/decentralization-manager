use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use tokio::sync::RwLock;

use crate::noise::MessageType;

/// Workflow steps in the coordinator-driven process
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkflowStep {
    /// Waiting for all attestors to connect
    WaitingForAttestors,
    /// Step 1: Upload DARs
    UploadDars,
    /// Step 1: Generate keys
    GenerateKeys,
    /// Coordinator creates proposals (no attestor action needed)
    CreateProposals,
    /// Step 2: Sign DNS proposals
    SignDns,
    /// Coordinator submits DNS proposals
    SubmitDns,
    /// Step 3: Sign P2P and PTK proposals
    SignP2pPtk,
    /// Coordinator submits final proposals
    SubmitFinal,
    /// Coordinator prepares submissions
    PrepareSubmissions,
    /// Step 4: Sign submissions
    SignSubmissions,
    /// Coordinator executes submissions
    ExecuteSubmissions,
    /// Workflow complete
    Complete,
}

impl WorkflowStep {
    /// Get the command message type for this step (if attestors need to execute something)
    pub fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::UploadDars => Some(MessageType::UploadDars),
            Self::GenerateKeys => Some(MessageType::GenerateKeys),
            Self::SignDns => Some(MessageType::SignDns),
            Self::SignP2pPtk => Some(MessageType::SignP2pPtk),
            Self::SignSubmissions => Some(MessageType::SignSubmissions),
            Self::Complete => Some(MessageType::Disconnect),
            // These steps are coordinator-only
            Self::WaitingForAttestors
            | Self::CreateProposals
            | Self::SubmitDns
            | Self::SubmitFinal
            | Self::PrepareSubmissions
            | Self::ExecuteSubmissions => None,
        }
    }

    /// Get the next step after this one
    pub fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForAttestors => Some(Self::UploadDars),
            Self::UploadDars => Some(Self::GenerateKeys),
            Self::GenerateKeys => Some(Self::CreateProposals),
            Self::CreateProposals => Some(Self::SignDns),
            Self::SignDns => Some(Self::SubmitDns),
            Self::SubmitDns => Some(Self::SignP2pPtk),
            Self::SignP2pPtk => Some(Self::SubmitFinal),
            Self::SubmitFinal => Some(Self::PrepareSubmissions),
            Self::PrepareSubmissions => Some(Self::SignSubmissions),
            Self::SignSubmissions => Some(Self::ExecuteSubmissions),
            Self::ExecuteSubmissions => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    /// Check if this step requires attestor participation
    pub fn requires_attestors(&self) -> bool {
        matches!(
            self,
            Self::UploadDars
                | Self::GenerateKeys
                | Self::SignDns
                | Self::SignP2pPtk
                | Self::SignSubmissions
        )
    }
}

/// Track workflow state and attestor progress
pub struct WorkflowState {
    /// Current workflow step
    current_step: RwLock<WorkflowStep>,
    /// Expected attestor IDs
    expected_attestors: HashSet<String>,
    /// Attestors that have connected
    connected_attestors: RwLock<HashSet<String>>,
    /// Attestors that have completed the current step
    completed_attestors: RwLock<HashSet<String>>,
    /// Data received from attestors (e.g., keys, signatures)
    attestor_data: RwLock<HashMap<String, Vec<u8>>>,
}

impl WorkflowState {
    /// Create a new workflow state
    pub fn new(expected_attestors: Vec<String>) -> Arc<Self> {
        Arc::new(Self {
            current_step: RwLock::new(WorkflowStep::WaitingForAttestors),
            expected_attestors: expected_attestors.into_iter().collect(),
            connected_attestors: RwLock::new(HashSet::new()),
            completed_attestors: RwLock::new(HashSet::new()),
            attestor_data: RwLock::new(HashMap::new()),
        })
    }

    /// Mark an attestor as connected
    pub async fn attestor_connected(&self, attestor_id: String) {
        let mut connected = self.connected_attestors.write().await;

        // Only log and process if this is the first connection
        let is_new = connected.insert(attestor_id.clone());
        if !is_new {
            // Already connected, nothing to do
            return;
        }

        let connected_count = connected.len();
        let total_count = self.expected_attestors.len();
        tracing::info!("Attestor connected: {attestor_id} ({connected_count}/{total_count})");

        // If all attestors connected and we're still waiting, move to first step
        if connected_count == total_count {
            let current = self.current_step.read().await;
            if *current == WorkflowStep::WaitingForAttestors {
                drop(current);
                self.advance_step().await;
            }
        }
    }

    /// Get the current step
    pub async fn current_step(&self) -> WorkflowStep {
        *self.current_step.read().await
    }

    /// Get the command for the current step (if any)
    pub async fn current_command(&self) -> Option<MessageType> {
        let step = self.current_step.read().await;
        step.to_command()
    }

    /// Mark an attestor as having completed the current step
    pub async fn attestor_completed(&self, attestor_id: String) {
        let mut completed = self.completed_attestors.write().await;
        completed.insert(attestor_id.clone());

        let current = self.current_step.read().await;
        let completed_count = completed.len();
        let total_count = self.expected_attestors.len();
        let step_name = format!("{current:?}");
        tracing::info!(
            "Attestor completed step {step_name}: {attestor_id} ({completed_count}/{total_count})"
        );

        // If all attestors completed and step requires attestors, move to next
        if current.requires_attestors() && completed_count == total_count {
            drop(current);
            drop(completed);
            self.advance_step().await;
        }
    }

    /// Store data received from an attestor
    pub async fn store_attestor_data(&self, attestor_id: String, data: Vec<u8>) {
        let mut attestor_data = self.attestor_data.write().await;
        attestor_data.insert(attestor_id, data);
    }

    /// Get all attestor data for the current step
    pub async fn get_all_attestor_data(&self) -> HashMap<String, Vec<u8>> {
        self.attestor_data.read().await.clone()
    }

    /// Clear attestor data (after processing)
    pub async fn clear_attestor_data(&self) {
        let mut attestor_data = self.attestor_data.write().await;
        attestor_data.clear();
    }

    /// Advance to the next workflow step
    pub async fn advance_step(&self) {
        let mut current = self.current_step.write().await;
        let mut completed = self.completed_attestors.write().await;

        if let Some(next_step) = current.next() {
            let current_name = format!("{current:?}");
            let next_name = format!("{next_step:?}");
            tracing::info!("Advancing workflow: {current_name} -> {next_name}");
            *current = next_step;
            completed.clear();
        } else {
            tracing::info!("Workflow complete!");
        }
    }

    /// Check if all attestors have connected
    pub async fn all_attestors_connected(&self) -> bool {
        let connected = self.connected_attestors.read().await;
        connected.len() == self.expected_attestors.len()
    }

    /// Check if a specific attestor has completed the current step
    pub async fn has_attestor_completed(&self, attestor_id: &str) -> bool {
        let completed = self.completed_attestors.read().await;
        completed.contains(attestor_id)
    }
}
