use canton_proto_rs::com::digitalasset::canton::protocol::v30::{
    DecentralizedNamespaceDefinition, PartyToParticipant,
};
use sqlx::SqlitePool;

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{
        kick::KickConfig,
        proposal_store,
        signing_keys::{
            adopt_legacy_signing_keys, known_signing_keys_by_member, signing_keys_after_kick,
        },
        storage::{WorkflowStorage, artifact_kinds},
        thresholds::{PartyThresholds, ensure_carried_from},
        topology,
    },
};

/// Create kick proposals
///
/// This step creates:
/// - DNS proposal to update namespace (remove kicked owner) — `KICK_DNS_PROPOSAL`
/// - P2P proposal to remove participant from mapping, with the kicked
///   member's Daml key taken out of `party_signing_keys`, the signing
///   threshold moved to the new one, and the confirmation threshold set by
///   [`carried_confirmation_threshold`](crate::workflow::thresholds::carried_confirmation_threshold)
///   — `KICK_P2P_PROPOSAL`
/// - New namespace definition — `KICK_NEW_NAMESPACE_DEF` (used by submit)
/// - Full party id — `KICK_PARTY_ID` (used by submit)
pub async fn create_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    kick_config: &KickConfig,
) -> Result {
    tracing::info!("Creating kick proposals...");

    // Read current namespace definition (length-prefixed protobuf, written by
    // export_state).
    let namespace_bytes = storage
        .read_artifact(instance_name, artifact_kinds::KICK_NAMESPACE_DEF, None)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("KICK_NAMESPACE_DEF artifact missing — did ExportState run?")
        })?;
    let current_namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_bytes(&namespace_bytes)?;

    tracing::info!(
        "Current namespace: {namespace}, threshold: {threshold}, owners: {owners_count}",
        namespace = current_namespace_def.decentralized_namespace,
        threshold = current_namespace_def.threshold,
        owners_count = current_namespace_def.owners.len()
    );

    // Read kick target (plaintext fingerprint).
    let kick_target_bytes = storage
        .read_artifact(instance_name, artifact_kinds::KICK_TARGET_NAMESPACE, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("KICK_TARGET_NAMESPACE artifact missing"))?;
    let kick_target = String::from_utf8(kick_target_bytes)?.trim().to_string();
    tracing::info!("Kick target: {kick_target}");

    // Read new threshold (plaintext integer).
    let threshold_bytes = storage
        .read_artifact(instance_name, artifact_kinds::KICK_NEW_THRESHOLD, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("KICK_NEW_THRESHOLD artifact missing"))?;
    let new_threshold: i32 = String::from_utf8(threshold_bytes)?
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("Failed to parse new threshold: {e}"))?;
    let thresholds = PartyThresholds::new(new_threshold, kick_config.confirmation_threshold);
    tracing::info!("New thresholds: {thresholds:?}");

    // Create new owner set without kicked member
    let new_owners: Vec<String> = current_namespace_def
        .owners
        .iter()
        .filter(|owner| *owner != &kick_target)
        .cloned()
        .collect();

    if new_owners.is_empty() {
        anyhow::bail!("Cannot remove all owners from decentralized namespace");
    }

    tracing::info!("New owners count: {count}", count = new_owners.len());

    // Create new namespace definition (keeping the same namespace hash)
    let new_namespace_def = DecentralizedNamespaceDefinition {
        decentralized_namespace: current_namespace_def.decentralized_namespace.clone(),
        threshold: new_threshold,
        owners: new_owners,
    };

    // Get party ID using prefix from decentralized party ID (provided via UI)
    let party_id_str = format!(
        "{party_id_prefix}::{namespace}",
        party_id_prefix = kick_config.decentralized_party_id.prefix,
        namespace = current_namespace_def.decentralized_namespace
    );
    let party_id = CantonId::parse(&party_id_str)?;
    tracing::info!("Party ID: {party_id}");

    // Read current P2P mapping to get the current state
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    // The serials the proposals are pinned to, read with the mappings they
    // are built on so the two cannot disagree.
    let head = proposal_store::fetch_party_head(config, &synchronizer_id, &party_id).await?;
    head.ensure_namespace_is(&current_namespace_def)?;
    let current_p2p = head.p2p.clone();

    tracing::info!(
        "Current P2P mapping has {count} participant(s)",
        count = current_p2p.participants.len()
    );

    // Create new P2P mapping without kicked participant. A member that owns
    // the namespace without hosting the party has no host entry, so its kick
    // keeps every host.
    let kicked_host = kick_config
        .participant_id
        .as_ref()
        .map(ToString::to_string)
        .filter(|uid| {
            current_p2p
                .participants
                .iter()
                .any(|p| &p.participant_uid == uid)
        });
    let member_count = current_p2p.participants.len();
    let on_chain = PartyThresholds::new(
        current_namespace_def.threshold,
        Some(i32::try_from(current_p2p.threshold)?),
    );
    let new_participants: Vec<_> = current_p2p
        .participants
        .into_iter()
        .filter(|p| Some(&p.participant_uid) != kicked_host.as_ref())
        .collect();

    tracing::info!(
        "New P2P mapping will have {count} participant(s)",
        count = new_participants.len()
    );

    if new_participants.is_empty() {
        anyhow::bail!("Cannot remove all participants from party mapping");
    }
    ensure_carried_from(on_chain, new_participants.len(), thresholds)?;

    // The signing threshold moves with the namespace one, and the kicked
    // member's Daml key comes out. Carrying `party_signing_keys` over verbatim
    // left the removed member's key counting towards the party's signing
    // threshold and left that threshold at its old value, which every peer
    // refuses to sign (#428).
    let current_signing_keys = current_p2p
        .party_signing_keys
        .map(|sk| sk.keys)
        .unwrap_or_default();
    let survivors: Vec<String> = new_participants
        .iter()
        .map(|p| p.participant_uid.clone())
        .collect();
    let new_signing_keys = if current_signing_keys.len() < member_count {
        // A party onboarded before Canton 3.4 keeps its keys in a deprecated
        // PartyToKeyMapping, and a pre-1.10.0 add leaves a party holding some
        // inline and the rest there. Adopting the survivors' keys moves them
        // inline and leaves the departing member's behind in the same step.
        adopt_legacy_signing_keys(
            config,
            storage,
            &synchronizer_id,
            &party_id,
            &survivors,
            &current_signing_keys,
        )
        .await?
    } else if kicked_host.is_some() || current_signing_keys.len() > member_count {
        // A member that removes its own hosting entry leaves its key behind,
        // so an owner that hosts nothing may still hold one. A kick takes out
        // every such key along with the kicked member's, so one key per host
        // stays, as the peers require.
        let removed = kick_config.participant_id.as_ref().map_or_else(
            || kick_config.namespace_fingerprint.clone(),
            ToString::to_string,
        );
        let claims =
            known_signing_keys_by_member(config, storage, &party_id, &current_signing_keys).await?;
        signing_keys_after_kick(&current_signing_keys, &removed, &survivors, &claims)?
    } else {
        current_signing_keys
    };

    if new_signing_keys.len() != new_participants.len() {
        anyhow::bail!(
            "Kick would leave the party with {keys} signing key(s) for {members} member(s). \
             Every member contributes exactly one, so the party's key set does not match \
             its membership and the peers would refuse the proposal",
            keys = new_signing_keys.len(),
            members = new_participants.len()
        );
    }

    let new_p2p = thresholds.party_to_participant(
        party_id_str.clone(),
        new_participants,
        new_signing_keys,
    )?;

    // Signed without being published: nothing reaches the synchronizer until
    // submit, so a member that never signs leaves the party unchanged (#448).
    tracing::info!("Signing kick proposals...");
    let p2p_change = changed_mapping(&head.p2p, new_p2p);
    let signed = proposal_store::sign_party_proposals(
        config,
        &synchronizer_id,
        instance_name,
        &party_id,
        &head,
        Some(new_namespace_def.clone()),
        p2p_change,
        topology::party_proposal_force_flags(),
    )
    .await?;
    let dns_transaction = signed
        .dns
        .ok_or_else(|| anyhow::anyhow!("No DNS kick proposal was signed"))?;
    // An owner that hosts nothing, left no key behind and keeps the threshold
    // leaves the hosting mapping as it is. Canton refuses to re-sign that at a
    // new serial, so the peers get the transaction already in force. They see
    // it as applied, and submit skips it.
    let p2p_transaction = match signed.p2p {
        Some(p2p) => p2p,
        None => {
            tracing::info!("The kick leaves the hosting mapping as it is");
            topology::fetch_signed_party_to_participant(config, &synchronizer_id, &party_id).await?
        }
    };

    // Persist proposals + supporting data to workflow storage. Each protobuf
    // is written with the same `varint(len)||proto` framing the original file
    // path used (so `read_first_message_from_bytes` works unchanged).
    let dns_bytes = utils::encode_length_prefixed_message(&dns_transaction);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::KICK_DNS_PROPOSAL,
            None,
            &dns_bytes,
        )
        .await?;
    tracing::info!("Saved DNS kick proposal to storage");

    let p2p_bytes = utils::encode_length_prefixed_message(&p2p_transaction);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::KICK_P2P_PROPOSAL,
            None,
            &p2p_bytes,
        )
        .await?;
    tracing::info!("Saved P2P kick proposal to storage");

    let new_namespace_bytes = utils::encode_length_prefixed_message(&new_namespace_def);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::KICK_NEW_NAMESPACE_DEF,
            None,
            &new_namespace_bytes,
        )
        .await?;
    tracing::info!("Saved new namespace definition to storage");

    // Save party ID — plaintext, mirrors the previous file write that
    // included a trailing newline (submit trims it).
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::KICK_PARTY_ID,
            None,
            format!("{party_id}\n").as_bytes(),
        )
        .await?;

    tracing::info!("Kick proposals created and saved successfully");
    Ok(())
}

