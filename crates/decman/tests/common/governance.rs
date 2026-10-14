use std::{collections::HashSet, time::Duration};

use anyhow::Context;
use dec_party_manager::server::{DomainGovernanceAction, GovernanceResponse};
use serde_json::{Value, json};

use crate::common::{Fixture, scenario::Scenario};

#[derive(Default)]
pub struct ProposalCycleCtx {
    pub proposal_cid: Option<String>,
    pub confirmation_cids: Vec<String>,
    /// Proposals already pending on P1 just before this cycle proposed.
    pending_before: HashSet<String>,
}

/// Builds the path for each confirmations read in the cycle. The path asks
/// for the endpoint's largest page, so one read lists every pending proposal.
/// [`whole_page`] fails the read when it does not.
pub fn all_pending_path(party_id: &str) -> String {
    format!("/governance/confirmations?party_id={party_id}&limit=200")
}

/// The pending proposals in `s`. Errors when the page was cut short, because a
/// partial list cannot show whether a proposal is pending or new.
pub fn whole_page(s: GovernanceResponse) -> anyhow::Result<Vec<DomainGovernanceAction>> {
    anyhow::ensure!(
        s.next_cursor.is_none(),
        "more proposals are pending than one /governance/confirmations page holds"
    );
    Ok(s.domain_actions)
}

/// Every pending proposal for `party_id` on the node at `port`. `None` while
/// the read fails, so the calling step polls again.
pub async fn probe_all_pending(
    f: &Fixture,
    port: u16,
    party_id: &str,
) -> Option<anyhow::Result<Vec<DomainGovernanceAction>>> {
    let s: GovernanceResponse = f.probe_get_json(port, &all_pending_path(party_id)).await?;
    Some(whole_page(s))
}

/// The cid of the one proposal labelled `label` that `before` lacks.
///
/// `Ok(None)` means it is not visible yet. Two or more is an error, because
/// the cycle cannot tell which one it created.
fn new_proposal_cid(
    before: &HashSet<String>,
    pending: &[DomainGovernanceAction],
    label: &str,
) -> anyhow::Result<Option<String>> {
    let mut fresh: Vec<&str> = pending
        .iter()
        .filter(|a| a.action_label == label && !before.contains(&a.proposal_cid))
        .map(|a| a.proposal_cid.as_str())
        .collect();
    match fresh.as_slice() {
        [] => Ok(None),
        [cid] => Ok(Some(cid.to_string())),
        _ => {
            fresh.sort_unstable();
            anyhow::bail!(
                "{} new proposals are labelled {label}, expected one: {fresh:?}",
                fresh.len()
            )
        }
    }
}

/// This enum names the decparty a governance cycle runs on.
///
/// The cycle sends every step to a fixed node. P1 proposes, P2 confirms and
/// P3 executes. A decparty therefore qualifies only when it has one member
/// party on each of those three nodes. Its threshold must also let two
/// confirmations execute an action.
///
/// The suite holds decparties that fail both tests. A two-member party has no
/// member on P3. A unanimous party needs three confirmations. Passing either
/// one produces a timeout on `can_execute`. That message names the wrong
/// cause.
#[derive(Clone)]
pub enum CycleParty {
    /// The suite's main decparty. Each step reads `f.party_id` and
    /// `f.rules_contract_id` when it runs.
    Primary,
    /// Any other decparty that meets the rules above. The caller supplies
    /// both ids.
    Named {
        party_id: String,
        rules_contract_id: String,
    },
}

impl CycleParty {
    /// Returns the party id and the rules contract id for this cycle.
    fn resolve(&self, f: &Fixture) -> anyhow::Result<(String, String)> {
        match self {
            CycleParty::Primary => Ok((
                f.party_id()?.to_string(),
                f.rules_contract_id()?.to_string(),
            )),
            CycleParty::Named {
                party_id,
                rules_contract_id,
            } => Ok((party_id.clone(), rules_contract_id.clone())),
        }
    }
}

