//! The propose endpoint.

use actix_web::{HttpRequest, HttpResponse, Responder, post, web};

use canton_proto_rs::com::daml::ledger::api::v2::{
    SubmitAndWaitForTransactionRequest, SubmitAndWaitRequest,
    command_service_client::CommandServiceClient,
};
use decman_lib::catalog::commands::build_confirm_proposal;
use decman_lib::catalog::proposals::custody::{
    AcceptTransfer, AcceptTransferWithContext, Transfer, TransferWithContext,
};
use decman_lib::catalog::proposals::rewards::SetupCouponReassignmentDelegation;
use decman_lib::catalog::proposals::utility::WithCredentialIdNonce;
use decman_lib::catalog::templates::governance_rules_template;
use decman_lib::framework::commands::{build_propose, first_created_contract_id};
use decman_lib::framework::encode::TransferValidity;

use crate::{
    canton_id::CantonId,
    error::Result,
    server::{
        AppState,
        audit::{AuditEvent, AuditParams, spawn_audit_log, with_proposal_cid},
        middleware::require_admin,
        queries::{resolve_contract_package_ref, select_input_holdings},
        transfer_context::{
            AcceptTransferContext, ProposeTransferArgs, fetch as fetch_accept_transfer_context,
            fetch_factory_for_propose, needs_registry_context,
        },
        types::{
            ActionType, ErrorResponse, GovernanceType, MessageResponse, ProposalType,
            ProposeActionRequest,
        },
    },
    utils,
};

use super::{
    credentials::{PartySession, packages},
    metrics::{ActionOutcome, ActionStep, count_action},
};

// ============================================================================
// Action Endpoints
// ============================================================================

/// True when this proposal, on a decparty that already has a live
/// `CouponReassignmentDelegation`, would create a SECOND one.
///
/// Canton cannot enforce the per-decparty singleton — contract keys do not give
/// cross-participant uniqueness — and `executeImpl` cannot query the ACS, so the
/// propose boundary is the only place the accident is catchable. Replacing an
/// existing delegation is fine; leaving `prior_delegation` empty while one is
/// live is not, because an assigner can still pay the superseded split.
///
/// This is the proposal shape alone — the handler pairs it with the ACS read.
/// Keeping the shape test separate is what lets `propose_action` skip that read
/// entirely for every other proposal type, which would otherwise pay a
/// `GetLedgerEnd` plus a `GetActiveContracts` for an answer that cannot change.
fn may_create_second_delegation(proposal: &ProposalType) -> bool {
    matches!(
        proposal,
        ProposalType::SetupCouponReassignmentDelegation(SetupCouponReassignmentDelegation {
            prior_delegation: None,
            ..
        })
    )
}

/// A fresh nonce for a proposal whose template mints governance
/// credentials: the first 12 hex digits of a v4 UUID, which carry 48 random
/// bits. The template folds it into each credential id, so two such
/// proposals that mint for one subject at one ledger time still mint
/// distinct ids.
fn credential_id_nonce() -> String {
    let mut nonce = uuid::Uuid::new_v4().simple().to_string();
    nonce.truncate(12);
    nonce
}

