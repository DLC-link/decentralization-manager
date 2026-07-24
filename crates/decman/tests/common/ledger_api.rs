use anyhow::Context;
use serde_json::{Value, json};

use super::Fixture;

/// JSON Ledger API HTTP ports for the three localnet participants (fixed by the
/// Splice localnet bundle; also used by `deploy_gov_core` for party/rights setup).
pub const P1_JSON_API: u16 = 3975;
pub const P2_JSON_API: u16 = 2975;
pub const P3_JSON_API: u16 = 4975;

/// `RewardCouponV2` addressed by package-NAME (`#splice-amulet`) so Canton
/// resolves whatever version the localnet bundle has vetted.
pub const REWARD_COUPON_V2_TEMPLATE: &str = "#splice-amulet:Splice.Amulet:RewardCouponV2";

/// Parameters for one seeded unassigned coupon.
pub struct SeedCoupon {
    pub dso: String,
    pub provider: String,
    pub amount: String,
    pub expires_at: String,
    pub round: i64,
}

/// Build the `/v2/commands/submit-and-wait` body creating an UNASSIGNED
/// `RewardCouponV2` (beneficiary = null, providerIsObserver = true so the
/// provider/decparty can see and reassign it). Submitted as `ledger-api-user`
/// acting as the coupon's sole signatory, `dso`.
pub fn reward_coupon_create_command(c: &SeedCoupon, command_id: &str) -> Value {
    json!({
        "commands": [{
            "CreateCommand": {
                "templateId": REWARD_COUPON_V2_TEMPLATE,
                "createArguments": {
                    "dso": c.dso,
                    "provider": c.provider,
                    "round": { "number": c.round.to_string() },
                    "amount": c.amount,
                    "expiresAt": c.expires_at,
                    "providerIsObserver": true,
                    "beneficiary": Value::Null,
                }
            }
        }],
        "commandId": command_id,
        "userId": "ledger-api-user",
        "actAs": [c.dso],
        "readAs": [c.dso],
    })
}

/// Build the `/v2/state/active-contracts` body: active `RewardCouponV2`
/// contracts visible to `party`, as of `offset`.
pub fn active_contracts_request(party: &str, template_id: &str, offset: i64) -> Value {
    json!({
        "eventFormat": {
            "filtersByParty": {
                party: {
                    "cumulative": [{
                        "identifierFilter": {
                            "TemplateFilter": {
                                "value": {
                                    "templateId": template_id,
                                    "includeCreatedEventBlob": false,
                                }
                            }
                        }
                    }]
                }
            },
            "verbose": false
        },
        "verbose": false,
        "activeAtOffset": offset,
    })
}

/// Extract `(beneficiary, amount)` from each active `RewardCouponV2` in an ACS
/// response. `beneficiary` is `None` for an unassigned coupon.
pub fn parse_coupon_amounts(acs_response: &Value) -> Vec<(Option<String>, String)> {
    acs_response
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let payload =
                entry.pointer("/contractEntry/JsActiveContract/createdEvent/createArgument")?;
            let amount = payload.get("amount")?.as_str()?.to_string();
            let beneficiary = payload
                .get("beneficiary")
                .and_then(|b| b.as_str())
                .map(|s| s.to_string());
            Some((beneficiary, amount))
        })
        .collect()
}

/// True when `beneficiary_total` is ~4x `operator_total` (the 0.8 / 0.2 split),
/// within `tolerance` (absolute, on the ratio).
pub fn split_ok(beneficiary_total: f64, operator_total: f64, tolerance: f64) -> bool {
    if operator_total <= 0.0 {
        return false;
    }
    (beneficiary_total / operator_total - 4.0).abs() <= tolerance
}

/// Normalize the `/v2/state/active-contracts` response body into a JSON array.
/// The endpoint may return a JSON array or newline-delimited JSON objects
/// depending on the Canton version; both become a `Value::Array` here so
/// `parse_coupon_amounts` has one shape to read.
fn normalize_acs_body(text: &str) -> anyhow::Result<Value> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Value::Array(vec![]));
    }
    if trimmed.starts_with('[') {
        return serde_json::from_str(trimmed).context("parse ACS array body");
    }
    let items = trimmed
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).context("parse ACS ndjson line"))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Value::Array(items))
}

impl Fixture {
    /// POST a create/exercise command to a participant's JSON Ledger API,
    /// returning the raw JSON response. Uses the same bearer as the other
    /// harness calls (localnet MOCK_TOKEN).
    pub async fn submit_create(&self, port: u16, body: &Value) -> anyhow::Result<Value> {
        self.post_json(port, "/v2/commands/submit-and-wait", body)
            .await
            .context("POST /v2/commands/submit-and-wait")
    }

