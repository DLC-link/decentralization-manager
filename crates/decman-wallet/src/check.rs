//! The check a prepared transaction must pass before the wallet signs it.
//!
//! A hash check alone does not protect the wallet. A participant can return a
//! different transaction together with that transaction's own correct hash.
//! So the wallet recomputes the hash from the transaction, and then reads the
//! transaction's root to see that it does what the wallet asked for.
//!
//! The check reads the root exercise and the metadata fields the wallet
//! asked for. It does not judge the sub-transaction the factory or the
//! instruction produces.

use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use decman_lib::{
    canton_hash::{HashingScheme, compute_prepared_transaction_hash},
    canton_proto_rs::com::daml::ledger::api::v2::{
        Value,
        interactive::{
            HashingSchemeVersion, PreparedTransaction,
            daml_transaction::node::VersionedNode,
            transaction::v1::{Exercise, node::NodeType},
        },
        value::Sum,
    },
};
use prost::Message;
use serde_json::{Map, Value as Json, json};

use crate::{
    error::{Error, Refusal},
    ledger::PreparedSubmission,
    text,
    transfer::TransferArgs,
};

/// What the wallet asked the participant to prepare.
pub(crate) struct Expected<'a> {
    /// The interface the root exercises, as `#package-name:Module:Entity`.
    pub(crate) interface_id: &'a str,
    /// The template the root must act on, as `#package-name:Module:Entity`,
    /// or as `package-id:Module:Entity` to pin one package. `None` leaves
    /// the template unchecked.
    pub(crate) template_id: Option<&'a str>,
    pub(crate) choice: &'a str,
    pub(crate) contract_id: &'a str,
    /// The one party the exercise may act as.
    pub(crate) party: &'a str,
    /// The command id the wallet sent to the prepare. The hash covers it, and
    /// the completion lookup after an unknown outcome needs it.
    pub(crate) command_id: &'a str,
    /// The max record time the wallet sent to the prepare, RFC 3339.
    pub(crate) max_record_time: &'a str,
    /// The `extraArgs` the wallet sent, in the JSON Ledger API's encoding.
    pub(crate) extra_args: &'a Json,
    /// For a send, the transfer the choice argument must describe.
    pub(crate) transfer: Option<SentTransfer<'a>>,
}

/// The transfer a send asked for.
pub(crate) struct SentTransfer<'a> {
    pub(crate) args: &'a TransferArgs,
    /// The `transfer.meta` the wallet sent, in the JSON Ledger API's
    /// encoding.
    pub(crate) meta: &'a Json,
}

/// Recompute the hash of `prepared`, check its root exercise against
/// `expected`, and return the hash to sign.
///
/// # Errors
/// [`Error::TransactionRefused`] for a transaction the wallet must not sign,
/// and [`Error::Base64`] for a field that is not base64.
pub(crate) fn check_prepared(
    host: &str,
    prepared: &PreparedSubmission,
    expected: &Expected<'_>,
) -> Result<Vec<u8>, Error> {
    let refused = |reason| Error::TransactionRefused {
        host: host.to_string(),
        reason,
    };
    let bytes = decode_b64(host, "preparedTransaction", &prepared.prepared_transaction)?;
    let supplied = decode_b64(
        host,
        "preparedTransactionHash",
        &prepared.prepared_transaction_hash,
    )?;

    let transaction = PreparedTransaction::decode(bytes.as_slice()).map_err(|e| {
        refused(Refusal::Unverifiable(format!(
            "it is not a prepared transaction: {e}"
        )))
    })?;
    let scheme = hashing_scheme(&prepared.hashing_scheme_version).map_err(refused)?;
    let recomputed = compute_prepared_transaction_hash(scheme, &transaction)
        .map_err(|e| refused(Refusal::Unverifiable(e.to_string())))?;
    if recomputed != supplied {
        return Err(refused(Refusal::HashMismatch {
            supplied: prepared.prepared_transaction_hash.clone(),
            recomputed: STANDARD.encode(&recomputed),
        }));
    }

    check_root(&transaction, expected).map_err(refused)?;
    Ok(recomputed)
}

fn decode_b64(host: &str, field: &'static str, value: &str) -> Result<Vec<u8>, Error> {
    STANDARD.decode(value).map_err(|source| Error::Base64 {
        host: host.to_string(),
        field,
        source,
    })
}

