//! Assembling the decentralized parties from topology.

use std::collections::{HashMap, HashSet};

use canton_proto_rs::com::digitalasset::canton::{
    crypto::admin::v30::vault_service_client::VaultServiceClient,
    protocol::v30::{DecentralizedNamespaceDefinition, PartyToParticipant},
    topology::admin::v30::{
        topology_aggregation_service_client::TopologyAggregationServiceClient,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use sqlx::SqlitePool;
use tonic::transport::Channel;

use super::{
    discovery::PARTY_READ_CONCURRENCY, list::PartyReadOpts, owner_claims::with_cached_owner_claims,
};
use crate::{
    auth::WorkflowAuth,
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig, PartyCredentials, default_package_config},
    db::{rows::DecPartyTopologyVersion, schema::SchemaRead},
    error::Result,
    party_discovery::{
        bounded_call, bounded_read, build_decentralized_namespace_request,
        build_party_to_participant_request, discover_hosted_party_ids, known_party_filters,
        my_namespace_fingerprints, owned_decentralized_namespaces, pair_namespaces_with_parties,
    },
    server::{
        queries::{
            TemplateId, contract_templates_all, fetch_package_versions, get_contracts,
            get_party_metadata, rules_templates,
        },
        types::{
            DecentralizedPartiesResponse, DecentralizedParty, ParticipantInfo, ResponseSource,
            permission_from_proto,
        },
    },
    utils,
    workflow::topology,
};

/// Topology serials travel beside the response, never through HTTP DTOs.
pub(super) struct DiscoveredParties {
    pub(super) response: DecentralizedPartiesResponse,
    pub(super) topology_versions: HashMap<String, DecPartyTopologyVersion>,
}

/// Fetch decentralized parties from Canton topology and ledger APIs.
///
/// Private on purpose: [`discover_and_cache`] is the only way in, so no caller
/// can start a discovery outside the per-prefix claim and the global bound.
pub(super) async fn fetch_decentralized_parties(
    config: &NodeConfig,
    db: &SqlitePool,
    prefix_filter: Option<&str>,
    auth: Option<WorkflowAuth>,
    party_credentials: &[PartyCredentials],
    opts: PartyReadOpts,
) -> Result<DiscoveredParties> {
    let exact_party_filters =
        exact_party_filters(config, db, prefix_filter, party_credentials).await?;

    let mut clients = TopologyClients::connect(config).await?;

    // Its own Admin API call on a cold process-wide cache, so it carries the
    // same deadline as the reads that follow it.
    let synchronizer_id =
        bounded_call("get_synchronizer_id", utils::get_synchronizer_id(config)).await?;
    let participant_id = config.participant_id().to_string();

    // Wanted twice: to discover which namespaces this node owns a key in, and
    // to pick each party's `my_owner_key` below.
    let namespace_key_fingerprints = my_namespace_fingerprints(&mut clients.vault).await?;

    let scope = TopologyScope {
        synchronizer_id: &synchronizer_id,
        participant_id: &participant_id,
    };
    let mut serials = TopologySerials::default();
    let (party_filters, discovered_definitions) = party_filters_or_discover(
        &mut clients,
        &scope,
        &namespace_key_fingerprints,
        prefix_filter,
        exact_party_filters,
        &mut serials.namespace,
    )
    .await?;
    let p2p_by_namespace = read_party_mappings(
        &mut clients.topology,
        &scope,
        party_filters,
        &mut serials.p2p,
    )
    .await?;
    let definitions = read_namespace_definitions(
        &mut clients.topology,
        &synchronizer_id,
        &p2p_by_namespace,
        discovered_definitions,
        &mut serials.namespace,
    )
    .await?;

    // Filter to parties where this participant is a member.
    let my_parties = pair_namespaces_with_parties(
        definitions,
        &p2p_by_namespace,
        &namespace_key_fingerprints,
        prefix_filter,
    );
    let (my_parties, total) = page_parties(my_parties, opts.page);

    let parties = enrich_parties(config, db, auth, opts.include_contracts, my_parties).await;

    let topology_versions = serials.versions_of(&parties);
    Ok(DiscoveredParties {
        response: DecentralizedPartiesResponse {
            parties,
            total,
            source: ResponseSource::Live,
            refreshing: false,
        },
        topology_versions,
    })
}

/// A party this participant hosts, paired with its decentralized namespace
/// and this node's owner key in it.
type HostedParty = (DecentralizedNamespaceDefinition, String, PartyToParticipant);

/// The three Admin API clients the topology reads use, on one channel.
struct TopologyClients {
    topology: TopologyManagerReadServiceClient<Channel>,
    aggregation: TopologyAggregationServiceClient<Channel>,
    vault: VaultServiceClient<Channel>,
}

impl TopologyClients {
    async fn connect(config: &NodeConfig) -> Result<Self> {
        let channel = config.admin_channel().await?;

        let topology = TopologyManagerReadServiceClient::new(channel.clone())
            .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);
        let aggregation = TopologyAggregationServiceClient::new(channel.clone())
            .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);
        let vault = VaultServiceClient::new(channel)
            .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);
        Ok(Self {
            topology,
            aggregation,
            vault,
        })
    }
}

