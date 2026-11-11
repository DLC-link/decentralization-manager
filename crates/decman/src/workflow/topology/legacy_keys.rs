use std::{collections::HashSet, time::Duration};

use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{
        PartyToKeyMapping, PartyToParticipant, TopologyMapping, enums, topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, ListPartyToKeyMappingRequest, authorize_request,
        list_party_to_key_mapping_response::result::Item as PartyToKeyItem,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};

use super::{
    reads::{fetch_p2p_mapping, fetch_party_to_key_mapping},
    requests::{head_state_query, synchronizer_store_id},
    retry::authorize_with_topology_retry,
};
use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    consts::{topology_retry_delay_secs, topology_retry_max_attempts},
    error::Result,
    utils,
};

/// Contribute this member's namespace signature to retiring the legacy mapping.
/// Called only after the replacement topology has become effective. Every
/// member reads the same live mapping and signs the same explicit next serial;
/// Canton merges their signatures under the current namespace threshold.
///
/// # Errors
///
/// Returns an error if the Admin API channel cannot be opened, or if the
/// `PartyToKeyMapping` or `PartyToParticipant` head read fails. Returns an
/// error if the `PartyToParticipant` names another party, or if its inline
/// signing keys are missing, repeat a key, or have an unusable threshold.
/// Returns an error if the legacy serial is not positive or cannot be advanced.
/// Returns an error if `Authorize` fails and a follow-up read still finds the
/// legacy mapping, or if that read fails.
pub async fn retire_legacy_keys(
    config: &NodeConfig,
    synchronizer_id: &str,
    party: &CantonId,
) -> Result {
    let mut reader = TopologyManagerReadServiceClient::new(config.admin_channel().await?);
    let response = reader
        .list_party_to_key_mapping(ListPartyToKeyMappingRequest {
            base_query: Some(head_state_query(synchronizer_id)),
            filter_party: party.to_string(),
        })
        .await?
        .into_inner();
    let Some((serial, mapping)) = response.results.into_iter().find_map(|row| {
        let context = row.context?;
        let PartyToKeyItem::V30(mapping) = row.item?;
        (context.operation == enums::TopologyChangeOp::AddReplace as i32
            && mapping.party == party.to_string())
        .then_some((context.serial, mapping))
    }) else {
        return Ok(());
    };
    let p2p = fetch_p2p_mapping(config, synchronizer_id, party).await?;
    validate_legacy_retirement(&p2p, &mapping)?;
    let request = legacy_retirement_request(synchronizer_id, serial, mapping)?;
    if let Err(error) =
        authorize_with_topology_retry(config, request, "retire legacy party keys").await
    {
        // Another member may have completed this exact removal while we signed.
        if fetch_party_to_key_mapping(config, synchronizer_id, party)
            .await?
            .is_some()
        {
            return Err(error);
        }
    }
    Ok(())
}

#[allow(deprecated)] // Removing the deprecated mapping requires its wire variant.
fn legacy_retirement_request(
    synchronizer_id: &str,
    serial: i32,
    mapping: PartyToKeyMapping,
) -> Result<AuthorizeRequest> {
    anyhow::ensure!(serial > 0, "Legacy mapping has an invalid serial");
    Ok(AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            authorize_request::Proposal {
                protocol_version: None,
                change: enums::TopologyChangeOp::Remove as i32,
                serial: serial
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("Legacy mapping serial overflow"))?
                    .try_into()?,
                mapping: Some(authorize_request::proposal::Mapping::V30(TopologyMapping {
                    mapping: Some(topology_mapping::Mapping::PartyToKeyMapping(mapping)),
                })),
            },
        )),
        store: Some(synchronizer_store_id(synchronizer_id)),
        must_fully_authorize: false,
        ..Default::default()
    })
}

