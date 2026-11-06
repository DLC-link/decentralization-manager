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
        change_threshold::ChangeThresholdConfig,
        proposal_store,
        signing_keys::adopt_legacy_signing_keys,
        storage::{WorkflowStorage, artifact_kinds},
        thresholds::{PartyThresholds, check_confirmation_threshold},
        topology,
    },
};

/// Create the change-threshold proposals.
///
/// This step creates:
/// - DNS proposal re-issuing the namespace with the new threshold (same
///   owners) — `CHANGE_THRESHOLD_DNS_PROPOSAL`
/// - P2P proposal re-issuing the party mapping with the new confirmation
///   threshold, and the new namespace threshold on its signing keys (same
///   participants, same keys) — `CHANGE_THRESHOLD_P2P_PROPOSAL`
/// - New namespace definition — `CHANGE_THRESHOLD_NEW_NAMESPACE_DEF` (used by
///   submit to poll the topology)
/// - Full party id — `CHANGE_THRESHOLD_PARTY_ID` (used by submit)
pub async fn create_proposals(
    config: &NodeConfig,
    storage: &SqlitePool,
    instance_name: &str,
    change_config: &ChangeThresholdConfig,
) -> Result {
    tracing::info!("Creating change-threshold proposals...");

    // Read current namespace definition (length-prefixed protobuf, written by
    // export_state).
    let namespace_bytes = storage
        .read_artifact(
            instance_name,
            artifact_kinds::CHANGE_THRESHOLD_NAMESPACE_DEF,
            None,
        )
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "CHANGE_THRESHOLD_NAMESPACE_DEF artifact missing — did ExportState run?"
            )
        })?;
    let current_namespace_def: DecentralizedNamespaceDefinition =
        utils::read_first_message_from_bytes(&namespace_bytes)?;

    let thresholds = change_config.thresholds();
    tracing::info!(
        "Current namespace: {namespace}, threshold: {threshold} -> {new_threshold}, owners: {owners_count}",
        namespace = current_namespace_def.decentralized_namespace,
        threshold = current_namespace_def.threshold,
        new_threshold = thresholds.namespace,
        owners_count = current_namespace_def.owners.len()
    );

    // Keep the full owner set — only the threshold changes.
    let new_namespace_def = DecentralizedNamespaceDefinition {
        decentralized_namespace: current_namespace_def.decentralized_namespace.clone(),
        threshold: thresholds.namespace,
        owners: current_namespace_def.owners.clone(),
    };

    // Party id: prefix (from the request) :: current namespace fingerprint.
    // The namespace hash is over the owner set only, which is unchanged, so
    // the party id is stable across the threshold change.
    let party_id_str = format!(
        "{party_id_prefix}::{namespace}",
        party_id_prefix = change_config.decentralized_party_id.prefix,
        namespace = current_namespace_def.decentralized_namespace
    );
    let party_id = CantonId::parse(&party_id_str)?;
    tracing::info!("Party ID: {party_id}");

    // Read the current P2P mapping so we re-issue it verbatim with only the
    // threshold changed.
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    tracing::debug!("Using synchronizer ID: {synchronizer_id}");

    // The serials the proposals are pinned to, read with the mappings they
    // are built on so the two cannot disagree.
    let head = proposal_store::fetch_party_head(config, &synchronizer_id, &party_id).await?;
    head.ensure_namespace_is(&current_namespace_def)?;
    let current_p2p = head.p2p.clone();
    tracing::info!(
        "Current P2P mapping has {count} participant(s), threshold {threshold} -> {confirmation}",
        count = current_p2p.participants.len(),
        threshold = current_p2p.threshold,
        confirmation = thresholds.confirmation,
    );
    // The HTTP handler bounds the confirmation threshold by cached
    // membership; the live host set is the source of truth.
    check_confirmation_threshold(thresholds.confirmation, current_p2p.participants.len())?;
    ensure_something_changes(&current_namespace_def, &current_p2p, thresholds)?;

    // A `PartyToParticipant` carries two thresholds: how many hosting
    // participants must confirm, and how many of the party's signing keys
    // authorize a transaction for it. The signing one follows the namespace
    // threshold, so re-issuing the signing keys verbatim changed only half of
    // it (#428).
    let current_signing_keys = current_p2p
        .party_signing_keys
        .map(|sk| sk.keys)
        .unwrap_or_default();
    let signing_keys = if current_signing_keys.len() < current_p2p.participants.len() {
        // A party onboarded before Canton 3.4 keeps its keys in a deprecated
        // PartyToKeyMapping, which carries a threshold of its own, and a
        // pre-1.10.0 add leaves a party holding some inline and the rest there.
        // Moving the members' keys inline puts both thresholds in one place.
        let members: Vec<String> = current_p2p
            .participants
            .iter()
            .map(|p| p.participant_uid.clone())
            .collect();
        adopt_legacy_signing_keys(
            config,
            storage,
            &synchronizer_id,
            &party_id,
            &members,
            &current_signing_keys,
        )
        .await?
    } else {
        current_signing_keys
    };

    let new_p2p = thresholds.party_to_participant(
        party_id_str.clone(),
        current_p2p.participants,
        signing_keys,
    )?;

    // Signed without being published: nothing reaches the synchronizer until
    // submit, so a member that never signs leaves the party unchanged (#448).
    tracing::info!("Signing change-threshold proposals...");
    let signed = proposal_store::sign_party_proposals(
        config,
        &synchronizer_id,
        instance_name,
        &party_id,
        &head,
        Some(new_namespace_def.clone()),
        new_p2p,
        topology::party_proposal_force_flags(),
    )
    .await?;
    let dns_transaction = signed
        .dns
        .ok_or_else(|| anyhow::anyhow!("No DNS change-threshold proposal was signed"))?;
    let p2p_transaction = signed.p2p;

    // Persist proposals + supporting data. Each protobuf is written with the
    // same `varint(len)||proto` framing the file path used, so
    // `read_first_message_from_bytes` works unchanged.
    let dns_bytes = utils::encode_length_prefixed_message(&dns_transaction);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::CHANGE_THRESHOLD_DNS_PROPOSAL,
            None,
            &dns_bytes,
        )
        .await?;
    tracing::info!("Saved DNS change-threshold proposal to storage");

    let p2p_bytes = utils::encode_length_prefixed_message(&p2p_transaction);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::CHANGE_THRESHOLD_P2P_PROPOSAL,
            None,
            &p2p_bytes,
        )
        .await?;
    tracing::info!("Saved P2P change-threshold proposal to storage");

    let new_namespace_bytes = utils::encode_length_prefixed_message(&new_namespace_def);
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::CHANGE_THRESHOLD_NEW_NAMESPACE_DEF,
            None,
            &new_namespace_bytes,
        )
        .await?;
    tracing::info!("Saved new namespace definition to storage");

    // Save party ID — plaintext with a trailing newline (submit trims it).
    storage
        .write_artifact(
            instance_name,
            artifact_kinds::CHANGE_THRESHOLD_PARTY_ID,
            None,
            format!("{party_id}\n").as_bytes(),
        )
        .await?;

    tracing::info!("Change-threshold proposals created and saved successfully");
    Ok(())
}

