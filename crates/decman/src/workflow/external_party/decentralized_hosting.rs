//! Approving hosting for a decentralized party that this node co-validates.
//!
//! The owners of a decentralized party hold its keys off-node, and their
//! participants submit its onboarding topology first. A covalidator submits
//! nothing of its own. It authorizes the `PartyToParticipant` proposal that is
//! already pending in its own synchronizer store. [`approve_hosting`] finds that
//! proposal by the hash the caller names, checks it, and signs it with this
//! node's topology key.
//!
//! The node never signs bytes the caller sends. It signs only what its own store
//! holds, and only after [`check_hosting_proposal`] accepts it.
//!
//! A covalidator is never an owner. When this node holds a key for one of the
//! party's owner namespaces, its signature would also count as that owner's
//! approval. It refuses such a party, before it signs anything.

use std::collections::HashSet;

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{SigningKeyUsage, SigningKeysWithThreshold},
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToParticipant, SignedTopologyTransaction,
        TopologyTransaction, topology_mapping,
    },
    topology::admin::v30::{
        SignTransactionsRequest,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};

use super::{
    add_hosts::pending_proposals,
    steps::{
        HostOnboardingStatus, OnboardingParty, check_key_usage, check_onboarding_mapping,
        host_onboarding_status,
    },
};
use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    signing::key_store::topology_transaction_hash,
    utils,
    workflow::{signing_keys::namespaces_held, topology, topology_version},
};

/// Why this node did not approve hosting. Each variant is a different answer
/// for the caller, so they are kept apart.
#[derive(Debug, thiserror::Error)]
pub enum ApproveHostingError {
    /// No pending proposal for the party names this participant. The owners'
    /// submission may not have reached this node yet, so the caller retries.
    #[error("no pending PartyToParticipant proposal for {party} names this participant yet")]
    NotPending { party: String },
    /// The proposal was replaced, or the party already exists.
    #[error("{0}")]
    Conflict(String),
    /// The request or the proposal fails a check.
    #[error("{0:#}")]
    Invalid(#[source] anyhow::Error),
    /// A Canton call failed, or this node could not read its own state.
    #[error("{0:#}")]
    Canton(#[source] anyhow::Error),
}

/// What this node did about a hosting request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Approval {
    /// This node already hosts the party, so it signed nothing.
    AlreadyHosted,
    /// This node signed and submitted the proposal. `hosted` says whether its
    /// own topology now shows the party hosted here.
    Approved { hosted: bool },
}

/// What this node reads from its own store to judge a proposal.
pub struct HostingContext<'a> {
    pub this_participant: &'a str,
    pub party_id: &'a str,
    /// The definition in force for the party's namespace, if any.
    pub namespace: Option<&'a DecentralizedNamespaceDefinition>,
    /// The party's `PartyToParticipant` transaction in force, if any.
    pub head: Option<&'a TopologyTransaction>,
}

/// Check a pending onboarding proposal for a decentralized party before this
/// node signs it.
///
/// Every rule of the single-key onboarding check applies (see
/// [`check_onboarding_mapping`]) except the signing-key rule. A decentralized
/// party may have any number of signing keys, at any threshold they can meet.
/// The party's namespace must be a decentralized namespace in force, and no
/// other mapping may be in force for the party.
///
/// # Errors
/// [`ApproveHostingError::Invalid`] when the proposal fails a rule, and
/// [`ApproveHostingError::Conflict`] when the party exists with other content.
pub fn check_hosting_proposal<'t>(
    context: &HostingContext<'_>,
    transaction: &'t TopologyTransaction,
) -> Result<&'t PartyToParticipant, ApproveHostingError> {
    let p2p = check_onboarding_mapping(
        &OnboardingParty {
            this_participant: context.this_participant,
            party_id: context.party_id,
            party_source: "the party the request names",
        },
        "the proposal",
        transaction,
    )
    .map_err(ApproveHostingError::Invalid)?;
    let keys = check_signing_keys(p2p).map_err(ApproveHostingError::Invalid)?;

    // The checks above take one field at a time. This refuses a field that a
    // later Canton version adds to the mapping, so it cannot ride along.
    let expected = PartyToParticipant {
        party: context.party_id.to_string(),
        threshold: p2p.threshold,
        participants: p2p.participants.clone(),
        party_signing_keys: Some(SigningKeysWithThreshold {
            keys: keys.keys.clone(),
            threshold: keys.threshold,
        }),
    };
    if *p2p != expected {
        return Err(ApproveHostingError::Invalid(anyhow::anyhow!(
            "the proposal carries fields beyond a plain decentralized-party PartyToParticipant \
             and will not be authorized"
        )));
    }

    if context.namespace.is_none() {
        return Err(ApproveHostingError::Invalid(anyhow::anyhow!(
            "{party} has no DecentralizedNamespaceDefinition in force; this route approves only \
             a decentralized party",
            party = context.party_id
        )));
    }
    topology::creates_mapping("P2P", transaction, context.head)
        .map_err(|e| ApproveHostingError::Conflict(format!("{e:#}")))?;
    Ok(p2p)
}

