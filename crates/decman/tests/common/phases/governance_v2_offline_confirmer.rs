//! Confirmation archives must not wait for the confirmer's participant.
//!
//! The member on P3 creates real votes while online. P3 then disconnects from
//! Canton, while P1 and P2 retain the governance party's hosting quorum.

use std::{collections::HashSet, path::Path, time::Duration};

use anyhow::{Context, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use canton_proto_rs::com::digitalasset::canton::admin::{
    participant::v30::{
        DisconnectSynchronizerRequest, ListConnectedSynchronizersRequest,
        ListRegisteredSynchronizersRequest, ModifySynchronizerRequest,
        ReconnectSynchronizerRequest, SynchronizerConnectionConfig,
        synchronizer_connectivity_service_client::SynchronizerConnectivityServiceClient,
    },
    sequencer::v30::SequencerConnectionValidation,
};
use common::{
    api::{ContractQueryResponse, DecentralizedPartiesResponse},
    canton_id::CantonId,
};
use dec_party_manager::{config::NodeConfig, utils, workflow::topology::fetch_p2p_mapping_at_head};
use serde_json::{Value, json};
use tokio::time::{Instant, sleep, timeout};
use tracing::info;

use super::{
    add_party_rehost_disconnect_first::admin_config, deploy_gov_core::configure_party_on_nodes,
};
use crate::common::{
    Fixture, TestTarget, chaos, db,
    http::probe_workflow_status,
    invitations::post_accept_invitation,
    ledger_api::{P1_JSON_API, P2_JSON_API, P3_JSON_API},
};

const RULES: &str = "Governance.Rules:GovernanceRules";
const SELF_CONFIRMATION: &str = "Governance.Rules:GovernanceSelfConfirmation";
const DOMAIN_CONFIRMATION: &str = "Governance.Confirmation:GovernanceConfirmation";
const GENERIC_VOTE: &str = "Governance.GenericVote:GenericVoteProposal";
const WAIT: Duration = Duration::from_secs(60);

struct Votes {
    party: String,
    member: String,
    long_rules: String,
    short_rules: String,
    self_votes: Vec<String>,
    domain_votes: Vec<String>,
    proposal: String,
    expired_self: String,
    expired_domain: String,
}

fn template(name: &str) -> String {
    format!("#governance-core-v2:{name}")
}

fn command(actor: &str, party: &str, command: Value) -> Value {
    json!({
        "commands": [command],
        "commandId": uuid::Uuid::new_v4().to_string(),
        "userId": "ledger-api-user",
        "actAs": [actor],
        "readAs": [party],
    })
}

fn exercise(name: &str, cid: &str, choice: &str, argument: Value) -> Value {
    json!({"ExerciseCommand": {
        "templateId": template(name),
        "contractId": cid,
        "choice": choice,
        "choiceArgument": argument,
    }})
}

async fn ids(f: &Fixture, port: u16, party: &str, name: &str) -> anyhow::Result<HashSet<String>> {
    let (module, entity) = name.split_once(':').context("template has no module")?;
    let path = format!(
        "/contracts/query?party_id={party}&package_id=%23governance-core-v2\
         &module_name={module}&entity_name={entity}&active_only=true"
    );
    let response: ContractQueryResponse = f.get_json(port, &path).await?;
    Ok(response
        .contracts
        .into_iter()
        .map(|c| c.contract_id)
        .collect())
}

async fn new_id(
    f: &Fixture,
    party: &str,
    name: &str,
    before: &HashSet<String>,
) -> anyhow::Result<String> {
    let deadline = Instant::now() + WAIT;
    loop {
        let current = ids(f, f.p1.http, party, name).await?;
        let added: Vec<_> = current.difference(before).cloned().collect();
        ensure!(
            added.len() <= 1,
            "{name} unexpectedly created {} contracts",
            added.len()
        );
        if let Some(cid) = added.into_iter().next() {
            return Ok(cid);
        }
        ensure!(Instant::now() < deadline, "no new {name} became visible");
        sleep(Duration::from_millis(500)).await;
    }
}

async fn submit_new(
    f: &Fixture,
    port: u16,
    actor: &str,
    party: &str,
    created_template: &str,
    operation: Value,
) -> anyhow::Result<String> {
    let before = ids(f, f.p1.http, party, created_template).await?;
    timeout(
        WAIT,
        f.submit_create(port, &command(actor, party, operation)),
    )
    .await
    .context("ledger command did not complete within one minute")??;
    new_id(f, party, created_template, &before).await
}

async fn accept_and_wait(f: &Fixture, run: &str, status: &str) -> anyhow::Result<()> {
    for port in [f.p2.http, f.p3.http] {
        let invite = chaos::wait_for_invite_for_instance(f, port, run, WAIT).await?;
        post_accept_invitation(f, port, &invite).await?;
    }
    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        if let Some(outcome) = probe_workflow_status(f, f.p1.http, status, run).await {
            return outcome;
        }
        ensure!(Instant::now() < deadline, "workflow {run} did not complete");
        sleep(Duration::from_secs(1)).await;
    }
}

