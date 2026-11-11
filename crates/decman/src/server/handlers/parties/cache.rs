//! Loading and storing the party cache.

use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

use sqlx::SqlitePool;

use super::owner_claims::{non_hosting_owners, owner_claims};
use crate::{
    canton_id::CantonId,
    db::{
        rows::{DecPartyContractRow, DecPartyOwnerRow, DecPartyParticipantRow, DecPartyRow},
        schema::{Commitable, SchemaRead, SchemaWrite},
    },
    error::Result,
    server::{
        queries::sort_contracts,
        types::{
            ContractInfo, DecentralizedPartiesResponse, DecentralizedParty, ParticipantInfo,
            Permission, ResponseSource,
        },
    },
};

/// Load cached parties from the dec_party tables.
/// Returns the response and the newest `updated_at` timestamp (unix seconds).
pub(super) async fn load_cached_parties(
    db: &SqlitePool,
    prefix: &str,
) -> Result<Option<(DecentralizedPartiesResponse, i64)>> {
    let rows = db.get_dec_parties_by_prefix(prefix).await?;
    if rows.is_empty() {
        return Ok(None);
    }

    // Bulk-fetch all related data in 3 queries instead of 3*N
    let all_owners = db.get_all_dec_party_owners(prefix).await?;
    let all_participants = db.get_all_dec_party_participants(prefix).await?;
    let all_contracts = db.get_all_dec_party_contracts(prefix).await?;

    // Group by party_id
    let mut owners_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut claims_map: HashMap<String, Vec<DecPartyOwnerRow>> = HashMap::new();
    for row in all_owners {
        owners_map
            .entry(row.dec_party_id.clone())
            .or_default()
            .push(row.owner_key.clone());
        claims_map
            .entry(row.dec_party_id.clone())
            .or_default()
            .push(row);
    }

    let mut participants_map: HashMap<String, Vec<ParticipantInfo>> = HashMap::new();
    for p in all_participants {
        if let Ok(uid) = CantonId::parse(&p.participant_uid) {
            participants_map
                .entry(p.dec_party_id.clone())
                .or_default()
                .push(ParticipantInfo {
                    participant_uid: uid,
                    permission: match p.permission.as_str() {
                        "submission" => Permission::Submission,
                        "confirmation" => Permission::Confirmation,
                        "observation" => Permission::Observation,
                        _ => Permission::Unknown,
                    },
                    owner_key: p.owner_key,
                });
        }
    }

    let mut contracts_map: HashMap<String, Vec<ContractInfo>> = HashMap::new();
    for c in all_contracts {
        contracts_map
            .entry(c.dec_party_id.clone())
            .or_default()
            .push(ContractInfo {
                contract_id: c.contract_id,
                template_id: c.template_id,
                package_id: c.package_id,
                package_name: c.package_name,
                package_version: c.package_version,
                created_at: c.created_at,
            });
    }
    for list in contracts_map.values_mut() {
        sort_contracts(list);
    }

    let max_updated_at = rows.iter().map(|r| r.updated_at).max().unwrap_or(0);

    let mut parties = Vec::with_capacity(rows.len());
    for row in rows {
        let owners = owners_map.remove(&row.party_id).unwrap_or_default();
        let participants = participants_map.remove(&row.party_id).unwrap_or_default();
        let claims = owner_claims(claims_map.remove(&row.party_id).unwrap_or_default());
        parties.push(DecentralizedParty {
            party_id: CantonId::parse(&row.party_id)?,
            threshold: row.threshold as i32,
            confirmation_threshold: row.confirmation_threshold.map(|t| t as i32),
            non_hosting_owners: non_hosting_owners(&owners, &participants, &claims),
            owners,
            my_owner_key: row.my_owner_key,
            participants,
            contracts: contracts_map.remove(&row.party_id).unwrap_or_default(),
            local_metadata: None,
        });
    }

    Ok(Some((
        DecentralizedPartiesResponse {
            total: parties.len(),
            parties,
            source: ResponseSource::Cache,
            refreshing: false,
        },
        max_updated_at,
    )))
}

/// Store parties into the dec_party tables
pub(super) async fn store_parties_to_db(
    db: &SqlitePool,
    prefix: &str,
    parties: &[DecentralizedParty],
) -> Result {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let mut tx = db.begin_transaction().await?;
    let fresh_party_ids: Vec<String> = parties.iter().map(|p| p.party_id.to_string()).collect();

    for party in parties {
        // Extract the real prefix from party_id (everything before "::")
        let party_id_str = party.party_id.to_string();
        let real_prefix = party_id_str
            .split_once("::")
            .map(|(p, _)| p)
            .unwrap_or(&party_id_str);

        let row = DecPartyRow {
            party_id: party_id_str.clone(),
            prefix: real_prefix.to_string(),
            threshold: party.threshold as i64,
            updated_at: now,
            my_owner_key: party.my_owner_key.clone(),
            confirmation_threshold: party.confirmation_threshold.map(i64::from),
        };
        tx.upsert_dec_party(&row).await?;

        tx.replace_dec_party_owners(&party.party_id, &party.owners)
            .await?;

        let participants: Vec<DecPartyParticipantRow> = party
            .participants
            .iter()
            .map(|p| DecPartyParticipantRow {
                dec_party_id: row.party_id.clone(),
                participant_uid: p.participant_uid.to_string(),
                permission: match p.permission {
                    Permission::Submission => "submission",
                    Permission::Confirmation => "confirmation",
                    Permission::Observation => "observation",
                    Permission::Unknown => "unknown",
                }
                .to_string(),
                owner_key: p.owner_key.clone(),
                // Reported by the participant itself over the OwnerKeys
                // exchange, never carried on the live Canton fetch — the
                // upsert COALESCEs a cached value rather than clearing it.
                signing_key: None,
            })
            .collect();
        tx.replace_dec_party_participants(&party.party_id, &participants)
            .await?;

        let contracts: Vec<DecPartyContractRow> = party
            .contracts
            .iter()
            .map(|c| DecPartyContractRow {
                dec_party_id: row.party_id.clone(),
                contract_id: c.contract_id.clone(),
                template_id: c.template_id.clone(),
                package_id: c.package_id.clone(),
                package_name: c.package_name.clone(),
                package_version: c.package_version.clone(),
                created_at: c.created_at.clone(),
            })
            .collect();
        tx.replace_dec_party_contracts(&party.party_id, &contracts)
            .await?;
    }

    // Remove stale parties no longer returned by Canton
    tx.delete_stale_dec_parties(prefix, &fresh_party_ids)
        .await?;

    Commitable::commit(tx).await
}