/// The scheme the JSON Ledger API names, for example
/// `HASHING_SCHEME_VERSION_V2`. Only the schemes `canton_hash` reproduces
/// pass.
fn hashing_scheme(name: &str) -> Result<HashingScheme, Refusal> {
    let version = HashingSchemeVersion::from_str_name(name)
        .ok_or_else(|| Refusal::Unverifiable(format!("unknown hashing scheme version {name}")))?;
    HashingScheme::from_proto(i32::from(version)).map_err(|e| Refusal::Unverifiable(e.to_string()))
}

fn check_root(transaction: &PreparedTransaction, expected: &Expected<'_>) -> Result<(), Refusal> {
    let exercise = root_exercise(transaction)?;

    let interface = exercise
        .interface_id
        .as_ref()
        .map(|id| {
            format!(
                "{module}:{entity}",
                module = id.module_name,
                entity = id.entity_name
            )
        })
        .unwrap_or_default();
    // The transaction names the package by id, the wallet by name, so only
    // the module and entity are compared.
    let expected_interface = expected
        .interface_id
        .split_once(':')
        .map_or(expected.interface_id, |(_, qualified)| qualified);
    same("interface", expected_interface, &interface)?;
    same("choice", expected.choice, &exercise.choice_id)?;
    same("contract id", expected.contract_id, &exercise.contract_id)?;
    same(
        "acting parties",
        expected.party,
        &exercise.acting_parties.join(", "),
    )?;
    let submitter = transaction
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.submitter_info.as_ref());
    same(
        "command id",
        expected.command_id,
        submitter.map_or("", |submitter| submitter.command_id.as_str()),
    )?;
    // Canton authorizes the transaction with the authority of the metadata's
    // submitters, and the hash covers them. They must be the party alone.
    same(
        "acting parties in the metadata",
        expected.party,
        &submitter.map_or_else(String::new, |submitter| submitter.act_as.join(", ")),
    )?;
    same_max_record_time(
        expected.max_record_time,
        transaction
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.max_record_time),
    )?;
    if let Some(template) = expected.template_id {
        same("template", template, &template_of(exercise, template)?)?;
    }

    let argument = exercise
        .chosen_value
        .as_ref()
        .ok_or_else(|| Refusal::Shape("its exercise has no choice argument".to_string()))?;
    if let Some(sent) = &expected.transfer {
        check_transfer(argument, sent.args)?;
        same_value(
            "transfer.meta",
            sent.meta,
            field_at(argument, "transfer.meta")?,
        )?;
    }
    same_value(
        "extraArgs",
        expected.extra_args,
        field_at(argument, "extraArgs")?,
    )
}

/// Compare the max record time in the transaction's metadata, in
/// microseconds, with the RFC 3339 time the wallet asked for.
///
/// Without a max record time, Canton may record the signed transaction long
/// after the wallet stopped waiting for it. So the check refuses a missing
/// one too.
fn same_max_record_time(asked: &str, found: Option<u64>) -> Result<(), Refusal> {
    let asked_micros = DateTime::parse_from_rfc3339(asked)
        .ok()
        .map(|time| time.timestamp_micros());
    let found_micros = found.and_then(|micros| i64::try_from(micros).ok());
    if asked_micros.is_some() && asked_micros == found_micros {
        return Ok(());
    }
    Err(Refusal::Mismatch {
        field: "max record time",
        expected: asked.to_string(),
        found: match found {
            None => "none".to_string(),
            Some(micros) => found_micros
                .and_then(canonical_time)
                .unwrap_or_else(|| micros.to_string()),
        },
    })
}

/// The template the exercise acts on, in the form of `like`.
///
/// A `like` that starts with `#` names the package by name, so the result
/// is `#package-name:Module:Entity`, and any package of that name passes.
/// Any other `like` names the package by id, so the result is
/// `package-id:Module:Entity`, and only that one package passes.
fn template_of(exercise: &Exercise, like: &str) -> Result<String, Refusal> {
    let id = exercise
        .template_id
        .as_ref()
        .ok_or_else(|| Refusal::Shape("its root exercise names no template".to_string()))?;
    let package = if like.starts_with('#') {
        format!("#{name}", name = exercise.package_name)
    } else {
        id.package_id.clone()
    };
    Ok(format!(
        "{package}:{module}:{entity}",
        module = id.module_name,
        entity = id.entity_name
    ))
}