/// Propose a domain governance action (creates a GovernableAction proposal contract)
#[utoipa::path(
    tag = "Governance",
    request_body = ProposeActionRequest,
    responses(
        (status = 200, description = "Proposal created", body = MessageResponse),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden: admin role required", body = ErrorResponse),
        (status = 404, description = "No credentials configured for party", body = ErrorResponse),
        (status = 409, description = "A CouponReassignmentDelegation is already active; set prior_delegation to replace it", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
        (status = 503, description = "Node not provisioned for this proposal: the target package is not configured (the response says so, and whether the proposal was already created), or it cannot be confirmed whether a CouponReassignmentDelegation is already active", body = ErrorResponse)
    )
)]
#[post("/governance/propose")]
pub async fn propose_action(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    body: web::Json<ProposeActionRequest>,
) -> impl Responder {
    if let Err(resp) = require_admin(&http_req, data.admin_role.as_deref()) {
        return resp;
    }
    if let Err(msg) = body.proposal.validate(&body.party_id) {
        return HttpResponse::BadRequest().json(ErrorResponse {
            error: msg.to_string(),
        });
    }
    let party_id = &body.party_id;
    let PartySession {
        token,
        member_party_id,
    } = match PartySession::load(&data, party_id).await {
        Ok(session) => session,
        Err(e) => return e.into_response(),
    };

    // Canton cannot enforce the per-decparty singleton, so the propose boundary
    // is the only place a second CouponReassignmentDelegation is catchable.
    //
    // Match on the proposal type BEFORE reading the ledger: only one variant can
    // trip this guard, and the read is a GetLedgerEnd plus a GetActiveContracts
    // that every other propose would otherwise pay for nothing.
    if may_create_second_delegation(&body.proposal) {
        match crate::server::reward_automation::active_delegation(
            &data.config,
            &packages(),
            data.test_mode,
            party_id,
            &token,
        )
        .await
        {
            Ok(Some(active)) => {
                return HttpResponse::Conflict().json(ErrorResponse {
                    error: format!(
                        "a CouponReassignmentDelegation is already active ({}); set \
                         prior_delegation to replace it — creating a second one stops \
                         assignment entirely",
                        active.cid
                    ),
                });
            }
            Ok(None) => {}
            // Do not swallow this. The guard exists for exactly the state a
            // failed read leaves us blind to, so let the proposer see it rather
            // than admitting a second delegation on a transport blip.
            Err(e) => {
                tracing::warn!(%party_id, error = %e, "delegation singleton check failed");
                return HttpResponse::ServiceUnavailable().json(ErrorResponse {
                    error: format!(
                        "cannot confirm whether a CouponReassignmentDelegation is already \
                         active, so this proposal is refused rather than risk a second \
                         one: {e}"
                    ),
                });
            }
        }
    }

    let audit_pool = data.db.clone();
    let audit_summary = crate::server::audit::proposal_summary(&body.proposal);
    let audit_details = serde_json::to_string(&*body).unwrap_or_default();
    let audit_party_id = party_id.clone();
    let audit_member = member_party_id.clone();

    let packages = packages();

    // Resolve registry-backed context for token-standard transfer flows:
    //   * `AcceptTransfer`: fetch the `transfer-rule` choice context the
    //     `TransferInstruction_Accept` choice reads at execute time. Without it
    //     execute fails with `Missing context entry for
    //     utility.digitalasset.com/transfer-rule`.
    //   * `Transfer` of a utility-registry instrument: `TransferFactory_Transfer`
    //     reads `utility.digitalasset.com/instrument-configuration` from
    //     `extraArgs.context.values` at execute time, so the context must be
    //     fetched from the registrar and baked into the proposal regardless of
    //     whether the dec party administers the instrument. For shared
    //     instruments (e.g. CBTC, admin = `cbtc-network`) the factory isn't on
    //     the dec party's ACS, so we also substitute the resolved factory cid.
    //     Canton Coin is excluded — its `AmuletRules` factory and context come
    //     from the DSO scan API. See `needs_registry_context`.
    // Bounded validity window for any Transfer proposal, captured ONCE so the
    // registry choice-context fetch and the on-chain create args agree
    // byte-for-byte. A bounded `executeBefore` lets an unaccepted two-step offer
    // expire and release its escrow instead of locking funds forever. The
    // window defaults to 24h but the caller may override it per-transfer.
    let now_micros = chrono::Utc::now().timestamp_micros();
    let transfer_validity = match &body.proposal {
        ProposalType::Transfer(Transfer {
            validity_window_hours: Some(hours),
            ..
        }) => TransferValidity::from_now_with_window(
            now_micros,
            i64::from(*hours).saturating_mul(60 * 60 * 1_000_000),
        ),
        _ => TransferValidity::from_now(now_micros),
    };

    let mut resolved_proposal = body.proposal.clone();
    let transfer_choice_context = match &mut resolved_proposal {
        ProposalType::AcceptTransfer(AcceptTransfer {
            transfer_instruction_cid,
        }) => match fetch_accept_transfer_context(
            &data.config,
            Some(token.clone()),
            data.config.canton.network,
            party_id,
            transfer_instruction_cid,
        )
        .await
        {
            Ok(ctx) => Some(ctx),
            Err(e) => {
                tracing::warn!(
                    "Failed to fetch AcceptTransfer choice context from registry: {e:#}"
                );
                return HttpResponse::BadGateway().json(ErrorResponse {
                    error: format!("Failed to fetch transfer choice context: {e}"),
                });
            }
        },
        ProposalType::Transfer(Transfer {
            transfer_factory_cid,
            receiver,
            amount,
            instrument_id,
            input_holding_cids,
            ..
        }) if needs_registry_context(
            transfer_factory_cid,
            &instrument_id.admin,
            &party_id.to_string(),
        ) =>
        {
            let admin: CantonId = match instrument_id.admin.parse() {
                Ok(p) => p,
                Err(e) => {
                    return HttpResponse::BadRequest().json(ErrorResponse {
                        error: format!("Invalid instrument admin party id: {e}"),
                    });
                }
            };
            // The token-standard transfer factory rejects an empty
            // `inputHoldingCids` ("No holdings provided"). When the caller
            // didn't pin specific holdings, fund the transfer with every
            // Holding the sender owns for this instrument and let the choice
            // consume what it needs (returning change).
            if input_holding_cids.is_empty() {
                match select_input_holdings(
                    &data.config,
                    party_id,
                    Some(token.clone()),
                    &admin,
                    &instrument_id.id,
                )
                .await
                {
                    Ok(cids) if cids.is_empty() => {
                        return HttpResponse::BadRequest().json(ErrorResponse {
                            error: format!(
                                "No holdings of instrument {} owned by {} to fund the transfer",
                                instrument_id.id, party_id
                            ),
                        });
                    }
                    Ok(cids) => *input_holding_cids = cids,
                    Err(e) => {
                        tracing::warn!("Failed to select input holdings for transfer: {e:#}");
                        return HttpResponse::InternalServerError().json(ErrorResponse {
                            error: format!("Failed to select input holdings: {e}"),
                        });
                    }
                }
            }
            match fetch_factory_for_propose(
                data.config.canton.network,
                ProposeTransferArgs {
                    sender: party_id,
                    receiver,
                    amount,
                    instrument_admin: &admin,
                    instrument_id: &instrument_id.id,
                    input_holding_cids,
                    requested_at_micros: transfer_validity.requested_at_micros,
                    execute_before_micros: transfer_validity.execute_before_micros,
                },
            )
            .await
            {
                Ok(resolved) => {
                    // Self-administered utility tokens already carry the factory
                    // cid the UI read from the dec party's ACS; only fill it in
                    // for shared instruments where the UI left it empty.
                    if transfer_factory_cid.is_empty() {
                        *transfer_factory_cid = resolved.factory_cid;
                    }
                    Some(AcceptTransferContext {
                        context: resolved.context,
                        disclosed_contracts: resolved.disclosed_contracts,
                    })
                }
                Err(e) => {
                    tracing::warn!("Failed to fetch Transfer choice context from registry: {e:#}");
                    return HttpResponse::BadGateway().json(ErrorResponse {
                        error: format!("Failed to fetch transfer factory: {e}"),
                    });
                }
            }
        }
        _ => None,
    };

    // The two transfer variants carry runtime data that is not a payload
    // field — the registry choice context, the validity window, and the
    // on-chain `sender` (the governance party) — so they submit through
    // their wrapper structs. The credential-minting variants carry a fresh
    // credential id nonce through theirs. Every other variant is its own
    // complete payload, reached through `grpc_payload()` without
    // re-listing every arm.
    let command_id = uuid::Uuid::new_v4().to_string();
    let commands = match &resolved_proposal {
        ProposalType::Transfer(t) => build_propose(
            &TransferWithContext {
                transfer: t,
                sender: party_id,
                context: transfer_choice_context.as_ref().map(|r| &r.context),
                validity: transfer_validity,
            },
            party_id,
            &member_party_id,
            &packages,
            command_id,
        ),
        ProposalType::AcceptTransfer(a) => build_propose(
            &AcceptTransferWithContext {
                accept: a,
                context: transfer_choice_context.as_ref().map(|r| &r.context),
            },
            party_id,
            &member_party_id,
            &packages,
            command_id,
        ),
        other => match (other.credential_minting_payload(), other.grpc_payload()) {
            (Some(p), _) => build_propose(
                &WithCredentialIdNonce {
                    proposal: p,
                    nonce: &credential_id_nonce(),
                },
                party_id,
                &member_party_id,
                &packages,
                command_id,
            ),
            (None, Some(p)) => build_propose(p, party_id, &member_party_id, &packages, command_id),
            // No variant reaches this today: `grpc_payload` returns `None`
            // only for the two transfer variants, and both match above. A
            // new variant that returns `None` gets a 500, not a panic.
            (None, None) => {
                tracing::error!("Proposal variant has no gRPC payload: {other:?}");
                return HttpResponse::InternalServerError().json(ErrorResponse {
                    error: "Unsupported proposal type".to_string(),
                });
            }
        },
    };
    // An unconfigured package is a provisioning gap, not bad input: 503 with
    // the lib's own `"<pkg> package not configured"` text, byte-identical to
    // what the hand-written package match returned.
    let commands = match commands {
        Ok(c) => c,
        Err(e @ decman_lib::Error::PackageNotConfigured(_)) => {
            return HttpResponse::ServiceUnavailable().json(ErrorResponse {
                error: e.to_string(),
            });
        }
        Err(e) => {
            return HttpResponse::BadRequest().json(ErrorResponse {
                error: format!("Failed to build proposal create arguments: {e}"),
            });
        }
    };

    let channel = match data.config.ledger_channel().await {
        Ok(channel) => channel,
        Err(e) => {
            count_action(ActionStep::Propose, ActionOutcome::Error);
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to connect to ledger API: {e:#}"),
            });
        }
    };

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    // Step 1: Create the proposal and get the contract ID back
    let mut create_req = tonic::Request::new(SubmitAndWaitForTransactionRequest {
        commands: Some(commands),
        transaction_format: None,
    });
    create_req
        .metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());

    let proposal_cid = match client.submit_and_wait_for_transaction(create_req).await {
        Ok(response) => {
            // Extract created contract ID from the transaction events
            match response
                .into_inner()
                .transaction
                .as_ref()
                .and_then(first_created_contract_id)
            {
                Some(cid) => cid,
                None => {
                    count_action(ActionStep::Propose, ActionOutcome::Error);
                    return HttpResponse::InternalServerError().json(ErrorResponse {
                        error: "Proposal created but could not extract contract ID".to_string(),
                    });
                }
            }
        }
        Err(e) => {
            tracing::error!("Failed to create proposal: {e}");
            count_action(ActionStep::Propose, ActionOutcome::Error);
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Propose,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: GovernanceType::CoreDomain,
                    action_summary: audit_summary,
                    details: audit_details,
                    status: "failed",
                    error_message: Some(format!("Failed to create proposal: {e}")),
                },
            );
            return HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to create proposal: {e}"),
            });
        }
    };

    tracing::info!("Proposal created with CID: {proposal_cid}");

    // Step 2: Immediately confirm the proposal as the proposer
    //
    // Reachable independently of the Step 1 checks: those only verify the
    // package the proposal type itself needs, so a rewards / token-custody /
    // utility proposal reaches here on a node with no governance_core. The
    // proposal is already on the ledger by this point, so the body has to say
    // so — the caller must not retry into a second proposal.
    let mut rules = match governance_rules_template(&packages) {
        Ok(rules) => rules,
        Err(_) => {
            count_action(ActionStep::Propose, ActionOutcome::Error);
            return HttpResponse::ServiceUnavailable().json(ErrorResponse {
                error: format!(
                    "Proposal {proposal_cid} was created but could not be confirmed: \
                     governance_core package not configured. Configure it and confirm \
                     the existing proposal; do not re-propose."
                ),
            });
        }
    };

    // The rules contract may be an out-of-date fallback living under an older
    // governance-core package — exercise it under its actual package ref.
    rules.package_ref = resolve_contract_package_ref(
        &data.config,
        party_id,
        Some(token.clone()),
        &body.rules_contract_id,
        &rules.package_ref,
    )
    .await;

    let confirm_commands = build_confirm_proposal(
        &rules,
        &body.rules_contract_id,
        &member_party_id,
        party_id,
        &proposal_cid,
        uuid::Uuid::new_v4().to_string(),
    );

    let mut confirm_req = tonic::Request::new(SubmitAndWaitRequest {
        commands: Some(confirm_commands),
    });
    confirm_req
        .metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());

    match client.submit_and_wait(confirm_req).await {
        Ok(_) => {
            tracing::info!("Proposal {proposal_cid} confirmed by proposer");
            count_action(ActionStep::Propose, ActionOutcome::Ok);
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Propose,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: GovernanceType::CoreDomain,
                    action_summary: audit_summary,
                    details: with_proposal_cid(&audit_details, &proposal_cid),
                    status: "success",
                    error_message: None,
                },
            );
            HttpResponse::Ok().json(MessageResponse {
                message: "Proposal created and confirmed".to_string(),
            })
        }
        Err(e) => {
            tracing::error!("Proposal created but confirmation failed: {e}");
            count_action(ActionStep::Propose, ActionOutcome::Error);
            spawn_audit_log(
                audit_pool,
                AuditParams {
                    event_type: AuditEvent::Propose,
                    party_id: audit_party_id,
                    member_party_id: audit_member,
                    governance_type: GovernanceType::CoreDomain,
                    action_summary: audit_summary,
                    details: with_proposal_cid(&audit_details, &proposal_cid),
                    status: "failed",
                    error_message: Some(format!("Proposal created but confirmation failed: {e}")),
                },
            );
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!(
                    "Proposal created (CID: {proposal_cid}) but confirmation failed: {e}"
                ),
            })
        }
    }
}

