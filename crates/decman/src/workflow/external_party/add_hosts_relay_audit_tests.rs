#![allow(deprecated)] // The supported Canton releases expose ListAll.

use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
};

use actix_web::{App, http::StatusCode, test as actix_test, web};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use canton_proto_rs::com::digitalasset::canton::{
    admin::participant::v30::{
        ClearPartyOnboardingFlagResponse, GetSynchronizerIdResponse,
        ListConnectedSynchronizersResponse, ListRegisteredSynchronizersResponse,
        ModifySynchronizerRequest, SynchronizerConnectionConfig,
        list_connected_synchronizers_response, list_registered_synchronizers_response,
    },
    crypto::v30::Signature,
    protocol::v30::{
        TopologyTransaction,
        party_to_participant::{HostingParticipant, hosting_participant},
    },
    topology::admin::v30::{
        BaseResult, ListAllResponse, ListPartyToParticipantResponse, SignTransactionsRequest,
        SignTransactionsResponse, TopologyTransactions, list_party_to_participant_response,
        topology_transactions,
    },
};
use common::types::{WorkflowKind, WorkflowProgress, WorkflowRole, WorkflowRun};
use http::{HeaderMap, HeaderValue};
use hyper::{
    Body, Request, Response, Server,
    service::{make_service_fn, service_fn},
};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use crate::{
    db::{
        MIGRATOR,
        schema::{Commitable, SchemaWrite},
    },
    server::{AppState, TestAuthMiddleware, tenant_call_audit, test_tenant_acs_import},
    workflow::party_replication::{
        TransferSource, import_party_acs,
        pipe::{PipeBlock, PipeTrailer},
    },
};

use super::*;

#[derive(Default)]
struct Connectivity {
    connected: bool,
    manual: bool,
    disconnects: usize,
    imports: usize,
    import_success: bool,
    reconnects: usize,
    reconnect_failures: usize,
    modifications: usize,
    clear_failures: usize,
    clears: usize,
    proposal: Vec<u8>,
    unrelated_proposals: Vec<Vec<u8>>,
    current_mapping: Option<(i32, PartyToParticipant)>,
    reject_signing: bool,
    sign_calls: usize,
    import_entered: Option<Arc<Notify>>,
    import_release: Option<Arc<Notify>>,
    entered: Option<Arc<Notify>>,
    release: Option<Arc<Notify>>,
}

