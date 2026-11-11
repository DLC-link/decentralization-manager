//! Party actions: the detail view, onboard, kick, add party and change
//! threshold.

use std::collections::HashSet;

use ratatui::widgets::TableState;

use crate::api::party_name;

use crate::app::forms::validate_prefix;
use crate::app::overlay::{AddPartyForm, ChangeThresholdForm, KickCandidate, KickForm};
use crate::app::overlay::{OnboardForm, Overlay, PeerChoice};
use crate::app::state::App;
use crate::app::tabs::filter_parties;
use crate::app::worker::Request;

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
        let Some(self_id) = self
            .peers
            .iter()
            .find(|peer| peer.is_self)
            .map(|peer| peer.participant_id.clone())
        else {
            self.overlay = Overlay::Message(
                "Still resolving this node's identity — try again in a moment.".to_owned(),
            );
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
        self.overlay = Overlay::Kick(KickForm {
            party_id: party.party_id.to_string(),
            party_name: party_name(party).to_owned(),
            previous_threshold: party.threshold,
            candidates,
            selected: 0,
            new_threshold: suggested,
            max_threshold: remaining,
        });
    }

    /// Send the kick request for the selected candidate in the kick form.
    pub(super) fn confirm_kick(&mut self) {
        let Overlay::Kick(form) = &self.overlay else {
            return;
        };
        let Some(candidate) = form.candidates.get(form.selected) else {
            return;
        };
        let request = Request::Kick {
            party_id: form.party_id.clone(),
            participant_id: candidate.participant_id.clone(),
            new_threshold: form.new_threshold,
            previous_threshold: form.previous_threshold,
        };
        let label = candidate.label.clone();
        let _ = self.requests.send(request);
        self.overlay = Overlay::Busy(format!("Kicking {label}…"));
    }

    /// Open the add-party form for the party in the detail view, listing the
    /// configured peers not already members (self excluded).
    pub(super) fn open_add_party(&mut self) {
        let Some(party) = self.detail.as_ref() else {
            return;
        };
        // Require this node's identity to be resolved so self is excluded.
        let Some(self_id) = self
            .peers
            .iter()
            .find(|peer| peer.is_self)
            .map(|peer| peer.participant_id.clone())
        else {
            self.overlay = Overlay::Message(
                "Still resolving this node's identity — try again in a moment.".to_owned(),
            );
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
        self.overlay = Overlay::AddParty(AddPartyForm {
            party_id: party.party_id.to_string(),
            party_name: party_name(party).to_owned(),
            previous_threshold: party.threshold,
            candidates,
            selected: 0,
            new_threshold: party.threshold.clamp(1, max_threshold),
            max_threshold,
        });
    }

    /// Send the add-party request for the selected candidate.
    pub(super) fn confirm_add_party(&mut self) {
        let Overlay::AddParty(form) = &self.overlay else {
            return;
        };
        let Some(candidate) = form.candidates.get(form.selected) else {
            return;
        };
        let request = Request::AddParty {
            party_id: form.party_id.clone(),
            participant_id: candidate.participant_id.clone(),
            new_threshold: form.new_threshold,
            previous_threshold: form.previous_threshold,
        };
        let label = candidate.label.clone();
        let _ = self.requests.send(request);
        self.overlay = Overlay::Busy(format!("Adding {label}…"));
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
