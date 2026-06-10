//! Token-standard registry lookups for transfer flows.
//!
//! `AcceptTransferProposal` execution invokes
//! `TransferInstruction_Accept`, which reads `utility.digitalasset.com/transfer-rule`
//! (and friends) from `extraArgs.context.values` and references the contracts
//! through `disclosed_contracts` on the submission. The registry's
//! `…/choice-contexts/accept` endpoint returns both pieces in one call.
//!
//! This module wraps `canton_registry::accept_context::get` and surfaces a
//! type the propose/execute handlers can pass into the action serializer and
//! the submission builder respectively.

use std::collections::HashMap;

use anyhow::Context as _;
use base64::Engine as _;
use canton_common::{
    decimal::DamlDecimal,
    transfer::{DisclosedContract as RegistryDisclosedContract, InstrumentId, Meta, Transfer},
    transfer_factory::{
        ChoiceArguments, Context, ContextValue, ExtraArgs, Meta as FactoryMeta,
        MetaValue as FactoryMetaValue,
    },
};
use canton_proto_rs::com::daml::ledger::api::v2::{
    CumulativeFilter, DisclosedContract, EventFormat, Filters, GetEventsByContractIdRequest,
    Identifier, InterfaceFilter, Record, WildcardFilter, cumulative_filter, value,
};
use chrono::DateTime;

use crate::{
    canton_id::CantonId,
    config::{Network, NodeConfig},
    error::Result,
    utils,
};

fn micros_to_rfc3339(micros: i64) -> Result<String> {
    DateTime::from_timestamp_micros(micros)
        .map(|dt| dt.to_rfc3339())
        .ok_or_else(|| anyhow::anyhow!("timestamp {micros} micros is out of range for RFC3339"))
}

/// The data needed to submit an `AcceptTransfer` flow against the ledger.
///
/// `context` lands inside the proposal's `extraArgs.context.values` at
/// proposal-creation time. `disclosed_contracts` must be attached to the
/// `Commands` submission at execute-confirmed-action time (the contracts are
/// not stored on the ledger so the executor needs them on the wire).
#[derive(Debug, Clone)]
pub struct AcceptTransferContext {
    pub context: Context,
    pub disclosed_contracts: Vec<RegistryDisclosedContract>,
}

/// Registry-resolved data for a `Transfer` proposal whose instrument is
/// administered by a shared party (e.g. CBTC, admin = `cbtc-network`). The
/// `factory_cid` is the registrar's singleton `TransferFactory` contract that
/// the proposal must reference; `context` populates `extraArgs.context.values`;
/// `disclosed_contracts` must accompany the execute-time submission.
#[derive(Debug, Clone)]
pub struct ProposeTransferContext {
    pub factory_cid: String,
    pub context: Context,
    pub disclosed_contracts: Vec<RegistryDisclosedContract>,
}

/// Decide whether a `Transfer` proposal must resolve its choice context from
/// the token-standard registry before creation.
///
/// Returns `true` for utility-registry instruments, whose `TransferFactory`
/// reads `utility.digitalasset.com/instrument-configuration` (and friends) from
/// `extraArgs.context.values` at execute time:
///
///   * **Self-administered dec-party tokens** (`instrument_admin == dec_party`):
///     the factory lives on the dec party's own ACS, so the UI supplies a
///     non-empty `transfer_factory_cid` — but the choice context still has to be
///     fetched and baked into the proposal, or execute fails with
///     `Missing context entry for utility.digitalasset.com/instrument-configuration`.
///   * **Shared instruments** (e.g. CBTC, `admin = cbtc-network`): the factory
///     lives on the registrar, so the UI leaves `transfer_factory_cid` empty and
///     the registrar resolves both the cid and the context.
///
/// Returns `false` for Canton Coin, whose `AmuletRules` factory and choice
/// context come from the DSO scan API rather than this registry. The UI surfaces
/// CC as a synthetic factory keyed on the DSO party (non-empty cid, admin is the
/// DSO), so it is excluded by both predicates.
pub fn needs_registry_context(
    transfer_factory_cid: &str,
    instrument_admin: &str,
    dec_party: &str,
) -> bool {
    transfer_factory_cid.is_empty() || instrument_admin == dec_party
}

