//! This node's owner keys and party signing keys, read from Canton.

use std::collections::{HashMap, HashSet};

use sqlx::SqlitePool;

use canton_proto_rs::com::digitalasset::canton::{
    crypto::{
        admin::v30::{
            ListMyKeysRequest, private_key_metadata, vault_service_client::VaultServiceClient,
        },
        v30::{SigningKeyUsage, public_key},
    },
    topology::admin::v30::{
        BaseQuery, ListDecentralizedNamespaceDefinitionRequest, StoreId, Synchronizer, base_query,
        store_id, synchronizer,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};

use crate::{
    canton_id::{CantonId, Fingerprint},
    config::NodeConfig,
    error::Result,
    utils::{self, compute_fingerprint},
    workflow,
};

/// Query Canton for this node's owner keys across a caller-supplied set of
/// decentralized parties. Returns JSON:
/// `[{"party_id": "prefix::namespace", "owner_key": "fingerprint",
/// "signing_key": "fingerprint"}, ...]`.
///
/// `signing_key` is this node's Daml (protocol) signing key for the party —
/// its entry in `PartyToParticipant.party_signing_keys`. That mapping exists
/// nowhere else: the protobuf records no owner per key and the key is not
/// delegated in topology, so only the node that generated it can say it is
/// theirs. Kick needs it to drop the removed member's key. `null` when this
/// node has no such key for the party, which the reader treats the same way
/// as the absent field it gets from a caller's older peers.
///
/// `requested_party_ids` is the list of parties the caller cares about, sent
/// in the Noise `RequestOwnerKeys` payload by `resolve_owner_keys_from_peers`.
/// Building the `namespace → party_id` map directly from this list lets us
/// skip the unfiltered `list_party_to_participant` call that previously
/// scanned every party on the synchronizer — that scan does not complete
/// within the Noise budget against a kubectl-tunneled Canton admin API on
/// devnet (~170 parties, never returns within 60s; see #149).
pub(super) async fn list_my_owner_keys(
    config: &NodeConfig,
    db: &SqlitePool,
    requested_party_ids: &[String],
) -> Result<Vec<u8>> {
    if requested_party_ids.is_empty() {
        return Ok(b"[]".to_vec());
    }

    // Caller provides full party_ids ("prefix::namespace_fingerprint"). The
    // namespace is the suffix after the final "::"; we use it to look up the
    // matching `DecentralizedNamespaceDefinition` entry. Party IDs without
    // "::" are dropped (malformed; nothing we could match against).
    let namespace_to_party: HashMap<String, &str> = requested_party_ids
        .iter()
        .filter_map(|pid| {
            pid.rsplit_once("::")
                .map(|(_, ns)| (ns.to_string(), pid.as_str()))
        })
        .collect();
    if namespace_to_party.is_empty() {
        return Ok(b"[]".to_vec());
    }

    let channel = config.admin_channel().await?;

    let mut vault_client = VaultServiceClient::new(channel.clone());
    let mut topology_client = TopologyManagerReadServiceClient::new(channel);

    // Get this node's namespace key fingerprints
    let keys_response = vault_client
        .list_my_keys(tonic::Request::new(ListMyKeysRequest {
            filters: None,
            base_request: None,
        }))
        .await?
        .into_inner();

    let mut my_fingerprints: Vec<String> = Vec::new();
    // Every protocol key this node holds, by fingerprint. The by-name map
    // below only finds a key minted under this tool's naming convention; a
    // party old enough to predate it can hold the key under any name, and
    // then only the fingerprint identifies it.
    let mut my_protocol_fingerprints: HashSet<Fingerprint> = HashSet::new();
    // Protocol-usage keys, with the key name onboarding, add-party or
    // rotate-key gave them (`<party prefix>-daml-transactions...`). The name is how
    // `get_or_create_signing_key` finds the key again on a retry, so it is
    // also the only local record of which party a protocol key belongs to.
    // A key DecMan generated carries the same kind of name, and a vault key
    // may share it, so two entries can have one name.
    let mut my_protocol_keys_by_name: Vec<(String, Fingerprint)> = Vec::new();
    for key_meta in keys_response.private_keys_metadata {
        if let Some(private_key_metadata::PublicKeyWithName::V30(pub_key_with_name)) =
            &key_meta.public_key_with_name
            && let Some(pub_key) = &pub_key_with_name.public_key
            && let Some(public_key::Key::SigningPublicKey(signing_key)) = &pub_key.key
        {
            if signing_key
                .usage
                .contains(&(SigningKeyUsage::Namespace as i32))
            {
                my_fingerprints.push(compute_fingerprint(signing_key).into());
            }
            if signing_key
                .usage
                .contains(&(SigningKeyUsage::Protocol as i32))
            {
                my_protocol_keys_by_name.push((
                    pub_key_with_name.name.clone(),
                    compute_fingerprint(signing_key),
                ));
                my_protocol_fingerprints.insert(compute_fingerprint(signing_key));
            }
        }
    }
    match generated_party_keys(config, db).await {
        Ok(generated) => {
            for (name, fingerprint) in generated {
                my_protocol_fingerprints.insert(fingerprint.clone());
                my_protocol_keys_by_name.push((name, fingerprint));
            }
        }
        Err(e) => tracing::warn!("Cannot list the Daml keys DecMan generated: {e:#}"),
    }

    // Cached after first call (`get_synchronizer_id` memoises via OnceCell).
    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    let base_query = BaseQuery {
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id)),
            })),
        }),
        proposals: false,
        operation: 0,
        time_query: Some(base_query::TimeQuery::HeadState(())),
        filter_signed_key: String::new(),
        protocol_version: None,
        client_version: None,
    };

    // Get all decentralized namespace definitions. The response is bounded by
    // the number of decentralized parties allocated on the synchronizer
    // (49 entries observed on devnet — completes in <1s).
    let dns_response = topology_client
        .list_decentralized_namespace_definition(tonic::Request::new(
            ListDecentralizedNamespaceDefinitionRequest {
                base_query: Some(base_query),
                filter_namespace: String::new(),
            },
        ))
        .await?
        .into_inner();

    // Match this node's fingerprints against each party's owners list, but
    // only for namespaces the caller asked about.
    let mut entries = Vec::new();
    for result in dns_response.results {
        let Some(item) = result.item else { continue };
        let Some(full_party_id) = namespace_to_party.get(&item.decentralized_namespace) else {
            continue;
        };
        let owned: Vec<&String> = item
            .owners
            .iter()
            .filter(|owner| my_fingerprints.contains(owner))
            .collect();
        if owned.is_empty() {
            continue;
        }

        // A rotation leaves the old key in the vault under the onboarding
        // name, next to the new one under its own name. Only the party's key
        // list says which one is in force, so two named keys go to the
        // fingerprint match below.
        let named = full_party_id.rsplit_once("::").and_then(|(prefix, _)| {
            let mut named = my_protocol_keys_by_name
                .iter()
                .filter(|(name, _)| workflow::signing_keys::is_party_daml_key_name(name, prefix));
            match (named.next(), named.next()) {
                (Some((_, fingerprint)), None) => Some(fingerprint.clone()),
                _ => None,
            }
        });
        // Falling back to the fingerprint matters for the parties that need it
        // most: a peer that reports no key cannot be attributed by a
        // coordinator, and no amount of refreshing repairs that.
        let signing_key = match named {
            Some(fingerprint) => Some(fingerprint),
            None => {
                party_signing_key_by_fingerprint(config, full_party_id, &my_protocol_fingerprints)
                    .await
            }
        };

        for owner in owned {
            entries.push(serde_json::json!({
                "party_id": full_party_id,
                "owner_key": owner,
                "signing_key": signing_key,
            }));
        }
    }

    Ok(serde_json::to_vec(&entries)?)
}

