//! Adding hosting participants to an **existing** external party.
//!
//! Onboarding ([`super::steps::prepare_topology`]) writes serial 1 and creates a
//! party. This module writes serial N+1 and changes one that already exists: it
//! reads the current `PartyToParticipant` from head state, keeps every current
//! host exactly as it is, and appends the new hosts at Confirmation with Canton's
//! `Onboarding` marker so the party stays suspended on them until their ACS
//! import lands.
//!
//! Two rules make this a read-modify-write rather than a rewrite, and both exist
//! because the wallet compares what every host prepared byte-for-byte before it
//! signs (see `onboard_co_validated` in the wallet crate):
//!
//! * The wallet pins the base serial in the request. Without it, two hosts that
//!   read head state a moment apart would build different transactions and the
//!   comparison would fail for a reason that is not an attack.
//! * The threshold does not move here. Not because it cannot: the decparty
//!   add-party flow writes a marked new member and a new threshold in one serial
//!   bump, and Canton takes it. The only hard rule is that the threshold must
//!   not exceed the hosts that can actually confirm, and a marked host cannot
//!   (`party_replication.proto` defines the flag being cleared as the point the
//!   party starts participating in transactions there). The known
//!   full-threshold bug is that rule broken from the other side.
//!
//!   Splitting it is about rollback rather than safety: if the raise lands with
//!   the add and the ACS replication then fails, the party sits at a threshold
//!   its live hosts may not meet until someone does another bump. Kept separate,
//!   raising is a cheap last step once the marker has cleared.

use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{
        PartyToParticipant, TopologyMapping, TopologyTransaction,
        enums::{ParticipantPermission, TopologyChangeOp},
        party_to_participant::{HostingParticipant, hosting_participant},
        topology_mapping,
    },
    topology::admin::v30::{
        GenerateTransactionsRequest, ListPartyToParticipantRequest, generate_transactions_request,
        list_party_to_participant_response::result::Item as P2pItem,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
    version::v1::{UntypedVersionedMessage, untyped_versioned_message},
};
use prost::Message as _;
use serde::{Deserialize, Serialize};

use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    utils,
    workflow::{external_party::steps::party_query, topology},
};

/// The unsigned add-hosts topology, plus the hash the party must sign for each
/// transaction. The two vectors are index-aligned.
///
/// Deliberately not [`super::steps::PreparedTopology`]: that carries the party's
/// public-key fingerprint because onboarding derives the party id from a key the
/// wallet just supplied. Here the party already exists and no key is supplied, so
/// there is nothing to report.
pub struct PreparedAddHosts {
    /// The party gaining hosts.
    pub party_id: String,
    /// The serial the submitted transactions carry (`base_serial + 1`).
    pub serial: u32,
    /// Canton's hash for each transaction, for the party to sign.
    pub transaction_hashes: Vec<Vec<u8>>,
    /// The serialized (versioned) topology transactions to submit.
    pub topology_transactions: Vec<Vec<u8>>,
}

/// The party's current authorized `PartyToParticipant`, with the serial it sits
/// at. The serial is what the next write increments, and what the wallet pins so
/// every host builds the same bytes.
#[derive(Clone, Debug)]
pub struct CurrentPartyTopology {
    /// Serial of the authorized mapping in head state.
    pub serial: u32,
    /// The mapping itself.
    pub mapping: PartyToParticipant,
}

/// Read `party_id`'s authorized `PartyToParticipant` from this node's head state.
///
/// Returns `None` when no authorized mapping exists — the party is unknown to
/// this node, or exists only as an unsigned proposal.
///
/// # Errors
/// Returns an error if the synchronizer id cannot be resolved, the
/// `ListPartyToParticipant` RPC fails, or Canton reports a negative serial.
pub async fn read_party_to_participant(
    config: &NodeConfig,
    party_id: &str,
) -> Result<Option<CurrentPartyTopology>> {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let mut client = TopologyManagerReadServiceClient::new(config.admin_channel().await?)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let response = client
        .list_party_to_participant(tonic::Request::new(ListPartyToParticipantRequest {
            base_query: Some(party_query(&synchronizer_id, false)),
            filter_party: party_id.to_string(),
            filter_participant: String::new(),
        }))
        .await
        .context("ListPartyToParticipant RPC failed")?
        .into_inner();

    for result in response.results {
        let serial = result
            .context
            .as_ref()
            .map(|c| c.serial)
            .unwrap_or_default();
        let Some(P2pItem::V30(mapping)) = result.item else {
            continue;
        };
        // `filter_party` is a prefix filter on some Canton versions, so confirm the
        // exact party rather than trusting the server-side filter.
        if mapping.party != party_id {
            continue;
        }
        let serial = u32::try_from(serial).with_context(|| {
            format!("Canton reported serial {serial} for {party_id}, which is not a valid serial")
        })?;
        return Ok(Some(CurrentPartyTopology { serial, mapping }));
    }
    Ok(None)
}

