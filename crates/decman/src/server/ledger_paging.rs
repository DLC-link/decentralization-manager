//! Paginated Canton ledger-API reads.
//!
//! Canton grew pagination for the bulk read endpoints in 3.5.1
//! (`GetActiveContractsPage`, `GetUpdatesPage`); before that the only option
//! was a server-side stream that ran until the whole result set had been sent.
//! Every read in this crate goes through the helpers here so there is one
//! place that speaks the paged protocol, and one place that falls back to the
//! old streaming call when the participant is older than 3.5.1.
//!
//! Note the two different page sizes. [`common::api::PAGE_SIZE`] is the *wire*
//! page size — what an API client or the UI gets in one response. [`FETCH_CHUNK`]
//! is how much we pull from Canton per round trip when a caller needs the whole
//! result set anyway; sizing that at 25 would turn one stream into hundreds of
//! round trips.

use canton_proto_rs::com::daml::ledger::api::v2::{
    CreatedEvent, EventFormat, GetActiveContractsPageRequest, GetActiveContractsRequest,
    GetLedgerEndRequest, GetUpdatesPageRequest, GetUpdatesRequest, ListVettedPackagesRequest,
    Transaction, UpdateFormat, VettedPackage, get_active_contracts_response::ContractEntry,
    get_update_response, get_updates_response,
};
use crate::{config::NodeConfig, error::Result, utils};

/// Rows pulled from Canton per round trip when collecting a full result set.
const FETCH_CHUNK: i32 = 1000;

/// Did the participant reject this RPC because it predates Canton 3.5.1?
fn is_unimplemented(status: &tonic::Status) -> bool {
    status.code() == tonic::Code::Unimplemented
}

/// Every `CreatedEvent` in the party's ACS matching `event_format`, read a page
/// at a time.
///
/// `active_at_offset` is pinned to the ledger end for the whole walk: a page
/// token is only valid against the offset and event format that produced it.
pub(crate) async fn fetch_active_contracts(
    config: &NodeConfig,
    token: Option<String>,
    event_format: EventFormat,
) -> Result<Vec<CreatedEvent>> {
    let mut client = utils::create_state_client(config, token.clone()).await?;

    let ledger_end = client
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {}))
        .await?
        .into_inner()
        .offset;

    let mut created = Vec::new();
    let mut page_token = None;

    loop {
        let request = GetActiveContractsPageRequest {
            active_at_offset: Some(ledger_end),
            event_format: Some(event_format.clone()),
            max_page_size: Some(FETCH_CHUNK),
            page_token,
        };

        let page = match client
            .get_active_contracts_page(tonic::Request::new(request))
            .await
        {
            Ok(page) => page.into_inner(),
            Err(status) if is_unimplemented(&status) => {
                tracing::debug!(
                    "GetActiveContractsPage unavailable (participant older than Canton 3.5.1); \
                     falling back to the streaming ACS read"
                );
                return stream_active_contracts(config, token, ledger_end, event_format).await;
            }
            Err(status) => return Err(status.into()),
        };

        for response in page.active_contracts {
            if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
                && let Some(event) = active.created_event
            {
                created.push(event);
            }
        }

        match page.next_page_token {
            Some(next) if !next.is_empty() => page_token = Some(next),
            _ => break,
        }
    }

    Ok(created)
}

/// Pre-3.5.1 path for [`fetch_active_contracts`]: one unbounded server stream.
///
/// Builds its own client because the caller's has already been moved into the
/// failed paged attempt.
async fn stream_active_contracts(
    config: &NodeConfig,
    token: Option<String>,
    ledger_end: i64,
    event_format: EventFormat,
) -> Result<Vec<CreatedEvent>> {
    let mut client = utils::create_state_client(config, token).await?;

    let request = GetActiveContractsRequest {
        active_at_offset: ledger_end,
        event_format: Some(event_format),
        stream_continuation_token: None,
    };

    let mut stream = client
        .get_active_contracts(tonic::Request::new(request))
        .await?
        .into_inner();

    let mut created = Vec::new();
    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(event) = active.created_event
        {
            created.push(event);
        }
    }

    Ok(created)
}

/// One page of transactions, newest first, plus the token for the page after
/// it.
///
/// The paged and streaming RPCs return different `update` oneofs
/// (`GetUpdateResponse` vs `GetUpdatesResponse` — the latter also carries
/// offset checkpoints), so both paths are narrowed to transactions here and
/// callers see one shape.
pub(crate) struct TransactionPage {
    pub transactions: Vec<Transaction>,
    pub next_page_token: Option<Vec<u8>>,
}