/// Reject an inline action the `core_self` path cannot carry, before any
/// ledger work happens.
///
/// The inline confirm/execute choices serialize a `GovernanceSelfAction`, so
/// only the self-management variants fit. Everything else in `ActionType`
/// exists for the read path and belongs on `POST /governance/propose`. Caught
/// here it is a clear 400; left to the serializer it would be a 500.
///
/// Also runs the action's own field validation (thresholds, timeouts) so a
/// malformed value surfaces as a 400 rather than a generic ledger submission
/// error.
///
/// Scoped to `core_self` deliberately. `core_domain` builds its choice from
/// `proposal_cid` and ignores `action` entirely, so both clients send a
/// deliberate `governance_set_threshold: 0` placeholder there for payload
/// symmetry — validating it would 400 every domain confirm and execute.
pub(super) fn validate_inline_action(
    action: &ActionType,
    governance_type: GovernanceType,
) -> Result<(), String> {
    if !matches!(governance_type, GovernanceType::CoreSelf) {
        return Ok(());
    }
    if !action.is_governance_self_action() {
        return Err(format!(
            "action '{}' is not a governance self-management action; submit it as a domain \
             proposal via POST /governance/propose",
            crate::server::audit::action_summary(action)
        ));
    }
    action.validate().map_err(|e| e.to_string())
}