async fn create_party(f: &Fixture) -> anyhow::Result<String> {
    let prefix = chaos::fresh_prefix("v2-offline-confirmer");
    chaos::post_onboarding(f, &prefix).await?;
    accept_and_wait(f, &format!("{prefix}-creation"), "/onboarding/status").await?;
    let deadline = Instant::now() + WAIT;
    loop {
        let path = format!("/decentralized-parties?prefix={prefix}&refresh=true");
        let parties: DecentralizedPartiesResponse = f.get_json(f.p1.http, &path).await?;
        if let Some(party) = parties
            .parties
            .into_iter()
            .find(|p| p.party_id.prefix == prefix)
        {
            ensure!(
                party.participants.len() == 3,
                "liveness party needs three hosts"
            );
            ensure!(
                party.threshold == 2,
                "liveness party needs namespace threshold 2"
            );
            return Ok(party.party_id.to_string());
        }
        ensure!(
            Instant::now() < deadline,
            "new liveness party was not discovered"
        );
        sleep(Duration::from_secs(1)).await;
    }
}

async fn deploy_rules(f: &Fixture, party: &str, timeout_micros: i64) -> anyhow::Result<String> {
    let before = ids(f, f.p1.http, party, RULES).await?;
    let members = [
        f.p1_member_party()?,
        f.p2_member_party()?,
        f.p3_member_party()?,
    ];
    let request = json!({
        "decentralized_party_id": party,
        "participant_ids": [&f.p1.participant_id, &f.p2.participant_id, &f.p3.participant_id],
        "participant_parties": members,
        "operator_party": members[0],
        "contracts": [{
            "id": format!("v2-liveness-rules-{timeout_micros}"),
            "name": "GovernanceRules",
            "package_id": "#governance-core-v2",
            "module_name": "Governance.Rules",
            "entity_name": "GovernanceRules",
            "fields": [
                {"type": "decentralized_party"},
                {"type": "party_set", "parties": members},
                {"type": "int64", "value": 2},
                {"type": "rel_time", "microseconds": timeout_micros},
                {"type": "none"},
                {"type": "none"},
            ],
        }],
    });
    let _: Value = f.post_json(f.p1.http, "/contracts", &request).await?;
    let run = db::latest_coordinator_instance(&f.db_path(1), "Contracts")
        .await?
        .context("v2 rules deploy did not leave a coordinator run")?;
    accept_and_wait(f, &run, "/contracts/status").await?;
    new_id(f, party, RULES, &before).await
}

async fn proposal(f: &Fixture, party: &str, member: &str) -> anyhow::Result<String> {
    submit_new(
        f,
        P1_JSON_API,
        member,
        party,
        GENERIC_VOTE,
        json!({"CreateCommand": {
            "templateId": template(GENERIC_VOTE),
            "createArguments": {
                "governanceParty": party,
                "proposer": member,
                "description": format!("offline confirmer liveness {}", uuid::Uuid::new_v4()),
            },
        }}),
    )
    .await
}