/// Read updates in `(begin_exclusive, end_inclusive]` newest-first, one page at
/// a time, so a caller that only wants the most recent N can stop early instead
/// of draining the whole ledger.
///
/// On a pre-3.5.1 participant there is no way to ask for "newest first" — the
/// fallback drains the range in ascending order and reverses, which is exactly
/// the behaviour this replaces.
pub(crate) async fn fetch_transactions_page(
    config: &NodeConfig,
    token: Option<String>,
    begin_exclusive: i64,
    end_inclusive: i64,
    update_format: UpdateFormat,
    page_token: Option<Vec<u8>>,
) -> Result<TransactionPage> {
    let mut client = utils::create_update_client(config, token.clone()).await?;

    let request = GetUpdatesPageRequest {
        begin_offset_exclusive: Some(begin_exclusive),
        end_offset_inclusive: Some(end_inclusive),
        max_page_size: Some(FETCH_CHUNK),
        update_format: Some(update_format.clone()),
        descending_order: true,
        page_token,
    };

    match client.get_updates_page(tonic::Request::new(request)).await {
        Ok(page) => {
            let page = page.into_inner();
            let transactions = page
                .updates
                .into_iter()
                .filter_map(|response| match response.update? {
                    get_update_response::Update::Transaction(tx) => Some(tx),
                    _ => None,
                })
                .collect();
            Ok(TransactionPage {
                transactions,
                next_page_token: page.next_page_token.filter(|t| !t.is_empty()),
            })
        }
        Err(status) if is_unimplemented(&status) => {
            tracing::debug!(
                "GetUpdatesPage unavailable (participant older than Canton 3.5.1); falling back \
                 to the streaming update read"
            );
            let mut transactions =
                stream_transactions(config, token, begin_exclusive, end_inclusive, update_format)
                    .await?;
            transactions.reverse();
            Ok(TransactionPage {
                transactions,
                next_page_token: None,
            })
        }
        Err(status) => Err(status.into()),
    }
}

/// Every package currently vetted by this participant, deduplicated by package
/// id.
///
/// `ListVettedPackages` landed in Canton 3.4.10 and is paginated from the
/// outset. It reports the participant's *topology* vetting state, one entry per
/// (participant, synchronizer) pair, so the same package id can appear more
/// than once when a participant is connected to several synchronizers.
pub(crate) async fn fetch_vetted_packages(
    config: &NodeConfig,
    token: Option<String>,
) -> Result<Vec<VettedPackage>> {
    let mut client = utils::create_package_client(config, token).await?;

    let mut seen = std::collections::HashSet::new();
    let mut vetted = Vec::new();
    let mut page_token = String::new();

    loop {
        let response = client
            .list_vetted_packages(tonic::Request::new(ListVettedPackagesRequest {
                package_metadata_filter: None,
                topology_state_filter: None,
                page_token: page_token.clone(),
                page_size: FETCH_CHUNK as u32,
            }))
            .await?
            .into_inner();

        for entry in response.vetted_packages {
            for package in entry.packages {
                if seen.insert(package.package_id.clone()) {
                    vetted.push(package);
                }
            }
        }

        if response.next_page_token.is_empty() {
            return Ok(vetted);
        }
        page_token = response.next_page_token;
    }
}

/// Pre-3.5.1 path for [`fetch_transactions_page`]: drain the range as one
/// stream.
async fn stream_transactions(
    config: &NodeConfig,
    token: Option<String>,
    begin_exclusive: i64,
    end_inclusive: i64,
    update_format: UpdateFormat,
) -> Result<Vec<Transaction>> {
    let mut client = utils::create_update_client(config, token).await?;

    let request = GetUpdatesRequest {
        begin_exclusive,
        end_inclusive: Some(end_inclusive),
        update_format: Some(update_format),
        descending_order: false,
    };

    let mut stream = client
        .get_updates(tonic::Request::new(request))
        .await?
        .into_inner();

    let mut transactions = Vec::new();
    while let Some(response) = stream.message().await? {
        if let Some(get_updates_response::Update::Transaction(tx)) = response.update {
            transactions.push(tx);
        }
    }

    Ok(transactions)
}
