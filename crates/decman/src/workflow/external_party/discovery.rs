//! Discover hosted IDs through Canton's batched aggregation read, then fetch
//! only those mappings. A participant filter on ListPartyToParticipant is
//! applied after the entire synchronizer's mappings have been materialized.

use std::{collections::HashMap, future::Future, time::Duration};

use anyhow::{Context, Result};
use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::enums::ParticipantPermission,
    topology::admin::v30::{
        ListDecentralizedNamespaceDefinitionRequest, ListDecentralizedNamespaceDefinitionResponse,
        ListPartiesRequest, ListPartiesResponse, ListPartyToParticipantRequest,
        ListPartyToParticipantResponse,
        list_party_to_participant_response::result::Item as P2pItem,
        topology_aggregation_service_client::TopologyAggregationServiceClient,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use tonic::{Request, Response, transport::Channel};

use super::steps::{
    HostedExternalParty, HostedExternalPartyHost, party_query, timestamp_to_rfc3339,
};
use crate::{config::NodeConfig, utils};

// ListParties has a limit but no cursor. Refuse a full result rather than
// publish a snapshot that silently omits parties. Match decentralized discovery.
const MAX_HOSTED_PARTIES: i32 = 5_000;
const READ_TIMEOUT: Duration = Duration::from_secs(60);

#[async_trait::async_trait]
trait DiscoveryReader {
    async fn parties(&mut self, request: ListPartiesRequest) -> Result<ListPartiesResponse>;
    async fn mapping(
        &mut self,
        request: ListPartyToParticipantRequest,
    ) -> Result<ListPartyToParticipantResponse>;
    async fn namespace(
        &mut self,
        request: ListDecentralizedNamespaceDefinitionRequest,
    ) -> Result<ListDecentralizedNamespaceDefinitionResponse>;
}

struct CantonReader {
    aggregation: TopologyAggregationServiceClient<Channel>,
    topology: TopologyManagerReadServiceClient<Channel>,
}

fn timed_request<T>(message: T) -> Request<T> {
    let mut request = Request::new(message);
    request.set_timeout(READ_TIMEOUT);
    request
}

async fn read<T>(
    operation: &str,
    timeout: Duration,
    call: impl Future<Output = std::result::Result<Response<T>, tonic::Status>>,
) -> Result<T> {
    tokio::time::timeout(timeout, call)
        .await
        .with_context(|| format!("{operation} exceeded {}s", timeout.as_secs()))?
        .with_context(|| format!("{operation} failed"))
        .map(Response::into_inner)
}

#[async_trait::async_trait]
impl DiscoveryReader for CantonReader {
    async fn parties(&mut self, request: ListPartiesRequest) -> Result<ListPartiesResponse> {
        read(
            "ListParties",
            READ_TIMEOUT,
            self.aggregation.list_parties(timed_request(request)),
        )
        .await
    }

    async fn mapping(
        &mut self,
        request: ListPartyToParticipantRequest,
    ) -> Result<ListPartyToParticipantResponse> {
        read(
            "ListPartyToParticipant",
            READ_TIMEOUT,
            self.topology
                .list_party_to_participant(timed_request(request)),
        )
        .await
    }

    async fn namespace(
        &mut self,
        request: ListDecentralizedNamespaceDefinitionRequest,
    ) -> Result<ListDecentralizedNamespaceDefinitionResponse> {
        read(
            "ListDecentralizedNamespaceDefinition",
            READ_TIMEOUT,
            self.topology
                .list_decentralized_namespace_definition(timed_request(request)),
        )
        .await
    }
}

pub(super) async fn list_hosted_external_parties(
    config: &NodeConfig,
) -> Result<Vec<HostedExternalParty>> {
    let synchronizer_id = tokio::time::timeout(READ_TIMEOUT, utils::get_synchronizer_id(config))
        .await
        .context("Resolving the synchronizer timed out")??;
    let channel = tokio::time::timeout(READ_TIMEOUT, config.admin_channel())
        .await
        .context("Connecting to the Admin API timed out")??;
    let mut reader = CantonReader {
        aggregation: TopologyAggregationServiceClient::new(channel.clone())
            .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE),
        topology: TopologyManagerReadServiceClient::new(channel)
            .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE),
    };
    discover(
        &mut reader,
        &synchronizer_id,
        &config.participant_id().to_string(),
    )
    .await
}

// Aggregation accepts a logical synchronizer ID, while the topology store
// needs the physical ID (logical ID followed by the protocol version).
fn logical_synchronizer_id(id: &str) -> &str {
    id.rsplit_once("::")
        .filter(|(logical, _)| logical.contains("::"))
        .map_or(id, |(logical, _)| logical)
}

