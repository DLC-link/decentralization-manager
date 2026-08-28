//! **Spike:** can an existing *local* party adopt a `party_signing_keys` entry?
//!
//! This is the deciding question for the scoping study's Plan B1 — converting a
//! partner's existing local party into a co-validated, externally-signed one —
//! and the study left it open because nobody had tried it. The authorization
//! rules permit adding signing keys to an existing party; whether Canton's
//! runtime accepts the flip from participant-signed to externally-signed
//! mid-life was unproven.
//!
//! It is a question about Canton's behaviour, so it is answered here rather
//! than reasoned about. The phase:
//!
//! 1. Creates a genuine local party on P1 — namespace is P1's own participant
//!    namespace, which is exactly what makes a party local — at serial 1.
//! 2. Writes serial 2 adding a wallet-held Ed25519 key as the party's
//!    `party_signing_keys`, authorized by P1's namespace key alone (a local
//!    party's namespace *is* the participant's key, so the participant can
//!    authorize its own party's topology).
//! 3. Asserts Canton accepted it and the key is in head state.
//!
//! **A failure here is a result, not a flake.** If Canton refuses, B1 does not
//! exist and the product answer for a local party is B2 — threshold-1 failover
//! hosting with no application change. That is worth learning from CI rather
//! than from a partner integration.
//!
//! What this does not yet prove: that the party can then actually *submit*
//! externally-signed transactions with that key. Adding the key and honouring
//! it at submission time are two different runtime paths, and the second needs
//! a ledger credential this harness does not wire up for an arbitrary party.
//! Step 3 is the necessary condition; if it fails, the sufficient one is moot.

use std::time::Duration;