/// The one exercise at the root of the transaction.
fn root_exercise(transaction: &PreparedTransaction) -> Result<&Exercise, Refusal> {
    let tree = transaction
        .transaction
        .as_ref()
        .ok_or_else(|| Refusal::Shape("it has no transaction".to_string()))?;
    let [root] = tree.roots.as_slice() else {
        return Err(Refusal::Shape(format!(
            "it has {count} root nodes, not one",
            count = tree.roots.len()
        )));
    };
    let node = tree
        .nodes
        .iter()
        .find(|node| &node.node_id == root)
        .ok_or_else(|| {
            Refusal::Shape(format!(
                "its root node {root} is missing",
                root = text::bounded(root)
            ))
        })?;
    let Some(VersionedNode::V1(node)) = node.versioned_node.as_ref() else {
        return Err(Refusal::Shape("its root node has no content".to_string()));
    };
    let Some(NodeType::Exercise(exercise)) = node.node_type.as_ref() else {
        return Err(Refusal::Shape(
            "its root node is not an exercise".to_string(),
        ));
    };
    Ok(exercise)
}

fn check_transfer(argument: &Value, args: &TransferArgs) -> Result<(), Refusal> {
    for (path, wanted) in [
        ("expectedAdmin", &args.instrument_admin),
        ("transfer.sender", &args.sender),
        ("transfer.receiver", &args.receiver),
        ("transfer.instrumentId.admin", &args.instrument_admin),
        ("transfer.instrumentId.id", &args.instrument_id),
    ] {
        same(path, wanted, text_at(argument, path)?)?;
    }

    let amount = text_at(argument, "transfer.amount")?;
    if canonical_decimal(amount) != canonical_decimal(&args.amount) {
        return Err(Refusal::Mismatch {
            field: "transfer.amount",
            expected: args.amount.clone(),
            found: amount.to_string(),
        });
    }

    // A later `executeBefore` would keep the offer open longer than asked.
    for (path, wanted) in [
        ("transfer.requestedAt", &args.requested_at),
        ("transfer.executeBefore", &args.execute_before),
    ] {
        let found = timestamp_at(argument, path)?;
        let requested = DateTime::parse_from_rfc3339(wanted)
            .ok()
            .map(|time| time.timestamp_micros());
        if requested != Some(found) {
            return Err(Refusal::Mismatch {
                field: path,
                expected: wanted.clone(),
                found: canonical_time(found).unwrap_or_else(|| found.to_string()),
            });
        }
    }

    let inputs = contract_ids_at(argument, "transfer.inputHoldingCids")?;
    if !inputs
        .iter()
        .copied()
        .eq(args.input_holding_cids.iter().map(String::as_str))
    {
        return Err(Refusal::Mismatch {
            field: "transfer.inputHoldingCids",
            expected: args.input_holding_cids.join(", "),
            found: inputs.join(", "),
        });
    }
    Ok(())
}

fn same(field: &'static str, expected: &str, found: &str) -> Result<(), Refusal> {
    if expected == found {
        return Ok(());
    }
    Err(Refusal::Mismatch {
        field,
        expected: expected.to_string(),
        found: found.to_string(),
    })
}

/// The value found by following the record labels in `path`, for example
/// `transfer.sender`.
fn field_at<'v>(value: &'v Value, path: &'static str) -> Result<&'v Value, Refusal> {
    let mut current = value;
    for label in path.split('.') {
        current = match current.sum.as_ref() {
            Some(Sum::Record(record)) => record
                .fields
                .iter()
                .find(|field| field.label == label)
                .and_then(|field| field.value.as_ref()),
            _ => None,
        }
        .ok_or_else(|| Refusal::Shape(format!("its choice argument has no {path}")))?;
    }
    Ok(current)
}

/// The content of the value at `path`.
fn value_at<'v>(value: &'v Value, path: &'static str) -> Result<&'v Sum, Refusal> {
    field_at(value, path)?
        .sum
        .as_ref()
        .ok_or_else(|| Refusal::Shape(format!("its choice argument's {path} is empty")))
}

/// The party, text or numeric at `path`.
fn text_at<'v>(value: &'v Value, path: &'static str) -> Result<&'v str, Refusal> {
    match value_at(value, path)? {
        Sum::Party(text) | Sum::Text(text) | Sum::Numeric(text) => Ok(text),
        _ => Err(Refusal::Shape(format!(
            "its choice argument's {path} is not text"
        ))),
    }
}

/// The time at `path`, in microseconds since the epoch.
fn timestamp_at(value: &Value, path: &'static str) -> Result<i64, Refusal> {
    match value_at(value, path)? {
        Sum::Timestamp(micros) => Ok(*micros),
        _ => Err(Refusal::Shape(format!(
            "its choice argument's {path} is not a time"
        ))),
    }
}