/// The synchronizer and participant every topology read names.
struct TopologyScope<'a> {
    synchronizer_id: &'a str,
    participant_id: &'a str,
}

/// The topology serials the reads saw: each decentralized namespace's, and
/// each party's hosting mapping's. A serial of 0 is not recorded.
#[derive(Default)]
struct TopologySerials {
    namespace: HashMap<String, i64>,
    p2p: HashMap<String, i64>,
}

impl TopologySerials {
    /// The versions of each party in `parties` that has both serials.
    fn versions_of(
        &self,
        parties: &[DecentralizedParty],
    ) -> HashMap<String, DecPartyTopologyVersion> {
        parties
            .iter()
            .filter_map(|party| {
                Some((
                    party.party_id.to_string(),
                    DecPartyTopologyVersion {
                        namespace_serial: *self
                            .namespace
                            .get(&party.party_id.namespace.to_hex())?,
                        p2p_serial: *self.p2p.get(&party.party_id.to_string())?,
                    },
                ))
            })
            .collect()
    }
}

/// The exact party ids this node knows from its local sources, narrowed to
/// `prefix_filter`.
async fn exact_party_filters(
    config: &NodeConfig,
    db: &SqlitePool,
    prefix_filter: Option<&str>,
    party_credentials: &[PartyCredentials],
) -> Result<Vec<String>> {
    let workflow_runs = db.get_visible_workflow_runs().await?;
    let cached_parties = db.get_dec_parties_by_prefix("").await?;
    let known_party_ids = known_party_filters(
        party_credentials,
        config.validator_operator_party.as_ref(),
        workflow_runs
            .into_iter()
            .filter_map(|run| run.dec_party_id.map(|party_id| party_id.to_string())),
        cached_parties.into_iter().map(|party| party.party_id),
        None,
    );
    Ok(known_party_ids
        .into_iter()
        .filter(|party_id| prefix_filter.is_none_or(|prefix| party_id.starts_with(prefix)))
        .collect())
}

