use std::time::Duration;

use tokio::{net::TcpStream, time::timeout};

use crate::{
    config::{NetworkConfig, Participant, ParticipantRole},
    error::Result,
};

/// Election result containing the elected coordinator
#[derive(Clone, Debug)]
pub struct ElectionResult {
    /// The participant that was elected as coordinator
    pub coordinator: Participant,
    /// Whether the current participant is the coordinator
    pub is_me: bool,
}

/// Perform leader election using the Bully algorithm
///
/// Algorithm:
/// 1. If there's a designated coordinator (role = coordinator), try connecting to it first
/// 2. If designated coordinator is reachable, use it
/// 3. If designated coordinator is unreachable or doesn't exist, run election:
///    - Sort all participants by ID (lexicographically)
///    - Starting from highest ID, try connecting
///    - First reachable participant becomes coordinator
/// 4. If current participant is the highest reachable ID, it becomes coordinator
///
/// # Errors
///
/// Returns an error if no participants are reachable (insufficient quorum)
pub async fn run_election(
    network_config: &NetworkConfig,
    my_node_id: &str,
) -> Result<ElectionResult> {
    tracing::info!("Starting leader election (Bully algorithm)");
    tracing::info!("My node ID: {my_node_id}");

    // Step 1: Try designated coordinator first
    if let Some(designated) = try_get_designated_coordinator(network_config) {
        tracing::info!(
            "Attempting to connect to designated coordinator: {id}",
            id = designated.id
        );

        if is_participant_reachable(designated).await {
            tracing::info!(
                "Designated coordinator {id} is reachable, using it",
                id = designated.id
            );
            let is_me = designated.id == my_node_id;
            return Ok(ElectionResult {
                coordinator: designated.clone(),
                is_me,
            });
        }

        tracing::warn!(
            "Designated coordinator {id} is unreachable, proceeding with election",
            id = designated.id
        );
    }

    // Step 2: Sort participants by ID (lexicographically, highest first)
    let mut participants = network_config.participants.clone();
    participants.sort_by(|a, b| b.id.cmp(&a.id));

    let candidate_ids: Vec<_> = participants.iter().map(|p| &p.id).collect();
    tracing::info!("Election candidates (sorted): {candidate_ids:?}");

    // Step 3: Iterate from highest to lowest ID
    for participant in &participants {
        tracing::debug!("Checking candidate: {id}", id = participant.id);

        // If this candidate is me, I'm the highest reachable → I become coordinator
        if participant.id == my_node_id {
            tracing::info!(
                "I ({my_node_id}) am the highest available ID, declaring myself coordinator"
            );
            return Ok(ElectionResult {
                coordinator: participant.clone(),
                is_me: true,
            });
        }

        // Try connecting to this higher-priority candidate
        if is_participant_reachable(participant).await {
            tracing::info!(
                "Participant {id} is reachable and has higher priority, accepting as coordinator",
                id = participant.id
            );
            return Ok(ElectionResult {
                coordinator: participant.clone(),
                is_me: false,
            });
        }

        tracing::debug!(
            "Participant {id} is unreachable, trying next",
            id = participant.id
        );
    }

    // Should never reach here if participants list is not empty
    anyhow::bail!("Election failed: no participants reachable (insufficient quorum)")
}

/// Try to get designated coordinator from config (if explicitly marked)
fn try_get_designated_coordinator(network_config: &NetworkConfig) -> Option<&Participant> {
    network_config
        .participants
        .iter()
        .find(|p| p.role == Some(ParticipantRole::Coordinator))
}

/// Check if a participant is reachable by attempting TCP connection
///
/// Returns true if a TCP connection can be established within the timeout period
async fn is_participant_reachable(participant: &Participant) -> bool {
    let address = format!(
        "{addr}:{port}",
        addr = participant.address,
        port = participant.port
    );
    tracing::debug!("Attempting TCP connection to {address}");

    // Try to connect with short timeout (3 seconds)
    match timeout(Duration::from_secs(3), TcpStream::connect(&address)).await {
        Ok(Ok(_stream)) => {
            tracing::debug!("Successfully connected to {address}");
            true
        }
        Ok(Err(e)) => {
            tracing::debug!("Failed to connect to {address}: {e}");
            false
        }
        Err(_) => {
            tracing::debug!("Timeout connecting to {address}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{
        config::{ApplicationConfig, CoordinatorStrategy, NetworkInfo, Timeouts},
        error::Result,
    };

    fn test_application_config() -> ApplicationConfig {
        ApplicationConfig {
            party_id_prefix: "test-network".to_string(),
            namespace_key_name: "test-namespace".to_string(),
            daml_key_name: "test-daml".to_string(),
            operator_party_hint: "operator".to_string(),
            contracts: vec![],
        }
    }

    fn create_test_network(coordinator_strategy: CoordinatorStrategy) -> Result<NetworkConfig> {
        Ok(NetworkConfig {
            network: NetworkInfo {
                name: "test-network".to_string(),
                protocol_version: "1.0".to_string(),
                port: 9000,
                coordinator_strategy,
                operator_party: None,
            },
            participants: vec![
                Participant {
                    id: "attestor-1".to_string(),
                    name: "Attestor 1".to_string(),
                    role: Some(ParticipantRole::Coordinator),
                    address: "10.0.1.101".to_string(),
                    port: 9000,
                    public_key: "abc123".to_string(),
                    party: None,
                },
                Participant {
                    id: "attestor-2".to_string(),
                    name: "Attestor 2".to_string(),
                    role: None,
                    address: "10.0.1.102".to_string(),
                    port: 9000,
                    public_key: "def456".to_string(),
                    party: None,
                },
                Participant {
                    id: "attestor-3".to_string(),
                    name: "Attestor 3".to_string(),
                    role: None,
                    address: "10.0.1.103".to_string(),
                    port: 9000,
                    public_key: "ghi789".to_string(),
                    party: None,
                },
            ],
            timeouts: Timeouts::default(),
            application: test_application_config(),
        })
    }

    #[test]
    fn test_designated_coordinator_selection() -> Result {
        let network = create_test_network(CoordinatorStrategy::Explicit)?;
        let designated = try_get_designated_coordinator(&network);

        assert!(designated.is_some());
        assert_eq!(designated.unwrap().id, "attestor-1");
        Ok(())
    }

    #[test]
    fn test_participant_sorting() -> Result {
        let mut participants = [
            Participant {
                id: "attestor-1".to_string(),
                name: "".to_string(),
                role: None,
                address: "".to_string(),
                port: 0,
                public_key: "".to_string(),
                party: None,
            },
            Participant {
                id: "attestor-3".to_string(),
                name: "".to_string(),
                role: None,
                address: "".to_string(),
                port: 0,
                public_key: "".to_string(),
                party: None,
            },
            Participant {
                id: "attestor-2".to_string(),
                name: "".to_string(),
                role: None,
                address: "".to_string(),
                port: 0,
                public_key: "".to_string(),
                party: None,
            },
        ];

        participants.sort_by(|a, b| b.id.cmp(&a.id));

        assert_eq!(participants[0].id, "attestor-3");
        assert_eq!(participants[1].id, "attestor-2");
        assert_eq!(participants[2].id, "attestor-1");
        Ok(())
    }
}
