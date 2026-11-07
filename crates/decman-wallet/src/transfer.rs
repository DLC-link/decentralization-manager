//! Token Standard transfers: send an asset, and accept one sent to the party.
//!
//! Both flows run the same steps as the splice-wallet-kernel SDK. The
//! registry supplies the choice context, the participant prepares the
//! transaction, the wallet checks it and signs its hash, and the participant
//! executes it. The key never leaves the [`Signer`].
//!
//! Canton Coin and utility instruments such as CBTC use the same interfaces
//! and the same choice arguments. Only the registry root differs.

use std::time::Duration;

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    check::{Expected, SentTransfer, check_prepared},
    error::{Error, Result},
    ledger::{DisclosedContract, ExecuteRequest, Executed, Holding, LedgerClient, PrepareRequest},
    registry::RegistryClient,
    signer::Signer,
};

/// The `Holding` interface, by package name.
pub const HOLDING_INTERFACE_ID: &str =
    "#splice-api-token-holding-v1:Splice.Api.Token.HoldingV1:Holding";
/// The `TransferFactory` interface, by package name.
pub const TRANSFER_FACTORY_INTERFACE_ID: &str = "#splice-api-token-transfer-instruction-v1:Splice.Api.Token.TransferInstructionV1:TransferFactory";
/// The `TransferInstruction` interface, by package name.
pub const TRANSFER_INSTRUCTION_INTERFACE_ID: &str = "#splice-api-token-transfer-instruction-v1:Splice.Api.Token.TransferInstructionV1:TransferInstruction";
/// Canton Coin's transfer instruction template, by package name.
pub const AMULET_TRANSFER_INSTRUCTION_TEMPLATE: &str =
    "#splice-amulet:Splice.AmuletTransferInstruction:AmuletTransferInstruction";
/// CBTC's transfer instruction template on DA's Utility registry, by package
/// name.
pub const CBTC_TRANSFER_INSTRUCTION_TEMPLATE: &str =
    "#utility-registry-app-v0:Utility.Registry.App.V0.Model.Transfer:TransferOffer";
/// The choice that sends.
pub const TRANSFER_CHOICE: &str = "TransferFactory_Transfer";
/// The choice that accepts a transfer offered to the party.
pub const ACCEPT_CHOICE: &str = "TransferInstruction_Accept";
/// The most holdings one transfer takes as inputs.
pub const MAX_INPUT_HOLDINGS: usize = 100;

/// How long after the prepare the synchronizer may record the transaction.
///
/// A prepare, a signature and an execute take seconds. The rest of the five
/// minutes absorbs clock drift and a slow host. After this time, Canton
/// refuses the transaction, so a resend cannot pay twice.
pub const MAX_RECORD_DELAY: Duration = Duration::from_secs(5 * 60);

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
    /// The template the factory must have. `None` leaves it unchecked.
    ///
    /// Write `package-id:Module:Entity` to pin one package, or
    /// `#package-name:Module:Entity` to pass any package of that name. A
    /// pinned send also asks the participant to prefer that package. A
    /// package upgrade changes the id, so a pinned send is refused after an
    /// upgrade until the caller passes the new id.
    ///
    /// [`TransferReceipt::factory_template`] shows the id the registry
    /// names. A host supplies that id, so check it against a package you
    /// trust before you pin it.
    pub factory_template: Option<String>,
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
            factory_template: None,
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

/// Refuse an id that is not a Canton contract id: lowercase hex of even
/// length that starts with `00` and has at least 66 digits.
///
/// An instruction id goes into the registry's URL path. Anything other than
/// hex could change that path.
pub(crate) fn check_contract_id(contract_id: &str) -> Result<()> {
    let valid = contract_id.len() >= 66
        && contract_id.len().is_multiple_of(2)
        && contract_id.starts_with("00")
        && contract_id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidContractId {
            contract_id: contract_id.to_string(),
        })
    }
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

