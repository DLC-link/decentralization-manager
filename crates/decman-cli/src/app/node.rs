//! Node actions: invitations, workflows, network edit, auth, party config,
//! grant rights and DARs.

use base64::prelude::*;

use common::types::{WorkflowProgress, WorkflowRole};

use crate::api::FeedItem;

use crate::app::forms::{discover_body, party_config_body, peers_to_json};
use crate::app::overlay::{Dar, GrantForm, Overlay, PeerChoice};
use crate::app::state::App;
use crate::app::worker::Request;

impl App {
    /// Accept (`accept = true`) or decline the selected invitation.
    pub(super) fn invitation_action(&mut self, accept: bool) {
        let id = match self.selected_feed_item() {
            Some(FeedItem::Invitation(invitation)) => invitation.id.clone(),
            _ => return,
        };
        let request = if accept {
            Request::Accept(id)
        } else {
            Request::Decline(id)
        };
        let _ = self.requests.send(request);
        let verb = if accept { "Accepting" } else { "Declining" };
        self.overlay = Overlay::Busy(format!("{verb} invitation…"));
    }

    /// Dismiss the selected finished workflow run.
    pub(super) fn dismiss_run(&mut self) {
        let name = match self.selected_feed_item() {
            Some(FeedItem::Run(run)) => run.instance_name.clone(),
            _ => return,
        };
        let _ = self.requests.send(Request::Dismiss(name));
        self.overlay = Overlay::Busy("Dismissing workflow…".to_owned());
    }

    /// Cancel the selected workflow run (only valid for an in-progress run this
    /// node coordinates).
    pub(super) fn cancel_run(&mut self) {
        let kind = match self.selected_feed_item() {
            Some(FeedItem::Run(run))
                if run.status == WorkflowProgress::InProgress
                    && run.role == WorkflowRole::Coordinator =>
            {
                run.kind
            }
            Some(FeedItem::Run(_)) => {
                self.overlay = Overlay::Message(
                    "Only in-progress workflows you coordinate can be cancelled.".to_owned(),
                );
                return;
            }
            _ => return,
        };
        let _ = self.requests.send(Request::CancelWorkflow(kind));
        self.overlay = Overlay::Busy("Cancelling workflow…".to_owned());
    }

    /// Retry the selected workflow run (only valid for a failed run this node
    /// coordinates).
    pub(super) fn retry_run(&mut self) {
        let name = match self.selected_feed_item() {
            Some(FeedItem::Run(run))
                if run.status == WorkflowProgress::Failed
                    && run.role == WorkflowRole::Coordinator =>
            {
                run.instance_name.clone()
            }
            Some(FeedItem::Run(_)) => {
                self.overlay = Overlay::Message(
                    "Only failed workflows you coordinate can be retried.".to_owned(),
                );
                return;
            }
            _ => return,
        };
        let _ = self.requests.send(Request::RetryWorkflow(name));
        self.overlay = Overlay::Busy("Retrying workflow…".to_owned());
    }

    /// Fetch the peer configuration and open the network-config editor.
    pub(super) fn open_network_edit(&mut self) {
        let _ = self.requests.send(Request::NetworkPeers);
        self.overlay = Overlay::Busy("Loading network configuration…".to_owned());
    }

    /// Fetch and show this node's key status and DSO network identity.
    pub(super) fn open_node_info(&mut self) {
        let _ = self.requests.send(Request::NodeInfo);
        self.overlay = Overlay::Busy("Loading node info…".to_owned());
    }

    /// Save the edited peer list back to the node.
    pub(super) fn save_network(&mut self) {
        let body = {
            let Overlay::NetworkEdit(state) = &self.overlay else {
                return;
            };
            peers_to_json(&state.peers)
        };
        let _ = self.requests.send(Request::SaveNetwork(Box::new(body)));
        self.overlay = Overlay::Busy("Saving network configuration…".to_owned());
    }

    /// Fetch and show the per-party authentication status overlay.
    pub(super) fn open_auth(&mut self) {
        let _ = self.requests.send(Request::AuthStatus);
        self.overlay = Overlay::Busy("Loading authentication status…".to_owned());
    }

    /// Re-test authentication for all parties, then refresh the auth overlay.
    pub(super) fn run_auth_test(&mut self) {
        let _ = self.requests.send(Request::TestAuth);
        self.overlay = Overlay::Busy("Testing authentication…".to_owned());
    }

    /// Open the IdP-config editor for the party selected in the auth overlay.
    pub(super) fn open_party_config(&mut self) {
        let dec_party_id = {
            let Overlay::Auth { parties, selected } = &self.overlay else {
                return;
            };
            match parties.get(*selected) {
                Some(party) => party.dec_party_id.clone(),
                None => return,
            }
        };
        let _ = self.requests.send(Request::PartyConfig(dec_party_id));
        self.overlay = Overlay::Busy("Loading party configuration…".to_owned());
    }

