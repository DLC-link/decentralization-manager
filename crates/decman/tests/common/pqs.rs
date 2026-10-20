//! Read-only queries against a Participant Query Store (PQS) database.
//!
//! The devnet reward e2e reads devnet's `pqs_cbtc`, which projects the
//! `cbtc-network` decparty's view of the ledger. The SQL uses the PQS helper
//! functions (`active`, `creates`, `latest_offset`). They resolve a template by
//! package name, so a package upgrade does not change the queries. The checks
//! on the rows are pure functions with unit tests below.

use std::{str::FromStr, time::Duration};

use anyhow::Context;
use canton_common::decimal::DamlDecimal;
use serde_json::Value;
use sqlx::{
    Executor, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};

pub const REWARD_COUPON_V2: &str = "splice-amulet:Splice.Amulet:RewardCouponV2";
pub const COUPON_REASSIGNMENT_DELEGATION: &str = "governance-rewards-automation-v1:\
     Governance.Rewards.CouponReassignmentDelegation:CouponReassignmentDelegation";

/// One entry of a delegation's split.
#[derive(Debug, Clone, PartialEq)]
pub struct Share {
    pub beneficiary: String,
    pub percentage: DamlDecimal,
}

/// A decparty's active `CouponReassignmentDelegation`, decoded from PQS.
#[derive(Debug, Clone, PartialEq)]
pub struct DeployedDelegation {
    pub cid: String,
    pub dso: String,
    pub assigners: Vec<String>,
    pub split: Vec<Share>,
}

