//! Parties this participant used to host, and the purge of the contracts it
//! still holds for them.
//!
//! A participant that leaves a party's hosting mapping, by a kick or on its
//! own, keeps every contract it held for the party. From that moment it no
//! longer sees the party's transactions, so archives made elsewhere never reach
//! it: those contracts stay active on it for good, and its ACS commitments with
//! the remaining hosts stay out of step.
//!
//! The purge follows the devnet recovery of 2026-10-02. It removes a contract
//! only when no party the participant still hosts is a stakeholder of it. A
//! contract another local party sees is still current, because the participant
//! keeps receiving its archive through that party, and Canton's party
//! replication never puts such a contract back once it is gone.

use std::{
    collections::{BTreeSet, HashSet},
    time::Duration,
};

use canton_proto_rs::com::digitalasset::canton::{
    admin::participant::v30::{
        DisconnectSynchronizerRequest, ListConnectedSynchronizersRequest, PurgeContractsRequest,
        ReconnectSynchronizerRequest,
        participant_repair_service_client::ParticipantRepairServiceClient,
        synchronizer_connectivity_service_client::SynchronizerConnectivityServiceClient,
    },
    protocol::v30::enums::TopologyChangeOp,
    topology::admin::v30::{
        ListPartyToParticipantRequest, list_party_to_participant_response::result::Item as P2pItem,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use futures::{StreamExt, TryStreamExt};
use sqlx::SqlitePool;
use tonic::transport::Channel;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    db::{
        rows::FormerDecPartyRow,
        schema::{SchemaRead, SchemaWrite},
    },
    error::Result,
    server::{
        event_filters::{party_event_format, wildcard_filter},
        ledger_paging::fetch_active_contracts_filtered,
        types::{FormerParty, PurgeStatus},
    },
    utils,
    workflow::{
        party_replication::acs::{synchronizer_healthy, verify_healthy},
        topology::{head_state_query, history_query},
    },
};

/// How long the removal must have been in force before a purge.
///
/// The participant has to process every event up to its removal before it
/// leaves the synchronizer. Until then it may still receive the archive of a
/// contract the purge removes, and replaying that archive on reconnect crashes
/// the participant (exit 117).
const MIN_REMOVAL_AGE: Duration = Duration::from_secs(60);

/// Contract ids per `PurgeContracts` call. A contract id is about 140
/// characters, so this keeps each request far below the 4 MiB gRPC cap.
const PURGE_BATCH: usize = 500;

/// Attempts per batch. The call is idempotent with `ignore_already_purged`.
const PURGE_BATCH_ATTEMPTS: usize = 3;

const PURGE_RETRY_DELAY: Duration = Duration::from_secs(2);

/// Hosting reads in flight at once while classifying stakeholders.
const HOSTING_READ_CONCURRENCY: usize = 8;

/// The error a purge that a restart cut off is left with.
pub(crate) const INTERRUPTED_PURGE_ERROR: &str = "decman restarted while this purge was \
     running. The participant may still be disconnected from its synchronizer: check it, \
     reconnect it if needed, then run the purge again";

/// One serial of a party's hosting mapping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HostingEntry {
    pub serial: i32,
    /// Seconds since the epoch.
    pub valid_from: i64,
    /// Whether this serial lists the participant as a host. A `Remove` of the
    /// whole mapping hosts nobody.
    pub hosts_participant: bool,
}

/// When the participant stopped hosting the party, in seconds since the epoch.
///
/// That is when the first serial after the last one listing it took effect.
/// `None` when the latest serial still lists it, or when no serial ever did.
pub(crate) fn removal_time(history: &[HostingEntry]) -> Option<i64> {
    let mut sorted: Vec<&HostingEntry> = history.iter().collect();
    sorted.sort_by_key(|entry| entry.serial);
    let last_hosted = sorted.iter().rposition(|entry| entry.hosts_participant)?;
    sorted.get(last_hosted + 1).map(|entry| entry.valid_from)
}