fn action() -> Value {
    json!({"tag": "SelfAction_SetThreshold", "value": {"updatedThreshold": "2"}})
}

async fn confirm(
    f: &Fixture,
    port: u16,
    member: &str,
    party: &str,
    rules: &str,
    proposal: Option<&str>,
) -> anyhow::Result<String> {
    let (choice, arguments, created) = match proposal {
        Some(cid) => (
            "GovernanceRules_ConfirmAction",
            json!({"confirmer": member, "actionProposalCid": cid}),
            DOMAIN_CONFIRMATION,
        ),
        None => (
            "GovernanceRules_ConfirmGovernanceAction",
            json!({"confirmer": member, "action": action()}),
            SELF_CONFIRMATION,
        ),
    };
    submit_new(
        f,
        port,
        member,
        party,
        created,
        exercise(RULES, rules, choice, arguments),
    )
    .await
}

async fn prepare(f: &Fixture) -> anyhow::Result<Votes> {
    let dar_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../daml/governance-core-v2/.daml/dist/governance-core-v2-0.1.0.dar");
    let dar = tokio::fs::read(&dar_path)
        .await
        .with_context(|| format!("read CI-built fixture {}", dar_path.display()))?;
    let request = json!({"dar_files": [{
        "filename": "governance-core-v2-0.1.0.dar", "data": STANDARD.encode(dar),
    }]});
    for port in [f.p1.http, f.p2.http, f.p3.http] {
        let _: Value = f.post_json(port, "/dars/upload", &request).await?;
    }
    let party = create_party(f).await?;
    configure_party_on_nodes(f, &party).await?;
    let member = f.p1_member_party()?.to_string();
    let offline_member = f.p3_member_party()?.to_string();
    let p1 = admin_config(f, 1)?;
    let synchronizer_id = utils::get_synchronizer_id(&p1).await?;
    let (_, governance_hosting) =
        fetch_p2p_mapping_at_head(&p1, &synchronizer_id, &CantonId::parse(&party)?).await?;
    ensure!(
        governance_hosting.threshold == 2,
        "governance party needs hosting quorum 2"
    );
    let (_, confirmer_hosting) =
        fetch_p2p_mapping_at_head(&p1, &synchronizer_id, &CantonId::parse(&offline_member)?)
            .await?;
    ensure!(
        confirmer_hosting.participants.len() == 1
            && confirmer_hosting.participants[0].participant_uid == f.p3.participant_id,
        "the confirmer must be hosted only on the participant being disconnected"
    );
    let long_rules = deploy_rules(f, &party, 1_800_000_000).await?;
    let proposal = proposal(f, &party, &member).await?;
    let mut self_votes = Vec::new();
    let mut domain_votes = Vec::new();
    for (port, actor) in [(P1_JSON_API, &member), (P3_JSON_API, &offline_member)] {
        self_votes.push(confirm(f, port, actor, &party, &long_rules, None).await?);
        domain_votes.push(confirm(f, port, actor, &party, &long_rules, Some(&proposal)).await?);
    }
    // A separate short-lived rules fixture avoids racing the consume votes' expiry.
    let short_rules = deploy_rules(f, &party, 10_000_000).await?;
    let expired_self = confirm(f, P3_JSON_API, &offline_member, &party, &short_rules, None).await?;
    let expired_domain = confirm(
        f,
        P3_JSON_API,
        &offline_member,
        &party,
        &short_rules,
        Some(&proposal),
    )
    .await?;
    let all_votes: HashSet<_> = self_votes
        .iter()
        .chain(&domain_votes)
        .chain([&expired_self, &expired_domain])
        .cloned()
        .collect();
    let deadline = Instant::now() + WAIT;
    loop {
        let mut visible_on_both = true;
        for port in [P1_JSON_API, P2_JSON_API] {
            let contracts = f.active_contracts(port, &party).await?;
            let votes: Vec<_> = contracts
                .iter()
                .filter(|c| all_votes.contains(&c.contract_id))
                .collect();
            visible_on_both &= votes.len() == all_votes.len();
            for vote in votes {
                ensure!(
                    vote.stakeholders == [party.clone()],
                    "confirmation {} has extra stakeholders",
                    vote.contract_id
                );
            }
        }
        if visible_on_both {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "confirmation votes did not reach both online hosts"
        );
        sleep(Duration::from_millis(500)).await;
    }
    Ok(Votes {
        party,
        member,
        long_rules,
        short_rules,
        self_votes,
        domain_votes,
        proposal,
        expired_self,
        expired_domain,
    })
}

