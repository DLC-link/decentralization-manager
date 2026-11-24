//! The dApp's party creation against real Canton.
//!
//! The dApp builds a multi-owner party's topology in Rust and creates it
//! through the JSON Ledger API. This phase runs the same Rust, the
//! `decman-dapp` crate, and sends its request bodies the way the dApp's
//! browser code does. It proves what the unit tests cannot: Canton accepts our
//! bytes, our hashes, and one signature per owner over the combined hash.
//!
//! Two owners hold one key each, hosted on P1 and P2. P3 is a covalidator. P2
//! and P3 approve the hosting first, then P1 receives every transaction. Each
//! host must then host the party, with every host confirming and the owners'
//! keys as the party's signing keys.

use std::time::Duration;

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{SigningKeyUsage, SigningPublicKey},
    protocol::v30::enums::ParticipantPermission,
};
use decman_dapp::{
    Onboarding, OwnerSignature, PartyProposal, ProposedOwner, allocation_requests,
    generate_topology_request, onboarding_transactions, party_hosted_locally, plan_party,
    topology_format_of_generated,
};
use decman_lib::topology::{TopologyFormat, party_signing_key};
use decman_wallet::ExternalKeyPair;
use serde_json::Value;
use tracing::info;

use dec_party_manager::{
    utils,
    workflow::{external_party::add_hosts::read_party_to_participant, topology, topology_version},
};

use crate::common::{
    Fixture,
    chaos::fresh_prefix,
    ledger_api::{P1_JSON_API, P2_JSON_API, P3_JSON_API},
    phases::legacy_key_retirement::configs,
    scenario::Scenario,
};

/// Canton writes topology transactions in `v31` from this protocol version.
const FIRST_V31_PROTOCOL_VERSION: u32 = 37;

