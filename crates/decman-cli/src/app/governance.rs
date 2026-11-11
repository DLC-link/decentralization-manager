//! Governance actions: the composer, the deploy form, the approvals and the
//! chain audit.

use serde_json::{Value, json};

use common::types::DecentralizedParty;

use crate::api::party_name;
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

    /// Add this node's confirmation to the selected governance action.
    pub(super) fn gov_confirm(&mut self) {
        let request = {
            let Overlay::Governance(view) = &self.overlay else {
                return;
            };
            let Some(item) = view.items.get(view.selected) else {
                return;
            };
            let op = match item {
                GovItem::OffChain(action) => GovOp::Confirm {
                    action: action.action.clone(),
                    governance_type: "core_self".to_owned(),
                    proposal_cid: None,
                },
                GovItem::Domain(domain) => GovOp::Confirm {
                    action: placeholder_action(),
                    governance_type: "core_domain".to_owned(),
                    proposal_cid: Some(domain.proposal_cid.clone()),
                },
            };
            Self::gov_request(view, op)
        };
        let _ = self.requests.send(Request::GovAction(Box::new(request)));
        self.overlay = Overlay::Busy("Confirming action…".to_owned());
    }

    /// Execute the selected governance action once its threshold is met.
    pub(super) fn gov_execute(&mut self) {
        let (can_execute, request) = {
            let Overlay::Governance(view) = &self.overlay else {
                return;
            };
            let Some(item) = view.items.get(view.selected) else {
                return;
            };
            let (can_execute, op) = match item {
                GovItem::OffChain(action) => (
                    action.can_execute,
                    GovOp::Execute {
                        action: action.action.clone(),
                        confirmation_cids: confirmation_cids(
                            action.executable_confirmation_cids.as_deref(),
                            &action.confirmations,
                        ),
                        governance_type: "core_self".to_owned(),
                        proposal_cid: None,
                    },
                ),
                GovItem::Domain(domain) => (
                    domain.can_execute,
                    GovOp::Execute {
                        action: placeholder_action(),
                        confirmation_cids: confirmation_cids(
                            domain.executable_confirmation_cids.as_deref(),
                            &domain.confirmations,
                        ),
                        governance_type: "core_domain".to_owned(),
                        proposal_cid: Some(domain.proposal_cid.clone()),
                    },
                ),
            };
            (can_execute, Self::gov_request(view, op))
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
            let Overlay::Governance(view) = &self.overlay else {
                return;
            };
            let Some(item) = view.items.get(view.selected) else {
                return;
            };
            let (confirmations, governance_type) = match item {
                GovItem::OffChain(action) => (&action.confirmations, "core_self".to_owned()),
                GovItem::Domain(domain) => (&domain.confirmations, "core_domain".to_owned()),
            };
            confirmations
                .iter()
                .find(|confirmation| confirmation.confirming_party == view.member_party_id)
                .map(|mine| {
                    Self::gov_request(
                        view,
                        GovOp::Cancel {
                            confirmation_cid: mine.contract_id.clone(),
                            governance_type,
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
            let Overlay::Governance(view) = &self.overlay else {
                return;
            };
            let Some(item) = view.items.get(view.selected) else {
                return;
            };
            let (confirmations, governance_type) = match item {
                GovItem::OffChain(action) => (&action.confirmations, "core_self".to_owned()),
                GovItem::Domain(domain) => (&domain.confirmations, "core_domain".to_owned()),
            };
            confirmations
                .iter()
                .find(|confirmation| confirmation.expires_at > 0 && confirmation.expires_at <= now)
                .map(|stale| {
                    Self::gov_request(
                        view,
                        GovOp::Expire {
                            confirmation_cid: stale.contract_id.clone(),
                            governance_type,
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