fn validate_legacy_retirement(p2p: &PartyToParticipant, legacy: &PartyToKeyMapping) -> Result {
    anyhow::ensure!(
        p2p.party == legacy.party,
        "Legacy mapping belongs to another party"
    );
    let keys = p2p.party_signing_keys.as_ref().ok_or_else(|| {
        anyhow::anyhow!("Cannot retire legacy keys before inline keys are effective")
    })?;
    let distinct: HashSet<_> = keys.keys.iter().map(utils::compute_fingerprint).collect();
    anyhow::ensure!(
        keys.threshold > 0 && keys.threshold as usize <= distinct.len(),
        "Cannot retire legacy keys without a usable inline signing threshold"
    );
    anyhow::ensure!(
        distinct.len() == keys.keys.len(),
        "Inline signing keys contain duplicates"
    );
    Ok(())
}

/// Coordinator side of the retirement round. Peers contribute while this
/// waits; completing the workflow requires observing the removal on-chain.
///
/// # Errors
///
/// Returns an error if the synchronizer ID cannot be read, or in the cases of
/// [`retire_legacy_keys`]. Returns an error if a `PartyToKeyMapping` read fails
/// while waiting. Returns an error if the legacy mapping is still in force
/// after `topology_retry_max_attempts` checks, as when too few members signed.
pub async fn retire_legacy_keys_and_wait(config: &NodeConfig, party: &CantonId) -> Result {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    retire_legacy_keys(config, &synchronizer_id, party).await?;
    for _ in 0..topology_retry_max_attempts() {
        if fetch_party_to_key_mapping(config, &synchronizer_id, party)
            .await?
            .is_none()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(topology_retry_delay_secs())).await;
    }
    anyhow::bail!(
        "Legacy PartyToKeyMapping removal for {party} did not reach the namespace threshold; retry with enough upgraded members online"
    )
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::crypto::v30::{
        SigningKeysWithThreshold, SigningPublicKey,
    };

    use super::*;

    #[test]
    #[allow(deprecated)]
    fn legacy_retirement_removes_the_exact_mapping_at_the_next_serial() -> Result {
        let mapping = PartyToKeyMapping {
            party: "party".into(),
            ..Default::default()
        };
        let request = legacy_retirement_request("sync", 4, mapping.clone())?;
        assert!(!request.must_fully_authorize);
        assert!(request.signed_by.is_empty());
        assert!(request.force_changes.is_empty());
        assert_eq!(request.store, Some(synchronizer_store_id("sync")));
        let Some(authorize_request::Type::Proposal(proposal)) = request.r#type else {
            anyhow::bail!("expected removal proposal");
        };
        assert_eq!(proposal.serial, 5);
        assert_eq!(proposal.change, enums::TopologyChangeOp::Remove as i32);
        let Some(authorize_request::proposal::Mapping::V30(topology)) = proposal.mapping else {
            anyhow::bail!("expected topology mapping");
        };
        assert_eq!(
            topology.mapping,
            Some(topology_mapping::Mapping::PartyToKeyMapping(
                mapping.clone()
            ))
        );
        for serial in [-1, 0, i32::MAX] {
            assert!(legacy_retirement_request("sync", serial, mapping.clone()).is_err());
        }
        Ok(())
    }

    #[test]
    fn legacy_retirement_requires_usable_inline_keys() -> Result {
        let legacy = PartyToKeyMapping {
            party: "party".into(),
            ..Default::default()
        };
        let mut p2p = PartyToParticipant {
            party: legacy.party.clone(),
            ..Default::default()
        };
        assert!(validate_legacy_retirement(&p2p, &legacy).is_err());
        let key = SigningPublicKey {
            public_key: vec![1; 32],
            ..Default::default()
        };
        p2p.party_signing_keys = Some(SigningKeysWithThreshold {
            keys: vec![key.clone()],
            threshold: 1,
        });
        validate_legacy_retirement(&p2p, &legacy)?;
        for (keys, threshold) in [
            (vec![key.clone()], 0),
            (vec![key.clone()], 2),
            (vec![key.clone(), key], 1),
        ] {
            p2p.party_signing_keys = Some(SigningKeysWithThreshold { keys, threshold });
            assert!(validate_legacy_retirement(&p2p, &legacy).is_err());
        }
        p2p.party = "another-party".into();
        assert!(validate_legacy_retirement(&p2p, &legacy).is_err());
        Ok(())
    }
}
