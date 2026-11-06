//! A namespace owner that does not host the party: listed, then kicked.
//!
//! A fresh party on P1, P2 and P3 at threshold 2. P3 removes its own hosting
//! entry and stays a namespace owner. That leaves P3's signing key in the
//! party, which is the shape MainNet is in. P1 must list P3's owner key as an
//! owner that hosts nothing, and then kick it with only P2 co-signing.
//!
//! Asserts: P1 lists P3's key under `non_hosting_owners`, claimed by P3. The
//! kick completes. The namespace drops P3's key and keeps threshold 2. The
//! hosting mapping keeps P1 and P2 and drops the signing key P3 left behind.
//! P1 lists no owner that hosts nothing afterwards.

use std::time::Duration;

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{TopologyMapping, enums::TopologyChangeOp, topology_mapping},
    topology::admin::v30::{AuthorizeRequest, ForceFlag, authorize_request},
};
use common::{
    api::DecentralizedPartiesResponse,
    canton_id::CantonId,
    types::{DecentralizedParty, InvitationType},
};
use dec_party_manager::{
    utils,
    workflow::topology::{
        authorize_with_topology_retry, fetch_namespace_definition_at_head,
        fetch_p2p_mapping_at_head, synchronizer_store_id,
    },
};
use serde_json::{Value, json};
use tracing::info;

use super::add_party_rehost_disconnect_first::admin_config;
use crate::common::{
    Fixture,
    chaos::fresh_prefix,
    http::probe_workflow_status,
    invitations::{InvitationIds, post_accept_invitation, probe_pending_invitation},
    scenario::Scenario,
};

#[derive(Default)]
struct Ctx {
    invites: InvitationIds,
    party_id: Option<CantonId>,
    p3_owner_key: Option<String>,
}

impl Ctx {
    fn party_id(&self) -> anyhow::Result<&CantonId> {
        self.party_id.as_ref().context("party id not set")
    }

    fn p3_owner_key(&self) -> anyhow::Result<&str> {
        self.p3_owner_key
            .as_deref()
            .context("P3's owner key not set")
    }
}