/// Every serial of `party`'s hosting mapping, as seen by this participant.
async fn hosting_history(
    config: &NodeConfig,
    synchronizer_id: &str,
    party: &CantonId,
) -> Result<Vec<HostingEntry>> {
    let participant_uid = config.participant_id().to_string();
    let party_id = party.to_string();
    let response = TopologyManagerReadServiceClient::new(config.admin_channel().await?)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE)
        .list_party_to_participant(tonic::Request::new(ListPartyToParticipantRequest {
            base_query: Some(history_query(synchronizer_id)),
            filter_party: party_id.clone(),
            filter_participant: String::new(),
        }))
        .await?
        .into_inner();

    // `filter_party` matches on a prefix, so a longer party id can come back.
    Ok(response
        .results
        .into_iter()
        .filter_map(|result| {
            let context = result.context?;
            let P2pItem::V30(mapping) = result.item?;
            (mapping.party == party_id).then(|| HostingEntry {
                serial: context.serial,
                valid_from: context.valid_from.map(|t| t.seconds).unwrap_or_default(),
                hosts_participant: context.operation == TopologyChangeOp::AddReplace as i32
                    && mapping
                        .participants
                        .iter()
                        .any(|p| p.participant_uid == participant_uid),
            })
        })
        .collect())
}

/// When this participant stopped hosting `party`, read live from its own
/// topology store. `None` while it still hosts the party.
async fn live_removal_time(
    config: &NodeConfig,
    synchronizer_id: &str,
    party: &CantonId,
) -> Result<Option<i64>> {
    Ok(removal_time(
        &hosting_history(config, synchronizer_id, party).await?,
    ))
}

/// Record the parties a discovery did not return that this participant has
/// stopped hosting, and forget the former parties it returned again.
///
/// `unreturned` are the parties this node knew before the discovery and did
/// not get back. A party that is merely missing is never recorded: a failed
/// read also returns nothing. Only a hosting history that lists this
/// participant, followed by a serial that does not, counts as a removal.
pub(crate) async fn reconcile(
    config: &NodeConfig,
    db: &SqlitePool,
    unreturned: &[String],
    returned: &[String],
) {
    if let Err(e) = db.forget_former_dec_parties(returned).await {
        tracing::warn!("Could not clear former parties this node hosts again: {e}");
    }

    let recorded: HashSet<String> = match db.get_former_dec_party_ids().await {
        Ok(ids) => ids.into_iter().collect(),
        Err(e) => {
            tracing::warn!("Could not read the recorded former parties: {e}");
            return;
        }
    };
    let candidates: Vec<&String> = unreturned
        .iter()
        .filter(|party_id| !recorded.contains(*party_id))
        .collect();
    if candidates.is_empty() {
        return;
    }

    let synchronizer_id = match utils::get_synchronizer_id(config).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("Could not check for former parties: {e}");
            return;
        }
    };
    for party_id in candidates {
        let Ok(party) = CantonId::parse(party_id) else {
            continue;
        };
        match live_removal_time(config, &synchronizer_id, &party).await {
            Ok(Some(removed_at)) => match db.record_former_dec_party(&party, removed_at).await {
                Ok(true) => tracing::info!(
                    %party,
                    removed_at,
                    "This participant no longer hosts the party; its leftover contracts can be \
                     purged"
                ),
                Ok(false) => {}
                Err(e) => tracing::warn!(%party, "Could not record a former party: {e}"),
            },
            Ok(None) => {}
            Err(e) => tracing::warn!(%party, "Could not read the party's hosting history: {e}"),
        }
    }
}

/// The wire form of a former party row.
pub(crate) fn to_former_party(row: FormerDecPartyRow) -> Result<FormerParty> {
    Ok(FormerParty {
        party_id: CantonId::parse(&row.party_id)?,
        removed_at: row.removed_at,
        detected_at: row.detected_at,
        purge_status: match row.purge_status.as_str() {
            "running" => PurgeStatus::Running,
            "succeeded" => PurgeStatus::Succeeded,
            "failed" => PurgeStatus::Failed,
            _ => PurgeStatus::None,
        },
        purge_started_at: row.purge_started_at,
        purge_finished_at: row.purge_finished_at,
        purged_count: row.purged_count,
        kept_count: row.kept_count,
        purge_error: row.purge_error,
    })
}

/// A contract the participant holds, with every party that is a stakeholder.
#[derive(Clone, Debug)]
pub(crate) struct HeldContract {
    pub contract_id: String,
    pub stakeholders: Vec<String>,
}