/// Build the serial-N+1 `PartyToParticipant` that adds `new_hosts` to `current`.
///
/// Current hosts are carried over untouched, new hosts land at Confirmation with
/// the `Onboarding` marker, and the threshold is left alone. The participant list
/// is sorted so every host produces byte-identical output for the same input.
///
/// # Errors
/// Returns an error if `new_hosts` is empty, contains a duplicate, or names a
/// participant that already hosts the party.
fn add_hosts_mapping(
    current: &PartyToParticipant,
    new_hosts: &[CantonId],
) -> Result<PartyToParticipant> {
    if new_hosts.is_empty() {
        anyhow::bail!("add-hosts named no new hosting participant");
    }

    let mut participants = current.participants.clone();
    for host in new_hosts {
        let uid = host.to_string();
        if participants.iter().any(|p| p.participant_uid == uid) {
            anyhow::bail!(
                "{uid} already hosts {party}; add-hosts only adds hosts",
                party = current.party
            );
        }
        participants.push(HostingParticipant {
            participant_uid: uid,
            // Confirmation, never Submission: a party whose threshold can rise
            // above 1 may not have a Submission host, and an external party signs
            // its own submissions anyway.
            permission: ParticipantPermission::Confirmation as i32,
            // The marker is what keeps the party suspended on the new host until
            // its ACS import lands. Without it the host would be expected to
            // confirm transactions it has no contracts for.
            onboarding: Some(hosting_participant::Onboarding {}),
        });
    }

    // Same canonical order onboarding uses, and for the same reason: the wallet
    // compares the hosts' bytes against each other, so list order must not depend
    // on which host built the transaction.
    participants.sort_by(|a, b| a.participant_uid.cmp(&b.participant_uid));

    Ok(PartyToParticipant {
        party: current.party.clone(),
        // Unchanged. The new hosts do not count toward it until their markers
        // clear, so raising it here could leave the party below its own threshold.
        threshold: current.threshold,
        participants,
        // Unchanged. This step adds hosts, never keys.
        party_signing_keys: current.party_signing_keys.clone(),
    })
}

/// Ask Canton to build the add-hosts topology and the hash the party must sign.
///
/// `base_serial` is the serial the wallet expects the party to sit at. It is
/// checked against head state rather than trusted: a host whose view has moved on
/// must fail loudly instead of quietly preparing a different transaction from its
/// peers.
///
/// # Errors
/// Returns an error if the party has no authorized mapping on this node, its
/// serial differs from `base_serial`, the host set is invalid, or
/// `GenerateTransactions` fails.
pub async fn prepare_add_hosts(
    config: &NodeConfig,
    party_id: &str,
    new_hosts: &[CantonId],
    base_serial: u32,
) -> Result<PreparedAddHosts> {
    let synchronizer_id = utils::get_synchronizer_id(config).await?;

    let Some(current) = read_party_to_participant(config, party_id).await? else {
        anyhow::bail!(
            "{party_id} has no authorized PartyToParticipant on this node; it cannot gain a host \
             here"
        );
    };
    if current.serial != base_serial {
        anyhow::bail!(
            "{party_id} is at serial {found} on this node, not the {base_serial} the request \
             pinned; re-read the party and retry",
            found = current.serial
        );
    }

    let mapping = add_hosts_mapping(&current.mapping, new_hosts)?;
    let next_serial = current.serial.checked_add(1).with_context(|| {
        format!(
            "{party_id} is at serial {s}, which cannot be advanced",
            s = current.serial
        )
    })?;

    let mut client = TopologyManagerWriteServiceClient::new(config.admin_channel().await?);
    let response = client
        .generate_transactions(tonic::Request::new(GenerateTransactionsRequest {
            proposals: vec![generate_transactions_request::Proposal {
                operation: TopologyChangeOp::AddReplace as i32,
                serial: next_serial,
                mapping: Some(generate_transactions_request::proposal::Mapping::V30(
                    TopologyMapping {
                        mapping: Some(topology_mapping::Mapping::PartyToParticipant(mapping)),
                    },
                )),
                store: Some(topology::synchronizer_store_id(&synchronizer_id)),
            }],
            base_request: None,
        }))
        .await
        .context("GenerateTransactions RPC failed")?
        .into_inner();

    if response.generated_transactions.is_empty() {
        anyhow::bail!("GenerateTransactions returned no transactions for {party_id}");
    }

    tracing::info!(
        %party_id,
        base_serial,
        next_serial,
        new_hosts = new_hosts.len(),
        "external-party: generated add-hosts topology"
    );

    let mut transaction_hashes = Vec::with_capacity(response.generated_transactions.len());
    let mut topology_transactions = Vec::with_capacity(response.generated_transactions.len());
    for tx in response.generated_transactions {
        transaction_hashes.push(tx.transaction_hash);
        topology_transactions.push(tx.serialized_transaction);
    }

    Ok(PreparedAddHosts {
        party_id: party_id.to_string(),
        serial: next_serial,
        transaction_hashes,
        topology_transactions,
    })
}