/// The list of contract ids at `path`, in order.
fn contract_ids_at<'v>(value: &'v Value, path: &'static str) -> Result<Vec<&'v str>, Refusal> {
    let not_ids = || Refusal::Shape(format!("its choice argument's {path} is not contract ids"));
    let Sum::List(list) = value_at(value, path)? else {
        return Err(not_ids());
    };
    list.elements
        .iter()
        .map(|element| match element.sum.as_ref() {
            Some(Sum::ContractId(id)) => Ok(id.as_str()),
            _ => Err(not_ids()),
        })
        .collect()
}

/// Compare a value the wallet sent, in the JSON Ledger API's encoding, with
/// the value at the same place in the transaction. Both sides are brought
/// to one canonical JSON form first, so they differ only when a value does.
fn same_value(field: &'static str, sent: &Json, found: &Value) -> Result<(), Refusal> {
    let found_json = canonical_json(found).map_err(|part| {
        Refusal::Shape(format!(
            "the wallet cannot read {part}, so it cannot compare its {field}"
        ))
    })?;
    let sent_json = canonical_sent(sent, found);
    if sent_json == found_json {
        return Ok(());
    }
    Err(Refusal::Mismatch {
        field,
        expected: sent_json.to_string(),
        found: found_json.to_string(),
    })
}

/// `value` in the JSON Ledger API's encoding, with one spelling for each
/// value. An Int64 is a string, a Numeric has no trailing zeros, and a time
/// is in UTC with six fractional digits.
///
/// The error names a part the wallet cannot read. A part it cannot read is
/// a part it cannot compare, so the caller refuses the transaction.
fn canonical_json(value: &Value) -> Result<Json, &'static str> {
    let sum = value.sum.as_ref().ok_or("an empty value")?;
    Ok(match sum {
        Sum::Unit(()) => Json::Object(Map::new()),
        Sum::Bool(flag) => Json::Bool(*flag),
        Sum::Int64(number) => Json::String(number.to_string()),
        Sum::Numeric(decimal) => Json::String(canonical_decimal(decimal).to_string()),
        Sum::Timestamp(micros) => {
            Json::String(canonical_time(*micros).ok_or("a time out of range")?)
        }
        Sum::Date(days) => Json::String(canonical_date(*days).ok_or("a date out of range")?),
        Sum::Party(text) | Sum::Text(text) | Sum::ContractId(text) => Json::String(text.clone()),
        Sum::Enum(constructor) => Json::String(constructor.constructor.clone()),
        Sum::List(list) => Json::Array(
            list.elements
                .iter()
                .map(canonical_json)
                .collect::<Result<_, _>>()?,
        ),
        Sum::TextMap(map) => {
            let mut object = Map::new();
            for entry in &map.entries {
                let value = entry
                    .value
                    .as_ref()
                    .ok_or("a text map entry without a value")?;
                if object
                    .insert(entry.key.clone(), canonical_json(value)?)
                    .is_some()
                {
                    return Err("a text map with a repeated key");
                }
            }
            Json::Object(object)
        }
        Sum::Record(record) => {
            let mut object = Map::new();
            for field in &record.fields {
                if field.label.is_empty() {
                    return Err("a record field without a label");
                }
                let value = field
                    .value
                    .as_ref()
                    .ok_or("a record field without a value")?;
                if object
                    .insert(field.label.clone(), canonical_json(value)?)
                    .is_some()
                {
                    return Err("a record with a repeated field");
                }
            }
            Json::Object(object)
        }
        Sum::Variant(variant) => {
            let value = variant
                .value
                .as_deref()
                .ok_or("a variant without a value")?;
            json!({ "tag": variant.constructor, "value": canonical_json(value)? })
        }
        Sum::Optional(_) => return Err("an optional value"),
        Sum::GenMap(_) => return Err("a generic map"),
    })
}

/// What the wallet sent, in the form [`canonical_json`] gives `like`.
///
/// The JSON alone cannot tell a Numeric or a time from a Text, so `like`
/// supplies the types. A Text is kept exactly as sent. A part that does not
/// fit `like` is kept too, and the comparison then fails.
fn canonical_sent(sent: &Json, like: &Value) -> Json {
    let Some(sum) = like.sum.as_ref() else {
        return sent.clone();
    };
    match (sum, sent) {
        (Sum::List(list), Json::Array(items)) => Json::Array(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| guided(item, list.elements.get(index)))
                .collect(),
        ),
        (Sum::TextMap(map), Json::Object(object)) => guided_object(object, |key| {
            map.entries
                .iter()
                .find(|entry| entry.key == key)
                .and_then(|entry| entry.value.as_ref())
        }),
        (Sum::Record(record), Json::Object(object)) => guided_object(object, |label| {
            record
                .fields
                .iter()
                .find(|field| field.label == label)
                .and_then(|field| field.value.as_ref())
        }),
        (Sum::Variant(variant), Json::Object(object)) => guided_object(object, |key| {
            if key == "value" {
                variant.value.as_deref()
            } else {
                None
            }
        }),
        _ => canonical_scalar(sum, sent).map_or_else(|| sent.clone(), Json::String),
    }
}