const ALLOCATE: &str = "/v2/parties/external/allocate";

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: dapp_party_onboarding");

    // The dApp reads each participant's id from its JSON Ledger API, and the
    // party-to-participant mapping names hosts by that id.
    let hosts = [
        (P1_JSON_API, f.p1.participant_id.clone()),
        (P2_JSON_API, f.p2.participant_id.clone()),
        (P3_JSON_API, f.p3.participant_id.clone()),
    ];
    for (port, expected) in &hosts {
        let answer: Value = f.get_json(*port, "/v2/parties/participant-id").await?;
        anyhow::ensure!(
            answer["participantId"] == expected.as_str(),
            "The JSON Ledger API on {port} reports participant {answer}, not {expected}"
        );
    }
    let synchronizer_id = connected_synchronizer(f, P1_JSON_API).await?;

    let owners = [ExternalKeyPair::generate(), ExternalKeyPair::generate()];
    let format = probe_format(f, &synchronizer_id, &owners[0]).await?;

    // The probe must name the format of the synchronizer's protocol version.
    let configs = configs(f)?;
    let physical_synchronizer = utils::get_synchronizer_id(&configs[0]).await?;
    let protocol_version = topology_version::protocol_version_of(&physical_synchronizer)?;
    let expected_format = if protocol_version >= FIRST_V31_PROTOCOL_VERSION {
        TopologyFormat::V31
    } else {
        TopologyFormat::V30
    };
    anyhow::ensure!(
        format == expected_format,
        "The generate-topology probe found {format:?}, but protocol version \
         {protocol_version} writes {expected_format:?}"
    );

    let proposal = PartyProposal {
        party_hint: fresh_prefix("dapp-party"),
        owners: vec![
            ProposedOwner {
                public_key: hex::encode(owners[0].public_key_bytes()),
                participant: f.p1.participant_id.clone(),
            },
            ProposedOwner {
                public_key: hex::encode(owners[1].public_key_bytes()),
                participant: f.p2.participant_id.clone(),
            },
        ],
        signing_threshold: 2,
        confirmation_threshold: 2,
        covalidators: vec![f.p3.participant_id.clone()],
    };
    let plan = plan_party(&proposal)?;
    let onboarding = onboarding_transactions(&plan, format);
    let signatures = owners
        .iter()
        .map(|owner| sign(owner, &onboarding))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let requests = allocation_requests(&plan, &onboarding, &synchronizer_id, &signatures)?;
    info!(
        "dApp party {} in format {format:?}, combined hash {}",
        plan.party_id, onboarding.combined_hash
    );

    let party_id = plan.party_id.clone();
    let expected_hosts = plan.hosts.clone();
    let mut expected_keys: Vec<Vec<u8>> = owners
        .iter()
        .map(|owner| party_signing_key(&owner.public_key_bytes()).public_key)
        .collect();
    expected_keys.sort();
    let namespace = plan.namespace.clone();
    let mut owner_namespaces: Vec<String> =
        plan.owners.iter().map(|o| o.namespace.clone()).collect();
    owner_namespaces.sort();

    Scenario::with_ctx(format!("create dApp party {party_id} on P1+P2+P3"), ())
        .when(
            "P2 and P3 approve the hosting, then P1 receives every transaction",
            {
                let approval = requests.hosting_approval;
                let allocate = requests.allocate;
                move |f, _| {
                    let approval = approval.clone();
                    let allocate = allocate.clone();
                    Box::pin(async move {
                        allocate_on(f, P2_JSON_API, &approval).await?;
                        allocate_on(f, P3_JSON_API, &approval).await?;
                        allocate_on(f, P1_JSON_API, &allocate).await
                    })
                }
            },
        )
        .then(
            "P1 hosts the party",
            Duration::from_secs(180),
            hosted_on(P1_JSON_API, &party_id),
        )
        .then(
            "P2 hosts the party",
            Duration::from_secs(60),
            hosted_on(P2_JSON_API, &party_id),
        )
        .then(
            "P3 hosts the party",
            Duration::from_secs(60),
            hosted_on(P3_JSON_API, &party_id),
        )
        .then(
            "every host confirms and the owners' keys sign for the party",
            Duration::from_secs(60),
            {
                let configs = configs.clone();
                let party_id = party_id.clone();
                move |_f, _| {
                    let configs = configs.clone();
                    let party_id = party_id.clone();
                    let expected_hosts = expected_hosts.clone();
                    let expected_keys = expected_keys.clone();
                    Box::pin(async move {
                        for config in &configs {
                            let current =
                                read_party_to_participant(config, &party_id).await.ok()??;
                            let mapping = current.mapping;
                            let mut hosts: Vec<String> = mapping
                                .participants
                                .iter()
                                .map(|p| p.participant_uid.clone())
                                .collect();
                            hosts.sort();
                            let all_confirm = mapping.participants.iter().all(|p| {
                                p.permission == ParticipantPermission::Confirmation as i32
                                    && p.onboarding.is_none()
                            });
                            let Some(keys) = mapping.party_signing_keys else {
                                return Some(Err(anyhow::anyhow!(
                                    "{party_id} has no party signing keys"
                                )));
                            };
                            let mut key_bytes: Vec<Vec<u8>> =
                                keys.keys.iter().map(|k| k.public_key.clone()).collect();
                            key_bytes.sort();
                            if current.serial != 1
                                || hosts != expected_hosts
                                || !all_confirm
                                || mapping.threshold != 2
                                || keys.threshold != 2
                                || key_bytes != expected_keys
                                || !keys.keys.iter().all(signs_for_the_party)
                            {
                                return Some(Err(anyhow::anyhow!(
                                    "{party_id}'s mapping differs from the dApp's: serial {}, \
                                     hosts {hosts:?}, all confirm {all_confirm}, threshold {}, \
                                     {} keys at threshold {}",
                                    current.serial,
                                    mapping.threshold,
                                    keys.keys.len(),
                                    keys.threshold
                                )));
                            }
                        }
                        Some(Ok(()))
                    })
                }
            },
        )
        .then(
            "every host sees both owners' decentralized namespace",
            Duration::from_secs(60),
            {
                let configs = configs.clone();
                move |_f, _| {
                    let configs = configs.clone();
                    let physical_synchronizer = physical_synchronizer.clone();
                    let namespace = namespace.clone();
                    let owner_namespaces = owner_namespaces.clone();
                    Box::pin(async move {
                        for config in &configs {
                            let (serial, definition) =
                                topology::fetch_namespace_definition_at_head(
                                    config,
                                    &physical_synchronizer,
                                    &namespace,
                                )
                                .await
                                .ok()?;
                            let mut owners = definition.owners.clone();
                            owners.sort();
                            if serial != 1
                                || owners != owner_namespaces
                                || definition.threshold != 2
                            {
                                return Some(Err(anyhow::anyhow!(
                                    "{namespace}'s definition differs from the dApp's: serial \
                                     {serial}, owners {owners:?}, threshold {}",
                                    definition.threshold
                                )));
                            }
                        }
                        Some(Ok(()))
                    })
                }
            },
        )
        .run(f)
        .await
}