/// Call the registrar's `transfer-factory` endpoint to resolve the singleton
/// `TransferFactory` cid and the choice context required to exercise
/// `TransferFactory_Transfer` later. Used by the propose handler for
/// shared-instrument transfers where the factory isn't on the dec party's ACS.
/// Inputs that need to match byte-for-byte between the registry's choice-
/// context resolution and the on-chain `TransferProposal` — drifting on any
/// field invalidates the context returned by the registrar.
pub struct ProposeTransferArgs<'a> {
    pub sender: &'a CantonId,
    pub receiver: &'a CantonId,
    pub amount: &'a DamlDecimal,
    pub instrument_admin: &'a CantonId,
    pub instrument_id: &'a str,
    pub input_holding_cids: &'a [String],
    pub requested_at_micros: i64,
    pub execute_before_micros: i64,
}

pub async fn fetch_factory_for_propose(
    network: Network,
    args: ProposeTransferArgs<'_>,
) -> Result<ProposeTransferContext> {
    let request = canton_registry::transfer_factory::Request {
        choice_arguments: ChoiceArguments {
            expected_admin: args.instrument_admin.to_string(),
            transfer: Transfer {
                sender: args.sender.to_string(),
                receiver: args.receiver.to_string(),
                amount: *args.amount,
                instrument_id: InstrumentId {
                    admin: args.instrument_admin.to_string(),
                    id: args.instrument_id.to_string(),
                },
                requested_at: micros_to_rfc3339(args.requested_at_micros)?,
                execute_before: micros_to_rfc3339(args.execute_before_micros)?,
                input_holding_cids: Some(args.input_holding_cids.to_vec()),
                // `values` has no skip_serializing_if, so `None` serializes as
                // `"values": null`. The registry's token-standard Metadata
                // decoder expects an object — `null` fails with
                // "Expected { but was null". Send an empty map instead.
                meta: Some(Meta {
                    values: Some(HashMap::new()),
                }),
            },
            extra_args: ExtraArgs {
                context: Context {
                    values: HashMap::new(),
                },
                meta: FactoryMeta {
                    values: FactoryMetaValue {},
                },
            },
        },
        exclude_debug_fields: true,
    };
    let response =
        canton_registry::transfer_factory::get(canton_registry::transfer_factory::Params {
            registry_url: network.registry_url().to_string(),
            decentralized_party_id: args.instrument_admin.to_string(),
            request,
        })
        .await
        .map_err(|e| anyhow::anyhow!("registry transfer-factory request failed: {e}"))?;

    Ok(ProposeTransferContext {
        factory_cid: response.factory_id,
        context: response.choice_context.choice_context_data,
        disclosed_contracts: response.choice_context.disclosed_contracts,
    })
}

