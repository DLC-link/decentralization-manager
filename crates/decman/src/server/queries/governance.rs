//! Governance confirmation queue and proposal paging.

use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
};

use canton_proto_rs::com::daml::ledger::api::v2::{GetEventsByContractIdRequest, Identifier};
use decman_lib::{
    catalog::{
        interpret::{
            self, DomainAction, ParsedConfirmation, ParsedDomainConfirmation, ProposalInfo,
        },
        templates::{governable_action_interface, governance_templates},
    },
    framework::TemplateId,
};

use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
    error::Result,
    server::{
        chain_audit::record_to_json,
        event_filters::{interface_filter, party_event_format, template_filter},
        ledger_paging::{AcsCursor, for_each_active_contract, page_active_contracts},
        types::{
            ActionType, DomainConfirmation, DomainGovernanceAction, GovernanceAction,
            GovernanceConfirmation, GovernanceState, ProposalSummary,
        },
    },
    utils,
    utils::now_secs,
};

// ============================================================================
// Governance Queries (with parsed actions)
// ============================================================================

/// Get governance confirmations for a decentralized party with parsed actions
///
/// Similar to get_governance_confirmations but parses the action field into ActionType
/// and groups by deterministic action hash.
/// One batch of a party's governance queue.
///
/// `limit` bounds how many proposals are read from the ledger per call, and the
/// returned cursor resumes the next batch. Reading a party's whole proposal set
/// in one request is what left the approvals feed spinning (#424). The
/// confirmations behind the batch come from a short-lived per-party cache, so
/// scrolling does not re-read them.
///
/// `rules` is the live rules contract, or `None` when it could not be read.
#[allow(clippy::too_many_arguments)]
pub async fn get_governance_confirmations(
    config: &NodeConfig,
    party_id: &CantonId,
    threshold: usize,
    rules: Option<&GovernanceState>,
    token: Option<String>,
    packages: &PackageConfig,
    limit: usize,
    cursor: Option<AcsCursor>,
) -> Result<GovernanceQueue> {
    // Collect confirmations grouped by action hash (core self-management)
    let mut confirmations_by_hash: HashMap<String, (ActionType, Vec<ParsedConfirmation>)> =
        HashMap::new();
    // Collect domain confirmations grouped by proposal CID (core domain actions)
    let mut domain_confirmations: HashMap<String, (String, Vec<ParsedDomainConfirmation>)> =
        HashMap::new();
    // Map of `contract_id -> ProposalInfo` for every active
    // `GovernableAction` proposal visible to this party on this participant.
    // The presence of a key here is what gates inclusion in `domain_actions`
    // below — `Confirmation`s referencing a proposal that's no longer active
    // (or never reached this participant's ACS) get filtered out, otherwise
    // surfacing them in the notification queue gives the user a Confirm
    // button that always 500s with `CONTRACT_NOT_FOUND` on the proposal cid.
    let mut proposal_infos: HashMap<String, ProposalInfo> = HashMap::new();
    // Whether `proposal_infos` reflects the full active-proposal set
    // for this party on this participant. If the proposal fetch errored we
    // can't tell orphans apart from "we just couldn't read the proposals", so
    // we skip orphan-marking below to avoid surfacing a flood of false
    // orphans to the user.
    // Whether `domain_confirmations` reflects every active on-ledger
    // `Governance.Confirmation:GovernanceConfirmation` contract for this party.
    // (The Rust value carrying each one is a `DomainConfirmation`; the Daml
    // template it comes from keeps its own name.) A confirmation query that fails leaves a confirmed
    // proposal looking untouched, and synthesizing it as a zero-confirmation
    // card would offer Confirm to a member who has already confirmed. Skip
    // synthesis in that case and wait for a refresh that reads cleanly.
    let mut domain_confirmations_complete = true;

    // Bounded, not cached: see read_domain_confirmations.
    let cached = match read_domain_confirmations(config, party_id, token.clone(), packages).await {
        Ok(read) => read,
        Err(e) => {
            tracing::warn!("Could not read confirmations: {e}");
            domain_confirmations_complete = false;
            (HashMap::new(), HashMap::new())
        }
    };
    confirmations_by_hash.extend(cached.0.clone());

    // One batch of proposals, not the party's whole history.
    let mut proposals_read_ok = true;
    let (batch, next_cursor) =
        match page_proposal_infos(config, party_id, token, packages, limit, cursor).await {
            Ok(page) => page,
            Err(e) => {
                // Warn, not debug: this drops every unconfirmed card from the page,
                // and the operator needs to know why the queue looks empty.
                tracing::warn!("Could not fetch proposal infos: {e}");
                proposals_read_ok = false;
                (Vec::new(), None)
            }
        };
    let proposal_infos_complete = proposals_read_ok;
    proposal_infos.extend(batch);

    // Only the confirmations belonging to this batch, so a card's count matches
    // the proposal beside it rather than the whole party's set.
    domain_confirmations.extend(
        proposal_infos
            .keys()
            .filter_map(|cid| cached.1.get(cid).map(|v| (cid.clone(), v.clone()))),
    );

    let now_seconds = now_secs();

    let members: Option<HashSet<CantonId>> =
        rules.map(|rules| rules.members.iter().cloned().collect());
    let actions = assemble_self_actions(
        confirmations_by_hash,
        threshold,
        members.as_ref(),
        now_seconds,
    );

    let domain_actions: Vec<DomainGovernanceAction> = interpret::assemble_domain_actions(
        domain_confirmations,
        proposal_infos,
        proposal_infos_complete,
        domain_confirmations_complete,
        threshold,
        members.as_ref(),
        now_seconds,
    )
    .into_iter()
    .map(|action| domain_action_dto(action, rules))
    .collect();

    Ok(GovernanceQueue {
        actions,
        domain_actions,
        next_cursor,
        complete: proposal_infos_complete && domain_confirmations_complete,
    })
}

