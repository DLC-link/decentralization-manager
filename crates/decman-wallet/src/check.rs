//! The check a prepared transaction must pass before the wallet signs it.
//!
//! A hash check alone does not protect the wallet. A participant can return a
//! different transaction together with that transaction's own correct hash.
//! So the wallet recomputes the hash from the transaction, and then reads the
//! transaction's root to see that it does what the wallet asked for.
//!
//! The check reads only the root exercise. It does not judge the
//! sub-transaction the factory or the instruction produces.

use base64::{Engine, engine::general_purpose::STANDARD};
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

use crate::{
    error::{Error, Refusal, Result},
    ledger::PreparedSubmission,
    transfer::TransferArgs,
};

/// What the wallet asked the participant to prepare.
pub(crate) struct Expected<'a> {
    /// The interface the root exercises, as `#package-name:Module:Entity`.
    pub(crate) interface_id: &'a str,
    pub(crate) choice: &'a str,
    pub(crate) contract_id: &'a str,
    /// The one party the exercise may act as.
    pub(crate) party: &'a str,
    /// For a send, the transfer the choice argument must describe.
    pub(crate) transfer: Option<&'a TransferArgs>,
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
) -> Result<Vec<u8>> {
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

fn decode_b64(host: &str, field: &'static str, value: &str) -> Result<Vec<u8>> {
    STANDARD.decode(value).map_err(|source| Error::Base64 {
        host: host.to_string(),
        field,
        source,
    })
}

/// The scheme the JSON Ledger API names, for example
/// `HASHING_SCHEME_VERSION_V2`. Only the schemes `canton_hash` reproduces
/// pass.
fn hashing_scheme(name: &str) -> std::result::Result<HashingScheme, Refusal> {
    let version = HashingSchemeVersion::from_str_name(name)
        .ok_or_else(|| Refusal::Unverifiable(format!("unknown hashing scheme version {name}")))?;
    HashingScheme::from_proto(i32::from(version)).map_err(|e| Refusal::Unverifiable(e.to_string()))
}

fn check_root(
    transaction: &PreparedTransaction,
    expected: &Expected<'_>,
) -> std::result::Result<(), Refusal> {
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

    if let Some(args) = expected.transfer {
        check_transfer(exercise, args)?;
    }
    Ok(())
}

/// The one exercise at the root of the transaction.
fn root_exercise(transaction: &PreparedTransaction) -> std::result::Result<&Exercise, Refusal> {
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
        .ok_or_else(|| Refusal::Shape(format!("its root node {root} is missing")))?;
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

fn check_transfer(exercise: &Exercise, args: &TransferArgs) -> std::result::Result<(), Refusal> {
    let argument = exercise
        .chosen_value
        .as_ref()
        .ok_or_else(|| Refusal::Shape("its exercise has no choice argument".to_string()))?;
    for (path, wanted) in [
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
    Ok(())
}

fn same(field: &'static str, expected: &str, found: &str) -> std::result::Result<(), Refusal> {
    if expected == found {
        return Ok(());
    }
    Err(Refusal::Mismatch {
        field,
        expected: expected.to_string(),
        found: found.to_string(),
    })
}

/// The party, text or numeric found by following the record labels in
/// `path`, for example `transfer.sender`.
fn text_at<'v>(value: &'v Value, path: &'static str) -> std::result::Result<&'v str, Refusal> {
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
    match current.sum.as_ref() {
        Some(Sum::Party(text) | Sum::Text(text) | Sum::Numeric(text)) => Ok(text),
        _ => Err(Refusal::Shape(format!(
            "its choice argument's {path} is not text"
        ))),
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

#[cfg(test)]
mod tests {
    use super::*;

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
