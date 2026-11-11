//! Governance read endpoints: confirmations, proposals, state and known members.

use actix_web::{HttpRequest, HttpResponse, Responder, get, web};
use base64::Engine;
use serde::Deserialize;

use super::credentials::{get_member_party_id, get_party_threshold, get_party_token, packages};
use crate::{
    canton_id::CantonId,
    config::NetworkConfig,
    db::schema::SchemaRead,
    error::Result,
    noise::{Message, MessageType, NoiseKeypair, parse_public_key, send_noise_message},
    server::{
        AppState,
        handlers::token_standard::GovernanceQuery,
        ledger_paging::AcsCursor,
        middleware::require_admin,
        queries::{
            GovernanceQueue, get_governance_confirmations,
            get_governance_state as query_governance_state,
        },
        types::{
            DomainGovernanceAction, ErrorResponse, GovernanceResponse, GovernanceStateResponse,
            KnownMember, KnownMembersResponse, ProposalsPageResponse,
        },
    },
};

// ============================================================================
// Read Endpoints
// ============================================================================

/// Drop each proposal's full payload from a confirmations response.
///
/// v1 lets every authenticated caller read any party on the node. Only an
/// admin may confirm or execute, so only an admin needs every field of what
/// they would sign. The summary fields stay for everyone.
fn withhold_payloads(domain_actions: &mut [DomainGovernanceAction]) {
    for action in domain_actions {
        action.proposal_payload = None;
        action.proposal_template_id = None;
    }
}

/// Get governance confirmations with parsed actions
#[utoipa::path(
    tag = "Governance",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "Governance confirmations", body = GovernanceResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/governance/confirmations")]
pub async fn get_governance(
    http_req: HttpRequest,
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let may_act = require_admin(&http_req, data.admin_role.as_deref()).is_ok();

    let token = get_party_token(&data, party_id).await;

    let member_party_id = get_member_party_id(&data, party_id).await;
    let packages = packages();

    // Pull `(rules_contract_id, threshold)` off the active GovernanceRules /
    // rules contract. The Daml `ExecuteGovernanceAction`
    // choice gates on THIS threshold ("Enough member confirmations to
    // execute action") — not the decentralized-namespace topology
    // threshold, which is a separate value used for signing
    // PartyToParticipant updates. Falling back to the DNS threshold for
    // historical compatibility only when the gov state isn't reachable.
    let gov_state =
        match query_governance_state(&data.config, party_id, token.clone(), &packages).await {
            Ok(state) => state,
            Err(e) => {
                tracing::warn!("Failed to fetch active rules contract: {e}");
                None
            }
        };
    let threshold = match &gov_state {
        Some(state) => state.threshold as usize,
        None => get_party_threshold(&data, party_id).await.unwrap_or(2),
    };

    let batch = query.limit.unwrap_or(25).clamp(1, 200);
    let from = match &query.cursor {
        Some(raw) if !raw.is_empty() => decode_cursor(raw),
        _ => None,
    };

    match get_governance_confirmations(
        &data.config,
        party_id,
        threshold,
        gov_state.as_ref(),
        token,
        &packages,
        batch,
        from,
    )
    .await
    {
        Ok(GovernanceQueue {
            actions,
            mut domain_actions,
            next_cursor,
            ..
        }) => {
            if !may_act {
                withhold_payloads(&mut domain_actions);
            }
            HttpResponse::Ok().json(GovernanceResponse {
                actions,
                next_cursor: next_cursor.as_ref().and_then(encode_cursor),
                domain_actions,
                threshold,
                member_party_id,
                rules_contract_id: gov_state.as_ref().map(|state| state.contract_id.clone()),
                gov_core_out_of_date: gov_state.as_ref().is_some_and(|state| state.out_of_date),
                gov_core_package_ref: gov_state.and_then(|state| state.package_ref),
            })
        }
        Err(e) => {
            tracing::error!("Failed to fetch governance confirmations: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch governance confirmations: {e}"),
            })
        }
    }
}

/// Get governance state (GovernanceRules contract state)
/// Query parameters for the paged proposals feed.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ProposalsQuery {
    pub party_id: CantonId,
    /// Proposals per batch. Defaults to 50, capped at 200.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Opaque `next_cursor` from the previous batch. Absent starts at the top.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Default and ceiling for one batch. The ceiling is what keeps a client from
/// asking for a party's whole proposal history in one request (#424).
const PROPOSALS_BATCH_DEFAULT: usize = 50;
const PROPOSALS_BATCH_MAX: usize = 200;