/// The party filters to read mappings for, and the namespace definitions a
/// discovery already read. A discovery records each namespace serial it
/// reads in `namespace_serials`.
///
/// Prefer exact IDs from every local source. A node with no exact ID for
/// what was asked has to discover: the decentralized namespaces it owns a
/// key in, then the parties its own participant hosts. Both are narrow
/// reads, unlike a participant-scoped party query, which Canton filters in
/// memory only after loading every party on the synchronizer.
///
/// Keying this on the filters rather than on "knows any party at all"
/// matters for a node that holds one party and is asked about another
/// prefix: it has to discover instead of issuing no query and reporting
/// nothing.
async fn party_filters_or_discover(
    clients: &mut TopologyClients,
    scope: &TopologyScope<'_>,
    namespace_key_fingerprints: &HashSet<String>,
    prefix_filter: Option<&str>,
    exact_party_filters: Vec<String>,
    namespace_serials: &mut HashMap<String, i64>,
) -> Result<(Vec<String>, Vec<DecentralizedNamespaceDefinition>)> {
    if !exact_party_filters.is_empty() {
        return Ok((exact_party_filters, Vec::new()));
    }
    let discovered_definitions = owned_decentralized_namespaces(
        &mut clients.topology,
        scope.synchronizer_id,
        namespace_key_fingerprints,
        namespace_serials,
    )
    .await?;

    let owned_namespaces: HashSet<String> = discovered_definitions
        .iter()
        .map(|definition| definition.decentralized_namespace.clone())
        .collect();

    let party_filters = discover_hosted_party_ids(
        &mut clients.aggregation,
        scope.synchronizer_id,
        scope.participant_id,
        prefix_filter,
        &owned_namespaces,
    )
    .await?;
    Ok((party_filters, discovered_definitions))
}

/// Read the party-to-participant mappings of each filter, grouped by the
/// party's namespace. Records each mapping's serial in `p2p_serials`.
async fn read_party_mappings(
    topology_client: &mut TopologyManagerReadServiceClient<Channel>,
    scope: &TopologyScope<'_>,
    party_filters: Vec<String>,
    p2p_serials: &mut HashMap<String, i64>,
) -> Result<HashMap<String, Vec<PartyToParticipant>>> {
    // Keyed to a list: a decentralized namespace is derived from its owner set
    // alone (`compute_decentralized_namespace`), so two parties with the same
    // owners share one, and a namespace-wide filter returns both.
    let mut p2p_by_namespace: HashMap<String, Vec<_>> = HashMap::new();
    // An exact party filter is an identifier *prefix* once Canton has split off
    // the namespace, so two filters can return the same party.
    let mut seen_parties: HashSet<String> = HashSet::new();
    for party_filter in party_filters {
        let response = bounded_read(
            "list_party_to_participant",
            topology_client.list_party_to_participant(tonic::Request::new(
                build_party_to_participant_request(
                    scope.synchronizer_id,
                    Some(party_filter.as_str()),
                    scope.participant_id,
                ),
            )),
        )
        .await?
        .into_inner();

        for (context, party_mapping) in topology::p2p_rows(response.results)? {
            let Some((_, namespace)) = party_mapping.party.rsplit_once("::") else {
                continue;
            };
            if namespace.is_empty() {
                continue;
            }
            if !seen_parties.insert(party_mapping.party.clone()) {
                continue;
            }
            if let Some(context) = context.filter(|context| context.serial > 0) {
                p2p_serials.insert(party_mapping.party.clone(), i64::from(context.serial));
            }
            p2p_by_namespace
                .entry(namespace.to_string())
                .or_default()
                .push(party_mapping);
        }
    }
    Ok(p2p_by_namespace)
}

/// The definitions of every namespace in `p2p_by_namespace`, reading only
/// the ones a discovery did not already read. Records each serial it reads
/// in `namespace_serials`.
async fn read_namespace_definitions(
    topology_client: &mut TopologyManagerReadServiceClient<Channel>,
    synchronizer_id: &str,
    p2p_by_namespace: &HashMap<String, Vec<PartyToParticipant>>,
    discovered_definitions: Vec<DecentralizedNamespaceDefinition>,
    namespace_serials: &mut HashMap<String, i64>,
) -> Result<Vec<DecentralizedNamespaceDefinition>> {
    // Discovery already read these, so only the exact-ID path pays for them.
    let mut definitions = discovered_definitions;
    let already_known: HashSet<String> = definitions
        .iter()
        .map(|definition| definition.decentralized_namespace.clone())
        .collect();

    let mut namespaces: Vec<_> = p2p_by_namespace
        .keys()
        .filter(|namespace| !already_known.contains(*namespace))
        .cloned()
        .collect();
    namespaces.sort();
    for namespace in namespaces {
        let response = bounded_read(
            "list_decentralized_namespace_definition",
            topology_client.list_decentralized_namespace_definition(tonic::Request::new(
                build_decentralized_namespace_request(synchronizer_id, &namespace),
            )),
        )
        .await?
        .into_inner();
        definitions.extend(response.results.into_iter().filter_map(|result| {
            let item = result.item?;
            if let Some(context) = result.context.filter(|context| context.serial > 0) {
                namespace_serials.insert(
                    item.decentralized_namespace.clone(),
                    i64::from(context.serial),
                );
            }
            Some(item)
        }));
    }
    Ok(definitions)
}