/// One batch of a party's governance queue.
pub struct GovernanceQueue {
    pub actions: Vec<GovernanceAction>,
    pub domain_actions: Vec<DomainGovernanceAction>,
    pub next_cursor: Option<AcsCursor>,
    /// False when a read behind the batch failed and left cards out. A caller
    /// that counts the queue must not take an incomplete batch as empty.
    pub complete: bool,
}

/// Map an assembled domain proposal onto the wire DTO.
///
/// The payload is rendered from the create arguments the proposal read already
/// holds, so it adds no ledger read. `rules` is the live rules contract, or
/// `None` when it could not be read.
fn domain_action_dto(
    action: DomainAction,
    rules: Option<&GovernanceState>,
) -> DomainGovernanceAction {
    let not_authorized = proposer_not_authorized(action.proposer.as_ref(), rules);
    DomainGovernanceAction {
        proposal_cid: action.proposal_cid,
        action_label: action.action_label,
        description: action.description,
        confirmations: action
            .confirmations
            .into_iter()
            .map(domain_confirmation_dto)
            .collect(),
        confirmation_count: action.confirmation_count,
        executable_confirmation_cids: action.executable_confirmation_cids,
        can_execute: action.can_execute,
        orphaned: action.orphaned,
        transfer_details: action.transfer_details,
        accept_transfer_details: action.accept_transfer_details,
        service_request_details: action.service_request_details,
        proposer: action.proposer,
        proposer_not_authorized: not_authorized,
        created_at: action.created_at,
        proposal_template_id: action
            .template_id
            .map(|t| format!("{}:{}:{}", t.package_id, t.module_name, t.entity_name)),
        proposal_payload: action
            .create_arguments
            .is_some()
            .then(|| record_to_json(&action.create_arguments)),
    }
}