/// A former party's contracts on this participant, split by the purge rule.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LeftoverSplit {
    /// Contracts no hosted party is a stakeholder of.
    pub purge: Vec<String>,
    /// How many contracts a hosted party is a stakeholder of.
    pub keep: usize,
}

/// Split `contracts` into those no party in `hosted` is a stakeholder of, which
/// the purge removes, and the rest, which it keeps.
pub(crate) fn split_leftovers(
    contracts: Vec<HeldContract>,
    hosted: &HashSet<String>,
) -> LeftoverSplit {
    let mut split = LeftoverSplit::default();
    for contract in contracts {
        if contract.stakeholders.iter().any(|s| hosted.contains(s)) {
            split.keep += 1;
        } else {
            split.purge.push(contract.contract_id);
        }
    }
    split.purge.sort();
    split
}

/// Every contract `party` is a stakeholder of on this participant.
async fn read_held_contracts(
    config: &NodeConfig,
    token: &str,
    party: &CantonId,
) -> Result<Vec<HeldContract>> {
    fetch_active_contracts_filtered(
        config,
        Some(token.to_string()),
        party_event_format(party, vec![wildcard_filter(false)], false),
        |created| {
            let mut stakeholders = created.signatories;
            stakeholders.extend(created.observers);
            Some(HeldContract {
                contract_id: created.contract_id,
                stakeholders,
            })
        },
    )
    .await
}

/// Whether this participant hosts `party` in its head state.
async fn hosts(
    mut client: TopologyManagerReadServiceClient<Channel>,
    synchronizer_id: &str,
    participant_uid: &str,
    party: String,
) -> Result<Option<String>> {
    // An exact party filter is the only one Canton narrows in the store query;
    // a participant filter alone would load every party on the synchronizer.
    let response = client
        .list_party_to_participant(tonic::Request::new(ListPartyToParticipantRequest {
            base_query: Some(head_state_query(synchronizer_id)),
            filter_party: party.clone(),
            filter_participant: participant_uid.to_string(),
        }))
        .await?
        .into_inner();
    let hosted = response.results.into_iter().any(|result| {
        let operation = result.context.map(|c| c.operation);
        matches!(result.item, Some(P2pItem::V30(ref m))
            if m.party == party
                && operation == Some(TopologyChangeOp::AddReplace as i32)
                && m.participants.iter().any(|p| p.participant_uid == participant_uid))
    });
    Ok(hosted.then_some(party))
}

/// The stakeholders of `contracts` this participant hosts, plus its admin
/// party, which it always hosts.
async fn hosted_stakeholders(
    config: &NodeConfig,
    synchronizer_id: &str,
    party: &CantonId,
    contracts: &[HeldContract],
) -> Result<HashSet<String>> {
    let participant_uid = config.participant_id().to_string();
    let former = party.to_string();
    let candidates: BTreeSet<String> = contracts
        .iter()
        .flat_map(|c| c.stakeholders.iter())
        .filter(|s| **s != former && **s != participant_uid)
        .cloned()
        .collect();

    // One connection for every read; a tonic channel multiplexes the requests.
    let client = TopologyManagerReadServiceClient::new(config.admin_channel().await?);
    let found: Vec<Option<String>> = futures::stream::iter(candidates)
        .map(|candidate| hosts(client.clone(), synchronizer_id, &participant_uid, candidate))
        .buffer_unordered(HOSTING_READ_CONCURRENCY)
        .try_collect()
        .await?;
    let mut hosted: HashSet<String> = found.into_iter().flatten().collect();
    hosted.insert(participant_uid);
    Ok(hosted)
}

/// Read the party's contracts on this participant and split them by the
/// purge rule.
pub(crate) async fn plan_purge(
    config: &NodeConfig,
    token: &str,
    party: &CantonId,
) -> Result<LeftoverSplit> {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let contracts = read_held_contracts(config, token, party).await?;
    let hosted = hosted_stakeholders(config, &synchronizer_id, party, &contracts).await?;
    Ok(split_leftovers(contracts, &hosted))
}

/// Why a purge may not start.
#[derive(Debug)]
pub(crate) enum PurgeRefusal {
    /// The party is not a former party of this node, or it was dismissed.
    NotFormer,
    /// The node is in a state where a purge is unsafe; the text says which.
    Unsafe(String),
}