/// Fetch the accept choice context from the token-standard registry for a
/// given `TransferInstruction`. Used by the propose handler (to bake the
/// context into the proposal) and the execute handler (to attach disclosed
/// contracts to the submission).
///
/// The registry serves the choice context under the instrument's *registrar*
/// (`/registrars/{admin}/…`), which is the instrument admin — NOT the
/// accepting party. For utility tokens a dec-party administers itself the two
/// coincide, but for shared instruments (e.g. CBTC, admin = `cbtc-network`)
/// they differ, so we resolve the admin from the instruction rather than
/// assuming it's the caller. `party_id` is the accepting party, used only for
/// ledger visibility when looking the instruction up.
pub async fn fetch(
    config: &NodeConfig,
    token: Option<String>,
    network: Network,
    party_id: &CantonId,
    transfer_instruction_cid: &str,
) -> Result<AcceptTransferContext> {
    let registrar =
        fetch_instruction_registrar(config, token, party_id, transfer_instruction_cid).await?;

    let response = canton_registry::accept_context::get(canton_registry::accept_context::Params {
        registry_url: network.registry_url().to_string(),
        decentralized_party_id: registrar.to_string(),
        // Upstream field is named `transfer_offer_contract_id` but the
        // registry endpoint actually keys on the TransferInstruction cid.
        // Naming mismatch is in canton-lib, not here.
        transfer_offer_contract_id: transfer_instruction_cid.to_string(),
        request: canton_registry::accept_context::Request {
            meta: canton_registry::accept_context::Meta {
                values: String::new(),
            },
        },
    })
    .await
    .map_err(|e| anyhow::anyhow!("registry accept-context request failed: {e}"))?;

    // The registry's `choiceContextData` JSON is `{"values": {<key>: <AnyValue>, ...}}`.
    // `canton_registry::accept_context::Response` already strips the outer wrapper into
    // `ChoiceContextData { values: serde_json::Value }`, so `response.choice_context_data.values`
    // is the inner key→AnyValue map. Deserialize it directly into the map type and wrap.
    let values: HashMap<String, ContextValue> =
        serde_json::from_value(response.choice_context_data.values)
            .context("Failed to deserialize registry choice_context_data.values")?;

    Ok(AcceptTransferContext {
        context: Context { values },
        disclosed_contracts: response.disclosed_contracts,
    })
}

/// Resolve the token-standard registrar for a `TransferInstruction` — its
/// instrument admin — by reading the instruction's interface view. The
/// registry's accept choice-context is keyed on this registrar, not on the
/// accepting party.
async fn fetch_instruction_registrar(
    config: &NodeConfig,
    token: Option<String>,
    party_id: &CantonId,
    transfer_instruction_cid: &str,
) -> Result<CantonId> {
    let mut client = utils::create_event_query_client(config, token).await?;

    let mut filters_by_party = HashMap::new();
    filters_by_party.insert(
        party_id.to_string(),
        Filters {
            cumulative: vec![CumulativeFilter {
                identifier_filter: Some(cumulative_filter::IdentifierFilter::InterfaceFilter(
                    InterfaceFilter {
                        interface_id: Some(Identifier {
                            package_id: "#splice-api-token-transfer-instruction-v1".to_string(),
                            module_name: "Splice.Api.Token.TransferInstructionV1".to_string(),
                            entity_name: "TransferInstruction".to_string(),
                        }),
                        include_interface_view: true,
                        include_created_event_blob: false,
                    },
                )),
            }],
        },
    );

    let request = GetEventsByContractIdRequest {
        contract_id: transfer_instruction_cid.to_string(),
        event_format: Some(EventFormat {
            filters_by_party,
            filters_for_any_party: None,
            verbose: true,
        }),
    };

    let created_event = client
        .get_events_by_contract_id(tonic::Request::new(request))
        .await
        .context("Failed to query transfer instruction by contract id")?
        .into_inner()
        .created
        .and_then(|c| c.created_event)
        .context("Transfer instruction not found or not visible to party")?;

    let view_record = created_event
        .interface_views
        .iter()
        .find(|v| {
            v.interface_id.as_ref().is_some_and(|id| {
                id.module_name == "Splice.Api.Token.TransferInstructionV1"
                    && id.entity_name == "TransferInstruction"
            })
        })
        .and_then(|v| v.view_value.as_ref())
        .context("Transfer instruction interface view missing")?;

    let instrument_record = view_record
        .fields
        .iter()
        .find(|f| f.label == "transfer")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Record(r)) => Some(r),
            _ => None,
        })
        .and_then(|transfer| {
            transfer
                .fields
                .iter()
                .find(|f| f.label == "instrumentId")
                .and_then(|f| f.value.as_ref())
                .and_then(|v| match &v.sum {
                    Some(value::Sum::Record(r)) => Some(r),
                    _ => None,
                })
        })
        .context("Transfer instruction missing transfer.instrumentId")?;

    let admin = instrument_record
        .fields
        .iter()
        .find(|f| f.label == "admin")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Party(p)) => Some(p.clone()),
            _ => None,
        })
        .context("instrumentId missing admin party")?;

    admin
        .parse()
        .context("Failed to parse instrument admin as a party id")
}