/// Sort the parties and cut out the requested page. Returns the page and
/// the number of parties before paging.
fn page_parties(
    my_parties: Vec<HostedParty>,
    page: Option<(usize, usize)>,
) -> (Vec<HostedParty>, usize) {
    // Stable order, so a paged caller sees each party once across requests.
    let mut my_parties = my_parties;
    my_parties.sort_by(|a, b| a.2.party.cmp(&b.2.party));
    let total = my_parties.len();
    if let Some((offset, limit)) = page {
        my_parties = my_parties.into_iter().skip(offset).take(limit).collect();
    }
    (my_parties, total)
}

/// Read each party's contracts, metadata and owner claims, at most
/// `PARTY_READ_CONCURRENCY` at a time. A party whose read fails is left out.
async fn enrich_parties(
    config: &NodeConfig,
    db: &SqlitePool,
    auth: Option<WorkflowAuth>,
    include_contracts: bool,
    my_parties: Vec<HostedParty>,
) -> Vec<DecentralizedParty> {
    // Check if we're in test mode (mock auth)
    let test_mode = matches!(auth, Some(WorkflowAuth::Mock(_)));

    // Hoisted out of the per-party futures: both are the same for every party,
    // and `list_packages` is a whole-participant Admin API read. Building them
    // per party meant one full package inventory in flight per hosted party.
    let packages = default_package_config();
    // One rules contract per party is all the parties list has a consumer for.
    let templates = if include_contracts {
        contract_templates_all(&packages)
    } else {
        rules_templates(&packages)
    };
    // Only when some party can use it. With no auth and no test mode every
    // per-party read below is skipped, so fetching a whole-participant
    // inventory would be pure waste — and it was never fetched on that path
    // before, when `get_contracts` fetched it for itself. A node that holds no
    // party reads nothing either, which is the ordinary state of one that has
    // not been onboarded yet.
    let package_versions = if !my_parties.is_empty() && (auth.is_some() || test_mode) {
        match fetch_package_versions(config).await {
            Ok(map) => map,
            Err(e) => {
                tracing::warn!("Failed to load package versions from Admin API: {e}");
                HashMap::new()
            }
        }
    } else {
        HashMap::new()
    };

    let reads = PartyReads {
        config,
        db,
        auth: &auth,
        test_mode,
        packages: &packages,
        package_versions: &package_versions,
        templates: &templates,
    };
    // Fetch contracts and metadata for all parties, at most
    // `PARTY_READ_CONCURRENCY` at a time.
    utils::bounded_ordered(
        PARTY_READ_CONCURRENCY,
        my_parties
            .into_iter()
            .map(|(item, my_owner_key, p2p)| reads.read_party(item, my_owner_key, p2p)),
    )
    .await
}

/// What every per-party read in [`enrich_parties`] shares.
struct PartyReads<'a> {
    config: &'a NodeConfig,
    db: &'a SqlitePool,
    auth: &'a Option<WorkflowAuth>,
    test_mode: bool,
    packages: &'a PackageConfig,
    package_versions: &'a HashMap<String, String>,
    templates: &'a [TemplateId],
}