    /// Validate and save the party-config form.
    pub(super) fn submit_party_config(&mut self) {
        let body = {
            let Overlay::PartyConfig(form) = &self.overlay else {
                return;
            };
            if form.member_party_id.trim().is_empty() || form.user_id.trim().is_empty() {
                self.overlay =
                    Overlay::Message("Member party id and user id are required.".to_owned());
                return;
            }
            party_config_body(form)
        };
        let _ = self.requests.send(Request::SavePartyConfig(Box::new(body)));
        self.overlay = Overlay::Busy("Saving party configuration…".to_owned());
    }

    /// Discover the member party from the form's current IdP credentials. The
    /// form stays open (with a status line) so the result patches into it.
    pub(super) fn run_discover(&mut self) {
        let Overlay::PartyConfig(form) = &mut self.overlay else {
            return;
        };
        let body = discover_body(form);
        form.status = "Discovering member party…".to_owned();
        let _ = self.requests.send(Request::Discover(Box::new(body)));
    }

    /// Open the grant-rights form for the party selected in the auth overlay.
    pub(super) fn open_grant(&mut self) {
        let form = {
            let Overlay::Auth { parties, selected } = &self.overlay else {
                return;
            };
            parties.get(*selected).map(|party| GrantForm {
                dec_party_id: party.dec_party_id.clone(),
                party_name: party
                    .dec_party_id
                    .split("::")
                    .next()
                    .unwrap_or(&party.dec_party_id)
                    .to_owned(),
                client_id: String::new(),
                client_secret: String::new(),
                cursor: 0,
            })
        };
        if let Some(form) = form {
            self.overlay = Overlay::GrantRights(Box::new(form));
        }
    }

    /// Submit the grant-rights form, then refresh the auth overlay.
    pub(super) fn submit_grant(&mut self) {
        let request = {
            let Overlay::GrantRights(form) = &self.overlay else {
                return;
            };
            if form.client_id.trim().is_empty() || form.client_secret.is_empty() {
                None
            } else {
                Some(Request::GrantRights {
                    dec_party_id: form.dec_party_id.clone(),
                    client_id: form.client_id.trim().to_owned(),
                    client_secret: form.client_secret.clone(),
                })
            }
        };
        match request {
            Some(request) => {
                let _ = self.requests.send(request);
                self.overlay = Overlay::Busy("Granting rights…".to_owned());
            }
            None => {
                self.overlay =
                    Overlay::Message("Admin client id and secret are required.".to_owned());
            }
        }
    }

    /// Kick off the package checker.
    pub(super) fn start_compare(&mut self) {
        let _ = self.requests.send(Request::Compare);
        self.overlay = Overlay::Busy("Comparing packages with peers…".to_owned());
    }

    /// Pick a DAR and upload it to this node.
    pub(super) fn start_upload(&mut self) {
        if let Some(dar) = self.pick_dar() {
            let _ = self.requests.send(Request::Upload {
                filename: dar.filename,
                data: dar.data,
            });
            self.overlay = Overlay::Busy("Uploading DAR…".to_owned());
        }
    }

    /// Pick a DAR, then open the peer-selection dialog for distribution.
    pub(super) fn start_distribute(&mut self) {
        let Some(dar) = self.pick_dar() else {
            return;
        };
        let peers: Vec<PeerChoice> = self
            .peers
            .iter()
            .filter(|peer| !peer.is_self)
            .map(|peer| PeerChoice {
                id: peer.participant_id.clone(),
                name: peer.name.clone(),
                checked: false,
            })
            .collect();
        if peers.is_empty() {
            self.overlay = Overlay::Message("No peers available to distribute to.".to_owned());
            return;
        }
        self.overlay = Overlay::PeerSelect {
            dar,
            peers,
            cursor: 0,
        };
    }

    /// Send the distribution request for the checked peers in the dialog.
    pub(super) fn confirm_distribute(&mut self) {
        let Overlay::PeerSelect { dar, peers, .. } = &self.overlay else {
            return;
        };
        let peer_ids: Vec<String> = peers
            .iter()
            .filter(|peer| peer.checked)
            .map(|peer| peer.id.clone())
            .collect();
        if peer_ids.is_empty() {
            return;
        }
        let (filename, data) = (dar.filename.clone(), dar.data.clone());
        let _ = self.requests.send(Request::Distribute {
            filename,
            data,
            peer_ids,
        });
        self.overlay = Overlay::Busy("Distributing DAR…".to_owned());
    }

    /// Open the native file dialog and read the chosen DAR as base64.
    fn pick_dar(&mut self) -> Option<Dar> {
        let path = rfd::FileDialog::new()
            .add_filter("DAR files", &["dar"])
            .pick_file()?;
        match std::fs::read(&path) {
            Ok(bytes) => {
                let filename = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "upload.dar".to_owned());
                Some(Dar {
                    filename,
                    data: BASE64_STANDARD.encode(bytes),
                })
            }
            Err(error) => {
                self.overlay = Overlay::Message(format!("Failed to read DAR: {error}"));
                None
            }
        }
    }
}
