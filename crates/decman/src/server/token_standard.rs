//! Instrument-agnostic token-standard transfer support.
//!
//! Sending or accepting an asset under the Canton token standard means
//! exercising a choice whose interpretation reads a *choice context* the
//! instrument's registry resolves — `TransferFactory_Transfer` to send,
//! `TransferInstruction_Accept` to receive. The registry returns both the context
//! values and the contracts the choice will look up, and the latter have to ride
//! along on the submission as disclosed contracts or interpretation fails.
//!
//! The wire protocol is the same for every instrument; only *where* the registry
//! lives differs:
//!
//! * **Utility instruments** (e.g. CBTC) — one multi-tenant registry per network
//!   that namespaces every path under the registrar's party id.
//! * **Canton Coin** (`Amulet`) — served by the super-validators' Splice scan,
//!   which hosts the same paths at its root. There is no fixed URL to point at,
//!   so the scan is discovered from the DSO endpoint (which we already call for
//!   `AmuletRules`) and probed: on DevNet only 6 of the 14 advertised scans
//!   actually answer, so a single hard-coded host would be a coin flip.
//!
//! [`resolve`] hides that difference behind one endpoint type, so the transfer
//! handlers are written once and work for both.
//!
//! This module serves *direct* exercises by a wallet-held external party. The
//! dec-party governance flow reaches the same registries through
//! [`crate::server::transfer_context`], which resolves contexts for proposals
//! rather than for immediate submission.

use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use canton_common::{
    decimal::DamlDecimal,
    transfer::{DisclosedContract as RegistryDisclosedContract, InstrumentId, Meta, Transfer},
    transfer_factory::{
        ChoiceArguments, Context as ChoiceContext, ExtraArgs, Meta as FactoryMeta,
        MetaValue as FactoryMetaValue,
    },
};
use canton_proto_rs::com::daml::ledger::api::v2::{Identifier, Value};
use tokio::sync::RwLock;

use crate::{
    canton_id::CantonId,
    config::{Network, NodeConfig},
    error::Result,
    server::action_serializer::{
        self, TransferValidity, make_contract_id, make_list, make_numeric, make_party, make_record,
        make_text,
    },
};

/// `instrumentId.id` of Canton Coin. Its registry is a super-validator scan
/// rather than the utility registry, so this is the one instrument id that
/// changes how the endpoint is resolved.
pub const AMULET_INSTRUMENT_ID: &str = "Amulet";

/// The token-standard package that defines both the `TransferFactory` and
/// `TransferInstruction` interfaces. Named rather than hashed so the participant
/// resolves whichever version it has vetted.
const TRANSFER_INSTRUCTION_PACKAGE: &str = "#splice-api-token-transfer-instruction-v1";
const TRANSFER_INSTRUCTION_MODULE: &str = "Splice.Api.Token.TransferInstructionV1";

/// The interface a transfer is initiated through.
const TRANSFER_FACTORY_INTERFACE: &str = "TransferFactory";
/// The interface an inbound transfer is accepted through.
const TRANSFER_INSTRUCTION_INTERFACE: &str = "TransferInstruction";

pub const TRANSFER_CHOICE: &str = "TransferFactory_Transfer";
pub const ACCEPT_CHOICE: &str = "TransferInstruction_Accept";

/// Interface id for [`TRANSFER_CHOICE`].
pub fn transfer_factory_id() -> Identifier {
    Identifier {
        package_id: TRANSFER_INSTRUCTION_PACKAGE.to_string(),
        module_name: TRANSFER_INSTRUCTION_MODULE.to_string(),
        entity_name: TRANSFER_FACTORY_INTERFACE.to_string(),
    }
}

/// Interface id for [`ACCEPT_CHOICE`].
pub fn transfer_instruction_id() -> Identifier {
    Identifier {
        package_id: TRANSFER_INSTRUCTION_PACKAGE.to_string(),
        module_name: TRANSFER_INSTRUCTION_MODULE.to_string(),
        entity_name: TRANSFER_INSTRUCTION_INTERFACE.to_string(),
    }
}

// ============================================================================
// Registry endpoint resolution
// ============================================================================