impl PartyReads<'_> {
    /// Read one party's contracts and metadata with its own token, and merge
    /// in the owner claims cached for its participants.
    async fn read_party(
        &self,
        item: DecentralizedNamespaceDefinition,
        my_owner_key: String,
        p2p: PartyToParticipant,
    ) -> anyhow::Result<DecentralizedParty> {
        let party_id_str = p2p.party.clone();
        let party_id = CantonId::parse(&p2p.party)?;
        let token = party_token(self.auth, &party_id_str).await;

        let token_clone = token.clone();
        let (contracts, local_metadata) = if token.is_some() || self.test_mode {
            tokio::join!(
                async {
                    get_contracts(
                        self.config,
                        &party_id,
                        token,
                        self.packages,
                        self.package_versions,
                        self.templates,
                    )
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!("Failed to get contracts for {party_id_str}: {e}");
                        Vec::new()
                    })
                },
                async {
                    get_party_metadata(self.config, &party_id, token_clone)
                        .await
                        .ok()
                        .flatten()
                }
            )
        } else {
            (Vec::new(), None)
        };

        let self_uid = self.config.participant_id().to_string();
        let participants = participant_infos(&p2p, &self_uid, &my_owner_key);

        let (participants, non_hosting_owners) =
            with_cached_owner_claims(self.db, &party_id, &item.owners, participants).await;

        Ok(DecentralizedParty {
            party_id,
            threshold: item.threshold,
            confirmation_threshold: i32::try_from(p2p.threshold).ok(),
            owners: item.owners,
            my_owner_key: Some(my_owner_key),
            participants,
            non_hosting_owners,
            contracts,
            local_metadata,
        })
    }
}

/// The token this node holds for `party_id`, from auth (real or mock).
async fn party_token(auth: &Option<WorkflowAuth>, party_id: &str) -> Option<String> {
    // Registry uses raw string keys (`_by_str`) so we still need the party
    // id string for lookup.
    match auth {
        Some(WorkflowAuth::Keycloak(registry)) => match registry.get_by_str(party_id) {
            Some(tm) => tm.get_token().await.ok(),
            None => None,
        },
        Some(WorkflowAuth::Mock(mock_registry)) => {
            Some(mock_registry.get_by_str(party_id).await.get_token())
        }
        None => None,
    }
}

