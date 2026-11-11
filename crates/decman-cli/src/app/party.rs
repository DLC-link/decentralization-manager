//! Party actions: the detail view, onboard, kick, add party and change
//! threshold.

use std::collections::HashSet;

use ratatui::widgets::TableState;

use crate::api::party_name;

use crate::app::forms::validate_prefix;
use crate::app::overlay::{ChangeThresholdForm, KickCandidate, MemberChange, MemberChangeForm};
use crate::app::overlay::{OnboardForm, Overlay, PeerChoice};
use crate::app::state::App;
use crate::app::tabs::filter_parties;
use crate::app::worker::Request;

/// Shown when a form must exclude this node but its identity is not known yet.
const STILL_RESOLVING_SELF: &str = "Still resolving this node's identity — try again in a moment.";

impl App {
    /// Open the selected audit entry's JSON details in a popup modal.
    pub(super) fn open_audit_json(&mut self) {
        let value = self
            .detail_data
            .as_ref()
            .and_then(|data| data.audit.as_ref().ok())
            .and_then(|audit| self.audit_table.selected().and_then(|i| audit.get(i)))
            .map(|entry| entry.details.clone());
        if let Some(value) = value {
            self.overlay = Overlay::Json { value, scroll: 0 };
        }
    }

    /// Open the detail view for the selected party.
    pub(super) fn open_party_detail(&mut self) {
        let party = {
            let filtered = filter_parties(&self.parties, &self.parties_query);
            self.parties_table
                .selected()
                .and_then(|index| filtered.get(index).copied())
                .cloned()
        };
        if let Some(party) = party {
            let _ = self
                .requests
                .send(Request::Detail(party.party_id.to_string()));
            self.detail = Some(party);
            self.detail_data = None;
            self.audit_table = TableState::default();
        }
    }

    /// Open the IdP config / login editor for the party in the detail view.
    pub(super) fn open_party_login(&mut self) {
        if let Some(party) = self.detail.as_ref() {
            let _ = self
                .requests
                .send(Request::PartyConfig(party.party_id.to_string()));
            self.overlay = Overlay::Busy("Loading party configuration…".to_owned());
        }
    }

    /// Open a scrollable detail view for the selected feed item.
    pub(super) fn open_feed_detail(&mut self) {
        if let Some(item) = self.selected_feed_item() {
            self.overlay = Overlay::FeedDetail {
                item: Box::new(item.clone()),
                scroll: 0,
            };
        }
    }

    /// Open the onboarding form, seeded with all configured peers (self
    /// excluded), each ticked by default — matching the web frontend.
    pub(super) fn open_onboard(&mut self) {
        let peers: Vec<PeerChoice> = self
            .peers
            .iter()
            .filter(|peer| !peer.is_self)
            .map(|peer| PeerChoice {
                id: peer.participant_id.clone(),
                name: peer.name.clone(),
                checked: true,
            })
            .collect();
        if peers.is_empty() {
            self.overlay = Overlay::Message(
                "No peers available to onboard with. Configure peers first.".to_owned(),
            );
            return;
        }
        self.overlay = Overlay::Onboard(OnboardForm {
            prefix: String::new(),
            peers,
            cursor: 0,
        });
    }

    /// Validate the onboarding form and send the start request.
    pub(super) fn confirm_onboard(&mut self) {
        let Overlay::Onboard(form) = &self.overlay else {
            return;
        };
        let prefix = form.prefix.trim().to_owned();
        if let Err(message) = validate_prefix(&prefix) {
            self.overlay = Overlay::Message(message);
            return;
        }
        let peer_ids: Vec<String> = form
            .peers
            .iter()
            .filter(|peer| peer.checked)
            .map(|peer| peer.id.clone())
            .collect();
        if peer_ids.is_empty() {
            self.overlay = Overlay::Message("Select at least one peer to onboard.".to_owned());
            return;
        }
        let _ = self.requests.send(Request::Onboard {
            prefix: prefix.clone(),
            peer_ids,
        });
        self.overlay = Overlay::Busy(format!("Starting onboarding for {prefix}…"));
    }

