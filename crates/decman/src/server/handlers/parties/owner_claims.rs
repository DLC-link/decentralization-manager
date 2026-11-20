//! Owner keys from topology, and the owner claims kept in the cache.

use std::collections::{HashMap, HashSet};

use canton_proto_rs::com::digitalasset::canton::topology::admin::v30::{
    BaseQuery, ListNamespaceDelegationRequest, StoreId, Synchronizer, base_query,
    list_namespace_delegation_response::result::Item as NsDelegationItem, store_id, synchronizer,
    topology_manager_read_service_client::TopologyManagerReadServiceClient,
};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    db::{
        rows::DecPartyOwnerRow,
        schema::{Commitable, SchemaRead, SchemaWrite},
    },
    error::Result,
    server::types::{DecentralizedParty, NonHostingOwner, ParticipantInfo},
    utils,
};

/// Fills in owner keys that this node lacks from public Canton topology. For
/// each participant without a known owner key, it lists the
/// `NamespaceDelegation` entries in the participant's namespace. It then
/// matches their target keys against the party's owners.
///
/// The match succeeds only when an owner is the participant's namespace key
/// or a key delegated under it. decman creates each owner key as the root of
/// its own namespace, so this resolves nothing for a party that decman
/// created. It covers a party whose owners are the members' participant
/// namespaces, such as one set up with the Canton console.
pub(super) async fn supplement_owner_keys_from_topology(
    config: &NodeConfig,
    db: &SqlitePool,
    parties: &[DecentralizedParty],
) -> Result {
    let channel = config.admin_channel().await?;
    let mut topology_client = TopologyManagerReadServiceClient::new(channel)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);
    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    let base_query = || BaseQuery {
        store: Some(StoreId {
            store: Some(store_id::Store::Synchronizer(Synchronizer {
                kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.clone())),
            })),
        }),
        proposals: false,
        operation: 0,
        time_query: Some(base_query::TimeQuery::HeadState(())),
        filter_signed_key: String::new(),
        protocol_version: None,
        client_version: None,
    };

    // Cache per-namespace fingerprints so a participant who appears in many
    // parties is only queried once.
    let mut delegated_fingerprints: HashMap<String, HashSet<String>> = HashMap::new();
    let mut delegated = async |namespace: &str| -> HashSet<String> {
        if let Some(known) = delegated_fingerprints.get(namespace) {
            return known.clone();
        }
        let mut fingerprints: HashSet<String> = HashSet::new();
        match topology_client
            .list_namespace_delegation(tonic::Request::new(ListNamespaceDelegationRequest {
                base_query: Some(base_query()),
                filter_namespace: namespace.to_string(),
                filter_target_key_fingerprint: String::new(),
            }))
            .await
        {
            Ok(resp) => {
                for result in resp.into_inner().results {
                    if let Some(NsDelegationItem::V30(item)) = result.item
                        && let Some(target_key) = item.target_key
                    {
                        fingerprints.insert(utils::compute_fingerprint(&target_key).into());
                    }
                }
            }
            // Cached empty so we don't retry on every party.
            Err(e) => tracing::debug!("ListNamespaceDelegation for {namespace} failed: {e}"),
        }
        delegated_fingerprints.insert(namespace.to_string(), fingerprints.clone());
        fingerprints
    };

    for party in parties {
        for participant in &party.participants {
            // Already known — nothing to derive.
            if participant.owner_key.is_some() {
                continue;
            }
            let uid = participant.participant_uid.to_string();
            let fingerprints = delegated(&participant.participant_uid.namespace.to_hex()).await;
            let Some(owner_key) = party.owners.iter().find(|o| fingerprints.contains(*o)) else {
                continue;
            };

            let mut tx = match db.begin_transaction().await {
                Ok(t) => t,
                Err(e) => {
                    tracing::debug!("Topology fallback: begin_transaction failed: {e}");
                    continue;
                }
            };
            match tx
                .update_participant_owner_key(&party.party_id, &uid, owner_key)
                .await
            {
                Ok(None) => {}
                Ok(Some(refusal)) => {
                    tracing::debug!(
                        "Topology fallback: {uid} keeps no key {owner_key}: {refusal:?}"
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        "Topology fallback: update_participant_owner_key for {uid}: {e}"
                    );
                    continue;
                }
            }
            if let Err(e) = Commitable::commit(tx).await {
                tracing::debug!("Topology fallback: commit failed: {e}");
            }
        }
    }

    // An owner that hosts nothing has no participant row to fill, so its
    // claim goes on the owner instead. Only configured peers are looked up:
    // they are the participants this node can name.
    let peers = db.get_all_peers().await?;
    let mut claims: Vec<(&CantonId, &String, String)> = Vec::new();
    for party in parties {
        if party.owners.len() <= party.participants.len() {
            continue;
        }
        let hosts: HashSet<&CantonId> = party
            .participants
            .iter()
            .map(|p| &p.participant_uid)
            .collect();
        for peer in peers.iter().filter(|p| !hosts.contains(&p.participant_id)) {
            let fingerprints = delegated(&peer.participant_id.namespace.to_hex()).await;
            for owner in party.owners.iter().filter(|o| fingerprints.contains(*o)) {
                claims.push((&party.party_id, owner, peer.participant_id.to_string()));
            }
        }
    }
    if !claims.is_empty() {
        let mut tx = db.begin_transaction().await?;
        for (party_id, owner_key, uid) in claims {
            if let Some(refusal) = tx.update_owner_claim(party_id, owner_key, &uid).await? {
                tracing::debug!("Topology fallback: {uid} claims no key {owner_key}: {refusal:?}");
            }
        }
        Commitable::commit(tx).await?;
    }
    Ok(())
}