/// The party's hosting participants. Only this node's own entry carries an
/// owner key; peers' keys are resolved later via Noise polling.
fn participant_infos(
    p2p: &PartyToParticipant,
    self_uid: &str,
    my_owner_key: &str,
) -> Vec<ParticipantInfo> {
    p2p.participants
        .iter()
        .filter_map(|p| {
            let participant_uid = CantonId::parse(&p.participant_uid).ok()?;
            let owner_key = if participant_uid.to_string() == self_uid {
                Some(my_owner_key.to_owned())
            } else {
                None // resolved later via Noise polling of peers
            };
            Some(ParticipantInfo {
                participant_uid,
                permission: permission_from_proto(p.permission),
                owner_key,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        super::{cache::store_parties_to_db, test_support::a_party},
        *,
    };
    use crate::{config::Network, db::MIGRATOR};

    /// Discovery is the same query on every network, because it reads the
    /// decentralized-namespace mappings rather than every party. MainNet used
    /// to be the size problem, so it must not be treated as a special case
    /// again: with nothing listening, every network has to fail rather than
    /// quietly answer empty.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn every_network_discovers_the_same_way(pool: SqlitePool) {
        for network in [Network::Devnet, Network::Testnet, Network::Mainnet] {
            let config = closed_admin_api(network);

            let result = fetch_decentralized_parties(
                &config,
                &pool,
                None,
                None,
                &[],
                PartyReadOpts::default(),
            )
            .await;

            assert!(result.is_err(), "{network:?} must attempt discovery");
        }
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    async fn an_older_discovery_cannot_reintroduce_a_completed_kicks_intermediate_state(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let mut confirmed = a_party()?;
        confirmed.threshold = 2;
        confirmed.confirmation_threshold = Some(2);
        confirmed.owners = vec!["surviving-owner".to_string()];
        let versions = HashMap::from([(
            confirmed.party_id.to_string(),
            DecPartyTopologyVersion {
                namespace_serial: 2,
                p2p_serial: 2,
            },
        )]);
        store_parties_to_db(&pool, "cbtc", &[confirmed.clone()], &versions).await?;
        let mut intermediate = confirmed.clone();
        intermediate.confirmation_threshold = Some(3);
        intermediate.owners.push("old-owner".to_string());
        let old = HashMap::from([(
            confirmed.party_id.to_string(),
            DecPartyTopologyVersion {
                namespace_serial: 2,
                p2p_serial: 1,
            },
        )]);

        store_parties_to_db(&pool, "cbtc", &[intermediate], &old).await?;

        let cached = pool.get_dec_parties_by_prefix("cbtc").await?;
        assert_eq!(cached[0].confirmation_threshold, Some(2));
        let owners = pool.get_dec_party_owners(&confirmed.party_id).await?;
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].owner_key, "surviving-owner");
        Ok(())
    }

    /// The read returned one party without its serials. That party is
    /// skipped and keeps the rows it had. Every other party is still cached.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_party_without_serials_does_not_block_the_others(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let unversioned = a_party()?;
        let mut versioned = a_party()?;
        versioned.party_id = CantonId::parse(&format!("cbtc::1220{}", "ab".repeat(32)))?;
        let serials = |party: &DecentralizedParty, serial: i64| {
            (
                party.party_id.to_string(),
                DecPartyTopologyVersion {
                    namespace_serial: serial,
                    p2p_serial: serial,
                },
            )
        };
        store_parties_to_db(
            &pool,
            "cbtc",
            std::slice::from_ref(&unversioned),
            &HashMap::from([serials(&unversioned, 1)]),
        )
        .await?;

        let mut changed = unversioned.clone();
        changed.threshold = 2;
        versioned.threshold = 3;
        store_parties_to_db(
            &pool,
            "cbtc",
            &[changed, versioned.clone()],
            &HashMap::from([serials(&versioned, 1)]),
        )
        .await?;

        let cached: HashMap<String, i64> = pool
            .get_dec_parties_by_prefix("cbtc")
            .await?
            .into_iter()
            .map(|row| (row.party_id, row.threshold))
            .collect();
        assert_eq!(
            cached,
            HashMap::from([
                (unversioned.party_id.to_string(), 1),
                (versioned.party_id.to_string(), 3),
            ])
        );
        Ok(())
    }

    /// A node that holds one party and is asked about a different prefix has
    /// no exact ID to use, so it must discover under that prefix rather than
    /// issue no query at all and report nothing.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_known_party_does_not_mask_another_prefix(pool: SqlitePool) -> anyhow::Result<()> {
        let config = closed_admin_api(Network::Mainnet);
        store_parties_to_db(
            &pool,
            "cbtc",
            &[a_party()?],
            &HashMap::from([(
                a_party()?.party_id.to_string(),
                DecPartyTopologyVersion {
                    namespace_serial: 1,
                    p2p_serial: 1,
                },
            )]),
        )
        .await?;

        let result = fetch_decentralized_parties(
            &config,
            &pool,
            Some("other"),
            None,
            &[],
            PartyReadOpts::default(),
        )
        .await;

        assert!(
            result.is_err(),
            "an unmatched prefix must still be queried against Canton"
        );
        Ok(())
    }

    /// Nothing can listen on port 1, so any attempt to reach Canton fails
    /// instead of finding a participant a developer happens to be running.
    fn closed_admin_api(network: Network) -> NodeConfig {
        let mut config = NodeConfig::default();
        config.canton.network = network;
        config.canton.admin_api_host = "127.0.0.1".to_string();
        config.canton.admin_api_port = 1;
        config
    }
}