/// Whether `GovernanceRules_ConfirmAction` rejects every confirmation of a
/// proposal from `proposer`. That choice accepts a proposer only when the rules
/// contract lists it as a member or as an additional proposer.
///
/// An unknown reads as authorized, so the card keeps Confirm and the ledger
/// decides. That covers an unread rules contract, an unparsed proposer, and an
/// empty member set, which the template's `ensure` rules out on-ledger and so
/// can only mean a decode failure.
fn proposer_not_authorized(proposer: Option<&CantonId>, rules: Option<&GovernanceState>) -> bool {
    let (Some(proposer), Some(rules)) = (proposer, rules) else {
        return false;
    };
    !rules.members.is_empty()
        && !rules.members.contains(proposer)
        && !rules.additional_proposers.contains(proposer)
}

/// Group self-action confirmations into cards. Only executable confirmations
/// from members count toward `threshold`. The Execute also submits live
/// non-member confirmations: `ExecuteGovernanceAction` tolerates them and
/// consuming them clears additional-proposer markers.
fn assemble_self_actions(
    confirmations_by_hash: HashMap<String, (ActionType, Vec<ParsedConfirmation>)>,
    threshold: usize,
    members: Option<&HashSet<CantonId>>,
    now_seconds: i64,
) -> Vec<GovernanceAction> {
    confirmations_by_hash
        .into_iter()
        .map(|(action_hash, (action, confirmations))| {
            let unique_confirmations = interpret::dedupe_newest_per_party(
                confirmations,
                |c| &c.confirming_party,
                |c| c.created_at,
            );
            let confirmation_count = unique_confirmations
                .iter()
                .filter(|c| {
                    interpret::counts_toward_threshold(
                        &c.confirming_party,
                        c.expires_at,
                        members,
                        now_seconds,
                    )
                })
                .count();
            let executable_confirmation_cids = unique_confirmations
                .iter()
                .filter(|c| interpret::is_executable(c.expires_at, now_seconds))
                .map(|c| c.contract_id.clone())
                .collect();
            let last_confirmation_at = unique_confirmations
                .iter()
                .map(|c| c.created_at)
                .max()
                .unwrap_or(0);
            GovernanceAction {
                action_hash,
                action,
                confirmations: unique_confirmations
                    .into_iter()
                    .map(confirmation_dto)
                    .collect(),
                confirmation_count,
                executable_confirmation_cids,
                can_execute: confirmation_count >= threshold,
                last_confirmation_at,
            }
        })
        .collect()
}

/// Map a parsed confirmation onto the wire DTO.
fn confirmation_dto(parsed: ParsedConfirmation) -> GovernanceConfirmation {
    GovernanceConfirmation {
        contract_id: parsed.contract_id,
        action: parsed.action,
        confirming_party: parsed.confirming_party,
        created_at: parsed.created_at,
        expires_at: parsed.expires_at,
    }
}

/// Map a parsed domain confirmation onto the wire DTO. The on-ledger domain
/// confirmation carries no action, and neither does the DTO.
fn domain_confirmation_dto(parsed: ParsedDomainConfirmation) -> DomainConfirmation {
    DomainConfirmation {
        contract_id: parsed.contract_id,
        confirming_party: parsed.confirming_party,
        created_at: parsed.created_at,
        expires_at: parsed.expires_at,
    }
}

/// Fetch governance confirmations for a specific template
async fn fetch_governance_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
    confirmations_by_hash: &mut HashMap<String, (ActionType, Vec<ParsedConfirmation>)>,
    domain_confirmations: &mut HashMap<String, (String, Vec<ParsedDomainConfirmation>)>,
) -> Result {
    let event_format = party_event_format(
        party_id,
        vec![template_filter(template.into(), false)],
        true,
    );

    for_each_active_contract(
        config,
        token,
        event_format,
        Some(MAX_CONFIRMATIONS_SCANNED),
        |created| {
            if created.template_id.as_ref().is_some_and(|t| {
                t.module_name == "Governance.Confirmation"
                    && t.entity_name == "GovernanceConfirmation"
            }) {
                // Domain confirmations carry no inline action — they group by the
                // proposal cid they reference, labelled by whichever confirmation
                // for that proposal arrived first.
                if let Some(parsed) = interpret::parse_domain_confirmation(&created) {
                    domain_confirmations
                        .entry(parsed.proposal_cid.clone())
                        .or_insert_with(|| (parsed.action_label.clone(), Vec::new()))
                        .1
                        .push(parsed);
                }
            } else if let Some(parsed) = interpret::parse_confirmation(&created) {
                // By-value confirmations group by a deterministic hash of the
                // action they carry — the hash is decman's, not the lib's.
                let action_hash = compute_action_hash(&parsed.action);
                let action = parsed.action.clone();
                confirmations_by_hash
                    .entry(action_hash)
                    .or_insert_with(|| (action, Vec::new()))
                    .1
                    .push(parsed);
            }
        },
    )
    .await?;

    Ok(())
}