/// Inspect a governance proposal contract by cid; if it's an
/// `AcceptTransferProposal`, fetch the choice context from the registry and
/// return it so the executor can attach disclosed contracts. Returns `Ok(None)`
/// for any other proposal type so the caller can pass through.
///
/// The lookup uses `EventQueryService.GetEventsByContractId`, which returns
/// the create event for an exact contract id (cheap, single round-trip).
pub async fn maybe_fetch_for_proposal(
    config: &NodeConfig,
    token: Option<String>,
    party_id: &CantonId,
    proposal_cid: &str,
) -> Result<Option<AcceptTransferContext>> {
    // `fetch` (below) needs the token too — clone before this client consumes it.
    let mut client = utils::create_event_query_client(config, token.clone()).await?;

    // `GetEventsByContractId` filters by party-visibility so the requester
    // must be authorized to read the proposal. Use the party that's executing
    // the action — they're a stakeholder on the governance proposal.
    let mut filters_by_party = HashMap::new();
    filters_by_party.insert(
        party_id.to_string(),
        Filters {
            cumulative: vec![CumulativeFilter {
                identifier_filter: Some(cumulative_filter::IdentifierFilter::WildcardFilter(
                    WildcardFilter {
                        include_created_event_blob: false,
                    },
                )),
            }],
        },
    );

    let request = GetEventsByContractIdRequest {
        contract_id: proposal_cid.to_string(),
        event_format: Some(EventFormat {
            filters_by_party,
            filters_for_any_party: None,
            verbose: true,
        }),
    };

    let response = client
        .get_events_by_contract_id(tonic::Request::new(request))
        .await
        .context("Failed to query event for proposal contract id")?
        .into_inner();

    let Some(created) = response.created else {
        // Proposal not visible or already archived — caller's submission will
        // surface the right error, no need to second-guess it here.
        return Ok(None);
    };
    let Some(created_event) = created.created_event else {
        return Ok(None);
    };

    // Identify the proposal template by id. The package id is package-name-resolved
    // (e.g. `#governance-token-custody-v1-rc1`) so match on the module + entity tuple.
    let template = created_event.template_id.as_ref();
    let is_accept_transfer = template
        .map(|t| {
            t.module_name == "Governance.TokenCustody.AcceptTransfer"
                && t.entity_name == "AcceptTransferProposal"
        })
        .unwrap_or(false);
    let is_transfer = template
        .map(|t| {
            t.module_name == "Governance.TokenCustody.TransferProposal"
                && t.entity_name == "TransferProposal"
        })
        .unwrap_or(false);
    if !is_accept_transfer && !is_transfer {
        return Ok(None);
    }

    let Some(create_args) = created_event.create_arguments else {
        anyhow::bail!("proposal create_arguments missing in event response");
    };

    if is_accept_transfer {
        let transfer_instruction_cid = create_args
            .fields
            .iter()
            .find(|f| f.label == "transferInstructionCid")
            .and_then(|f| f.value.as_ref())
            .and_then(|v| match &v.sum {
                Some(value::Sum::ContractId(cid)) => Some(cid.clone()),
                _ => None,
            })
            .context("AcceptTransferProposal missing transferInstructionCid field")?;

        let ctx = fetch(
            config,
            token,
            config.canton.network,
            party_id,
            &transfer_instruction_cid,
        )
        .await?;

        return Ok(Some(ctx));
    }

    // is_transfer: re-resolve disclosed contracts for the stored TransferProposal
    // so the executor's submission can exercise TransferFactory_Transfer. Only
    // needed for shared-instrument transfers (e.g. CBTC); for utility tokens the
    // factory lives in the dec party's own ACS and no extra disclosure is needed.
    let transfer = transfer_record_from_proposal(&create_args)?;
    let instrument_admin: CantonId = transfer
        .instrument_id
        .admin
        .parse()
        .context("TransferProposal transfer.instrumentId.admin is not a valid party id")?;
    if &instrument_admin == party_id {
        return Ok(None);
    }
    let sender: CantonId = transfer
        .sender
        .parse()
        .context("TransferProposal transfer.sender is not a valid party id")?;
    let receiver: CantonId = transfer
        .receiver
        .parse()
        .context("TransferProposal transfer.receiver is not a valid party id")?;
    let resolved = fetch_factory_for_propose(
        config.canton.network,
        ProposeTransferArgs {
            sender: &sender,
            receiver: &receiver,
            amount: &transfer.amount,
            instrument_admin: &instrument_admin,
            instrument_id: &transfer.instrument_id.id,
            input_holding_cids: transfer.input_holding_cids.as_deref().unwrap_or(&[]),
            requested_at_micros: transfer.requested_at_micros,
            execute_before_micros: transfer.execute_before_micros,
        },
    )
    .await?;

    Ok(Some(AcceptTransferContext {
        context: resolved.context,
        disclosed_contracts: resolved.disclosed_contracts,
    }))
}

