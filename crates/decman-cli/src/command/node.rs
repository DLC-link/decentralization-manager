//! The commands for the node, its peers, packages, DARs, invitations,
//! workflows, contracts, party config, auth and ledger reads.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use common::api::GrantRightsRequest;
use reqwest::Method;
use serde_json::{Value, json};

use super::args::{ApiArgs, AuthCommand, ContractsCommand, DarsCommand, InvitationsCommand};
use super::args::{LedgerCommand, NodeCommand, PackagesCommand, PartyArg, PartyConfigCommand};
use super::args::{PeersCommand, WorkflowsCommand};
use super::credentials::with_profile_secret;
use super::credentials::{ADMIN_SECRET_ENV, admin_credentials, credential_fields};
use super::credentials::{ensure_writes_allowed, party_config_body, party_credentials};
use super::http::{get, optional, peer_list, post, segment, to_body};
use super::parties::{canton_id, resolve_party, start_workflow};
use super::wait::{RunSelector, WaitOptions, wait_for_run};
use super::{NodeSettings, Outcome, contracts, input};
use crate::api::DecmanClient;

pub fn run_api(client: &mut DecmanClient, args: &ApiArgs) -> Result<Outcome> {
    let method = Method::from_bytes(args.method.to_ascii_uppercase().as_bytes())
        .with_context(|| format!("{:?} is not an HTTP method", args.method))?;
    let path = if args.path.starts_with('/') {
        args.path.clone()
    } else {
        format!("/{}", args.path)
    };
    let query = args
        .queries
        .iter()
        .map(|pair| input::query_pair(pair))
        .collect::<Result<Vec<_>>>()?;
    let body = args.body.as_deref().map(input::read_json).transpose()?;
    Ok(Outcome::ok(client.request(
        method,
        &path,
        &query,
        body.as_ref(),
    )?))
}

pub fn run_node(client: &mut DecmanClient, command: &NodeCommand) -> Result<Outcome> {
    let path = match command {
        NodeCommand::Config => "/node-config",
        NodeCommand::Health => "/node-health",
        NodeCommand::Keys => "/keys/status",
        NodeCommand::Operator => "/operator-info",
        NodeCommand::NetworkInfo => "/network-info",
    };
    get(client, path, &[])
}

pub fn run_peers(client: &mut DecmanClient, command: PeersCommand) -> Result<Outcome> {
    match command {
        PeersCommand::Status => get(client, "/participants-status", &[]),
        PeersCommand::Config => get(client, "/network-config", &[]),
        PeersCommand::SetConfig { body } => {
            let peers = peer_list(input::read_json(&body)?);
            post(client, "/network-config", Some(&peers))
        }
    }
}

pub fn run_packages(client: &mut DecmanClient, command: PackagesCommand) -> Result<Outcome> {
    match command {
        PackagesCommand::Vetted => get(client, "/packages/vetted", &[]),
        PackagesCommand::Expected => get(client, "/packages/expected-versions", &[]),
        PackagesCommand::Compare { participants } => {
            let mut query = Vec::new();
            if !participants.is_empty() {
                query.push(("participants", participants.join(",")));
            }
            get(client, "/packages/compare-peers", &query)
        }
        PackagesCommand::Config { party } => {
            let party = resolve_party(client, &party)?;
            get(client, "/packages", &[("party_id", party.id)])
        }
    }
}

pub fn run_dars(client: &mut DecmanClient, command: DarsCommand) -> Result<Outcome> {
    match command {
        DarsCommand::Upload { files } => {
            let body = json!({ "dar_files": input::dar_files(&files)?, "peer_ids": [] });
            post(client, "/dars/upload", Some(&body))
        }
        DarsCommand::Distribute { files, peers, wait } => {
            let body = json!({ "dar_files": input::dar_files(&files)?, "peer_ids": peers });
            start_workflow(client, "/dars/distribute", &body, &wait)
        }
        DarsCommand::DistributePackage {
            package_id,
            peers,
            wait,
        } => {
            let body = json!({ "package_id": package_id, "peer_ids": peers });
            start_workflow(client, "/dars/distribute-package", &body, &wait)
        }
    }
}