impl DeployedDelegation {
    /// Decodes the delegation's create arguments from a PQS `payload`.
    pub fn from_payload(cid: &str, payload: &Value) -> anyhow::Result<Self> {
        let party = |v: &Value, what: &str| -> anyhow::Result<String> {
            v.as_str()
                .map(str::to_string)
                .with_context(|| format!("delegation {cid}: {what} is not a party"))
        };
        let list = |field: &str| -> anyhow::Result<&Vec<Value>> {
            payload
                .get(field)
                .and_then(Value::as_array)
                .with_context(|| format!("delegation {cid}: `{field}` is not a list"))
        };
        let dso = party(payload.get("dso").unwrap_or(&Value::Null), "`dso`")?;
        let assigners = list("assigners")?
            .iter()
            .map(|a| party(a, "an assigner"))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let split = list("split")?
            .iter()
            .map(|entry| {
                let beneficiary = party(
                    entry.get("beneficiary").unwrap_or(&Value::Null),
                    "a beneficiary",
                )?;
                let percentage = entry
                    .get("percentage")
                    .and_then(numeric_text)
                    .with_context(|| {
                        format!("delegation {cid}: the share of {beneficiary} is not a number")
                    })?;
                let percentage = DamlDecimal::parse(&percentage).map_err(|e| {
                    anyhow::anyhow!("delegation {cid}: the share of {beneficiary}: {e}")
                })?;
                Ok(Share {
                    beneficiary,
                    percentage,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        anyhow::ensure!(!split.is_empty(), "delegation {cid} names no beneficiaries");
        Ok(Self {
            cid: cid.to_string(),
            dso,
            assigners,
            split,
        })
    }
}

/// A Daml `Numeric` as text. PQS writes it as a JSON string; a JSON number is
/// accepted too.
fn numeric_text(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::to_string)
        .or_else(|| v.as_number().map(ToString::to_string))
}

/// The coupons assigned to one beneficiary, summed.
#[derive(Debug, Clone, PartialEq)]
pub struct AssignedTotal {
    pub beneficiary: String,
    pub coupons: i64,
    pub amount: DamlDecimal,
}

/// What a split check saw when it passed.
#[derive(Debug, Clone, PartialEq)]
pub struct SplitSeen {
    pub coupons_per_beneficiary: i64,
    pub total: DamlDecimal,
}

/// The largest gap allowed between a beneficiary's total and its share of the
/// grand total.
///
/// `RewardCoupon_AssignBeneficiaries` creates `amount * percentage` for each
/// beneficiary, rounded to 10 decimal places. Each of `coupons` original
/// coupons therefore adds at most half a unit in the last place to each of the
/// `beneficiaries` totals. The bound below allows twice that, plus one unit for
/// rounding the expected share itself.
pub fn share_tolerance(coupons: i64, beneficiaries: usize) -> anyhow::Result<DamlDecimal> {
    let width = i64::try_from(beneficiaries).context("beneficiary count")?;
    let units = coupons
        .checked_mul(width + 1)
        .and_then(|u| u.checked_add(1))
        .context("tolerance overflows")?;
    // `units` in the tenth decimal place, written out exactly.
    let text = format!("{}.{:010}", units / 10_000_000_000, units % 10_000_000_000);
    DamlDecimal::parse(&text).map_err(|e| anyhow::anyhow!("tolerance {text}: {e}"))
}

/// Checks the assigned coupons against the delegation's split by value.
///
/// Every assigned coupon must go to a beneficiary the split names, and every
/// beneficiary must have coupons. Each original coupon yields one coupon per
/// beneficiary, so all beneficiaries must hold the same count. Each
/// beneficiary's total must match its percentage of the grand total within
/// [`share_tolerance`].
pub fn check_split(split: &[Share], assigned: &[AssignedTotal]) -> anyhow::Result<SplitSeen> {
    anyhow::ensure!(!split.is_empty(), "the delegation's split is empty");
    if let Some(stray) = assigned
        .iter()
        .find(|a| !split.iter().any(|s| s.beneficiary == a.beneficiary))
    {
        anyhow::bail!(
            "{} coupon(s) went to {}, which the delegation's split does not name",
            stray.coupons,
            stray.beneficiary
        );
    }
    let mut rows = Vec::with_capacity(split.len());
    for share in split {
        let row = assigned
            .iter()
            .find(|a| a.beneficiary == share.beneficiary)
            .with_context(|| {
                format!(
                    "no assigned coupon for {}, which the split names at {}",
                    share.beneficiary, share.percentage
                )
            })?;
        rows.push((share, row));
    }
    let (first, first_row) = rows[0];
    if let Some((share, row)) = rows.iter().find(|(_, r)| r.coupons != first_row.coupons) {
        anyhow::bail!(
            "{} holds {} assigned coupon(s) but {} holds {}; each assigned coupon must yield \
             one coupon per beneficiary",
            first.beneficiary,
            first_row.coupons,
            share.beneficiary,
            row.coupons
        );
    }
    let total: DamlDecimal = rows.iter().map(|(_, r)| r.amount).sum();
    let tolerance = share_tolerance(first_row.coupons, split.len())?;
    for (share, row) in &rows {
        let expected = total * share.percentage;
        let gap = row.amount - expected;
        let gap = if gap < DamlDecimal::ZERO { -gap } else { gap };
        anyhow::ensure!(
            gap <= tolerance,
            "{} received {} of {total}, expected {expected} ({} of the total); off by {gap}, \
             tolerance {tolerance}",
            share.beneficiary,
            row.amount,
            share.percentage
        );
    }
    Ok(SplitSeen {
        coupons_per_beneficiary: first_row.coupons,
        total,
    })
}

fn decimal(text: &str, what: &str) -> anyhow::Result<DamlDecimal> {
    DamlDecimal::parse(text).map_err(|e| anyhow::anyhow!("{what} {text:?}: {e}"))
}

/// A read-only connection pool to one PQS database.
#[derive(Clone)]
pub struct Pqs {
    pool: PgPool,
}

impl Pqs {
    /// Connects to `url`. Every session is read-only, so no query can write to
    /// the shared database. Errors never echo `url`, which carries a password.
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let options = PgConnectOptions::from_str(url)
            .map_err(|e| anyhow::anyhow!("DECPM_IT_PQS_URL is not a Postgres URL: {e}"))?
            .application_name("decman-it");
        // A `SET` rather than a startup option: a connection pooler in front of
        // Postgres may refuse startup options.
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(Duration::from_secs(30))
            .after_connect(|conn, _| {
                Box::pin(async move {
                    conn.execute("SET default_transaction_read_only = on")
                        .await
                        .map(|_| ())
                })
            })
            .connect_with(options)
            .await
            .context("connect to PQS at DECPM_IT_PQS_URL")?;
        Ok(Self { pool })
    }

    /// The highest ledger offset PQS has ingested.
    pub async fn latest_offset(&self) -> anyhow::Result<i64> {
        sqlx::query_scalar("SELECT latest_offset()")
            .fetch_one(&self.pool)
            .await
            .context("PQS latest_offset()")
    }

    /// The decparty's active delegations, newest first.
    pub async fn active_delegations(
        &self,
        decparty: &str,
    ) -> anyhow::Result<Vec<DeployedDelegation>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT contract_id, payload::text FROM active($1::text) \
             WHERE payload->>'decparty' = $2 \
             ORDER BY created_at_offset DESC",
        )
        .bind(COUPON_REASSIGNMENT_DELEGATION)
        .bind(decparty)
        .fetch_all(&self.pool)
        .await
        .context("PQS active CouponReassignmentDelegation")?;
        rows.iter()
            .map(|(cid, payload)| {
                let payload: Value = serde_json::from_str(payload)
                    .with_context(|| format!("delegation {cid}: payload is not JSON"))?;
                DeployedDelegation::from_payload(cid, &payload)
            })
            .collect()
    }