/// What a committed send produced.
#[derive(Clone, Debug, Serialize)]
pub struct TransferReceipt {
    pub update_id: String,
    pub completion_offset: i64,
    pub factory_id: String,
    /// The factory's template, as `package-id:Module:Entity`. It is the
    /// template id of the registry's disclosed contract for the factory, or
    /// `None` when the registry names none. The registry supplies it, so it
    /// is for display until you check it against a package you trust.
    pub factory_template: Option<String>,
    /// `offer` means the receiver must accept before the asset moves.
    pub transfer_kind: String,
    pub input_holding_cids: Vec<String>,
    /// The hash the wallet signed, base64.
    pub prepared_transaction_hash: String,
}

/// What a committed accept produced.
#[derive(Clone, Debug, Serialize)]
pub struct AcceptReceipt {
    pub update_id: String,
    pub completion_offset: i64,
    pub instruction_cid: String,
    /// The hash the wallet signed, base64.
    pub prepared_transaction_hash: String,
}

/// Send `args.amount` of an instrument from `args.sender` to `args.receiver`.
///
/// When `args.input_holding_cids` is empty, the send spends every unlocked
/// holding of the instrument the sender owns, up to [`MAX_INPUT_HOLDINGS`].
/// The factory consumes what it needs and returns the change.
///
/// `key` must be the sender's key. The wallet signs only after the prepared
/// transaction passes its check: the hash is recomputed, and the root must be
/// `TransferFactory_Transfer` on the registry's factory, acting as the sender
/// alone. The transaction's metadata must carry the command id and the max
/// record time the wallet sent, and name the sender alone as its acting
/// party. The root's argument must carry these arguments: the instrument
/// admin, sender, receiver, amount, instrument, both times, and the input
/// holdings in order.
/// Its `transfer.meta` and `extraArgs` must be the ones the wallet sent.
///
/// When `args.factory_template` names a template, the factory must have it.
/// The pinned form binds the package id too, and the prepare asks the
/// participant to prefer that package. Without `factory_template`, the check
/// leaves the factory's template open. With smart contract upgrade, the
/// participant may then interpret the factory with any version of its
/// package.
///
/// # Errors
/// [`Error::NoHoldings`] before any registry or prepare call when there is
/// nothing to spend. [`Error::TransactionRefused`] when the prepared
/// transaction fails the check; nothing is signed or executed then.
/// [`Error::ExecuteOutcomeUnknown`] when the execute's answer did not arrive,
/// could not be read, or had a status that leaves the outcome open; the
/// transaction may have committed, so do not send again before checking.
/// Any ledger, registry or signer error otherwise.
pub async fn send_transfer(
    ledger: &LedgerClient,
    registry: &RegistryClient,
    key: &dyn Signer,
    args: &TransferArgs,
) -> Result<TransferReceipt> {
    let mut args = args.clone();
    if args.input_holding_cids.is_empty() {
        let holdings = ledger.holdings(&args.sender).await?;
        args.input_holding_cids = unlocked_holdings(
            &holdings,
            &args.sender,
            &args.instrument_admin,
            &args.instrument_id,
        )
        .into_iter()
        .map(|holding| holding.contract_id.clone())
        .collect();
    }
    if args.input_holding_cids.is_empty() {
        return Err(Error::NoHoldings {
            party: args.sender,
            instrument_admin: args.instrument_admin,
            instrument_id: args.instrument_id,
        });
    }
    tracing::info!(
        sender = %args.sender,
        inputs = args.input_holding_cids.len(),
        "selected the input holdings"
    );

    let factory = registry.transfer_factory(&args).await?;
    tracing::info!(
        registry = registry.root(),
        factory_id = %factory.factory_id,
        transfer_kind = %factory.transfer_kind,
        "fetched the transfer factory"
    );

    let argument = transfer_choice_argument(&args, &factory.choice_context.choice_context_data);
    let command = exercise(
        TRANSFER_FACTORY_INTERFACE_ID,
        &factory.factory_id,
        TRANSFER_CHOICE,
        &argument,
    );
    let command_id = Uuid::new_v4().to_string();
    let max_record_time = max_record_time_from_now();
    let expected = Expected {
        interface_id: TRANSFER_FACTORY_INTERFACE_ID,
        template_id: args.factory_template.as_deref(),
        choice: TRANSFER_CHOICE,
        contract_id: &factory.factory_id,
        party: &args.sender,
        command_id: &command_id,
        max_record_time: &max_record_time,
        extra_args: &argument["extraArgs"],
        transfer: Some(SentTransfer {
            args: &args,
            meta: &argument["transfer"]["meta"],
        }),
    };
    let submitted = submit(
        ledger,
        key,
        &args.sender,
        command,
        factory.choice_context.disclosed_contracts.clone(),
        &expected,
    )
    .await?;

    let factory_template = factory
        .choice_context
        .disclosed_contracts
        .iter()
        .find(|contract| contract.contract_id == factory.factory_id)
        .and_then(|contract| contract.template_id.clone());
    Ok(TransferReceipt {
        update_id: submitted.executed.update_id,
        completion_offset: submitted.executed.completion_offset,
        factory_id: factory.factory_id,
        factory_template,
        transfer_kind: factory.transfer_kind,
        input_holding_cids: args.input_holding_cids,
        prepared_transaction_hash: submitted.prepared_transaction_hash,
    })
}

