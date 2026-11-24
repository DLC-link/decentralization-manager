// Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Ledger values, parties and coupons shared by the reward automation tests.

use canton_proto_rs::com::daml::ledger::api::v2::{Record, RecordField, Value, value};
use chrono::{DateTime, Utc};

use super::coupons::CouponInfo;
use crate::canton_id::CantonId;

pub fn value(sum: value::Sum) -> Value {
    Value { sum: Some(sum) }
}

pub fn field(label: &str, sum: value::Sum) -> RecordField {
    RecordField {
        label: label.to_string(),
        value: Some(value(sum)),
    }
}

pub fn record(fields: Vec<RecordField>) -> Record {
    Record {
        record_id: None,
        fields,
    }
}

pub fn party(p: &str) -> value::Sum {
    value::Sum::Party(p.to_string())
}

pub fn numeric(n: &str) -> value::Sum {
    value::Sum::Numeric(n.to_string())
}

pub fn timestamp(micros: i64) -> value::Sum {
    value::Sum::Timestamp(micros)
}

// Valid Canton party ids: `prefix::<multihash>` where the namespace is a
// 34-byte (68-hex-char) SHA-256 multihash (`1220` + 64 hex chars).
pub const GOV: &str = "gov::12200000000000000000000000000000000000000000000000000000000000000000";
pub const ALICE: &str =
    "alice::1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const BOB: &str = "bob::1220bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const CAROL: &str =
    "carol::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
pub const DAVE: &str = "dave::1220dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
pub const EVE: &str = "eve::1220eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

pub fn dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .expect("valid rfc3339")
        .with_timezone(&Utc)
}

pub fn coupon(id: &str, expires: &str) -> CouponInfo {
    CouponInfo {
        cid: id.to_string(),
        expires_at: dt(expires),
    }
}

pub fn cids(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("c{i}")).collect()
}

pub fn alice() -> CantonId {
    CantonId::parse(ALICE).expect("valid canton id")
}

/// A ledger rejection carrying a real Canton error id, shaped exactly as
/// the devnet logs show it: `ID(category,hash): message`.
pub fn canton_err(id: &str) -> anyhow::Error {
    anyhow::Error::new(tonic::Status::aborted(format!(
        "{id}(2,60893414): Rejected transaction"
    )))
}
