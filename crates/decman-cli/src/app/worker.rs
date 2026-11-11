//! The worker protocol and the background workers that call the API.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

use common::types::{DecentralizedParty, PeerPackageComparison, VettedPackageInfo, WorkflowKind};

use crate::api::{AuthSettings, ChainAuditEntry, DecmanClient, DiscoverResult, ExecuteParams};
use crate::api::{FeedItem, GovConfirmation, GovernanceConfirmations, KnownMember};
use crate::api::{PartyAuthStatus, PeerEntry, PeerView};
use crate::composer::{PickerOption, PickerSource};

use crate::app::forms::build_party_config_form;
use crate::app::overlay::{DeployForm, DetailData, GovActionRequest, GovItem, GovOp, GovView};
use crate::app::overlay::{NodeInfoData, PartyConfigForm};

/// How often the background thread re-probes peers (matches the web frontend).
pub const PEER_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// A request to the background fetcher.
pub enum Request {
    Parties,
    Dars,
    Feed,
    Compare,
    Accept(String),
    Decline(String),
    Dismiss(String),
    Upload {
        filename: String,
        data: String,
    },
    Distribute {
        filename: String,
        data: String,
        peer_ids: Vec<String>,
    },
    Detail(String),
    Onboard {
        prefix: String,
        peer_ids: Vec<String>,
    },
    Kick {
        party_id: String,
        participant_id: String,
        new_threshold: i32,
        previous_threshold: i32,
    },
    AddParty {
        party_id: String,
        participant_id: String,
        new_threshold: i32,
        previous_threshold: i32,
    },
    ChangeThreshold {
        party_id: String,
        new_threshold: i32,
        previous_threshold: i32,
    },
    CancelWorkflow(WorkflowKind),
    RetryWorkflow(String),
    ChainAudit(String),
    AuthStatus,
    TestAuth,
    GrantRights {
        dec_party_id: String,
        client_id: String,
        client_secret: String,
    },
    Governance {
        party_id: String,
        party_name: String,
    },
    GovAction(Box<GovActionRequest>),
    OperatorInfo,
    DeployContext {
        party_id: String,
        party_name: String,
    },
    DeploySubmit(Box<Value>),
    NetworkPeers,
    SaveNetwork(Box<Value>),
    PartyConfig(String),
    SavePartyConfig(Box<Value>),
    Discover(Box<Value>),
    NodeInfo,
    /// Fetch the selectable contracts for the picker fields of an open composer.
    PickerOptions {
        party_id: String,
        sources: Vec<PickerSource>,
    },
}

/// A result delivered by a background worker.
pub enum Update {
    Parties(Result<Vec<DecentralizedParty>, String>),
    Dars(Result<Vec<VettedPackageInfo>, String>),
    Feed(Result<Vec<FeedItem>, String>),
    Peers(Result<Vec<PeerView>, String>),
    Compare(Result<PeerPackageComparison, String>),
    Action(Result<String, String>),
    Detail(DetailData),
    ChainAudit(Result<Vec<ChainAuditEntry>, String>),
    AuthStatus(Result<Vec<PartyAuthStatus>, String>),
    Governance(Result<Box<GovView>, String>),
    OperatorInfo(String),
    DeployContext(Result<Box<DeployForm>, String>),
    NetworkPeers(Result<Vec<PeerEntry>, String>),
    PartyConfig(Result<Box<PartyConfigForm>, String>),
    Discovered(Result<DiscoverResult, String>),
    NodeInfo(Box<NodeInfoData>),
    /// Fetched picker options, keyed by the source they belong to.
    PickerOptions(Vec<(PickerSource, Result<Vec<PickerOption>, String>)>),
}