/// Accept the transfer instruction `instruction_cid` as `party`, its
/// receiver.
///
/// `instruction_template` is the template the instruction must have, as
/// `#package-name:Module:Entity`, for example
/// [`AMULET_TRANSFER_INSTRUCTION_TEMPLATE`]. The instruction id usually comes
/// from the participant's own listing. Any template can implement the
/// interface, so the caller names the one it trusts.
///
/// The `#` form passes any package of that name. To pin one package, pass
/// `package-id:Module:Entity` instead. A pinned accept also asks the
/// participant to prefer that package. A package upgrade changes the id, so
/// a pinned accept is refused after an upgrade until the caller passes the
/// new id. An id read from a host, such as
/// [`TransferInstruction::package_id`](crate::TransferInstruction::package_id),
/// must be checked against a package you trust before you pin it.
///
/// `key` must be the party's key. The wallet signs only after the prepared
/// transaction passes its check: the hash is recomputed, and the root must be
/// `TransferInstruction_Accept` on this instruction, acting as `party` alone.
/// The transaction's metadata must carry the command id and the max record
/// time the wallet sent, and name `party` alone as its acting party. The
/// instruction must have `instruction_template`, and the `extraArgs` must be
/// the ones the wallet sent.
///
/// # Errors
/// [`Error::InvalidContractId`] before any call when `instruction_cid` is not
/// a contract id. [`Error::TransactionRefused`] when the prepared transaction
/// fails the check; nothing is signed or executed then.
/// [`Error::ExecuteOutcomeUnknown`] when the execute's answer did not arrive,
/// could not be read, or had a status that leaves the outcome open; the
/// transaction may have committed. Any ledger, registry or signer error
/// otherwise.
pub async fn accept_transfer(
    ledger: &LedgerClient,
    registry: &RegistryClient,
    key: &dyn Signer,
    party: &str,
    instruction_cid: &str,
    instruction_template: &str,
) -> Result<AcceptReceipt> {
    check_contract_id(instruction_cid)?;
    let context = registry.accept_context(instruction_cid).await?;
    tracing::info!(
        registry = registry.root(),
        instruction_cid,
        "fetched the accept context"
    );

    let argument = accept_choice_argument(&context.choice_context_data);
    let command = exercise(
        TRANSFER_INSTRUCTION_INTERFACE_ID,
        instruction_cid,
        ACCEPT_CHOICE,
        &argument,
    );
    let command_id = Uuid::new_v4().to_string();
    let max_record_time = max_record_time_from_now();
    let expected = Expected {
        interface_id: TRANSFER_INSTRUCTION_INTERFACE_ID,
        template_id: Some(instruction_template),
        choice: ACCEPT_CHOICE,
        contract_id: instruction_cid,
        party,
        command_id: &command_id,
        max_record_time: &max_record_time,
        extra_args: &argument["extraArgs"],
        transfer: None,
    };
    let submitted = submit(
        ledger,
        key,
        party,
        command,
        context.disclosed_contracts,
        &expected,
    )
    .await?;

    Ok(AcceptReceipt {
        update_id: submitted.executed.update_id,
        completion_offset: submitted.executed.completion_offset,
        instruction_cid: instruction_cid.to_string(),
        prepared_transaction_hash: submitted.prepared_transaction_hash,
    })
}