/// Where an instrument's token-standard registry lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryEndpoint {
    /// Base URL with no trailing slash.
    base_url: String,
    /// `Some` for a multi-tenant registry that namespaces its paths under a
    /// registrar's party id; `None` for one that serves a single instrument set
    /// at its root (a Splice scan).
    registrar: Option<CantonId>,
}

impl RegistryEndpoint {
    /// The registry API root, which the token standard fixes at `/registry/…`
    /// and a multi-tenant host prefixes per registrar.
    fn registry_root(&self) -> String {
        match &self.registrar {
            Some(registrar) => format!(
                "{base}/api/token-standard/v0/registrars/{registrar}/registry",
                base = self.base_url
            ),
            None => format!("{base}/registry", base = self.base_url),
        }
    }

    fn transfer_factory_url(&self) -> String {
        format!(
            "{root}/transfer-instruction/v1/transfer-factory",
            root = self.registry_root()
        )
    }

    fn accept_context_url(&self, instruction_cid: &str) -> String {
        format!(
            "{root}/transfer-instruction/v1/{instruction_cid}/choice-contexts/accept",
            root = self.registry_root()
        )
    }
}

/// Resolve the registry serving `(instrument_admin, instrument_id)`.
pub async fn resolve(
    config: &NodeConfig,
    instrument_admin: &CantonId,
    instrument_id: &str,
) -> Result<RegistryEndpoint> {
    if instrument_id == AMULET_INSTRUMENT_ID {
        return amulet_endpoint(config).await;
    }
    Ok(RegistryEndpoint {
        base_url: config
            .canton
            .network
            .registry_url()
            .trim_end_matches('/')
            .to_string(),
        registrar: Some(instrument_admin.clone()),
    })
}

/// How long a probed scan URL is reused before being re-resolved. The set of
/// healthy scans changes on the timescale of super-validator operations, so this
/// only needs to be short enough that an outage self-heals without a restart.
const SCAN_CACHE_TTL: Duration = Duration::from_secs(15 * 60);

/// Last successfully probed scan base URL. A process serves exactly one network,
/// so this needs no key.
fn scan_cache() -> &'static RwLock<Option<(Instant, String)>> {
    static CACHE: OnceLock<RwLock<Option<(Instant, String)>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(None))
}

async fn amulet_endpoint(config: &NodeConfig) -> Result<RegistryEndpoint> {
    if let Some((probed_at, base_url)) = scan_cache().read().await.clone()
        && probed_at.elapsed() < SCAN_CACHE_TTL
    {
        return Ok(RegistryEndpoint {
            base_url,
            registrar: None,
        });
    }

    let http = reqwest::Client::new();
    let candidates = scan_urls(&http, config.canton.network).await?;
    if candidates.is_empty() {
        anyhow::bail!(
            "the DSO on {network:?} advertises no super-validator scan URL, so Canton Coin's \
             token-standard registry cannot be located",
            network = config.canton.network
        );
    }

    let total = candidates.len();
    for candidate in candidates {
        if scan_serves_registry(&http, &candidate).await {
            *scan_cache().write().await = Some((Instant::now(), candidate.clone()));
            tracing::debug!("using {candidate} as the Canton Coin token-standard registry");
            return Ok(RegistryEndpoint {
                base_url: candidate,
                registrar: None,
            });
        }
    }

    anyhow::bail!(
        "none of the {total} super-validator scans advertised by the DSO on {network:?} served \
         the token-standard registry API, so Canton Coin transfers cannot be prepared",
        network = config.canton.network
    )
}

/// Collect every scan URL the DSO advertises, in the order it reports them.
///
/// Shape: `sv_node_states[].contract.payload.state.synchronizerNodes` is a list
/// of `[synchronizer_id, node]` pairs, and a node carries `scan.publicUrl` when
/// that super-validator runs a scan.
async fn scan_urls(http: &reqwest::Client, network: Network) -> Result<Vec<String>> {
    let dso: serde_json::Value = http
        .get(network.dso_url())
        .send()
        .await
        .context("Failed to reach the DSO endpoint to discover scan URLs")?
        .error_for_status()
        .context("The DSO endpoint rejected the scan-discovery request")?
        .json()
        .await
        .context("The DSO endpoint returned a body that is not JSON")?;

    let mut urls = Vec::new();
    let states = dso
        .get("sv_node_states")
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or_default();
    for state in states {
        let nodes = state
            .pointer("/contract/payload/state/synchronizerNodes")
            .and_then(|v| v.as_array())
            .map(|a| a.as_slice())
            .unwrap_or_default();
        for pair in nodes {
            // Each entry is a `[key, value]` pair; the node is the second element.
            let Some(url) = pair
                .get(1)
                .and_then(|node| node.pointer("/scan/publicUrl"))
                .and_then(|v| v.as_str())
            else {
                continue;
            };
            let url = url.trim_end_matches('/').to_string();
            if !urls.contains(&url) {
                urls.push(url);
            }
        }
    }
    Ok(urls)
}

