//! Decentralizing an existing **local** party, end to end, the way a partner
//! runs it: convert, add a host, replicate, then transact.
//!
//! Two sibling phases already cover the halves.
//! [`local_party_adopt_endpoints`](super::local_party_adopt_endpoints) converts
//! a local party and stops there.
//! [`external_party_add_hosts`](super::external_party_add_hosts) adds a host,
//! but to a party that was born external at serial 1. Nothing joined them, and
//! the join is the whole operator procedure — so the two questions a partner
//! integration would ask first had no answer in CI:
//!
//! 1. **Does a converted party accept a second host?** Its namespace is still
//!    the source participant's root key, not the adopted signing key, so
//!    `add-hosts` authorizes against one key and signs with another. A party
//!    born external has the two collapsed into one and never exercises that.
//! 2. **Does a converted party transact?** Canton accepting
//!    `party_signing_keys` is proven. The party then submitting with that key
//!    is a different runtime path, and it is the one the partner's application
//!    depends on after its cutover to interactive submission.
//!
//! This phase answers both against real Canton, and it answers the second one
//! twice: once on the original host, and once through the host that joined,
//! which is the failover the whole exercise is for.
//!
//! **A failure here is a result, not a flake.** If the conversion and the host
//! add do not compose, the operator procedure in
//! `docs/DECENTRALIZING_AN_EXISTING_PARTY.md` is wrong and we would rather find
//! that in CI than on a call with a partner.
//!
//! The party holds contracts before it is converted, because an empty ACS is
//! the case that proves nothing: the relay has to carry real state, and the
//! joiner has to validate it against a package it vetted. `orphan-marker` is
//! the leaf DAR for that — its one template is signed by a single party, so the
//! test party can create instances alone, first as a local party through an
//! ordinary submission and afterwards as a converted one through interactive
//! submission.
//!
//! P2 joins rather than P3. P3 must stay without `orphan-marker` for
//! [`add_party_missing_dar`](super::add_party_missing_dar) to reproduce its
//! failure, and a joiner here needs that DAR vetted before the import.

use std::{path::Path, time::Duration};

use anyhow::Context;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        Command, CreateCommand, Identifier, Record, RecordField, Signature, Transaction,
        Value as LedgerValue, command, event,
        interactive::{
            ExecuteSubmissionAndWaitForTransactionRequest, PartySignatures,
            PrepareSubmissionRequest, SinglePartySignatures,
        },
        value,
    },
    digitalasset::canton::{
        crypto::v30::{SignatureFormat, SigningAlgorithmSpec},
        protocol::v30::{
            PartyToParticipant, TopologyMapping,
            enums::{ParticipantPermission, TopologyChangeOp},
            party_to_participant::HostingParticipant,
            topology_mapping,
        },
        topology::admin::v30::{AuthorizeRequest, authorize_request},
    },
};
use serde_json::{Value, json};
use tracing::info;
use uuid::Uuid;

use dec_party_manager::{
    canton_id::CantonId,
    config::NodeConfig,
    utils,
    workflow::{
        external_party::add_hosts::read_party_to_participant,
        topology::{authorize_with_topology_retry, synchronizer_store_id},
    },
};
use decman_wallet::ExternalKeyPair;

use crate::common::{
    Fixture,
    chaos::fresh_prefix,
    http::probe_workflow_status,
    ledger_api::{P1_JSON_API, P2_JSON_API},
    phases::deploy_gov_core::grant_rights,
    scenario::Scenario,
};

/// The leaf DAR whose one template a single party signs alone.
const ORPHAN_DAR: &str = "orphan-marker-0.1.0.dar";

/// Addressed by package NAME so Canton resolves the vetted version.
const ORPHAN_PACKAGE: &str = "#orphan-marker";
const ORPHAN_MODULE: &str = "OrphanMarker";
const ORPHAN_TEMPLATE: &str = "#orphan-marker:OrphanMarker:OrphanMarker";

/// The localnet ledger user the harness submits as.
const LEDGER_USER: &str = "ledger-api-user";