/// Decoded view of a `TransferProposal`'s `transfer` record. Keeps the raw
/// micros for `requestedAt` / `executeBefore` so the registry call can build a
/// request that matches the on-chain choice arguments byte-for-byte.
#[derive(Debug)]
struct StoredTransfer {
    sender: String,
    receiver: String,
    amount: DamlDecimal,
    instrument_id: InstrumentId,
    input_holding_cids: Option<Vec<String>>,
    requested_at_micros: i64,
    execute_before_micros: i64,
}

/// Pull the `transfer` record out of a `TransferProposal` create_arguments.
fn transfer_record_from_proposal(record: &Record) -> Result<StoredTransfer> {
    let transfer_value = record
        .fields
        .iter()
        .find(|f| f.label == "transfer")
        .and_then(|f| f.value.as_ref())
        .context("TransferProposal missing transfer field")?;
    let transfer_record = match &transfer_value.sum {
        Some(value::Sum::Record(r)) => r,
        _ => anyhow::bail!("TransferProposal transfer field is not a record"),
    };
    let sender = field_party_str(transfer_record, "sender")?;
    let receiver = field_party_str(transfer_record, "receiver")?;
    let amount = DamlDecimal::parse(&field_numeric_str(transfer_record, "amount")?)
        .context("TransferProposal transfer.amount is not a valid Daml decimal")?;
    let instrument_record = transfer_record
        .fields
        .iter()
        .find(|f| f.label == "instrumentId")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Record(r)) => Some(r),
            _ => None,
        })
        .context("transfer record missing instrumentId")?;
    let admin = field_party_str(instrument_record, "admin")?;
    let id = field_text_str(instrument_record, "id")?;
    let input_holding_cids = transfer_record
        .fields
        .iter()
        .find(|f| f.label == "inputHoldingCids")
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::List(l)) => Some(
                l.elements
                    .iter()
                    .filter_map(|el| match &el.sum {
                        Some(value::Sum::ContractId(cid)) => Some(cid.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        });
    let requested_at_micros = field_timestamp_micros(transfer_record, "requestedAt")
        .context("TransferProposal transfer.requestedAt missing or not a Timestamp")?;
    let execute_before_micros = field_timestamp_micros(transfer_record, "executeBefore")
        .context("TransferProposal transfer.executeBefore missing or not a Timestamp")?;
    Ok(StoredTransfer {
        sender,
        receiver,
        amount,
        instrument_id: InstrumentId { admin, id },
        input_holding_cids,
        requested_at_micros,
        execute_before_micros,
    })
}

fn field_timestamp_micros(record: &Record, label: &str) -> Option<i64> {
    record
        .fields
        .iter()
        .find(|f| f.label == label)
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Timestamp(t)) => Some(*t),
            _ => None,
        })
}