async fn discover(
    reader: &mut impl DiscoveryReader,
    synchronizer_id: &str,
    participant_id: &str,
) -> Result<Vec<HostedExternalParty>> {
    let response = reader
        .parties(ListPartiesRequest {
            as_of: None,
            limit: MAX_HOSTED_PARTIES,
            synchronizer_ids: vec![logical_synchronizer_id(synchronizer_id).to_string()],
            filter_party: String::new(),
            filter_participant: participant_id.to_string(),
        })
        .await?;
    anyhow::ensure!(
        response.results.len() < MAX_HOSTED_PARTIES as usize,
        "External party discovery reached the {MAX_HOSTED_PARTIES}-party limit; \
         refusing to publish an incomplete snapshot"
    );

    let own_namespace = participant_id.rsplit_once("::").map(|(_, ns)| ns);
    let mut party_ids: Vec<_> = response.results.into_iter().map(|r| r.party).collect();
    party_ids.sort();
    party_ids.dedup();

    let mut decentralized = HashMap::new();
    let mut parties = Vec::new();
    // Sequential reads retain only one party's topology response at a time.
    for party_id in party_ids {
        let Some((_, namespace)) = party_id.rsplit_once("::") else {
            continue;
        };
        if namespace.is_empty() || Some(namespace) == own_namespace {
            continue;
        }
        let is_decentralized = match decentralized.get(namespace) {
            Some(value) => *value,
            None => {
                let response = reader
                    .namespace(ListDecentralizedNamespaceDefinitionRequest {
                        base_query: Some(party_query(synchronizer_id, false)),
                        filter_namespace: namespace.to_string(),
                    })
                    .await?;
                let value = response.results.into_iter().any(|r| {
                    r.item
                        .is_some_and(|item| item.decentralized_namespace == namespace)
                });
                decentralized.insert(namespace.to_string(), value);
                value
            }
        };
        if is_decentralized {
            continue;
        }
        let response = reader
            .mapping(ListPartyToParticipantRequest {
                base_query: Some(party_query(synchronizer_id, false)),
                filter_party: party_id.clone(),
                filter_participant: participant_id.to_string(),
            })
            .await?;
        for result in response.results {
            let Some(P2pItem::V30(mapping)) = result.item else {
                continue;
            };
            // Canton uses prefix matching, even when passed a full party ID.
            if mapping.party != party_id {
                continue;
            }
            let Some(entry) = mapping.participants.iter().find(|h| {
                h.participant_uid == participant_id
                    && h.permission == ParticipantPermission::Confirmation as i32
            }) else {
                continue;
            };
            parties.push(HostedExternalParty {
                party_id: mapping.party,
                fingerprint: namespace.to_string(),
                threshold: mapping.threshold,
                host_count: mapping.participants.len() as u32,
                created_at: result
                    .context
                    .as_ref()
                    .and_then(|c| c.valid_from.as_ref())
                    .map(timestamp_to_rfc3339),
                onboarding: entry.onboarding.is_some(),
                hosts: mapping
                    .participants
                    .into_iter()
                    .map(|h| HostedExternalPartyHost {
                        participant_uid: h.participant_uid,
                        permission: h.permission,
                    })
                    .collect(),
            });
        }
    }
    Ok(parties)
}

#[cfg(test)]
mod tests {
    use super::*;
    use canton_proto_rs::com::digitalasset::canton::{
        protocol::v30::{
            DecentralizedNamespaceDefinition, PartyToParticipant,
            party_to_participant::{HostingParticipant, hosting_participant::Onboarding},
        },
        topology::admin::v30::{
            BaseResult, base_query, list_decentralized_namespace_definition_response,
            list_parties_response, list_party_to_participant_response,
        },
    };

    const PARTICIPANT: &str = "participant::own";
    const SYNCHRONIZER: &str = "global::synchronizer::42";

    #[derive(Default)]
    struct Reader {
        ids: Vec<String>,
        mappings: HashMap<String, ListPartyToParticipantResponse>,
        definitions: HashMap<String, ListDecentralizedNamespaceDefinitionResponse>,
        party_requests: Vec<ListPartiesRequest>,
        mapping_requests: Vec<ListPartyToParticipantRequest>,
        namespace_requests: Vec<ListDecentralizedNamespaceDefinitionRequest>,
        fail_mapping: Option<String>,
    }

    #[async_trait::async_trait]
    impl DiscoveryReader for Reader {
        async fn parties(&mut self, request: ListPartiesRequest) -> Result<ListPartiesResponse> {
            self.party_requests.push(request);
            Ok(ListPartiesResponse {
                results: self
                    .ids
                    .iter()
                    .map(|party| list_parties_response::Result {
                        party: party.clone(),
                        ..Default::default()
                    })
                    .collect(),
            })
        }