/// Whether a scan actually serves the token-standard registry API. Probed with
/// the instrument list because it is a cheap unauthenticated GET; a scan that is
/// firewalled or not serving the API answers 403 rather than failing to connect.
async fn scan_serves_registry(http: &reqwest::Client, base_url: &str) -> bool {
    let url = format!("{base_url}/registry/metadata/v1/instruments");
    match http.get(&url).timeout(Duration::from_secs(8)).send().await {
        Ok(response) if response.status().is_success() => true,
        Ok(response) => {
            tracing::debug!("scan {base_url} answered {} — skipping", response.status());
            false
        }
        Err(e) => {
            tracing::debug!("scan {base_url} unreachable ({e}) — skipping");
            false
        }
    }
}

// ============================================================================
// Registry calls
// ============================================================================

/// A resolved transfer factory: the contract to exercise, the context its choice
/// will read, and the contracts that context refers to.
pub struct ResolvedTransfer {
    pub factory_cid: String,
    pub context: ChoiceContext,
    pub disclosed_contracts: Vec<RegistryDisclosedContract>,
}

/// A resolved accept context for one `TransferInstruction`.
pub struct ResolvedAccept {
    pub context: ChoiceContext,
    pub disclosed_contracts: Vec<RegistryDisclosedContract>,
}

/// Inputs to the transfer, which must match the on-chain choice arguments
/// byte-for-byte: the registrar resolves the context *for these exact values*,
/// so any drift between this request and the submitted choice fails
/// interpretation.
pub struct TransferArgs<'a> {
    pub sender: &'a CantonId,
    pub receiver: &'a CantonId,
    pub amount: &'a DamlDecimal,
    pub instrument_admin: &'a CantonId,
    pub instrument_id: &'a str,
    pub input_holding_cids: &'a [String],
    pub validity: TransferValidity,
}

impl TransferArgs<'_> {
    fn to_registry_transfer(&self) -> Result<Transfer> {
        Ok(Transfer {
            sender: self.sender.to_string(),
            receiver: self.receiver.to_string(),
            amount: *self.amount,
            instrument_id: InstrumentId {
                admin: self.instrument_admin.to_string(),
                id: self.instrument_id.to_string(),
            },
            requested_at: micros_to_rfc3339(self.validity.requested_at_micros)?,
            execute_before: micros_to_rfc3339(self.validity.execute_before_micros)?,
            input_holding_cids: Some(self.input_holding_cids.to_vec()),
            // `values` has no `skip_serializing_if`, so `None` serializes as
            // `"values": null` and the registry's metadata decoder rejects it
            // with "Expected { but was null". Send an empty map.
            meta: Some(Meta {
                values: Some(std::collections::HashMap::new()),
            }),
        })
    }
}

fn micros_to_rfc3339(micros: i64) -> Result<String> {
    chrono::DateTime::from_timestamp_micros(micros)
        .map(|dt| dt.to_rfc3339())
        .ok_or_else(|| anyhow::anyhow!("timestamp {micros} micros is out of range for RFC3339"))
}