async fn registered(config: &NodeConfig) -> anyhow::Result<SynchronizerConnectionConfig> {
    let response = SynchronizerConnectivityServiceClient::new(config.admin_channel().await?)
        .list_registered_synchronizers(tonic::Request::new(ListRegisteredSynchronizersRequest {
            all_statuses: false,
        }))
        .await?
        .into_inner();
    response
        .results
        .into_iter()
        .filter_map(|r| r.config)
        .find(|c| c.synchronizer_alias == config.synchronizer())
        .context("P3 synchronizer is not registered")
}

async fn modify(
    config: &NodeConfig,
    connection: SynchronizerConnectionConfig,
) -> anyhow::Result<()> {
    SynchronizerConnectivityServiceClient::new(config.admin_channel().await?)
        .modify_synchronizer(tonic::Request::new(ModifySynchronizerRequest {
            physical_synchronizer_id: None,
            new_config: Some(connection),
            sequencer_connection_validation: SequencerConnectionValidation::Disabled as i32,
        }))
        .await?;
    Ok(())
}

async fn assert_disconnected(config: &NodeConfig) -> anyhow::Result<()> {
    let response = SynchronizerConnectivityServiceClient::new(config.admin_channel().await?)
        .list_connected_synchronizers(tonic::Request::new(ListConnectedSynchronizersRequest {}))
        .await?
        .into_inner();
    ensure!(
        response.connected_synchronizers.is_empty(),
        "P3 reconnected during the liveness check"
    );
    ensure!(
        registered(config).await?.manual_connect,
        "P3 is not held on manual connection"
    );
    Ok(())
}