/// The participant that claimed each owner key, by key.
pub(super) fn owner_claims(rows: Vec<DecPartyOwnerRow>) -> HashMap<String, CantonId> {
    rows.into_iter()
        .filter_map(|row| {
            let uid = CantonId::parse(row.participant_uid.as_deref()?).ok()?;
            Some((row.owner_key, uid))
        })
        .collect()
}

/// The party's owners that host nothing, as far as this node can tell.
///
/// An owner claimed by a participant that does not host is one. An unclaimed
/// owner is one only once every host's own key is known and listed, because
/// until then it may belong to a host whose answer has not arrived.
pub(super) fn non_hosting_owners(
    owners: &[String],
    participants: &[ParticipantInfo],
    claims: &HashMap<String, CantonId>,
) -> Vec<NonHostingOwner> {
    let host_keys: HashSet<&str> = participants
        .iter()
        .filter_map(|p| p.owner_key.as_deref())
        .collect();
    let hosts: HashSet<&CantonId> = participants.iter().map(|p| &p.participant_uid).collect();
    let every_host_known = participants
        .iter()
        .all(|p| p.owner_key.as_ref().is_some_and(|key| owners.contains(key)));
    owners
        .iter()
        .filter(|owner| !host_keys.contains(owner.as_str()))
        .filter_map(|owner| match claims.get(owner) {
            Some(claimant) if hosts.contains(claimant) => None,
            Some(claimant) => Some(NonHostingOwner {
                owner_key: owner.clone(),
                participant_uid: Some(claimant.clone()),
            }),
            None if every_host_known => Some(NonHostingOwner {
                owner_key: owner.clone(),
                participant_uid: None,
            }),
            None => None,
        })
        .collect()
}

