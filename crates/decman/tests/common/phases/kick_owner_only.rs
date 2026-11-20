//! A namespace owner that does not host the party: listed, then kicked.
//!
//! A fresh party on P1, P2 and P3 at threshold 2. P3 removes its own hosting
//! entry and stays a namespace owner. P1 must list P3's owner key as an owner
//! that hosts nothing, and then kick it with only P2 co-signing. The phase runs
//! twice, once for each [`Shape`].
//!
//! Asserts: P1 lists P3's key under `non_hosting_owners`, claimed by P3. The
//! kick completes. The namespace drops P3's key and keeps threshold 2. The
//! hosting mapping keeps P1 and P2 with one signing key each. P1 lists no
//! owner that hosts nothing afterwards.

use std::{collections::HashSet, time::Duration};

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::{
    crypto::{
        admin::v30::{
            ListMyKeysRequest, private_key_metadata, vault_service_client::VaultServiceClient,
        },
        v30::public_key,
    },
    protocol::v30::{PartyToParticipant, topology_mapping},
    topology::admin::v30::{AuthorizeRequest, ForceFlag, authorize_request},
};
use common::{
    api::{DecentralizedPartiesResponse, PendingInvitationsResponse},
    canton_id::CantonId,
    types::{DecentralizedParty, InvitationType},
};
use dec_party_manager::{
    config::NodeConfig,
    utils,
    workflow::{
        topology::{
            authorize_with_topology_retry, fetch_namespace_definition_at_head,
            fetch_p2p_mapping_at_head, synchronizer_store_id,
        },
        topology_version,
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

/// What P3 leaves in the hosting mapping when it removes its own entry.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Shape {
    /// P3's signing key stays, as on MainNet. The kick drops it, so the
    /// hosting mapping changes.
    LeftoverKey,
    /// P1 and P2 drop P3's key before the kick. The kick then leaves the
    /// hosting mapping as it is, so the peers sign the mapping in force and
    /// its serial does not move.
    NoLeftoverKey,
}

#[derive(Default)]
struct Ctx {
    invites: InvitationIds,
    party_id: Option<CantonId>,
    p3_owner_key: Option<String>,
    /// The hosting mapping's serial just before the kick.
    p2p_serial: Option<u32>,
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

/// The fingerprints of every signing key in the participant's vault.
async fn vault_signing_keys(config: &NodeConfig) -> anyhow::Result<HashSet<String>> {
    let response = VaultServiceClient::new(config.admin_channel().await?)
        .list_my_keys(tonic::Request::new(ListMyKeysRequest {
            filters: None,
            base_request: None,
        }))
        .await?
        .into_inner();
    Ok(response
        .private_keys_metadata
        .iter()
        .filter_map(|meta| match &meta.public_key_with_name {
            Some(private_key_metadata::PublicKeyWithName::V30(named)) => named.public_key.as_ref(),
            _ => None,
        })
        .filter_map(|key| match &key.key {
            Some(public_key::Key::SigningPublicKey(signing)) => {
                Some(utils::compute_fingerprint(signing).into())
            }
            _ => None,
        })
        .collect())
}

/// An add-or-replace proposal of `mapping` on the synchronizer at `serial`.
fn p2p_proposal(
    synchronizer_id: &str,
    serial: u32,
    mapping: PartyToParticipant,
    must_fully_authorize: bool,
    force_changes: Vec<i32>,
) -> AuthorizeRequest {
    AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            topology_version::ProposalFormat::for_synchronizer(synchronizer_id).proposal(
                serial,
                topology_mapping::Mapping::PartyToParticipant(mapping),
            ),
        )),
        must_fully_authorize,
        force_changes,
        signed_by: vec![],
        store: Some(synchronizer_store_id(synchronizer_id)),
        wait_to_become_effective: None,
    }
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: kick_owner_only");
    kick_owner_only(f, Shape::LeftoverKey).await?;
    kick_owner_only(f, Shape::NoLeftoverKey).await
}

