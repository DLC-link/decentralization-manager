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
        party_replication::acs::{manual_connect_enabled, synchronizer_healthy, verify_healthy},
        topology::{head_state_query, history_query},
    },
};

/// Serializes the operations that take the participant off its synchronizer
/// or put it back. A purge holds it from its start until it records its
/// outcome. A reconnect takes it, or refuses while a purge holds it.
static CONNECTIVITY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
     running. The participant may still be disconnected from its synchronizer. Reconnect it \
     from this page if it is, then run the purge again";

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
///
/// Recorded parties are read again too. A rehost and a second removal can
/// both fall between two discoveries, and only the history shows the newer
/// removal.
pub(crate) async fn reconcile(
    config: &NodeConfig,
    db: &SqlitePool,
    unreturned: &[String],
    returned: &[String],
) {
    if let Err(e) = db.forget_former_dec_parties(returned).await {
        tracing::warn!("Could not clear former parties this node hosts again: {e}");
    }

    if unreturned.is_empty() {
        return;
    }

    let synchronizer_id = match utils::get_synchronizer_id(config).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("Could not check for former parties: {e}");
            return;
        }
    };
    for party_id in unreturned {
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

/// Why a purge or a reconnect may not start.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// The party is not a former party of this node, or it was dismissed.
    NotFormer,
    /// The node is in a state where the operation is unsafe; the text says
    /// which.
    Unsafe(String),
}

/// The in-progress workflow runs, named for an operator, or `None` when there
/// are none.
async fn running_workflows(db: &SqlitePool) -> Result<Option<String>> {
    let runs = db.get_in_progress_workflow_runs().await?;
    Ok((!runs.is_empty()).then(|| {
        runs.iter()
            .map(|run| format!("{:?} {}", run.kind, run.instance_name))
            .collect::<Vec<_>>()
            .join(", ")
    }))
}