/// Spawn the background workers. One thread services on-demand requests; a
/// second re-probes peers every `peer_interval`. Both stream results over the
/// shared `Update` channel and exit when the app quits.
pub fn spawn_workers(
    base_url: String,
    auth: AuthSettings,
    peer_interval: Duration,
) -> (Sender<Request>, Receiver<Update>) {
    let (request_tx, request_rx) = mpsc::channel::<Request>();
    let (update_tx, update_rx) = mpsc::channel::<Update>();

    let peer_tx = update_tx.clone();
    let peer_url = base_url.clone();
    let peer_auth = auth.clone();
    thread::spawn(move || {
        let mut client = match DecmanClient::new(peer_url, peer_auth) {
            Ok(client) => client,
            Err(error) => {
                let _ = peer_tx.send(Update::Peers(Err(format!("{error:#}"))));
                return;
            }
        };
        loop {
            // Re-probe peers and refresh the workflows feed on the same cadence
            // the web frontend uses, so live progress and incoming invitations
            // appear without the operator pressing refresh.
            let peers = client.fetch_peers().map_err(|error| format!("{error:#}"));
            if peer_tx.send(Update::Peers(peers)).is_err() {
                break;
            }
            let feed = client.fetch_feed().map_err(|error| format!("{error:#}"));
            if peer_tx.send(Update::Feed(feed)).is_err() {
                break;
            }
            thread::sleep(peer_interval);
        }
    });

    thread::spawn(move || {
        let mut client = match DecmanClient::new(base_url, auth) {
            Ok(client) => client,
            Err(error) => {
                let _ = update_tx.send(Update::Parties(Err(format!("{error:#}"))));
                return;
            }
        };
        while let Ok(request) = request_rx.recv() {
            let update = handle_request(&mut client, request);
            if update_tx.send(update).is_err() {
                break;
            }
        }
    });

    (request_tx, update_rx)
}