async fn kick_owner_only(f: &mut Fixture, shape: Shape) -> anyhow::Result<()> {
    let prefix = fresh_prefix(match shape {
        Shape::LeftoverKey => "owner-only",
        Shape::NoLeftoverKey => "owner-only-nokey",
    });
    info!("Using prefix: {prefix} ({shape:?})");

    let mut scenario = Scenario::with_ctx(
        format!("kick an owner of {prefix} that does not host it ({shape:?})"),
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
                p2p_proposal(
                    &synchronizer_id,
                    0,
                    mapping,
                    true,
                    vec![ForceFlag::DisablePartyWithActiveContracts as i32],
                ),
                "un-host P3",
            )
            .await
            .context("P3 un-hosting itself")?;
            Ok(())
        })
    });

    if shape == Shape::NoLeftoverKey {
        scenario = scenario
            .then(
                "the hosting mapping no longer hosts P3",
                Duration::from_secs(60),
                |f, ctx| {
                    Box::pin(async move {
                        let check = async {
                            let p1 = admin_config(f, 1)?;
                            let synchronizer_id = utils::get_synchronizer_id(&p1).await?;
                            let (_, mapping) =
                                fetch_p2p_mapping_at_head(&p1, &synchronizer_id, ctx.party_id()?)
                                    .await?;
                            Ok::<_, anyhow::Error>(mapping.participants.len())
                        };
                        match check.await {
                            Ok(2) => Some(Ok(())),
                            Ok(_) => None,
                            Err(e) => Some(Err(e)),
                        }
                    })
                },
            )
            .when("P1 and P2 drop the signing key P3 left behind", |f, ctx| {
                Box::pin(async move {
                    let p3_keys = vault_signing_keys(&admin_config(f, 3)?).await?;
                    let p1 = admin_config(f, 1)?;
                    let synchronizer_id = utils::get_synchronizer_id(&p1).await?;
                    let (serial, mut mapping) =
                        fetch_p2p_mapping_at_head(&p1, &synchronizer_id, ctx.party_id()?).await?;
                    let keys = mapping
                        .party_signing_keys
                        .as_mut()
                        .context("the party carries no signing keys")?;
                    keys.keys.retain(|key| {
                        !p3_keys.contains(&String::from(utils::compute_fingerprint(key)))
                    });
                    anyhow::ensure!(
                        keys.keys.len() == 2,
                        "P3's vault should hold exactly one of the party's three signing keys"
                    );
                    // The namespace threshold is 2, so P1's and P2's owner
                    // signatures together authorize the change.
                    let proposal =
                        p2p_proposal(&synchronizer_id, serial + 1, mapping, false, vec![]);
                    for node in [1, 2] {
                        authorize_with_topology_retry(
                            &admin_config(f, node)?,
                            proposal.clone(),
                            "drop P3's leftover key",
                        )
                        .await
                        .with_context(|| format!("P{node} signing the key removal"))?;
                    }
                    Ok(())
                })
            })
            .then(
                "the hosting mapping holds only P1's and P2's keys",
                Duration::from_secs(60),
                |f, ctx| {
                    Box::pin(async move {
                        let check = async {
                            let p1 = admin_config(f, 1)?;
                            let synchronizer_id = utils::get_synchronizer_id(&p1).await?;
                            let (_, mapping) =
                                fetch_p2p_mapping_at_head(&p1, &synchronizer_id, ctx.party_id()?)
                                    .await?;
                            Ok::<_, anyhow::Error>(mapping.party_signing_keys.map(|k| k.keys.len()))
                        };
                        match check.await {
                            Ok(Some(2)) => Some(Ok(())),
                            Ok(_) => None,
                            Err(e) => Some(Err(e)),
                        }
                    })
                },
            );
    }

    scenario
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
        .when("P1 posts /kick naming P3's owner key", move |f, ctx| {
            Box::pin(async move {
                let p1 = admin_config(f, 1)?;
                let synchronizer_id = utils::get_synchronizer_id(&p1).await?;
                let (serial, _) =
                    fetch_p2p_mapping_at_head(&p1, &synchronizer_id, ctx.party_id()?).await?;
                ctx.p2p_serial = Some(serial);
                // The second shape names the owner key alone, which a kick of
                // an unclaimed key also has to do.
                let participant_id =
                    (shape == Shape::LeftoverKey).then(|| f.p3.participant_id.clone());
                let req = json!({
                    "decentralized_party_id": ctx.party_id()?.to_string(),
                    "participant_id": participant_id,
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
                    let r: PendingInvitationsResponse =
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
                Box::pin(async move {
                    probe_workflow_status(&*f, f.p1.http, "/kick/status", "kick").await
                })
            },
        )
        .then(
            "the namespace dropped P3's key and the mapping keeps one key per host",
            Duration::from_secs(60),
            move |f, ctx| {
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
                        let (serial, mapping) =
                            fetch_p2p_mapping_at_head(&p1, &synchronizer_id, party_id).await?;
                        let keys = mapping
                            .party_signing_keys
                            .as_ref()
                            .map(|k| (k.keys.len(), k.threshold));
                        Ok::<_, anyhow::Error>((
                            (
                                namespace.owners.len(),
                                namespace.owners.iter().any(|o| o == owner_key),
                                namespace.threshold,
                                mapping.participants.len(),
                                keys,
                            ),
                            serial,
                        ))
                    };
                    let before = match ctx.p2p_serial.context("P2P serial before the kick not set")
                    {
                        Ok(serial) => serial,
                        Err(e) => return Some(Err(e)),
                    };
                    match check.await {
                        Ok(((2, false, 2, 2, Some((2, 2))), serial)) => {
                            let moved = serial != before;
                            match (shape, moved) {
                                (Shape::LeftoverKey, true) | (Shape::NoLeftoverKey, false) => {
                                    Some(Ok(()))
                                }
                                (Shape::LeftoverKey, false) => None,
                                (Shape::NoLeftoverKey, true) => Some(Err(anyhow::anyhow!(
                                    "the kick moved the hosting mapping from serial {before} \
                                     to {serial}, but it had nothing to change"
                                ))),
                            }
                        }
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