/// Contracts seeded before the conversion, so the relay carries real state.
const SEEDED_MARKERS: i64 = 5;

/// A `NodeConfig` pointed at one participant's Canton, admin and ledger both.
///
/// The phase drives Canton directly for the two things no DecMan endpoint
/// exposes: allocating the local party the partner already has, and submitting
/// as the party once it holds a key.
fn node_config(
    participant_id: &str,
    admin_env: &str,
    ledger_env: &str,
) -> anyhow::Result<NodeConfig> {
    let mut config = NodeConfig::default();
    config.canton.admin_api_host = "127.0.0.1".to_string();
    config.canton.admin_api_port = read_port(admin_env)?;
    config.canton.ledger_api_host = "127.0.0.1".to_string();
    config.canton.ledger_api_port = read_port(ledger_env)?;
    config.node.participant_id = Some(CantonId::parse(participant_id)?);
    Ok(config)
}

fn read_port(var: &str) -> anyhow::Result<u16> {
    std::env::var(var)
        .with_context(|| format!("{var} not set"))?
        .parse()
        .with_context(|| format!("{var} is not a port"))
}

/// Allocate the party as a partner holds it today: one host at Submission,
/// threshold 1, no key of its own.
///
/// `Authorize` rather than prepare/sign, because a local party's namespace is
/// its participant's, so that participant authors the mapping alone.
async fn allocate_local_party(config: &NodeConfig, party_id: &str) -> anyhow::Result<()> {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    authorize_with_topology_retry(
        config,
        AuthorizeRequest {
            r#type: Some(authorize_request::Type::Proposal(
                authorize_request::Proposal {
                    change: TopologyChangeOp::AddReplace as i32,
                    serial: 0,
                    mapping: Some(authorize_request::proposal::Mapping::V30(TopologyMapping {
                        mapping: Some(topology_mapping::Mapping::PartyToParticipant(
                            PartyToParticipant {
                                party: party_id.to_string(),
                                threshold: 1,
                                participants: vec![HostingParticipant {
                                    participant_uid: config.participant_id().to_string(),
                                    permission: ParticipantPermission::Submission as i32,
                                    onboarding: None,
                                }],
                                party_signing_keys: None,
                            },
                        )),
                    })),
                },
            )),
            must_fully_authorize: true,
            force_changes: vec![],
            signed_by: vec![],
            store: Some(synchronizer_store_id(&synchronizer_id)),
            wait_to_become_effective: None,
        },
        "local-party decentralization",
    )
    .await?;
    Ok(())
}