    /// Open the kick form for the party in the detail view, listing the owner
    /// participants that can be removed (self excluded).
    pub(super) fn open_kick(&mut self) {
        let Some(party) = self.detail.as_ref() else {
            return;
        };
        // Require this node's identity to be resolved before listing candidates,
        // otherwise self could not be excluded and might be kicked by mistake.
        let Some(self_id) = self.self_participant_id() else {
            self.overlay = Overlay::Message(STILL_RESOLVING_SELF.to_owned());
            return;
        };
        let candidates: Vec<KickCandidate> = party
            .participants
            .iter()
            .filter(|participant| participant.owner_key.is_some())
            .filter_map(|participant| {
                let id = participant.participant_uid.to_string();
                if id == self_id {
                    return None;
                }
                Some(KickCandidate {
                    participant_id: id,
                    label: participant.participant_uid.prefix.clone(),
                })
            })
            .collect();
        if candidates.is_empty() {
            self.overlay = Overlay::Message("No owner participants available to kick.".to_owned());
            return;
        }
        let owners = i32::try_from(party.owners.len()).unwrap_or(i32::MAX);
        let remaining = (owners - 1).max(0);
        if remaining < 1 {
            self.overlay =
                Overlay::Message("Cannot kick: the party would be left with no owners.".to_owned());
            return;
        }
        let suggested = (((remaining + 1) / 2).max(1)).min(remaining);
        self.overlay = Overlay::MemberChange(MemberChangeForm {
            kind: MemberChange::Kick,
            party_id: party.party_id.to_string(),
            party_name: party_name(party).to_owned(),
            previous_threshold: party.threshold,
            candidates,
            selected: 0,
            new_threshold: suggested,
            max_threshold: remaining,
        });
    }

    /// Open the add-party form for the party in the detail view, listing the
    /// configured peers not already members (self excluded).
    pub(super) fn open_add_party(&mut self) {
        let Some(party) = self.detail.as_ref() else {
            return;
        };
        // Require this node's identity to be resolved so self is excluded.
        let Some(self_id) = self.self_participant_id() else {
            self.overlay = Overlay::Message(STILL_RESOLVING_SELF.to_owned());
            return;
        };
        let members: HashSet<String> = party
            .participants
            .iter()
            .map(|participant| participant.participant_uid.to_string())
            .collect();
        let candidates: Vec<KickCandidate> = self
            .peers
            .iter()
            .filter(|peer| !peer.is_self && peer.participant_id != self_id)
            .filter(|peer| !members.contains(&peer.participant_id))
            .map(|peer| KickCandidate {
                participant_id: peer.participant_id.clone(),
                label: peer.name.clone(),
            })
            .collect();
        if candidates.is_empty() {
            self.overlay = Overlay::Message(
                "No peers left to add — every configured peer is already a member.".to_owned(),
            );
            return;
        }
        // The signing threshold is over the namespace owners (like the kick
        // flow) — not every participant is necessarily an owner. Bound it by the
        // post-add owner count: the current owners plus the joining member.
        let owner_count = i32::try_from(party.owners.len()).unwrap_or(i32::MAX);
        let max_threshold = owner_count.saturating_add(1);
        self.overlay = Overlay::MemberChange(MemberChangeForm {
            kind: MemberChange::Add,
            party_id: party.party_id.to_string(),
            party_name: party_name(party).to_owned(),
            previous_threshold: party.threshold,
            candidates,
            selected: 0,
            new_threshold: party.threshold.clamp(1, max_threshold),
            max_threshold,
        });
    }

    /// Send the kick or add-party request for the selected candidate.
    pub(super) fn confirm_member_change(&mut self) {
        let Overlay::MemberChange(form) = &self.overlay else {
            return;
        };
        let Some(candidate) = form.candidates.get(form.selected) else {
            return;
        };
        let party_id = form.party_id.clone();
        let participant_id = candidate.participant_id.clone();
        let (new_threshold, previous_threshold) = (form.new_threshold, form.previous_threshold);
        let (request, busy) = match form.kind {
            MemberChange::Kick => (
                Request::Kick {
                    party_id,
                    participant_id,
                    new_threshold,
                    previous_threshold,
                },
                format!("Kicking {}…", candidate.label),
            ),
            MemberChange::Add => (
                Request::AddParty {
                    party_id,
                    participant_id,
                    new_threshold,
                    previous_threshold,
                },
                format!("Adding {}…", candidate.label),
            ),
        };
        let _ = self.requests.send(request);
        self.overlay = Overlay::Busy(busy);
    }

    /// This node's participant id, once the peer poll has reported it.
    fn self_participant_id(&self) -> Option<String> {
        self.peers
            .iter()
            .find(|peer| peer.is_self)
            .map(|peer| peer.participant_id.clone())
    }

    /// Open the change-threshold form for the party in the detail view.
    pub(super) fn open_change_threshold(&mut self) {
        let Some(party) = self.detail.as_ref() else {
            return;
        };
        // The signing threshold is over the namespace owners (like the kick
        // flow). A party with fewer than two owners has a fixed threshold of 1
        // and nothing to change (mirrors the server's rejection).
        let owner_count = i32::try_from(party.owners.len()).unwrap_or(i32::MAX);
        if owner_count < 2 {
            self.overlay = Overlay::Message(
                "Cannot change threshold: the party has only one owner.".to_owned(),
            );
            return;
        }
        self.overlay = Overlay::ChangeThreshold(ChangeThresholdForm {
            party_id: party.party_id.to_string(),
            party_name: party_name(party).to_owned(),
            previous_threshold: party.threshold,
            new_threshold: party.threshold.clamp(1, owner_count),
            max_threshold: owner_count,
        });
    }

