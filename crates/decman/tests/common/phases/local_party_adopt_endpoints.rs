//! Plan B1 end to end, through the endpoints a wallet actually calls.
//!
//! The spike phase proved Canton accepts `party_signing_keys` on an existing
//! local party. It proved it by writing the topology itself, which leaves the
//! part a partner would use untested: `POST /v0/tenant/local-party/adopt-key/`
//! `prepare` and `.../onboard`.
//!
//! That pair does something the spike did not. It submits with `proposal:
//! false`, an empty `signed_by`, and `AllowUnvalidatedSigningKeys` — the node
//! co-signing bytes a caller handed it, against a party whose namespace is the
//! node's own key. Whether Canton takes that combination is a question about
//! Canton, so it is answered here.
//!
//! The phase allocates a genuine local party on P1, then acts as the wallet:
//! read the serial, prepare, sign the returned hash locally, onboard, and assert
//! head state carries the wallet's key with the host demoted to Confirmation.

use std::time::Duration;

use anyhow::Context;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{
        PartyToParticipant, TopologyMapping,
        enums::{ParticipantPermission, TopologyChangeOp},
        party_to_participant::HostingParticipant,
        topology_mapping,
    },
    topology::admin::v30::{AuthorizeRequest, authorize_request},
};
use serde_json::{Value, json};
use tracing::info;

use dec_party_manager::{
    canton_id::CantonId,
    config::NodeConfig,
    workflow::{
        external_party::add_hosts::read_party_to_participant,
        topology::{authorize_with_topology_retry, synchronizer_store_id},
    },
};
use decman_wallet::ExternalKeyPair;

use crate::common::{Fixture, chaos::fresh_prefix, scenario::Scenario};

/// Point a `NodeConfig` at P1's Canton, so the phase can allocate the party the
/// way a partner's node would have. The admin API is tokenless, the same path
/// the tenant flow uses.
fn p1_config(f: &Fixture) -> anyhow::Result<NodeConfig> {
    let admin_port: u16 = std::env::var("P1_CANTON_ADMIN")
        .context("P1_CANTON_ADMIN not set")?
        .parse()
        .context("P1_CANTON_ADMIN is not a port")?;
    let mut config = NodeConfig::default();
    config.canton.admin_api_host = "127.0.0.1".to_string();
    config.canton.admin_api_port = admin_port;
    config.node.participant_id = Some(CantonId::parse(&f.p1.participant_id)?);
    Ok(config)
}

/// Allocate the party as it exists at a partner today: hosted by one
/// participant at Submission, no key of its own.
///
/// `Authorize` rather than the prepare/sign pair, because here the participant
/// is authoring its own topology — which for a local party it can do alone,
/// since the party's namespace is its namespace. `serial: 0` lets Canton pick.
async fn allocate_local_party(config: &NodeConfig, party_id: &str) -> anyhow::Result<()> {
    let synchronizer_id = dec_party_manager::utils::get_synchronizer_id(config).await?;
    authorize_with_topology_retry(
        config,
        AuthorizeRequest {
            r#type: Some(authorize_request::Type::Proposal(
                authorize_request::Proposal {
                    change: TopologyChangeOp::AddReplace as i32,
                    serial: 0,
                    mapping: Some(authorize_request::proposal::Mapping::V30(TopologyMapping {
                        mapping: Some(topology_mapping::Mapping::PartyToParticipant(
                            PartyToParticipant {
                                party: party_id.to_string(),
                                threshold: 1,
                                participants: vec![HostingParticipant {
                                    participant_uid: config.participant_id().to_string(),
                                    permission: ParticipantPermission::Submission as i32,
                                    onboarding: None,
                                }],
                                party_signing_keys: None,
                            },
                        )),
                    })),
                },
            )),
            must_fully_authorize: true,
            force_changes: vec![],
            signed_by: vec![],
            store: Some(synchronizer_store_id(&synchronizer_id)),
            wait_to_become_effective: None,
        },
        "local-party adopt endpoints",
    )
    .await?;
    Ok(())
}