/// Wait until the allocation is in head state and return the serial it sits at.
///
/// `Authorize` returning is not the write being effective, and the endpoints
/// refuse a stale pin, so the serial is read rather than assumed.
async fn wait_for_party(config: &NodeConfig, party_id: &str) -> anyhow::Result<u32> {
    for _ in 0..60 {
        if let Some(current) = read_party_to_participant(config, party_id).await? {
            return Ok(current.serial);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    anyhow::bail!("{party_id} never appeared in head state after allocation")
}

/// Create one `OrphanMarker` through an ordinary submission, which is what the
/// partner's application does while the party is still local and its host still
/// holds Submission.
async fn create_marker_as_local(f: &Fixture, party_id: &str, marker: i64) -> anyhow::Result<()> {
    let body = json!({
        "commands": [{
            "CreateCommand": {
                "templateId": ORPHAN_TEMPLATE,
                "createArguments": {
                    "governanceParty": party_id,
                    "marker": marker.to_string(),
                },
            }
        }],
        "commandId": format!("seed-marker-{marker}-{}", Uuid::new_v4()),
        "userId": LEDGER_USER,
        "actAs": [party_id],
        "readAs": [party_id],
    });
    f.submit_create(P1_JSON_API, &body)
        .await
        .with_context(|| format!("seeding OrphanMarker {marker} as the local party"))?;
    Ok(())
}

/// Submit as the **converted** party through interactive submission, and return
/// how many contracts the committed transaction created.
///
/// This is the path the partner's application moves to at the cutover: the node
/// prepares, the key holder signs the hash the node returns, and the node
/// executes. Nothing here holds the party's key but the caller.
async fn create_marker_as_converted(
    config: &NodeConfig,
    token: &str,
    party_id: &str,
    wallet: &ExternalKeyPair,
    marker: i64,
) -> anyhow::Result<usize> {
    let mut client = utils::create_submission_client(config, Some(token.to_string())).await?;

    let create = Command {
        command: Some(command::Command::Create(CreateCommand {
            template_id: Some(Identifier {
                package_id: ORPHAN_PACKAGE.to_string(),
                module_name: ORPHAN_MODULE.to_string(),
                entity_name: ORPHAN_MODULE.to_string(),
            }),
            create_arguments: Some(Record {
                record_id: None,
                fields: vec![
                    RecordField {
                        label: "governanceParty".to_string(),
                        value: Some(LedgerValue {
                            sum: Some(value::Sum::Party(party_id.to_string())),
                        }),
                    },
                    RecordField {
                        label: "marker".to_string(),
                        value: Some(LedgerValue {
                            sum: Some(value::Sum::Int64(marker)),
                        }),
                    },
                ],
            }),
        })),
    };

    let prepared = client
        .prepare_submission(tonic::Request::new(PrepareSubmissionRequest {
            user_id: LEDGER_USER.to_string(),
            command_id: format!("converted-marker-{marker}-{}", Uuid::new_v4()),
            commands: vec![create],
            min_ledger_time: None,
            max_record_time: None,
            act_as: vec![party_id.to_string()],
            read_as: vec![],
            disclosed_contracts: vec![],
            synchronizer_id: String::new(),
            package_id_selection_preference: vec![],
            verbose_hashing: false,
            prefetch_contract_keys: vec![],
            estimate_traffic_cost: None,
            hashing_scheme_version: None,
            taps_max_passes: None,
        }))
        .await
        .context("PrepareSubmission for the converted party")?
        .into_inner();

    // Ed25519 over the hash Canton computed. The wallet never re-derives it,
    // exactly as `decman-wallet` does for the topology writes.
    let signature = Signature {
        format: SignatureFormat::Concat as i32,
        signature: wallet.sign(&prepared.prepared_transaction_hash).to_vec(),
        signed_by: wallet.fingerprint(),
        signing_algorithm_spec: SigningAlgorithmSpec::Ed25519 as i32,
    };

    let response = client
        .execute_submission_and_wait_for_transaction(tonic::Request::new(
            ExecuteSubmissionAndWaitForTransactionRequest {
                prepared_transaction: prepared.prepared_transaction.clone(),
                party_signatures: Some(PartySignatures {
                    signatures: vec![SinglePartySignatures {
                        party: party_id.to_string(),
                        signatures: vec![signature],
                    }],
                }),
                deduplication_period: None,
                submission_id: Uuid::new_v4().to_string(),
                user_id: LEDGER_USER.to_string(),
                hashing_scheme_version: prepared.hashing_scheme_version,
                min_ledger_time: None,
                transaction_format: None,
            },
        ))
        .await
        .context("ExecuteSubmission for the converted party")?
        .into_inner();

    Ok(created_count(response.transaction.as_ref()))
}

fn created_count(transaction: Option<&Transaction>) -> usize {
    transaction
        .map(|tx| {
            tx.events
                .iter()
                .filter(|e| matches!(e.event, Some(event::Event::Created(_))))
                .count()
        })
        .unwrap_or_default()
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: local_party_decentralization");

    let p1_config = node_config(&f.p1.participant_id, "P1_CANTON_ADMIN", "P1_CANTON_LEDGER")?;
    let p2_config = node_config(&f.p2.participant_id, "P2_CANTON_ADMIN", "P2_CANTON_LEDGER")?;

    // A local party's namespace IS its participant's, which is what makes it
    // local and what the conversion leaves untouched.
    let p1 = CantonId::parse(&f.p1.participant_id)?;
    let hint = fresh_prefix("decentralize");
    let party_id = format!("{hint}::{ns}", ns = p1.namespace.to_hex());
    let wallet = ExternalKeyPair::generate();
    let seed = *wallet.seed();
    info!("Local party to decentralize: {party_id}");

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let dar_path = Path::new(manifest_dir)
        .join("../../releases/v1")
        .join(ORPHAN_DAR);
    let dar_bytes = tokio::fs::read(&dar_path)
        .await
        .with_context(|| format!("reading {path}", path = dar_path.display()))?;
    let upload_req = json!({
        "dar_files": [{ "filename": ORPHAN_DAR, "data": STANDARD.encode(&dar_bytes) }],
    });

    // ------------------------------------------------------------------
    // 1 — the party a partner already has: local, hosted once, with contracts.
    // ------------------------------------------------------------------
    Scenario::with_ctx(format!("local party {hint} exists and holds contracts"), ())
        .when("P1 and P2 vet orphan-marker, and P1 allocates the party", {
            let upload_req = upload_req.clone();
            let party_id = party_id.clone();
            let config = p1_config.clone();
            move |f, _| {
                let upload_req = upload_req.clone();
                let party_id = party_id.clone();
                let config = config.clone();
                Box::pin(async move {
                    // The joiner needs the package BEFORE the import, which is
                    // the precondition the runbook puts first: a missing DAR
                    // fails the import after the joiner has disconnected.
                    for (host, name) in [(f.p1.http, "P1"), (f.p2.http, "P2")] {
                        let _: Value = f
                            .post_json(host, "/dars/upload", &upload_req)
                            .await
                            .with_context(|| format!("upload orphan-marker to {name}"))?;
                    }

                    allocate_local_party(&config, &party_id)
                        .await
                        .context("allocating the local party")?;
                    wait_for_party(&config, &party_id)
                        .await
                        .context("waiting for the local party to reach head state")?;

                    // The submitting user needs the party before it can act for
                    // it. Granted on both participants now: user rights are
                    // participant-local and survive every topology change
                    // below, so P2 is ready to submit once it hosts the party.
                    grant_rights(&*f, P1_JSON_API, &party_id, "P1").await?;
                    grant_rights(&*f, P2_JSON_API, &party_id, "P2").await?;

                    for marker in 0..SEEDED_MARKERS {
                        create_marker_as_local(&*f, &party_id, marker).await?;
                    }
                    Ok(())
                })
            }
        })
        .then(
            "the party is local, hosted once, and submits for itself",
            Duration::from_secs(120),
            {
                let party_id = party_id.clone();
                let config = p1_config.clone();
                move |_f, _| {
                    let party_id = party_id.clone();
                    let config = config.clone();
                    Box::pin(async move {
                        let current =
                            read_party_to_participant(&config, &party_id).await.ok()??;
                        // No key, one host, and that host still submits. This
                        // is the shape the conversion has to change.
                        if current.mapping.party_signing_keys.is_some()
                            || current.mapping.participants.len() != 1
                            || current.mapping.participants[0].permission
                                != ParticipantPermission::Submission as i32
                        {
                            return None;
                        }
                        Some(Ok(()))
                    })
                }
            },
        )
        .run(f)
        .await?;

    // ------------------------------------------------------------------
    // 2 — convert it, then prove the converted party can still transact.
    // ------------------------------------------------------------------
    Scenario::with_ctx(
        format!("{hint} adopts a wallet key and submits with it"),
        (),
    )
    .when("the owner converts the party through /v0/tenant", {
        let party_id = party_id.clone();
        let public_key = wallet.public_key_b64();
        move |f, _| {
            let party_id = party_id.clone();
            let public_key = public_key.clone();
            let wallet = ExternalKeyPair::from_seed(seed);
            Box::pin(async move {
                let state: Value = f
                    .get_json(f.p1.http, &format!("/v0/tenant/{party_id}/state"))
                    .await?;
                let base_serial = state
                    .get("serial")
                    .and_then(Value::as_u64)
                    .context("party state missing serial")?;

                let prep: Value = f
                    .post_json(
                        f.p1.http,
                        "/v0/tenant/local-party/adopt-key/prepare",
                        &json!({
                            "party_id": party_id,
                            "public_key": public_key,
                            "base_serial": base_serial,
                        }),
                    )
                    .await?;
                let signatures = sign_prepared(&prep, seed)?;

                let _: Value = f
                    .post_json(
                        f.p1.http,
                        "/v0/tenant/local-party/adopt-key/onboard",
                        &json!({
                            "party_id": party_id,
                            "base_serial": base_serial,
                            "public_key": public_key,
                            "topology_transactions": prep
                                .get("topology_transactions")
                                .cloned()
                                .context("prepare response missing topology_transactions")?,
                            "signatures": signatures,
                            "signed_by": wallet.fingerprint(),
                        }),
                    )
                    .await?;
                Ok(())
            })
        }
    })
    .then(
        "the party carries the wallet key and its host only confirms",
        Duration::from_secs(120),
        {
            let party_id = party_id.clone();
            let config = p1_config.clone();
            let expected = ExternalKeyPair::from_seed(seed).public_key_bytes();
            move |_f, _| {
                let party_id = party_id.clone();
                let config = config.clone();
                Box::pin(async move {
                    let current = read_party_to_participant(&config, &party_id).await.ok()??;
                    let keys = current.mapping.party_signing_keys.as_ref()?;
                    let wanted =
                        dec_party_manager::workflow::external_party::steps::party_signing_key(
                            &expected,
                        );
                    let demoted = current
                        .mapping
                        .participants
                        .iter()
                        .all(|p| p.permission == ParticipantPermission::Confirmation as i32);
                    if !demoted
                        || keys.keys.len() != 1
                        || keys.keys[0].public_key != wanted.public_key
                    {
                        return None;
                    }
                    Some(Ok(()))
                })
            }
        },
    )
    // The open question the scoping study left: Canton takes the key, but
    // does the party then act with it? Nothing before this line proves the
    // partner's application still works after the cutover.
    .when("the converted party creates a contract it signs itself", {
        let party_id = party_id.clone();
        let config = p1_config.clone();
        move |f, _| {
            let party_id = party_id.clone();
            let config = config.clone();
            let wallet = ExternalKeyPair::from_seed(seed);
            Box::pin(async move {
                let token = f.refresher.token().await?;
                let created =
                    create_marker_as_converted(&config, &token, &party_id, &wallet, SEEDED_MARKERS)
                        .await
                        .context("the converted party submitting through its original host")?;
                anyhow::ensure!(
                    created == 1,
                    "the converted party's submission created {created} contract(s), not 1"
                );
                info!("converted party submitted through P1 with its own key");
                Ok(())
            })
        }
    })
    .run(f)
    .await?;

    // ------------------------------------------------------------------
    // 3 — the seam: a converted party gains a second host.
    // ------------------------------------------------------------------
    Scenario::with_ctx(format!("add P2 as a host of the converted {hint}"), ())
        .when("both nodes authorize the add with their own vault keys", {
            let party_id = party_id.clone();
            move |f, _| {
                let party_id = party_id.clone();
                Box::pin(async move {
                    let state: Value = f
                        .get_json(f.p1.http, &format!("/v0/tenant/{party_id}/state"))
                        .await?;
                    let base_serial = state
                        .get("serial")
                        .and_then(Value::as_u64)
                        .context("party state missing serial")?;

                    let request = json!({
                        "party_id": party_id,
                        "new_hosts": [&f.p2.participant_id],
                        "base_serial": base_serial,
                    });

                    // The comparison is the security property, and it has to
                    // hold for a party whose namespace is a participant key
                    // rather than its own signing key.
                    let mut prepared = Vec::new();
                    for host in [f.p1.http, f.p2.http] {
                        let prep: Value = f
                            .post_json(host, "/v0/tenant/add-hosts/prepare", &request)
                            .await?;
                        prepared.push(prep);
                    }
                    let first = prepared
                        .first()
                        .context("no host answered add-hosts/prepare")?;
                    for (index, other) in prepared.iter().enumerate().skip(1) {
                        anyhow::ensure!(
                            first.get("topology_transactions")
                                == other.get("topology_transactions"),
                            "host {index} prepared different bytes from host 0 — the wallet \
                             could not compare them:\n  host 0: {first:?}\n  host {index}: \
                             {other:?}"
                        );
                    }

                    // NOT the wallet-signed onboard. The party's namespace is
                    // still P1's participant key — adopting a signing key does
                    // not move it — so nothing the wallet holds can authorize
                    // this write. Each node signs its own half with its vault
                    // key instead, and the change stays a proposal until both
                    // have done so.
                    for (host, name) in [(f.p1.http, "P1"), (f.p2.http, "P2")] {
                        let _: Value = f
                            .post_json(host, "/v0/tenant/add-hosts/authorize", &request)
                            .await
                            .with_context(|| format!("add-hosts/authorize on {name}"))?;
                    }

                    // The wallet-signed path must still refuse this party, or
                    // the rule that only a party's namespace authorizes its
                    // topology has quietly stopped meaning anything.
                    let refused: anyhow::Result<Value> = f
                        .post_json(
                            f.p2.http,
                            "/v0/tenant/add-hosts/onboard",
                            &json!({
                                "party_id": party_id,
                                "base_serial": base_serial,
                                "topology_transactions": first
                                    .get("topology_transactions")
                                    .cloned()
                                    .context("prepare response missing topology_transactions")?,
                                "signatures": sign_prepared(first, seed)?,
                                "signed_by": ExternalKeyPair::from_seed(seed).fingerprint(),
                            }),
                        )
                        .await;
                    let Err(e) = refused else {
                        anyhow::bail!(
                            "a wallet signature must not authorize a party whose namespace \
                                 is a participant key"
                        );
                    };
                    let reason = format!("{e:#}");
                    anyhow::ensure!(
                        reason.contains("400") && reason.contains("namespace"),
                        "the refusal must name the namespace, got: {reason}"
                    );
                    Ok(())
                })
            }
        })
        .then("the party reports two hosts", Duration::from_secs(180), {
            let party_id = party_id.clone();
            move |f, _| {
                let party_id = party_id.clone();
                Box::pin(async move {
                    let state: Value = f
                        .probe_get_json(f.p1.http, &format!("/v0/tenant/{party_id}/state"))
                        .await?;
                    if state.get("host_count").and_then(Value::as_u64) != Some(2) {
                        return None;
                    }
                    Some(Ok(()))
                })
            }
        })
        .run(f)
        .await?;

    // ------------------------------------------------------------------
    // 4 — carry the contracts across and switch the new host on.
    // ------------------------------------------------------------------
    Scenario::with_ctx(format!("replicate {hint}'s ACS onto P2"), ())
        .when("the wallet relays the snapshot from P1 to P2", {
            let party_id = party_id.clone();
            move |f, _| {
                let party_id = party_id.clone();
                Box::pin(async move {
                    let target = f.p2.participant_id.clone();
                    let state: Value = f
                        .get_json(f.p1.http, &format!("/v0/tenant/{party_id}/state"))
                        .await?;
                    let current_serial = state
                        .get("serial")
                        .and_then(Value::as_u64)
                        .context("party state missing serial")?;
                    // The staged replication is keyed by the serial the add was
                    // pinned to, and the add advanced exactly one.
                    let base_serial = current_serial
                        .checked_sub(1)
                        .context("the add-hosts write should have advanced the serial")?;

                    let mut seq: u64 = 1;
                    let result = loop {
                        anyhow::ensure!(seq < 512, "the relay did not converge");
                        let block: Value = f
                            .get_json(
                                f.p1.http,
                                &format!(
                                    "/v0/tenant/{party_id}/acs/{target}\
                                     ?base_serial={base_serial}&seq={seq}"
                                ),
                            )
                            .await?;
                        anyhow::ensure!(
                            block.get("seq").and_then(Value::as_u64) == Some(seq),
                            "the source served a block other than {seq}: {block}"
                        );
                        let end = block.get("end").and_then(Value::as_bool) == Some(true);

                        let progress: Value = f
                            .post_json(
                                f.p2.http,
                                "/v0/tenant/add-hosts/import",
                                &json!({
                                    "party_id": party_id,
                                    "base_serial": base_serial,
                                    "seq": seq,
                                    "chunk": block
                                        .get("chunk")
                                        .cloned()
                                        .context("acs block missing chunk")?,
                                    "end": end,
                                    "total_len": block
                                        .get("total_len")
                                        .cloned()
                                        .unwrap_or(json!(0)),
                                    "sha256": block
                                        .get("sha256")
                                        .cloned()
                                        .unwrap_or(json!("")),
                                    "package_ids": block
                                        .get("package_ids")
                                        .cloned()
                                        .context("acs block missing package_ids")?,
                                }),
                            )
                            .await?;

                        if progress.get("complete").and_then(Value::as_bool) == Some(true) {
                            anyhow::ensure!(
                                end,
                                "the joiner completed on a block that was not the end: \
                                 {progress}"
                            );
                            break progress;
                        }
                        anyhow::ensure!(
                            !end,
                            "the joiner took the final block without completing: {progress}"
                        );
                        seq += 1;
                    };
                    info!("add-hosts import on P2 after {seq} block(s): {result}");

                    // Checked inside the importing step: a later Then would only
                    // see P1 once the relay finished, and P1 dropping out during
                    // the import is exactly the regression worth catching.
                    let p1_status: Value = f
                        .get_json(f.p1.http, &format!("/v0/tenant/{party_id}/status"))
                        .await
                        .context("P1's view of the party right after the import")?;
                    anyhow::ensure!(
                        p1_status.get("status").and_then(Value::as_str) == Some("completed"),
                        "P1 stopped reporting the party live across the import: {p1_status}"
                    );
                    Ok(())
                })
            }
        })
        .then(
            "P2 hosts the party with the onboarding marker cleared",
            Duration::from_secs(180),
            {
                let party_id = party_id.clone();
                move |f, _| {
                    let party_id = party_id.clone();
                    Box::pin(async move {
                        probe_workflow_status(
                            &*f,
                            f.p2.http,
                            &format!("/v0/tenant/{party_id}/status"),
                            "tenant-add-hosts",
                        )
                        .await
                    })
                }
            },
        )
        .run(f)
        .await?;

    // ------------------------------------------------------------------
    // 5 — the point of the whole exercise: the party transacts from the new
    //     host, which is what "our node is down" has to look like.
    // ------------------------------------------------------------------
    Scenario::with_ctx(format!("{hint} transacts through its new host"), ())
        .when("the converted party submits through P2", {
            let party_id = party_id.clone();
            let config = p2_config.clone();
            move |f, _| {
                let party_id = party_id.clone();
                let config = config.clone();
                let wallet = ExternalKeyPair::from_seed(seed);
                Box::pin(async move {
                    let token = f.refresher.token().await?;
                    let created = create_marker_as_converted(
                        &config,
                        &token,
                        &party_id,
                        &wallet,
                        SEEDED_MARKERS + 1,
                    )
                    .await
                    .context("the converted party submitting through the host that joined")?;
                    anyhow::ensure!(
                        created == 1,
                        "the submission through P2 created {created} contract(s), not 1"
                    );
                    info!("converted party submitted through the joined host");
                    Ok(())
                })
            }
        })
        .run(f)
        .await
}

/// Sign every transaction hash in a prepare response with the wallet's key,
/// returning the base64 signatures index-aligned with the transactions.
fn sign_prepared(prepared: &Value, seed: [u8; 32]) -> anyhow::Result<Vec<String>> {
    let wallet = ExternalKeyPair::from_seed(seed);
    prepared
        .get("transaction_hashes")
        .and_then(Value::as_array)
        .context("prepare response missing transaction_hashes")?
        .iter()
        .map(|hash| {
            let encoded = hash
                .as_str()
                .context("transaction_hashes entry is not a string")?;
            let bytes = STANDARD
                .decode(encoded)
                .context("transaction hash is not valid base64")?;
            Ok(STANDARD.encode(wallet.sign(&bytes)))
        })
        .collect()
}