use anyhow::Context;
use base64::Engine as _;
use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{
        PartyToParticipant, TopologyMapping,
        enums::{ParticipantPermission, TopologyChangeOp},
        party_to_participant::HostingParticipant,
        topology_mapping,
    },
    topology::admin::v30::{
        AddTransactionsRequest, GenerateTransactionsRequest, generate_transactions_request,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use tracing::info;

use dec_party_manager::{
    canton_id::CantonId,
    config::NodeConfig,
    workflow::{
        external_party::add_hosts::read_party_to_participant,
        topology::{sign_transactions_with_topology_retry, synchronizer_store_id},
    },
};
use decman_wallet::ExternalKeyPair;

use crate::common::{Fixture, chaos::fresh_prefix, scenario::Scenario};

/// Point a `NodeConfig` at P1's Canton, so the phase can write topology the way
/// the node itself does. The admin API is tokenless, which is the same path the
/// whole tenant flow uses.
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

/// Submit one `PartyToParticipant` at `serial`, signed by whatever key the node
/// picks — for a local party that is the participant's own namespace key, which
/// is the party's namespace too.
async fn submit_mapping(
    config: &NodeConfig,
    mapping: PartyToParticipant,
    serial: u32,
) -> anyhow::Result<()> {
    let synchronizer_id = dec_party_manager::utils::get_synchronizer_id(config).await?;
    let store = synchronizer_store_id(&synchronizer_id);

    let mut client = TopologyManagerWriteServiceClient::new(config.admin_channel().await?);
    let generated = client
        .generate_transactions(tonic::Request::new(GenerateTransactionsRequest {
            proposals: vec![generate_transactions_request::Proposal {
                operation: TopologyChangeOp::AddReplace as i32,
                serial,
                mapping: Some(generate_transactions_request::proposal::Mapping::V30(
                    TopologyMapping {
                        mapping: Some(topology_mapping::Mapping::PartyToParticipant(mapping)),
                    },
                )),
                store: Some(store.clone()),
            }],
            base_request: None,
        }))
        .await
        .context("GenerateTransactions failed")?
        .into_inner()
        .generated_transactions;

    let unsigned: Vec<_> = generated
        .into_iter()
        .map(|tx| {
            canton_proto_rs::com::digitalasset::canton::protocol::v30::SignedTopologyTransaction {
                transaction: tx.serialized_transaction,
                signatures: vec![],
                proposal: false,
                multi_transaction_signatures: vec![],
            }
        })
        .collect();

    let signed = sign_transactions_with_topology_retry(
        config,
        canton_proto_rs::com::digitalasset::canton::topology::admin::v30::SignTransactionsRequest {
            transactions: unsigned,
            signed_by: vec![],
            store: Some(store.clone()),
            force_flags: vec![],
        },
        "local-party spike",
    )
    .await?
    .transactions;

    client
        .add_transactions(tonic::Request::new(AddTransactionsRequest {
            transactions: signed,
            force_changes: vec![],
            store: Some(store),
            wait_to_become_effective: None,
        }))
        .await
        .context("AddTransactions failed")?;
    Ok(())
}

pub async fn run(f: &mut Fixture) -> anyhow::Result<()> {
    info!("Phase: local_party_adopt_key");

    let config = p1_config(f)?;
    // A local party's namespace IS its participant's namespace. That is the
    // whole definition, and it is what makes the party un-decentralizable in
    // place: the id embeds the namespace and the namespace never changes.
    let p1 = CantonId::parse(&f.p1.participant_id)?;
    let hint = fresh_prefix("spike-local");
    let party_id = format!("{hint}::{ns}", ns = p1.namespace.to_hex());
    let wallet = ExternalKeyPair::generate();
    info!("Local party for the adopt-key spike: {party_id}");

    Scenario::with_ctx(format!("local party {hint} adopts a wallet key"), ())
        .when(
            "P1 allocates a local party, then adds a party signing key",
            {
                let party_id = party_id.clone();
                let public_key = wallet.public_key_b64();
                move |_f, _| {
                    let party_id = party_id.clone();
                    let public_key = public_key.clone();
                    let config = config.clone();
                    Box::pin(async move {
                        let hosts = vec![HostingParticipant {
                            participant_uid: config.participant_id().to_string(),
                            // Submission: this is what a real local party looks like
                            // before any conversion — the participant submits for it.
                            permission: ParticipantPermission::Submission as i32,
                            onboarding: None,
                        }];

                        // 1) The party, as it exists at a partner today.
                        submit_mapping(
                            &config,
                            PartyToParticipant {
                                party: party_id.clone(),
                                threshold: 1,
                                participants: hosts.clone(),
                                party_signing_keys: None,
                            },
                            1,
                        )
                        .await
                        .context("allocating the local party at serial 1")?;

                        // 2) The question: bolt a wallet-held key onto it.
                        // Built by the product's own helper, so the spike proves
                        // Canton accepts the key DecMan would actually write, not a
                        // lookalike assembled in a test.
                        let raw = base64::engine::general_purpose::STANDARD
                            .decode(&public_key)
                            .context("wallet public key is not base64")?;
                        let raw: [u8; 32] = raw
                            .try_into()
                            .map_err(|_| anyhow::anyhow!("wallet public key must be 32 bytes"))?;
                        let key =
                            dec_party_manager::workflow::external_party::steps::party_signing_key(
                                &raw,
                            );
                        submit_mapping(
                        &config,
                        PartyToParticipant {
                            party: party_id.clone(),
                            threshold: 1,
                            participants: hosts,
                            party_signing_keys: Some(
                                canton_proto_rs::com::digitalasset::canton::crypto::v30::
                                    SigningKeysWithThreshold {
                                        keys: vec![key],
                                        threshold: 1,
                                    },
                            ),
                        },
                        2,
                    )
                    .await
                    .context(
                        "adding party_signing_keys to the existing local party — if Canton \
                         refuses this, Plan B1 does not exist and B2 is the product answer",
                    )?;
                        Ok(())
                    })
                }
            },
        )
        .then(
            "the local party carries the wallet key in head state",
            Duration::from_secs(120),
            {
                let party_id = party_id.clone();
                move |f, _| {
                    let party_id = party_id.clone();
                    Box::pin(async move {
                        let config = p1_config(&*f).ok()?;
                        let current =
                            read_party_to_participant(&config, &party_id).await.ok()??;
                        let keys = current.mapping.party_signing_keys.as_ref()?;
                        if keys.keys.len() != 1 || current.serial < 2 {
                            return None;
                        }
                        info!(
                            "SPIKE RESULT: Canton accepted party_signing_keys on an existing \
                             local party (serial {serial}) — Plan B1 is viable",
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