/// The mapping to propose, or `None` when it equals the one in force.
fn changed_mapping(
    current: &PartyToParticipant,
    proposed: PartyToParticipant,
) -> Option<PartyToParticipant> {
    (proposed != *current).then_some(proposed)
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::{SigningKeysWithThreshold, SigningPublicKey},
        protocol::v30::party_to_participant::HostingParticipant,
    };

    use super::*;

    fn mapping(threshold: u32, keys: u8) -> PartyToParticipant {
        PartyToParticipant {
            party: "party".to_string(),
            threshold,
            participants: vec![HostingParticipant {
                participant_uid: "p1".to_string(),
                permission: 2,
                onboarding: None,
            }],
            party_signing_keys: Some(SigningKeysWithThreshold {
                keys: (1..=keys)
                    .map(|seed| SigningPublicKey {
                        public_key: vec![seed; 32],
                        ..Default::default()
                    })
                    .collect(),
                threshold,
            }),
        }
    }

    /// Canton refuses a mapping equal to the one in force at a new serial, so
    /// an owner-only kick at the same threshold proposes none.
    #[test]
    fn an_unchanged_mapping_is_not_proposed() {
        assert_eq!(changed_mapping(&mapping(1, 1), mapping(1, 1)), None);
    }

    #[test]
    fn a_new_threshold_or_a_dropped_key_is_proposed() {
        assert_eq!(
            changed_mapping(&mapping(2, 1), mapping(1, 1)),
            Some(mapping(1, 1))
        );
        assert_eq!(
            changed_mapping(&mapping(1, 2), mapping(1, 1)),
            Some(mapping(1, 1))
        );
    }
}
