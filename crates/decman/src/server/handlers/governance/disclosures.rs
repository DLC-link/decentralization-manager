//! Disclosed contracts that a paid accept and an AmuletRules action need.

use std::collections::HashSet;

use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD};
use canton_proto_rs::com::daml::ledger::api::v2::{CreatedEvent, DisclosedContract, value};
use decman_lib::framework::record::{
    extract_contract_id, field_contract_id, field_record, record_field,
};

use crate::{
    config::NodeConfig,
    error::Result,
    server::{
        handlers::token_standard::{AmuletRulesContract, fetch_amulet_rules},
        scan::{self, OpenMiningRound, ScanNotConfigured},
    },
};

/// The `AmuletRules` disclosure for a `RequestDevNetFeaturedAppRight` or
/// `AcceptPaidCredential` execute.
///
/// Only the DSO can see `AmuletRules`, so the executor discloses the current
/// contract from the DSO scan API. The proposal pins a contract id. When the
/// DSO has replaced its `AmuletRules` since, the pinned contract is archived
/// and the execute can never succeed, so report that instead of disclosing a
/// contract the choice will not read.
pub(super) fn amulet_rules_disclosure(
    proposed_cid: Option<&str>,
    current: &AmuletRulesContract,
) -> Result<DisclosedContract> {
    let proposed_cid = proposed_cid.context("The proposal has no amuletRulesCid")?;
    if proposed_cid != current.contract_id {
        anyhow::bail!(
            "The proposal names AmuletRules {proposed_cid}, but the DSO's current AmuletRules is {}. Cancel this proposal and propose again.",
            current.contract_id
        );
    }
    disclosure(
        &current.contract_id,
        &current.created_event_blob,
        "the DSO's AmuletRules",
    )
}

/// A disclosed contract from a base64 created-event blob. `what` names the
/// contract in the error.
fn disclosure(contract_id: &str, blob: &str, what: &str) -> Result<DisclosedContract> {
    Ok(DisclosedContract {
        template_id: None,
        contract_id: contract_id.to_owned(),
        created_event_blob: STANDARD
            .decode(blob)
            .with_context(|| format!("Invalid base64 in the created_event_blob of {what}"))?,
        synchronizer_id: String::new(),
    })
}

/// The DSO contracts an `AcceptPaidCredential` proposal pins in its
/// `appTransferContext`.
#[derive(Debug)]
pub(super) struct PaidAcceptPins {
    amulet_rules: String,
    open_mining_round: String,
    featured_app_right: Option<String>,
}

impl PaidAcceptPins {
    pub(super) fn of(created: &CreatedEvent) -> Result<Self> {
        let context = created
            .create_arguments
            .as_ref()
            .and_then(|args| field_record(args, "appTransferContext"))
            .context("The AcceptPaidCredential proposal has no appTransferContext")?;
        let featured_app_right = match record_field(context, "featuredAppRight") {
            Some(value::Sum::Optional(optional)) => optional
                .value
                .as_deref()
                .map(extract_contract_id)
                .transpose()
                .context("The AcceptPaidCredential featuredAppRight is not a contract id")?,
            _ => None,
        };
        Ok(Self {
            amulet_rules: field_contract_id(context, "amuletRules")
                .context("The AcceptPaidCredential proposal has no amuletRules")?,
            open_mining_round: field_contract_id(context, "openMiningRound")
                .context("The AcceptPaidCredential proposal has no openMiningRound")?,
            featured_app_right,
        })
    }
}

/// The disclosures an `AcceptPaidCredential` execute needs, minus those the
/// request already carries. The governance party sees none of the three
/// contracts. `AmuletRules` comes from the DSO API, the rest from scan.
///
/// # Errors
/// Returns [`ScanNotConfigured`] when scan must be read and `DECPM_SCAN_URL`
/// is not set. Returns an error when a pinned contract is no longer active.
pub(super) async fn paid_accept_disclosures(
    http_client: &reqwest::Client,
    config: &NodeConfig,
    pins: &PaidAcceptPins,
    requested: &HashSet<&str>,
) -> Result<Vec<DisclosedContract>> {
    let needs_round = !requested.contains(pins.open_mining_round.as_str());
    let featured_app_right = pins
        .featured_app_right
        .as_deref()
        .filter(|cid| !requested.contains(cid));
    let scan_url = if needs_round || featured_app_right.is_some() {
        Some(config.scan_url().ok_or(ScanNotConfigured)?)
    } else {
        None
    };

    let mut disclosed = Vec::new();
    if !requested.contains(pins.amulet_rules.as_str()) {
        let current = fetch_amulet_rules(http_client, config)
            .await
            .context("Failed to fetch AmuletRules from the DSO API")?;
        disclosed.push(amulet_rules_disclosure(Some(&pins.amulet_rules), &current)?);
    }
    let Some(scan_url) = scan_url else {
        return Ok(disclosed);
    };
    if needs_round {
        let rounds = scan::fetch_open_mining_rounds(http_client, scan_url)
            .await
            .context("Failed to fetch the open mining rounds from scan")?;
        disclosed.push(open_round_disclosure(&pins.open_mining_round, &rounds)?);
    }
    if let Some(cid) = featured_app_right {
        let right = scan::fetch_featured_app_right(http_client, scan_url, cid)
            .await
            .context("Failed to fetch the FeaturedAppRight from scan")?
            .with_context(|| {
                format!(
                    "The proposal names FeaturedAppRight {cid}, which scan does not list as active. Cancel this proposal and propose again."
                )
            })?;
        disclosed.push(disclosure(
            &right.contract_id,
            &right.created_event_blob,
            "the FeaturedAppRight",
        )?);
    }
    Ok(disclosed)
}

