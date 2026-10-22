//! Reads from a Splice scan API, the one `DECPM_SCAN_URL` names.
//!
//! A paid credential accept pins an open mining round and may name a
//! featured app right. Only the DSO and the app provider see those contracts,
//! so the execute submission must disclose them. Scan serves each contract
//! with its created-event blob.

use std::collections::HashMap;

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use serde::Deserialize;

/// `DECPM_SCAN_URL` is not set, so this node cannot read from scan.
#[derive(Debug)]
pub(crate) struct ScanNotConfigured;

impl std::fmt::Display for ScanNotConfigured {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "DECPM_SCAN_URL is not set, so this node cannot read the open mining round. \
             Set it to a Splice scan API to accept a paid credential.",
        )
    }
}

impl std::error::Error for ScanNotConfigured {}

/// A contract as scan serves it.
#[derive(Debug, Deserialize)]
pub(crate) struct ScanContract {
    pub contract_id: String,
    /// Base64 created-event blob, ready to disclose on a submission.
    pub created_event_blob: String,
}

/// A `Splice.Round:OpenMiningRound` contract from scan.
#[derive(Debug)]
pub(crate) struct OpenMiningRound {
    pub contract: ScanContract,
    pub round: u64,
    pub opens_at: DateTime<Utc>,
    pub target_closes_at: DateTime<Utc>,
}

/// `GetOpenAndIssuingMiningRoundsResponse`. The issuing rounds are not read.
#[derive(Deserialize)]
struct OpenAndIssuingMiningRoundsResponse {
    open_mining_rounds: HashMap<String, MaybeCachedContract>,
}

/// `MaybeCachedContractWithState`. Scan leaves `contract` out for an id the
/// request listed as cached. Decman lists none, but a round without its
/// contract cannot be disclosed, so the parser skips it.
#[derive(Deserialize)]
struct MaybeCachedContract {
    contract: Option<RoundContract>,
}

#[derive(Deserialize)]
struct RoundContract {
    #[serde(flatten)]
    contract: ScanContract,
    payload: RoundPayload,
}

/// The `OpenMiningRound` fields decman reads. Scan encodes `Int64` and `Time`
/// as JSON strings.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RoundPayload {
    round: RoundNumber,
    opens_at: String,
    target_closes_at: String,
}

#[derive(Deserialize)]
struct RoundNumber {
    number: String,
}

/// `LookupFeaturedAppRightResponse`.
#[derive(Deserialize)]
struct LookupFeaturedAppRightResponse {
    featured_app_right: Option<ScanContract>,
}

