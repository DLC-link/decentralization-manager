//! Governance actions: the composer, the deploy form, the approvals and the
//! chain audit.

use serde_json::{Value, json};

use common::types::DecentralizedParty;

use crate::api::{GovConfirmation, party_name};
use crate::auth::now_secs;
use crate::composer::{self, Composer, ComposerContext, ComposerSubmit, FieldKind, PickerSource};

use crate::app::forms::build_contracts_request;
use crate::app::overlay::{ComposerKind, ComposerPick, GovActionRequest, GovItem, GovOp};
use crate::app::overlay::{GovView, Overlay};
use crate::app::state::App;
use crate::app::worker::{Request, confirmation_cids};

/// The placeholder action sent with on-chain (`core_domain`) confirm/execute —
/// the server builds the real choice from `proposal_cid` and ignores this.
fn placeholder_action() -> Value {
    json!({ "type": "governance_set_threshold", "new_threshold": 0 })
}

/// The governance type for a party's rules contract, or `None` when the party
/// has no governance rules contract. Mirrors the web frontend's classification.
fn party_governance_type(party: &DecentralizedParty) -> Option<&'static str> {
    party.contracts.iter().find_map(|contract| {
        let template = contract.template_id.as_str();
        if template == "Governance.Rules:GovernanceRules" {
            Some("core_self")
        } else {
            None
        }
    })
}

/// What the governance actions send for one approvals item. An off-chain
/// (`core_self`) item carries its own action. An on-chain (`core_domain`)
/// item sends the placeholder action and its proposal cid.
struct GovTarget<'a> {
    action: Value,
    governance_type: &'static str,
    proposal_cid: Option<String>,
    confirmations: &'a [GovConfirmation],
    executable_confirmation_cids: Option<&'a [String]>,
    can_execute: bool,
}

impl GovView {
    /// What the governance actions send for the selected item, or `None`
    /// when nothing is selected.
    fn selected_target(&self) -> Option<GovTarget<'_>> {
        let target = match self.items.get(self.selected)? {
            GovItem::OffChain(action) => GovTarget {
                action: action.action.clone(),
                governance_type: "core_self",
                proposal_cid: None,
                confirmations: &action.confirmations,
                executable_confirmation_cids: action.executable_confirmation_cids.as_deref(),
                can_execute: action.can_execute,
            },
            GovItem::Domain(domain) => GovTarget {
                action: placeholder_action(),
                governance_type: "core_domain",
                proposal_cid: Some(domain.proposal_cid.clone()),
                confirmations: &domain.confirmations,
                executable_confirmation_cids: domain.executable_confirmation_cids.as_deref(),
                can_execute: domain.can_execute,
            },
        };
        Some(target)
    }
}

impl App {
    /// Open the action / proposal type picker from the governance overlay.
    pub(super) fn open_composer_pick(&mut self, kind: ComposerKind) {
        let pick = {
            let Overlay::Governance(view) = &self.overlay else {
                return;
            };
            let options = match kind {
                ComposerKind::Action => composer::action_types(),
                ComposerKind::Proposal => composer::proposal_types(),
            };
            ComposerPick {
                kind,
                party_id: view.party_id.clone(),
                party_name: view.party_name.clone(),
                rules_contract_id: view.rules_contract_id.clone(),
                default_threshold: i64::from(view.threshold),
                options,
                selected: 0,
            }
        };
        self.overlay = Overlay::ComposerPick(Box::new(pick));
    }

    /// Open the composer form for the type selected in the picker.
    pub(super) fn open_composer(&mut self) {
        let composer = {
            let Overlay::ComposerPick(pick) = &self.overlay else {
                return;
            };
            let Some(option) = pick.options.get(pick.selected) else {
                return;
            };
            let ctx = ComposerContext {
                party_id: pick.party_id.clone(),
                operator_party: self.operator_party.clone(),
                default_threshold: pick.default_threshold,
            };
            let (fields, submit) = match pick.kind {
                ComposerKind::Action => (
                    composer::fields_for_action(option.key, &ctx),
                    ComposerSubmit::Confirm,
                ),
                ComposerKind::Proposal => (
                    composer::fields_for_proposal(option.key, &ctx),
                    ComposerSubmit::Propose,
                ),
            };
            Composer {
                title: option.label.to_owned(),
                action_type: option.key,
                submit,
                party_id: pick.party_id.clone(),
                party_name: pick.party_name.clone(),
                rules_contract_id: pick.rules_contract_id.clone(),
                fields,
                cursor: 0,
            }
        };
        // Gather the distinct picker sources this form needs and kick off their
        // background fetch; options stream in via `Update::PickerOptions`.
        let party_id = composer.party_id.clone();
        let mut sources: Vec<PickerSource> = Vec::new();
        for field in &composer.fields {
            if let FieldKind::Picker { source, .. } = field.kind
                && !sources.contains(&source)
            {
                sources.push(source);
            }
        }
        self.overlay = Overlay::Composer(Box::new(composer));
        if !sources.is_empty() {
            let _ = self
                .requests
                .send(Request::PickerOptions { party_id, sources });
        }
    }