/// Check that a purge of `party` is safe to start now.
pub(crate) async fn check_purge_allowed(
    config: &NodeConfig,
    db: &SqlitePool,
    party: &CantonId,
) -> Result<std::result::Result<(), PurgeRefusal>> {
    let refuse = |why: String| Ok(Err(PurgeRefusal::Unsafe(why)));

    match db.get_former_dec_party(party).await? {
        Some(row) if row.dismissed_at.is_none() => {
            if row.purge_status == "running" {
                return refuse("A purge of this party is already running".to_string());
            }
        }
        _ => return Ok(Err(PurgeRefusal::NotFormer)),
    }

    // Disconnecting would break a run in flight, and an add-party may be about
    // to host this very party again.
    let runs = db.get_in_progress_workflow_runs().await?;
    if !runs.is_empty() {
        let names: Vec<String> = runs
            .iter()
            .map(|run| format!("{:?} {}", run.kind, run.instance_name))
            .collect();
        return refuse(format!(
            "Workflows are running on this node ({}). A purge takes the participant off the \
             synchronizer, so wait for them to finish",
            names.join(", ")
        ));
    }

    // A participant that is already off the synchronizer may be in the middle
    // of a recovery. Reconnecting it at the end of the purge could replay a
    // window in which it hosted a party without that party's contracts.
    let alias = config.synchronizer();
    let connected = SynchronizerConnectivityServiceClient::new(config.admin_channel().await?)
        .list_connected_synchronizers(tonic::Request::new(ListConnectedSynchronizersRequest {}))
        .await?
        .into_inner()
        .connected_synchronizers;
    if !synchronizer_healthy(&connected, alias) {
        return refuse(format!(
            "The participant is not connected and healthy on synchronizer '{alias}'. The purge \
             disconnects and reconnects it, so it only starts from a healthy connection"
        ));
    }

    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let Some(removed_at) = live_removal_time(config, &synchronizer_id, party).await? else {
        return refuse(
            "This participant hosts the party again, so its contracts are current".to_string(),
        );
    };
    let age = chrono::Utc::now().timestamp() - removed_at;
    let min_age = MIN_REMOVAL_AGE.as_secs() as i64;
    if age < min_age {
        return refuse(format!(
            "The removal took effect {age}s ago. Wait until it is at least {min_age}s old, so \
             the participant has processed every event up to it"
        ));
    }
    Ok(Ok(()))
}

/// Purge the leftovers of `party` and record the outcome. Runs after
/// [`check_purge_allowed`] passed and the purge was claimed.
pub(crate) async fn run_purge(config: NodeConfig, db: SqlitePool, token: String, party: CantonId) {
    tracing::info!(%party, "Purging the former party's leftover contracts");
    let outcome = purge_leftovers(&config, &token, &party).await;
    let recorded = match outcome {
        Ok((purged, kept)) => {
            tracing::info!(%party, purged, kept, "Purge of the former party's contracts completed");
            db.complete_former_party_purge(&party, purged as i64, kept as i64)
                .await
        }
        Err(e) => {
            tracing::error!(%party, "Purge of the former party's contracts failed: {e:#}");
            db.fail_former_party_purge(&party, &format!("{e:#}")).await
        }
    };
    if let Err(e) = recorded {
        tracing::error!(%party, "Could not record the purge outcome: {e}");
    }
}

/// Returns how many contracts were purged and how many were kept.
async fn purge_leftovers(
    config: &NodeConfig,
    token: &str,
    party: &CantonId,
) -> Result<(usize, usize)> {
    let split = plan_purge(config, token, party).await?;
    if split.purge.is_empty() {
        return Ok((0, split.keep));
    }

    let alias = config.synchronizer().to_string();
    let mut connectivity =
        SynchronizerConnectivityServiceClient::new(config.admin_channel().await?);
    // Disconnect and purge as one fallible unit. The reconnect below runs on
    // every outcome, so the participant is never left off the synchronizer.
    let purged = async {
        connectivity
            .disconnect_synchronizer(tonic::Request::new(DisconnectSynchronizerRequest {
                synchronizer_alias: alias.clone(),
            }))
            .await?;
        purge_in_batches(config, &alias, &split.purge).await
    }
    .await;

    let reconnected = async {
        connectivity
            .reconnect_synchronizer(tonic::Request::new(ReconnectSynchronizerRequest {
                synchronizer_alias: alias.clone(),
                retry: true,
            }))
            .await?;
        verify_healthy(config).await
    }
    .await;

    // An unhealthy participant is the failure an operator must act on first,
    // so it is reported even when the purge failed too.
    if let Err(reconnect_err) = reconnected {
        let purge_note = match &purged {
            Ok(()) => "the purge itself completed".to_string(),
            Err(e) => format!("the purge also failed: {e:#}"),
        };
        anyhow::bail!(
            "the participant did not come back connected and healthy on '{alias}' after the \
             purge ({purge_note}): {reconnect_err:#}"
        );
    }
    purged?;

    let after = plan_purge(config, token, party).await?;
    anyhow::ensure!(
        after.purge.is_empty(),
        "{} contract(s) no hosted party sees are still active after the purge",
        after.purge.len()
    );
    Ok((split.purge.len(), after.keep))
}