    /// Send the change-threshold request from the form.
    pub(super) fn confirm_change_threshold(&mut self) {
        let Overlay::ChangeThreshold(form) = &self.overlay else {
            return;
        };
        if form.new_threshold == form.previous_threshold {
            self.overlay =
                Overlay::Message(format!("Threshold is already {}.", form.previous_threshold));
            return;
        }
        let request = Request::ChangeThreshold {
            party_id: form.party_id.clone(),
            new_threshold: form.new_threshold,
            previous_threshold: form.previous_threshold,
        };
        let _ = self.requests.send(request);
        self.overlay = Overlay::Busy("Changing threshold…".to_owned());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver};

    use common::canton_id::CantonId;
    use common::types::{DecentralizedParty, ParticipantInfo, Permission};
    use ratatui::crossterm::event::{KeyCode, KeyEvent};

    use crate::api::PeerView;
    use crate::app::overlay::{KickCandidate, MemberChange, MemberChangeForm, Overlay};
    use crate::app::state::App;
    use crate::app::worker::Request;

    const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

    fn id(prefix: &str) -> CantonId {
        match CantonId::parse(&format!("{prefix}::{NS}")) {
            Ok(id) => id,
            Err(e) => panic!("test id must parse: {e}"),
        }
    }

    fn app() -> (App, Receiver<Request>) {
        let (requests, request_rx) = mpsc::channel();
        let (_updates_tx, updates) = mpsc::channel();
        (App::new(requests, updates, false), request_rx)
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_overlay_key(KeyEvent::from(code));
    }

    fn candidates() -> Vec<KickCandidate> {
        ["alpha", "beta"]
            .into_iter()
            .map(|label| KickCandidate {
                participant_id: format!("{label}::1220"),
                label: label.to_owned(),
            })
            .collect()
    }

    fn member_change_overlay(kind: MemberChange) -> Overlay {
        Overlay::MemberChange(MemberChangeForm {
            kind,
            party_id: "dec::1220".to_owned(),
            party_name: "treasury".to_owned(),
            previous_threshold: 3,
            candidates: candidates(),
            selected: 0,
            new_threshold: 2,
            max_threshold: 3,
        })
    }

    /// The open kick or add-party form.
    fn member_form(app: &App) -> &MemberChangeForm {
        match &app.overlay {
            Overlay::MemberChange(form) => form,
            _ => panic!("expected a kick or add-party form"),
        }
    }

    /// The pick and the new threshold of the open kick or add-party form.
    fn picked(app: &App) -> (usize, i32) {
        let form = member_form(app);
        (form.selected, form.new_threshold)
    }

    /// Check the open form's kind, party, candidates (id, label) and
    /// thresholds (previous, new, max).
    fn assert_form(
        app: &App,
        kind: MemberChange,
        candidates: &[(String, &str)],
        thresholds: (i32, i32, i32),
    ) {
        let form = member_form(app);
        assert_eq!(form.kind, kind);
        assert_eq!(form.party_id, id("treasury").to_string());
        assert_eq!(form.party_name, "treasury");
        let found: Vec<(String, &str)> = form
            .candidates
            .iter()
            .map(|c| (c.participant_id.clone(), c.label.as_str()))
            .collect();
        assert_eq!(found, candidates);
        assert_eq!(form.selected, 0);
        let found = (
            form.previous_threshold,
            form.new_threshold,
            form.max_threshold,
        );
        assert_eq!(found, thresholds);
    }