pub fn run_invitations(client: &mut DecmanClient, command: InvitationsCommand) -> Result<Outcome> {
    match command {
        InvitationsCommand::List => get(client, "/invitations", &[]),
        InvitationsCommand::Handled { limit } => {
            let mut query = Vec::new();
            optional(&mut query, "limit", limit.map(|limit| limit.to_string()));
            get(client, "/invitations/handled", &query)
        }
        InvitationsCommand::Accept { id, wait } => {
            // Read the invitation first: after the accept it is gone, and the
            // wait needs its coordinator run to find this node's peer run.
            let selector = if wait.wait {
                Some(peer_run_selector(client, &id)?)
            } else {
                None
            };
            let response = client.request(
                Method::POST,
                "/invitations/accept",
                &[],
                Some(&json!({ "id": id })),
            )?;
            match selector {
                Some(selector) => wait_for_run(client, &selector, wait.options()),
                None => Ok(Outcome::ok(response)),
            }
        }
        InvitationsCommand::Decline { id } => {
            post(client, "/invitations/decline", Some(&json!({ "id": id })))
        }
        InvitationsCommand::Dar { id, index, out } => {
            let bytes =
                client.request_bytes(&format!("/invitations/{}/dars/{index}", segment(&id)?))?;
            std::fs::write(&out, &bytes)
                .with_context(|| format!("failed to write {}", out.display()))?;
            Ok(Outcome::ok(json!({ "file": out, "bytes": bytes.len() })))
        }
    }
}

/// The selector for the peer run that accepting invitation `id` starts.
fn peer_run_selector(client: &mut DecmanClient, id: &str) -> Result<RunSelector> {
    let response = client.request(Method::GET, "/invitations", &[], None)?;
    let invitation = response
        .get("invitations")
        .and_then(Value::as_array)
        .and_then(|invitations| invitations.iter().find(|invitation| invitation["id"] == id))
        .with_context(|| format!("no pending invitation has id {id}"))?;
    let field = |key: &str| {
        invitation[key]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .with_context(|| format!("invitation {id} has no {key}, so --wait cannot find its run"))
    };
    Ok(RunSelector::PeerOf {
        coordinator_instance: field("workflow_instance")?,
        coordinator_pubkey: field("coordinator_pubkey")?,
    })
}

pub fn run_workflows(client: &mut DecmanClient, command: WorkflowsCommand) -> Result<Outcome> {
    match command {
        WorkflowsCommand::List => get(client, "/workflows", &[]),
        WorkflowsCommand::Get { name } => {
            let response = client.request(Method::GET, "/workflows", &[], None)?;
            let run = response
                .get("runs")
                .and_then(Value::as_array)
                .and_then(|runs| {
                    runs.iter()
                        .find(|run| run["instance_name"] == name.as_str())
                })
                .with_context(|| format!("workflow run {name} is not in /workflows"))?;
            Ok(Outcome::ok(run.clone()))
        }
        WorkflowsCommand::Wait {
            name,
            timeout,
            poll_interval,
        } => {
            let options = WaitOptions {
                timeout: timeout.map(Duration::from_secs),
                interval: Duration::from_secs(poll_interval.max(1)),
            };
            wait_for_run(client, &RunSelector::Instance(name), options)
        }
        WorkflowsCommand::Cancel { name } => post(
            client,
            &format!("/workflows/{}/cancel", segment(&name)?),
            None,
        ),
        WorkflowsCommand::Dismiss { name } => post(
            client,
            &format!("/workflows/{}/dismiss", segment(&name)?),
            None,
        ),
        WorkflowsCommand::Retry { name, wait } => {
            let response = client.request(
                Method::POST,
                &format!("/workflows/{}/retry", segment(&name)?),
                &[],
                None,
            )?;
            if wait.wait {
                wait_for_run(client, &RunSelector::Instance(name), wait.options())
            } else {
                Ok(Outcome::ok(response))
            }
        }
    }
}