    /// Count and total of the decparty's active unassigned coupons from `dso`.
    pub async fn unassigned_coupons(
        &self,
        decparty: &str,
        dso: &str,
    ) -> anyhow::Result<(i64, DamlDecimal)> {
        let (count, total): (i64, String) = sqlx::query_as(
            "SELECT count(*), coalesce(sum((payload->>'amount')::numeric), 0)::text \
             FROM active($1::text) \
             WHERE payload->>'provider' = $2 AND payload->>'dso' = $3 \
               AND payload->>'beneficiary' IS NULL",
        )
        .bind(REWARD_COUPON_V2)
        .bind(decparty)
        .bind(dso)
        .fetch_one(&self.pool)
        .await
        .context("PQS active unassigned RewardCouponV2")?;
        Ok((count, decimal(&total, "unassigned total")?))
    }

    /// The decparty's coupons from `dso` created with a beneficiary after
    /// `offset`, summed per beneficiary.
    ///
    /// It reads creates, not archives, so a coupon counts whether or not its
    /// beneficiary has minted it yet.
    pub async fn assigned_since(
        &self,
        decparty: &str,
        dso: &str,
        offset: i64,
    ) -> anyhow::Result<Vec<AssignedTotal>> {
        let rows: Vec<(String, i64, String)> = sqlx::query_as(
            "SELECT payload->>'beneficiary', count(*), sum((payload->>'amount')::numeric)::text \
             FROM creates($1::text, $2::bigint) \
             WHERE created_at_offset > $2 \
               AND payload->>'provider' = $3 AND payload->>'dso' = $4 \
               AND payload->>'beneficiary' IS NOT NULL \
             GROUP BY 1 ORDER BY 1",
        )
        .bind(REWARD_COUPON_V2)
        .bind(offset)
        .bind(decparty)
        .bind(dso)
        .fetch_all(&self.pool)
        .await
        .context("PQS assigned RewardCouponV2 since the run started")?;
        rows.into_iter()
            .map(|(beneficiary, coupons, amount)| {
                let amount = decimal(&amount, "assigned total")?;
                Ok(AssignedTotal {
                    beneficiary,
                    coupons,
                    amount,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn d(s: &str) -> DamlDecimal {
        DamlDecimal::parse(s).expect("test decimal")
    }

    fn share(beneficiary: &str, pct: &str) -> Share {
        Share {
            beneficiary: beneficiary.to_string(),
            percentage: d(pct),
        }
    }

    fn row(beneficiary: &str, coupons: i64, amount: &str) -> AssignedTotal {
        AssignedTotal {
            beneficiary: beneficiary.to_string(),
            coupons,
            amount: d(amount),
        }
    }

    fn eighty_twenty() -> Vec<Share> {
        vec![share("benef::1220", "0.8"), share("operator::1220", "0.2")]
    }

    /// One original coupon split the way Daml does it: each share rounded to
    /// 10 decimal places with banker's rounding.
    fn assign(amount: &str, split: &[Share]) -> Vec<DamlDecimal> {
        split.iter().map(|s| d(amount) * s.percentage).collect()
    }

    #[test]
    fn decodes_a_pqs_delegation_payload() {
        let payload = json!({
            "decparty": "cbtc-network::1220",
            "dso": "DSO::1220",
            "assigners": ["attestor-1::1220", "attestor-2::1220"],
            "split": [
                {"beneficiary": "benef::1220", "percentage": "0.8000000000"},
                {"beneficiary": "operator::1220", "percentage": "0.2000000000"},
            ],
        });
        let got = DeployedDelegation::from_payload("00del", &payload).expect("decodes");
        assert_eq!(
            got,
            DeployedDelegation {
                cid: "00del".to_string(),
                dso: "DSO::1220".to_string(),
                assigners: vec![
                    "attestor-1::1220".to_string(),
                    "attestor-2::1220".to_string()
                ],
                split: eighty_twenty(),
            }
        );
    }

    #[test]
    fn accepts_a_numeric_written_as_a_json_number() {
        let payload = json!({
            "dso": "DSO::1220",
            "assigners": ["attestor-1::1220"],
            "split": [{"beneficiary": "benef::1220", "percentage": 1.0}],
        });
        let got = DeployedDelegation::from_payload("00del", &payload).expect("decodes");
        assert_eq!(got.split, vec![share("benef::1220", "1")]);
    }

    #[test]
    fn rejects_a_payload_missing_its_split_or_shares() {
        let no_split = json!({"dso": "DSO::1220", "assigners": []});
        let err = DeployedDelegation::from_payload("00del", &no_split).unwrap_err();
        assert!(format!("{err:#}").contains("`split`"), "got: {err:#}");

        let empty = json!({"dso": "DSO::1220", "assigners": [], "split": []});
        let err = DeployedDelegation::from_payload("00del", &empty).unwrap_err();
        assert!(
            format!("{err:#}").contains("no beneficiaries"),
            "got: {err:#}"
        );

        let no_share = json!({
            "dso": "DSO::1220",
            "assigners": [],
            "split": [{"beneficiary": "benef::1220"}],
        });
        let err = DeployedDelegation::from_payload("00del", &no_share).unwrap_err();
        assert!(format!("{err:#}").contains("benef::1220"), "got: {err:#}");
    }

    #[test]
    fn tolerance_scales_with_coupons_and_beneficiaries() {
        assert_eq!(share_tolerance(0, 2).expect("tol"), d("0.0000000001"));
        assert_eq!(share_tolerance(10, 2).expect("tol"), d("0.0000000031"));
        // Written out exactly past one whole unit.
        assert_eq!(
            share_tolerance(5_000_000_000, 1).expect("tol"),
            d("1.0000000001")
        );
        assert!(share_tolerance(i64::MAX, 2).is_err());
    }

    /// The manual devnet run reported 27,689.39 and 6,922.35, rounded to
    /// cents. The exact 4:1 operator total is 6,922.3475.
    #[test]
    fn accepts_the_manual_devnet_totals() {
        let assigned = [
            row("benef::1220", 40, "27689.39"),
            row("operator::1220", 40, "6922.3475"),
        ];
        let seen = check_split(&eighty_twenty(), &assigned).expect("split holds");
        assert_eq!(seen.coupons_per_beneficiary, 40);
        assert_eq!(seen.total, d("34611.7375"));
    }

    /// Amounts whose shares round on every coupon still pass: the tolerance
    /// covers Daml's per-coupon rounding.
    #[test]
    fn accepts_rounding_from_every_coupon() {
        let split = vec![
            share("a::1220", "0.3333333333"),
            share("b::1220", "0.3333333333"),
            share("c::1220", "0.3333333334"),
        ];
        let originals = [
            "0.0000000007",
            "1.2345678901",
            "99.9999999999",
            "0.0000000013",
        ];
        let mut sums = [DamlDecimal::ZERO; 3];
        for amount in originals {
            for (sum, part) in sums.iter_mut().zip(assign(amount, &split)) {
                *sum += part;
            }
        }
        let assigned: Vec<AssignedTotal> = split
            .iter()
            .zip(sums)
            .map(|(s, amount)| AssignedTotal {
                beneficiary: s.beneficiary.clone(),
                coupons: originals.len() as i64,
                amount,
            })
            .collect();
        check_split(&split, &assigned).expect("rounding stays inside the tolerance");
    }

    #[test]
    fn rejects_a_split_off_by_more_than_rounding() {
        // 0.79 / 0.21 of the total, where the delegation says 0.8 / 0.2.
        let assigned = [
            row("benef::1220", 2, "79.0"),
            row("operator::1220", 2, "21.0"),
        ];
        let err = check_split(&eighty_twenty(), &assigned).unwrap_err();
        assert!(format!("{err:#}").contains("benef::1220"), "got: {err:#}");
    }

    #[test]
    fn rejects_one_unit_past_the_tolerance() {
        // One coupon, two beneficiaries: the tolerance is 4 units. Adding 20
        // units to the 0.8 share leaves each total 4 units off its share.
        let assigned = [
            row("benef::1220", 1, "80.0000000020"),
            row("operator::1220", 1, "20.0"),
        ];
        check_split(&eighty_twenty(), &assigned).expect("4 units is inside");
        // Adding 25 units leaves each total 5 units off.
        let assigned = [
            row("benef::1220", 1, "80.0000000025"),
            row("operator::1220", 1, "20.0"),
        ];
        assert!(check_split(&eighty_twenty(), &assigned).is_err());
    }

    #[test]
    fn rejects_a_beneficiary_the_split_does_not_name() {
        let assigned = [
            row("benef::1220", 1, "80.0"),
            row("operator::1220", 1, "20.0"),
            row("stranger::1220", 1, "5.0"),
        ];
        let err = check_split(&eighty_twenty(), &assigned).unwrap_err();
        assert!(
            format!("{err:#}").contains("stranger::1220"),
            "got: {err:#}"
        );
    }

    #[test]
    fn rejects_a_missing_beneficiary() {
        let assigned = [row("benef::1220", 1, "100.0")];
        let err = check_split(&eighty_twenty(), &assigned).unwrap_err();
        assert!(
            format!("{err:#}").contains("operator::1220"),
            "got: {err:#}"
        );
    }

    #[test]
    fn rejects_unequal_coupon_counts() {
        // The shares match by value, but one beneficiary got an extra coupon.
        let assigned = [
            row("benef::1220", 3, "80.0"),
            row("operator::1220", 2, "20.0"),
        ];
        let err = check_split(&eighty_twenty(), &assigned).unwrap_err();
        assert!(
            format!("{err:#}").contains("one coupon per"),
            "got: {err:#}"
        );
    }
}