/// A JSON Ledger API exercise through an interface.
fn exercise(interface_id: &str, contract_id: &str, choice: &str, argument: &Value) -> Value {
    json!({
        "ExerciseCommand": {
            "templateId": interface_id,
            "contractId": contract_id,
            "choice": choice,
            "choiceArgument": argument,
        }
    })
}

/// Now plus [`MAX_RECORD_DELAY`], as RFC 3339 with microseconds. The ledger
/// keeps microseconds, so the check can compare this time exactly.
fn max_record_time_from_now() -> String {
    let now = Utc::now();
    let delay = TimeDelta::from_std(MAX_RECORD_DELAY).unwrap_or(TimeDelta::MAX);
    now.checked_add_signed(delay)
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
        .to_rfc3339_opts(SecondsFormat::Micros, true)
}

struct Submitted {
    executed: Executed,
    prepared_transaction_hash: String,
}

/// Prepare `command` as `party` under `expected.command_id` and
/// `expected.max_record_time`, check the result, sign its hash with `key`,
/// and execute it.
async fn submit(
    ledger: &LedgerClient,
    key: &dyn Signer,
    party: &str,
    command: Value,
    disclosed_contracts: Vec<DisclosedContract>,
    expected: &Expected<'_>,
) -> Result<Submitted> {
    let prepared = ledger
        .prepare(&PrepareRequest {
            user_id: ledger.user_id().to_string(),
            command_id: expected.command_id.to_string(),
            act_as: vec![party.to_string()],
            read_as: Vec::new(),
            commands: vec![command],
            disclosed_contracts,
            synchronizer_id: String::new(),
            package_id_selection_preference: pinned_package(expected.template_id)
                .into_iter()
                .collect(),
            verbose_hashing: false,
            max_record_time: expected.max_record_time.to_string(),
        })
        .await?;

    let hash = check_prepared(ledger.base_url(), &prepared, expected)?;
    tracing::info!(
        party,
        hash = %prepared.prepared_transaction_hash,
        scheme = %prepared.hashing_scheme_version,
        "the prepared transaction passed the check; signing its hash"
    );
    let signature = key.sign_b64_async(&hash).await?;

    let submission_id = Uuid::new_v4().to_string();
    let executed = ledger
        .execute(&ExecuteRequest {
            user_id: ledger.user_id().to_string(),
            submission_id: submission_id.clone(),
            prepared_transaction: prepared.prepared_transaction,
            hashing_scheme_version: prepared.hashing_scheme_version,
            deduplication_period: json!({ "Empty": {} }),
            party_signatures: json!({
                "signatures": [{
                    "party": party,
                    "signatures": [{
                        "format": "SIGNATURE_FORMAT_CONCAT",
                        "signature": signature,
                        "signedBy": key.fingerprint(),
                        "signingAlgorithmSpec": "SIGNING_ALGORITHM_SPEC_ED25519",
                    }],
                }],
            }),
        })
        .await
        .map_err(|error| {
            after_execute(
                error,
                expected.command_id,
                &submission_id,
                &prepared.prepared_transaction_hash,
                expected.max_record_time,
            )
        })?;
    tracing::info!(
        party,
        update_id = %executed.update_id,
        completion_offset = executed.completion_offset,
        "the transaction committed"
    );

    Ok(Submitted {
        executed,
        prepared_transaction_hash: prepared.prepared_transaction_hash,
    })
}