    /// Validate the composer form and submit it (confirm or propose).
    pub(super) fn submit_composer(&mut self) {
        let outcome = {
            let Overlay::Composer(composer) = &self.overlay else {
                return;
            };
            composer::build_payload(composer).map(|payload| {
                let op = match composer.submit {
                    ComposerSubmit::Confirm => GovOp::Confirm {
                        action: payload,
                        governance_type: "core_self".to_owned(),
                        proposal_cid: None,
                    },
                    ComposerSubmit::Propose => GovOp::Propose { proposal: payload },
                };
                GovActionRequest {
                    op,
                    party_id: composer.party_id.clone(),
                    party_name: composer.party_name.clone(),
                    rules_contract_id: composer.rules_contract_id.clone(),
                }
            })
        };
        match outcome {
            Ok(request) => {
                let _ = self.requests.send(Request::GovAction(Box::new(request)));
                self.overlay = Overlay::Busy("Submitting…".to_owned());
            }
            Err(error) => self.overlay = Overlay::Message(error),
        }
    }

    /// Start a governance-core contract deployment for the party in detail view.
    pub(super) fn open_deploy(&mut self) {
        let Some(party) = self.detail.as_ref() else {
            return;
        };
        if party_governance_type(party).is_some() {
            self.overlay = Overlay::Message(
                "This party already has governance contracts deployed.".to_owned(),
            );
            return;
        }
        let _ = self.requests.send(Request::DeployContext {
            party_id: party.party_id.to_string(),
            party_name: party_name(party).to_owned(),
        });
        self.overlay = Overlay::Busy("Preparing contract deployment…".to_owned());
    }

    /// Validate the deploy form and start the contracts workflow.
    pub(super) fn submit_deploy(&mut self) {
        let outcome = {
            let Overlay::Deploy(form) = &self.overlay else {
                return;
            };
            build_contracts_request(form)
        };
        match outcome {
            Ok(body) => {
                let _ = self.requests.send(Request::DeploySubmit(Box::new(body)));
                self.overlay = Overlay::Busy("Deploying contracts…".to_owned());
            }
            Err(error) => self.overlay = Overlay::Message(error),
        }
    }

    /// Open the governance approvals list for the party in the detail view.
    pub(super) fn open_governance(&mut self) {
        let Some(party) = self.detail.as_ref() else {
            return;
        };
        if party_governance_type(party).is_none() {
            self.overlay =
                Overlay::Message("This party has no governance rules contract.".to_owned());
            return;
        }
        let _ = self.requests.send(Request::Governance {
            party_id: party.party_id.to_string(),
            party_name: party_name(party).to_owned(),
        });
        self.overlay = Overlay::Busy("Loading governance approvals…".to_owned());
    }

    /// Build a governance-action request for the selected item, applying `op`.
    fn gov_request(view: &GovView, op: GovOp) -> GovActionRequest {
        GovActionRequest {
            op,
            party_id: view.party_id.clone(),
            party_name: view.party_name.clone(),
            rules_contract_id: view.rules_contract_id.clone(),
        }
    }

    /// The open approvals view and what its selected item carries, or `None`
    /// when no approvals view is open or it has no items.
    fn gov_selection(&self) -> Option<(&GovView, GovTarget<'_>)> {
        let Overlay::Governance(view) = &self.overlay else {
            return None;
        };
        Some((view, view.selected_target()?))
    }