/// Check that a purge of `party` is safe to start now.
pub(crate) async fn check_purge_allowed(
    config: &NodeConfig,
    db: &SqlitePool,
    party: &CantonId,
) -> Result<std::result::Result<(), Refusal>> {
    let refuse = |why: String| Ok(Err(Refusal::Unsafe(why)));

    match db.get_former_dec_party(party).await? {
        Some(row) if row.dismissed_at.is_none() => {
            if row.purge_status == "running" {
                return refuse("A purge of this party is already running".to_string());
            }
        }
        _ => return Ok(Err(Refusal::NotFormer)),
    }

    // Disconnecting would break a run in flight, and an add-party may be about
    // to host this very party again.
    if let Some(runs) = running_workflows(db).await? {
        return refuse(format!(
            "Workflows are running on this node ({runs}). A purge takes the participant off the \
             synchronizer, so wait for them to finish"
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
    let _connectivity = CONNECTIVITY.lock().await;
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
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
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
        // The participant's topology stops changing once it is off the
        // synchronizer. A rehost since the plan makes the planned contracts
        // current again, so check for one here, before anything is removed.
        anyhow::ensure!(
            live_removal_time(config, &synchronizer_id, party)
                .await?
                .is_some(),
            "this participant hosts the party again, so the purge stopped before removing \
             anything"
        );
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

/// Reconnect the participant after a purge of `party` failed, and wait until
/// its connection is healthy.
///
/// A purge reconnects on every outcome. This covers the cases it could not:
/// decman stopped during the purge, or the reconnect itself failed. DecMan
/// keeps a participant off on purpose in other cases, for example after a
/// failed add-party run. It then sets manual connection, and this refuses.
pub(crate) async fn reconnect_after_failed_purge(
    config: &NodeConfig,
    db: &SqlitePool,
    party: &CantonId,
) -> Result<std::result::Result<(), Refusal>> {
    let refuse = |why: String| Ok(Err(Refusal::Unsafe(why)));

    match db.get_former_dec_party(party).await? {
        Some(row) if row.dismissed_at.is_none() => {
            if row.purge_status != "failed" {
                return refuse(
                    "Reconnect is for a purge that failed, and this party's last purge did not"
                        .to_string(),
                );
            }
        }
        _ => return Ok(Err(Refusal::NotFormer)),
    }
    if let Some(runs) = running_workflows(db).await? {
        return refuse(format!(
            "Workflows are running on this node ({runs}). A workflow can keep the participant \
             off the synchronizer on purpose, so wait for them to finish"
        ));
    }
    let purge_running =
        "A purge is running on this node. It reconnects the participant when it ends".to_string();
    let Ok(connectivity) = CONNECTIVITY.try_lock() else {
        return refuse(purge_running);
    };
    // A claimed purge takes the lock only once its task starts.
    if db
        .get_former_dec_parties()
        .await?
        .iter()
        .any(|row| row.purge_status == "running")
    {
        return refuse(purge_running);
    }

    let alias = config.synchronizer().to_string();
    let connected = SynchronizerConnectivityServiceClient::new(config.admin_channel().await?)
        .list_connected_synchronizers(tonic::Request::new(ListConnectedSynchronizersRequest {}))
        .await?
        .into_inner()
        .connected_synchronizers;
    if synchronizer_healthy(&connected, &alias) {
        return refuse(format!(
            "The participant is already connected and healthy on synchronizer '{alias}'"
        ));
    }
    if manual_connect_enabled(config).await? {
        return refuse(format!(
            "Synchronizer '{alias}' is set to manual connection. DecMan sets that when it keeps \
             the participant off on purpose, for example after a failed add-party run. \
             Reconnecting it here can crash the participant, so follow the guide for that case"
        ));
    }

    tracing::info!(%party, "Reconnecting the participant after a failed purge");
    // A spawned task finishes the reconnect even if the request is dropped.
    let config = config.clone();
    tokio::spawn(async move {
        let _connectivity = connectivity;
        SynchronizerConnectivityServiceClient::new(config.admin_channel().await?)
            .reconnect_synchronizer(tonic::Request::new(ReconnectSynchronizerRequest {
                synchronizer_alias: alias,
                retry: true,
            }))
            .await?;
        verify_healthy(&config).await
    })
    .await??;
    tracing::info!(%party, "The participant is connected and healthy again");
    Ok(Ok(()))
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

    fn former(prefix: &str) -> CantonId {
        CantonId::parse(&format!("{prefix}::1220{}", "b".repeat(64))).unwrap()
    }

    async fn failed_purge(db: &SqlitePool, party: &CantonId) -> Result {
        db.record_former_dec_party(party, 1).await?;
        db.claim_former_party_purge(party).await?;
        db.fail_former_party_purge(party, INTERRUPTED_PURGE_ERROR)
            .await
    }

    fn refusal(outcome: std::result::Result<(), Refusal>) -> String {
        match outcome {
            Err(Refusal::Unsafe(why)) => why,
            other => panic!("expected an unsafe refusal, got {other:?}"),
        }
    }

    // The refusals below come before any call to the participant, so a
    // default config never reaches one.

    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn reconnect_is_only_for_a_failed_purge(db: SqlitePool) -> Result {
        let config = NodeConfig::default();
        let party = former("left");
        assert!(matches!(
            reconnect_after_failed_purge(&config, &db, &party).await?,
            Err(Refusal::NotFormer)
        ));

        db.record_former_dec_party(&party, 1).await?;
        let why = refusal(reconnect_after_failed_purge(&config, &db, &party).await?);
        assert!(why.contains("did not"), "{why}");

        db.claim_former_party_purge(&party).await?;
        db.complete_former_party_purge(&party, 1, 0).await?;
        db.dismiss_former_dec_party(&party).await?;
        assert!(matches!(
            reconnect_after_failed_purge(&config, &db, &party).await?,
            Err(Refusal::NotFormer)
        ));
        Ok(())
    }

    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn reconnect_waits_for_running_workflows(db: SqlitePool) -> Result {
        let party = former("left");
        failed_purge(&db, &party).await?;
        sqlx::query(
            "INSERT INTO workflow_runs (
                 instance_name, kind, role, status, current_step, step_index, step_total,
                 config_json, expected_peers_json, completed_peers_json, created_at, updated_at
             ) VALUES ('add-1', 'AddParty', 'Peer', 'inprogress', 'Active', 0, 1, '{}', '[]',
                       '[]', 0, 0)",
        )
        .execute(&db)
        .await?;

        let why = refusal(reconnect_after_failed_purge(&NodeConfig::default(), &db, &party).await?);
        assert!(why.contains("AddParty add-1"), "{why}");
        Ok(())
    }

    /// The only test that takes the connectivity lock, so the parallel tests
    /// above never see it held.
    #[sqlx::test(migrator = "crate::db::MIGRATOR")]
    async fn reconnect_waits_for_a_running_purge(db: SqlitePool) -> Result {
        let config = NodeConfig::default();
        let (failed, other) = (former("failed"), former("other"));
        failed_purge(&db, &failed).await?;

        {
            let _purge = CONNECTIVITY.lock().await;
            let why = refusal(reconnect_after_failed_purge(&config, &db, &failed).await?);
            assert!(why.contains("A purge is running"), "{why}");
        }

        // Claimed, but its task has not taken the lock yet.
        db.record_former_dec_party(&other, 1).await?;
        db.claim_former_party_purge(&other).await?;
        let why = refusal(reconnect_after_failed_purge(&config, &db, &failed).await?);
        assert!(why.contains("A purge is running"), "{why}");
        Ok(())
    }
}