fn guided(sent: &Json, like: Option<&Value>) -> Json {
    like.map_or_else(|| sent.clone(), |like| canonical_sent(sent, like))
}

fn guided_object<'v>(object: &Map<String, Json>, like: impl Fn(&str) -> Option<&'v Value>) -> Json {
    Json::Object(
        object
            .iter()
            .map(|(key, value)| (key.clone(), guided(value, like(key))))
            .collect(),
    )
}

/// The canonical spelling of a scalar the wallet sent, or `None` when it
/// does not read as the type of `like`. The JSON Ledger API takes an Int64
/// as a number or a string, and a time with or without fractional seconds.
fn canonical_scalar(like: &Sum, sent: &Json) -> Option<String> {
    match (like, sent) {
        (Sum::Int64(_), Json::Number(number)) => number.as_i64().map(|n| n.to_string()),
        (Sum::Int64(_), Json::String(text)) => text.parse::<i64>().ok().map(|n| n.to_string()),
        (Sum::Numeric(_), Json::String(text)) => Some(canonical_decimal(text).to_string()),
        (Sum::Timestamp(_), Json::String(text)) => {
            let time = DateTime::parse_from_rfc3339(text).ok()?;
            // The ledger keeps microseconds. A finer time is not the same time.
            if time.timestamp_subsec_nanos() % 1_000 != 0 {
                return None;
            }
            canonical_time(time.timestamp_micros())
        }
        (Sum::Date(_), Json::String(text)) => NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .ok()
            .map(|date| date.format("%Y-%m-%d").to_string()),
        _ => None,
    }
}

/// A decimal without the trailing fractional zeros that do not change its
/// value. The ledger writes `1.25` as `1.2500000000`. Text only, so nothing
/// is rounded.
fn canonical_decimal(text: &str) -> &str {
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.')
    } else {
        text
    }
}

/// A time in microseconds since the epoch, as RFC 3339 in UTC with six
/// fractional digits.
fn canonical_time(micros: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp_micros(micros)
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Micros, true))
}

/// A date in days since the epoch, as `YYYY-MM-DD`.
fn canonical_date(days: i32) -> Option<String> {
    DateTime::<Utc>::from_timestamp(i64::from(days) * 86_400, 0)
        .map(|time| time.format("%Y-%m-%d").to_string())
}

#[cfg(test)]
mod tests {
    use decman_lib::canton_proto_rs::com::daml::ledger::api::v2::{
        Enum, GenMap, Identifier, List, Optional, Record, RecordField, TextMap, Variant,
        interactive::{
            DamlTransaction, Metadata, daml_transaction::Node, metadata::SubmitterInfo,
            transaction::v1,
        },
        text_map,
    };

    use super::*;

    fn daml(sum: Sum) -> Value {
        Value { sum: Some(sum) }
    }

    fn variant(tag: &str, value: Value) -> Value {
        daml(Sum::Variant(Box::new(Variant {
            variant_id: None,
            constructor: tag.to_string(),
            value: Some(Box::new(value)),
        })))
    }

    fn record(fields: Vec<(&str, Value)>) -> Value {
        daml(Sum::Record(Record {
            record_id: None,
            fields: fields
                .into_iter()
                .map(|(label, value)| RecordField {
                    label: label.to_string(),
                    value: Some(value),
                })
                .collect(),
        }))
    }

    fn text_map(entries: Vec<(&str, Value)>) -> Value {
        daml(Sum::TextMap(TextMap {
            entries: entries
                .into_iter()
                .map(|(key, value)| text_map::Entry {
                    key: key.to_string(),
                    value: Some(value),
                })
                .collect(),
        }))
    }

    /// 2026-11-04T10:00:00Z.
    const TEN_AM: i64 = 1_793_786_400_000_000;
    /// 2026-11-04.
    const NOV_4: i32 = 20_761;