/// Refuse a run that would re-issue both thresholds unchanged.
fn ensure_something_changes(
    namespace_def: &DecentralizedNamespaceDefinition,
    p2p: &PartyToParticipant,
    thresholds: PartyThresholds,
) -> Result {
    let confirmation = i32::try_from(p2p.threshold)?;
    if namespace_def.threshold == thresholds.namespace && confirmation == thresholds.confirmation {
        anyhow::bail!(
            "threshold {namespace} and confirmation threshold {confirmation} are already the \
             current ones — nothing to change",
            namespace = thresholds.namespace,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn namespace(threshold: i32) -> DecentralizedNamespaceDefinition {
        DecentralizedNamespaceDefinition {
            decentralized_namespace: "ns".to_string(),
            threshold,
            owners: vec!["a".to_string(), "b".to_string(), "c".to_string()],
        }
    }

    fn p2p(threshold: u32) -> PartyToParticipant {
        PartyToParticipant {
            party: "party".to_string(),
            threshold,
            participants: Vec::new(),
            party_signing_keys: None,
        }
    }

    #[test]
    fn refuses_a_run_that_changes_neither_threshold() {
        assert!(
            ensure_something_changes(&namespace(2), &p2p(2), PartyThresholds::new(2, None))
                .is_err()
        );
        assert!(
            ensure_something_changes(&namespace(1), &p2p(3), PartyThresholds::new(1, Some(3)))
                .is_err()
        );
    }

    /// Moving only the confirmation threshold is a change, though the
    /// namespace threshold stays where it is.
    #[test]
    fn accepts_a_run_that_changes_only_the_confirmation_threshold() -> Result {
        ensure_something_changes(&namespace(2), &p2p(2), PartyThresholds::new(2, Some(3)))
    }

    /// Without a confirmation threshold, a party whose two thresholds differ
    /// is brought back to one value, as before they could differ.
    #[test]
    fn an_omitted_confirmation_threshold_rejoins_the_two() -> Result {
        let config = ChangeThresholdConfig::new(
            CantonId::parse(&format!("xyz::1220{}", "ab".repeat(32)))?,
            2,
            None,
            2,
            "xyz-change-threshold-1".to_string(),
        );
        ensure_something_changes(&namespace(2), &p2p(3), config.thresholds())?;
        assert_eq!(config.thresholds(), PartyThresholds::new(2, Some(2)));
        Ok(())
    }
}
