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

use crate::{config::NodeConfig, error::Result, utils};
use canton_proto_rs::com::daml::ledger::api::v2::{
    CreatedEvent, EventFormat, GetActiveContractsPageRequest, GetActiveContractsRequest,
    GetLedgerEndRequest, GetUpdatesPageRequest, GetUpdatesRequest, Transaction, UpdateFormat,
    get_active_contracts_response::ContractEntry, get_update_response, get_updates_response,
};

/// Rows pulled from Canton per round trip when collecting a full result set.
pub(crate) const FETCH_CHUNK: i32 = 1000;

/// Did the participant reject this RPC because it predates Canton 3.5.1?
fn is_unimplemented(status: &tonic::Status) -> bool {
    status.code() == tonic::Code::Unimplemented
}

/// Like [`fetch_active_contracts`], but applies `extract` to each event as its
/// page arrives and keeps only what it returns.
///
/// For callers that discard most of what they read — a wildcard query narrowed
/// client-side, or a filter Canton can't express. Collecting every event first
/// and filtering afterwards would hold the whole matching ACS in memory, which
/// is worse than the streaming read this replaced.
pub(crate) async fn fetch_active_contracts_filtered<T, F>(
    config: &NodeConfig,
    token: Option<String>,
    event_format: EventFormat,
    extract: F,
) -> Result<Vec<T>>
where
    F: FnMut(CreatedEvent) -> Option<T>,
{
    collect_active_contracts(config, token, event_format, None, extract).await
}

/// Run `f` over every matching `CreatedEvent` as its page arrives, keeping
/// nothing.
///
/// For callers that fold results into state they already own (a caller-supplied
/// accumulator, a map keyed by contract id) rather than building a list.
pub(crate) async fn for_each_active_contract<F>(
    config: &NodeConfig,
    token: Option<String>,
    event_format: EventFormat,
    mut f: F,
) -> Result<()>
where
    F: FnMut(CreatedEvent),
{
    collect_active_contracts(config, token, event_format, None, |created| {
        f(created);
        None::<()>
    })
    .await?;
    Ok(())
}

/// The first event `extract` accepts, or `None` — stopping at the first match
/// rather than walking the rest of the ACS.
pub(crate) async fn fetch_first_matching<T, F>(
    config: &NodeConfig,
    token: Option<String>,
    event_format: EventFormat,
    extract: F,
) -> Result<Option<T>>
where
    F: FnMut(CreatedEvent) -> Option<T>,
{
    Ok(
        collect_active_contracts(config, token, event_format, Some(1), extract)
            .await?
            .into_iter()
            .next(),
    )
}

/// The first `CreatedEvent` matching `event_format`, or `None`.
///
/// For "does this contract exist / read its state" lookups. Stops after one
/// page instead of walking the whole ACS to then discard all but the first
/// row.
pub(crate) async fn fetch_first_active_contract(
    config: &NodeConfig,
    token: Option<String>,
    event_format: EventFormat,
) -> Result<Option<CreatedEvent>> {
    Ok(
        collect_active_contracts(config, token, event_format, Some(1), Some)
            .await?
            .into_iter()
            .next(),
    )
}

/// Walk the ACS a page at a time, stopping once `max_events` have been
/// collected (or at the end of the ACS when it is `None`).
///
/// `active_at_offset` is pinned to the ledger end for the whole walk: a page
/// token is only valid against the offset and event format that produced it.
async fn collect_active_contracts<T, F>(
    config: &NodeConfig,
    token: Option<String>,
    event_format: EventFormat,
    max_events: Option<usize>,
    mut extract: F,
) -> Result<Vec<T>>
where
    F: FnMut(CreatedEvent) -> Option<T>,
{
    let mut client = utils::create_state_client(config, token.clone()).await?;

    let ledger_end = client
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {}))
        .await?
        .into_inner()
        .offset;

    // Asking for fewer rows than a full chunk when the caller only wants a
    // handful keeps the "find first" case to a single small round trip.
    let page_size = match max_events {
        Some(max) => FETCH_CHUNK.min(max.try_into().unwrap_or(FETCH_CHUNK)),
        None => FETCH_CHUNK,
    };

    let mut created = Vec::new();
    let mut page_token = None;

    loop {
        let request = GetActiveContractsPageRequest {
            active_at_offset: Some(ledger_end),
            event_format: Some(event_format.clone()),
            max_page_size: Some(page_size),
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
                return stream_active_contracts(
                    config,
                    token,
                    ledger_end,
                    event_format,
                    max_events,
                    extract,
                )
                .await;
            }
            Err(status) => return Err(status.into()),
        };

        for response in page.active_contracts {
            if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
                && let Some(event) = active.created_event
                && let Some(kept) = extract(event)
            {
                created.push(kept);
            }
        }

        if max_events.is_some_and(|max| created.len() >= max) {
            break;
        }

        match page.next_page_token {
            Some(next) if !next.is_empty() => page_token = Some(next),
            _ => break,
        }
    }

    Ok(created)
}