    /// Add this node's confirmation to the selected governance action.
    pub(super) fn gov_confirm(&mut self) {
        let request = {
            let Some((view, target)) = self.gov_selection() else {
                return;
            };
            let op = GovOp::Confirm {
                action: target.action,
                governance_type: target.governance_type.to_owned(),
                proposal_cid: target.proposal_cid,
            };
            Self::gov_request(view, op)
        };
        let _ = self.requests.send(Request::GovAction(Box::new(request)));
        self.overlay = Overlay::Busy("Confirming action…".to_owned());
    }

    /// Execute the selected governance action once its threshold is met.
    pub(super) fn gov_execute(&mut self) {
        let (can_execute, request) = {
            let Some((view, target)) = self.gov_selection() else {
                return;
            };
            let op = GovOp::Execute {
                action: target.action,
                confirmation_cids: confirmation_cids(
                    target.executable_confirmation_cids,
                    target.confirmations,
                ),
                governance_type: target.governance_type.to_owned(),
                proposal_cid: target.proposal_cid,
            };
            (target.can_execute, Self::gov_request(view, op))
        };
        if !can_execute {
            self.overlay =
                Overlay::Message("Not enough confirmations to execute this action yet.".to_owned());
            return;
        }
        let _ = self.requests.send(Request::GovAction(Box::new(request)));
        self.overlay = Overlay::Busy("Executing action…".to_owned());
    }

    /// Revoke this node's own confirmation on the selected governance action.
    pub(super) fn gov_revoke(&mut self) {
        let request = {
            let Some((view, target)) = self.gov_selection() else {
                return;
            };
            target
                .confirmations
                .iter()
                .find(|confirmation| confirmation.confirming_party == view.member_party_id)
                .map(|mine| {
                    Self::gov_request(
                        view,
                        GovOp::Cancel {
                            confirmation_cid: mine.contract_id.clone(),
                            governance_type: target.governance_type.to_owned(),
                        },
                    )
                })
        };
        match request {
            Some(request) => {
                let _ = self.requests.send(Request::GovAction(Box::new(request)));
                self.overlay = Overlay::Busy("Revoking confirmation…".to_owned());
            }
            None => {
                self.overlay = Overlay::Message("You have no confirmation to revoke.".to_owned());
            }
        }
    }

    /// Expire the first stale confirmation on the selected governance action.
    pub(super) fn gov_expire(&mut self) {
        let now = now_secs();
        let request = {
            let Some((view, target)) = self.gov_selection() else {
                return;
            };
            target
                .confirmations
                .iter()
                .find(|confirmation| confirmation.expires_at > 0 && confirmation.expires_at <= now)
                .map(|stale| {
                    Self::gov_request(
                        view,
                        GovOp::Expire {
                            confirmation_cid: stale.contract_id.clone(),
                            governance_type: target.governance_type.to_owned(),
                        },
                    )
                })
        };
        match request {
            Some(request) => {
                let _ = self.requests.send(Request::GovAction(Box::new(request)));
                self.overlay = Overlay::Busy("Expiring confirmation…".to_owned());
            }
            None => {
                self.overlay =
                    Overlay::Message("No expired confirmations on this action.".to_owned());
            }
        }
    }