async fn archive_while_offline(
    f: &Fixture,
    config: &NodeConfig,
    votes: &Votes,
) -> anyhow::Result<()> {
    let mut connection = registered(config).await?;
    connection.manual_connect = true;
    modify(config, connection).await?;
    SynchronizerConnectivityServiceClient::new(config.admin_channel().await?)
        .disconnect_synchronizer(tonic::Request::new(DisconnectSynchronizerRequest {
            synchronizer_alias: config.synchronizer().to_string(),
        }))
        .await?;
    assert_disconnected(config).await?;

    info!("WHEN P1 consumes self confirmations, including the offline member's vote");
    let new_rules = submit_new(
        f,
        P1_JSON_API,
        &votes.member,
        &votes.party,
        RULES,
        exercise(
            RULES,
            &votes.long_rules,
            "GovernanceRules_ExecuteGovernanceAction",
            json!({
                "executor": votes.member, "action": action(), "confirmations": votes.self_votes,
            }),
        ),
    )
    .await?;
    assert_disconnected(config).await?;

    info!("WHEN P1 consumes domain confirmations with an online proposer");
    let operation = exercise(
        RULES,
        &new_rules,
        "GovernanceRules_ExecuteConfirmedAction",
        json!({
            "executor": votes.member, "actionProposalCid": votes.proposal,
            "confirmations": votes.domain_votes,
        }),
    );
    timeout(
        WAIT,
        f.submit_create(
            P1_JSON_API,
            &command(&votes.member, &votes.party, operation),
        ),
    )
    .await
    .context("domain execution waited for the offline confirmer")??;
    assert_disconnected(config).await?;

    // The last short confirmation was committed before disconnect; strictly
    // more than ten seconds must pass before its Expire choice is eligible.
    sleep(Duration::from_secs(11)).await;
    for (choice, cid) in [
        (
            "GovernanceRules_ExpireGovernanceSelfConfirmation",
            &votes.expired_self,
        ),
        ("GovernanceRules_ExpireConfirmation", &votes.expired_domain),
    ] {
        info!("WHEN P1 archives {choice} while P3 remains disconnected");
        let operation = exercise(
            RULES,
            &votes.short_rules,
            choice,
            json!({
                "member": votes.member, "staleConfirmationCid": cid,
            }),
        );
        timeout(
            WAIT,
            f.submit_create(
                P1_JSON_API,
                &command(&votes.member, &votes.party, operation),
            ),
        )
        .await
        .with_context(|| format!("{choice} waited for the offline confirmer"))??;
        assert_disconnected(config).await?;
    }
    info!("THEN both online hosts have archived all six confirmations");
    let targets: HashSet<_> = votes
        .self_votes
        .iter()
        .chain(&votes.domain_votes)
        .chain([&votes.expired_self, &votes.expired_domain])
        .cloned()
        .collect();
    let deadline = Instant::now() + WAIT;
    loop {
        let mut archived_on_both = true;
        for port in [P1_JSON_API, P2_JSON_API] {
            let contracts = f.active_contracts(port, &votes.party).await?;
            archived_on_both &= contracts.iter().all(|c| !targets.contains(&c.contract_id));
        }
        if archived_on_both {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "archived confirmations remain in an online ACS"
        );
        sleep(Duration::from_millis(500)).await;
    }
    assert_disconnected(config).await
}

async fn restore(
    config: &NodeConfig,
    original: SynchronizerConnectionConfig,
) -> anyhow::Result<()> {
    let restore_config = modify(config, original).await;
    let reconnect = SynchronizerConnectivityServiceClient::new(config.admin_channel().await?)
        .reconnect_synchronizer(tonic::Request::new(ReconnectSynchronizerRequest {
            synchronizer_alias: config.synchronizer().to_string(),
            retry: true,
        }))
        .await;
    restore_config.context("restore P3's original synchronizer configuration")?;
    reconnect.context("reconnect P3 after the liveness check")?;
    let deadline = Instant::now() + WAIT;
    loop {
        let response = SynchronizerConnectivityServiceClient::new(config.admin_channel().await?)
            .list_connected_synchronizers(tonic::Request::new(ListConnectedSynchronizersRequest {}))
            .await?
            .into_inner();
        if response
            .connected_synchronizers
            .iter()
            .any(|s| s.healthy && s.synchronizer_alias == config.synchronizer())
        {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "P3 did not reconnect healthily after the liveness check"
        );
        sleep(Duration::from_secs(1)).await;
    }
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    ensure!(
        f.target == TestTarget::Localnet,
        "participant disconnect is localnet-only"
    );
    info!("GIVEN a fresh 2-of-3 party and real confirmations made by P3's member");
    let votes = prepare(f).await?;
    let config = admin_config(f, 3)?;
    let original = registered(&config).await?;
    // Cleanup is outside the timed body, so a failed command or assertion
    // reconnects Canton before the failure returns to the suite.
    let outcome = timeout(
        Duration::from_secs(300),
        archive_while_offline(f, &config, &votes),
    )
    .await
    .context("offline confirmer liveness scenario exceeded five minutes")
    .and_then(|outcome| outcome);
    let restored = timeout(Duration::from_secs(90), restore(&config, original))
        .await
        .context("P3 recovery exceeded ninety seconds")
        .and_then(|outcome| outcome);
    match (outcome, restored) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("P3 recovery also failed: {cleanup:#}")))
        }
    }
}