/// The disclosure of the open round a paid accept pins. Scan lists a round
/// until the DSO archives it. A pinned round missing from the list has
/// closed, and the accept can never execute.
fn open_round_disclosure(pinned: &str, rounds: &[OpenMiningRound]) -> Result<DisclosedContract> {
    let round = rounds
        .iter()
        .find(|r| r.contract.contract_id == pinned)
        .with_context(|| {
            format!(
                "The proposal pins open mining round {pinned}, which has closed. Cancel this proposal and propose again."
            )
        })?;
    disclosure(
        &round.contract.contract_id,
        &round.contract.created_event_blob,
        "the open mining round",
    )
}

#[cfg(test)]
mod paid_accept_disclosure_tests {
    use actix_web::{body::to_bytes, http::StatusCode};
    use base64::engine::general_purpose::STANDARD;
    use decman_lib::{
        catalog::{proposals::credential::AcceptPaidCredential, types::AppTransferContextInput},
        framework::DamlProtoEncode,
    };
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    use super::{super::actions::execute_failure_response, *};

    const ROUNDS: &str = include_str!("../../testdata/scan/open_and_issuing_mining_rounds.json");
    const FEATURED_APP_RIGHT: &str = include_str!("../../testdata/scan/featured_app_right.json");

    /// Round 66344 of the scan fixture.
    fn round_66344() -> scan::ScanContract {
        scan::parse_open_mining_rounds(ROUNDS)
            .expect("the fixture parses")
            .into_iter()
            .find(|r| r.round == 66344)
            .expect("the fixture lists round 66344")
            .contract
    }

    /// The create event of an `AcceptPaidCredential` proposal, encoded the
    /// way decman proposes it.
    fn proposal(open_mining_round: &str, featured_app_right: Option<&str>) -> CreatedEvent {
        let payload = AcceptPaidCredential {
            user_service_cid: "00usc".to_string(),
            credential_offer_cid: "00offer".to_string(),
            deposit_amulet_cids: vec!["00amulet".to_string()],
            app_transfer_context: AppTransferContextInput {
                amulet_rules_cid: "00rules".to_string(),
                open_mining_round_cid: open_mining_round.to_string(),
                featured_app_right_cid: featured_app_right.map(str::to_string),
            },
            deposit_amount_usd: None,
        }
        .to_daml_proto()
        .expect("the proposal encodes");
        let Some(value::Sum::Record(record)) = payload.sum else {
            panic!("a proposal encodes as a record");
        };
        CreatedEvent {
            create_arguments: Some(record),
            ..CreatedEvent::default()
        }
    }

    fn config(scan_url: Option<String>) -> NodeConfig {
        let mut config = NodeConfig::default();
        config.canton.scan_url = scan_url;
        config
    }

    async fn scan() -> MockServer {
        let scan = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/scan/v0/open-and-issuing-mining-rounds"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ROUNDS))
            .mount(&scan)
            .await;
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
        scan
    }

    /// Disclosures for `created` when the request already carries the
    /// AmuletRules, so no test reaches the public DSO API.
    async fn disclosures(
        created: &CreatedEvent,
        scan_url: Option<String>,
    ) -> Result<Vec<DisclosedContract>> {
        let pins = PaidAcceptPins::of(created)?;
        let requested = HashSet::from(["00rules"]);
        paid_accept_disclosures(
            &reqwest::Client::new(),
            &config(scan_url),
            &pins,
            &requested,
        )
        .await
    }

    #[test]
    fn pins_are_read_from_the_app_transfer_context() -> Result<()> {
        let pins = PaidAcceptPins::of(&proposal("00round", Some("00far")))?;
        assert_eq!(pins.amulet_rules, "00rules");
        assert_eq!(pins.open_mining_round, "00round");
        assert_eq!(pins.featured_app_right.as_deref(), Some("00far"));

        let pins = PaidAcceptPins::of(&proposal("00round", None))?;
        assert_eq!(pins.featured_app_right, None);
        Ok(())
    }

    #[test]
    fn a_proposal_without_a_transfer_context_is_an_error() {
        let err = PaidAcceptPins::of(&CreatedEvent::default()).expect_err("nothing to read");
        assert!(format!("{err:#}").contains("appTransferContext"));
    }