/// Wait until the allocation is in head state and return the serial it sits at.
///
/// `Authorize` returning is not the write being effective: the transaction has
/// to reach the synchronizer and come back. Reading the serial rather than
/// assuming 1 is also what the endpoint requires — it refuses a stale pin.
async fn wait_for_party(config: &NodeConfig, party_id: &str) -> anyhow::Result<u32> {
    for _ in 0..60 {
        if let Some(current) = read_party_to_participant(config, party_id).await? {
            return Ok(current.serial);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    anyhow::bail!("{party_id} never appeared in head state after allocation")
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: local_party_adopt_endpoints");

    let config = p1_config(f)?;
    // A local party's namespace IS its participant's. That is the definition,
    // and it is why the conversion cannot move the party to a shared namespace:
    // the id embeds the namespace and the namespace never changes.
    let p1 = CantonId::parse(&f.p1.participant_id)?;
    let hint = fresh_prefix("adopt-api");
    let party_id = format!("{hint}::{ns}", ns = p1.namespace.to_hex());
    let wallet = ExternalKeyPair::generate();
    info!("Local party for the adopt-key endpoints: {party_id}");

    Scenario::with_ctx(format!("local party {hint} adopts a key via the API"), ())
        .when(
            "P1 allocates a local party, then a wallet converts it through /v0/tenant",
            {
                let party_id = party_id.clone();
                let public_key = wallet.public_key_b64();
                // Copied out so each retry can rebuild the key; the Zeroizing
                // wrapper is not Copy.
                let seed = *wallet.seed();
                move |f, _| {
                    let party_id = party_id.clone();
                    let public_key = public_key.clone();
                    let config = config.clone();
                    let wallet = ExternalKeyPair::from_seed(seed);
                    Box::pin(async move {
                        allocate_local_party(&config, &party_id)
                            .await
                            .context("allocating the local party")?;
                        let base_serial = wait_for_party(&config, &party_id)
                            .await
                            .context("waiting for the local party to reach head state")?;

                        // The wallet asks the node to build the conversion. It
                        // never sees the node's namespace key and the node never
                        // sees the wallet's.
                        let prep: Value = f
                            .post_json(
                                f.p1.http,
                                "/v0/tenant/local-party/adopt-key/prepare",
                                &json!({
                                    "party_id": party_id,
                                    "public_key": public_key,
                                    "base_serial": base_serial,
                                }),
                            )
                            .await?;
                        let hashes = prep
                            .get("transaction_hashes")
                            .and_then(Value::as_array)
                            .context("prepare response missing transaction_hashes")?
                            .iter()
                            .map(|h| {
                                let encoded = h
                                    .as_str()
                                    .context("transaction_hashes entry is not a string")?;
                                STANDARD
                                    .decode(encoded)
                                    .context("transaction hash is not valid base64")
                            })
                            .collect::<anyhow::Result<Vec<_>>>()?;
                        let topology_transactions = prep
                            .get("topology_transactions")
                            .cloned()
                            .context("prepare response missing topology_transactions")?;

                        // The owner's half of the two signatures topology.proto
                        // demands for adding a signing key. The node adds its
                        // namespace half inside the onboard call.
                        let signatures: Vec<String> = hashes
                            .iter()
                            .map(|h| STANDARD.encode(wallet.sign(h)))
                            .collect();

                        let _: Value = f
                            .post_json(
                                f.p1.http,
                                "/v0/tenant/local-party/adopt-key/onboard",
                                &json!({
                                    "party_id": party_id,
                                    "base_serial": base_serial,
                                    "public_key": public_key,
                                    "topology_transactions": topology_transactions,
                                    "signatures": signatures,
                                    "signed_by": wallet.fingerprint(),
                                }),
                            )
                            .await?;
                        Ok(())
                    })
                }
            },
        )
        .then(
            "the converted party carries the wallet key and its host confirms",
            Duration::from_secs(120),
            {
                let party_id = party_id.clone();
                let expected_public_key = wallet.public_key_bytes();
                move |f, _| {
                    let party_id = party_id.clone();
                    Box::pin(async move {
                        let config = p1_config(&*f).ok()?;
                        let current =
                            read_party_to_participant(&config, &party_id).await.ok()??;
                        let keys = current.mapping.party_signing_keys.as_ref()?;
                        // Presence alone would pass for any key. What matters is
                        // that the party answers to the wallet's key and only
                        // that one. Usage is not compared: Canton normalizes it
                        // by appending ProofOfOwnership.
                        let expected =
                            dec_party_manager::workflow::external_party::steps::party_signing_key(
                                &expected_public_key,
                            );
                        // Confirmation, not Submission: the host stops submitting
                        // for a party that signs for itself, and Canton refuses
                        // the mapping otherwise.
                        let demoted =
                            current.mapping.participants.iter().all(|p| {
                                p.permission == ParticipantPermission::Confirmation as i32
                            });
                        if !demoted
                            || keys.threshold != 1
                            || keys.keys.len() != 1
                            || keys.keys[0].public_key != expected.public_key
                            || keys.keys[0].format != expected.format
                            || keys.keys[0].key_spec != expected.key_spec
                        {
                            return None;
                        }
                        info!(
                            "local party converted through the tenant endpoints (serial \
                             {serial})",
                            serial = current.serial
                        );
                        Some(Ok(()))
                    })
                }
            },
        )
        .run(f)
        .await
}