    /// Drive the pick and threshold keys the kick and add-party forms share.
    fn assert_pick_and_threshold_keys(overlay: Overlay) {
        let (mut app, requests) = app();
        app.overlay = overlay;
        press(&mut app, KeyCode::Down);
        assert_eq!(picked(&app), (1, 2));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(picked(&app), (1, 2), "the pick stops at the last candidate");
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(picked(&app), (0, 2));
        press(&mut app, KeyCode::Up);
        assert_eq!(
            picked(&app),
            (0, 2),
            "the pick stops at the first candidate"
        );
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Right);
        assert_eq!(picked(&app), (1, 3));
        press(&mut app, KeyCode::Char('+'));
        assert_eq!(picked(&app), (1, 3), "the threshold stops at the maximum");
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Char('-'));
        assert_eq!(picked(&app), (1, 1));
        press(&mut app, KeyCode::Char('-'));
        assert_eq!(picked(&app), (1, 1), "the threshold stops at 1");
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Tab);
        assert_eq!(picked(&app), (1, 1), "other keys do nothing");
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.overlay, Overlay::None));
        assert!(requests.try_recv().is_err(), "only Enter sends a request");
    }

    #[test]
    fn kick_form_keys_move_the_pick_and_bound_the_threshold() {
        assert_pick_and_threshold_keys(member_change_overlay(MemberChange::Kick));
    }

    #[test]
    fn add_party_form_keys_move_the_pick_and_bound_the_threshold() {
        assert_pick_and_threshold_keys(member_change_overlay(MemberChange::Add));
    }

    #[test]
    fn kick_form_enter_sends_the_kick_for_the_pick() {
        let (mut app, requests) = app();
        app.overlay = member_change_overlay(MemberChange::Kick);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Enter);
        let Ok(Request::Kick {
            party_id,
            participant_id,
            new_threshold,
            previous_threshold,
        }) = requests.try_recv()
        else {
            panic!("expected a kick request");
        };
        assert_eq!(party_id, "dec::1220");
        assert_eq!(participant_id, "beta::1220");
        assert_eq!((new_threshold, previous_threshold), (1, 3));
        assert!(matches!(&app.overlay, Overlay::Busy(text) if text == "Kicking beta…"));
    }

    #[test]
    fn add_party_form_enter_sends_the_add_for_the_pick() {
        let (mut app, requests) = app();
        app.overlay = member_change_overlay(MemberChange::Add);
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Enter);
        let Ok(Request::AddParty {
            party_id,
            participant_id,
            new_threshold,
            previous_threshold,
        }) = requests.try_recv()
        else {
            panic!("expected an add-party request");
        };
        assert_eq!(party_id, "dec::1220");
        assert_eq!(participant_id, "alpha::1220");
        assert_eq!((new_threshold, previous_threshold), (3, 3));
        assert!(matches!(&app.overlay, Overlay::Busy(text) if text == "Adding alpha…"));
    }

    fn peer(prefix: &str, name: &str, is_self: bool) -> PeerView {
        PeerView {
            participant_id: id(prefix).to_string(),
            name: name.to_owned(),
            address: String::new(),
            port: 0,
            status: None,
            latency_ms: None,
            version: None,
            workflow: None,
            is_self,
        }
    }

    fn participant(prefix: &str, owner: bool) -> ParticipantInfo {
        ParticipantInfo {
            participant_uid: id(prefix),
            permission: Permission::Confirmation,
            owner_key: owner.then(|| format!("key-{prefix}")),
        }
    }

    /// A party hosted by this node and alpha (both owners) and beta (not an
    /// owner), with three namespace owners and threshold 2.
    fn party() -> DecentralizedParty {
        DecentralizedParty {
            party_id: id("treasury"),
            threshold: 2,
            confirmation_threshold: None,
            owners: vec!["o1".to_owned(), "o2".to_owned(), "o3".to_owned()],
            my_owner_key: None,
            participants: vec![
                participant("me", true),
                participant("alpha", true),
                participant("beta", false),
            ],
            non_hosting_owners: Vec::new(),
            contracts: Vec::new(),
            local_metadata: None,
        }
    }

    /// An app with the party detail open and this node, alpha and gamma
    /// configured as peers.
    fn app_with_party() -> (App, Receiver<Request>) {
        let (mut app, requests) = app();
        app.detail = Some(party());
        app.peers = vec![
            peer("me", "me", true),
            peer("alpha", "Alpha", false),
            peer("gamma", "Gamma", false),
        ];
        (app, requests)
    }

    #[test]
    fn open_kick_and_add_party_wait_for_this_nodes_identity() {
        for open in [App::open_kick, App::open_add_party] {
            let (mut app, _requests) = app_with_party();
            app.peers.retain(|peer| !peer.is_self);
            open(&mut app);
            assert!(matches!(
                &app.overlay,
                Overlay::Message(text)
                    if text == "Still resolving this node's identity — try again in a moment."
            ));
        }
    }

    #[test]
    fn open_kick_lists_the_other_owners() {
        let (mut app, _requests) = app_with_party();
        app.open_kick();
        let alpha = (id("alpha").to_string(), "alpha");
        assert_form(&app, MemberChange::Kick, &[alpha], (2, 1, 2));
    }

    #[test]
    fn open_add_party_lists_the_peers_outside_the_party() {
        let (mut app, _requests) = app_with_party();
        app.open_add_party();
        let gamma = (id("gamma").to_string(), "Gamma");
        assert_form(&app, MemberChange::Add, &[gamma], (2, 2, 4));
    }
}