/// The package id of a pinned template, `package-id:Module:Entity`.
///
/// With smart contract upgrade, the participant picks a version of each
/// package by name. A pinned check refuses every version but one, so the
/// prepare asks for that one. The `#` form pins nothing.
fn pinned_package(template_id: Option<&str>) -> Option<String> {
    let template = template_id.filter(|template| !template.starts_with('#'))?;
    let (package_id, _) = template.split_once(':')?;
    (!package_id.is_empty()).then(|| package_id.to_string())
}

/// An execute error, as the caller must read it.
///
/// Once the request has left the wallet, the participant may have committed
/// the transaction. These errors become [`Error::ExecuteOutcomeUnknown`]:
///
/// - A transport error after the connection opened.
/// - An answer the client could not read: a body that ended early or timed
///   out, that is not JSON, or that is over the cap.
/// - A 408 or 500 from the participant. Its HTTP server answers 408 when it
///   stops waiting, while the submission may still run. Canton's JSON API
///   answers 500 for an internal, unknown or data-loss gRPC error. That
///   error can follow a submission the participant accepted.
/// - A 502, 503 or 504. A gateway in front of the participant answers these
///   while the participant may still execute.
///
/// A connection that never opened reached nobody and stays
/// [`Error::Transport`]. Any other non-2xx answer stays [`Error::Api`].
///
/// [`Error::ResponseTooLarge`] does not carry the status, so a non-2xx
/// answer with a body over the cap also becomes an unknown outcome.
fn after_execute(
    error: Error,
    command_id: &str,
    submission_id: &str,
    prepared_transaction_hash: &str,
    max_record_time: &str,
) -> Error {
    let left_the_wallet = match &error {
        Error::Transport { source, .. } => !source.is_connect(),
        Error::Decode { .. } | Error::Json { .. } | Error::ResponseTooLarge { .. } => true,
        Error::Api { status, .. } => matches!(status, 408 | 500 | 502..=504),
        _ => false,
    };
    if !left_the_wallet {
        return error;
    }
    Error::ExecuteOutcomeUnknown {
        host: error.host().to_string(),
        command_id: command_id.to_string(),
        submission_id: submission_id.to_string(),
        prepared_transaction_hash: prepared_transaction_hash.to_string(),
        max_record_time: max_record_time.into(),
        source: Box::new(error),
    }
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
            factory_template: None,
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
    fn only_a_canton_contract_id_passes() {
        let id = format!("00{}", "ab".repeat(32));
        assert!(check_contract_id(&id).is_ok());
        assert!(check_contract_id(&format!("{id}ca111220aa")).is_ok());
        for bad in [
            String::new(),
            "00b1".to_string(),
            format!("{id}a"),
            format!("11{}", "ab".repeat(32)),
            format!("00{}", "AB".repeat(32)),
            format!("00{}/../admin", "ab".repeat(32)),
            format!("00{}?x=1", "ab".repeat(32)),
        ] {
            assert!(
                matches!(check_contract_id(&bad), Err(Error::InvalidContractId { contract_id }) if contract_id == bad),
                "{bad} must be refused"
            );
        }
    }

    #[test]
    fn only_a_pinned_template_names_a_package_to_prefer() {
        assert_eq!(
            pinned_package(Some("amuletpkg:Splice.Amulet:Amulet")).as_deref(),
            Some("amuletpkg")
        );
        for unpinned in [
            None,
            Some(AMULET_TRANSFER_INSTRUCTION_TEMPLATE),
            Some(":Splice.Amulet:Amulet"),
            Some("Amulet"),
        ] {
            assert_eq!(pinned_package(unpinned), None, "{unpinned:?}");
        }
    }

    const MAX_RECORD_TIME: &str = "2026-11-07T12:05:00.000000Z";

    fn transport(source: reqwest::Error) -> Error {
        Error::Transport {
            host: "http://127.0.0.1".to_string(),
            method: "POST",
            path: "/v2/interactive-submission/executeAndWait".to_string(),
            source,
        }
    }

    /// Nothing listens on the port, so the request never left the wallet.
    #[tokio::test]
    async fn an_execute_that_never_connected_keeps_its_transport_error() {
        let address = match std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
        {
            Ok(address) => address,
            Err(e) => panic!("a free port must be found: {e}"),
        };
        let source = match reqwest::Client::new()
            .post(format!("http://{address}/"))
            .send()
            .await
        {
            Ok(_) => panic!("nothing listens on {address}"),
            Err(e) => e,
        };
        assert!(source.is_connect(), "{source}");

        let error = after_execute(
            transport(source),
            "command-1",
            "submission-1",
            "aGFzaA==",
            MAX_RECORD_TIME,
        );

        assert!(matches!(error, Error::Transport { .. }), "{error}");
    }

    /// The request reached the participant and the answer did not come back.
    #[tokio::test]
    async fn an_execute_that_timed_out_has_an_unknown_outcome() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::any())
            .respond_with(wiremock::ResponseTemplate::new(200).set_delay(Duration::from_secs(3)))
            .mount(&server)
            .await;
        let source = match reqwest::Client::builder()
            .timeout(Duration::from_millis(200))
            .build()
            .map(|client| client.post(server.uri()).send())
        {
            Ok(sent) => match sent.await {
                Ok(_) => panic!("the answer must come too late"),
                Err(e) => e,
            },
            Err(e) => panic!("the client must build: {e}"),
        };

        let error = after_execute(
            transport(source),
            "command-1",
            "submission-1",
            "aGFzaA==",
            MAX_RECORD_TIME,
        );

        match &error {
            Error::ExecuteOutcomeUnknown {
                command_id,
                submission_id,
                prepared_transaction_hash,
                max_record_time,
                ..
            } => {
                assert_eq!(command_id, "command-1");
                assert_eq!(submission_id, "submission-1");
                assert_eq!(prepared_transaction_hash, "aGFzaA==");
                assert_eq!(&**max_record_time, MAX_RECORD_TIME);
            }
            other => panic!("expected an unknown outcome, got {other}"),
        }
        let message = error.to_string();
        assert!(message.contains("may have committed"), "{message}");
        assert!(
            message.contains(
                "After 2026-11-07T12:05:00.000000Z, with no completion for command command-1, \
                 the transaction did not commit and a new one is safe."
            ),
            "{message}"
        );
    }

    fn answered(status: u16) -> Error {
        Error::Api {
            host: "http://127.0.0.1".to_string(),
            method: "POST",
            path: "/v2/interactive-submission/executeAndWait".to_string(),
            status,
            message: "refused".to_string(),
        }
    }

    #[test]
    fn an_execute_answered_with_an_error_status_keeps_its_api_error() {
        for status in [400, 409, 429] {
            let error = after_execute(
                answered(status),
                "command-1",
                "submission-1",
                "aGFzaA==",
                MAX_RECORD_TIME,
            );

            assert!(error.is_status(status), "{status}: {error}");
        }
    }

    /// A 408 or 500 from the participant, or a gateway's 502, 503 or 504,
    /// can follow a submission that still commits.
    #[test]
    fn an_execute_answered_with_a_status_that_leaves_the_outcome_open_is_unknown() {
        for status in [408, 500, 502, 503, 504] {
            let error = after_execute(
                answered(status),
                "command-1",
                "submission-1",
                "aGFzaA==",
                MAX_RECORD_TIME,
            );

            match &error {
                Error::ExecuteOutcomeUnknown {
                    command_id,
                    submission_id,
                    prepared_transaction_hash,
                    max_record_time,
                    source,
                    ..
                } => {
                    assert_eq!(command_id, "command-1");
                    assert_eq!(submission_id, "submission-1");
                    assert_eq!(prepared_transaction_hash, "aGFzaA==");
                    assert_eq!(&**max_record_time, MAX_RECORD_TIME);
                    assert!(source.is_status(status), "{status}: {source}");
                }
                other => panic!("{status}: expected an unknown outcome, got {other}"),
            }
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
