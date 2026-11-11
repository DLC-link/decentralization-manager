//! Topology reads: hosted party ids, owned namespaces and the request builders.

use std::collections::{HashMap, HashSet};

use canton_proto_rs::com::digitalasset::canton::{
    crypto::{
        admin::v30::{
            ListMyKeysRequest, private_key_metadata, vault_service_client::VaultServiceClient,
        },
        v30::public_key,
    },
    protocol::v30::{DecentralizedNamespaceDefinition, PartyToParticipant},
    topology::admin::v30::{
        ListDecentralizedNamespaceDefinitionRequest, ListPartiesRequest,
        ListPartyToParticipantRequest,
        topology_aggregation_service_client::TopologyAggregationServiceClient,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use tonic::transport::Channel;

use super::bounded::bounded_read;
use crate::{
    canton_id::CantonId, config::PartyCredentials, error::Result, utils, workflow::topology,
};

/// Most parties `ListParties` returns for one participant.
///
/// The RPC takes a limit and offers no cursor, so this truncates rather than
/// pages. It sits far above any realistic number of parties on one
/// participant; a node holding more would discover the first
/// `MAX_HOSTED_PARTIES` of them, and the exact-ID path covers the rest as soon
/// as one is known locally.
const MAX_HOSTED_PARTIES: i32 = 5_000;

/// The full IDs of the decentralized parties this node hosts, found without
/// reading every party on the synchronizer.
///
/// There is no way to ask Canton for a party by namespace.
/// `listPartyToParticipant` re-filters its own result with
/// `partyId.startsWith(filter_party)` against the unsplit string, and a party
/// ID can never begin with the `::` delimiter, so a namespace-only filter
/// matches nothing however the store reads it. `ListParties` takes a
/// participant filter, which is the question discovery actually asks, and
/// answers with full party IDs the exact-filter path can use.
pub(crate) async fn discover_hosted_party_ids(
    aggregation_client: &mut TopologyAggregationServiceClient<Channel>,
    synchronizer_id: &str,
    participant_id: &str,
    prefix_filter: Option<&str>,
    owned_namespaces: &HashSet<String>,
) -> Result<Vec<String>> {
    if owned_namespaces.is_empty() {
        return Ok(Vec::new());
    }

    // Unscoped, this asks across every synchronizer the participant is
    // connected to, and those parties eat the result budget while the reads
    // around it are scoped to this one.
    let synchronizer_ids = match logical_synchronizer_id(synchronizer_id) {
        Some(id) => vec![id.to_string()],
        None => {
            tracing::warn!(
                "Cannot read a logical synchronizer ID out of '{synchronizer_id}', so party \
                 discovery asks across every connected synchronizer. Parties from the others \
                 count against the {MAX_HOSTED_PARTIES} result budget."
            );
            Vec::new()
        }
    };

    let response = bounded_read(
        "list_parties",
        aggregation_client.list_parties(tonic::Request::new(build_list_parties_request(
            synchronizer_ids,
            prefix_filter,
            participant_id,
        ))),
    )
    .await?
    .into_inner();

    // The RPC truncates at the limit and says nothing, so a full page is the
    // only signal that parties were left behind. It has no cursor, so there is
    // nothing to page with: a participant hosting more parties than this needs
    // the query narrowed instead.
    if response.results.len() == MAX_HOSTED_PARTIES as usize {
        tracing::warn!(
            "ListParties returned the full {MAX_HOSTED_PARTIES}-party limit for this \
             participant{scope}, so discovery may have missed a decentralized party. Ask for \
             a party-name prefix to narrow it, which Canton applies in the store query. Any \
             party already known locally is unaffected, since those are queried by exact ID.",
            scope = match prefix_filter {
                Some(prefix) => format!(" under prefix '{prefix}'"),
                None => String::new(),
            }
        );
    }

    Ok(parties_in_namespaces(
        response.results.into_iter().map(|result| result.party),
        owned_namespaces,
    ))
}

/// Build the `ListParties` request that discovers this participant's parties.
fn build_list_parties_request(
    synchronizer_ids: Vec<String>,
    prefix_filter: Option<&str>,
    participant_id: &str,
) -> ListPartiesRequest {
    ListPartiesRequest {
        as_of: None,
        limit: MAX_HOSTED_PARTIES,
        synchronizer_ids,
        // A bare prefix, with no `::`, narrows by party identifier on both
        // sides of Canton's filtering: `identifier LIKE 'prefix%'` in the
        // store query and `identifier.startsWith` in the post-filter. That
        // is what keeps this bounded on a participant hosting a very large
        // number of parties, where the result limit would otherwise truncate
        // before reaching any decentralized party.
        filter_party: prefix_filter.unwrap_or_default().to_string(),
        filter_participant: participant_id.to_string(),
    }
}

/// The logical synchronizer ID inside a physical one.
///
/// `ListParties` parses this field as a `SynchronizerId`, so the physical form
/// `<name>::<fingerprint>::<protocol version>` is rejected outright. Returns
/// `None` for anything that is not in that shape, and the caller then sends no
/// synchronizer filter rather than a request Canton will refuse.
fn logical_synchronizer_id(physical: &str) -> Option<&str> {
    let (logical, _protocol_version) = physical.rsplit_once("::")?;
    logical.contains("::").then_some(logical)
}

/// Keep the party IDs whose namespace is one of `namespaces`.
///
/// `ListParties` answers with every party the participant hosts, decentralized
/// or not, so the namespace is what separates the ones this node is a member of
/// from the ordinary parties it also hosts.
fn parties_in_namespaces(
    parties: impl IntoIterator<Item = String>,
    namespaces: &HashSet<String>,
) -> Vec<String> {
    let mut party_ids: Vec<String> = parties
        .into_iter()
        .filter(|party| {
            party
                .rsplit_once("::")
                .is_some_and(|(hint, namespace)| !hint.is_empty() && namespaces.contains(namespace))
        })
        .collect();
    party_ids.sort();
    party_ids.dedup();

    party_ids
}

/// This node's namespace signing-key fingerprints, from the local vault.
pub(crate) async fn my_namespace_fingerprints(
    vault_client: &mut VaultServiceClient<Channel>,
) -> Result<HashSet<String>> {
    let response = bounded_read(
        "list_my_keys",
        vault_client.list_my_keys(tonic::Request::new(ListMyKeysRequest {
            filters: None,
            base_request: None,
        })),
    )
    .await?
    .into_inner();

    let mut fingerprints = HashSet::new();
    for key_meta in response.private_keys_metadata {
        if let Some(private_key_metadata::PublicKeyWithName::V30(pub_key_with_name)) =
            &key_meta.public_key_with_name
            && let Some(pub_key) = &pub_key_with_name.public_key
            && let Some(public_key::Key::SigningPublicKey(signing_key)) = &pub_key.key
            && signing_key.usage.contains(&1)
        {
            // SigningKeyUsage::Namespace = 1
            fingerprints.insert(utils::compute_fingerprint(signing_key).into());
        }
    }

    Ok(fingerprints)
}

/// The decentralized namespaces this node owns a namespace key in.
///
/// This is the discovery step for a node that holds no local record of its
/// parties. It goes through the namespaces rather than the parties because a
/// party is only selectable by an in-memory participant filter, while a
/// namespace is a store-level predicate.
pub(crate) async fn owned_decentralized_namespaces(
    topology_client: &mut TopologyManagerReadServiceClient<Channel>,
    synchronizer_id: &str,
    my_fingerprints: &HashSet<String>,
) -> Result<Vec<DecentralizedNamespaceDefinition>> {
    let response = bounded_read(
        "list_decentralized_namespace_definition",
        topology_client.list_decentralized_namespace_definition(tonic::Request::new(
            build_decentralized_namespace_request(synchronizer_id, ""),
        )),
    )
    .await?
    .into_inner();

    let mut owned: Vec<_> = response
        .results
        .into_iter()
        .filter_map(|result| {
            let item = result.item?;
            item.owners
                .iter()
                .any(|owner| my_fingerprints.contains(owner))
                .then_some(item)
        })
        .collect();
    owned.sort_by(|a, b| a.decentralized_namespace.cmp(&b.decentralized_namespace));
    owned.dedup_by(|a, b| a.decentralized_namespace == b.decentralized_namespace);

    Ok(owned)
}

/// Pair each decentralized namespace this node owns a key in with every party
/// in it, dropping any party the requested prefix excludes.
///
/// A namespace is derived from its owner set alone, so two parties with the
/// same owners share one and a namespace-scoped query returns both. Keeping one
/// party per namespace silently dropped the other, and left a prefix query
/// answering empty depending on which of them arrived last.
pub(crate) fn pair_namespaces_with_parties(
    namespace_definitions: Vec<DecentralizedNamespaceDefinition>,
    parties_by_namespace: &HashMap<String, Vec<PartyToParticipant>>,
    my_fingerprints: &HashSet<String>,
    prefix_filter: Option<&str>,
) -> Vec<(DecentralizedNamespaceDefinition, String, PartyToParticipant)> {
    let mut paired = Vec::new();

    for definition in namespace_definitions {
        let Some(my_owner_key) = definition
            .owners
            .iter()
            .find(|owner| my_fingerprints.contains(*owner))
            .cloned()
        else {
            continue;
        };
        let Some(parties) = parties_by_namespace.get(&definition.decentralized_namespace) else {
            continue;
        };

        for party in parties {
            // Namespace-scoped discovery does not know about the requested
            // prefix, so apply it to each party the namespace resolved to.
            if !prefix_filter.is_none_or(|prefix| party.party.starts_with(prefix)) {
                continue;
            }
            paired.push((definition.clone(), my_owner_key.clone(), party.clone()));
        }
    }

    paired
}

/// Build the `list_party_to_participant` request used to discover this node's
/// decentralized parties.
///
/// `filter_party` is the only scalable filter here. Canton splits it on `::`
/// and compiles both halves into the store query, while `filter_participant`
/// and `filter_signed_key` are applied in memory after every party has been
/// loaded. So pass an exact party id where one is known. A namespace-only
/// filter matches nothing (see [`discover_hosted_party_ids`]), and `None`
/// sends no party filter at all.
pub(crate) fn build_party_to_participant_request(
    synchronizer_id: &str,
    party_filter: Option<&str>,
    participant_id: &str,
) -> ListPartyToParticipantRequest {
    ListPartyToParticipantRequest {
        base_query: Some(topology::head_state_query(synchronizer_id)),
        filter_party: party_filter.unwrap_or_default().to_string(),
        filter_participant: participant_id.to_string(),
    }
}

/// Build a decentralized-namespace query, for one namespace or for all of them.
///
/// An empty namespace enumerates every decentralized namespace on the
/// synchronizer. Canton pushes the mapping type into the store query, so that
/// reads one small slice of the topology store rather than all of it: a
/// synchronizer holds a handful of decentralized namespaces against a
/// `PartyToParticipant` mapping for every party on it.
pub(crate) fn build_decentralized_namespace_request(
    synchronizer_id: &str,
    namespace: &str,
) -> ListDecentralizedNamespaceDefinitionRequest {
    ListDecentralizedNamespaceDefinitionRequest {
        base_query: Some(topology::head_state_query(synchronizer_id)),
        filter_namespace: namespace.to_string(),
    }
}

/// Union every decentralized party ID the node already knows locally.
///
/// Credentials cover configured parties, workflow runs cover a party while it
/// is being onboarded (before credentials exist), and cached rows preserve
/// discovery after the originating workflow has been dismissed. Keeping all
/// three sources prevents exact-filter discovery from hiding a newly added
/// party on a node that already has credentials for another party.
///
/// The validator operator party's credential is skipped. It exists for the
/// Splice cleanup of an unhost, the party is not decentralized, and as the only
/// exact filter it would stop the discovery of the parties this node hosts.
pub(crate) fn known_party_filters(
    party_credentials: &[PartyCredentials],
    validator_operator_party: Option<&CantonId>,
    workflow_party_ids: impl IntoIterator<Item = String>,
    cached_party_ids: impl IntoIterator<Item = String>,
    prefix_filter: Option<&str>,
) -> Vec<String> {
    let mut parties: Vec<_> = party_credentials
        .iter()
        .filter(|credentials| Some(&credentials.dec_party_id) != validator_operator_party)
        .map(|credentials| credentials.dec_party_id.to_string())
        .chain(workflow_party_ids)
        .chain(cached_party_ids)
        .filter(|party_id| prefix_filter.is_none_or(|prefix| party_id.starts_with(prefix)))
        .collect();
    parties.sort();
    parties.dedup();
    parties
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ListParties` answers with every party the participant hosts, so the
    /// namespace is the only thing separating a decentralized party this node
    /// is a member of from an ordinary party it happens to host.
    #[test]
    fn only_parties_in_an_owned_namespace_are_discovered() {
        let mine = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        let theirs = "1220d5010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5893";
        let owned = HashSet::from([mine.to_string()]);

        let found = parties_in_namespaces(
            [
                format!("beta::{mine}"),
                format!("alpha::{mine}"),
                // Same namespace twice: `ListParties` reports a party once per
                // participant that hosts it.
                format!("alpha::{mine}"),
                // Someone else's decentralized party, and an ordinary party on
                // this participant's own namespace.
                format!("outsider::{theirs}"),
                format!("plain::{theirs}"),
                // Malformed, and a namespace with no identifier half, which
                // Canton would never mint.
                "no-delimiter".to_string(),
                format!("::{mine}"),
            ],
            &owned,
        );

        assert_eq!(
            found,
            vec![format!("alpha::{mine}"), format!("beta::{mine}")],
            "expected the owned namespace's parties, sorted and deduplicated"
        );
    }

    /// Two parties with the same owners share one decentralized namespace, so
    /// a namespace-scoped query returns both. Reporting one of them silently
    /// loses a party the node is a member of.
    #[test]
    fn a_shared_namespace_reports_every_party_in_it() {
        let namespace = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        let mine = "1220aaaa";
        let definition = DecentralizedNamespaceDefinition {
            decentralized_namespace: namespace.to_string(),
            threshold: 1,
            owners: vec![mine.to_string(), "1220bbbb".to_string()],
        };
        let parties_by_namespace = HashMap::from([(
            namespace.to_string(),
            vec![
                a_mapping(&format!("alpha::{namespace}")),
                a_mapping(&format!("beta::{namespace}")),
            ],
        )]);
        let mut fingerprints = HashSet::new();
        fingerprints.insert(mine.to_string());

        let paired = pair_namespaces_with_parties(
            vec![definition.clone()],
            &parties_by_namespace,
            &fingerprints,
            None,
        );

        let found: Vec<_> = paired.iter().map(|(_, _, p2p)| p2p.party.clone()).collect();
        assert_eq!(
            found,
            vec![format!("alpha::{namespace}"), format!("beta::{namespace}")]
        );
        assert!(paired.iter().all(|(_, key, _)| key == mine));

        // The prefix narrows within the namespace, and picks the party by name
        // rather than by whichever mapping arrived last.
        let filtered = pair_namespaces_with_parties(
            vec![definition.clone()],
            &parties_by_namespace,
            &fingerprints,
            Some("beta"),
        );
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].2.party, format!("beta::{namespace}"));

        let partial = pair_namespaces_with_parties(
            vec![definition],
            &parties_by_namespace,
            &fingerprints,
            Some("b"),
        );
        assert_eq!(partial.len(), 1);
        assert_eq!(partial[0].2.party, format!("beta::{namespace}"));
    }

    /// A namespace this node owns no key in is somebody else's party.
    #[test]
    fn a_namespace_without_my_key_is_skipped() {
        let namespace = "1220dddd";
        let definition = DecentralizedNamespaceDefinition {
            decentralized_namespace: namespace.to_string(),
            threshold: 1,
            owners: vec!["1220bbbb".to_string()],
        };
        let parties_by_namespace = HashMap::from([(
            namespace.to_string(),
            vec![a_mapping(&format!("theirs::{namespace}"))],
        )]);

        let paired = pair_namespaces_with_parties(
            vec![definition],
            &parties_by_namespace,
            &HashSet::from(["1220aaaa".to_string()]),
            None,
        );

        assert!(paired.is_empty());
    }

    /// Canton's `ListParties` matches a bare identifier by prefix on a DB
    /// store, so a partial prefix goes to it as is. This pins the request
    /// only; the `create_dec_party` integration phase pins Canton's behavior.
    #[test]
    fn list_parties_request_sends_a_partial_prefix_unchanged() {
        let request = build_list_parties_request(
            vec!["sync::1220abcd".to_string()],
            Some("cb"),
            "participant::abc123",
        );

        assert_eq!(request.filter_party, "cb");
        assert_eq!(request.filter_participant, "participant::abc123");
        assert_eq!(request.synchronizer_ids, vec!["sync::1220abcd".to_string()]);
        assert_eq!(request.limit, MAX_HOSTED_PARTIES);

        let unfiltered = build_list_parties_request(Vec::new(), None, "participant::abc123");
        assert_eq!(unfiltered.filter_party, "");
    }

    fn a_mapping(party: &str) -> PartyToParticipant {
        PartyToParticipant {
            party: party.to_string(),
            threshold: 1,
            participants: Vec::new(),
            party_signing_keys: None,
        }
    }

    /// The physical form carries the protocol version and `ListParties` parses
    /// the field as a logical `SynchronizerId`, so sending the physical one
    /// would be refused outright.
    #[test]
    fn the_logical_synchronizer_id_drops_the_protocol_version() {
        let logical = "global-domain::1220be58c29e65de40bf273be1dc2b266d43a9a002ea5b18955aeef7";

        assert_eq!(
            logical_synchronizer_id(&format!("{logical}::35-5")),
            Some(logical)
        );
        // Already logical, or otherwise not the shape we expect: send no
        // filter rather than a request Canton will reject.
        assert_eq!(logical_synchronizer_id(logical), None);
        assert_eq!(logical_synchronizer_id("nonsense"), None);
    }

    #[test]
    fn party_to_participant_fallback_scopes_to_local_participant() {
        // The no-local-knowledge onboarding fallback has to use an empty party
        // filter, but still post-filters the result to this participant.
        let request =
            build_party_to_participant_request("sync::physical", None, "participant::abc123");

        assert_eq!(request.filter_participant, "participant::abc123");
        assert_eq!(request.filter_party, "");
    }

    #[test]
    fn party_to_participant_request_uses_exact_party_and_participant() {
        let request = build_party_to_participant_request(
            "sync::physical",
            Some("alice::namespace"),
            "participant::abc123",
        );

        assert_eq!(request.filter_participant, "participant::abc123");
        assert_eq!(request.filter_party, "alice::namespace");
    }

    #[test]
    fn decentralized_namespace_request_uses_exact_namespace_filter() {
        let namespace = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        let request = build_decentralized_namespace_request("sync::physical", namespace);

        assert_eq!(request.filter_namespace, namespace);
    }

    /// Both discovery requests read the synchronizer's authorized head state,
    /// the same query every other topology read sends.
    #[test]
    fn discovery_requests_read_the_synchronizer_head_state() {
        let head_state = Some(topology::head_state_query("sync::physical"));

        let party = build_party_to_participant_request("sync::physical", None, "participant::a");
        let namespace = build_decentralized_namespace_request("sync::physical", "");

        assert_eq!(party.base_query, head_state);
        assert_eq!(namespace.base_query, head_state);
    }

    #[test]
    fn known_party_filters_union_all_sources_deduplicate_and_scope() -> anyhow::Result<()> {
        let namespace_a = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        let namespace_b = "1220d5010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5893";
        let namespace_c = "1220e6010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5894";
        let credentials = |party: &str, namespace: &str| -> anyhow::Result<PartyCredentials> {
            Ok(PartyCredentials {
                dec_party_id: CantonId::parse(&format!("{party}::{namespace}"))?,
                member_party_id: CantonId::parse(&format!("member::{namespace}"))?,
                user_id: "user".to_string(),
                keycloak: Default::default(),
                auth0: None,
                packages: Default::default(),
            })
        };
        let configured = credentials("cbtc-configured", namespace_a)?;
        let parties = vec![configured.clone(), configured];
        let workflow_ids = vec![
            format!("cbtc-workflow::{namespace_b}"),
            format!("cbtc-configured::{namespace_a}"),
        ];
        let cached_ids = vec![
            format!("cbtc-cached::{namespace_c}"),
            format!("other-network::{namespace_b}"),
        ];

        assert_eq!(
            known_party_filters(
                &parties,
                None,
                workflow_ids.clone(),
                cached_ids.clone(),
                Some("cbtc-")
            ),
            vec![
                format!("cbtc-cached::{namespace_c}"),
                format!("cbtc-configured::{namespace_a}"),
                format!("cbtc-workflow::{namespace_b}"),
            ]
        );
        assert_eq!(
            known_party_filters(&parties, None, workflow_ids, cached_ids, Some("missing")),
            Vec::<String>::new()
        );
        Ok(())
    }

    /// A node whose only credential is its validator operator's must still
    /// discover the parties it hosts. As an exact filter, that party would make
    /// discovery look for it alone and find nothing decentralized.
    #[test]
    fn the_validator_operator_party_is_not_a_discovery_filter() -> anyhow::Result<()> {
        let namespace = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        let operator = CantonId::parse(&format!("validator::{namespace}"))?;
        let parties = vec![PartyCredentials {
            dec_party_id: operator.clone(),
            member_party_id: operator.clone(),
            user_id: "validator-user".to_string(),
            keycloak: Default::default(),
            auth0: None,
            packages: Default::default(),
        }];

        assert!(
            known_party_filters(&parties, Some(&operator), Vec::new(), Vec::new(), None).is_empty()
        );
        assert_eq!(
            known_party_filters(&parties, None, Vec::new(), Vec::new(), None),
            vec![operator.to_string()]
        );
        Ok(())
    }
}