/// Run a single request against the client, mapping errors to display strings.
fn handle_request(client: &mut DecmanClient, request: Request) -> Update {
    let err = |error: anyhow::Error| format!("{error:#}");
    match request {
        Request::Parties => Update::Parties(client.fetch_parties().map_err(err)),
        Request::Dars => Update::Dars(client.fetch_dars().map_err(err)),
        Request::Feed => Update::Feed(client.fetch_feed().map_err(err)),
        Request::Compare => Update::Compare(client.compare_packages().map_err(err)),
        Request::Accept(id) => Update::Action(
            client
                .accept_invitation(&id)
                .map(|()| "Invitation accepted".to_owned())
                .map_err(err),
        ),
        Request::Decline(id) => Update::Action(
            client
                .decline_invitation(&id)
                .map(|()| "Invitation declined".to_owned())
                .map_err(err),
        ),
        Request::Dismiss(name) => Update::Action(
            client
                .dismiss_workflow(&name)
                .map(|()| "Workflow dismissed".to_owned())
                .map_err(err),
        ),
        Request::Upload { filename, data } => Update::Action(
            client
                .upload_dar(&filename, &data)
                .map(|()| format!("Uploaded {filename}"))
                .map_err(err),
        ),
        Request::Distribute {
            filename,
            data,
            peer_ids,
        } => Update::Action(
            client
                .distribute_dar(&filename, &data, &peer_ids)
                .map(|()| "Distribution started".to_owned())
                .map_err(err),
        ),
        Request::Detail(party_id) => Update::Detail(DetailData {
            holdings: client.fetch_holdings(&party_id).map_err(err),
            audit: client.fetch_audit(&party_id).map_err(err),
            gov_state: client.fetch_governance_state(&party_id).map_err(err),
        }),
        Request::ChainAudit(party_id) => {
            Update::ChainAudit(client.fetch_chain_audit(&party_id).map_err(err))
        }
        Request::AuthStatus => Update::AuthStatus(client.fetch_auth_status().map_err(err)),
        Request::TestAuth => Update::AuthStatus(
            client
                .test_auth()
                .and_then(|()| client.fetch_auth_status())
                .map_err(err),
        ),
        Request::GrantRights {
            dec_party_id,
            client_id,
            client_secret,
        } => Update::AuthStatus(
            client
                .grant_rights(&dec_party_id, &client_id, &client_secret)
                .and_then(|()| client.fetch_auth_status())
                .map_err(err),
        ),
        Request::Governance {
            party_id,
            party_name,
        } => Update::Governance(
            build_gov_view(client, &party_id, &party_name)
                .map(Box::new)
                .map_err(err),
        ),
        Request::GovAction(request) => {
            let GovActionRequest {
                op,
                party_id,
                party_name,
                rules_contract_id,
            } = *request;
            let result = perform_gov_op(client, &party_id, &rules_contract_id, &op)
                .and_then(|()| build_gov_view(client, &party_id, &party_name))
                .map(Box::new)
                .map_err(err);
            Update::Governance(result)
        }
        Request::OperatorInfo => match client.fetch_operator_info() {
            Ok(party_id) => Update::OperatorInfo(party_id),
            // Operator info is best-effort prefill; swallow errors silently.
            Err(_) => Update::OperatorInfo(String::new()),
        },
        Request::DeployContext {
            party_id,
            party_name,
        } => Update::DeployContext(
            build_deploy_context(client, &party_id, &party_name)
                .map(Box::new)
                .map_err(err),
        ),
        Request::DeploySubmit(body) => Update::Action(
            client
                .deploy_contracts(*body)
                .map(|()| "Contracts workflow started".to_owned())
                .map_err(err),
        ),
        Request::NetworkPeers => Update::NetworkPeers(client.fetch_network_peers().map_err(err)),
        Request::SaveNetwork(body) => Update::Action(
            client
                .save_network_peers(*body)
                .map(|()| "Network configuration saved".to_owned())
                .map_err(err),
        ),
        Request::PartyConfig(dec_party_id) => Update::PartyConfig(
            build_party_config_form(client, &dec_party_id)
                .map(Box::new)
                .map_err(err),
        ),
        Request::SavePartyConfig(body) => Update::Action(
            client
                .save_party_config(*body)
                .map(|()| "Party configuration saved".to_owned())
                .map_err(err),
        ),
        Request::Discover(body) => {
            Update::Discovered(client.discover_member_party(*body).map_err(err))
        }
        Request::NodeInfo => Update::NodeInfo(Box::new(NodeInfoData {
            key: client.fetch_key_status().map_err(err),
            network: client.fetch_network_info().map_err(err),
        })),
        Request::PickerOptions { party_id, sources } => {
            let mut options = Vec::with_capacity(sources.len());
            for source in sources {
                let result = client.fetch_picker_options(source, &party_id).map_err(err);
                options.push((source, result));
            }
            Update::PickerOptions(options)
        }
        Request::Onboard { prefix, peer_ids } => Update::Action(
            client
                .start_onboarding(&prefix, &peer_ids)
                .map(|()| format!("Onboarding started for {prefix}"))
                .map_err(err),
        ),
        Request::Kick {
            party_id,
            participant_id,
            new_threshold,
            previous_threshold,
        } => Update::Action(
            client
                .kick_participant(
                    &party_id,
                    &participant_id,
                    new_threshold,
                    previous_threshold,
                )
                .map(|()| "Kick started".to_owned())
                .map_err(err),
        ),
        Request::AddParty {
            party_id,
            participant_id,
            new_threshold,
            previous_threshold,
        } => Update::Action(
            client
                .start_add_party(
                    &party_id,
                    &participant_id,
                    new_threshold,
                    previous_threshold,
                )
                .map(|()| "Add-party started".to_owned())
                .map_err(err),
        ),
        Request::ChangeThreshold {
            party_id,
            new_threshold,
            previous_threshold,
        } => Update::Action(
            client
                .start_change_threshold(&party_id, new_threshold, previous_threshold)
                .map(|()| "Change-threshold started".to_owned())
                .map_err(err),
        ),
        Request::CancelWorkflow(kind) => Update::Action(
            client
                .cancel_workflow(kind)
                .map(|()| "Workflow cancelled".to_owned())
                .map_err(err),
        ),
        Request::RetryWorkflow(name) => Update::Action(
            client
                .retry_workflow(&name)
                .map(|()| "Workflow retry started".to_owned())
                .map_err(err),
        ),
    }
}

/// Fetch the pending governance confirmations for a party and assemble the
/// approvals view (off-chain actions first, then on-chain proposals).
fn build_gov_view(client: &mut DecmanClient, party_id: &str, party_name: &str) -> Result<GovView> {
    let GovernanceConfirmations {
        actions,
        domain_actions,
        threshold,
        rules_contract_id,
        member_party_id,
        ..
    } = client.fetch_governance(party_id)?;
    // Confirm/execute/propose all need a real rules contract id; bail early with
    // a clear message rather than sending empty ids that fail obscurely later.
    let rules_contract_id = rules_contract_id
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("no governance rules contract found for this party"))?;
    let items = actions
        .into_iter()
        .map(GovItem::OffChain)
        .chain(domain_actions.into_iter().map(GovItem::Domain))
        .collect();
    Ok(GovView {
        party_name: party_name.to_owned(),
        party_id: party_id.to_owned(),
        rules_contract_id,
        // Empty is tolerated: it only weakens "revoke", which already handles
        // not finding this node's own confirmation gracefully.
        member_party_id: member_party_id.unwrap_or_default(),
        threshold,
        items,
        selected: 0,
    })
}

