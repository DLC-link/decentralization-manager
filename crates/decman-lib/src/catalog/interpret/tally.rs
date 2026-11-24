//! Confirmation dedupe, expiry and threshold rules, and the assembly of
//! domain-action cards.

use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
};

use canton_proto_rs::com::daml::ledger::api::v2::{Identifier, Record};
use common::canton_id::CantonId;

use super::{ParsedDomainConfirmation, ProposalInfo};
use crate::catalog::types::{
    AcceptTransferDetails, ServiceRequestDetails, TransferProposalDetails,
};

/// Newest-first, one confirmation per confirming party. Sorts a
/// (`created_at` descending, i.e. `Reverse`) copy of `items` and keeps only
/// the first occurrence per party, so a member who confirmed twice — the
/// second confirmation superseding the first, as governance-core's own
/// Confirm choice does on-ledger — is represented once, by its newest
/// confirmation.
pub fn dedupe_newest_per_party<T>(
    mut items: Vec<T>,
    party: impl Fn(&T) -> &CantonId,
    created_at: impl Fn(&T) -> i64,
) -> Vec<T> {
    items.sort_by_key(|item| Reverse(created_at(item)));
    let mut seen_parties: HashSet<CantonId> = HashSet::new();
    items
        .into_iter()
        .filter(|item| seen_parties.insert(party(item).clone()))
        .collect()
}

/// Count confirmations that still count toward the threshold at
/// `now_seconds`. Mirrors Daml's `expiresAt > now` filter so the caller
/// doesn't offer an Execute that chain would reject; `expires_at == 0` means
/// "no expiry resolved" and counts as live, mirroring decman.
pub fn live_count<T>(items: &[T], expires_at: impl Fn(&T) -> i64, now_seconds: i64) -> usize {
    items
        .iter()
        .filter(|item| is_live(expires_at(item), now_seconds))
        .count()
}

fn is_live(expires_at: i64, now_seconds: i64) -> bool {
    expires_at == 0 || expires_at > now_seconds
}

/// How long before its `expiresAt` a confirmation stops being offered to
/// Execute, so it cannot lapse between page load and the click.
pub const EXECUTE_EXPIRY_MARGIN_SECONDS: i64 = 30;

/// Whether an Execute submitted from a view built at `now_seconds` can still
/// consume a confirmation that expires at `expires_at`. Consuming an expired
/// confirmation aborts the whole Execute on-ledger.
pub fn is_executable(expires_at: i64, now_seconds: i64) -> bool {
    is_live(
        expires_at,
        now_seconds.saturating_add(EXECUTE_EXPIRY_MARGIN_SECONDS),
    )
}

/// Whether a confirmation counts toward the Execute threshold: executable and
/// confirmed by a member of the rules contract the Execute runs against.
/// `members` is `None` when that contract could not be read, and then no
/// member check applies.
pub fn counts_toward_threshold(
    confirmer: &CantonId,
    expires_at: i64,
    members: Option<&HashSet<CantonId>>,
    now_seconds: i64,
) -> bool {
    is_executable(expires_at, now_seconds)
        && members.is_none_or(|members| members.contains(confirmer))
}

/// Label used for a proposal synthesized from a bare `GovernableAction` when
/// nothing names it — no `actionLabel` in the interface view or the
/// create-arguments, and no template id on the event either.
pub const FALLBACK_PROPOSAL_LABEL: &str = "Proposal";

/// A pending governance-core domain proposal with its (deduped, live-counted)
/// confirmations, ready for a caller to render as a notification card.
#[derive(Clone, Debug, PartialEq)]
pub struct DomainAction {
    pub proposal_cid: String,
    pub action_label: String,
    pub description: Option<String>,
    pub confirmations: Vec<ParsedDomainConfirmation>,
    pub confirmation_count: usize,
    pub executable_confirmation_cids: Vec<String>,
    pub can_execute: bool,
    pub orphaned: bool,
    pub transfer_details: Option<TransferProposalDetails>,
    pub accept_transfer_details: Option<AcceptTransferDetails>,
    pub service_request_details: Option<ServiceRequestDetails>,
    pub proposer: Option<CantonId>,
    pub created_at: Option<i64>,
    /// See [`ProposalInfo::template_id`]. `None` on an orphan.
    pub template_id: Option<Identifier>,
    /// See [`ProposalInfo::create_arguments`]. `None` on an orphan.
    pub create_arguments: Option<Record>,
}