/// A decentralized party's signing keys: at least one, none twice, each able
/// to sign transactions, at a threshold the keys can meet.
fn check_signing_keys(p2p: &PartyToParticipant) -> anyhow::Result<&SigningKeysWithThreshold> {
    let Some(keys) = p2p
        .party_signing_keys
        .as_ref()
        .filter(|keys| !keys.keys.is_empty())
    else {
        anyhow::bail!("the proposal carries no party signing keys");
    };
    let mut seen = HashSet::with_capacity(keys.keys.len());
    for key in &keys.keys {
        let fingerprint = utils::compute_fingerprint(key);
        if !seen.insert(fingerprint.clone()) {
            anyhow::bail!("the proposal lists the party signing key {fingerprint} twice");
        }
        // Without Protocol usage the key can never sign for the party, as on
        // the single-key route.
        check_key_usage(
            "the proposal",
            &format!("the party signing key {fingerprint}"),
            key,
            &[SigningKeyUsage::Protocol],
        )?;
    }
    let max_threshold = u32::try_from(keys.keys.len()).unwrap_or(u32::MAX);
    if keys.threshold < 1 || keys.threshold > max_threshold {
        anyhow::bail!(
            "the proposal has signing threshold {t}, which must be between 1 and \
             {max_threshold} (the number of party signing keys)",
            t = keys.threshold
        );
    }
    Ok(keys)
}