async fn purge_in_batches(config: &NodeConfig, alias: &str, contract_ids: &[String]) -> Result {
    let mut repair = ParticipantRepairServiceClient::new(config.admin_channel().await?);
    for (index, batch) in contract_ids.chunks(PURGE_BATCH).enumerate() {
        let mut attempt = 1;
        loop {
            let request = PurgeContractsRequest {
                synchronizer_alias: alias.to_string(),
                contract_ids: batch.to_vec(),
                ignore_already_purged: true,
            };
            match repair.purge_contracts(tonic::Request::new(request)).await {
                Ok(_) => break,
                Err(status) if attempt < PURGE_BATCH_ATTEMPTS => {
                    tracing::warn!(
                        "PurgeContracts batch {} failed (attempt {attempt}/{PURGE_BATCH_ATTEMPTS}): \
                         {status}",
                        index + 1
                    );
                    attempt += 1;
                    tokio::time::sleep(PURGE_RETRY_DELAY).await;
                }
                Err(status) => anyhow::bail!(
                    "PurgeContracts failed on batch {} of {}: {status}",
                    index + 1,
                    contract_ids.len().div_ceil(PURGE_BATCH)
                ),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(serial: i32, valid_from: i64, hosts_participant: bool) -> HostingEntry {
        HostingEntry {
            serial,
            valid_from,
            hosts_participant,
        }
    }

    #[test]
    fn removal_is_the_first_serial_after_the_last_hosting_one() {
        let history = [entry(1, 10, true), entry(2, 20, true), entry(3, 30, false)];
        assert_eq!(removal_time(&history), Some(30));
    }

    #[test]
    fn a_current_host_has_no_removal() {
        let history = [entry(1, 10, false), entry(2, 20, true)];
        assert_eq!(removal_time(&history), None);
    }

    #[test]
    fn a_participant_that_never_hosted_has_no_removal() {
        let history = [entry(1, 10, false), entry(2, 20, false)];
        assert_eq!(removal_time(&history), None);
        assert_eq!(removal_time(&[]), None);
    }

    #[test]
    fn a_rehost_and_second_removal_counts_from_the_latest() {
        let history = [
            entry(4, 40, false),
            entry(1, 10, true),
            entry(3, 30, true),
            entry(2, 20, false),
            entry(5, 50, false),
        ];
        assert_eq!(removal_time(&history), Some(40));
    }

    #[test]
    fn keeps_every_contract_a_hosted_party_sees() {
        let contract = |id: &str, stakeholders: &[&str]| HeldContract {
            contract_id: id.into(),
            stakeholders: stakeholders.iter().map(|s| s.to_string()).collect(),
        };
        let hosted: HashSet<String> = ["member-p3", "other-dec-party", "participant::p3"]
            .map(String::from)
            .into();
        let split = split_leftovers(
            vec![
                contract("party-only", &["party"]),
                contract("remote-member", &["party", "member-p1"]),
                contract("admin-party", &["party", "participant::p3"]),
                contract("local-member", &["member-p3", "party"]),
                contract("other-dec-party", &["party", "other-dec-party"]),
                contract("another-former", &["party", "former-too"]),
            ],
            &hosted,
        );
        assert_eq!(
            split,
            LeftoverSplit {
                purge: vec![
                    "another-former".into(),
                    "party-only".into(),
                    "remote-member".into()
                ],
                keep: 3,
            }
        );
    }
}