/// The one synchronizer the participant on `port` is connected to.
async fn connected_synchronizer(f: &Fixture, port: u16) -> anyhow::Result<String> {
    let answer: Value = f
        .get_json(port, "/v2/state/connected-synchronizers")
        .await?;
    let synchronizers = answer["connectedSynchronizers"]
        .as_array()
        .context("connected-synchronizers answer has no list")?;
    let [synchronizer] = synchronizers.as_slice() else {
        anyhow::bail!("Expected one connected synchronizer, got {answer}");
    };
    synchronizer["synchronizerId"]
        .as_str()
        .map(str::to_owned)
        .context("connected synchronizer has no id")
}

/// The topology format P1 writes, read the way the dApp reads it.
async fn probe_format(
    f: &Fixture,
    synchronizer_id: &str,
    owner: &ExternalKeyPair,
) -> anyhow::Result<TopologyFormat> {
    let request =
        generate_topology_request(synchronizer_id, &hex::encode(owner.public_key_bytes()))?;
    let answer: Value = f
        .post_json(
            P1_JSON_API,
            "/v2/parties/external/generate-topology",
            &request,
        )
        .await?;
    Ok(topology_format_of_generated(&answer)?)
}

fn sign(owner: &ExternalKeyPair, onboarding: &Onboarding) -> anyhow::Result<OwnerSignature> {
    let message = hex::decode(&onboarding.combined_hash)?;
    Ok(OwnerSignature {
        public_key: hex::encode(owner.public_key_bytes()),
        signature: hex::encode(owner.sign(&message)),
    })
}

/// Sends an allocate body and keeps the participant's error text.
async fn allocate_on(f: &Fixture, port: u16, body: &Value) -> anyhow::Result<()> {
    let (status, text) = f.post_expect_status(port, ALLOCATE, body).await?;
    anyhow::ensure!(
        status.is_success(),
        "Allocate on {port} returned {status}: {text}"
    );
    Ok(())
}

/// Polls `GET /v2/parties/{party}` on `port` until the participant hosts it.
fn hosted_on(
    port: u16,
    party_id: &str,
) -> impl for<'a> FnMut(&'a mut Fixture, &'a mut ()) -> penelope::BoxProbe<'a> + Send + 'static {
    // As the dApp's encodeURIComponent does. A party id has no other
    // character that needs escaping.
    let path = format!("/v2/parties/{}", party_id.replace(':', "%3A"));
    let party_id = party_id.to_owned();
    move |f, _| {
        let path = path.clone();
        let party_id = party_id.clone();
        Box::pin(async move {
            let answer: Value = f.probe_get_json(port, &path).await?;
            party_hosted_locally(&answer, &party_id).then_some(Ok(()))
        })
    }
}

/// Canton may add usages, so the key needs at least these two.
fn signs_for_the_party(key: &SigningPublicKey) -> bool {
    [SigningKeyUsage::Namespace, SigningKeyUsage::Protocol]
        .iter()
        .all(|usage| key.usage.contains(&(*usage as i32)))
}
