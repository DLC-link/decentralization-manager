//! Canton regression for shadowed pre-3.4 party keys. The live party already
//! has inline keys; seed the legacy mapping that migration used to leave behind,
//! including an unclaimed key. Run a real threshold-change workflow and verify
//! the removal on all participants. Also interrupt a partial retirement and
//! retry it with another namespace owner's signature.

use std::time::Duration;

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::{
    crypto::{
        admin::v30::{
            GenerateSigningKeyRequest, generate_signing_key_response,
            vault_service_client::VaultServiceClient,
        },
        v30::SigningKeyUsage,
    },
    protocol::v30::{
        PartyToKeyMapping, TopologyMapping, enums::TopologyChangeOp, topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, ListPartyToKeyMappingRequest, authorize_request,
        list_party_to_key_mapping_response::result::Item,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use dec_party_manager::{config::NodeConfig, utils, workflow::topology};

use crate::common::{Fixture, chaos::poll_until};

type Legacy = (i32, PartyToKeyMapping);

/// A `NodeConfig` per participant that reaches its Canton admin API directly.
pub fn configs(f: &Fixture) -> anyhow::Result<Vec<NodeConfig>> {
    [&f.p1, &f.p2, &f.p3]
        .into_iter()
        .enumerate()
        .map(|(index, node)| {
            let mut config = NodeConfig::default();
            config.canton.admin_api_host = "127.0.0.1".into();
            config.canton.admin_api_port =
                std::env::var(format!("P{}_CANTON_ADMIN", index + 1))?.parse()?;
            config.node.participant_id = Some(node.participant_id.parse()?);
            Ok(config)
        })
        .collect()
}

async fn head(
    config: &NodeConfig,
    sync: &str,
    party: &str,
) -> anyhow::Result<Option<(i32, i32, PartyToKeyMapping)>> {
    let response = TopologyManagerReadServiceClient::new(config.admin_channel().await?)
        .list_party_to_key_mapping(ListPartyToKeyMappingRequest {
            base_query: Some(topology::head_state_query(sync)),
            filter_party: party.into(),
        })
        .await?
        .into_inner();
    Ok(response.results.into_iter().find_map(|row| {
        let context = row.context?;
        let Item::V30(mapping) = row.item?;
        (mapping.party == party).then_some((context.serial, context.operation, mapping))
    }))
}

#[allow(deprecated)]
pub async fn seed(f: &Fixture) -> anyhow::Result<Legacy> {
    let configs = configs(f)?;
    let party: common::canton_id::CantonId = f.party_id()?.parse()?;
    let sync = utils::get_synchronizer_id(&configs[0]).await?;
    let inline = topology::fetch_p2p_mapping(&configs[0], &sync, &party)
        .await?
        .party_signing_keys
        .context("fixture needs inline keys")?;
    let serial = head(&configs[0], &sync, &party.to_string())
        .await?
        .map_or(1, |(serial, _, _)| serial + 1);
    // A key no current member claims for this party, held by P1 only so the
    // fixture can provide the proof of possession required to install it.
    let generated = VaultServiceClient::new(configs[0].admin_channel().await?)
        .generate_signing_key(GenerateSigningKeyRequest {
            name: format!("legacy-retirement-{serial}"),
            usage_v30: vec![SigningKeyUsage::Protocol as i32],
            ..Default::default()
        })
        .await?
        .into_inner();
    let Some(generate_signing_key_response::PublicKey::V30(key)) = generated.public_key else {
        anyhow::bail!("fixture key generation returned no key");
    };
    let mapping = PartyToKeyMapping {
        party: party.to_string(),
        threshold: 1,
        signing_keys: vec![key],
    };
    let request = AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            authorize_request::Proposal {
                change: TopologyChangeOp::AddReplace as i32,
                serial: serial.try_into()?,
                mapping: Some(authorize_request::proposal::Mapping::V30(TopologyMapping {
                    mapping: Some(topology_mapping::Mapping::PartyToKeyMapping(
                        mapping.clone(),
                    )),
                })),
            },
        )),
        store: Some(topology::synchronizer_store_id(&sync)),
        must_fully_authorize: false,
        ..Default::default()
    };
    // At threshold 1 only P1 is needed; at threshold 2 both contribute.
    // Avoid reauthorizing an already-effective AddReplace with the same serial.
    for config in configs.iter().take(inline.threshold as usize) {
        topology::authorize_with_topology_retry(
            config,
            request.clone(),
            "seed shadowed legacy keys",
        )
        .await?;
    }
    poll_until(Duration::from_secs(60), || async {
        for config in &configs {
            if topology::fetch_party_to_key_mapping(config, &sync, &party)
                .await?
                .as_ref()
                != Some(&mapping)
            {
                return Ok(false);
            }
        }
        Ok(true)
    })
    .await
    .context("legacy mapping did not become active on all three participants")?;
    Ok((serial, mapping))
}

pub async fn assert_removed(f: &Fixture, legacy: &Legacy) -> anyhow::Result<()> {
    let configs = configs(f)?;
    let sync = utils::get_synchronizer_id(&configs[0]).await?;
    poll_until(Duration::from_secs(60), || async {
        for config in &configs {
            let Some((serial, operation, mapping)) = head(config, &sync, &legacy.1.party).await?
            else {
                return Ok(false);
            };
            if serial != legacy.0 + 1
                || operation != TopologyChangeOp::Remove as i32
                || mapping != legacy.1
            {
                return Ok(false);
            }
        }
        Ok(true)
    })
    .await
    .context("exact next-serial removal did not propagate to all participants")
}

pub async fn retry_after_partial_authorization(f: &Fixture) -> anyhow::Result<()> {
    let legacy = seed(f).await?;
    let configs = configs(f)?;
    let party: common::canton_id::CantonId = f.party_id()?.parse()?;
    let sync = utils::get_synchronizer_id(&configs[0]).await?;
    let inline = topology::fetch_p2p_mapping(&configs[0], &sync, &party).await?;
    anyhow::ensure!(
        inline
            .party_signing_keys
            .as_ref()
            .is_some_and(|k| k.threshold == 2),
        "retry regression needs threshold 2"
    );

    // Simulate a coordinator interrupted after contributing its signature.
    // A single owner must not satisfy the post-change namespace threshold.
    let first = tokio::time::timeout(
        Duration::from_secs(3),
        topology::retire_legacy_keys_and_wait(&configs[0], &party),
    )
    .await;
    anyhow::ensure!(
        first.is_err(),
        "retirement finished without the second owner: {first:?}"
    );
    anyhow::ensure!(
        topology::fetch_party_to_key_mapping(&configs[0], &sync, &party)
            .await?
            .is_some(),
        "one signature removed the mapping"
    );

    // The retry must reuse the pending serial, merge another member's signature,
    // and finish only after the removal has landed. Repeating completed cleanup
    // is a no-op and must not alter the inline topology.
    let (coordinator, peer) = tokio::join!(
        topology::retire_legacy_keys_and_wait(&configs[0], &party),
        topology::retire_legacy_keys(&configs[1], &sync, &party),
    );
    coordinator?;
    peer?;
    assert_removed(f, &legacy).await?;
    topology::retire_legacy_keys_and_wait(&configs[0], &party).await?;
    anyhow::ensure!(
        topology::fetch_p2p_mapping(&configs[0], &sync, &party).await? == inline,
        "retirement changed the inline topology"
    );
    Ok(())
}