/// Build a scenario that drives one propose → confirm → execute cycle on the
/// named decparty: P1 proposes, P2 confirms, P3 executes. The scenario asserts
/// that no pending action remains for this proposal.
///
/// Every step after the propose tracks the proposal by cid. The propose
/// endpoint returns no cid, so the cycle records the pending set first. It
/// then takes the one new proposal labelled `label`, and fails if there are
/// two or more.
///
/// The scenario includes cross-participant visibility steps. They stop a WHEN
/// step from running before the confirmer or the executor sees the proposal on
/// its own ledger view.
pub fn propose_confirm_execute_on(
    party: CycleParty,
    label: &str,
    proposal: Value,
) -> Scenario<ProposalCycleCtx> {
    let label = label.to_string();
    Scenario::with_ctx(label.clone(), ProposalCycleCtx::default())
        .given("party and governance rules contract present", {
            let party = party.clone();
            move |f, _ctx| {
                let party = party.clone();
                Box::pin(async move {
                    party.resolve(f)?;
                    Ok(())
                })
            }
        })
        .when(format!("P1 proposes {label}"), {
            let proposal = proposal.clone();
            let party = party.clone();
            move |f, ctx| {
                let proposal = proposal.clone();
                let party = party.clone();
                Box::pin(async move {
                    let (party_id, rules_contract_id) = party.resolve(f)?;
                    // The propose response carries no cid. Note what is
                    // already pending so the next step can pick out the new one.
                    let s: GovernanceResponse =
                        f.get_json(f.p1.http, &all_pending_path(&party_id)).await?;
                    ctx.pending_before =
                        whole_page(s)?.into_iter().map(|a| a.proposal_cid).collect();
                    let req = json!({
                        "party_id": party_id,
                        "rules_contract_id": rules_contract_id,
                        "proposal": proposal,
                    });
                    let _: Value = f.post_json(f.p1.http, "/governance/propose", &req).await?;
                    Ok(())
                })
            }
        })
        .then(
            "proposal visible in confirmations on P1",
            Duration::from_secs(60),
            {
                let label_p1 = label.clone();
                let party = party.clone();
                move |f, ctx| {
                    let label_p1 = label_p1.clone();
                    let party = party.clone();
                    Box::pin(async move {
                        let party_id = match party.resolve(f) {
                            Ok((p, _)) => p,
                            Err(e) => return Some(Err(e)),
                        };
                        // THIS cycle's proposal is the one with our label that
                        // was not pending before we proposed. A prior phase (e.g.
                        // notification_feed) can leave an unrelated proposal
                        // pending, so neither `len() == 1` nor the label alone is
                        // a safe gate.
                        let pending = match probe_all_pending(f, f.p1.http, &party_id).await? {
                            Ok(p) => p,
                            Err(e) => return Some(Err(e)),
                        };
                        match new_proposal_cid(&ctx.pending_before, &pending, &label_p1) {
                            Ok(Some(cid)) => {
                                ctx.proposal_cid = Some(cid);
                                Some(Ok(()))
                            }
                            Ok(None) => None,
                            Err(e) => Some(Err(e)),
                        }
                    })
                }
            },
        )
        .then("proposal visible on P2", Duration::from_secs(60), {
            let party = party.clone();
            move |f, ctx| {
                let party = party.clone();
                Box::pin(async move {
                    let cid = match ctx.proposal_cid.as_ref() {
                        Some(c) => c.clone(),
                        None => {
                            return Some(Err(anyhow::anyhow!(
                                "proposal_cid not set by previous step"
                            )));
                        }
                    };
                    let party_id = match party.resolve(f) {
                        Ok((p, _)) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let pending = match probe_all_pending(f, f.p2.http, &party_id).await? {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    pending
                        .iter()
                        .any(|a| a.proposal_cid == cid)
                        .then_some(Ok(()))
                })
            }
        })
        .when("P2 confirms", {
            let party = party.clone();
            move |f, ctx| {
                let party = party.clone();
                Box::pin(async move {
                    let proposal_cid = ctx
                        .proposal_cid
                        .as_deref()
                        .context("proposal_cid not set")?
                        .to_string();
                    let (party_id, rules_contract_id) = party.resolve(f)?;
                    let req = json!({
                        "party_id": party_id, "rules_contract_id": rules_contract_id,
                        "action": {"type": "governance_set_threshold", "new_threshold": 1},
                        "governance_type": "core_domain", "proposal_cid": proposal_cid,
                    });
                    let _: Value = f.post_json(f.p2.http, "/governance/confirm", &req).await?;
                    Ok(())
                })
            }
        })
        .then("can_execute=true on P1", Duration::from_secs(60), {
            let party = party.clone();
            move |f, ctx| {
                let party = party.clone();
                Box::pin(async move {
                    let party_id = match party.resolve(f) {
                        Ok((p, _)) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let pending = match probe_all_pending(f, f.p1.http, &party_id).await? {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e)),
                    };
                    let our_cid = ctx.proposal_cid.clone();
                    let action = pending
                        .into_iter()
                        .find(|a| Some(&a.proposal_cid) == our_cid.as_ref() && a.can_execute)?;
                    ctx.confirmation_cids = action
                        .confirmations
                        .iter()
                        .map(|c| c.contract_id.clone())
                        .collect();
                    Some(Ok(()))
                })
            }
        })
        .then(
            "proposal + confirmations visible on P3",
            Duration::from_secs(60),
            {
                let party = party.clone();
                move |f, ctx| {
                    let party = party.clone();
                    Box::pin(async move {
                        let cid = match ctx.proposal_cid.as_ref() {
                            Some(c) => c.clone(),
                            None => {
                                return Some(Err(anyhow::anyhow!(
                                    "proposal_cid not set by previous step"
                                )));
                            }
                        };
                        let party_id = match party.resolve(f) {
                            Ok((p, _)) => p,
                            Err(e) => return Some(Err(e)),
                        };
                        let pending = match probe_all_pending(f, f.p3.http, &party_id).await? {
                            Ok(p) => p,
                            Err(e) => return Some(Err(e)),
                        };
                        let action = pending
                            .into_iter()
                            .find(|a| a.proposal_cid == cid && a.can_execute)?;
                        ctx.confirmation_cids = action
                            .confirmations
                            .iter()
                            .map(|c| c.contract_id.clone())
                            .collect();
                        Some(Ok(()))
                    })
                }
            },
        )
        .when("P3 executes", {
            let party = party.clone();
            move |f, ctx| {
                let party = party.clone();
                Box::pin(async move {
                    let proposal_cid = ctx
                        .proposal_cid
                        .as_deref()
                        .context("proposal_cid not set")?
                        .to_string();
                    let confirmation_cids = ctx.confirmation_cids.clone();
                    let (party_id, rules_contract_id) = party.resolve(f)?;
                    let req = json!({
                        "party_id": party_id, "rules_contract_id": rules_contract_id,
                        "action": {"type": "governance_set_threshold", "new_threshold": 1},
                        "confirmation_cids": confirmation_cids, "disclosed_contracts": [],
                        "governance_type": "core_domain", "proposal_cid": proposal_cid,
                    });
                    let _: Value = f.post_json(f.p3.http, "/governance/execute", &req).await?;
                    Ok(())
                })
            }
        })
        .then(
            "this proposal no longer pending",
            Duration::from_secs(60),
            {
                let party = party.clone();
                move |f, ctx| {
                    let party = party.clone();
                    Box::pin(async move {
                        let party_id = match party.resolve(f) {
                            Ok((p, _)) => p,
                            Err(e) => return Some(Err(e)),
                        };
                        // A cut-short page fails the step: a cid missing from it
                        // would read as executed.
                        let pending = match probe_all_pending(f, f.p1.http, &party_id).await? {
                            Ok(p) => p,
                            Err(e) => return Some(Err(e)),
                        };
                        // This cycle is done when ITS proposal is gone (executed).
                        // Don't assert a globally empty slate — an unrelated prior
                        // proposal may still be pending (see the P1 visibility note).
                        let our_cid = ctx.proposal_cid.clone();
                        (!pending
                            .iter()
                            .any(|a| Some(&a.proposal_cid) == our_cid.as_ref()))
                        .then_some(Ok(()))
                    })
                }
            },
        )
}

/// Drive one cycle on the decparty the fixture holds.
pub fn propose_confirm_execute(label: &str, proposal: Value) -> Scenario<ProposalCycleCtx> {
    propose_confirm_execute_on(CycleParty::Primary, label, proposal)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pending action as `/governance/confirmations` sends it.
    fn action(cid: &str, label: &str) -> DomainGovernanceAction {
        serde_json::from_value(json!({
            "proposal_cid": cid,
            "action_label": label,
            "confirmations": [],
            "confirmation_count": 1,
            "executable_confirmation_cids": [],
            "can_execute": false,
        }))
        .expect("wire-shaped DomainGovernanceAction")
    }

    fn cids(list: &[&str]) -> HashSet<String> {
        list.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn new_proposal_cid_picks_the_one_new_proposal_with_the_label() {
        let before = cids(&["old-vote", "old-preapproval"]);
        let pending = [
            action("old-vote", "GenericVote"),
            action("old-preapproval", "SetupCcPreapproval"),
            action("new-preapproval", "SetupCcPreapproval"),
            action("new-vote", "GenericVote"),
        ];
        let got = new_proposal_cid(&before, &pending, "GenericVote").unwrap();
        assert_eq!(got.as_deref(), Some("new-vote"));
    }

    #[test]
    fn new_proposal_cid_waits_while_only_older_proposals_carry_the_label() {
        let before = cids(&["old-vote"]);
        let pending = [
            action("old-vote", "GenericVote"),
            action("new-preapproval", "SetupCcPreapproval"),
        ];
        assert_eq!(
            new_proposal_cid(&before, &pending, "GenericVote").unwrap(),
            None
        );
    }

    #[test]
    fn new_proposal_cid_rejects_two_new_proposals_with_the_label() {
        let pending = [
            action("new-b", "GenericVote"),
            action("new-a", "GenericVote"),
        ];
        let err = new_proposal_cid(&HashSet::new(), &pending, "GenericVote").unwrap_err();
        assert!(err.to_string().contains(r#"["new-a", "new-b"]"#), "{err}");
    }

    #[test]
    fn whole_page_rejects_a_page_with_more_to_follow() {
        let page = |next_cursor: Option<&str>| -> GovernanceResponse {
            serde_json::from_value(json!({
                "actions": [],
                "next_cursor": next_cursor,
                "domain_actions": [action("p1", "GenericVote")],
                "threshold": 2,
            }))
            .expect("wire-shaped GovernanceResponse")
        };
        assert!(whole_page(page(Some("eyJvZmZzZXQiOjF9"))).is_err());
        let pending = whole_page(page(None)).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].proposal_cid, "p1");
    }
}