    /// A context with one entry of every `AnyValue` constructor, as the
    /// ledger writes it.
    fn every_any_value() -> Value {
        text_map(vec![
            (
                "text",
                variant("AV_Text", daml(Sum::Text("1.50".to_string()))),
            ),
            ("int", variant("AV_Int", daml(Sum::Int64(42)))),
            (
                "decimal",
                variant("AV_Decimal", daml(Sum::Numeric("1.2500000000".to_string()))),
            ),
            ("bool", variant("AV_Bool", daml(Sum::Bool(true)))),
            ("date", variant("AV_Date", daml(Sum::Date(NOV_4)))),
            ("time", variant("AV_Time", daml(Sum::Timestamp(TEN_AM)))),
            (
                "reltime",
                variant(
                    "AV_RelTime",
                    record(vec![("microseconds", daml(Sum::Int64(86_400_000_000)))]),
                ),
            ),
            (
                "party",
                variant("AV_Party", daml(Sum::Party("alice::1220aa".to_string()))),
            ),
            (
                "cid",
                variant("AV_ContractId", daml(Sum::ContractId("00c1".to_string()))),
            ),
            (
                "list",
                variant(
                    "AV_List",
                    daml(Sum::List(List {
                        elements: vec![
                            variant("AV_Int", daml(Sum::Int64(-7))),
                            variant("AV_Text", daml(Sum::Text("x".to_string()))),
                        ],
                    })),
                ),
            ),
            (
                "map",
                variant(
                    "AV_Map",
                    text_map(vec![("flag", variant("AV_Bool", daml(Sum::Bool(false))))]),
                ),
            ),
        ])
    }

    /// The same context as the registry sends it, with `int` and `time`
    /// spelled as given.
    fn sent_any_values(int: Json, time: &str) -> Json {
        json!({
            "text": { "tag": "AV_Text", "value": "1.50" },
            "int": { "tag": "AV_Int", "value": int },
            "decimal": { "tag": "AV_Decimal", "value": "1.25" },
            "bool": { "tag": "AV_Bool", "value": true },
            "date": { "tag": "AV_Date", "value": "2026-11-04" },
            "time": { "tag": "AV_Time", "value": time },
            "reltime": { "tag": "AV_RelTime", "value": { "microseconds": 86_400_000_000_i64 } },
            "party": { "tag": "AV_Party", "value": "alice::1220aa" },
            "cid": { "tag": "AV_ContractId", "value": "00c1" },
            "list": { "tag": "AV_List", "value": [
                { "tag": "AV_Int", "value": "-7" },
                { "tag": "AV_Text", "value": "x" },
            ] },
            "map": { "tag": "AV_Map", "value": { "flag": { "tag": "AV_Bool", "value": false } } },
        })
    }

    #[test]
    fn canonical_json_spells_every_any_value_the_way_the_json_ledger_api_does() {
        assert_eq!(
            canonical_json(&every_any_value()),
            Ok(json!({
                "text": { "tag": "AV_Text", "value": "1.50" },
                "int": { "tag": "AV_Int", "value": "42" },
                "decimal": { "tag": "AV_Decimal", "value": "1.25" },
                "bool": { "tag": "AV_Bool", "value": true },
                "date": { "tag": "AV_Date", "value": "2026-11-04" },
                "time": { "tag": "AV_Time", "value": "2026-11-04T10:00:00.000000Z" },
                "reltime": { "tag": "AV_RelTime", "value": { "microseconds": "86400000000" } },
                "party": { "tag": "AV_Party", "value": "alice::1220aa" },
                "cid": { "tag": "AV_ContractId", "value": "00c1" },
                "list": { "tag": "AV_List", "value": [
                    { "tag": "AV_Int", "value": "-7" },
                    { "tag": "AV_Text", "value": "x" },
                ] },
                "map": { "tag": "AV_Map", "value": { "flag": { "tag": "AV_Bool", "value": false } } },
            }))
        );
    }

    #[test]
    fn canonical_json_writes_unit_and_enum_as_the_json_ledger_api_does() {
        let enumeration = daml(Sum::Enum(Enum {
            enum_id: None,
            constructor: "Red".to_string(),
        }));
        assert_eq!(canonical_json(&daml(Sum::Unit(()))), Ok(json!({})));
        assert_eq!(canonical_json(&enumeration), Ok(json!("Red")));
    }

    /// The JSON Ledger API takes an Int64 as a number or a string, and a
    /// time with or without fractional seconds. Each spelling is the same
    /// value.
    #[test]
    fn same_value_accepts_every_spelling_of_an_int_and_a_time() {
        let ledger = every_any_value();
        for int in [json!(42), json!("42")] {
            for time in [
                "2026-11-04T10:00:00Z",
                "2026-11-04T10:00:00.000Z",
                "2026-11-04T10:00:00.000000Z",
                "2026-11-04T11:00:00+01:00",
            ] {
                let sent = sent_any_values(int.clone(), time);
                assert!(
                    same_value("extraArgs", &sent, &ledger).is_ok(),
                    "int {int} and time {time} must compare equal"
                );
            }
        }
    }