#[cfg(test)]
mod propose_guard_tests {
    use decman_lib::catalog::proposals::core::GenericVote;

    use super::*;

    fn party() -> CantonId {
        // `prefix::<68 hex chars>` = a 34-byte all-zero namespace: a well-formed id.
        CantonId::parse(&format!("p::{}", "0".repeat(68))).expect("valid canton id")
    }

    /// Only a Setup that omits `prior_delegation` can conflict with an active
    /// delegation. Replacing one is the normal path and must stay allowed, and no
    /// other proposal type is affected — a rule broad enough to block either would
    /// make the reward action unusable.
    ///
    /// This also gates the ACS read: a shape that returns false here never
    /// touches the ledger, so a false positive would tax every propose.
    #[test]
    fn only_an_unnamed_replacement_can_create_a_second_delegation() {
        let setup_without_prior =
            ProposalType::SetupCouponReassignmentDelegation(SetupCouponReassignmentDelegation {
                dso: party(),
                assigners: vec![party()],
                new_beneficiaries: vec![],
                prior_delegation: None,
            });
        let setup_with_prior =
            ProposalType::SetupCouponReassignmentDelegation(SetupCouponReassignmentDelegation {
                dso: party(),
                assigners: vec![party()],
                new_beneficiaries: vec![],
                prior_delegation: Some("00abc".to_string()),
            });
        let unrelated = ProposalType::GenericVote(GenericVote {
            description: "unrelated".to_string(),
        });

        // the accident: creating one without naming what it replaces
        assert!(may_create_second_delegation(&setup_without_prior));
        // replacement names what it replaces -> always allowed, no read needed
        assert!(!may_create_second_delegation(&setup_with_prior));
        // unrelated proposals are never blocked, and never pay for the read
        assert!(!may_create_second_delegation(&unrelated));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// `core_domain` builds its choice from `proposal_cid` and never reads
    /// `action`, so both clients send `governance_set_threshold: 0` as a
    /// deliberate placeholder (`NotificationsView.tsx`, `app.rs`). The inline
    /// guard must let it through untouched — validating it would 400 every
    /// domain confirm and execute.
    #[test]
    fn domain_confirms_accept_the_zero_threshold_placeholder() {
        let placeholder = ActionType::GovernanceSetThreshold { new_threshold: 0 };
        assert!(
            validate_inline_action(&placeholder, GovernanceType::CoreDomain).is_ok(),
            "the core_domain placeholder must not be validated"
        );
    }

    /// The same placeholder on the inline path is a real threshold, and 0 is
    /// not a legal one.
    #[test]
    fn self_management_confirms_still_validate_the_threshold() {
        let bad = ActionType::GovernanceSetThreshold { new_threshold: 0 };
        match validate_inline_action(&bad, GovernanceType::CoreSelf) {
            Ok(()) => panic!("new_threshold 0 must not pass core_self validation"),
            Err(error) => assert!(
                error.contains("at least 1"),
                "error should name the bound: {error}"
            ),
        }
    }

    /// A non-self-management action paired with `core_self` is a client error,
    /// caught before it reaches the serializer that cannot encode it.
    #[test]
    fn self_management_confirms_reject_a_domain_action() {
        let action = ActionType::DevNetFeatureApp {
            amulet_rules_cid: "00amulet".to_owned(),
        };
        match validate_inline_action(&action, GovernanceType::CoreSelf) {
            Ok(()) => panic!("DevNetFeatureApp is not a self-management action"),
            Err(error) => assert!(
                error.contains("/governance/propose"),
                "error should point at the proposal path: {error}"
            ),
        }
    }

    /// A cancellation vote is a self-management action, so `core_self` takes
    /// it, but only when it names the proposal to cancel.
    #[test]
    fn self_management_confirms_take_a_cancellation_with_a_proposal_cid() {
        let cancel = |proposal_cid: &str| ActionType::GovernanceCancelAction {
            proposal_cid: proposal_cid.to_owned(),
        };
        assert!(validate_inline_action(&cancel("00proposal"), GovernanceType::CoreSelf).is_ok());
        match validate_inline_action(&cancel(""), GovernanceType::CoreSelf) {
            Ok(()) => panic!("a cancellation without a proposal cid must not pass"),
            Err(error) => assert!(
                error.contains("proposal_cid"),
                "error should name the field: {error}"
            ),
        }
    }

    /// Each nonce is 12 lowercase hex digits, which the template's ensure
    /// clause accepts (non-empty, no "/"), and no two draws repeat.
    #[test]
    fn credential_id_nonces_are_distinct_lowercase_hex() {
        let nonces: HashSet<String> = (0..1000).map(|_| credential_id_nonce()).collect();
        assert_eq!(nonces.len(), 1000, "a nonce repeated");
        for nonce in &nonces {
            assert_eq!(nonce.len(), 12, "{nonce}");
            assert!(
                nonce
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
                "{nonce}"
            );
        }
    }
}