/// Fill in from the cache what a live Canton read cannot know: the owner key
/// each other host reported, and the participant that claimed each owner.
///
/// A cached key that is no longer an owner stays out. The host may have
/// rotated its key, and a filled-in key stops the topology fallback from
/// looking up the new one.
pub(super) async fn with_cached_owner_claims(
    db: &SqlitePool,
    party_id: &CantonId,
    owners: &[String],
    mut participants: Vec<ParticipantInfo>,
) -> (Vec<ParticipantInfo>, Vec<NonHostingOwner>) {
    let cached_keys: HashMap<String, String> = match db.get_dec_party_participants(party_id).await {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| Some((row.participant_uid, row.owner_key?)))
            .collect(),
        Err(e) => {
            tracing::debug!("Failed to read the cached owner keys of {party_id}: {e}");
            HashMap::new()
        }
    };
    for participant in &mut participants {
        if participant.owner_key.is_none() {
            participant.owner_key = cached_keys
                .get(&participant.participant_uid.to_string())
                .filter(|key| owners.contains(*key))
                .cloned();
        }
    }
    let claims = match db.get_dec_party_owners(party_id).await {
        Ok(rows) => owner_claims(rows),
        Err(e) => {
            tracing::debug!("Failed to read the cached owner claims of {party_id}: {e}");
            HashMap::new()
        }
    };
    let non_hosting = non_hosting_owners(owners, &participants, &claims);
    (participants, non_hosting)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::MIGRATOR;
    use crate::{
        db::rows::{DecPartyParticipantRow, DecPartyRow},
        server::types::Permission,
    };

    fn uid(tag: u8) -> anyhow::Result<CantonId> {
        let ns = format!("1220{:0>64}", format!("{tag:02x}"));
        CantonId::parse(&format!("p{tag}::{ns}"))
    }

    fn host(tag: u8, owner_key: Option<&str>) -> anyhow::Result<ParticipantInfo> {
        Ok(ParticipantInfo {
            participant_uid: uid(tag)?,
            permission: Permission::Confirmation,
            owner_key: owner_key.map(str::to_string),
        })
    }

    fn keys(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    /// An owner nobody hosts with shows up, named by whoever claimed it.
    #[test]
    fn an_owner_no_host_holds_is_listed() -> anyhow::Result<()> {
        let hosts = [host(1, Some("k1"))?, host(2, Some("k2"))?];
        let claims: HashMap<String, CantonId> = [("k3".to_string(), uid(3)?)].into();

        let listed = non_hosting_owners(&keys(&["k1", "k2", "k3", "k4"]), &hosts, &claims);

        assert_eq!(
            listed,
            vec![
                NonHostingOwner {
                    owner_key: "k3".to_string(),
                    participant_uid: Some(uid(3)?),
                },
                NonHostingOwner {
                    owner_key: "k4".to_string(),
                    participant_uid: None,
                },
            ]
        );
        Ok(())
    }

    /// While a host's key is unknown, an unclaimed owner may be that host's,
    /// so only owners that a non-host claimed are listed.
    #[test]
    fn an_unclaimed_owner_waits_for_every_host_key() -> anyhow::Result<()> {
        let hosts = [host(1, Some("k1"))?, host(2, None)?];
        let claims: HashMap<String, CantonId> = [("k3".to_string(), uid(3)?)].into();

        let listed = non_hosting_owners(&keys(&["k1", "k2", "k3"]), &hosts, &claims);

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].owner_key, "k3");
        Ok(())
    }

    /// A cached key that is no longer an owner says nothing about the host's
    /// current key, so it does not count as known.
    #[test]
    fn a_stale_host_key_does_not_count_as_known() -> anyhow::Result<()> {
        let hosts = [host(1, Some("k1"))?, host(2, Some("old"))?];

        let listed = non_hosting_owners(&keys(&["k1", "k2"]), &hosts, &HashMap::new());

        assert!(listed.is_empty(), "listed {listed:?}");
        Ok(())
    }

    /// A claim by a host is that host's key, even when the host row has not
    /// learned it yet.
    #[test]
    fn a_key_a_host_claimed_is_not_listed() -> anyhow::Result<()> {
        let hosts = [host(1, Some("k1"))?, host(2, None)?];
        let claims: HashMap<String, CantonId> = [("k2".to_string(), uid(2)?)].into();

        let listed = non_hosting_owners(&keys(&["k1", "k2"]), &hosts, &claims);

        assert!(listed.is_empty(), "listed {listed:?}");
        Ok(())
    }

    /// The live read knows only this node's key. The cache fills in the other
    /// hosts' keys and the claims, so the live answer lists the same owners
    /// as a cached one.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_live_read_lists_owners_from_the_cache(pool: SqlitePool) -> anyhow::Result<()> {
        let party_id = CantonId::parse(&format!("net-a::1220{}", "ab".repeat(32)))?;
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&DecPartyRow {
            party_id: party_id.to_string(),
            prefix: "net-a".to_string(),
            threshold: 2,
            updated_at: 0,
            my_owner_key: Some("k1".to_string()),
            confirmation_threshold: None,
        })
        .await?;
        tx.replace_dec_party_participants(
            &party_id,
            &[DecPartyParticipantRow {
                dec_party_id: party_id.to_string(),
                participant_uid: uid(2)?.to_string(),
                permission: "confirmation".to_string(),
                owner_key: Some("k2".to_string()),
                signing_key: None,
            }],
        )
        .await?;
        tx.replace_dec_party_owners(&party_id, &keys(&["k1", "k2", "k3"]))
            .await?;
        tx.update_owner_claim(&party_id, "k3", &uid(3)?.to_string())
            .await?;
        Commitable::commit(tx).await?;

        let live = vec![host(1, Some("k1"))?, host(2, None)?];
        let (participants, listed) =
            with_cached_owner_claims(&pool, &party_id, &keys(&["k1", "k2", "k3"]), live).await;

        assert_eq!(participants[1].owner_key.as_deref(), Some("k2"));
        assert_eq!(
            listed,
            vec![NonHostingOwner {
                owner_key: "k3".to_string(),
                participant_uid: Some(uid(3)?),
            }]
        );
        Ok(())
    }

    /// Host 2 rotated its owner key from k2 to k4, and the cache still holds
    /// k2. The live read leaves host 2's key unknown, so the topology fallback
    /// looks it up, and the unclaimed k4 is not listed as hosting nothing.
    #[sqlx::test(migrator = "MIGRATOR")]
    async fn a_live_read_drops_a_cached_key_that_is_no_longer_an_owner(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let party_id = CantonId::parse(&format!("net-a::1220{}", "ab".repeat(32)))?;
        let mut tx = pool.begin_transaction().await?;
        tx.upsert_dec_party(&DecPartyRow {
            party_id: party_id.to_string(),
            prefix: "net-a".to_string(),
            threshold: 2,
            updated_at: 0,
            my_owner_key: Some("k1".to_string()),
            confirmation_threshold: None,
        })
        .await?;
        tx.replace_dec_party_participants(
            &party_id,
            &[DecPartyParticipantRow {
                dec_party_id: party_id.to_string(),
                participant_uid: uid(2)?.to_string(),
                permission: "confirmation".to_string(),
                owner_key: Some("k2".to_string()),
                signing_key: None,
            }],
        )
        .await?;
        tx.replace_dec_party_owners(&party_id, &keys(&["k1", "k4"]))
            .await?;
        Commitable::commit(tx).await?;

        let live = vec![host(1, Some("k1"))?, host(2, None)?];
        let (participants, listed) =
            with_cached_owner_claims(&pool, &party_id, &keys(&["k1", "k4"]), live).await;

        assert_eq!(participants[1].owner_key, None);
        assert_eq!(listed, Vec::new());
        Ok(())
    }
}