        async fn mapping(
            &mut self,
            request: ListPartyToParticipantRequest,
        ) -> Result<ListPartyToParticipantResponse> {
            anyhow::ensure!(
                !request.filter_party.is_empty(),
                "network-wide mapping read"
            );
            let party = request.filter_party.clone();
            self.mapping_requests.push(request);
            anyhow::ensure!(
                self.fail_mapping.as_ref() != Some(&party),
                "mapping unavailable"
            );
            Ok(self.mappings.remove(&party).unwrap_or_default())
        }

        async fn namespace(
            &mut self,
            request: ListDecentralizedNamespaceDefinitionRequest,
        ) -> Result<ListDecentralizedNamespaceDefinitionResponse> {
            anyhow::ensure!(
                !request.filter_namespace.is_empty(),
                "network-wide namespace read"
            );
            let namespace = request.filter_namespace.clone();
            self.namespace_requests.push(request);
            Ok(self.definitions.remove(&namespace).unwrap_or_default())
        }
    }

    fn mapping(
        party: &str,
        host: &str,
        permission: ParticipantPermission,
        onboarding: bool,
    ) -> list_party_to_participant_response::Result {
        list_party_to_participant_response::Result {
            context: Some(BaseResult {
                valid_from: Some(prost_types::Timestamp {
                    seconds: 1_000,
                    nanos: 0,
                }),
                ..Default::default()
            }),
            item: Some(P2pItem::V30(PartyToParticipant {
                party: party.to_string(),
                threshold: 1,
                participants: vec![HostingParticipant {
                    participant_uid: host.to_string(),
                    permission: permission as i32,
                    onboarding: onboarding.then_some(Onboarding {}),
                }],
                ..Default::default()
            })),
        }
    }