/// Pre-3.5.1 path for [`collect_active_contracts`]: one server stream, stopped
/// early once `max_events` have arrived.
///
/// Builds its own client because the caller's has already been moved into the
/// failed paged attempt.
async fn stream_active_contracts<T, F>(
    config: &NodeConfig,
    token: Option<String>,
    ledger_end: i64,
    event_format: EventFormat,
    max_events: Option<usize>,
    mut extract: F,
) -> Result<Vec<T>>
where
    F: FnMut(CreatedEvent) -> Option<T>,
{
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
            && let Some(kept) = extract(event)
        {
            created.push(kept);
            if max_events.is_some_and(|max| created.len() >= max) {
                break;
            }
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
            let transactions =
                stream_transactions(config, token, begin_exclusive, end_inclusive, update_format)
                    .await?;
            Ok(descending_page(transactions))
        }
        Err(status) => Err(status.into()),
    }
}

/// Present an ascending stream of transactions as one descending page.
///
/// The pre-3.5.1 `GetUpdates` has no "newest first" option, so the reversal
/// here is what makes the fallback meet the same contract as the paged RPC.
/// There is no continuation token to hand back: the stream drained the whole
/// range in one go.
fn descending_page(mut transactions: Vec<Transaction>) -> TransactionPage {
    transactions.reverse();

    TransactionPage {
        transactions,
        next_page_token: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tx_at(offset: i64) -> Transaction {
        Transaction {
            offset,
            update_id: format!("update-{offset}"),
            ..Default::default()
        }
    }

    fn offsets_of(page: &TransactionPage) -> Vec<i64> {
        page.transactions.iter().map(|tx| tx.offset).collect()
    }

    /// The pre-3.5.1 stream arrives oldest-first; callers page newest-first, so
    /// the fallback has to hand back the reverse of what it read.
    #[test]
    fn fallback_page_is_newest_first() {
        let page = descending_page(vec![tx_at(10), tx_at(20), tx_at(30)]);

        assert_eq!(offsets_of(&page), vec![30, 20, 10]);
    }

    /// The stream drained the whole range, so there is nothing to continue
    /// from — a token here would send the caller round the same range again.
    #[test]
    fn fallback_page_has_no_continuation() {
        let page = descending_page(vec![tx_at(10)]);
        assert!(page.next_page_token.is_none());

        let empty = descending_page(Vec::new());
        assert!(empty.transactions.is_empty());
        assert!(empty.next_page_token.is_none());
    }

    /// Only `Unimplemented` means "this participant predates the paged RPCs".
    /// Any other status is a real error and must not be answered with a full
    /// unpaged read.
    #[test]
    fn only_unimplemented_selects_the_fallback() {
        assert!(is_unimplemented(&tonic::Status::unimplemented(
            "GetUpdatesPage"
        )));

        for status in [
            tonic::Status::unavailable("participant restarting"),
            tonic::Status::permission_denied("bad token"),
            tonic::Status::invalid_argument("bad page token"),
            tonic::Status::deadline_exceeded("too slow"),
        ] {
            assert!(
                !is_unimplemented(&status),
                "{code:?} must not fall back",
                code = status.code()
            );
        }
    }
}