/// Resolve each `TransferInstruction` cid captured on
/// `AcceptTransferProposal`s into an `AcceptTransferDetails` and store it on
/// the corresponding `ProposalInfo`. Skips silently per-cid on failure — the
/// card just falls back to its cid-only rendering rather than blocking the
/// whole confirmations response on one bad instruction.
async fn resolve_accept_transfer_details(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    proposal_infos: &mut HashMap<String, ProposalInfo>,
) -> Result {
    let pending: Vec<(String, String)> = proposal_infos
        .iter()
        .filter_map(|(proposal_cid, info)| {
            if info.accept_transfer.is_some() {
                return None;
            }
            info.accept_transfer_instruction_cid
                .as_ref()
                .map(|cid| (proposal_cid.clone(), cid.clone()))
        })
        .collect();
    if pending.is_empty() {
        return Ok(());
    }

    let mut client = utils::create_event_query_client(config, token).await?;

    for (proposal_cid, instruction_cid) in pending {
        let request = GetEventsByContractIdRequest {
            contract_id: instruction_cid.clone(),
            event_format: Some(party_event_format(
                party_id,
                vec![interface_filter(
                    Identifier {
                        package_id: "#splice-api-token-transfer-instruction-v1".to_string(),
                        module_name: "Splice.Api.Token.TransferInstructionV1".to_string(),
                        entity_name: "TransferInstruction".to_string(),
                    },
                    false,
                )],
                true,
            )),
        };
        let created_event = match client
            .get_events_by_contract_id(tonic::Request::new(request))
            .await
        {
            Ok(resp) => resp.into_inner().created.and_then(|c| c.created_event),
            Err(e) => {
                tracing::debug!(
                    "Could not resolve TransferInstruction {instruction_cid} for proposal \
                     {proposal_cid}: {e}"
                );
                continue;
            }
        };
        let Some(created_event) = created_event else {
            continue;
        };
        if let Some(details) = interpret::extract_accept_transfer_details_from_view(&created_event)
            && let Some(info) = proposal_infos.get_mut(&proposal_cid)
        {
            info.accept_transfer = Some(details);
        }
    }
    Ok(())
}

/// Most `GovernanceConfirmation` contracts one read walks.
///
/// Confirmations are small individually but a load-tested party can hold tens
/// of thousands, and walking them all is what left the approvals feed spinning
/// (#424). Every proposal a member could act on has at most a handful, so this
/// is far above what a real queue needs.
const MAX_CONFIRMATIONS_SCANNED: usize = 5_000;

/// One batch of a party's open proposals, keyed by cid, plus where to resume.
async fn page_proposal_infos(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
    limit: usize,
    cursor: Option<AcsCursor>,
) -> Result<(Vec<(String, ProposalInfo)>, Option<AcsCursor>)> {
    let Ok(template) = governable_action_interface(packages) else {
        return Ok((Vec::new(), None));
    };

    let event_format = party_event_format(
        party_id,
        vec![interface_filter((&template).into(), false)],
        true,
    );

    let (items, next) = page_active_contracts(
        config,
        token.clone(),
        event_format,
        limit,
        cursor,
        |created| interpret::extract_proposal_info(&created, party_id),
    )
    .await?;

    // Resolve the linked `TransferInstruction`s for this batch only, so an
    // AcceptTransfer card still renders sender/amount/instrument.
    let mut map: HashMap<String, ProposalInfo> = items.into_iter().collect();
    resolve_accept_transfer_details(config, party_id, token, &mut map).await?;

    Ok((map.into_iter().collect(), next))
}