/// Approve hosting `party_id` on this node: sign the pending onboarding
/// proposal whose hash is `transaction_hash`, then submit it.
///
/// A party this node already hosts answers [`Approval::AlreadyHosted`], so a
/// retry is safe.
///
/// # Errors
/// See [`ApproveHostingError`].
pub async fn approve_hosting(
    config: &NodeConfig,
    party_id: &str,
    transaction_hash: &[u8],
) -> Result<Approval, ApproveHostingError> {
    let party = CantonId::parse(party_id).map_err(|e| {
        ApproveHostingError::Invalid(anyhow::anyhow!("party_id is not a valid party id: {e}"))
    })?;
    if host_onboarding_status(config, party_id)
        .await
        .map_err(ApproveHostingError::Canton)?
        == HostOnboardingStatus::Hosted
    {
        return Ok(Approval::AlreadyHosted);
    }

    let synchronizer_id = utils::get_synchronizer_id(config)
        .await
        .map_err(ApproveHostingError::Canton)?;
    let namespace = party.namespace.to_hex();
    let this_participant = config.participant_id().to_string();
    let proposals = pending_proposals(config, &namespace)
        .await
        .map_err(ApproveHostingError::Canton)?;
    let head = topology::party_to_participant_head(config, &synchronizer_id, &party)
        .await
        .map_err(ApproveHostingError::Canton)?;
    let Some(proposal) = proposals
        .iter()
        .find(|proposal| topology_transaction_hash(&proposal.transaction) == transaction_hash)
    else {
        return Err(unmatched(
            party_id,
            &this_participant,
            &proposals,
            head.as_ref(),
        ));
    };

    let transaction = topology_version::decode_transaction(&proposal.transaction)
        .context("the proposal is not a TopologyTransaction")
        .map_err(ApproveHostingError::Invalid)?;
    let definition =
        topology::find_namespace_definition_at_head(config, &synchronizer_id, &namespace)
            .await
            .map_err(ApproveHostingError::Canton)?
            .map(|(_, definition)| definition);
    let p2p = check_hosting_proposal(
        &HostingContext {
            this_participant: &this_participant,
            party_id,
            namespace: definition.as_ref(),
            head: head.as_ref(),
        },
        &transaction,
    )?;
    let owners = definition
        .as_ref()
        .map(|definition| definition.owners.as_slice())
        .unwrap_or_default();
    let held = namespaces_held(config, owners)
        .await
        .map_err(ApproveHostingError::Canton)?;
    refuse_co_owned(party_id, &held)?;

    sign_and_submit(config, &synchronizer_id, proposal)
        .await
        .map_err(ApproveHostingError::Canton)?;
    tracing::info!(
        %party_id,
        owners = definition.as_ref().map_or(0, |d| d.owners.len()),
        namespace_threshold = definition.as_ref().map_or(0, |d| d.threshold),
        signing_threshold = p2p.party_signing_keys.as_ref().map_or(0, |k| k.threshold),
        signing_keys = p2p.party_signing_keys.as_ref().map_or(0, |k| k.keys.len()),
        confirmation_threshold = p2p.threshold,
        "decentralized-party hosting: approved on this host"
    );

    // The mapping goes in force once its last host signs. Until then this
    // host still reads it as a proposal.
    let hosted = match host_onboarding_status(config, party_id).await {
        Ok(status) => status == HostOnboardingStatus::Hosted,
        Err(e) => {
            tracing::warn!(
                %party_id,
                "decentralized-party hosting: post-submit status read failed: {e:#}"
            );
            false
        }
    };
    Ok(Approval::Approved { hosted })
}

/// Refuse a party when this node holds a key for one of its owner
/// namespaces. `held` lists those namespaces.
fn refuse_co_owned(party_id: &str, held: &[String]) -> Result<(), ApproveHostingError> {
    match held.first() {
        None => Ok(()),
        Some(owner) => Err(ApproveHostingError::Invalid(anyhow::anyhow!(
            "this node holds a key for {owner}, which owns the namespace of {party_id}; its \
             signature would also count as that owner's approval, so it does not approve \
             hosting as a covalidator"
        ))),
    }
}

/// Why no pending proposal has the requested hash.
///
/// A party already in force cannot be onboarded again. A pending proposal
/// that names this participant under another hash means the caller's proposal
/// was replaced, or the caller sent the wrong hash. Anything else means the
/// owners' submission has not reached this node yet.
fn unmatched(
    party_id: &str,
    this_participant: &str,
    proposals: &[SignedTopologyTransaction],
    head: Option<&TopologyTransaction>,
) -> ApproveHostingError {
    if let Some(head) = head {
        return ApproveHostingError::Conflict(format!(
            "{party_id} already has a PartyToParticipant in force at serial {serial}, and no \
             pending proposal has the requested hash",
            serial = head.serial
        ));
    }
    let names_this_participant = proposals
        .iter()
        .filter_map(|proposal| topology_version::decode_transaction(&proposal.transaction).ok())
        .any(|transaction| {
            matches!(
                transaction.mapping.and_then(|m| m.mapping),
                Some(topology_mapping::Mapping::PartyToParticipant(p2p))
                    if p2p.party == party_id
                        && p2p
                            .participants
                            .iter()
                            .any(|host| host.participant_uid == this_participant)
            )
        });
    if names_this_participant {
        ApproveHostingError::Conflict(format!(
            "a pending proposal for {party_id} names this participant, but none has the \
             requested hash; the proposal was replaced, or the hash is wrong"
        ))
    } else {
        ApproveHostingError::NotPending {
            party: party_id.to_string(),
        }
    }
}