/// Parse a `/v0/open-and-issuing-mining-rounds` response into its open rounds.
///
/// # Errors
/// Returns an error when the body is not that response, or a round's number
/// or times do not parse.
pub(crate) fn parse_open_mining_rounds(body: &str) -> anyhow::Result<Vec<OpenMiningRound>> {
    let response: OpenAndIssuingMiningRoundsResponse = serde_json::from_str(body)
        .context("Scan answered with an unexpected open mining rounds response")?;
    let mut rounds = response
        .open_mining_rounds
        .into_values()
        .filter_map(|entry| entry.contract)
        .map(|RoundContract { contract, payload }| {
            let time = |label: &str, value: &str| {
                DateTime::parse_from_rfc3339(value)
                    .map(|t| t.with_timezone(&Utc))
                    .with_context(|| {
                        format!(
                            "Round {} has an invalid {label}: {value}",
                            contract.contract_id
                        )
                    })
            };
            Ok(OpenMiningRound {
                round: payload.round.number.parse().with_context(|| {
                    format!(
                        "Round {} has an invalid number: {}",
                        contract.contract_id, payload.round.number
                    )
                })?,
                opens_at: time("opensAt", &payload.opens_at)?,
                target_closes_at: time("targetClosesAt", &payload.target_closes_at)?,
                contract,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    rounds.sort_by_key(|r| r.round);
    Ok(rounds)
}

/// The newest round that is open at `now`: it has opened, and its target
/// close time has not passed. Scan also lists the next round before it opens,
/// so the highest round number is not always open.
pub(crate) fn newest_open_round(
    rounds: &[OpenMiningRound],
    now: DateTime<Utc>,
) -> Option<&OpenMiningRound> {
    rounds
        .iter()
        .filter(|r| r.opens_at <= now && now < r.target_closes_at)
        .max_by_key(|r| r.round)
}

/// Every open mining round scan lists, oldest first.
///
/// # Errors
/// Returns an error when scan cannot be reached, answers with an error
/// status, or answers with a body that does not parse.
pub(crate) async fn fetch_open_mining_rounds(
    http_client: &reqwest::Client,
    scan_url: &str,
) -> anyhow::Result<Vec<OpenMiningRound>> {
    let res = http_client
        .post(format!("{scan_url}/v0/open-and-issuing-mining-rounds"))
        .json(&serde_json::json!({
            "cached_open_mining_round_contract_ids": [],
            "cached_issuing_round_contract_ids": [],
        }))
        .send()
        .await
        .context("Failed to reach the scan API")?;
    parse_open_mining_rounds(&success_body(res).await?)
}

/// The `FeaturedAppRight` with this contract id, or `None` when scan does not
/// know it as an active contract.
///
/// # Errors
/// As [`fetch_open_mining_rounds`].
pub(crate) async fn fetch_featured_app_right(
    http_client: &reqwest::Client,
    scan_url: &str,
    contract_id: &str,
) -> anyhow::Result<Option<ScanContract>> {
    let res = http_client
        .get(format!(
            "{scan_url}/v0/featured-apps/by-contract-id/{contract_id}"
        ))
        .send()
        .await
        .context("Failed to reach the scan API")?;
    let response: LookupFeaturedAppRightResponse = serde_json::from_str(&success_body(res).await?)
        .context("Scan answered with an unexpected featured app right response")?;
    Ok(response.featured_app_right)
}

async fn success_body(res: reqwest::Response) -> anyhow::Result<String> {
    let status = res.status();
    let body = res
        .text()
        .await
        .context("Failed to read the scan response")?;
    if !status.is_success() {
        anyhow::bail!("Scan API returned {status}: {body}");
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, method, path},
    };

    use super::*;

    /// Three `OpenMiningRound` contracts in the shape scan's
    /// `/v0/open-and-issuing-mining-rounds` returns. The contracts are real
    /// DevNet rounds 66343 to 66345, read from the DSO's public `/dso` page.
    const ROUNDS: &str = include_str!("testdata/scan/open_and_issuing_mining_rounds.json");

    /// A `/v0/featured-apps/by-contract-id` answer, in the shape the scan
    /// OpenAPI gives.
    const FEATURED_APP_RIGHT: &str = include_str!("testdata/scan/featured_app_right.json");

    const ROUND_66343: &str = "00daa21cd6810308cb0d541a0f7efba0332eaac91bb1845b98bb918b454c51e89bca12122018a77e59114857735d29eca844e521e734a76d3e5e7e463274d9272602f7f6f8";

    fn at(time: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(time)
            .expect("a valid test time")
            .with_timezone(&Utc)
    }

    fn rounds() -> Vec<OpenMiningRound> {
        parse_open_mining_rounds(ROUNDS).expect("the fixture parses")
    }

    #[test]
    fn parses_every_open_round_with_its_blob_and_times() {
        let rounds = rounds();

        assert_eq!(
            rounds.iter().map(|r| r.round).collect::<Vec<_>>(),
            [66343, 66344, 66345]
        );
        let first = &rounds[0];
        assert_eq!(first.contract.contract_id, ROUND_66343);
        assert!(first.contract.created_event_blob.starts_with("CgMyLjES"));
        assert_eq!(first.opens_at, at("2026-10-21T13:54:17.279730Z"));
        assert_eq!(first.target_closes_at, at("2026-10-21T14:14:17.279730Z"));
    }

    #[test]
    fn the_newest_open_round_skips_the_round_that_has_not_opened() {
        let rounds = rounds();

        // 66343 and 66344 are open; 66345 opens at 14:14:51.
        let picked = newest_open_round(&rounds, at("2026-10-21T14:10:00Z"));
        assert_eq!(picked.map(|r| r.round), Some(66344));
    }

    #[test]
    fn the_newest_open_round_moves_on_once_the_next_round_opens() {
        let rounds = rounds();

        // 66345 opened at 14:14:51, and 66344 is still open.
        let picked = newest_open_round(&rounds, at("2026-10-21T14:20:00Z"));
        assert_eq!(picked.map(|r| r.round), Some(66345));
    }

    #[test]
    fn a_round_past_its_target_close_is_not_open() {
        let rounds = rounds();

        // 66345, the newest round, closes at 14:34:51.
        assert!(newest_open_round(&rounds, at("2026-10-21T14:40:00Z")).is_none());
    }

    #[test]
    fn a_round_opens_at_its_opens_at_and_closes_at_its_target() {
        let rounds = rounds();
        let r66345 = &rounds[2];

        let picked = newest_open_round(&rounds, r66345.opens_at);
        assert_eq!(picked.map(|r| r.round), Some(66345));
        assert!(newest_open_round(&rounds, r66345.target_closes_at).is_none());
    }

    #[test]
    fn no_round_is_open_before_the_first_one_opens() {
        let rounds = rounds();

        assert!(newest_open_round(&rounds, at("2026-10-21T13:50:00Z")).is_none());
    }

    #[test]
    fn a_round_listed_without_its_contract_is_skipped() {
        let body = serde_json::json!({
            "time_to_live_in_microseconds": 60_000_000,
            "open_mining_rounds": { ROUND_66343: { "domain_id": "global-domain::1220" } },
            "issuing_mining_rounds": {},
        });

        let rounds = parse_open_mining_rounds(&body.to_string()).expect("the body parses");
        assert!(rounds.is_empty());
    }

    #[test]
    fn a_body_without_open_rounds_is_an_error() {
        let err = parse_open_mining_rounds(r#"{"error":"not found"}"#)
            .expect_err("the body is not a rounds response");
        assert!(format!("{err:#}").contains("unexpected open mining rounds response"));
    }

    #[tokio::test]
    async fn fetches_the_open_rounds_without_a_cache() {
        let scan = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/scan/v0/open-and-issuing-mining-rounds"))
            .and(body_json(serde_json::json!({
                "cached_open_mining_round_contract_ids": [],
                "cached_issuing_round_contract_ids": [],
            })))
            .respond_with(ResponseTemplate::new(200).set_body_string(ROUNDS))
            .expect(1)
            .mount(&scan)
            .await;

        let rounds =
            fetch_open_mining_rounds(&reqwest::Client::new(), &format!("{}/api/scan", scan.uri()))
                .await
                .expect("scan answers");
        assert_eq!(rounds.len(), 3);
    }

    #[tokio::test]
    async fn a_scan_error_status_is_reported_with_its_body() {
        let scan = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503).set_body_string("scan is down"))
            .mount(&scan)
            .await;

        let err = fetch_open_mining_rounds(&reqwest::Client::new(), &scan.uri())
            .await
            .expect_err("a 503 is an error");
        let message = format!("{err:#}");
        assert!(message.contains("503"), "{message}");
        assert!(message.contains("scan is down"), "{message}");
    }

    #[tokio::test]
    async fn fetches_a_featured_app_right_by_its_contract_id() {
        let scan = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/scan/v0/featured-apps/by-contract-id/00far"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FEATURED_APP_RIGHT))
            .mount(&scan)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/scan/v0/featured-apps/by-contract-id/00gone"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&scan)
            .await;
        let scan_url = format!("{}/api/scan", scan.uri());
        let client = reqwest::Client::new();

        let right = fetch_featured_app_right(&client, &scan_url, "00far")
            .await
            .expect("scan answers")
            .expect("scan knows the right");
        assert_eq!(right.contract_id, "00far");
        assert_eq!(right.created_event_blob, "ZmFy");

        let gone = fetch_featured_app_right(&client, &scan_url, "00gone")
            .await
            .expect("scan answers");
        assert!(gone.is_none());
    }
}