    #[test]
    fn same_value_refuses_a_changed_value_and_names_both_sides() {
        let ledger = every_any_value();
        let mut changes = Vec::new();
        for (key, value) in [
            // A Text is compared as written, never as a decimal.
            ("text", json!({ "tag": "AV_Text", "value": "1.5" })),
            ("int", json!({ "tag": "AV_Int", "value": 43 })),
            ("int", json!({ "tag": "AV_Int", "value": 42.5 })),
            ("decimal", json!({ "tag": "AV_Decimal", "value": "1.251" })),
            // A Decimal sent as a JSON number is never parsed as money.
            ("decimal", json!({ "tag": "AV_Decimal", "value": 1.25 })),
            (
                "time",
                json!({ "tag": "AV_Time", "value": "2026-11-04T10:00:00.000001Z" }),
            ),
            (
                "time",
                json!({ "tag": "AV_Time", "value": "2026-11-04T10:00:00.0000001Z" }),
            ),
            ("date", json!({ "tag": "AV_Date", "value": "2026-11-05" })),
            ("cid", json!({ "tag": "AV_Text", "value": "00c1" })),
            ("bool", json!({ "tag": "AV_Bool", "value": "true" })),
        ] {
            let mut sent = sent_any_values(json!(42), "2026-11-04T10:00:00Z");
            sent[key] = value;
            changes.push(sent);
        }
        let mut removed = sent_any_values(json!(42), "2026-11-04T10:00:00Z");
        if let Some(object) = removed.as_object_mut() {
            object.remove("cid");
        }
        changes.push(removed);
        let mut added = sent_any_values(json!(42), "2026-11-04T10:00:00Z");
        added["extra"] = json!({ "tag": "AV_Text", "value": "x" });
        changes.push(added);

        for sent in changes {
            match same_value("extraArgs", &sent, &ledger) {
                Err(Refusal::Mismatch {
                    field,
                    expected,
                    found,
                }) => {
                    assert_eq!(field, "extraArgs");
                    assert_eq!(
                        found,
                        canonical_json(&ledger).unwrap_or_default().to_string()
                    );
                    assert!(!expected.contains('\n'), "compact JSON: {expected}");
                }
                other => panic!("{sent} must be refused as a mismatch, got {other:?}"),
            }
        }
    }