    #[tokio::test]
    async fn the_pinned_round_and_featured_app_right_come_from_scan() -> Result<()> {
        let scan = scan().await;
        let round = round_66344();

        let disclosed = disclosures(
            &proposal(&round.contract_id, Some("00far")),
            Some(format!("{}/api/scan", scan.uri())),
        )
        .await?;

        let cids: Vec<&str> = disclosed.iter().map(|d| d.contract_id.as_str()).collect();
        assert_eq!(cids, [round.contract_id.as_str(), "00far"]);
        // The blobs are the decoded `created_event_blob`s scan served.
        assert_eq!(
            disclosed[0].created_event_blob,
            STANDARD.decode(&round.created_event_blob)?
        );
        assert_eq!(disclosed[1].created_event_blob, b"far");
        Ok(())
    }

    #[tokio::test]
    async fn without_a_scan_url_the_accept_is_refused_as_not_configured() {
        let err = disclosures(&proposal(&round_66344().contract_id, None), None)
            .await
            .expect_err("the round must come from scan");

        assert!(err.downcast_ref::<ScanNotConfigured>().is_some(), "{err:#}");
        let response = execute_failure_response(&err);
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn without_a_scan_url_a_request_that_discloses_everything_still_executes() -> Result<()> {
        let pins = PaidAcceptPins::of(&proposal("00round", Some("00far")))?;
        let requested = HashSet::from(["00rules", "00round", "00far"]);

        let disclosed =
            paid_accept_disclosures(&reqwest::Client::new(), &config(None), &pins, &requested)
                .await?;

        assert!(disclosed.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn a_pinned_round_scan_no_longer_lists_has_closed() {
        let scan = scan().await;

        let err = disclosures(
            &proposal("00closed", None),
            Some(format!("{}/api/scan", scan.uri())),
        )
        .await
        .expect_err("a closed round cannot be disclosed");

        let message = format!("{err:#}");
        assert!(message.contains("00closed"), "{message}");
        assert!(message.contains("propose again"), "{message}");
    }

    #[tokio::test]
    async fn a_featured_app_right_scan_does_not_list_is_refused() {
        let scan = scan().await;

        let err = disclosures(
            &proposal(&round_66344().contract_id, Some("00gone")),
            Some(format!("{}/api/scan", scan.uri())),
        )
        .await
        .expect_err("an inactive right cannot be disclosed");

        let message = format!("{err:#}");
        assert!(message.contains("00gone"), "{message}");
    }

    #[actix_web::test]
    async fn any_other_execute_failure_stays_a_500() -> Result<()> {
        let response = execute_failure_response(&anyhow::anyhow!("ledger said no"));

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = to_bytes(response.into_body())
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let body: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(body["error"], "Failed to execute action: ledger said no");
        Ok(())
    }

    #[actix_web::test]
    async fn an_execute_failure_names_its_underlying_cause() -> Result<()> {
        let error = anyhow::anyhow!("scan answered 403 Forbidden: access denied")
            .context("Failed to fetch the open mining rounds from scan");
        let response = execute_failure_response(&error);

        let body = to_bytes(response.into_body())
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let body: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(
            body["error"],
            "Failed to execute action: Failed to fetch the open mining rounds from scan: \
             scan answered 403 Forbidden: access denied"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canton_id::CantonId;

    fn amulet_rules(contract_id: &str, created_event_blob: &str) -> AmuletRulesContract {
        AmuletRulesContract {
            dso: CantonId::parse(
                "dso::1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892",
            )
            .expect("valid canton id"),
            contract_id: contract_id.to_owned(),
            created_event_blob: created_event_blob.to_owned(),
        }
    }

    /// "blob" in base64.
    const BLOB_B64: &str = "YmxvYg==";

    #[test]
    fn amulet_rules_disclosure_discloses_the_current_contract_when_the_proposal_matches() {
        let current = amulet_rules("00amulet", BLOB_B64);
        let disclosed = amulet_rules_disclosure(Some("00amulet"), &current)
            .expect("a matching cid is disclosed");
        assert_eq!(disclosed.contract_id, "00amulet");
        assert_eq!(disclosed.created_event_blob, b"blob");
        assert!(disclosed.template_id.is_none());
    }

    #[test]
    fn amulet_rules_disclosure_names_both_cids_when_the_dso_replaced_amulet_rules() {
        let current = amulet_rules("00new", BLOB_B64);
        let err = amulet_rules_disclosure(Some("00old"), &current)
            .expect_err("a stale proposal cannot be executed");
        let message = format!("{err:#}");
        assert!(message.contains("00old"), "{message}");
        assert!(message.contains("00new"), "{message}");
    }

    #[test]
    fn amulet_rules_disclosure_rejects_a_proposal_without_amulet_rules_cid() {
        let current = amulet_rules("00amulet", BLOB_B64);
        assert!(amulet_rules_disclosure(None, &current).is_err());
    }

    #[test]
    fn amulet_rules_disclosure_rejects_a_malformed_blob() {
        let current = amulet_rules("00amulet", "not base64!");
        assert!(amulet_rules_disclosure(Some("00amulet"), &current).is_err());
    }
}
