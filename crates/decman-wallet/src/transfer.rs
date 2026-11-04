//! Token Standard transfers: the arguments, and the choice arguments built
//! from them.
//!
//! Canton Coin and utility instruments such as CBTC use the same interfaces
//! and the same choice arguments. Only the registry root differs.

use std::time::Duration;

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde_json::{Value, json};

use crate::ledger::Holding;

/// The `Holding` interface, by package name.
pub const HOLDING_INTERFACE_ID: &str =
    "#splice-api-token-holding-v1:Splice.Api.Token.HoldingV1:Holding";
/// The `TransferFactory` interface, by package name.
pub const TRANSFER_FACTORY_INTERFACE_ID: &str = "#splice-api-token-transfer-instruction-v1:Splice.Api.Token.TransferInstructionV1:TransferFactory";
/// The `TransferInstruction` interface, by package name.
pub const TRANSFER_INSTRUCTION_INTERFACE_ID: &str = "#splice-api-token-transfer-instruction-v1:Splice.Api.Token.TransferInstructionV1:TransferInstruction";
/// The choice that sends.
pub const TRANSFER_CHOICE: &str = "TransferFactory_Transfer";
/// The choice that accepts a transfer offered to the party.
pub const ACCEPT_CHOICE: &str = "TransferInstruction_Accept";
/// The most holdings one transfer takes as inputs.
pub const MAX_INPUT_HOLDINGS: usize = 100;

/// How far `requested_at` is set into the past. Canton Coin's factory
/// refuses a request time that is not in the past, and this absorbs clock
/// drift. The splice-wallet-kernel SDK uses the same minute.
const REQUESTED_AT_SKEW: Duration = Duration::from_secs(60);

/// What to send, to whom, and when the offer expires.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferArgs {
    pub sender: String,
    pub receiver: String,
    /// A decimal string, for example `1.25`. It is never parsed as a number.
    pub amount: String,
    pub instrument_admin: String,
    pub instrument_id: String,
    /// The holdings to spend. When empty, the send uses every unlocked
    /// holding of the instrument the sender owns, up to
    /// [`MAX_INPUT_HOLDINGS`].
    pub input_holding_cids: Vec<String>,
    /// RFC 3339.
    pub requested_at: String,
    /// RFC 3339.
    pub execute_before: String,
}

impl TransferArgs {
    /// Arguments for a transfer requested now that expires after `validity`.
    ///
    /// The two times are fixed here, once. The registry resolves the context
    /// for these exact values, and the ledger command must carry the same
    /// ones.
    pub fn new(
        sender: impl Into<String>,
        receiver: impl Into<String>,
        amount: impl Into<String>,
        instrument_admin: impl Into<String>,
        instrument_id: impl Into<String>,
        validity: Duration,
    ) -> Self {
        let now = Utc::now();
        let skew = TimeDelta::from_std(REQUESTED_AT_SKEW).unwrap_or_default();
        let validity = TimeDelta::from_std(validity).unwrap_or(TimeDelta::MAX);
        let requested_at = now.checked_sub_signed(skew).unwrap_or(now);
        let execute_before = now
            .checked_add_signed(validity)
            .unwrap_or(DateTime::<Utc>::MAX_UTC);
        Self {
            sender: sender.into(),
            receiver: receiver.into(),
            amount: amount.into(),
            instrument_admin: instrument_admin.into(),
            instrument_id: instrument_id.into(),
            input_holding_cids: Vec::new(),
            requested_at: requested_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            execute_before: execute_before.to_rfc3339_opts(SecondsFormat::Millis, true),
        }
    }
}

/// The `TransferFactory_Transfer` choice argument, in the JSON Ledger API's
/// encoding.
///
/// `context` is the registry's `choiceContextData`. The registry request
/// itself passes `Value::Null`, which becomes an empty context.
pub fn transfer_choice_argument(args: &TransferArgs, context: &Value) -> Value {
    json!({
        "expectedAdmin": args.instrument_admin,
        "transfer": {
            "sender": args.sender,
            "receiver": args.receiver,
            "amount": args.amount,
            "instrumentId": {
                "admin": args.instrument_admin,
                "id": args.instrument_id,
            },
            "requestedAt": args.requested_at,
            "executeBefore": args.execute_before,
            "inputHoldingCids": args.input_holding_cids,
            "meta": { "values": {} },
        },
        "extraArgs": extra_args(context),
    })
}

/// The `TransferInstruction_Accept` choice argument. The choice takes only
/// `extraArgs`.
pub fn accept_choice_argument(context: &Value) -> Value {
    json!({ "extraArgs": extra_args(context) })
}

/// `ExtraArgs` with the registry's context. Both `values` maps are Daml
/// `TextMap`s, so an absent map is sent as `{}`, never as `null`.
fn extra_args(context: &Value) -> Value {
    let values = context
        .get("values")
        .filter(|values| !values.is_null())
        .cloned()
        .unwrap_or_else(|| json!({}));
    json!({
        "context": { "values": values },
        "meta": { "values": {} },
    })
}