    fn definition(namespace: &str) -> ListDecentralizedNamespaceDefinitionResponse {
        ListDecentralizedNamespaceDefinitionResponse {
            results: vec![list_decentralized_namespace_definition_response::Result {
                item: Some(DecentralizedNamespaceDefinition {
                    decentralized_namespace: namespace.to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }],
        }
    }

    #[tokio::test]
    async fn discovery_scopes_every_read_and_preserves_hosting_details() -> Result<()> {
        let mut alice = mapping(
            "alice::external",
            PARTICIPANT,
            ParticipantPermission::Confirmation,
            true,
        );
        if let Some(P2pItem::V30(mapping)) = &mut alice.item {
            mapping.threshold = 2;
            mapping.participants.push(HostingParticipant {
                participant_uid: "peer::host".to_string(),
                permission: ParticipantPermission::Confirmation as i32,
                onboarding: None,
            });
        }
        let mut reader = Reader {
            ids: [
                "alice::external",
                "bob::external",
                "alice::external",
                "dec::shared",
                "local::own",
                "observer::observed",
                "foreign::other",
                "gone::missing",
                "malformed",
                "empty::",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            ..Default::default()
        };
        reader.mappings.insert(
            "alice::external".into(),
            ListPartyToParticipantResponse {
                results: vec![
                    alice,
                    // A prefix match must not introduce an unrequested party.
                    mapping(
                        "alice::external-extra",
                        PARTICIPANT,
                        ParticipantPermission::Confirmation,
                        false,
                    ),
                ],
            },
        );
        for (party, host, permission) in [
            (
                "bob::external",
                PARTICIPANT,
                ParticipantPermission::Confirmation,
            ),
            (
                "observer::observed",
                PARTICIPANT,
                ParticipantPermission::Observation,
            ),
            (
                "foreign::other",
                "remote::host",
                ParticipantPermission::Confirmation,
            ),
        ] {
            reader.mappings.insert(
                party.into(),
                ListPartyToParticipantResponse {
                    results: vec![mapping(party, host, permission, false)],
                },
            );
        }
        reader
            .definitions
            .insert("shared".into(), definition("shared"));
        // A namespace prefix match is not the requested definition.
        reader
            .definitions
            .insert("external".into(), definition("external-extra"));
        let parties = discover(&mut reader, SYNCHRONIZER, PARTICIPANT).await?;
        assert_eq!(
            parties
                .iter()
                .map(|p| p.party_id.as_str())
                .collect::<Vec<_>>(),
            ["alice::external", "bob::external"]
        );
        let alice = &parties[0];
        assert_eq!(alice.fingerprint, "external");
        assert!(alice.onboarding, "suspended hosts must remain visible");
        assert!(!parties[1].onboarding);
        assert_eq!(alice.threshold, 2);
        assert_eq!(alice.host_count, 2);
        assert_eq!(alice.hosts[0].participant_uid, PARTICIPANT);
        assert_eq!(alice.hosts[1].participant_uid, "peer::host");
        assert_eq!(
            alice.hosts[0].permission,
            ParticipantPermission::Confirmation as i32
        );
        assert_eq!(
            alice.created_at.as_deref(),
            Some("1970-01-01T00:16:40+00:00")
        );

        assert_eq!(reader.party_requests.len(), 1);
        let request = &reader.party_requests[0];
        assert_eq!(request.limit, MAX_HOSTED_PARTIES);
        assert_eq!(request.synchronizer_ids, ["global::synchronizer"]);
        assert_eq!(request.filter_participant, PARTICIPANT);
        assert_eq!(
            reader
                .namespace_requests
                .iter()
                .filter(|r| r.filter_namespace == "external")
                .count(),
            1
        );
        assert!(
            !reader
                .namespace_requests
                .iter()
                .any(|r| r.filter_namespace == "own")
        );
        assert!(
            !reader
                .mapping_requests
                .iter()
                .any(|r| r.filter_party == "dec::shared")
        );
        for request in &reader.mapping_requests {
            assert_eq!(request.filter_participant, PARTICIPANT);
            let base = request.base_query.as_ref().context("missing base query")?;
            assert!(!base.proposals);
            assert!(matches!(
                base.time_query,
                Some(base_query::TimeQuery::HeadState(()))
            ));
            assert_eq!(base.store, party_query(SYNCHRONIZER, false).store);
        }
        for request in &reader.namespace_requests {
            assert_eq!(request.base_query, Some(party_query(SYNCHRONIZER, false)));
        }
        Ok(())
    }

    #[tokio::test]
    async fn empty_discovery_does_not_scan_any_topology() -> Result<()> {
        let mut reader = Reader::default();
        assert!(
            discover(&mut reader, SYNCHRONIZER, PARTICIPANT)
                .await?
                .is_empty()
        );
        assert!(reader.mapping_requests.is_empty());
        assert!(reader.namespace_requests.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn a_full_enumeration_fails_before_reading_or_publishing_partial_results() {
        let mut reader = Reader {
            ids: vec!["alice::external".into(); MAX_HOSTED_PARTIES as usize],
            ..Default::default()
        };
        let result = discover(&mut reader, SYNCHRONIZER, PARTICIPANT).await;
        assert!(matches!(result, Err(e) if e.to_string().contains("incomplete snapshot")));
        assert!(reader.mapping_requests.is_empty());
        assert!(reader.namespace_requests.is_empty());
    }

    #[tokio::test]
    async fn a_failed_party_read_does_not_publish_a_partial_snapshot() {
        let mut reader = Reader {
            ids: vec!["alice::external".into(), "bob::external".into()],
            mappings: HashMap::from([(
                "alice::external".into(),
                ListPartyToParticipantResponse {
                    results: vec![mapping(
                        "alice::external",
                        PARTICIPANT,
                        ParticipantPermission::Confirmation,
                        false,
                    )],
                },
            )]),
            fail_mapping: Some("bob::external".into()),
            ..Default::default()
        };
        let result = discover(&mut reader, SYNCHRONIZER, PARTICIPANT).await;
        assert!(matches!(result, Err(e) if e.to_string() == "mapping unavailable"));
        assert_eq!(reader.mapping_requests.len(), 2);
    }

    #[test]
    fn aggregation_uses_logical_ids_without_losing_the_namespace() {
        assert_eq!(
            logical_synchronizer_id(SYNCHRONIZER),
            "global::synchronizer"
        );
        assert_eq!(
            logical_synchronizer_id("global::synchronizer"),
            "global::synchronizer"
        );
    }

    #[test]
    fn topology_reads_send_a_server_deadline() -> Result<()> {
        let request = timed_request(ListPartiesRequest::default());
        assert_eq!(
            request
                .metadata()
                .get("grpc-timeout")
                .context("missing gRPC deadline")?
                .to_str()?,
            "60000000u"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_timed_out_read_drops_its_pending_work() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        struct PendingRead(Arc<AtomicBool>);
        impl Drop for PendingRead {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let guard = PendingRead(dropped.clone());
        let result = read("ListParties", Duration::from_millis(1), async move {
            let _guard = guard;
            std::future::pending::<std::result::Result<Response<()>, tonic::Status>>().await
        })
        .await;
        assert!(matches!(result, Err(e) if e.to_string().contains("ListParties exceeded")));
        assert!(dropped.load(Ordering::SeqCst));
    }
}