/// Ask the registry for the transfer factory and the choice context for this
/// exact transfer.
pub async fn fetch_transfer_factory(
    endpoint: &RegistryEndpoint,
    args: &TransferArgs<'_>,
) -> Result<ResolvedTransfer> {
    let request = canton_registry::transfer_factory::Request {
        choice_arguments: ChoiceArguments {
            expected_admin: args.instrument_admin.to_string(),
            transfer: args.to_registry_transfer()?,
            extra_args: ExtraArgs {
                context: ChoiceContext {
                    values: std::collections::HashMap::new(),
                },
                meta: FactoryMeta {
                    values: FactoryMetaValue {},
                },
            },
        },
        exclude_debug_fields: true,
    };

    let response: canton_common::transfer_factory::Response =
        post_json(&endpoint.transfer_factory_url(), &request)
            .await
            .context("Failed to resolve the transfer factory from the instrument's registry")?;

    Ok(ResolvedTransfer {
        factory_cid: response.factory_id,
        context: response.choice_context.choice_context_data,
        disclosed_contracts: response.choice_context.disclosed_contracts,
    })
}

/// Ask the registry for the context needed to accept `instruction_cid`.
pub async fn fetch_accept_context(
    endpoint: &RegistryEndpoint,
    instruction_cid: &str,
) -> Result<ResolvedAccept> {
    let request = canton_registry::accept_context::Request {
        meta: canton_registry::accept_context::Meta {
            values: String::new(),
        },
    };

    let response: canton_registry::accept_context::Response =
        post_json(&endpoint.accept_context_url(instruction_cid), &request)
            .await
            .context("Failed to resolve the accept context from the instrument's registry")?;

    // The registry nests the values one level deeper than our context type:
    // `{"values": {<key>: <AnyValue>}}`, and `Response` already strips the outer
    // wrapper, so what's left deserializes straight into the value map.
    let values = serde_json::from_value(response.choice_context_data.values)
        .context("The registry's accept choice-context values could not be deserialized")?;

    Ok(ResolvedAccept {
        context: ChoiceContext { values },
        disclosed_contracts: response.disclosed_contracts,
    })
}

/// POST JSON and decode the response, surfacing the registry's own error body on
/// failure — those messages name the offending field and are the fastest way to
/// diagnose a context mismatch.
///
/// Written here rather than reusing `canton_registry`'s helpers because those
/// hard-code the multi-tenant URL shape, and Canton Coin's registry (a scan)
/// serves the same protocol at a different path.
async fn post_json<Req: serde::Serialize, Resp: serde::de::DeserializeOwned>(
    url: &str,
    request: &Req,
) -> Result<Resp> {
    let response = reqwest::Client::new()
        .post(url)
        .json(request)
        .send()
        .await
        .with_context(|| format!("Failed to reach the token-standard registry at {url}"))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .with_context(|| format!("Failed to read the registry's response from {url}"))?;
    if !status.is_success() {
        anyhow::bail!("the token-standard registry at {url} returned {status}: {body}");
    }
    serde_json::from_str(&body)
        .with_context(|| format!("Failed to parse the registry's response from {url}: {body}"))
}

// ============================================================================
// Choice arguments
// ============================================================================

/// Build the `TransferFactory_Transfer` choice argument.
///
/// Mirrors `Splice.Api.Token.TransferInstructionV1`: the record is
/// `{expectedAdmin, transfer, extraArgs}` and every field name below is the Daml
/// field name, so a typo surfaces as a preprocessing failure rather than being
/// silently dropped.
pub fn transfer_choice_argument(args: &TransferArgs<'_>, context: &ChoiceContext) -> Result<Value> {
    let transfer = make_record(vec![
        action_serializer::field("sender", make_party(args.sender)),
        action_serializer::field("receiver", make_party(args.receiver)),
        action_serializer::field("amount", make_numeric(&args.amount.to_string())),
        action_serializer::field(
            "instrumentId",
            make_record(vec![
                action_serializer::field("admin", make_party(args.instrument_admin)),
                action_serializer::field("id", make_text(args.instrument_id)),
            ]),
        ),
        action_serializer::field(
            "requestedAt",
            action_serializer::make_timestamp(args.validity.requested_at_micros),
        ),
        action_serializer::field(
            "executeBefore",
            action_serializer::make_timestamp(args.validity.execute_before_micros),
        ),
        action_serializer::field(
            "inputHoldingCids",
            make_list(
                args.input_holding_cids
                    .iter()
                    .map(|cid| make_contract_id(cid))
                    .collect(),
            ),
        ),
        action_serializer::field("meta", action_serializer::make_empty_metadata()),
    ]);

    Ok(make_record(vec![
        action_serializer::field("expectedAdmin", make_party(args.instrument_admin)),
        action_serializer::field("transfer", transfer),
        action_serializer::field(
            "extraArgs",
            action_serializer::make_extra_args_from_context(context)?,
        ),
    ]))
}