fn encode_cursor(cursor: &AcsCursor) -> Option<String> {
    serde_json::to_vec(cursor)
        .ok()
        .map(|bytes| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(raw: &str) -> Option<AcsCursor> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// One batch of a party's open proposals.
///
/// Paged because a party can hold tens of thousands: reading them all in one
/// request either times out or exhausts the node (#424). Carries no
/// confirmation counts — resolving those means reading every
/// `GovernanceConfirmation`, which is the same unbounded scan.
#[utoipa::path(
    tag = "Governance",
    params(ProposalsQuery),
    responses(
        (status = 200, description = "A batch of proposals", body = ProposalsPageResponse),
        (status = 400, description = "Malformed cursor", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/governance/proposals")]
pub async fn get_proposals_page(
    data: web::Data<AppState>,
    query: web::Query<ProposalsQuery>,
) -> impl Responder {
    let party_id = &query.party_id;
    let limit = query
        .limit
        .unwrap_or(PROPOSALS_BATCH_DEFAULT)
        .clamp(1, PROPOSALS_BATCH_MAX);

    let cursor = match &query.cursor {
        Some(raw) if !raw.is_empty() => match decode_cursor(raw) {
            Some(c) => Some(c),
            None => {
                return HttpResponse::BadRequest().json(ErrorResponse {
                    error: "cursor is not one this endpoint issued".to_string(),
                });
            }
        },
        _ => None,
    };

    let token = get_party_token(&data, party_id).await;
    let packages = packages();

    // One contract, so cheap next to the batch. Lets a card render
    // "n of threshold" without the client fetching the rules separately.
    let threshold = query_governance_state(&data.config, party_id, token.clone(), &packages)
        .await
        .ok()
        .flatten()
        .map(|state| state.threshold.max(0) as usize)
        .unwrap_or(0);

    match crate::server::queries::page_proposal_summaries(
        &data.config,
        party_id,
        token,
        &packages,
        limit,
        cursor,
    )
    .await
    {
        Ok((proposals, next)) => HttpResponse::Ok().json(ProposalsPageResponse {
            proposals,
            threshold,
            next_cursor: next.as_ref().and_then(encode_cursor),
        }),
        Err(e) => {
            tracing::error!(%party_id, "Failed to read a proposals batch: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to read proposals: {e}"),
            })
        }
    }
}

#[utoipa::path(
    tag = "Governance",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "Governance state", body = GovernanceStateResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/governance/state")]
pub async fn get_governance_state(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    let party_id = &query.party_id;

    let token = get_party_token(&data, party_id).await;
    let packages = packages();

    match query_governance_state(&data.config, party_id, token, &packages).await {
        Ok(state) => HttpResponse::Ok().json(GovernanceStateResponse { state }),
        Err(e) => {
            tracing::error!("Failed to fetch governance state: {e}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to fetch governance state: {e}"),
            })
        }
    }
}

#[utoipa::path(
    tag = "Governance",
    params(GovernanceQuery),
    responses(
        (status = 200, description = "Member parties known to each participant", body = KnownMembersResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
#[get("/governance/known-members")]
pub async fn get_known_members(
    data: web::Data<AppState>,
    query: web::Query<GovernanceQuery>,
) -> impl Responder {
    match collect_known_members(&data, &query.party_id).await {
        Ok(members) => HttpResponse::Ok().json(KnownMembersResponse { members }),
        Err(e) => {
            tracing::error!("Failed to collect known members: {e:#}");
            HttpResponse::InternalServerError().json(ErrorResponse {
                error: format!("Failed to collect known members: {e}"),
            })
        }
    }
}

async fn collect_known_members(
    data: &web::Data<AppState>,
    dec_party_id: &CantonId,
) -> Result<Vec<KnownMember>> {
    let dec_party_str = dec_party_id.to_string();
    let network_config = NetworkConfig::from_peers(data.db.get_all_peers().await?);
    let keypair = NoiseKeypair::from_file(&data.config.key_file_path()).await?;
    let self_id = data.config.participant_id();
    let identity_bytes = self_id.to_string();
    let request = Message::new(
        MessageType::RequestMemberParty,
        dec_party_str.as_bytes().to_vec(),
    );

    let mut out = Vec::new();

    {
        let creds = data.party_credentials.read().await;
        let self_member = creds
            .iter()
            .find(|c| c.dec_party_id == *dec_party_id)
            .map(|c| c.member_party_id.clone());
        out.push(KnownMember {
            participant_uid: self_id.clone(),
            member_party_id: self_member,
        });
    }

    for peer in &network_config.peers {
        if peer.participant_id == *self_id {
            continue;
        }
        if peer.public_key.is_empty() {
            out.push(KnownMember {
                participant_uid: peer.participant_id.clone(),
                member_party_id: None,
            });
            continue;
        }
        let peer_pub_key = match parse_public_key(&peer.public_key) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::warn!(
                    "Skipping member-party query to {pid}: invalid public key: {e}",
                    pid = peer.participant_id,
                );
                out.push(KnownMember {
                    participant_uid: peer.participant_id.clone(),
                    member_party_id: None,
                });
                continue;
            }
        };

        let psk = keypair.derive_psk(&peer_pub_key);
        let response = match send_noise_message(
            &peer.address,
            peer.port,
            &psk,
            identity_bytes.as_bytes(),
            &request,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "Failed to query member party from {pid}: {e}",
                    pid = peer.participant_id,
                );
                out.push(KnownMember {
                    participant_uid: peer.participant_id.clone(),
                    member_party_id: None,
                });
                continue;
            }
        };

        let member_party = match Message::from_bytes(&response) {
            Ok(msg) if msg.msg_type == MessageType::MemberPartyResponse => {
                String::from_utf8(msg.payload)
                    .ok()
                    .filter(|s| !s.is_empty())
                    .and_then(|s| CantonId::parse(&s).ok())
            }
            _ => None,
        };
        out.push(KnownMember {
            participant_uid: peer.participant_id.clone(),
            member_party_id: member_party,
        });
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn withholding_drops_the_payload_and_keeps_the_summary() -> anyhow::Result<()> {
        let mut actions: Vec<DomainGovernanceAction> =
            vec![serde_json::from_value(serde_json::json!({
                "proposal_cid": "00proposal",
                "action_label": "GenericVote",
                "description": "vote on it",
                "confirmations": [],
                "confirmation_count": 0,
                "executable_confirmation_cids": [],
                "can_execute": false,
                "proposal_template_id": "pkg:Governance.GenericVote:GenericVoteProposal",
                "proposal_payload": {"description": "vote on it", "secret": "x"},
            }))?];

        withhold_payloads(&mut actions);

        let wire = serde_json::to_value(&actions[0])?;
        assert!(wire.get("proposal_payload").is_none(), "{wire}");
        assert!(wire.get("proposal_template_id").is_none(), "{wire}");
        assert_eq!(wire["description"], "vote on it");
        assert_eq!(wire["proposal_cid"], "00proposal");
        Ok(())
    }
}