/// Merge confirmed domain proposals with the full active-proposal set.
///
/// `domain_confirmations` covers only proposals that already have at least
/// one confirmation; `proposal_infos` covers every active `GovernableAction`
/// visible to the caller's governance party, confirmed or not. Confirmations
/// whose proposal isn't in `proposal_infos` are marked `orphaned` (rather
/// than dropped) so the caller can offer a dismiss-only card — the
/// underlying Confirmation contracts are still on-ledger and need to be
/// expired explicitly to clear them — but only when `proposal_infos_complete`
/// is `true`: otherwise the missing-from-map signal is unreliable and
/// everything would be falsely marked orphaned.
///
/// Whatever remains in `proposal_infos` after the confirmed proposals are
/// enriched and removed is a proposal nobody has confirmed yet. Those are
/// synthesized into zero-confirmation cards, but only when BOTH
/// `proposal_infos_complete` and `domain_confirmations_complete` are `true`:
/// synthesizing off an incomplete confirmation fetch would offer Confirm to a
/// member who has already confirmed.
pub fn assemble_domain_actions(
    domain_confirmations: HashMap<String, (String, Vec<ParsedDomainConfirmation>)>,
    mut proposal_infos: HashMap<String, ProposalInfo>,
    proposal_infos_complete: bool,
    domain_confirmations_complete: bool,
    threshold: usize,
    members: Option<&HashSet<CantonId>>,
    now_seconds: i64,
) -> Vec<DomainAction> {
    let mut domain_actions: Vec<DomainAction> = domain_confirmations
        .into_iter()
        .map(|(proposal_cid, (action_label, confirmations))| {
            let unique_confirmations =
                dedupe_newest_per_party(confirmations, |c| &c.confirming_party, |c| c.created_at);
            let info = proposal_infos.remove(&proposal_cid);
            let orphaned = info.is_none() && proposal_infos_complete;
            let info = info.unwrap_or_default();
            let executable_confirmation_cids: Vec<String> = unique_confirmations
                .iter()
                .filter(|c| {
                    counts_toward_threshold(&c.confirming_party, c.expires_at, members, now_seconds)
                })
                .map(|c| c.contract_id.clone())
                .collect();
            let confirmation_count = executable_confirmation_cids.len();
            DomainAction {
                proposal_cid,
                action_label,
                description: info.description,
                confirmations: unique_confirmations,
                confirmation_count,
                executable_confirmation_cids,
                // Orphans can't be executed regardless of threshold.
                can_execute: !orphaned && confirmation_count >= threshold,
                orphaned,
                transfer_details: info.transfer,
                accept_transfer_details: info.accept_transfer,
                service_request_details: info.service_request,
                proposer: info.proposer,
                created_at: info.created_at,
                template_id: info.template_id,
                create_arguments: info.create_arguments,
            }
        })
        .collect();

    if proposal_infos_complete && domain_confirmations_complete {
        for (proposal_cid, info) in proposal_infos {
            let action_label = info
                .action_label
                .unwrap_or_else(|| FALLBACK_PROPOSAL_LABEL.to_string());
            domain_actions.push(DomainAction {
                proposal_cid,
                action_label,
                description: info.description,
                confirmations: Vec::new(),
                confirmation_count: 0,
                executable_confirmation_cids: Vec::new(),
                can_execute: false,
                orphaned: false,
                transfer_details: info.transfer,
                accept_transfer_details: info.accept_transfer,
                service_request_details: info.service_request,
                // A proposal nobody has confirmed is the likeliest one to
                // retract, so the card needs its proposer as much as any
                // other.
                proposer: info.proposer,
                created_at: info.created_at,
                template_id: info.template_id,
                create_arguments: info.create_arguments,
            });
        }
    }

    domain_actions
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::daml::ledger::api::v2::Identifier;

    use super::*;
    use crate::{
        catalog::interpret::{
            extract_proposal_info,
            fixtures::{
                ALICE, BOB, GOV, cid, governable_action_interface_view, proposal_created_event,
                record_of,
            },
        },
        framework::encode::{field, make_party, make_text},
    };

    // -- dedupe_newest_per_party / live_count / assemble_domain_actions --

    fn domain_confirmation(
        contract_id: &str,
        proposal_cid: &str,
        party: &str,
        created_at: i64,
        expires_at: i64,
    ) -> ParsedDomainConfirmation {
        ParsedDomainConfirmation {
            contract_id: contract_id.to_string(),
            proposal_cid: proposal_cid.to_string(),
            action_label: "Some Action".to_string(),
            confirming_party: cid(party),
            created_at,
            expires_at,
        }
    }

    fn proposal_info_fixture(action_label: Option<&str>) -> ProposalInfo {
        ProposalInfo {
            description: Some("a description".to_string()),
            transfer: None,
            accept_transfer_instruction_cid: None,
            accept_transfer: None,
            service_request: None,
            action_label: action_label.map(str::to_string),
            proposer: Some(cid(GOV)),
            created_at: Some(1_700_000_000),
            template_id: Some(Identifier {
                package_id: "#governance-core".to_string(),
                module_name: "Governance.Action".to_string(),
                entity_name: "GenericVoteProposal".to_string(),
            }),
            create_arguments: Some(record_of(vec![
                field("governanceParty", make_party(GOV)),
                field("proposer", make_party(GOV)),
                field("description", make_text("a description")),
            ])),
        }
    }

    fn confirmations_map(
        proposal_cid: &str,
        label: &str,
        confirmations: Vec<ParsedDomainConfirmation>,
    ) -> HashMap<String, (String, Vec<ParsedDomainConfirmation>)> {
        let mut map = HashMap::new();
        map.insert(proposal_cid.to_string(), (label.to_string(), confirmations));
        map
    }

    #[test]
    fn dedupe_keeps_the_newest_confirmation_per_party() {
        let older = domain_confirmation("c-old", "proposal-1", ALICE, 100, 0);
        let newer = domain_confirmation("c-new", "proposal-1", ALICE, 200, 0);

        let deduped = dedupe_newest_per_party(
            vec![older, newer.clone()],
            |c| &c.confirming_party,
            |c| c.created_at,
        );

        assert_eq!(deduped, vec![newer]);
    }

    #[test]
    fn expiry_boundary_excludes_expires_at_equal_now() {
        let now = 1_700_000_000;
        let at_now = domain_confirmation("c1", "proposal-1", ALICE, 0, now);
        let zero_expiry = domain_confirmation("c2", "proposal-1", BOB, 0, 0);
        let past_now = domain_confirmation("c3", "proposal-1", GOV, 0, now + 1);

        assert_eq!(
            live_count(std::slice::from_ref(&at_now), |c| c.expires_at, now),
            0,
            "expires_at == now must mirror `expires_at > now` and NOT be live"
        );
        assert_eq!(
            live_count(std::slice::from_ref(&zero_expiry), |c| c.expires_at, now),
            1,
            "expires_at == 0 (unresolved) always counts as live"
        );
        assert_eq!(
            live_count(std::slice::from_ref(&past_now), |c| c.expires_at, now),
            1,
            "expires_at > now counts as live"
        );
    }

    fn single_proposal_actions(
        confirmations: Vec<ParsedDomainConfirmation>,
        threshold: usize,
        members: Option<&HashSet<CantonId>>,
        now: i64,
    ) -> Vec<DomainAction> {
        let mut infos = HashMap::new();
        infos.insert(
            "proposal-1".to_string(),
            proposal_info_fixture(Some("Action")),
        );
        assemble_domain_actions(
            confirmations_map("proposal-1", "Action", confirmations),
            infos,
            true,
            true,
            threshold,
            members,
            now,
        )
    }

    #[test]
    fn executable_confirmation_cids_skip_expired_confirmations() {
        let now = 1_700_000_000;
        let confs = vec![
            domain_confirmation("expired", "proposal-1", ALICE, 100, now - 1),
            domain_confirmation("live-bob", "proposal-1", BOB, 100, now + 3600),
            domain_confirmation("live-gov", "proposal-1", GOV, 100, now + 3600),
        ];

        let actions = single_proposal_actions(confs, 2, None, now);

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].confirmations.len(), 3);
        let mut cids = actions[0].executable_confirmation_cids.clone();
        cids.sort();
        assert_eq!(cids, vec!["live-bob".to_string(), "live-gov".to_string()]);
        assert_eq!(actions[0].confirmation_count, 2);
        assert!(actions[0].can_execute);
    }

    #[test]
    fn executable_confirmation_cids_skip_non_members() {
        let now = 1_700_000_000;
        let members: HashSet<CantonId> = [cid(ALICE), cid(BOB)].into_iter().collect();
        let confs = vec![
            domain_confirmation("member", "proposal-1", ALICE, 100, now + 3600),
            domain_confirmation("removed", "proposal-1", GOV, 100, now + 3600),
        ];

        let actions = single_proposal_actions(confs, 2, Some(&members), now);

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].confirmations.len(), 2);
        assert_eq!(
            actions[0].executable_confirmation_cids,
            vec!["member".to_string()]
        );
        assert_eq!(actions[0].confirmation_count, 1);
        assert!(!actions[0].can_execute);
    }

    #[test]
    fn executable_confirmation_cids_skip_confirmations_inside_the_margin() {
        let now = 1_700_000_000;
        let edge = now + EXECUTE_EXPIRY_MARGIN_SECONDS;
        assert!(!is_executable(edge, now));
        assert!(is_executable(edge + 1, now));
        assert!(is_executable(0, now));

        let confs = vec![
            domain_confirmation("near-expiry", "proposal-1", ALICE, 100, edge),
            domain_confirmation("safe", "proposal-1", BOB, 100, edge + 1),
        ];

        let actions = single_proposal_actions(confs, 2, None, now);

        assert_eq!(actions.len(), 1);
        assert_eq!(
            actions[0].executable_confirmation_cids,
            vec!["safe".to_string()]
        );
        assert_eq!(actions[0].confirmation_count, 1);
        assert!(!actions[0].can_execute);
    }

    #[test]
    fn threshold_boundaries() {
        let now = 1_700_000_000;

        // count == threshold -> can_execute.
        let mut infos = HashMap::new();
        infos.insert(
            "proposal-1".to_string(),
            proposal_info_fixture(Some("Action")),
        );
        let confs = vec![
            domain_confirmation("c1", "proposal-1", ALICE, 100, 0),
            domain_confirmation("c2", "proposal-1", BOB, 100, 0),
        ];
        let actions = assemble_domain_actions(
            confirmations_map("proposal-1", "Action", confs),
            infos,
            true,
            true,
            2,
            None,
            now,
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].confirmation_count, 2);
        assert!(actions[0].can_execute, "count == threshold must execute");

        // threshold == 0 -> any count can_execute.
        let mut infos = HashMap::new();
        infos.insert(
            "proposal-2".to_string(),
            proposal_info_fixture(Some("Action")),
        );
        let confs = vec![domain_confirmation("c1", "proposal-2", ALICE, 100, 0)];
        let actions = assemble_domain_actions(
            confirmations_map("proposal-2", "Action", confs),
            infos,
            true,
            true,
            0,
            None,
            now,
        );
        assert_eq!(actions.len(), 1);
        assert!(
            actions[0].can_execute,
            "threshold == 0 must execute regardless of count"
        );

        // count == threshold - 1 -> not.
        let mut infos = HashMap::new();
        infos.insert(
            "proposal-3".to_string(),
            proposal_info_fixture(Some("Action")),
        );
        let confs = vec![domain_confirmation("c1", "proposal-3", ALICE, 100, 0)];
        let actions = assemble_domain_actions(
            confirmations_map("proposal-3", "Action", confs),
            infos,
            true,
            true,
            2,
            None,
            now,
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].confirmation_count, 1);
        assert!(
            !actions[0].can_execute,
            "count == threshold - 1 must not execute"
        );
    }

    #[test]
    fn orphaned_proposals_never_execute() {
        // Three confirmations, well over a threshold of 1, but the proposal
        // cid they reference is absent from a *complete* proposal-info fetch.
        let confs = vec![
            domain_confirmation("c1", "missing-cid", ALICE, 100, 0),
            domain_confirmation("c2", "missing-cid", BOB, 100, 0),
            domain_confirmation("c3", "missing-cid", GOV, 100, 0),
        ];

        let actions = assemble_domain_actions(
            confirmations_map("missing-cid", "Action", confs),
            HashMap::new(),
            true,
            true,
            1,
            None,
            1_700_000_000,
        );

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].confirmation_count, 3);
        assert!(actions[0].orphaned);
        assert!(
            !actions[0].can_execute,
            "an orphaned proposal must not execute even over threshold"
        );
    }

    #[test]
    fn incomplete_proposal_fetch_suppresses_orphan_marking() {
        // Same missing-cid shape as the orphan test, but the proposal-info
        // fetch itself failed — the missing-from-map signal is unreliable,
        // so nothing gets marked orphaned off it.
        let confs = vec![domain_confirmation("c1", "missing-cid", ALICE, 100, 0)];

        let actions = assemble_domain_actions(
            confirmations_map("missing-cid", "Action", confs),
            HashMap::new(),
            false,
            true,
            1,
            None,
            1_700_000_000,
        );

        assert_eq!(actions.len(), 1);
        assert!(!actions[0].orphaned);
    }

    #[test]
    fn synthesis_requires_both_fetches_complete() {
        let fixture_infos = || {
            let mut infos = HashMap::new();
            infos.insert(
                "unconfirmed-cid".to_string(),
                proposal_info_fixture(Some("SetupCcPreapproval")),
            );
            infos
        };

        // proposal_infos_complete alone is not enough.
        let actions =
            assemble_domain_actions(HashMap::new(), fixture_infos(), true, false, 1, None, 0);
        assert!(actions.is_empty());

        // domain_confirmations_complete alone is not enough.
        let actions =
            assemble_domain_actions(HashMap::new(), fixture_infos(), false, true, 1, None, 0);
        assert!(actions.is_empty());

        // Both complete -> the unconfirmed proposal is synthesized as a
        // zero-confirmation card.
        let actions =
            assemble_domain_actions(HashMap::new(), fixture_infos(), true, true, 1, None, 0);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].proposal_cid, "unconfirmed-cid");
        assert_eq!(actions[0].confirmation_count, 0);
        assert!(actions[0].confirmations.is_empty());
        assert!(!actions[0].can_execute);
        assert!(!actions[0].orphaned);
    }

    #[test]
    fn a_proposal_from_an_unknown_package_still_gets_a_card() {
        // The visibility rule: a package decman has never heard of, whose
        // template names its own fields differently, still gets a card. No
        // allowlist of labels or templates may gate the pending path.
        let view = governable_action_interface_view(vec![
            field("actionLabel", make_text("PauseTrading")),
            field("description", make_text("pause trading")),
            field("proposer", make_party(ALICE)),
            field("governanceParty", make_party(GOV)),
        ]);
        let created = proposal_created_event("unknown-pkg-cid", vec![view], None, None);
        let (proposal_cid, info) = extract_proposal_info(&created, &cid(GOV)).expect("parses");

        let mut infos = HashMap::new();
        infos.insert(proposal_cid, info);
        let actions = assemble_domain_actions(HashMap::new(), infos, true, true, 2, None, 0);

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].action_label, "PauseTrading");
        assert_eq!(actions[0].confirmation_count, 0);
    }

    #[test]
    fn a_synthesized_card_carries_every_proposal_field() {
        // The synthesis loop assigns each remaining field by hand, so each one
        // is asserted. Without this a swapped assignment would still pass.
        let mut infos = HashMap::new();
        infos.insert(
            "new-proposal-cid".to_string(),
            proposal_info_fixture(Some("SetupCcPreapproval")),
        );

        let actions = assemble_domain_actions(HashMap::new(), infos, true, true, 2, None, 0);

        assert_eq!(actions.len(), 1);
        let action = &actions[0];
        assert_eq!(action.proposal_cid, "new-proposal-cid");
        assert_eq!(action.action_label, "SetupCcPreapproval");
        assert_eq!(action.description, Some("a description".to_string()));
        assert_eq!(action.proposer, Some(cid(GOV)));
        assert_eq!(action.created_at, Some(1_700_000_000));
        assert!(action.transfer_details.is_none());
        assert!(action.accept_transfer_details.is_none());
        assert!(action.service_request_details.is_none());
        let fixture = proposal_info_fixture(Some("SetupCcPreapproval"));
        assert_eq!(action.template_id, fixture.template_id);
        assert_eq!(action.create_arguments, fixture.create_arguments);
    }

    #[test]
    fn a_confirmed_card_carries_its_proposal_contract_and_an_orphan_none() {
        let mut infos = HashMap::new();
        infos.insert(
            "live-cid".to_string(),
            proposal_info_fixture(Some("Action")),
        );
        let mut confirmations = confirmations_map(
            "live-cid",
            "Action",
            vec![domain_confirmation("c1", "live-cid", ALICE, 100, 0)],
        );
        confirmations.extend(confirmations_map(
            "gone-cid",
            "Action",
            vec![domain_confirmation("c2", "gone-cid", BOB, 100, 0)],
        ));

        let actions = assemble_domain_actions(confirmations, infos, true, true, 2, None, 0);

        let live = actions
            .iter()
            .find(|a| a.proposal_cid == "live-cid")
            .expect("the live proposal has a card");
        let fixture = proposal_info_fixture(Some("Action"));
        assert!(!live.orphaned);
        assert_eq!(live.template_id, fixture.template_id);
        assert_eq!(live.create_arguments, fixture.create_arguments);

        let orphan = actions
            .iter()
            .find(|a| a.proposal_cid == "gone-cid")
            .expect("the orphan keeps a dismiss-only card");
        assert!(orphan.orphaned);
        assert_eq!(orphan.template_id, None);
        assert_eq!(orphan.create_arguments, None);
    }

    #[test]
    fn fallback_label_is_proposal() {
        let mut infos = HashMap::new();
        infos.insert("cid-no-label".to_string(), proposal_info_fixture(None));

        let actions = assemble_domain_actions(HashMap::new(), infos, true, true, 1, None, 0);

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].action_label, "Proposal");
        assert_eq!(actions[0].action_label, FALLBACK_PROPOSAL_LABEL);
    }
}