/// The name and fingerprint of each Daml key DecMan generated, or none when
/// the key store is off.
async fn generated_party_keys(
    config: &NodeConfig,
    db: &SqlitePool,
) -> Result<Vec<(String, Fingerprint)>> {
    match crate::signing::key_store::KeyStore::open(config, db.clone()).await? {
        Some(store) => store.generated_keys().await,
        None => Ok(Vec::new()),
    }
}

/// This node's Daml signing key for a party, identified by fingerprint rather
/// than by vault name: the one key in the party's on-chain key list whose
/// private half this node holds.
///
/// `mine` is every protocol-usage fingerprint in this node's vault, and every
/// key DecMan generated. Answers
/// `None` when the party's keys cannot be read or none of them is ours; the
/// caller then reports no key, exactly as it did before this fallback existed.
async fn party_signing_key_by_fingerprint(
    config: &NodeConfig,
    full_party_id: &str,
    mine: &HashSet<Fingerprint>,
) -> Option<Fingerprint> {
    let resolve = async {
        let party_id = CantonId::parse(full_party_id)?;
        let synchronizer_id = utils::get_synchronizer_id(config).await?;
        let mut keys = workflow::topology::fetch_p2p_mapping(config, &synchronizer_id, &party_id)
            .await?
            .party_signing_keys
            .map(|k| k.keys)
            .unwrap_or_default();
        if keys.is_empty() {
            keys =
                workflow::topology::fetch_party_to_key_mapping(config, &synchronizer_id, &party_id)
                    .await?
                    .map(|mapping| mapping.signing_keys)
                    .unwrap_or_default();
        }
        Ok::<_, anyhow::Error>(
            keys.iter()
                .map(compute_fingerprint)
                .find(|fingerprint| mine.contains(fingerprint)),
        )
    };

    match resolve.await {
        Ok(Some(fingerprint)) => {
            tracing::info!(
                "Reporting Daml signing key {fingerprint} for {full_party_id}: matched by \
                 fingerprint, not by vault name"
            );
            Some(fingerprint)
        }
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(
                "Cannot match a vault key against {full_party_id}'s signing keys: {e:#}"
            );
            None
        }
    }
}