/// Perform one governance mutation against the API.
fn perform_gov_op(
    client: &mut DecmanClient,
    party_id: &str,
    rules_contract_id: &str,
    op: &GovOp,
) -> Result<()> {
    match op {
        GovOp::Confirm {
            action,
            governance_type,
            proposal_cid,
        } => client.confirm_action(
            party_id,
            rules_contract_id,
            action,
            governance_type,
            proposal_cid.as_deref(),
        ),
        GovOp::Execute {
            action,
            confirmation_cids,
            governance_type,
            proposal_cid,
        } => client.execute_action(
            party_id,
            rules_contract_id,
            &ExecuteParams {
                action: action.clone(),
                confirmation_cids: confirmation_cids.clone(),
                governance_type: governance_type.clone(),
                proposal_cid: proposal_cid.clone(),
                disclosed: Vec::new(),
            },
        ),
        GovOp::Cancel {
            confirmation_cid,
            governance_type,
        } => client.cancel_confirmation(party_id, confirmation_cid, governance_type),
        GovOp::Expire {
            confirmation_cid,
            governance_type,
        } => client.expire_confirmation(
            party_id,
            rules_contract_id,
            confirmation_cid,
            governance_type,
        ),
        GovOp::Propose { proposal } => client.propose_action(party_id, rules_contract_id, proposal),
    }
}

/// The confirmation ids for the execute call: the server's live subset, or
/// every confirmation when an older server does not send that subset.
pub(super) fn confirmation_cids(
    executable: Option<&[String]>,
    confirmations: &[GovConfirmation],
) -> Vec<String> {
    executable.map(<[String]>::to_vec).unwrap_or_else(|| {
        confirmations
            .iter()
            .map(|confirmation| confirmation.contract_id.clone())
            .collect()
    })
}

/// Resolve the package id, members and operator needed to deploy governance
/// core for a party, and build the deployment form with sensible defaults.
fn build_deploy_context(
    client: &mut DecmanClient,
    party_id: &str,
    party_name: &str,
) -> Result<DeployForm> {
    let packages = client.fetch_packages(party_id)?;
    let known = client.fetch_known_members(party_id)?;
    // Operator party is best-effort prefill — a deploy can still be sent without.
    let operator_party = client.fetch_operator_info().unwrap_or_default();

    let mut participant_ids = Vec::new();
    let mut member_parties = Vec::new();
    for KnownMember {
        participant_uid,
        member_party_id,
    } in known
    {
        if let Some(party) = member_party_id.filter(|party| !party.is_empty()) {
            participant_ids.push(participant_uid);
            member_parties.push(party);
        }
    }

    // Default threshold ≈ a 2/3 majority, at least 2, but never above the
    // resolved member count so the initial form is always within the valid range.
    let count = i64::try_from(member_parties.len()).unwrap_or(0);
    let threshold = ((count * 2 + 2) / 3).max(2).min(count.max(1));

    Ok(DeployForm {
        party_id: party_id.to_owned(),
        party_name: party_name.to_owned(),
        package_id: packages.governance_core.unwrap_or_default(),
        operator_party,
        participant_ids,
        member_parties,
        threshold: threshold.to_string(),
        timeout_micros: "86400000000".to_owned(),
        cursor: 0,
    })
}

#[cfg(test)]
mod tests {

    use crate::api::GovConfirmation;

    use super::*;

    fn gov_confirmation(contract_id: &str) -> GovConfirmation {
        GovConfirmation {
            contract_id: contract_id.to_owned(),
            confirming_party: String::new(),
            expires_at: 0,
        }
    }

    #[test]
    fn confirmation_cids_use_the_server_executable_set() {
        let confirmations = [gov_confirmation("expired"), gov_confirmation("live")];
        let executable = ["live".to_owned()];

        let cids = confirmation_cids(Some(&executable), &confirmations);

        assert_eq!(cids, vec!["live".to_owned()]);
    }

    #[test]
    fn confirmation_cids_fall_back_to_every_confirmation() {
        let confirmations = [gov_confirmation("a"), gov_confirmation("b")];

        let cids = confirmation_cids(None, &confirmations);

        assert_eq!(cids, vec!["a".to_owned(), "b".to_owned()]);
    }
}