/// Add this node's signature to `proposal` and submit it to the synchronizer
/// store. Canton merges it with the signatures already pending, and puts the
/// mapping in force once every authorizer has signed.
async fn sign_and_submit(
    config: &NodeConfig,
    synchronizer_id: &str,
    proposal: &SignedTopologyTransaction,
) -> anyhow::Result<()> {
    // An empty `signed_by` lets the node pick its own key, as `allocate_party`
    // does for a single-key party.
    let signed = topology::sign_transactions_with_topology_retry(
        config,
        SignTransactionsRequest {
            transactions: vec![proposal.clone()],
            signed_by: vec![],
            store: Some(topology::synchronizer_store_id(synchronizer_id)),
            force_flags: vec![],
        },
        "decentralized-party hosting approval",
    )
    .await?
    .transactions;
    let [signed] = <[SignedTopologyTransaction; 1]>::try_from(signed).map_err(|signed| {
        anyhow::anyhow!(
            "Canton returned {count} signed transactions for one proposal",
            count = signed.len()
        )
    })?;
    anyhow::ensure!(
        signed.transaction == proposal.transaction,
        "Canton signed a different transaction than the pending proposal"
    );

    let mut client = TopologyManagerWriteServiceClient::new(config.admin_channel().await?);
    client
        .add_transactions(tonic::Request::new(topology::add_transactions_request(
            synchronizer_id,
            signed,
            vec![],
        )))
        .await
        .context("AddTransactions RPC failed for the decentralized-party hosting approval")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::{
        crypto::v30::SigningPublicKey,
        protocol::v30::{
            TopologyMapping,
            enums::{ParticipantPermission, TopologyChangeOp},
            party_to_participant::{HostingParticipant, hosting_participant},
        },
        version::v1::{UntypedVersionedMessage, untyped_versioned_message},
    };
    use prost::Message as _;

    use super::*;
    use crate::workflow::external_party::steps::party_signing_key;

    fn participant(tag: u8) -> String {
        format!("participant-{tag}::1220{}", format!("{tag:02x}").repeat(32))
    }

    fn namespace() -> String {
        format!("1220{}", "dd".repeat(32))
    }

    fn party() -> String {
        format!("dapp-party::{}", namespace())
    }

    fn hosting(tag: u8, permission: ParticipantPermission) -> HostingParticipant {
        HostingParticipant {
            participant_uid: participant(tag),
            permission: permission as i32,
            onboarding: None,
        }
    }

    /// `count` distinct Ed25519 signing keys.
    fn signing_keys(count: u8) -> Vec<SigningPublicKey> {
        (1..=count).map(|i| party_signing_key(&[i; 32])).collect()
    }

    /// A party with `key_count` owner keys at `signing_threshold`, hosted on
    /// participants 1..=3 at confirmation threshold 2.
    fn mapping(key_count: u8, signing_threshold: u32) -> PartyToParticipant {
        PartyToParticipant {
            party: party(),
            threshold: 2,
            participants: vec![
                hosting(1, ParticipantPermission::Confirmation),
                hosting(2, ParticipantPermission::Confirmation),
                hosting(3, ParticipantPermission::Confirmation),
            ],
            party_signing_keys: Some(SigningKeysWithThreshold {
                keys: signing_keys(key_count),
                threshold: signing_threshold,
            }),
        }
    }

    fn transaction(mapping: topology_mapping::Mapping) -> TopologyTransaction {
        TopologyTransaction {
            operation: TopologyChangeOp::AddReplace as i32,
            serial: 1,
            mapping: Some(TopologyMapping {
                mapping: Some(mapping),
            }),
        }
    }

    fn p2p(mapping: PartyToParticipant) -> TopologyTransaction {
        transaction(topology_mapping::Mapping::PartyToParticipant(mapping))
    }

    fn definition() -> DecentralizedNamespaceDefinition {
        DecentralizedNamespaceDefinition {
            decentralized_namespace: namespace(),
            threshold: 2,
            owners: vec![
                format!("1220{}", "e1".repeat(32)),
                format!("1220{}", "e2".repeat(32)),
            ],
        }
    }

    /// Check `transaction` as participant 1, with the namespace definition in
    /// force and no mapping in force.
    fn check(transaction: &TopologyTransaction) -> Result<(), ApproveHostingError> {
        check_with(transaction, Some(&definition()), None)
    }

    fn check_with(
        transaction: &TopologyTransaction,
        namespace: Option<&DecentralizedNamespaceDefinition>,
        head: Option<&TopologyTransaction>,
    ) -> Result<(), ApproveHostingError> {
        let this_participant = participant(1);
        let party_id = party();
        check_hosting_proposal(
            &HostingContext {
                this_participant: &this_participant,
                party_id: &party_id,
                namespace,
                head,
            },
            transaction,
        )
        .map(|_| ())
    }

    /// The reason `transaction` is refused as invalid.
    fn refusal(transaction: &TopologyTransaction) -> String {
        match check(transaction) {
            Err(ApproveHostingError::Invalid(e)) => format!("{e:#}"),
            other => panic!("expected the proposal to be refused as invalid, got {other:?}"),
        }
    }

    #[test]
    fn accepts_2_3_and_10_signing_keys_at_every_threshold() {
        for (keys, thresholds) in [(2, &[1, 2][..]), (3, &[1, 2, 3]), (10, &[1, 5, 10])] {
            for &threshold in thresholds {
                if let Err(e) = check(&p2p(mapping(keys, threshold))) {
                    panic!("{keys} keys at threshold {threshold} must pass: {e}");
                }
            }
        }
    }

    #[test]
    fn refuses_an_operation_other_than_add_replace() {
        let mut removal = p2p(mapping(2, 2));
        removal.operation = TopologyChangeOp::Remove as i32;
        assert!(refusal(&removal).contains("ADD_REPLACE"));
    }

    /// Serial 1 creates the party. A higher serial would change one that exists.
    #[test]
    fn refuses_a_serial_other_than_one() {
        let mut later = p2p(mapping(2, 2));
        later.serial = 2;
        assert!(refusal(&later).contains("serial 1"));
    }

    #[test]
    fn refuses_a_mapping_that_is_not_party_to_participant() {
        let namespace = transaction(topology_mapping::Mapping::DecentralizedNamespaceDefinition(
            definition(),
        ));
        assert!(refusal(&namespace).contains("PartyToParticipant"));
    }

    #[test]
    fn refuses_a_proposal_for_another_party() {
        let mut other = mapping(2, 2);
        other.party = format!("other::{}", namespace());
        assert!(refusal(&p2p(other)).contains("the party the request names"));
    }

    /// Submission permission would let a host submit for the party, which
    /// signs its own transactions.
    #[test]
    fn refuses_submission_permission() {
        let mut submitting = mapping(2, 2);
        submitting.participants[1] = hosting(2, ParticipantPermission::Submission);
        assert!(refusal(&p2p(submitting)).contains("Confirmation"));
    }

    /// A marked host holds none of the party's contracts. A new party has no
    /// contracts to copy, so a marker has no place in its onboarding.
    #[test]
    fn refuses_an_onboarding_marker() {
        let mut marked = mapping(2, 2);
        marked.participants[2].onboarding = Some(hosting_participant::Onboarding {});
        assert!(refusal(&p2p(marked)).contains("onboarding details"));
    }

    #[test]
    fn refuses_a_host_listed_twice() {
        let mut twice = mapping(2, 1);
        twice.participants[2] = hosting(1, ParticipantPermission::Confirmation);
        assert!(refusal(&p2p(twice)).contains("twice"));
    }

    /// A host signs only a mapping that hosts it.
    #[test]
    fn refuses_a_proposal_that_does_not_name_this_participant() {
        let mut elsewhere = mapping(2, 2);
        elsewhere.participants[0] = hosting(4, ParticipantPermission::Confirmation);
        assert!(refusal(&p2p(elsewhere)).contains("does not name this participant"));
    }

    /// The cap of the existing routes: at least 1, and one less than the
    /// hosts, so a host can still leave.
    #[test]
    fn refuses_a_confirmation_threshold_out_of_range() {
        for threshold in [0, 3] {
            let mut out_of_range = mapping(2, 2);
            out_of_range.threshold = threshold;
            assert!(
                refusal(&p2p(out_of_range)).contains("confirmation threshold"),
                "confirmation threshold {threshold}"
            );
        }
    }

    #[test]
    fn refuses_a_party_without_signing_keys() {
        let mut none = mapping(2, 2);
        none.party_signing_keys = None;
        assert!(refusal(&p2p(none)).contains("no party signing keys"));

        let empty = mapping(0, 0);
        assert!(refusal(&p2p(empty)).contains("no party signing keys"));
    }

    /// One key listed twice would count twice toward the signing threshold.
    /// The usage does not make it another key.
    #[test]
    fn refuses_a_signing_key_listed_twice() {
        let mut twice = mapping(2, 2);
        if let Some(keys) = twice.party_signing_keys.as_mut() {
            let mut copy = keys.keys[0].clone();
            copy.usage.reverse();
            keys.keys[1] = copy;
        }
        assert!(refusal(&p2p(twice)).contains("twice"));
    }

    /// A key without Protocol usage cannot sign a transaction, so the party
    /// would exist and never transact.
    #[test]
    fn refuses_a_signing_key_without_protocol_usage() {
        let mut namespace_only = mapping(3, 2);
        if let Some(keys) = namespace_only.party_signing_keys.as_mut() {
            keys.keys[1].usage = vec![SigningKeyUsage::Namespace as i32];
        }
        let reason = refusal(&p2p(namespace_only));
        assert!(reason.contains("missing Protocol"), "{reason}");
    }

    #[test]
    fn refuses_a_signing_threshold_out_of_range() {
        for threshold in [0, 4] {
            assert!(
                refusal(&p2p(mapping(3, threshold))).contains("signing threshold"),
                "signing threshold {threshold}"
            );
        }
    }

    /// A party whose namespace is one key is a single-key external party,
    /// which `/v0/tenant/execute` serves.
    #[test]
    fn refuses_a_party_without_a_decentralized_namespace() {
        let Err(ApproveHostingError::Invalid(e)) = check_with(&p2p(mapping(2, 2)), None, None)
        else {
            panic!("a party without a namespace definition in force must be refused");
        };
        assert!(
            e.to_string().contains("DecentralizedNamespaceDefinition"),
            "{e}"
        );
    }

    /// Onboarding creates a party. Signing a second mapping for one that exists
    /// would change it.
    #[test]
    fn answers_conflict_when_another_mapping_is_in_force() {
        let proposal = p2p(mapping(2, 2));
        let mut other = mapping(2, 2);
        other.threshold = 1;
        let in_force = p2p(other);

        let result = check_with(&proposal, Some(&definition()), Some(&in_force));
        assert!(
            matches!(result, Err(ApproveHostingError::Conflict(_))),
            "{result:?}"
        );
        // The same mapping in force is no change, so it is no conflict.
        assert!(check_with(&proposal, Some(&definition()), Some(&proposal)).is_ok());
    }

    #[test]
    fn unmatched_names_the_reason() {
        let proposal = |mapping: PartyToParticipant| SignedTopologyTransaction {
            transaction: UntypedVersionedMessage {
                wrapper: Some(untyped_versioned_message::Wrapper::Data(
                    p2p(mapping).encode_to_vec(),
                )),
                version: 30,
            }
            .encode_to_vec(),
            proposal: true,
            ..Default::default()
        };
        let this_participant = participant(1);

        let in_force = p2p(mapping(2, 2));
        assert!(matches!(
            unmatched(&party(), &this_participant, &[], Some(&in_force)),
            ApproveHostingError::Conflict(_)
        ));

        let naming_this_host = [proposal(mapping(2, 2))];
        assert!(matches!(
            unmatched(&party(), &this_participant, &naming_this_host, None),
            ApproveHostingError::Conflict(_)
        ));

        let mut elsewhere = mapping(2, 2);
        elsewhere.participants[0] = hosting(4, ParticipantPermission::Confirmation);
        assert!(matches!(
            unmatched(&party(), &this_participant, &[proposal(elsewhere)], None),
            ApproveHostingError::NotPending { .. }
        ));
        assert!(matches!(
            unmatched(&party(), &this_participant, &[], None),
            ApproveHostingError::NotPending { .. }
        ));
    }
}