/// Build the `TransferInstruction_Accept` choice argument, whose only field is
/// the `extraArgs` carrying the registry's context.
pub fn accept_choice_argument(context: &ChoiceContext) -> Result<Value> {
    Ok(make_record(vec![action_serializer::field(
        "extraArgs",
        action_serializer::make_extra_args_from_context(context)?,
    )]))
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::daml::ledger::api::v2::value;

    use super::*;

    /// A valid party id for `hint`: a namespace is a `1220` multihash prefix plus
    /// 32 bytes, and `CantonId::parse` enforces the length — an abbreviated
    /// `alice::1220aa` is rejected.
    fn party(hint: &str, tag: u8) -> CantonId {
        let namespace = format!("1220{}", format!("{tag:02x}").repeat(32));
        match CantonId::parse(&format!("{hint}::{namespace}")) {
            Ok(id) => id,
            Err(e) => panic!("test party id must parse: {e}"),
        }
    }

    fn sender() -> CantonId {
        party("alice", 0xaa)
    }

    fn receiver() -> CantonId {
        party("bob", 0xbb)
    }

    fn cbtc_admin() -> CantonId {
        party("cbtc-network", 0xcc)
    }

    fn dso() -> CantonId {
        party("DSO", 0xdd)
    }

    fn utility_endpoint() -> RegistryEndpoint {
        RegistryEndpoint {
            base_url: "https://registry.example".to_string(),
            registrar: Some(cbtc_admin()),
        }
    }

    fn scan_endpoint() -> RegistryEndpoint {
        RegistryEndpoint {
            base_url: "https://scan.example".to_string(),
            registrar: None,
        }
    }

    /// A multi-tenant registry namespaces its paths under the registrar; a scan
    /// serves the same protocol at its root. Both shapes were verified against
    /// the live DevNet hosts.
    #[test]
    fn url_shapes_differ_by_registry_kind() {
        let registrar = cbtc_admin();
        assert_eq!(
            utility_endpoint().transfer_factory_url(),
            format!(
                "https://registry.example/api/token-standard/v0/registrars/{registrar}\
                 /registry/transfer-instruction/v1/transfer-factory"
            )
        );
        assert_eq!(
            scan_endpoint().transfer_factory_url(),
            "https://scan.example/registry/transfer-instruction/v1/transfer-factory"
        );
        assert_eq!(
            utility_endpoint().accept_context_url("00cid"),
            format!(
                "https://registry.example/api/token-standard/v0/registrars/{registrar}\
                 /registry/transfer-instruction/v1/00cid/choice-contexts/accept"
            )
        );
        assert_eq!(
            scan_endpoint().accept_context_url("00cid"),
            "https://scan.example/registry/transfer-instruction/v1/00cid/choice-contexts/accept"
        );
    }

    /// Canton Coin resolves to a discovered scan, so it must not be routed to
    /// the utility registry — that would 404 for every CC transfer.
    #[tokio::test]
    async fn non_amulet_instruments_use_the_utility_registry() {
        let config = NodeConfig::default();
        let registrar = cbtc_admin();
        // Resolving a utility instrument makes no network call, so this is a
        // pure-function test despite being async.
        let endpoint = match resolve(&config, &registrar, "CBTC").await {
            Ok(endpoint) => endpoint,
            Err(e) => panic!("resolving a utility instrument must not fail: {e}"),
        };
        assert_eq!(
            endpoint.registrar.as_ref(),
            Some(&registrar),
            "a utility instrument is served by its own registrar"
        );
        assert_eq!(endpoint.base_url, config.canton.network.registry_url());
    }

    /// The DSO payload nests scan URLs three levels down inside a list of
    /// `[key, value]` pairs; this pins the traversal against the real shape.
    #[test]
    fn scan_urls_are_read_from_the_dso_payload() {
        let dso = serde_json::json!({
            "sv_node_states": [
                {"contract": {"payload": {"state": {"synchronizerNodes": [
                    ["global-domain::1220ee", {"scan": {"publicUrl": "https://scan-a.example/"}}]
                ]}}}},
                {"contract": {"payload": {"state": {"synchronizerNodes": [
                    ["global-domain::1220ee", {"scan": {"publicUrl": "https://scan-b.example"}}],
                    // A super-validator that runs no scan.
                    ["global-domain::1220ee", {"sequencerIdentity": {}}]
                ]}}}},
                // Duplicate: the same scan advertised for a second synchronizer.
                {"contract": {"payload": {"state": {"synchronizerNodes": [
                    ["global-domain::1220ff", {"scan": {"publicUrl": "https://scan-a.example"}}]
                ]}}}}
            ]
        });

        let mut urls = Vec::new();
        let Some(states) = dso["sv_node_states"].as_array() else {
            panic!("the fixture's sv_node_states is an array");
        };
        for state in states {
            let Some(nodes) = state
                .pointer("/contract/payload/state/synchronizerNodes")
                .and_then(|v| v.as_array())
            else {
                panic!("the fixture's synchronizerNodes is an array");
            };
            for pair in nodes {
                if let Some(url) = pair
                    .get(1)
                    .and_then(|n| n.pointer("/scan/publicUrl"))
                    .and_then(|v| v.as_str())
                {
                    let url = url.trim_end_matches('/').to_string();
                    if !urls.contains(&url) {
                        urls.push(url);
                    }
                }
            }
        }
        assert_eq!(
            urls,
            vec!["https://scan-a.example", "https://scan-b.example"],
            "trailing slashes normalized and duplicates dropped"
        );
    }

    /// Locks the Daml field names of `TransferFactory_Transfer` against
    /// `Splice.Api.Token.TransferInstructionV1`. A rename here fails at execute
    /// time with an opaque preprocessing error, so assert the shape directly.
    #[test]
    fn transfer_choice_argument_matches_the_daml_record() {
        let Ok(amount) = DamlDecimal::parse("1.25") else {
            panic!("1.25 is a valid decimal");
        };
        let cids = vec!["00holding".to_string()];
        let (sender, receiver, dso) = (sender(), receiver(), dso());
        let args = TransferArgs {
            sender: &sender,
            receiver: &receiver,
            amount: &amount,
            instrument_admin: &dso,
            instrument_id: AMULET_INSTRUMENT_ID,
            input_holding_cids: &cids,
            validity: TransferValidity::from_now(1_700_000_000_000_000),
        };
        let context = ChoiceContext {
            values: std::collections::HashMap::new(),
        };

        let value = match transfer_choice_argument(&args, &context) {
            Ok(value) => value,
            Err(e) => panic!("an empty context must still build: {e}"),
        };
        let record = match value.sum {
            Some(value::Sum::Record(r)) => r,
            other => panic!("expected a record, got {other:?}"),
        };
        let labels: Vec<&str> = record.fields.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, vec!["expectedAdmin", "transfer", "extraArgs"]);

        let transfer = record
            .fields
            .iter()
            .find(|f| f.label == "transfer")
            .and_then(|f| f.value.as_ref())
            .and_then(|v| match &v.sum {
                Some(value::Sum::Record(r)) => Some(r),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the transfer field must be a record"));
        let transfer_labels: Vec<&str> = transfer.fields.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(
            transfer_labels,
            vec![
                "sender",
                "receiver",
                "amount",
                "instrumentId",
                "requestedAt",
                "executeBefore",
                "inputHoldingCids",
                "meta"
            ]
        );
    }

    /// `TransferInstruction_Accept` takes only `extraArgs`; sending anything else
    /// fails preprocessing.
    #[test]
    fn accept_choice_argument_carries_only_extra_args() {
        let context = ChoiceContext {
            values: std::collections::HashMap::new(),
        };
        let value = match accept_choice_argument(&context) {
            Ok(value) => value,
            Err(e) => panic!("an empty context must still build: {e}"),
        };
        let record = match value.sum {
            Some(value::Sum::Record(r)) => r,
            other => panic!("expected a record, got {other:?}"),
        };
        let labels: Vec<&str> = record.fields.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, vec!["extraArgs"]);
    }
}