    /// GET the participant's current ledger-end offset.
    pub async fn ledger_end(&self, port: u16) -> anyhow::Result<i64> {
        let r: Value = self
            .get_json(port, "/v2/state/ledger-end")
            .await
            .context("GET /v2/state/ledger-end")?;
        r.get("offset")
            .and_then(|o| o.as_i64())
            .context("ledger-end response missing integer offset")
    }

    /// Read the decoded active `RewardCouponV2` coupons visible to `party` as of
    /// the current ledger end, returning `(beneficiary, amount)` pairs. Fetches
    /// the raw body and normalizes array-or-NDJSON before parsing, so a
    /// Canton-version difference in the response framing does not break it.
    pub async fn active_reward_coupons(
        &self,
        port: u16,
        party: &str,
    ) -> anyhow::Result<Vec<(Option<String>, String)>> {
        let offset = self.ledger_end(port).await?;
        let body = active_contracts_request(party, REWARD_COUPON_V2_TEMPLATE, offset);
        let (status, text) = self
            .post_expect_status(port, "/v2/state/active-contracts", &body)
            .await
            .context("POST /v2/state/active-contracts")?;
        if !status.is_success() {
            anyhow::bail!("POST /v2/state/active-contracts returned {status}: {text}");
        }
        Ok(parse_coupon_amounts(&normalize_acs_body(&text)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_command_shapes_unassigned_coupon() {
        let c = SeedCoupon {
            dso: "dso::1220".into(),
            provider: "decparty::1220".into(),
            amount: "100.0".into(),
            expires_at: "2026-07-24T20:00:00Z".into(),
            round: 0,
        };
        let v = reward_coupon_create_command(&c, "seed-1");
        let cmd = &v["commands"][0]["CreateCommand"];
        assert_eq!(cmd["templateId"], REWARD_COUPON_V2_TEMPLATE);
        let args = &cmd["createArguments"];
        assert_eq!(args["dso"], "dso::1220");
        assert_eq!(args["provider"], "decparty::1220");
        assert_eq!(args["amount"], "100.0");
        assert_eq!(args["expiresAt"], "2026-07-24T20:00:00Z");
        assert_eq!(args["round"]["number"], "0"); // Int encoded as string
        assert_eq!(args["providerIsObserver"], true);
        assert!(args["beneficiary"].is_null()); // unassigned
        assert_eq!(v["actAs"][0], "dso::1220"); // signatory = dso
        assert_eq!(v["userId"], "ledger-api-user");
        assert_eq!(v["commandId"], "seed-1");
    }

    #[test]
    fn active_contracts_request_filters_by_party_and_template() {
        // Distinct dummy template id (not REWARD_COUPON_V2_TEMPLATE) so this
        // proves the value is threaded through, not hardcoded.
        let v = active_contracts_request("benef::1220", "#dummy:Mod:Ent", 42);
        assert_eq!(v["activeAtOffset"], 42);
        let f = &v["eventFormat"]["filtersByParty"]["benef::1220"]["cumulative"][0]["identifierFilter"]
            ["TemplateFilter"]["value"];
        assert_eq!(f["templateId"], "#dummy:Mod:Ent");
    }

    #[test]
    fn parse_coupon_amounts_reads_beneficiary_and_amount() {
        // Mirrors the /v2/state/active-contracts POST response: an array of
        // entries, each { contractEntry: { JsActiveContract: { createdEvent:
        // { createArgument: <payload> } } } }.
        let resp = serde_json::json!([
            {"contractEntry": {"JsActiveContract": {"createdEvent": {
                "createArgument": {"beneficiary": "benef::1220", "amount": "80.0"}
            }}}},
            {"contractEntry": {"JsActiveContract": {"createdEvent": {
                "createArgument": {"beneficiary": "op::1220", "amount": "20.0"}
            }}}}
        ]);
        let mut got = parse_coupon_amounts(&resp);
        got.sort();
        let mut want = vec![
            (Some("benef::1220".to_string()), "80.0".to_string()),
            (Some("op::1220".to_string()), "20.0".to_string()),
        ];
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn split_ok_accepts_four_to_one() {
        assert!(split_ok(80.0, 20.0, 0.01));
        assert!(!split_ok(50.0, 50.0, 0.01));
    }

    #[test]
    fn normalize_acs_body_handles_array_and_ndjson() {
        let entry = r#"{"contractEntry":{"JsActiveContract":{"createdEvent":{"createArgument":{"beneficiary":"b::1","amount":"80.0"}}}}}"#;
        // JSON array form
        let arr = format!("[{entry}]");
        let v = normalize_acs_body(&arr).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        // newline-delimited form (two objects, one blank line)
        let ndjson = format!("{entry}\n\n{entry}\n");
        let v = normalize_acs_body(&ndjson).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
        // both parse into the same shape parse_coupon_amounts expects
        assert_eq!(parse_coupon_amounts(&v).len(), 2);
        // empty / whitespace-only body normalizes to an empty array
        assert!(
            normalize_acs_body("")
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            normalize_acs_body("  \n ")
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