fn field_party_str(record: &Record, label: &str) -> Result<String> {
    record
        .fields
        .iter()
        .find(|f| f.label == label)
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Party(p)) => Some(p.clone()),
            _ => None,
        })
        .with_context(|| format!("missing party field {label}"))
}

fn field_text_str(record: &Record, label: &str) -> Result<String> {
    record
        .fields
        .iter()
        .find(|f| f.label == label)
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Text(t)) => Some(t.clone()),
            _ => None,
        })
        .with_context(|| format!("missing text field {label}"))
}

fn field_numeric_str(record: &Record, label: &str) -> Result<String> {
    record
        .fields
        .iter()
        .find(|f| f.label == label)
        .and_then(|f| f.value.as_ref())
        .and_then(|v| match &v.sum {
            Some(value::Sum::Numeric(n)) => Some(n.clone()),
            _ => None,
        })
        .with_context(|| format!("missing numeric field {label}"))
}

/// Translate the registry's JSON `DisclosedContract` view into the Ledger
/// API's proto form, base64-decoding the created-event blob.
pub fn to_proto_disclosed_contracts(
    contracts: &[RegistryDisclosedContract],
) -> Result<Vec<DisclosedContract>> {
    contracts
        .iter()
        .map(|dc| {
            let created_event_blob = base64::engine::general_purpose::STANDARD
                .decode(&dc.created_event_blob)
                .context("Invalid base64 in registry-provided disclosed contract blob")?;
            Ok(DisclosedContract {
                template_id: None,
                contract_id: dc.contract_id.clone(),
                created_event_blob,
                synchronizer_id: dc.synchronizer_id.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::daml::ledger::api::v2::{List, RecordField, Value};

    use super::*;

    const DEC_PARTY: &str = "Test01::1220c5deadbeef";
    const DSO: &str = "DSO::1220ffaabbcc";

    // ---- Small `Value` constructors (mirrors action_serializer.rs:26-94, which
    // are module-private). Kept local so the parser tests can build the proto
    // `Record` inputs they exercise. ----

    fn party(s: &str) -> Value {
        Value {
            sum: Some(value::Sum::Party(s.to_string())),
        }
    }

    fn text(s: &str) -> Value {
        Value {
            sum: Some(value::Sum::Text(s.to_string())),
        }
    }

    fn numeric(s: &str) -> Value {
        Value {
            sum: Some(value::Sum::Numeric(s.to_string())),
        }
    }

    fn timestamp(n: i64) -> Value {
        Value {
            sum: Some(value::Sum::Timestamp(n)),
        }
    }

    fn contract_id(s: &str) -> Value {
        Value {
            sum: Some(value::Sum::ContractId(s.to_string())),
        }
    }

    fn list(vs: Vec<Value>) -> Value {
        Value {
            sum: Some(value::Sum::List(List { elements: vs })),
        }
    }

    fn record(fields: Vec<(&str, Value)>) -> Value {
        Value {
            sum: Some(value::Sum::Record(Record {
                record_id: None,
                fields: fields
                    .into_iter()
                    .map(|(label, value)| RecordField {
                        label: label.to_string(),
                        value: Some(value),
                    })
                    .collect(),
            })),
        }
    }

    /// Unwrap a `Value` into the inner proto `Record`, panicking with a clear
    /// message if it isn't one (test-only helper, so a panic is the assertion).
    fn as_record(v: Value) -> Record {
        match v.sum {
            Some(value::Sum::Record(r)) => r,
            other => panic!("expected a Record value, got {other:?}"),
        }
    }

    /// A fully-populated `transfer` record nested inside a `TransferProposal`
    /// create-arguments record, matching the field names the parser reads.
    fn valid_transfer_inner() -> Value {
        record(vec![
            ("sender", party(DEC_PARTY)),
            ("receiver", party(DSO)),
            ("amount", numeric("12.5")),
            (
                "instrumentId",
                record(vec![
                    ("admin", party("cbtc-network::1220aa")),
                    ("id", text("CBTC")),
                ]),
            ),
            ("requestedAt", timestamp(1_700_000_000_000_000)),
            ("executeBefore", timestamp(1_700_000_600_000_000)),
            (
                "inputHoldingCids",
                list(vec![contract_id("00holdingaa"), contract_id("00holdingbb")]),
            ),
        ])
    }

    /// Wrap a `transfer` value in the outer `TransferProposal` create-arguments
    /// record the parser is handed.
    fn proposal_with_transfer(transfer: Value) -> Record {
        as_record(record(vec![("transfer", transfer)]))
    }

    #[test]
    fn transfer_record_from_proposal_parses_full_record() -> Result {
        let proposal = proposal_with_transfer(valid_transfer_inner());

        let parsed = transfer_record_from_proposal(&proposal)?;

        assert_eq!(parsed.sender, DEC_PARTY);
        assert_eq!(parsed.receiver, DSO);
        assert_eq!(parsed.amount, DamlDecimal::parse("12.5")?);
        assert_eq!(parsed.instrument_id.admin, "cbtc-network::1220aa");
        assert_eq!(parsed.instrument_id.id, "CBTC");
        assert_eq!(parsed.requested_at_micros, 1_700_000_000_000_000);
        assert_eq!(parsed.execute_before_micros, 1_700_000_600_000_000);
        assert_eq!(
            parsed.input_holding_cids,
            Some(vec!["00holdingaa".to_string(), "00holdingbb".to_string()]),
        );

        Ok(())
    }

    /// Assert a `Result` is `Err` and its rendered message contains `needle`.
    /// Test-only: a non-matching error trips a `panic!` (an allowed assertion
    /// macro), keeping the suite free of `.unwrap_err()`.
    fn assert_err_contains<T: std::fmt::Debug>(result: Result<T>, needle: &str) {
        match result {
            Ok(v) => panic!("expected Err containing {needle:?}, got Ok({v:?})"),
            Err(e) => assert!(
                e.to_string().contains(needle),
                "error {e:?} did not contain {needle:?}"
            ),
        }
    }

    #[test]
    fn transfer_record_from_proposal_missing_transfer_field_errs() {
        // Outer record with no `transfer` field at all.
        let proposal = as_record(record(vec![("notTransfer", text("x"))]));

        assert_err_contains(
            transfer_record_from_proposal(&proposal),
            "missing transfer field",
        );
    }

    #[test]
    fn transfer_record_from_proposal_transfer_not_record_errs() {
        // `transfer` present but a Text, not a Record.
        let proposal = as_record(record(vec![("transfer", text("not a record"))]));

        assert_err_contains(transfer_record_from_proposal(&proposal), "is not a record");
    }

    #[test]
    fn transfer_record_from_proposal_bad_amount_errs() {
        let transfer = record(vec![
            ("sender", party(DEC_PARTY)),
            ("receiver", party(DSO)),
            ("amount", numeric("not-a-decimal")),
            (
                "instrumentId",
                record(vec![("admin", party(DSO)), ("id", text("CBTC"))]),
            ),
            ("requestedAt", timestamp(1)),
            ("executeBefore", timestamp(2)),
        ]);
        let proposal = proposal_with_transfer(transfer);

        assert_err_contains(
            transfer_record_from_proposal(&proposal),
            "not a valid Daml decimal",
        );
    }

    #[test]
    fn transfer_record_from_proposal_missing_requested_at_errs() {
        // Everything present except `requestedAt`.
        let transfer = record(vec![
            ("sender", party(DEC_PARTY)),
            ("receiver", party(DSO)),
            ("amount", numeric("1.0")),
            (
                "instrumentId",
                record(vec![("admin", party(DSO)), ("id", text("CBTC"))]),
            ),
            ("executeBefore", timestamp(2)),
        ]);
        let proposal = proposal_with_transfer(transfer);

        assert_err_contains(transfer_record_from_proposal(&proposal), "requestedAt");
    }

    #[test]
    fn transfer_record_from_proposal_missing_execute_before_errs() {
        // Everything present except `executeBefore`.
        let transfer = record(vec![
            ("sender", party(DEC_PARTY)),
            ("receiver", party(DSO)),
            ("amount", numeric("1.0")),
            (
                "instrumentId",
                record(vec![("admin", party(DSO)), ("id", text("CBTC"))]),
            ),
            ("requestedAt", timestamp(1)),
        ]);
        let proposal = proposal_with_transfer(transfer);

        assert_err_contains(transfer_record_from_proposal(&proposal), "executeBefore");
    }

    #[test]
    fn to_proto_disclosed_contracts_decodes_valid_blob() -> Result {
        let raw = b"hello-disclosed-blob";
        let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
        let input = vec![RegistryDisclosedContract {
            template_id: None,
            contract_id: "00contractcid".to_string(),
            created_event_blob: encoded,
            synchronizer_id: "sync::1220beef".to_string(),
        }];

        let out = to_proto_disclosed_contracts(&input)?;

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].created_event_blob, raw.to_vec());
        assert_eq!(out[0].contract_id, "00contractcid");
        assert_eq!(out[0].synchronizer_id, "sync::1220beef");

        Ok(())
    }

    #[test]
    fn to_proto_disclosed_contracts_rejects_invalid_base64() {
        let input = vec![RegistryDisclosedContract {
            template_id: None,
            contract_id: "00contractcid".to_string(),
            created_event_blob: "!!!".to_string(),
            synchronizer_id: "sync::1220beef".to_string(),
        }];

        assert_err_contains(to_proto_disclosed_contracts(&input), "Invalid base64");
    }

    #[test]
    fn micros_to_rfc3339_rejects_out_of_range() {
        assert_err_contains(micros_to_rfc3339(i64::MAX), "out of range");
    }

    #[test]
    fn micros_to_rfc3339_formats_normal_value() -> Result {
        let s = micros_to_rfc3339(1_700_000_000_000_000)?;

        // 1_700_000_000 s since epoch is 2023-11-14T…Z; assert it both parses
        // back as RFC3339 and carries the expected date prefix.
        assert!(
            DateTime::parse_from_rfc3339(&s).is_ok(),
            "not parseable as RFC3339: {s}"
        );
        assert!(s.starts_with("2023-11-14"), "unexpected timestamp: {s}");

        Ok(())
    }

    #[test]
    fn self_administered_utility_token_needs_registry_context() {
        // The dec party administers the instrument itself, so the UI prefills
        // the on-ACS factory cid — yet the context still must be fetched.
        assert!(needs_registry_context(
            "00factory_cid_from_acs",
            DEC_PARTY,
            DEC_PARTY,
        ));
    }

    #[test]
    fn shared_instrument_with_empty_factory_needs_registry_context() {
        // CBTC-style: the factory lives on the registrar, so the UI leaves the
        // cid empty and the registrar resolves both the cid and the context.
        assert!(needs_registry_context(
            "",
            "cbtc-network::1220aa",
            DEC_PARTY
        ));
    }

    #[test]
    fn canton_coin_does_not_need_registry_context() {
        // CC surfaces a synthetic factory: AmuletRules cid + DSO admin. Its
        // context comes from the DSO scan API, not this registry.
        assert!(!needs_registry_context(
            "00amulet_rules_cid",
            DSO,
            DEC_PARTY,
        ));
    }
}