/// The holdings a send may spend: owned by `owner`, of the instrument
/// `(admin, id)`, and not locked. At most [`MAX_INPUT_HOLDINGS`] of them.
pub fn unlocked_holdings<'a>(
    holdings: &'a [Holding],
    owner: &str,
    admin: &str,
    id: &str,
) -> Vec<&'a Holding> {
    holdings
        .iter()
        .filter(|h| {
            h.owner == owner && h.instrument_admin == admin && h.instrument_id == id && !h.locked
        })
        .take(MAX_INPUT_HOLDINGS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> TransferArgs {
        TransferArgs {
            sender: "alice::1220aa".to_string(),
            receiver: "bob::1220bb".to_string(),
            amount: "1.25".to_string(),
            instrument_admin: "DSO::1220dd".to_string(),
            instrument_id: "Amulet".to_string(),
            input_holding_cids: vec!["00aa".to_string()],
            requested_at: "2026-11-04T10:00:00.000Z".to_string(),
            execute_before: "2026-11-05T10:00:00.000Z".to_string(),
        }
    }

    fn holding(contract_id: &str, owner: &str, id: &str, locked: bool) -> Holding {
        Holding {
            contract_id: contract_id.to_string(),
            owner: owner.to_string(),
            instrument_admin: "DSO::1220dd".to_string(),
            instrument_id: id.to_string(),
            amount: "10.0000000000".to_string(),
            locked,
        }
    }

    #[test]
    fn transfer_choice_argument_matches_the_daml_record() {
        let context =
            json!({ "values": { "amulet-rules": { "tag": "AV_ContractId", "value": "00cc" } } });

        let argument = transfer_choice_argument(&args(), &context);

        assert_eq!(
            argument,
            json!({
                "expectedAdmin": "DSO::1220dd",
                "transfer": {
                    "sender": "alice::1220aa",
                    "receiver": "bob::1220bb",
                    "amount": "1.25",
                    "instrumentId": { "admin": "DSO::1220dd", "id": "Amulet" },
                    "requestedAt": "2026-11-04T10:00:00.000Z",
                    "executeBefore": "2026-11-05T10:00:00.000Z",
                    "inputHoldingCids": ["00aa"],
                    "meta": { "values": {} },
                },
                "extraArgs": {
                    "context": context,
                    "meta": { "values": {} },
                },
            })
        );
    }

    /// An empty `values` sent as `null` fails in the registry with
    /// "Expected { but was null".
    #[test]
    fn a_missing_context_becomes_an_empty_text_map() {
        for context in [Value::Null, json!({}), json!({ "values": null })] {
            assert_eq!(
                accept_choice_argument(&context),
                json!({ "extraArgs": { "context": { "values": {} }, "meta": { "values": {} } } })
            );
        }
    }

    #[test]
    fn new_fixes_both_times_once_with_the_request_in_the_past() {
        let before = Utc::now();
        let args = TransferArgs::new(
            "alice",
            "bob",
            "1.25",
            "DSO",
            "Amulet",
            Duration::from_secs(3600),
        );

        let parse = |value: &str| match DateTime::parse_from_rfc3339(value) {
            Ok(time) => time.with_timezone(&Utc),
            Err(e) => panic!("{value} must be RFC 3339: {e}"),
        };
        let requested_at = parse(&args.requested_at);
        let execute_before = parse(&args.execute_before);
        assert!(
            requested_at < before,
            "the request time must lie in the past"
        );
        assert!(execute_before > before + TimeDelta::minutes(59));
        assert!(args.input_holding_cids.is_empty());
        assert_eq!(args.amount, "1.25");
    }

    #[test]
    fn unlocked_holdings_keeps_the_owners_unlocked_holdings_of_one_instrument_up_to_the_cap() {
        let mut holdings = vec![
            holding("00a1", "alice", "Amulet", false),
            holding("00a2", "alice", "Amulet", true),
            holding("00a3", "bob", "Amulet", false),
            holding("00a4", "alice", "CBTC", false),
        ];

        let picked = unlocked_holdings(&holdings, "alice", "DSO::1220dd", "Amulet");
        let picked: Vec<&str> = picked.iter().map(|h| h.contract_id.as_str()).collect();
        assert_eq!(picked, vec!["00a1"]);

        holdings.extend(
            (0..MAX_INPUT_HOLDINGS + 5)
                .map(|i| holding(&format!("{i:04x}"), "alice", "Amulet", false)),
        );
        assert_eq!(
            unlocked_holdings(&holdings, "alice", "DSO::1220dd", "Amulet").len(),
            MAX_INPUT_HOLDINGS
        );
    }
}
