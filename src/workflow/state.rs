use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use tokio::sync::RwLock;

use crate::noise::MessageType;

/// Trait for workflow steps
pub trait WorkflowStep:
    Copy + std::fmt::Debug + PartialEq + Eq + std::hash::Hash + Send + Sync
{
    fn to_command(&self) -> Option<MessageType>;
    fn next(&self) -> Option<Self>;
    fn requires_attestors(&self) -> bool;
    fn is_waiting_for_attestors(&self) -> bool;
}

/// Onboarding workflow steps (decentralized party creation)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OnboardingStep {
    /// Waiting for all attestors to connect
    WaitingForAttestors,
    /// Generate keys
    GenerateKeys,
    /// Coordinator creates proposals
    CreateProposals,
    /// Sign DNS proposals
    SignDns,
    /// Coordinator submits DNS proposals
    SubmitDns,
    /// Sign P2P proposals
    SignP2p,
    /// Coordinator submits final proposals
    SubmitFinal,
    /// Workflow complete
    Complete,
}

impl WorkflowStep for OnboardingStep {
    fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::GenerateKeys => Some(MessageType::GenerateKeys),
            Self::SignDns => Some(MessageType::SignDns),
            Self::SignP2p => Some(MessageType::SignP2p),
            Self::Complete => Some(MessageType::Disconnect),
            Self::WaitingForAttestors
            | Self::CreateProposals
            | Self::SubmitDns
            | Self::SubmitFinal => None,
        }
    }

    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForAttestors => Some(Self::GenerateKeys),
            Self::GenerateKeys => Some(Self::CreateProposals),
            Self::CreateProposals => Some(Self::SignDns),
            Self::SignDns => Some(Self::SubmitDns),
            Self::SubmitDns => Some(Self::SignP2p),
            Self::SignP2p => Some(Self::SubmitFinal),
            Self::SubmitFinal => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    fn requires_attestors(&self) -> bool {
        matches!(self, Self::GenerateKeys | Self::SignDns | Self::SignP2p)
    }

    fn is_waiting_for_attestors(&self) -> bool {
        matches!(self, Self::WaitingForAttestors)
    }
}

/// Contracts workflow steps (DAR upload and contract creation)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContractsStep {
    /// Waiting for all attestors to connect
    WaitingForAttestors,
    /// Upload DARs
    UploadDars,
    /// Coordinator prepares submissions
    PrepareSubmissions,
    /// Sign submissions
    SignSubmissions,
    /// Coordinator executes submissions
    ExecuteSubmissions,
    /// Workflow complete
    Complete,
}

impl WorkflowStep for ContractsStep {
    fn to_command(&self) -> Option<MessageType> {
        match self {
            Self::UploadDars => Some(MessageType::UploadDars),
            Self::SignSubmissions => Some(MessageType::SignSubmissions),
            Self::Complete => Some(MessageType::Disconnect),
            Self::WaitingForAttestors | Self::PrepareSubmissions | Self::ExecuteSubmissions => None,
        }
    }

    fn next(&self) -> Option<Self> {
        match self {
            Self::WaitingForAttestors => Some(Self::UploadDars),
            Self::UploadDars => Some(Self::PrepareSubmissions),
            Self::PrepareSubmissions => Some(Self::SignSubmissions),
            Self::SignSubmissions => Some(Self::ExecuteSubmissions),
            Self::ExecuteSubmissions => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    fn requires_attestors(&self) -> bool {
        matches!(self, Self::UploadDars | Self::SignSubmissions)
    }

    fn is_waiting_for_attestors(&self) -> bool {
        matches!(self, Self::WaitingForAttestors)
    }
}

/// Generic workflow state tracker
pub struct WorkflowState<S> {
    /// Current workflow step
    current_step: RwLock<S>,
    /// Expected attestor IDs
    expected_attestors: HashSet<String>,
    /// Attestors that have connected
    connected_attestors: RwLock<HashSet<String>>,
    /// Attestors that have completed the current step
    completed_attestors: RwLock<HashSet<String>>,
    /// Data received from attestors (e.g., keys, signatures)
    attestor_data: RwLock<HashMap<String, Vec<u8>>>,
}

impl<S: WorkflowStep + 'static> WorkflowState<S> {
    pub fn new(initial_step: S, expected_attestors: Vec<String>) -> Arc<Self> {
        Arc::new(Self {
            current_step: RwLock::new(initial_step),
            expected_attestors: expected_attestors.into_iter().collect(),
            connected_attestors: RwLock::new(HashSet::new()),
            completed_attestors: RwLock::new(HashSet::new()),
            attestor_data: RwLock::new(HashMap::new()),
        })
    }

    pub async fn current_step(&self) -> S {
        *self.current_step.read().await
    }

    pub async fn store_attestor_data(&self, attestor_id: String, data: Vec<u8>) {
        let mut attestor_data = self.attestor_data.write().await;
        attestor_data.insert(attestor_id, data);
    }

    pub async fn get_all_attestor_data(&self) -> HashMap<String, Vec<u8>> {
        self.attestor_data.read().await.clone()
    }

    pub async fn clear_attestor_data(&self) {
        let mut attestor_data = self.attestor_data.write().await;
        attestor_data.clear();
    }

    pub async fn has_attestor_completed(&self, attestor_id: &str) -> bool {
        let completed = self.completed_attestors.read().await;
        completed.contains(attestor_id)
    }

    pub async fn attestor_connected(&self, attestor_id: String) {
        let mut connected = self.connected_attestors.write().await;

        let is_new = connected.insert(attestor_id.clone());
        if !is_new {
            return;
        }

        let connected_count = connected.len();
        let total_count = self.expected_attestors.len();
        tracing::info!("Attestor connected: {attestor_id} ({connected_count}/{total_count})");

        if connected_count == total_count {
            let current = self.current_step.read().await;
            if current.is_waiting_for_attestors() {
                drop(current);
                self.advance_step().await;
            }
        }
    }

    pub async fn current_command(&self) -> Option<MessageType> {
        let step = self.current_step.read().await;
        step.to_command()
    }

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

        if current.requires_attestors() && completed_count == total_count {
            drop(current);
            drop(completed);
            self.advance_step().await;
        }
    }

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
}