/// P1's live view of the party with `prefix`.
async fn p1_party(f: &Fixture, prefix: &str) -> Option<DecentralizedParty> {
    let path = format!("/decentralized-parties?prefix={prefix}&refresh=true");
    let r: DecentralizedPartiesResponse = f.probe_get_json(f.p1.http, &path).await?;
    r.parties.into_iter().find(|p| p.party_id.prefix == prefix)
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: kick_owner_only");
    let prefix = fresh_prefix("owner-only");
    info!("Using prefix: {prefix}");

    Scenario::with_ctx(
        format!("kick an owner of {prefix} that does not host it"),
        Ctx::default(),
    )
    .when(
        "P1 posts /onboarding inviting P2 and P3 with threshold 2",
        {
            let prefix = prefix.clone();
            move |f, _| {
                let prefix = prefix.clone();
                Box::pin(async move {
                    let req = json!({
                        "party_id_prefix": prefix,
                        "peer_ids": [&f.p2.participant_id, &f.p3.participant_id],
                        "threshold": 2,
                    });
                    let _: Value = f.post_json(f.p1.http, "/onboarding", &req).await?;
                    Ok(())
                })
            }
        },
    )
    .then(
        "Onboarding invitations visible on P2 and P3",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                if ctx.invites.p2.is_none() {
                    ctx.invites.p2 =
                        probe_pending_invitation(f, f.p2.http, InvitationType::Onboarding).await;
                }
                if ctx.invites.p3.is_none() {
                    ctx.invites.p3 =
                        probe_pending_invitation(f, f.p3.http, InvitationType::Onboarding).await;
                }
                (ctx.invites.p2.is_some() && ctx.invites.p3.is_some()).then_some(Ok(()))
            })
        },
    )
    .when("P2 and P3 accept", |f, ctx| {
        Box::pin(async move {
            let p2 = ctx.invites.p2.clone().context("P2 invitation id not set")?;
            let p3 = ctx.invites.p3.clone().context("P3 invitation id not set")?;
            post_accept_invitation(f, f.p2.http, &p2)
                .await
                .context("accept on P2")?;
            post_accept_invitation(f, f.p3.http, &p3)
                .await
                .context("accept on P3")
        })
    })
    .then(
        "onboarding workflow reaches completed",
        Duration::from_secs(240),
        |f, _| {
            Box::pin(async move {
                probe_workflow_status(&*f, f.p1.http, "/onboarding/status", "onboarding").await
            })
        },
    )
    .then(
        "P1 lists the party with three hosts",
        Duration::from_secs(60),
        {
            let prefix = prefix.clone();
            move |f, ctx| {
                let prefix = prefix.clone();
                Box::pin(async move {
                    let party = p1_party(f, &prefix).await?;
                    if party.participants.len() != 3 {
                        return None;
                    }
                    ctx.party_id = Some(party.party_id);
                    Some(Ok(()))
                })
            }
        },
    )
    .when("P3 removes its own hosting entry", |f, ctx| {
        Box::pin(async move {
            let p3 = admin_config(f, 3)?;
            let party_id = ctx.party_id()?.clone();
            let synchronizer_id = utils::get_synchronizer_id(&p3).await?;
            let (_, mut mapping) =
                fetch_p2p_mapping_at_head(&p3, &synchronizer_id, &party_id).await?;
            anyhow::ensure!(
                mapping.party_signing_keys.as_ref().map(|k| k.keys.len()) == Some(3),
                "a fresh three-member party carries three signing keys"
            );
            mapping
                .participants
                .retain(|p| p.participant_uid != f.p3.participant_id);
            anyhow::ensure!(
                mapping.participants.len() == 2,
                "un-hosting should leave P1 + P2"
            );
            authorize_with_topology_retry(
                &p3,
                AuthorizeRequest {
                    r#type: Some(authorize_request::Type::Proposal(
                        authorize_request::Proposal {
                            change: TopologyChangeOp::AddReplace as i32,
                            serial: 0,
                            mapping: Some(authorize_request::proposal::Mapping::V30(
                                TopologyMapping {
                                    mapping: Some(topology_mapping::Mapping::PartyToParticipant(
                                        mapping,
                                    )),
                                },
                            )),
                        },
                    )),
                    must_fully_authorize: true,
                    force_changes: vec![ForceFlag::DisablePartyWithActiveContracts as i32],
                    signed_by: vec![],
                    store: Some(synchronizer_store_id(&synchronizer_id)),
                    wait_to_become_effective: None,
                },
                "un-host P3",
            )
            .await
            .context("P3 un-hosting itself")?;
            Ok(())
        })
    })
    .then(
        "P1 lists P3's owner key as an owner that hosts nothing",
        Duration::from_secs(120),
        {
            let prefix = prefix.clone();
            move |f, ctx| {
                let prefix = prefix.clone();
                Box::pin(async move {
                    let p3_uid: CantonId = match f.p3.participant_id.parse() {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    };
                    let party = p1_party(f, &prefix).await?;
                    if party.participants.len() != 2 {
                        return None;
                    }
                    let owner = party
                        .non_hosting_owners
                        .into_iter()
                        .find(|o| o.participant_uid.as_ref() == Some(&p3_uid))?;
                    ctx.p3_owner_key = Some(owner.owner_key);
                    Some(Ok(()))
                })
            }
        },
    )
    .when("P1 posts /kick naming P3's owner key", |f, ctx| {
        Box::pin(async move {
            let req = json!({
                "decentralized_party_id": ctx.party_id()?.to_string(),
                "participant_id": f.p3.participant_id.clone(),
                "owner_key": ctx.p3_owner_key()?,
                "new_threshold": 2_i64,
                "previous_threshold": 2_i64,
            });
            let _: Value = f
                .post_json(f.p1.http, "/kick", &req)
                .await
                .context("POST /kick")?;
            Ok(())
        })
    })
    .then(
        "Kick invitation visible on P2 with P3's owner key",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let r: common::api::PendingInvitationsResponse =
                    f.probe_get_json(f.p2.http, "/invitations").await?;
                let owner_key = match ctx.p3_owner_key() {
                    Ok(key) => key.to_string(),
                    Err(e) => return Some(Err(e)),
                };
                let invite = r.invitations.into_iter().find(|i| {
                    i.invitation_type == InvitationType::Kick
                        && i.kicked_owner_key.as_deref() == Some(owner_key.as_str())
                })?;
                ctx.invites.p2 = Some(invite.id);
                Some(Ok(()))
            })
        },
    )
    .when("P2 accepts the Kick invitation", |f, ctx| {
        Box::pin(async move {
            let id = ctx.invites.p2.clone().context("P2 invitation id not set")?;
            post_accept_invitation(f, f.p2.http, &id)
                .await
                .context("accept Kick on P2")
        })
    })
    .then(
        "kick workflow reaches completed",
        Duration::from_secs(240),
        |f, _| {
            Box::pin(
                async move { probe_workflow_status(&*f, f.p1.http, "/kick/status", "kick").await },
            )
        },
    )
    .then(
        "the namespace dropped P3's key and the mapping dropped the key P3 left behind",
        Duration::from_secs(60),
        |f, ctx| {
            Box::pin(async move {
                let check = async {
                    let p1 = admin_config(f, 1)?;
                    let party_id = ctx.party_id()?;
                    let owner_key = ctx.p3_owner_key()?;
                    let synchronizer_id = utils::get_synchronizer_id(&p1).await?;
                    let (_, namespace) = fetch_namespace_definition_at_head(
                        &p1,
                        &synchronizer_id,
                        &party_id.namespace.to_hex(),
                    )
                    .await?;
                    let (_, mapping) =
                        fetch_p2p_mapping_at_head(&p1, &synchronizer_id, party_id).await?;
                    let keys = mapping
                        .party_signing_keys
                        .as_ref()
                        .map(|k| (k.keys.len(), k.threshold));
                    Ok::<_, anyhow::Error>((
                        namespace.owners.len(),
                        namespace.owners.iter().any(|o| o == owner_key),
                        namespace.threshold,
                        mapping.participants.len(),
                        keys,
                    ))
                };
                match check.await {
                    Ok((2, false, 2, 2, Some((2, 2)))) => Some(Ok(())),
                    Ok(state) => {
                        info!("topology not settled yet: {state:?}");
                        None
                    }
                    Err(e) => Some(Err(e)),
                }
            })
        },
    )
    .then(
        "P1 lists no owner that hosts nothing",
        Duration::from_secs(60),
        {
            let prefix = prefix.clone();
            move |f, _| {
                let prefix = prefix.clone();
                Box::pin(async move {
                    let party = p1_party(f, &prefix).await?;
                    (party.owners.len() == 2 && party.non_hosting_owners.is_empty())
                        .then_some(Ok(()))
                })
            }
        },
    )
    .run(f)
    .await
}