    /// Fetch and show the on-chain governance audit trail for the open party.
    pub(super) fn open_chain_audit(&mut self) {
        if let Some(party) = self.detail.as_ref() {
            let _ = self
                .requests
                .send(Request::ChainAudit(party.party_id.to_string()));
            self.overlay = Overlay::Busy("Loading on-chain audit…".to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver};

    use super::*;
    use crate::api::{DomainGovAction, GovAction};

    fn confirmation(contract_id: &str, party: &str, expires_at: i64) -> GovConfirmation {
        GovConfirmation {
            contract_id: contract_id.to_owned(),
            confirming_party: party.to_owned(),
            expires_at,
        }
    }

    fn off_chain_action() -> Value {
        json!({ "type": "governance_set_threshold", "new_threshold": 3 })
    }

    /// An off-chain item: this node confirmed with `00c1`, which has expired.
    fn off_chain(can_execute: bool) -> GovItem {
        GovItem::OffChain(GovAction {
            action: off_chain_action(),
            confirmations: vec![
                confirmation("00c1", "me::1220", 1),
                confirmation("00c2", "other::1220", 0),
            ],
            executable_confirmation_cids: Some(vec!["00c1".to_owned()]),
            confirmation_count: 2,
            can_execute,
        })
    }

    /// An on-chain item: this node confirmed with `00d2`; `00d1` has expired.
    fn domain(can_execute: bool) -> GovItem {
        GovItem::Domain(DomainGovAction {
            proposal_cid: "00prop".to_owned(),
            action_label: "Mint".to_owned(),
            description: None,
            confirmations: vec![
                confirmation("00d1", "other::1220", 1),
                confirmation("00d2", "me::1220", 0),
            ],
            executable_confirmation_cids: None,
            confirmation_count: 2,
            can_execute,
            orphaned: false,
        })
    }

    fn app_with(items: Vec<GovItem>) -> (App, Receiver<Request>) {
        let (requests, request_rx) = mpsc::channel();
        let (_updates_tx, updates) = mpsc::channel();
        let mut app = App::new(requests, updates, false);
        app.overlay = Overlay::Governance(Box::new(GovView {
            party_name: "treasury".to_owned(),
            party_id: "dec::1220".to_owned(),
            rules_contract_id: "00rules".to_owned(),
            member_party_id: "me::1220".to_owned(),
            threshold: 2,
            items,
            selected: 0,
        }));
        (app, request_rx)
    }

    /// The governance op the app sent, after checking the view's context.
    fn sent_op(requests: &Receiver<Request>) -> GovOp {
        let Ok(Request::GovAction(request)) = requests.try_recv() else {
            panic!("expected a governance request");
        };
        let GovActionRequest {
            op,
            party_id,
            party_name,
            rules_contract_id,
        } = *request;
        assert_eq!(
            (
                party_id.as_str(),
                party_name.as_str(),
                rules_contract_id.as_str()
            ),
            ("dec::1220", "treasury", "00rules")
        );
        op
    }

    fn assert_overlay(app: &App, busy: bool, text: &str) {
        match &app.overlay {
            Overlay::Busy(shown) if busy => assert_eq!(shown, text),
            Overlay::Message(shown) if !busy => assert_eq!(shown, text),
            _ => panic!("expected {text}"),
        }
    }

    #[test]
    fn selected_target_reads_both_item_kinds() {
        let (app, _requests) = app_with(vec![off_chain(true), domain(false)]);
        let Overlay::Governance(mut view) = app.overlay else {
            panic!("the approvals view is open");
        };
        let Some(target) = view.selected_target() else {
            panic!("item 0 is selected");
        };
        assert_eq!(target.action, off_chain_action());
        assert_eq!(target.governance_type, "core_self");
        assert_eq!(target.proposal_cid, None);
        assert_eq!(target.confirmations[0].contract_id, "00c1");
        assert_eq!(
            target.executable_confirmation_cids,
            Some(&["00c1".to_owned()][..])
        );
        assert!(target.can_execute);

        view.selected = 1;
        let Some(target) = view.selected_target() else {
            panic!("item 1 is selected");
        };
        assert_eq!(target.action, placeholder_action());
        assert_eq!(target.governance_type, "core_domain");
        assert_eq!(target.proposal_cid.as_deref(), Some("00prop"));
        assert_eq!(target.confirmations[0].contract_id, "00d1");
        assert_eq!(target.executable_confirmation_cids, None);
        assert!(!target.can_execute);

        view.selected = 2;
        assert!(view.selected_target().is_none());
    }

    #[test]
    fn confirm_sends_the_items_action_or_the_proposal() {
        let (mut app, requests) = app_with(vec![off_chain(false)]);
        app.gov_confirm();
        let GovOp::Confirm {
            action,
            governance_type,
            proposal_cid,
        } = sent_op(&requests)
        else {
            panic!("expected a confirm");
        };
        assert_eq!(action, off_chain_action());
        assert_eq!(governance_type, "core_self");
        assert_eq!(proposal_cid, None);
        assert_overlay(&app, true, "Confirming action…");

        let (mut app, requests) = app_with(vec![domain(false)]);
        app.gov_confirm();
        let GovOp::Confirm {
            action,
            governance_type,
            proposal_cid,
        } = sent_op(&requests)
        else {
            panic!("expected a confirm");
        };
        assert_eq!(action, placeholder_action());
        assert_eq!(governance_type, "core_domain");
        assert_eq!(proposal_cid.as_deref(), Some("00prop"));
    }

    #[test]
    fn execute_sends_the_executable_confirmations() {
        let (mut app, requests) = app_with(vec![off_chain(true)]);
        app.gov_execute();
        let GovOp::Execute {
            action,
            confirmation_cids,
            governance_type,
            proposal_cid,
        } = sent_op(&requests)
        else {
            panic!("expected an execute");
        };
        assert_eq!(action, off_chain_action());
        assert_eq!(confirmation_cids, vec!["00c1"]);
        assert_eq!(governance_type, "core_self");
        assert_eq!(proposal_cid, None);
        assert_overlay(&app, true, "Executing action…");

        // Without the server's executable subset, every confirmation goes.
        let (mut app, requests) = app_with(vec![domain(true)]);
        app.gov_execute();
        let GovOp::Execute {
            action,
            confirmation_cids,
            governance_type,
            proposal_cid,
        } = sent_op(&requests)
        else {
            panic!("expected an execute");
        };
        assert_eq!(action, placeholder_action());
        assert_eq!(confirmation_cids, vec!["00d1", "00d2"]);
        assert_eq!(governance_type, "core_domain");
        assert_eq!(proposal_cid.as_deref(), Some("00prop"));
    }

    #[test]
    fn execute_waits_for_enough_confirmations() {
        for item in [off_chain(false), domain(false)] {
            let (mut app, requests) = app_with(vec![item]);
            app.gov_execute();
            assert!(requests.try_recv().is_err());
            assert_overlay(
                &app,
                false,
                "Not enough confirmations to execute this action yet.",
            );
        }
    }

    #[test]
    fn revoke_cancels_this_nodes_own_confirmation() {
        for (item, cid, kind) in [
            (off_chain(false), "00c1", "core_self"),
            (domain(false), "00d2", "core_domain"),
        ] {
            let (mut app, requests) = app_with(vec![item]);
            app.gov_revoke();
            let GovOp::Cancel {
                confirmation_cid,
                governance_type,
            } = sent_op(&requests)
            else {
                panic!("expected a cancel");
            };
            assert_eq!(
                (confirmation_cid.as_str(), governance_type.as_str()),
                (cid, kind)
            );
            assert_overlay(&app, true, "Revoking confirmation…");
        }

        let (mut app, requests) = app_with(vec![off_chain(false)]);
        if let Overlay::Governance(view) = &mut app.overlay {
            view.member_party_id = "nobody::1220".to_owned();
        }
        app.gov_revoke();
        assert!(requests.try_recv().is_err());
        assert_overlay(&app, false, "You have no confirmation to revoke.");
    }

    #[test]
    fn expire_targets_the_first_stale_confirmation() {
        for (item, cid, kind) in [
            (off_chain(false), "00c1", "core_self"),
            (domain(false), "00d1", "core_domain"),
        ] {
            let (mut app, requests) = app_with(vec![item]);
            app.gov_expire();
            let GovOp::Expire {
                confirmation_cid,
                governance_type,
            } = sent_op(&requests)
            else {
                panic!("expected an expire");
            };
            assert_eq!(
                (confirmation_cid.as_str(), governance_type.as_str()),
                (cid, kind)
            );
            assert_overlay(&app, true, "Expiring confirmation…");
        }

        let mut fresh = off_chain(false);
        if let GovItem::OffChain(action) = &mut fresh {
            action.confirmations[0].expires_at = i64::MAX;
        }
        let (mut app, requests) = app_with(vec![fresh]);
        app.gov_expire();
        assert!(requests.try_recv().is_err());
        assert_overlay(&app, false, "No expired confirmations on this action.");
    }

    #[test]
    fn gov_actions_need_a_selected_item() {
        let actions: [fn(&mut App); 4] = [
            App::gov_confirm,
            App::gov_execute,
            App::gov_revoke,
            App::gov_expire,
        ];
        for action in actions {
            let (mut app, requests) = app_with(Vec::new());
            action(&mut app);
            assert!(requests.try_recv().is_err());
            assert!(matches!(app.overlay, Overlay::Governance(_)));

            app.overlay = Overlay::None;
            action(&mut app);
            assert!(requests.try_recv().is_err());
            assert!(matches!(app.overlay, Overlay::None));
        }
    }
}