/// Live confirmation counts for a party, keyed by proposal cid.
///
/// Cached briefly: the walk covers every `GovernanceConfirmation` the party
/// holds, which is cheap per contract but not per scroll. Without this, each
/// batch of a paged feed would rescan the whole set.
/// A party's parsed confirmations: domain ones keyed by proposal cid, and
/// self-management ones keyed by action hash.
type ParsedConfirmations = (
    HashMap<String, (ActionType, Vec<ParsedConfirmation>)>,
    HashMap<String, (String, Vec<ParsedDomainConfirmation>)>,
);

/// Read a party's confirmations, bounded by [`MAX_CONFIRMATIONS_SCANNED`].
///
/// Deliberately uncached. These are small — a few hundred bytes each — and a
/// confirmation has to show up on the next read, not once a TTL lapses: caching
/// them for five minutes hid a peer's confirmation and left `can_execute` false
/// long after quorum was reached.
async fn read_domain_confirmations(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
) -> Result<ParsedConfirmations> {
    let mut by_hash: HashMap<String, (ActionType, Vec<ParsedConfirmation>)> = HashMap::new();
    let mut domain: HashMap<String, (String, Vec<ParsedDomainConfirmation>)> = HashMap::new();
    for t in &governance_templates(packages) {
        // A template this participant lacks means "no confirmations of that
        // kind", not a failed read.
        if let Err(e) = fetch_governance_for_template(
            config,
            party_id,
            token.clone(),
            t,
            &mut by_hash,
            &mut domain,
        )
        .await
        {
            tracing::debug!(%party_id, "confirmation read skipped {}:{}: {e}", t.module, t.entity);
        }
    }

    Ok((by_hash, domain))
}

/// One batch of a party's open proposals, plus where to resume.
///
/// Unlike [`fetch_proposal_infos`], this reads only as far as `limit` and hands
/// back a cursor, so a caller can show a batch while fetching the next instead
/// of waiting on a party's whole proposal history (#424).
pub async fn page_proposal_summaries(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
    limit: usize,
    cursor: Option<AcsCursor>,
) -> Result<(Vec<ProposalSummary>, Option<AcsCursor>)> {
    let Ok(template) = governable_action_interface(packages) else {
        return Ok((Vec::new(), None));
    };

    let event_format = party_event_format(
        party_id,
        vec![interface_filter((&template).into(), false)],
        true,
    );

    let token_for_counts = token.clone();
    let (summaries, next) =
        page_active_contracts(config, token, event_format, limit, cursor, |created| {
            interpret::extract_proposal_info(&created, party_id).map(|(cid, info)| {
                ProposalSummary {
                    proposal_cid: cid,
                    // Filled in below, once the counts map is resolved.
                    confirmation_count: 0,
                    action_label: info.action_label,
                    description: info.description,
                    proposer: info.proposer,
                    created_at: info.created_at,
                }
            })
        })
        .await?;

    let now_seconds = now_secs();
    let confirmations = read_domain_confirmations(config, party_id, token_for_counts, packages)
        .await
        .unwrap_or_default();
    let summaries = summaries
        .into_iter()
        .map(|mut p| {
            p.confirmation_count = confirmations
                .1
                .get(&p.proposal_cid)
                .map(|(_label, list)| {
                    let unique = interpret::dedupe_newest_per_party(
                        list.clone(),
                        |c| &c.confirming_party,
                        |c| c.created_at,
                    );
                    interpret::live_count(&unique, |c| c.expires_at, now_seconds)
                })
                .unwrap_or(0);
            p
        })
        .collect();

    Ok((summaries, next))
}