    /// A value the wallet cannot read is a value it cannot compare, so the
    /// transaction is refused.
    #[test]
    fn a_value_the_converter_cannot_read_is_refused() {
        let unlabeled = daml(Sum::Record(Record {
            record_id: None,
            fields: vec![RecordField {
                label: String::new(),
                value: Some(daml(Sum::Bool(true))),
            }],
        }));
        let repeated = text_map(vec![
            ("k", daml(Sum::Text("a".to_string()))),
            ("k", daml(Sum::Text("b".to_string()))),
        ]);
        for value in [
            daml(Sum::Optional(Box::new(Optional { value: None }))),
            daml(Sum::GenMap(GenMap::default())),
            Value { sum: None },
            unlabeled,
            repeated,
            variant("AV_Time", daml(Sum::Timestamp(i64::MAX))),
        ] {
            match same_value("extraArgs", &json!({}), &value) {
                Err(Refusal::Shape(reason)) => {
                    assert!(reason.ends_with("cannot compare its extraArgs"), "{reason}")
                }
                other => panic!("{value:?} must be refused, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_max_record_time_must_be_the_one_the_wallet_asked_for() {
        let asked = "2026-11-04T10:00:00.000000Z";
        assert!(same_max_record_time(asked, Some(TEN_AM.unsigned_abs())).is_ok());
        assert!(
            same_max_record_time("2026-11-04T11:00:00+01:00", Some(TEN_AM.unsigned_abs())).is_ok(),
            "the same instant in another offset is the same time"
        );

        for (found, shown) in [
            (
                Some(TEN_AM.unsigned_abs() + 1),
                "2026-11-04T10:00:00.000001Z",
            ),
            (None, "none"),
            (Some(u64::MAX), "18446744073709551615"),
        ] {
            match same_max_record_time(asked, found) {
                Err(Refusal::Mismatch {
                    field,
                    expected,
                    found,
                }) => {
                    assert_eq!(field, "max record time");
                    assert_eq!(expected, asked);
                    assert_eq!(found, shown);
                }
                other => panic!("{shown} must be refused, got {other:?}"),
            }
        }
    }

    /// The host names the template, so a refusal must not print a long
    /// name whole, nor a newline in it.
    #[test]
    fn a_refusal_bounds_a_long_template_from_the_host() {
        let module = format!("Forged\n{}", "x".repeat(2993));
        assert_eq!(module.chars().count(), 3000);
        let exercise = Exercise {
            contract_id: "00c1".to_string(),
            package_name: "splice-amulet".to_string(),
            template_id: Some(Identifier {
                package_id: "package-id".to_string(),
                module_name: module,
                entity_name: "AmuletTransferInstruction".to_string(),
            }),
            acting_parties: vec!["alice::1220aa".to_string()],
            interface_id: Some(Identifier {
                package_id: "interface-package-id".to_string(),
                module_name: "Splice.Api.Token.TransferInstructionV1".to_string(),
                entity_name: "TransferInstruction".to_string(),
            }),
            choice_id: "TransferInstruction_Accept".to_string(),
            ..Exercise::default()
        };
        let transaction = PreparedTransaction {
            transaction: Some(DamlTransaction {
                roots: vec!["0".to_string()],
                nodes: vec![Node {
                    node_id: "0".to_string(),
                    versioned_node: Some(VersionedNode::V1(v1::Node {
                        node_type: Some(NodeType::Exercise(exercise)),
                    })),
                }],
                ..DamlTransaction::default()
            }),
            metadata: Some(Metadata {
                submitter_info: Some(SubmitterInfo {
                    act_as: vec!["alice::1220aa".to_string()],
                    command_id: "command-1".to_string(),
                }),
                max_record_time: Some(TEN_AM.unsigned_abs()),
                ..Metadata::default()
            }),
        };
        let expected = Expected {
            interface_id: "#splice-api-token-transfer-instruction-v1:Splice.Api.Token.TransferInstructionV1:TransferInstruction",
            template_id: Some(
                "#splice-amulet:Splice.AmuletTransferInstruction:AmuletTransferInstruction",
            ),
            choice: "TransferInstruction_Accept",
            contract_id: "00c1",
            party: "alice::1220aa",
            command_id: "command-1",
            max_record_time: "2026-11-04T10:00:00.000000Z",
            extra_args: &json!({}),
            transfer: None,
        };

        let refusal = match check_root(&transaction, &expected) {
            Err(refusal) => refusal,
            Ok(()) => panic!("the long template must be refused"),
        };

        assert!(
            matches!(
                refusal,
                Refusal::Mismatch {
                    field: "template",
                    ..
                }
            ),
            "{refusal:?}"
        );
        let message = refusal.to_string();
        assert!(message.contains(" ... ("), "{message}");
        assert!(message.chars().count() < 1300, "{message}");
        assert!(!message.contains('\n'), "{message}");
    }

    /// The host names the hashing scheme, so the refusal bounds that name.
    #[test]
    fn a_refusal_bounds_an_unknown_scheme_name_from_the_host() {
        let name = format!("V9\n{}", "x".repeat(2000));

        let message = match hashing_scheme(&name) {
            Err(refusal) => refusal.to_string(),
            Ok(_) => panic!("an unknown scheme must be refused"),
        };

        assert!(message.contains(" ... ("), "{message}");
        assert!(!message.contains('\n'), "{message}");
    }

    #[test]
    fn canonical_decimal_drops_only_zeros_that_do_not_change_the_value() {
        assert_eq!(canonical_decimal("1.2500000000"), "1.25");
        assert_eq!(canonical_decimal("1.25"), "1.25");
        assert_eq!(canonical_decimal("10.0000000000"), "10");
        assert_eq!(canonical_decimal("10"), "10");
        assert_eq!(canonical_decimal("100"), "100");
        assert_ne!(canonical_decimal("1.2500000001"), canonical_decimal("1.25"));
    }

    #[test]
    fn only_the_schemes_the_hash_code_reproduces_are_accepted() {
        assert!(matches!(
            hashing_scheme("HASHING_SCHEME_VERSION_V2"),
            Ok(HashingScheme::V2)
        ));
        assert!(matches!(
            hashing_scheme("HASHING_SCHEME_VERSION_V3"),
            Ok(HashingScheme::V3)
        ));
        for name in [
            "HASHING_SCHEME_VERSION_V4",
            "HASHING_SCHEME_VERSION_UNSPECIFIED",
            "HASHING_SCHEME_VERSION_V9",
        ] {
            assert!(
                matches!(hashing_scheme(name), Err(Refusal::Unverifiable(_))),
                "{name} must be refused"
            );
        }
    }
}