struct CantonBoundary {
    config: NodeConfig,
    state: Arc<Mutex<Connectivity>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for CantonBoundary {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl CantonBoundary {
    fn new(connected: bool, manual: bool) -> Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let mut config = NodeConfig::default();
        config.node.participant_id = Some(participant(2));
        config.canton.admin_api_port = listener.local_addr()?.port();
        let state = Arc::new(Mutex::new(Connectivity {
            connected,
            manual,
            ..Default::default()
        }));
        let shared = state.clone();
        let service = make_service_fn(move |_| {
            let shared = shared.clone();
            async move {
                Ok::<_, Infallible>(service_fn(move |request| respond(request, shared.clone())))
            }
        });
        let server = Server::from_tcp(listener)?.http2_only(true).serve(service);
        let task = tokio::spawn(async move {
            server.await.expect("the Canton boundary serves HTTP/2");
        });
        Ok(Self {
            config,
            state,
            task,
        })
    }
}

async fn respond(
    request: Request<Body>,
    state: Arc<Mutex<Connectivity>>,
) -> Result<Response<Body>, Infallible> {
    let method = request
        .uri()
        .path()
        .rsplit('/')
        .next()
        .expect("an RPC name")
        .to_string();
    let body = hyper::body::to_bytes(request.into_body())
        .await
        .expect("the unary request completes");
    let request_bytes = body.get(5..).expect("a gRPC frame");
    if method == "SignTransactions" {
        let rejects = {
            let mut state = state.lock().expect("state lock");
            state.sign_calls += 1;
            state.reject_signing
        };
        if rejects {
            return Ok(Response::builder()
                .header("content-type", "application/grpc")
                .header("grpc-status", "3")
                .header("grpc-message", "the audit test refuses to sign")
                .body(Body::empty())
                .expect("a valid nonretryable signing refusal"));
        }
    }
    if method == "ImportPartyAcs" {
        let (succeeds, entered, release) = {
            let mut state = state.lock().expect("state lock");
            state.imports += 1;
            (
                state.import_success,
                state.import_entered.take(),
                state.import_release.take(),
            )
        };
        if let Some(entered) = entered {
            entered.notify_one();
        }
        if let Some(release) = release {
            release.notified().await;
        }
        if !succeeds {
            return Ok(Response::builder()
                .header("content-type", "application/grpc")
                .header("grpc-status", "3")
                .header("grpc-message", "the snapshot is deliberately rejected")
                .body(Body::empty())
                .expect("a valid gRPC refusal"));
        }
    }
    if method == "ClearPartyOnboardingFlag" {
        let fails = {
            let mut state = state.lock().expect("state lock");
            state.clears += 1;
            if state.clear_failures > 0 {
                state.clear_failures -= 1;
                true
            } else {
                false
            }
        };
        if fails {
            return Ok(Response::builder()
                .header("content-type", "application/grpc")
                .header("grpc-status", "14")
                .header(
                    "grpc-message",
                    "the first marker clearance is deliberately unavailable",
                )
                .body(Body::empty())
                .expect("a valid gRPC refusal"));
        }
    }
    let payload = match method.as_str() {
        "ListPackages" | "ListVettedPackages" => vec![],
        "SignTransactions" => {
            let request = SignTransactionsRequest::decode(request_bytes)
                .expect("the signing request decodes");
            SignTransactionsResponse {
                transactions: request.transactions,
            }
            .encode_to_vec()
        }
        "ImportPartyAcs" => vec![],
        "ListConnectedSynchronizers" => {
            let connected = state.lock().expect("state lock").connected;
            ListConnectedSynchronizersResponse {
                connected_synchronizers: if connected {
                    vec![list_connected_synchronizers_response::Result {
                        synchronizer_alias: "global".into(),
                        healthy: true,
                        ..Default::default()
                    }]
                } else {
                    vec![]
                },
            }
            .encode_to_vec()
        }
        "ListRegisteredSynchronizers" => {
            let state = state.lock().expect("state lock");
            ListRegisteredSynchronizersResponse {
                results: vec![list_registered_synchronizers_response::Result {
                    config: Some(SynchronizerConnectionConfig {
                        synchronizer_alias: "global".into(),
                        manual_connect: state.manual,
                        ..Default::default()
                    }),
                    connected: state.connected,
                    status: 1,
                    ..Default::default()
                }],
            }
            .encode_to_vec()
        }
        "ModifySynchronizer" => {
            let request = ModifySynchronizerRequest::decode(request_bytes)
                .expect("the modified config decodes");
            let mut state = state.lock().expect("state lock");
            state.modifications += 1;
            state.manual = request.new_config.expect("a new config").manual_connect;
            vec![]
        }
        "ReconnectSynchronizers" => {
            let (entered, release, fails) = {
                let mut state = state.lock().expect("state lock");
                state.reconnects += 1;
                let fails = state.reconnect_failures > 0;
                if fails {
                    state.reconnect_failures -= 1;
                }
                (state.entered.take(), state.release.take(), fails)
            };
            if let Some(entered) = entered {
                entered.notify_one();
            }
            if let Some(release) = release {
                release.notified().await;
            }
            if fails {
                return Ok(Response::builder()
                    .header("content-type", "application/grpc")
                    .header("grpc-status", "14")
                    .header(
                        "grpc-message",
                        "the first reconnect is deliberately unavailable",
                    )
                    .body(Body::empty())
                    .expect("a valid gRPC refusal"));
            }
            state.lock().expect("state lock").connected = true;
            vec![]
        }
        "ClearPartyOnboardingFlag" => ClearPartyOnboardingFlagResponse {
            onboarded: true,
            earliest_retry_timestamp: None,
        }
        .encode_to_vec(),
        "DisconnectSynchronizer" => {
            let (entered, release) = {
                let mut state = state.lock().expect("state lock");
                assert!(
                    state.manual,
                    "automatic reconnect must be disabled before disconnecting"
                );
                state.disconnects += 1;
                (state.entered.clone(), state.release.clone())
            };
            if let Some(entered) = entered {
                entered.notify_one();
            }
            if let Some(release) = release {
                release.notified().await;
            }
            state.lock().expect("state lock").connected = false;
            vec![]
        }
        "GetSynchronizerId" => GetSynchronizerIdResponse {
            synchronizer_id: format!("global::1220{}", "cc".repeat(32)),
            physical_synchronizer_id: format!("global::1220{}::36-0", "cc".repeat(32)),
        }
        .encode_to_vec(),
        "ListAll" => {
            let state = state.lock().expect("state lock");
            ListAllResponse {
                result: Some(TopologyTransactions {
                    items: state
                        .unrelated_proposals
                        .iter()
                        .chain(std::iter::once(&state.proposal))
                        .map(|bytes| topology_transactions::Item {
                            transaction: bytes.clone(),
                            ..Default::default()
                        })
                        .collect(),
                }),
            }
            .encode_to_vec()
        }
        "ListPartyToParticipant" => {
            let (serial, mapping) = state
                .lock()
                .expect("state lock")
                .current_mapping
                .clone()
                .unwrap_or_else(|| (8, mapping()));
            ListPartyToParticipantResponse {
                results: vec![list_party_to_participant_response::Result {
                    context: Some(BaseResult {
                        serial,
                        operation: TopologyChangeOp::AddReplace as i32,
                        ..Default::default()
                    }),
                    item: Some(list_party_to_participant_response::result::Item::V30(
                        mapping,
                    )),
                }],
            }
            .encode_to_vec()
        }
        other => panic!("unexpected RPC {other}; the stage must never publish or reconnect"),
    };
    let mut frame = vec![0];
    frame.extend((payload.len() as u32).to_be_bytes());
    frame.extend(payload);
    let (mut sender, response) = Body::channel();
    tokio::spawn(async move {
        sender
            .send_data(frame.into())
            .await
            .expect("send the protobuf response");
        let mut trailers = HeaderMap::new();
        trailers.insert("grpc-status", HeaderValue::from_static("0"));
        sender
            .send_trailers(trailers)
            .await
            .expect("send the status");
    });
    Ok(Response::builder()
        .header("content-type", "application/grpc")
        .body(response)
        .expect("valid response"))
}

fn participant(tag: u8) -> CantonId {
    format!("participant-{tag}::1220{}", format!("{tag:02x}").repeat(32))
        .parse()
        .expect("valid participant")
}

fn mapping() -> PartyToParticipant {
    PartyToParticipant {
        party: format!("observed::1220{}", "ab".repeat(32)),
        threshold: 1,
        participants: vec![
            HostingParticipant {
                participant_uid: participant(1).to_string(),
                permission: 2,
                onboarding: None,
            },
            HostingParticipant {
                participant_uid: participant(2).to_string(),
                permission: 2,
                onboarding: Some(hosting_participant::Onboarding {}),
            },
        ],
        party_signing_keys: None,
    }
}

fn signed(mapping: PartyToParticipant) -> SignedTopologyTransaction {
    let transaction = TopologyTransaction {
        serial: 8,
        operation: TopologyChangeOp::AddReplace as i32,
        mapping: Some(TopologyMapping {
            mapping: Some(topology_mapping::Mapping::PartyToParticipant(mapping)),
        }),
    };
    SignedTopologyTransaction {
        transaction: UntypedVersionedMessage {
            version: 30,
            wrapper: Some(untyped_versioned_message::Wrapper::Data(
                transaction.encode_to_vec(),
            )),
        }
        .encode_to_vec(),
        signatures: vec![Signature {
            signed_by: participant(2).namespace.to_hex(),
            signature: vec![3; 64],
            ..Default::default()
        }],
        proposal: true,
        multi_transaction_signatures: vec![],
    }
}

async fn stored_stage(db: &SqlitePool) -> Result<(ReplicationTarget, Vec<Vec<u8>>)> {
    let mapping = mapping();
    let target = replication_target(&mapping.party, &participant(2), 7)?;
    let signatures = vec![encode_signed(&signed(mapping.clone()))];
    let staged = Staged {
        party_id: mapping.party,
        new_hosts: vec![participant(2).to_string()],
        permission: HostPermission::Confirmation,
        base_serial: 7,
        signed_transactions: signatures.clone(),
    };
    target
        .write_artifact(
            db,
            artifact_kinds::TENANT_ADD_HOSTS_STAGED_SIGNATURES,
            None,
            &serde_json::to_vec(&staged)?,
        )
        .await?;
    assert!(claim_import_window(db, &target, None).await?);
    Ok((target, signatures))
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn a_resumed_stage_hides_its_signature_until_manual_disconnect_finishes(
    db: SqlitePool,
) -> Result<()> {
    let boundary = CantonBoundary::new(true, false)?;
    let (target, signatures) = stored_stage(&db).await?;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    {
        let mut state = boundary.state.lock().expect("state lock");
        state.entered = Some(entered.clone());
        state.release = Some(release.clone());
    }
    let config = boundary.config.clone();
    let pool = db.clone();
    let party = target.party_id.to_string();
    let request = tokio::spawn(async move {
        stage_add_hosts(
            &config,
            &pool,
            &party,
            &[participant(2)],
            HostPermission::Confirmation,
            7,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified()).await?;
    assert!(
        !request.is_finished(),
        "the signature must not escape before the disconnect completes"
    );
    assert!(
        target
            .read_artifact(&db, artifact_kinds::TENANT_ADD_HOSTS_STAGE_READY, None)
            .await?
            .is_none()
    );
    assert_eq!(
        target
            .read_artifact(&db, target.artifacts.import_inflight, None)
            .await?
            .as_deref(),
        Some(b"disconnect-first".as_slice())
    );
    release.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), request).await???,
        signatures
    );
    assert!(
        target
            .read_artifact(&db, artifact_kinds::TENANT_ADD_HOSTS_STAGE_READY, None)
            .await?
            .is_some()
    );
    let state = boundary.state.lock().expect("state lock");
    assert!(!state.connected);
    assert!(state.manual);
    assert_eq!(state.disconnects, 1);
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn a_ready_stage_does_not_disconnect_a_reconnected_participant_or_release_signatures(
    db: SqlitePool,
) -> Result<()> {
    let boundary = CantonBoundary::new(true, false)?;
    let (target, _) = stored_stage(&db).await?;
    target
        .write_artifact(
            &db,
            artifact_kinds::TENANT_ADD_HOSTS_STAGE_READY,
            None,
            b"1",
        )
        .await?;
    let result = stage_add_hosts(
        &boundary.config,
        &db,
        &target.party_id.to_string(),
        &[participant(2)],
        HostPermission::Confirmation,
        7,
    )
    .await;
    assert!(matches!(result, Err(AddHostsError::Busy(_))));
    let state = boundary.state.lock().expect("state lock");
    assert!(state.connected);
    assert!(!state.manual);
    assert_eq!(state.disconnects, 0);
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn import_requires_every_durable_proof_and_the_exact_local_proposal(
    db: SqlitePool,
) -> Result<()> {
    let boundary = CantonBoundary::new(false, true)?;
    let (target, signatures) = stored_stage(&db).await?;
    {
        let mut state = boundary.state.lock().expect("state lock");
        state.proposal = signatures[0].clone();
        state.unrelated_proposals = vec![
            vec![0xff],
            UntypedVersionedMessage {
                version: 31,
                wrapper: Some(untyped_versioned_message::Wrapper::Data(vec![])),
            }
            .encode_to_vec(),
        ];
        let mut unrelated_mapping = mapping();
        unrelated_mapping.party = unrelated_mapping.party.replace("observed", "unrelated");
        let mut multi_signature_proposal = signed(unrelated_mapping);
        multi_signature_proposal
            .multi_transaction_signatures
            .push(Default::default());
        state
            .unrelated_proposals
            .push(encode_signed(&multi_signature_proposal));
    }
    assert!(!import_onboarding_ready(&boundary.config, &db, &target, 7).await?);
    target
        .write_artifact(
            &db,
            artifact_kinds::TENANT_ADD_HOSTS_STAGE_READY,
            None,
            b"1",
        )
        .await?;
    assert!(!import_onboarding_ready(&boundary.config, &db, &target, 7).await?);
    target
        .write_artifact(
            &db,
            target.artifacts.import_inflight,
            None,
            b"disconnect-first",
        )
        .await?;
    assert!(import_onboarding_ready(&boundary.config, &db, &target, 7).await?);
    assert!(!import_onboarding_ready(&boundary.config, &db, &target, 8).await?);
    boundary.state.lock().expect("state lock").proposal = encode_signed(&signed({
        let mut changed = mapping();
        changed.threshold = 2;
        changed
    }));
    assert!(!import_onboarding_ready(&boundary.config, &db, &target, 7).await?);
    boundary.state.lock().expect("state lock").proposal = signatures[0].clone();
    release_import_window_owner(&db, &target).await?;
    assert!(!import_onboarding_ready(&boundary.config, &db, &target, 7).await?);
    Ok(())
}

#[test]
fn exact_relay_validation_rejects_other_parties_hosts_permissions_and_keys() -> Result<()> {
    let expected = mapping();
    let mut changes = vec![];
    let mut changed = expected.clone();
    changed.party = mapping().party.replace("observed", "attacker");
    changes.push(changed);
    let mut changed = expected.clone();
    changed.participants.remove(0);
    changes.push(changed);
    let mut changed = expected.clone();
    changed.participants[0].permission = 1;
    changes.push(changed);
    let mut changed = expected.clone();
    changed.participants[1].permission = 1;
    changes.push(changed);
    let mut changed = expected.clone();
    changed.party_signing_keys = Some(Default::default());
    changes.push(changed);
    for changed in changes {
        assert!(exact_mapping(&signed(changed), &expected, 8).is_err());
    }
    let mut altered = signed(expected.clone());
    altered.signatures.clear();
    assert!(decode_signed(&encode_signed(&altered)).is_err());
    let mut altered = signed(expected);
    altered.proposal = false;
    assert!(decode_signed(&encode_signed(&altered)).is_err());
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn a_failed_staged_import_keeps_the_participant_offline_and_retains_ownership(
    db: SqlitePool,
) -> Result<()> {
    let boundary = CantonBoundary::new(false, true)?;
    let (target, _) = stored_stage(&db).await?;
    target
        .write_artifact(
            &db,
            target.artifacts.import_inflight,
            None,
            b"disconnect-first",
        )
        .await?;
    let bytes = b"a snapshot Canton rejects".to_vec();
    let trailer = PipeTrailer {
        total_len: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(&bytes)),
    };
    let mut blocks = std::collections::VecDeque::from([
        PipeBlock::Data { seq: 1, bytes },
        PipeBlock::End { seq: 2, trailer },
    ]);
    let error = import_party_acs(
        &boundary.config,
        &db,
        &target,
        &[],
        TransferSource::Relay,
        move |_| {
            futures::future::ready(
                blocks
                    .pop_front()
                    .ok_or_else(|| anyhow::anyhow!("no more blocks")),
            )
        },
    )
    .await
    .expect_err("the boundary rejects the import");
    assert!(format!("{error:#}").contains("stays disconnected"));
    assert_eq!(
        import_window_owner(&db, &participant(2)).await?,
        Some(target.instance_name)
    );
    assert!(
        db.get_acs_import_quarantine(&target.party_id, &target.target_participant_id)
            .await?
            .is_some()
    );
    let state = boundary.state.lock().expect("state lock");
    assert_eq!(state.imports, 1);
    assert!(!state.connected);
    assert!(state.manual);
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn a_cancelled_final_http_request_still_releases_successful_import_ownership(
    db: SqlitePool,
) -> Result<()> {
    let boundary = CantonBoundary::new(false, true)?;
    let (target, signatures) = stored_stage(&db).await?;
    for (kind, scope, bytes) in [
        (
            artifact_kinds::TENANT_ADD_HOSTS_STAGE_READY,
            None,
            b"1".as_slice(),
        ),
        (
            target.artifacts.import_inflight,
            None,
            b"disconnect-first".as_slice(),
        ),
        (
            target.artifacts.pre_activation_offset,
            Some(participant(2).to_string()),
            b"1".as_slice(),
        ),
    ] {
        target
            .write_artifact(&db, kind, scope.as_deref(), bytes)
            .await?;
    }
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    {
        let mut state = boundary.state.lock().expect("state lock");
        state.proposal = signatures[0].clone();
        state.clear_failures = 1;
        state.entered = Some(entered.clone());
        state.release = Some(release.clone());
    }
    let mut state = Arc::try_unwrap(AppState::for_test_on(None, db.clone()).into_inner())
        .unwrap_or_else(|_| panic!("the test state has no other owner"));
    state.config = boundary.config.clone();
    let app = actix_test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .wrap(TestAuthMiddleware)
            .service(test_tenant_acs_import),
    )
    .await;
    let final_block = serde_json::json!({
        "party_id": target.party_id.to_string(), "base_serial": 7,
        "seq": 1, "end": true, "chunk": "", "total_len": 0,
        "sha256": hex::encode(Sha256::digest([])),
    });
    let request = actix_test::TestRequest::post()
        .uri("/v0/tenant/add-hosts/import")
        .set_json(&final_block)
        .to_request();
    let mut response = Box::pin(actix_test::call_service(&app, request));
    tokio::select! {
        _ = &mut response => panic!("the final response must wait for a healthy reconnect"),
        result = tokio::time::timeout(Duration::from_secs(5), entered.notified()) => result?,
    }
    assert!(import_window_owner(&db, &participant(2)).await?.is_some());
    drop(response);
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let rows = tenant_call_audit::list(&db, None, 10).await?;
            if rows.iter().any(|row| row.http_status == Some(500)) {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    assert!(import_window_owner(&db, &participant(2)).await?.is_none());
    assert!(
        target
            .read_artifact(&db, artifact_kinds::TENANT_ADD_HOSTS_IMPORT_COMPLETED, None)
            .await?
            .is_some()
    );
    assert_eq!(
        boundary.state.lock().expect("state lock").imports,
        0,
        "an empty ACS requires no import RPC"
    );
    let before = {
        let mut state = boundary.state.lock().expect("state lock");
        state.entered = None;
        state.release = None;
        assert_eq!(state.clears, 1);
        (
            state.imports,
            state.reconnects,
            state.disconnects,
            state.modifications,
        )
    };
    let response = actix_test::call_service(
        &app,
        actix_test::TestRequest::post()
            .uri("/v0/tenant/add-hosts/import")
            .set_json(&final_block)
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), actix_web::http::StatusCode::OK);
    let body: serde_json::Value = actix_test::read_body_json(response).await;
    assert_eq!(body["complete"], true);
    assert_eq!(body["marker_cleared"], true);
    let state = boundary.state.lock().expect("state lock");
    assert_eq!(
        (
            state.imports,
            state.reconnects,
            state.disconnects,
            state.modifications
        ),
        before,
        "a completed final-block retry may only request marker clearance"
    );
    assert_eq!(state.clears, 2);
    Ok(())
}

async fn reconnect_fixture(
    db: &SqlitePool,
    boundary: &CantonBoundary,
) -> Result<(ReplicationTarget, web::Data<AppState>)> {
    let (target, signatures) = stored_stage(db).await?;
    boundary.state.lock().expect("state lock").proposal = signatures[0].clone();
    for (kind, scope, bytes) in [
        (
            artifact_kinds::TENANT_ADD_HOSTS_STAGE_READY,
            None,
            b"1".as_slice(),
        ),
        (
            target.artifacts.import_inflight,
            None,
            b"disconnect-first".as_slice(),
        ),
        (
            target.artifacts.pre_activation_offset,
            Some(participant(2).to_string()),
            b"1".as_slice(),
        ),
    ] {
        target
            .write_artifact(db, kind, scope.as_deref(), bytes)
            .await?;
    }
    let mut state = Arc::try_unwrap(AppState::for_test_on(None, db.clone()).into_inner())
        .unwrap_or_else(|_| panic!("the test state has no other owner"));
    state.config = boundary.config.clone();
    Ok((target, web::Data::new(state)))
}

fn final_import_block(target: &ReplicationTarget, seq: u64, bytes: &[u8]) -> serde_json::Value {
    serde_json::json!({
        "party_id": target.party_id.to_string(), "base_serial": 7,
        "seq": seq, "end": true, "chunk": "", "total_len": bytes.len(),
        "sha256": hex::encode(Sha256::digest(bytes)),
    })
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn a_successful_import_reconnect_failure_recovers_without_importing_twice(
    db: SqlitePool,
) -> Result<()> {
    let boundary = CantonBoundary::new(false, true)?;
    let (target, data) = reconnect_fixture(&db, &boundary).await?;
    {
        let mut state = boundary.state.lock().expect("state lock");
        state.import_success = true;
        state.reconnect_failures = 1;
    }
    let app = actix_test::init_service(
        App::new()
            .app_data(data.clone())
            .wrap(TestAuthMiddleware)
            .service(test_tenant_acs_import),
    )
    .await;
    let bytes = b"the original snapshot bytes accepted by the Canton boundary";
    let first = actix_test::call_service(
        &app,
        actix_test::TestRequest::post()
            .uri("/v0/tenant/add-hosts/import")
            .set_json(serde_json::json!({
                "party_id": target.party_id.to_string(), "base_serial": 7,
                "seq": 1, "end": false, "chunk": STANDARD.encode(bytes),
            }))
            .to_request(),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let final_block = final_import_block(&target, 2, bytes);
    let failed = actix_test::call_service(
        &app,
        actix_test::TestRequest::post()
            .uri("/v0/tenant/add-hosts/import")
            .set_json(&final_block)
            .to_request(),
    )
    .await;
    assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let error: serde_json::Value = actix_test::read_body_json(failed).await;
    let error = error["error"]
        .as_str()
        .expect("the failed request explains recovery");
    assert!(
        error.contains("ACS is imported") && error.contains("final block"),
        "{error}"
    );
    assert!(
        !error.contains("stays disconnected"),
        "manual mode is already off: {error}"
    );
    assert!(
        target
            .read_artifact(&db, artifact_kinds::TENANT_ADD_HOSTS_ACS_IMPORTED, None)
            .await?
            .is_some()
    );
    assert!(
        target
            .read_artifact(&db, artifact_kinds::TENANT_ADD_HOSTS_IMPORT_COMPLETED, None)
            .await?
            .is_none()
    );
    assert_eq!(
        import_window_owner(&db, &participant(2)).await?,
        Some(target.instance_name.clone())
    );
    assert!(
        db.get_acs_import_quarantine(&target.party_id, &target.target_participant_id)
            .await?
            .is_none()
    );
    {
        let state = boundary.state.lock().expect("state lock");
        assert_eq!(state.imports, 1);
        assert_eq!(state.reconnects, 1);
        assert_eq!(state.clears, 0);
        assert!(!state.manual);
    }
    let restart = actix_test::call_service(
        &app,
        actix_test::TestRequest::post()
            .uri("/v0/tenant/add-hosts/import")
            .set_json(serde_json::json!({
                "party_id": target.party_id.to_string(), "base_serial": 7,
                "seq": 1, "end": false, "chunk": STANDARD.encode(bytes),
            }))
            .to_request(),
    )
    .await;
    assert_eq!(restart.status(), StatusCode::CONFLICT);
    assert_eq!(boundary.state.lock().expect("state lock").imports, 1);
    let resumed = actix_test::call_service(
        &app,
        actix_test::TestRequest::post()
            .uri("/v0/tenant/add-hosts/import")
            .set_json(&final_block)
            .to_request(),
    )
    .await;
    assert_eq!(resumed.status(), StatusCode::OK);
    let result: serde_json::Value = actix_test::read_body_json(resumed).await;
    assert_eq!(result["complete"], true);
    assert_eq!(result["marker_cleared"], true);
    assert!(import_window_owner(&db, &participant(2)).await?.is_none());
    assert!(
        target
            .read_artifact(&db, artifact_kinds::TENANT_ADD_HOSTS_IMPORT_COMPLETED, None)
            .await?
            .is_some()
    );
    let state = boundary.state.lock().expect("state lock");
    assert_eq!(state.imports, 1, "recovery must never reimport the ACS");
    assert_eq!(state.reconnects, 2);
    assert_eq!(state.clears, 1);
    assert!(state.connected && !state.manual);
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn an_empty_import_cannot_resume_while_retiring_but_can_after_reconnect_fails(
    db: SqlitePool,
) -> Result<()> {
    let boundary = CantonBoundary::new(false, true)?;
    let (target, data) = reconnect_fixture(&db, &boundary).await?;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    {
        let mut state = boundary.state.lock().expect("state lock");
        state.reconnect_failures = 1;
        state.entered = Some(entered.clone());
        state.release = Some(release.clone());
    }
    let task_data = data.clone();
    let task_target = target.clone();
    let first = tokio::spawn(async move {
        task_data
            .relay_sessions
            .accept_block(
                &task_data.config,
                &task_data.db,
                &task_target,
                &[],
                PipeBlock::End {
                    seq: 1,
                    trailer: PipeTrailer {
                        total_len: 0,
                        sha256: hex::encode(Sha256::digest([])),
                    },
                },
                None,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified()).await?;
    assert!(
        target
            .read_artifact(&db, artifact_kinds::TENANT_ADD_HOSTS_ACS_IMPORTED, None)
            .await?
            .is_some()
    );
    assert!(
        data.relay_sessions
            .any_session(|name| name == target.instance_name)
            .await,
        "the actual import must still own its retiring relay session"
    );
    let app = actix_test::init_service(
        App::new()
            .app_data(data.clone())
            .wrap(TestAuthMiddleware)
            .service(test_tenant_acs_import),
    )
    .await;
    let final_block = final_import_block(&target, 1, &[]);
    let busy = tokio::time::timeout(
        Duration::from_secs(5),
        actix_test::call_service(
            &app,
            actix_test::TestRequest::post()
                .uri("/v0/tenant/add-hosts/import")
                .set_json(&final_block)
                .to_request(),
        ),
    )
    .await?;
    assert_eq!(busy.status(), StatusCode::CONFLICT);
    assert_eq!(
        boundary.state.lock().expect("state lock").reconnects,
        1,
        "a retiring import must finish before a recovery reconnect starts"
    );
    release.notify_one();
    let first = tokio::time::timeout(Duration::from_secs(5), first).await??;
    assert!(
        first.is_err(),
        "the first empty import fails at its reconnect"
    );
    assert!(
        !data
            .relay_sessions
            .any_session(|name| name == target.instance_name)
            .await
    );
    let resumed = actix_test::call_service(
        &app,
        actix_test::TestRequest::post()
            .uri("/v0/tenant/add-hosts/import")
            .set_json(&final_block)
            .to_request(),
    )
    .await;
    assert_eq!(resumed.status(), StatusCode::OK);
    let result: serde_json::Value = actix_test::read_body_json(resumed).await;
    assert_eq!(result["complete"], true);
    assert_eq!(result["marker_cleared"], true);
    assert!(import_window_owner(&db, &participant(2)).await?.is_none());
    assert!(
        target
            .read_artifact(&db, artifact_kinds::TENANT_ADD_HOSTS_IMPORT_COMPLETED, None)
            .await?
            .is_some()
    );
    let state = boundary.state.lock().expect("state lock");
    assert_eq!(state.imports, 0, "the empty ACS requires no import RPC");
    assert_eq!(state.reconnects, 2);
    assert_eq!(state.clears, 1);
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn a_cancelled_nonempty_final_request_serializes_block_one_until_completion(
    db: SqlitePool,
) -> Result<()> {
    let boundary = CantonBoundary::new(false, true)?;
    let (target, data) = reconnect_fixture(&db, &boundary).await?;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    {
        let mut state = boundary.state.lock().expect("state lock");
        state.import_success = true;
        state.import_entered = Some(entered.clone());
        state.import_release = Some(release.clone());
    }
    let app = actix_test::init_service(
        App::new()
            .app_data(data.clone())
            .wrap(TestAuthMiddleware)
            .service(test_tenant_acs_import),
    )
    .await;
    let bytes = b"nonempty snapshot whose Canton acknowledgement is delayed";
    let first_block = serde_json::json!({
        "party_id": target.party_id.to_string(), "base_serial": 7,
        "seq": 1, "end": false, "chunk": STANDARD.encode(bytes),
    });
    let first = actix_test::call_service(
        &app,
        actix_test::TestRequest::post()
            .uri("/v0/tenant/add-hosts/import")
            .set_json(&first_block)
            .to_request(),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let final_block = final_import_block(&target, 2, bytes);
    let mut final_response = Box::pin(actix_test::call_service(
        &app,
        actix_test::TestRequest::post()
            .uri("/v0/tenant/add-hosts/import")
            .set_json(&final_block)
            .to_request(),
    ));
    tokio::select! {
        _ = &mut final_response => panic!("the final request must wait for Canton's acknowledgement"),
        result = tokio::time::timeout(Duration::from_secs(15), entered.notified()) => result?,
    }
    drop(final_response);
    assert!(
        data.tenant_import_mu.try_lock().is_err(),
        "cancelling the caller must not release admission before import finalization"
    );
    let mut restarted = Box::pin(actix_test::call_service(
        &app,
        actix_test::TestRequest::post()
            .uri("/v0/tenant/add-hosts/import")
            .set_json(&first_block)
            .to_request(),
    ));
    tokio::select! {
        _ = &mut restarted => panic!("block one must wait until the cancelled final request finishes"),
        _ = tokio::time::sleep(Duration::from_millis(100)) => {},
    }
    {
        let state = boundary.state.lock().expect("state lock");
        assert_eq!(state.imports, 1);
        assert!(state.manual && !state.connected);
        assert_eq!(
            state.modifications, 0,
            "manual mode must remain enabled until import acknowledgement"
        );
        assert_eq!(state.reconnects, 0);
    }
    release.notify_one();
    let restarted = tokio::time::timeout(Duration::from_secs(10), restarted).await?;
    assert_eq!(
        restarted.status(),
        StatusCode::CONFLICT,
        "a stale block-one request must recheck completed ownership after waiting"
    );
    assert!(import_window_owner(&db, &participant(2)).await?.is_none());
    assert!(
        target
            .read_artifact(&db, artifact_kinds::TENANT_ADD_HOSTS_IMPORT_COMPLETED, None)
            .await?
            .is_some()
    );
    let state = boundary.state.lock().expect("state lock");
    assert_eq!(
        state.imports, 1,
        "the cancelled final request must not permit reimport"
    );
    assert_eq!(state.disconnects, 0);
    assert_eq!(state.reconnects, 1);
    assert_eq!(state.modifications, 1);
    assert!(state.connected && !state.manual);
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn a_completion_release_failure_returns_500_and_retries_without_reimport(
    db: SqlitePool,
) -> Result<()> {
    let boundary = CantonBoundary::new(false, true)?;
    let (target, data) = reconnect_fixture(&db, &boundary).await?;
    boundary.state.lock().expect("state lock").import_success = true;
    sqlx::query(
        "CREATE TRIGGER refuse_tenant_release BEFORE DELETE ON tenant_import_window \
        BEGIN SELECT RAISE(ABORT, 'the audit test refuses owner deletion'); END",
    )
    .execute(&db)
    .await?;
    let app = actix_test::init_service(
        App::new()
            .app_data(data.clone())
            .wrap(TestAuthMiddleware)
            .service(test_tenant_acs_import),
    )
    .await;
    let bytes = b"snapshot imported before the durable owner release fails";
    let first = actix_test::call_service(
        &app,
        actix_test::TestRequest::post()
            .uri("/v0/tenant/add-hosts/import")
            .set_json(serde_json::json!({
                "party_id": target.party_id.to_string(), "base_serial": 7,
                "seq": 1, "end": false, "chunk": STANDARD.encode(bytes),
            }))
            .to_request(),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let final_block = final_import_block(&target, 2, bytes);
    let failed = actix_test::call_service(
        &app,
        actix_test::TestRequest::post()
            .uri("/v0/tenant/add-hosts/import")
            .set_json(&final_block)
            .to_request(),
    )
    .await;
    assert_eq!(
        failed.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "an owner release failure must not report a completed import"
    );
    let body: serde_json::Value = actix_test::read_body_json(failed).await;
    assert_ne!(body.get("complete"), Some(&serde_json::Value::Bool(true)));
    let error = body["error"]
        .as_str()
        .expect("the release failure explains recovery");
    assert!(
        error.contains("durable import window")
            && error.contains("Retry the identical final block"),
        "{error}"
    );
    assert_eq!(
        import_window_owner(&db, &participant(2)).await?,
        Some(target.instance_name.clone())
    );
    assert!(
        target
            .read_artifact(&db, artifact_kinds::TENANT_ADD_HOSTS_ACS_IMPORTED, None)
            .await?
            .is_some()
    );
    assert!(
        target
            .read_artifact(&db, artifact_kinds::TENANT_ADD_HOSTS_IMPORT_COMPLETED, None)
            .await?
            .is_none()
    );
    {
        let state = boundary.state.lock().expect("state lock");
        assert_eq!(state.imports, 1);
        assert_eq!(
            state.clears, 0,
            "clearance must wait for durable ownership release"
        );
        assert!(state.connected && !state.manual);
    }
    sqlx::query("DROP TRIGGER refuse_tenant_release")
        .execute(&db)
        .await?;
    let resumed = actix_test::call_service(
        &app,
        actix_test::TestRequest::post()
            .uri("/v0/tenant/add-hosts/import")
            .set_json(&final_block)
            .to_request(),
    )
    .await;
    assert_eq!(resumed.status(), StatusCode::OK);
    let body: serde_json::Value = actix_test::read_body_json(resumed).await;
    assert_eq!(body["complete"], true);
    assert_eq!(body["marker_cleared"], true);
    assert!(import_window_owner(&db, &participant(2)).await?.is_none());
    assert!(
        target
            .read_artifact(&db, artifact_kinds::TENANT_ADD_HOSTS_IMPORT_COMPLETED, None)
            .await?
            .is_some()
    );
    let state = boundary.state.lock().expect("state lock");
    assert_eq!(
        state.imports, 1,
        "release recovery must not reimport the snapshot"
    );
    assert_eq!(state.clears, 1);
    Ok(())
}

async fn fresh_stage_fixture(
    db: &SqlitePool,
    boundary: &CantonBoundary,
) -> Result<ReplicationTarget> {
    let proposed = mapping();
    let target = replication_target(&proposed.party, &participant(2), 7)?;
    let mut current = proposed.clone();
    current
        .participants
        .retain(|host| host.participant_uid != participant(2).to_string());
    {
        let mut state = boundary.state.lock().expect("state lock");
        state.current_mapping = Some((7, current));
        state.proposal = encode_signed(&signed(proposed));
    }
    // Prepare captures these offsets before staging; neither is a signature or lease.
    target
        .write_artifact(db, target.artifacts.export_offset, None, b"1")
        .await?;
    target
        .write_artifact(
            db,
            target.artifacts.pre_activation_offset,
            Some(&participant(2).to_string()),
            b"1",
        )
        .await?;
    Ok(target)
}

async fn admit_workflow_after_stage_failure(db: &SqlitePool) -> Result<()> {
    let run: WorkflowRun = serde_json::from_value(serde_json::json!({
        "instance_name": "workflow-after-refused-stage",
        "kind": WorkflowKind::AddParty,
        "role": WorkflowRole::Coordinator,
        "status": WorkflowProgress::InProgress,
        "current_step": "WaitingForPeers", "step_index": 0, "step_total": 1,
        "config_json": "{}", "expected_peers": [], "completed_peers": [],
        "dismissed": false, "created_at": 1, "updated_at": 1,
    }))?;
    let mut transaction = db.begin_transaction().await?;
    transaction.upsert_workflow_run(&run).await?;
    transaction.commit().await?;
    assert_eq!(
        db.get_workflow_run(&run.instance_name)
            .await?
            .expect("the workflow is persisted")
            .status,
        WorkflowProgress::InProgress
    );
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn a_nonretryable_signing_failure_does_not_claim_or_disconnect_the_participant(
    db: SqlitePool,
) -> Result<()> {
    let boundary = CantonBoundary::new(true, false)?;
    let target = fresh_stage_fixture(&db, &boundary).await?;
    boundary.state.lock().expect("state lock").reject_signing = true;
    let result = stage_add_hosts(
        &boundary.config,
        &db,
        &target.party_id.to_string(),
        &[participant(2)],
        HostPermission::Confirmation,
        7,
    )
    .await;
    let error = result.expect_err("Canton refuses the signing request");
    assert!(
        format!("{error:#}").contains("the audit test refuses to sign"),
        "{error:#}"
    );
    assert!(import_window_owner(&db, &participant(2)).await?.is_none());
    assert!(
        target
            .read_artifact(
                &db,
                artifact_kinds::TENANT_ADD_HOSTS_STAGED_SIGNATURES,
                None
            )
            .await?
            .is_none()
    );
    assert!(
        target
            .read_artifact(&db, artifact_kinds::TENANT_ADD_HOSTS_STAGE_READY, None)
            .await?
            .is_none()
    );
    {
        let state = boundary.state.lock().expect("state lock");
        assert_eq!(
            state.sign_calls, 1,
            "an InvalidArgument refusal is not retried"
        );
        assert!(state.connected && !state.manual);
        assert_eq!(state.disconnects, 0);
        assert_eq!(state.modifications, 0);
    }
    admit_workflow_after_stage_failure(&db).await?;
    Ok(())
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn a_staged_signature_insert_failure_rolls_back_its_participant_claim(
    db: SqlitePool,
) -> Result<()> {
    let boundary = CantonBoundary::new(true, false)?;
    let target = fresh_stage_fixture(&db, &boundary).await?;
    sqlx::query(
        "CREATE TRIGGER refuse_staged_signature BEFORE INSERT ON tenant_replication_artifacts \
        WHEN NEW.artifact_kind = 'tenant_add_hosts_staged_signatures' \
        BEGIN SELECT RAISE(ABORT, 'the audit test refuses signature persistence'); END",
    )
    .execute(&db)
    .await?;
    let result = stage_add_hosts(
        &boundary.config,
        &db,
        &target.party_id.to_string(),
        &[participant(2)],
        HostPermission::Confirmation,
        7,
    )
    .await;
    let error = result.expect_err("the real SQLite trigger refuses signature persistence");
    assert!(
        format!("{error:#}").contains("the audit test refuses signature persistence"),
        "{error:#}"
    );
    assert!(
        import_window_owner(&db, &participant(2)).await?.is_none(),
        "signature persistence and participant ownership must roll back together"
    );
    assert!(
        target
            .read_artifact(
                &db,
                artifact_kinds::TENANT_ADD_HOSTS_STAGED_SIGNATURES,
                None
            )
            .await?
            .is_none()
    );
    assert!(
        target
            .read_artifact(&db, target.artifacts.import_inflight, None)
            .await?
            .is_none()
    );
    {
        let state = boundary.state.lock().expect("state lock");
        assert_eq!(
            state.sign_calls, 1,
            "the test must reach persistence after signing succeeds"
        );
        assert!(state.connected && !state.manual);
        assert_eq!(state.disconnects, 0);
        assert_eq!(state.modifications, 0);
    }
    admit_workflow_after_stage_failure(&db).await?;
    Ok(())
}