/// Compute a deterministic hash of an action for grouping confirmations
fn compute_action_hash(action: &ActionType) -> String {
    // Serialize to JSON for deterministic representation
    let json = serde_json::to_string(action).unwrap_or_default();

    let mut hasher = DefaultHasher::new();
    json.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::daml::ledger::api::v2::{
        CreatedEvent, InterfaceView, Record, TextMap, Value, text_map, value,
    };

    use super::{super::test_values::*, *};

    #[test]
    fn self_actions_count_only_executable_member_confirmations() -> Result {
        let now = 1_700_000_000;
        let alice: CantonId =
            "alice::1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .parse()?;
        let bob: CantonId =
            "bob::1220bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".parse()?;
        let proposer: CantonId =
            "carol::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                .parse()?;
        let members: HashSet<CantonId> = [alice.clone(), bob.clone()].into_iter().collect();
        let action = ActionType::GovernanceSetThreshold { new_threshold: 2 };
        let confirmation = |contract_id: &str, party: &CantonId, expires_at| ParsedConfirmation {
            contract_id: contract_id.to_string(),
            action: action.clone(),
            confirming_party: party.clone(),
            created_at: 100,
            expires_at,
        };
        let confirmations = vec![
            confirmation("member", &alice, now + 3600),
            confirmation(
                "near-expiry",
                &bob,
                now + interpret::EXECUTE_EXPIRY_MARGIN_SECONDS,
            ),
            confirmation("proposer", &proposer, now + 3600),
        ];
        let mut by_hash = HashMap::new();
        by_hash.insert("hash".to_string(), (action.clone(), confirmations));

        let actions = assemble_self_actions(by_hash, 2, Some(&members), now);

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].confirmations.len(), 3);
        assert_eq!(actions[0].confirmation_count, 1);
        assert!(!actions[0].can_execute);
        let mut cids = actions[0].executable_confirmation_cids.clone();
        cids.sort();
        assert_eq!(cids, vec!["member".to_string(), "proposer".to_string()]);

        Ok(())
    }

    // ------------------------------------------------------------------------
    // proposer_not_authorized
    //
    // Mirrors the proposer check in `GovernanceRules_ConfirmAction`
    // (daml/governance-core/daml/Governance/Rules.daml).
    // ------------------------------------------------------------------------

    fn party(hint: &str, namespace_byte: &str) -> Result<CantonId> {
        format!("{hint}::1220{}", namespace_byte.repeat(32)).parse()
    }

    fn rules_with(
        members: &[&CantonId],
        additional_proposers: &[&CantonId],
    ) -> Result<GovernanceState> {
        Ok(GovernanceState {
            contract_id: "00rules".to_string(),
            governance_party: party("gov", "00")?,
            members: members.iter().map(|p| (*p).clone()).collect(),
            threshold: 1,
            additional_proposers: additional_proposers.iter().map(|p| (*p).clone()).collect(),
            disclosure_observers: vec![],
            action_confirmation_timeout_microseconds: None,
            package_ref: None,
            out_of_date: false,
        })
    }

    #[test]
    fn members_and_additional_proposers_may_propose() -> Result {
        let member = party("member", "aa")?;
        let agent = party("agent", "bb")?;
        let rules = rules_with(&[&member], &[&agent])?;

        assert!(!proposer_not_authorized(Some(&member), Some(&rules)));
        assert!(!proposer_not_authorized(Some(&agent), Some(&rules)));
        Ok(())
    }

    #[test]
    fn a_proposer_the_live_rules_no_longer_list_cannot_gather_confirmations() -> Result {
        // The #518 case: the proposal came from an earlier seat's paying agent,
        // and the live rules authorize a different one.
        let member = party("member", "aa")?;
        let former_agent = party("agent", "bb")?;
        let current_agent = party("agent", "cc")?;
        let rules = rules_with(&[&member], &[&current_agent])?;

        assert!(proposer_not_authorized(Some(&former_agent), Some(&rules)));
        Ok(())
    }

    #[test]
    fn a_party_sharing_a_members_namespace_is_not_that_member() -> Result {
        // Daml compares whole party ids. Parties allocated on one participant
        // share its namespace, so a sibling party must not pass as the member.
        let member = party("member", "aa")?;
        let sibling = party("operator", "aa")?;
        let rules = rules_with(&[&member], &[])?;

        assert!(proposer_not_authorized(Some(&sibling), Some(&rules)));
        Ok(())
    }

    #[test]
    fn an_unknown_never_withholds_confirm() -> Result {
        let member = party("member", "aa")?;
        let stranger = party("stranger", "dd")?;
        let rules = rules_with(&[&member], &[])?;
        // `ensure not (Set.null members)` holds on-ledger, so no members means
        // the decode failed, not that nobody may propose.
        let undecoded_rules = rules_with(&[], &[])?;

        // The rules contract could not be read.
        assert!(!proposer_not_authorized(Some(&stranger), None));
        // The proposal named no proposer we could parse.
        assert!(!proposer_not_authorized(None, Some(&rules)));
        assert!(!proposer_not_authorized(
            Some(&stranger),
            Some(&undecoded_rules)
        ));
        Ok(())
    }

    // ------------------------------------------------------------------------
    // domain_action_dto
    //
    // Review shows `proposal_payload` as the whole of what a member confirms
    // or executes, so it has to carry every create argument of the proposal.
    // ------------------------------------------------------------------------

    fn contract_id_value(cid: &str) -> Value {
        Value {
            sum: Some(value::Sum::ContractId(cid.to_string())),
        }
    }

    fn text_map_value(entries: &[(&str, Value)]) -> Value {
        Value {
            sum: Some(value::Sum::TextMap(TextMap {
                entries: entries
                    .iter()
                    .map(|(key, value)| text_map::Entry {
                        key: (*key).to_string(),
                        value: Some(value.clone()),
                    })
                    .collect(),
            })),
        }
    }

    /// A `TransferProposal` the way the proposal read returns it: the
    /// `GovernableAction` view beside the template's create arguments.
    fn transfer_proposal_event(gov: &CantonId, proposer: &CantonId) -> Result<CreatedEvent> {
        let admin = party("dso", "dd")?.to_string();
        let receiver = party("bob", "ee")?.to_string();
        let transfer = record_value(vec![
            field("sender", party_value(&gov.to_string())),
            field("receiver", party_value(&receiver)),
            field("amount", numeric_value("12.5000000000")),
            field(
                "instrumentId",
                record_value(vec![
                    field("admin", party_value(&admin)),
                    field("id", text_value("Amulet")),
                ]),
            ),
            field("requestedAt", timestamp_value(1_700_000_000_000_000)),
            field("executeBefore", timestamp_value(1_700_086_400_000_000)),
            field(
                "inputHoldingCids",
                list_value(vec![contract_id_value("00holding")]),
            ),
            field(
                "meta",
                record_value(vec![field(
                    "values",
                    text_map_value(&[("memo", text_value("March rent"))]),
                )]),
            ),
        ]);
        let empty_metadata = || record_value(vec![field("values", text_map_value(&[]))]);
        Ok(CreatedEvent {
            contract_id: "00transfer-proposal".to_string(),
            template_id: Some(Identifier {
                package_id: "4d1f".to_string(),
                module_name: "Governance.TokenCustody.TransferProposal".to_string(),
                entity_name: "TransferProposal".to_string(),
            }),
            create_arguments: Some(Record {
                record_id: None,
                fields: vec![
                    field("governanceParty", party_value(&gov.to_string())),
                    field("proposer", party_value(&proposer.to_string())),
                    field("transferFactoryCid", contract_id_value("00factory")),
                    field("expectedAdmin", party_value(&admin)),
                    field("transfer", transfer),
                    field(
                        "extraArgs",
                        record_value(vec![
                            field("context", empty_metadata()),
                            field("meta", empty_metadata()),
                        ]),
                    ),
                ],
            }),
            interface_views: vec![InterfaceView {
                interface_id: Some(Identifier {
                    package_id: "#governance-action".to_string(),
                    module_name: "Governance.Action".to_string(),
                    entity_name: "GovernableAction".to_string(),
                }),
                view_status: None,
                view_value: Some(Record {
                    record_id: None,
                    fields: vec![
                        field("governanceParty", party_value(&gov.to_string())),
                        field("proposer", party_value(&proposer.to_string())),
                        field("actionLabel", text_value("Transfer")),
                        field("description", text_value("Pay March rent")),
                    ],
                }),
                implementation_package_id: String::new(),
            }],
            created_at: Some(prost_types::Timestamp {
                seconds: 1_700_000_000,
                nanos: 0,
            }),
            ..Default::default()
        })
    }

    #[test]
    fn a_domain_action_carries_every_create_argument_as_json() -> Result {
        let gov = party("gov", "aa")?;
        let proposer = party("member", "bb")?;
        let created = transfer_proposal_event(&gov, &proposer)?;
        let (cid, info) = interpret::extract_proposal_info(&created, &gov)
            .ok_or_else(|| anyhow::anyhow!("a proposal our party governs parses"))?;

        let actions = interpret::assemble_domain_actions(
            HashMap::new(),
            HashMap::from([(cid, info)]),
            true,
            true,
            2,
            None,
            0,
        );
        let action = actions
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("the unconfirmed proposal gets a card"))?;
        let dto = domain_action_dto(action, None);

        assert_eq!(dto.proposal_cid, "00transfer-proposal");
        assert_eq!(
            dto.proposal_template_id.as_deref(),
            Some("4d1f:Governance.TokenCustody.TransferProposal:TransferProposal")
        );
        let admin = party("dso", "dd")?.to_string();
        assert_eq!(
            dto.proposal_payload,
            Some(serde_json::json!({
                "governanceParty": gov.to_string(),
                "proposer": proposer.to_string(),
                "transferFactoryCid": "00factory",
                "expectedAdmin": admin,
                "transfer": {
                    "sender": gov.to_string(),
                    "receiver": party("bob", "ee")?.to_string(),
                    "amount": "12.5000000000",
                    "instrumentId": { "admin": admin, "id": "Amulet" },
                    "requestedAt": 1_700_000_000_000_000_i64,
                    "executeBefore": 1_700_086_400_000_000_i64,
                    "inputHoldingCids": ["00holding"],
                    "meta": { "values": { "memo": "March rent" } },
                },
                "extraArgs": {
                    "context": { "values": {} },
                    "meta": { "values": {} },
                },
            }))
        );
        Ok(())
    }

    #[test]
    fn an_orphaned_domain_action_has_no_payload() -> Result {
        let confirmations = HashMap::from([(
            "00gone".to_string(),
            (
                "Transfer".to_string(),
                vec![ParsedDomainConfirmation {
                    contract_id: "00conf".to_string(),
                    proposal_cid: "00gone".to_string(),
                    action_label: "Transfer".to_string(),
                    confirming_party: party("member", "bb")?,
                    created_at: 100,
                    expires_at: 0,
                }],
            ),
        )]);

        let actions = interpret::assemble_domain_actions(
            confirmations,
            HashMap::new(),
            true,
            true,
            2,
            None,
            0,
        );
        let action = actions
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("the orphan keeps a dismiss-only card"))?;
        let dto = domain_action_dto(action, None);

        assert!(dto.orphaned);
        assert_eq!(dto.proposal_template_id, None);
        assert_eq!(dto.proposal_payload, None);
        let wire = serde_json::to_value(&dto)?;
        assert!(
            wire.get("proposal_payload").is_none() && wire.get("proposal_template_id").is_none(),
            "an orphan omits both fields on the wire: {wire}"
        );
        Ok(())
    }
}