/// The party-signed add-hosts bundle a wallet submits to each new host.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExternalPartyAddHostsPayload {
    /// The party gaining hosts.
    pub party_id: String,
    /// The serial the wallet read before it prepared. The submitted transaction
    /// must be exactly one past this.
    pub base_serial: u32,
    /// The serialized (versioned) topology transactions to submit.
    pub topology_transactions: Vec<Vec<u8>>,
    /// The party's signature per transaction, index-aligned with the above.
    pub signatures: Vec<Vec<u8>>,
    /// Fingerprint of the key that produced those signatures.
    pub signed_by: String,
}

/// Reject any submitted add-hosts topology that is not exactly this change.
///
/// This exists for the same reason [`super::steps`]'s onboarding validator does:
/// the host co-signs the caller's bytes with its own topology key and submits
/// them, so unvalidated bytes are this node's key applied to a topology change
/// its operator never saw. Here the stakes are higher than at onboarding — the
/// party already exists and already holds contracts, so a forged serial N+1 could
/// evict its current hosts or drop its threshold rather than merely create
/// something unwanted.
///
/// `current` is this node's own head-state read, never anything the caller sent.
/// Every field is checked against it.
///
/// # Errors
/// Returns an error naming the first field that does not match.
pub fn validate_add_hosts_topology(
    config: &NodeConfig,
    current: &CurrentPartyTopology,
    bundle: &ExternalPartyAddHostsPayload,
) -> Result<()> {
    if bundle.topology_transactions.is_empty() {
        anyhow::bail!("external-party add-hosts submitted no topology transactions");
    }
    if bundle.signatures.len() != bundle.topology_transactions.len() {
        anyhow::bail!(
            "external-party add-hosts submitted {t} transaction(s) and {s} signature(s); they \
             must be index-aligned",
            t = bundle.topology_transactions.len(),
            s = bundle.signatures.len()
        );
    }
    if bundle.party_id != current.mapping.party {
        anyhow::bail!(
            "add-hosts names party {found}, but this node read {expected} from head state",
            found = bundle.party_id,
            expected = current.mapping.party
        );
    }
    if bundle.base_serial != current.serial {
        anyhow::bail!(
            "add-hosts pinned base serial {pinned}, but this node reads {found}; the party moved \
             under the request",
            pinned = bundle.base_serial,
            found = current.serial
        );
    }
    let next_serial = current.serial.checked_add(1).with_context(|| {
        format!(
            "{party} is at serial {s}, which cannot be advanced",
            party = current.mapping.party,
            s = current.serial
        )
    })?;

    let this_participant = config.participant_id().to_string();

    for (index, serialized) in bundle.topology_transactions.iter().enumerate() {
        let versioned =
            UntypedVersionedMessage::decode(serialized.as_slice()).with_context(|| {
                format!("topology transaction {index} is not a versioned Canton message")
            })?;
        let Some(untyped_versioned_message::Wrapper::Data(inner)) = versioned.wrapper else {
            anyhow::bail!("topology transaction {index} carries no payload");
        };
        let transaction = TopologyTransaction::decode(inner.as_slice()).with_context(|| {
            format!("topology transaction {index} is not a TopologyTransaction")
        })?;

        if transaction.operation != TopologyChangeOp::AddReplace as i32 {
            anyhow::bail!(
                "topology transaction {index} must be an ADD_REPLACE, got operation {op}",
                op = transaction.operation
            );
        }
        if transaction.serial != next_serial {
            anyhow::bail!(
                "topology transaction {index} has serial {serial}, which must be exactly one past \
                 the current {current_serial}",
                serial = transaction.serial,
                current_serial = current.serial
            );
        }

        let Some(TopologyMapping {
            mapping: Some(topology_mapping::Mapping::PartyToParticipant(p2p)),
        }) = transaction.mapping
        else {
            anyhow::bail!(
                "topology transaction {index} must carry a PartyToParticipant mapping; this \
                 endpoint adds hosts and authorizes nothing else"
            );
        };

        if p2p.party != current.mapping.party {
            anyhow::bail!(
                "topology transaction {index} is for party {found}, not {expected}",
                found = p2p.party,
                expected = current.mapping.party
            );
        }
        // The threshold is a separate serial bump. Letting it move here would let a
        // caller drop a party to threshold 1 while adding a host it controls.
        if p2p.threshold != current.mapping.threshold {
            anyhow::bail!(
                "topology transaction {index} changes the confirmation threshold from {current_t} \
                 to {found}; adding hosts must leave it alone",
                current_t = current.mapping.threshold,
                found = p2p.threshold
            );
        }
        // Adding a key would hand its holder the party. Only the key already in
        // head state may appear.
        if p2p.party_signing_keys != current.mapping.party_signing_keys {
            anyhow::bail!(
                "topology transaction {index} changes the party's signing keys; adding hosts must \
                 leave them alone"
            );
        }

        let mut seen: Vec<&str> = p2p
            .participants
            .iter()
            .map(|p| p.participant_uid.as_str())
            .collect();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        if seen.len() != count {
            anyhow::bail!("topology transaction {index} lists a hosting participant twice");
        }

        let mut added = 0usize;
        for participant in &p2p.participants {
            match current
                .mapping
                .participants
                .iter()
                .find(|p| p.participant_uid == participant.participant_uid)
            {
                // A current host must survive untouched. Anything else is an
                // eviction or a permission change wearing an add-hosts costume.
                Some(existing) => {
                    if participant != existing {
                        anyhow::bail!(
                            "topology transaction {index} alters current host {uid}; adding hosts \
                             must leave existing hosts exactly as they are",
                            uid = participant.participant_uid
                        );
                    }
                }
                None => {
                    added += 1;
                    if participant.permission != ParticipantPermission::Confirmation as i32 {
                        anyhow::bail!(
                            "topology transaction {index} gives new host {uid} permission {perm}; \
                             a new host must be Confirmation",
                            uid = participant.participant_uid,
                            perm = participant.permission
                        );
                    }
                    // Without the marker the party goes live on a host that holds
                    // none of its contracts, and that host starts confirming
                    // transactions it cannot validate.
                    if participant.onboarding.is_none() {
                        anyhow::bail!(
                            "topology transaction {index} adds host {uid} without the onboarding \
                             marker; it would go live before its ACS import",
                            uid = participant.participant_uid
                        );
                    }
                }
            }
        }

        // Every current host must still be there. The loop above proves each
        // *listed* current host is unchanged, not that none went missing.
        for existing in &current.mapping.participants {
            if !p2p
                .participants
                .iter()
                .any(|p| p.participant_uid == existing.participant_uid)
            {
                anyhow::bail!(
                    "topology transaction {index} drops current host {uid}; adding hosts never \
                     removes one",
                    uid = existing.participant_uid
                );
            }
        }
        if added == 0 {
            anyhow::bail!("topology transaction {index} adds no host");
        }
        if !p2p
            .participants
            .iter()
            .any(|p| p.participant_uid == this_participant)
        {
            anyhow::bail!(
                "topology transaction {index} does not name this participant \
                 ({this_participant}); a host only authorizes topology that hosts it"
            );
        }

        // The checks above are field-by-field, so a field Canton adds later would
        // ride along unexamined. This refuses anything that is not exactly the
        // mapping they proved.
        let expected = PartyToParticipant {
            party: current.mapping.party.clone(),
            threshold: current.mapping.threshold,
            participants: p2p.participants.clone(),
            party_signing_keys: current.mapping.party_signing_keys.clone(),
        };
        if p2p != expected {
            anyhow::bail!(
                "topology transaction {index} carries fields beyond a plain add-hosts \
                 PartyToParticipant and will not be authorized"
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::crypto::v30::{
        CryptoKeyFormat, SigningKeySpec, SigningKeyUsage, SigningKeysWithThreshold,
        SigningPublicKey,
    };

    use super::*;

    fn participant(tag: u8) -> CantonId {
        let namespace = format!("1220{}", format!("{tag:02x}").repeat(32));
        match CantonId::parse(&format!("participant-{tag}::{namespace}")) {
            Ok(id) => id,
            Err(e) => panic!("test participant id must parse: {e}"),
        }
    }

    fn test_config(tag: u8) -> NodeConfig {
        let mut config = NodeConfig::default();
        config.node.participant_id = Some(participant(tag));
        config
    }

    fn party_keys() -> Option<SigningKeysWithThreshold> {
        Some(SigningKeysWithThreshold {
            keys: vec![SigningPublicKey {
                format: CryptoKeyFormat::DerX509SubjectPublicKeyInfo as i32,
                public_key: vec![7u8; 44],
                key_spec: SigningKeySpec::EcCurve25519 as i32,
                usage: vec![
                    SigningKeyUsage::Namespace as i32,
                    SigningKeyUsage::Protocol as i32,
                ],
                ..Default::default()
            }],
            threshold: 1,
        })
    }

    fn host(tag: u8, permission: ParticipantPermission, onboarding: bool) -> HostingParticipant {
        HostingParticipant {
            participant_uid: participant(tag).to_string(),
            permission: permission as i32,
            onboarding: onboarding.then_some(hosting_participant::Onboarding {}),
        }
    }

    /// A party at serial 4, hosted on participants 1 and 2 at threshold 1.
    fn current() -> CurrentPartyTopology {
        CurrentPartyTopology {
            serial: 4,
            mapping: PartyToParticipant {
                party: "alice::1220aa".to_string(),
                threshold: 1,
                participants: vec![
                    host(1, ParticipantPermission::Confirmation, false),
                    host(2, ParticipantPermission::Confirmation, false),
                ],
                party_signing_keys: party_keys(),
            },
        }
    }

    fn serialize(mapping: PartyToParticipant, serial: u32) -> Vec<u8> {
        let transaction = TopologyTransaction {
            operation: TopologyChangeOp::AddReplace as i32,
            serial,
            mapping: Some(TopologyMapping {
                mapping: Some(topology_mapping::Mapping::PartyToParticipant(mapping)),
            }),
        };
        UntypedVersionedMessage {
            wrapper: Some(untyped_versioned_message::Wrapper::Data(
                transaction.encode_to_vec(),
            )),
            version: 30,
        }
        .encode_to_vec()
    }

    fn bundle_of(mapping: PartyToParticipant, serial: u32) -> ExternalPartyAddHostsPayload {
        ExternalPartyAddHostsPayload {
            party_id: "alice::1220aa".to_string(),
            base_serial: 4,
            topology_transactions: vec![serialize(mapping, serial)],
            signatures: vec![vec![0u8; 64]],
            signed_by: "1220aa".to_string(),
        }
    }

    /// Validate as participant 3 — the host being added.
    fn validate(mapping: PartyToParticipant) -> Result<()> {
        validate_add_hosts_topology(&test_config(3), &current(), &bundle_of(mapping, 5))
    }

    fn built() -> PartyToParticipant {
        match add_hosts_mapping(&current().mapping, &[participant(3)]) {
            Ok(m) => m,
            Err(e) => panic!("building the add-hosts mapping must succeed: {e}"),
        }
    }

    // ------------------------------------------------------------------
    // The builder
    // ------------------------------------------------------------------

    /// The marker is the whole point: without it the new host goes live holding
    /// none of the party's contracts.
    #[test]
    fn new_hosts_land_at_confirmation_carrying_the_marker() {
        let mapping = built();
        let Some(added) = mapping
            .participants
            .iter()
            .find(|p| p.participant_uid == participant(3).to_string())
        else {
            panic!("the new host must appear in the mapping");
        };
        assert_eq!(added.permission, ParticipantPermission::Confirmation as i32);
        assert!(added.onboarding.is_some(), "the new host must be marked");
    }

    /// The wallet compares every host's bytes, so list order must not depend on
    /// which host built the transaction.
    #[test]
    fn participants_are_sorted_so_every_host_builds_the_same_bytes() {
        let mapping = match add_hosts_mapping(&current().mapping, &[participant(9), participant(3)])
        {
            Ok(m) => m,
            Err(e) => panic!("building must succeed: {e}"),
        };
        let uids: Vec<&str> = mapping
            .participants
            .iter()
            .map(|p| p.participant_uid.as_str())
            .collect();
        let mut sorted = uids.clone();
        sorted.sort_unstable();
        assert_eq!(uids, sorted, "participants must be in canonical order");
    }

    /// Current hosts survive untouched, and the threshold does not move — raising
    /// it is a separate serial bump.
    #[test]
    fn current_hosts_and_threshold_are_untouched() {
        let mapping = built();
        assert_eq!(mapping.threshold, current().mapping.threshold);
        assert_eq!(
            mapping.party_signing_keys,
            current().mapping.party_signing_keys
        );
        for existing in &current().mapping.participants {
            assert!(
                mapping.participants.contains(existing),
                "current host {uid} must survive byte-identical",
                uid = existing.participant_uid
            );
        }
    }

    #[test]
    fn builder_refuses_a_host_that_already_hosts_the_party() {
        let Err(e) = add_hosts_mapping(&current().mapping, &[participant(2)]) else {
            panic!("adding an existing host must be refused");
        };
        assert!(e.to_string().contains("already hosts"), "{e}");
    }

    #[test]
    fn builder_refuses_an_empty_host_list() {
        let Err(e) = add_hosts_mapping(&current().mapping, &[]) else {
            panic!("adding no host must be refused");
        };
        assert!(e.to_string().contains("no new hosting participant"), "{e}");
    }

    // ------------------------------------------------------------------
    // The validator
    //
    // Each case below is something a tenant API key holder could otherwise talk
    // this node into co-signing with its own topology key, against a party that
    // already exists and already holds contracts.
    // ------------------------------------------------------------------

    /// The baseline: what the builder produces must validate, or the flow cannot
    /// work at all.
    #[test]
    fn accepts_what_the_builder_produces() {
        if let Err(e) = validate(built()) {
            panic!("the topology this node generates must validate: {e}");
        }
    }

    #[test]
    fn rejects_a_serial_that_is_not_one_past_the_current() {
        for serial in [4u32, 6, 1] {
            let bundle = bundle_of(built(), serial);
            let Err(e) = validate_add_hosts_topology(&test_config(3), &current(), &bundle) else {
                panic!("serial {serial} must be refused");
            };
            assert!(e.to_string().contains("serial"), "{e}");
        }
    }

    /// A pinned base serial that disagrees with head state means the party moved
    /// under the request. Preparing anyway would give the wallet two different
    /// transactions from two hosts.
    #[test]
    fn rejects_a_base_serial_that_does_not_match_head_state() {
        let mut bundle = bundle_of(built(), 5);
        bundle.base_serial = 3;
        let Err(e) = validate_add_hosts_topology(&test_config(3), &current(), &bundle) else {
            panic!("a stale base serial must be refused");
        };
        assert!(e.to_string().contains("base serial"), "{e}");
    }

    /// The eviction case: a caller rewrites the host set to drop the hosts it does
    /// not control.
    #[test]
    fn rejects_a_dropped_current_host() {
        let mut mapping = built();
        mapping
            .participants
            .retain(|p| p.participant_uid != participant(2).to_string());
        let Err(e) = validate(mapping) else {
            panic!("dropping a current host must be refused");
        };
        assert!(e.to_string().contains("drops current host"), "{e}");
    }

    /// The demotion case: current hosts stay listed but lose their say.
    #[test]
    fn rejects_an_altered_current_host() {
        let mut mapping = built();
        for entry in &mut mapping.participants {
            if entry.participant_uid == participant(2).to_string() {
                entry.permission = ParticipantPermission::Observation as i32;
            }
        }
        let Err(e) = validate(mapping) else {
            panic!("altering a current host must be refused");
        };
        assert!(e.to_string().contains("alters current host"), "{e}");
    }

    #[test]
    fn rejects_a_new_host_without_the_onboarding_marker() {
        let mut mapping = built();
        for entry in &mut mapping.participants {
            if entry.participant_uid == participant(3).to_string() {
                entry.onboarding = None;
            }
        }
        let Err(e) = validate(mapping) else {
            panic!("an unmarked new host must be refused");
        };
        assert!(e.to_string().contains("onboarding marker"), "{e}");
    }

    /// Threshold > 1 forbids a Submission host, and an external party signs its
    /// own submissions, so Submission is never right here.
    #[test]
    fn rejects_a_new_host_that_is_not_confirmation() {
        for permission in [
            ParticipantPermission::Submission,
            ParticipantPermission::Observation,
        ] {
            let mut mapping = built();
            for entry in &mut mapping.participants {
                if entry.participant_uid == participant(3).to_string() {
                    entry.permission = permission as i32;
                }
            }
            let Err(e) = validate(mapping) else {
                panic!("a new host at {permission:?} must be refused");
            };
            assert!(e.to_string().contains("must be Confirmation"), "{e}");
        }
    }

    /// Bundling a threshold change with an add is how a caller would drop a party
    /// to threshold 1 while adding a host it controls.
    #[test]
    fn rejects_a_threshold_change() {
        let mut mapping = built();
        mapping.threshold = 2;
        let Err(e) = validate(mapping) else {
            panic!("a threshold change must be refused");
        };
        assert!(e.to_string().contains("threshold"), "{e}");
    }

    /// An extra signing key hands its holder the party.
    #[test]
    fn rejects_a_signing_key_change() {
        let mut mapping = built();
        if let Some(keys) = &mut mapping.party_signing_keys {
            keys.keys.push(SigningPublicKey {
                format: CryptoKeyFormat::DerX509SubjectPublicKeyInfo as i32,
                public_key: vec![9u8; 44],
                key_spec: SigningKeySpec::EcCurve25519 as i32,
                usage: vec![SigningKeyUsage::Protocol as i32],
                ..Default::default()
            });
        }
        let Err(e) = validate(mapping) else {
            panic!("a signing-key change must be refused");
        };
        assert!(e.to_string().contains("signing keys"), "{e}");
    }

    #[test]
    fn rejects_a_mapping_that_does_not_host_this_node() {
        // Validate as participant 8, which the transaction never names.
        let bundle = bundle_of(built(), 5);
        let Err(e) = validate_add_hosts_topology(&test_config(8), &current(), &bundle) else {
            panic!("a host must refuse topology that does not host it");
        };
        assert!(
            e.to_string().contains("does not name this participant"),
            "{e}"
        );
    }

    #[test]
    fn rejects_a_duplicate_hosting_participant() {
        let mut mapping = built();
        mapping
            .participants
            .push(host(3, ParticipantPermission::Confirmation, true));
        let Err(e) = validate(mapping) else {
            panic!("a duplicate host must be refused");
        };
        assert!(e.to_string().contains("twice"), "{e}");
    }

    /// A serial bump that adds nobody is a rewrite of the current mapping wearing
    /// an add-hosts costume.
    #[test]
    fn rejects_a_transaction_that_adds_no_host() {
        let Err(e) = validate(current().mapping) else {
            panic!("adding no host must be refused");
        };
        assert!(e.to_string().contains("adds no host"), "{e}");
    }

    #[test]
    fn rejects_a_mapping_for_another_party() {
        let mut mapping = built();
        mapping.party = "mallory::1220bb".to_string();
        let Err(e) = validate(mapping) else {
            panic!("another party's mapping must be refused");
        };
        assert!(e.to_string().contains("is for party"), "{e}");
    }

    #[test]
    fn rejects_signatures_that_are_not_index_aligned() {
        let mut bundle = bundle_of(built(), 5);
        bundle.signatures.clear();
        let Err(e) = validate_add_hosts_topology(&test_config(3), &current(), &bundle) else {
            panic!("a signature count mismatch must be refused");
        };
        assert!(e.to_string().contains("index-aligned"), "{e}");
    }

    #[test]
    fn rejects_an_empty_bundle() {
        let mut bundle = bundle_of(built(), 5);
        bundle.topology_transactions.clear();
        bundle.signatures.clear();
        let Err(e) = validate_add_hosts_topology(&test_config(3), &current(), &bundle) else {
            panic!("an empty bundle must be refused");
        };
        assert!(e.to_string().contains("no topology transactions"), "{e}");
    }
}