pub fn run_contracts(client: &mut DecmanClient, command: ContractsCommand) -> Result<Outcome> {
    match command {
        ContractsCommand::Deploy { body, wait } => {
            start_workflow(client, "/contracts", &input::read_json(&body)?, &wait)
        }
        ContractsCommand::DeployGovernance {
            party,
            threshold,
            confirmation_timeout_micros,
            members,
            extra_proposers,
            disclosure_observers,
            operator_party,
            dry_run,
            wait,
        } => {
            let overrides = members
                .iter()
                .map(|member| input::query_pair(member))
                .collect::<Result<Vec<_>>>()?;
            let party = resolve_party(client, &party)?;
            if party.participants.is_empty() {
                bail!("this node does not list {} with its participants", party.id);
            }
            let context = contracts::fetch_context(
                client,
                &party.id,
                &party.participants,
                &overrides,
                operator_party,
            )?;
            let input = contracts::GovernanceCoreInput {
                threshold,
                confirmation_timeout_micros,
                extra_proposers,
                disclosure_observers,
            };
            let body = contracts::body(&party.id, &context, &input)?;
            if dry_run {
                return Ok(Outcome::ok(body));
            }
            start_workflow(client, "/contracts", &body, &wait)
        }
    }
}

pub fn run_party_config(
    client: &mut DecmanClient,
    command: PartyConfigCommand,
    settings: &NodeSettings,
) -> Result<Outcome> {
    match command {
        PartyConfigCommand::Get { party } => {
            let party = resolve_party(client, &party)?;
            get(
                client,
                &format!("/party-config/{}", segment(&party.id)?),
                &[],
            )
        }
        PartyConfigCommand::Set {
            party,
            body,
            keep_secret,
        } => {
            ensure_writes_allowed(client)?;
            let body = match (body, party) {
                (Some(body), _) => input::read_json(&body)?,
                (None, Some(party)) => {
                    let credentials = party_credentials(settings)?;
                    let party = resolve_party(client, &PartyArg { party })?;
                    party_config_body(&party.id, credentials)
                }
                (None, None) => bail!("pass --party or --body"),
            };
            let body = if keep_secret {
                body
            } else {
                with_profile_secret(body, settings)?
            };
            Ok(Outcome::ok(client.request(
                Method::PUT,
                "/party-config",
                &[],
                Some(&body),
            )?))
        }
        PartyConfigCommand::DiscoverMemberParty { body } => {
            ensure_writes_allowed(client)?;
            let body = match body {
                Some(body) => input::read_json(&body)?,
                None => Value::Object(credential_fields(party_credentials(settings)?)),
            };
            let body = with_profile_secret(body, settings)?;
            post(client, "/party-config/discover-member-party", Some(&body))
        }
    }
}

pub fn run_auth(
    client: &mut DecmanClient,
    command: AuthCommand,
    settings: &NodeSettings,
) -> Result<Outcome> {
    match command {
        AuthCommand::Status => get(client, "/auth/status", &[]),
        AuthCommand::Test => post(client, "/auth/test", None),
        AuthCommand::GrantRights {
            party,
            admin_client_id,
        } => {
            ensure_writes_allowed(client)?;
            let (admin_client_id, admin_client_secret) = admin_credentials(
                admin_client_id,
                settings.admin_client.as_ref(),
                std::env::var(ADMIN_SECRET_ENV).ok(),
            )?;
            let party = resolve_party(client, &party)?;
            let body = to_body(&GrantRightsRequest {
                dec_party_id: canton_id(&party.id)?,
                admin_client_id,
                admin_client_secret,
            })?;
            post(client, "/auth/grant-rights", Some(&body))
        }
    }
}

pub fn run_ledger(client: &mut DecmanClient, command: LedgerCommand) -> Result<Outcome> {
    match command {
        LedgerCommand::List {
            resource,
            party,
            limit,
            cursor,
        } => {
            let party = resolve_party(client, &party)?;
            let mut query = vec![("party_id", party.id)];
            optional(&mut query, "limit", limit.map(|limit| limit.to_string()));
            optional(&mut query, "cursor", cursor);
            get(client, resource.path(), &query)
        }
        LedgerCommand::Query {
            party,
            package_id,
            module,
            entity,
            interface,
        } => {
            let party = resolve_party(client, &party)?;
            let query = vec![
                ("party_id", party.id),
                ("package_id", package_id),
                ("module_name", module),
                ("entity_name", entity),
                ("interface", interface.to_string()),
            ];
            get(client, "/contracts/query", &query)
        }
    }
}
