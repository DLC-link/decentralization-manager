use anyhow::Context;
use canton_proto_rs::com::digitalasset::canton::protocol::v30::{
    PartyToParticipant, TopologyMapping,
    enums::{ParticipantPermission, TopologyChangeOp},
    topology_mapping,
};
use serde::{Deserialize, Serialize};

use crate::{
    config::NodeConfig,
    error::Result,
    workflow::{
        external_party::keys::PartySignatureScheme,
        topology_version::{self},
    },
};

use super::head::CurrentPartyTopology;

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
/// This exists for the same reason [`super::super::steps`]'s onboarding validator does:
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
    // The party's namespace is the fingerprint half of its id, and Canton will
    // only accept a signature attributed to it. A wrong `signed_by` otherwise
    // fails inside SignTransactions and comes back as a 500, telling the wallet
    // this host is unhealthy when the request was simply wrong.
    let namespace = bundle
        .party_id
        .rsplit_once("::")
        .map(|(_, ns)| ns)
        .unwrap_or_default();
    if bundle.signed_by != namespace {
        anyhow::bail!(
            "signed_by {signed_by} is not {party}'s namespace ({namespace}); only the party's \
             own key can authorize its topology",
            signed_by = bundle.signed_by,
            party = bundle.party_id
        );
    }
    // Base64 decoding accepts any length, so without this an empty or
    // truncated signature passes validation and fails inside a Canton RPC
    // instead, surfacing as a 500 for what is plainly malformed input. The
    // forms depend on the party key's type, read off the mapping.
    let scheme = PartySignatureScheme::for_party(&current.mapping, &bundle.signed_by);
    for (index, signature) in bundle.signatures.iter().enumerate() {
        if !scheme.accepts(signature) {
            anyhow::bail!(
                "signature {index} is {len} byte(s); {expected}",
                len = signature.len(),
                expected = scheme.expected_form()
            );
        }
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
        let transaction = topology_version::decode_transaction(serialized).with_context(|| {
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
                    // Confirmation always; Submission only while the party needs
                    // one confirmation, which is Canton's constraint rather than
                    // ours. Observation is never useful here: it confirms
                    // nothing and cannot submit, so it would add a host that
                    // does neither.
                    let confirming =
                        participant.permission == ParticipantPermission::Confirmation as i32;
                    let submitting =
                        participant.permission == ParticipantPermission::Submission as i32;
                    if !(confirming || (submitting && p2p.threshold == 1)) {
                        anyhow::bail!(
                            "topology transaction {index} gives new host {uid} permission {perm}; \
                             a new host must be Confirmation, or Submission while the party's \
                             threshold is 1",
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
        // The invariant an add must never break: hosts still carrying the
        // onboarding marker cannot confirm, so they do not count toward the
        // threshold. If the unmarked hosts no longer reach it, the party stops
        // being able to transact the moment this lands — a self-inflicted
        // outage dressed as an expansion.
        // Unmarked, and able to act for the party. Confirmation counts, and so
        // does Submission — which only exists at threshold 1 and is precisely a
        // host that can act alone. Observation counts for neither: it is listed
        // and live but confirms nothing and cannot submit, so counting it would
        // let an add through that leaves the party unable to transact.
        let active = p2p
            .participants
            .iter()
            .filter(|p| {
                p.onboarding.is_none()
                    && (p.permission == ParticipantPermission::Confirmation as i32
                        || p.permission == ParticipantPermission::Submission as i32)
            })
            .count();
        if (active as u32) < p2p.threshold {
            anyhow::bail!(
                "topology transaction {index} would leave {active} host(s) able to act for the \
                 party, \
                 below the party's threshold of {threshold}; the party could not transact",
                threshold = p2p.threshold
            );
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
    use common::api::HostPermission;

    use super::*;
    use crate::workflow::external_party::add_hosts::{mapping::add_hosts_mapping, test_support::*};

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
    fn rejects_a_new_host_that_can_neither_confirm_nor_submit() {
        // Observation is refused outright: it confirms nothing and cannot
        // submit, so it adds a host that does neither. Submission is refused
        // here specifically because `current()` sits at threshold 2 for this
        // case — at threshold 1 it is the failover offer and allowed.
        let mut current = current();
        current.mapping.threshold = 2;
        for permission in [
            ParticipantPermission::Submission,
            ParticipantPermission::Observation,
        ] {
            let mut mapping = built();
            mapping.threshold = 2;
            for entry in &mut mapping.participants {
                if entry.participant_uid == participant(3).to_string() {
                    entry.permission = permission as i32;
                }
            }
            let Err(e) =
                validate_add_hosts_topology(&test_config(3), &current, &bundle_of(mapping, 5))
            else {
                panic!("a new host at {permission:?} must be refused");
            };
            assert!(
                e.to_string().contains("must be Confirmation"),
                "{permission:?}: {e}"
            );
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

    /// The outage this guards: marked hosts cannot confirm, so an add that
    /// leaves too few unmarked hosts stops the party transacting the moment it
    /// lands.
    #[test]
    fn rejects_an_add_that_drops_active_hosts_below_the_threshold() {
        // A party at threshold 2 whose current hosts are both already marked —
        // adding another marked host leaves nobody able to confirm.
        let current = CurrentPartyTopology {
            serial: 4,
            mapping: PartyToParticipant {
                party: "alice::1220aa".to_string(),
                threshold: 2,
                participants: vec![
                    host(1, ParticipantPermission::Confirmation, true),
                    host(2, ParticipantPermission::Confirmation, true),
                ],
                party_signing_keys: party_keys(),
            },
        };
        let Ok(mapping) = add_hosts_mapping(
            &current.mapping,
            &[participant(3)],
            HostPermission::Confirmation,
        ) else {
            panic!("building must succeed");
        };
        let bundle = bundle_of(mapping, 5);
        let Err(e) = validate_add_hosts_topology(&test_config(3), &current, &bundle) else {
            panic!("an add that strands the party below its threshold must be refused");
        };
        assert!(e.to_string().contains("below the party's threshold"), "{e}");
    }

    /// An Observation host is listed and unmarked but confirms nothing, so it
    /// must not count toward the threshold either. Counting it would wave
    /// through an add that still leaves the party short of confirmations.
    #[test]
    fn an_observation_host_does_not_count_as_active() {
        let current = CurrentPartyTopology {
            serial: 4,
            mapping: PartyToParticipant {
                party: "alice::1220aa".to_string(),
                threshold: 2,
                participants: vec![
                    host(1, ParticipantPermission::Confirmation, false),
                    host(2, ParticipantPermission::Observation, false),
                ],
                party_signing_keys: party_keys(),
            },
        };
        let Ok(mapping) = add_hosts_mapping(
            &current.mapping,
            &[participant(3)],
            HostPermission::Confirmation,
        ) else {
            panic!("building must succeed");
        };
        let bundle = bundle_of(mapping, 5);
        let Err(e) = validate_add_hosts_topology(&test_config(3), &current, &bundle) else {
            panic!("only one host can confirm, so a threshold of 2 must be refused");
        };
        assert!(e.to_string().contains("below the party's threshold"), "{e}");
    }

    /// B2, the failover-only offer: a Submission host at threshold 1. It needs
    /// no change to the party's application, because the host keeps submitting
    /// on the party's behalf rather than the party signing for itself.
    #[test]
    fn accepts_a_submission_host_while_the_threshold_is_one() {
        let current = CurrentPartyTopology {
            serial: 4,
            mapping: PartyToParticipant {
                party: "alice::1220aa".to_string(),
                threshold: 1,
                participants: vec![host(1, ParticipantPermission::Submission, false)],
                party_signing_keys: party_keys(),
            },
        };
        let Ok(mapping) = add_hosts_mapping(
            &current.mapping,
            &[participant(3)],
            HostPermission::Submission,
        ) else {
            panic!("a failover host at threshold 1 must be allowed");
        };
        let Some(added) = mapping
            .participants
            .iter()
            .find(|p| p.participant_uid == participant(3).to_string())
        else {
            panic!("the new host must appear");
        };
        assert_eq!(added.permission, ParticipantPermission::Submission as i32);
        assert!(added.onboarding.is_some(), "even a failover host is marked");

        if let Err(e) =
            validate_add_hosts_topology(&test_config(3), &current, &bundle_of(mapping, 5))
        {
            panic!("the mapping this builds must validate: {e}");
        }
    }

    /// And the validator refuses it too, since the onboard endpoint is
    /// independently callable and must not trust that prepare was consulted.
    #[test]
    fn validator_refuses_a_submission_host_above_threshold_one() {
        // threshold 2, so Submission is not permitted for the joiner
        let mut mapping = built();
        for entry in &mut mapping.participants {
            if entry.participant_uid == participant(3).to_string() {
                entry.permission = ParticipantPermission::Submission as i32;
            }
        }
        mapping.threshold = 2;
        let mut current = current();
        current.mapping.threshold = 2;
        let Err(e) = validate_add_hosts_topology(&test_config(3), &current, &bundle_of(mapping, 5))
        else {
            panic!("a Submission joiner above threshold 1 must be refused");
        };
        assert!(e.to_string().contains("threshold is 1"), "{e}");
    }

    /// A Submission host can act for the party alone, so it satisfies the
    /// active-hosts invariant that an Observation host does not.
    #[test]
    fn a_submission_host_counts_as_active() {
        let current = CurrentPartyTopology {
            serial: 4,
            mapping: PartyToParticipant {
                party: "alice::1220aa".to_string(),
                threshold: 1,
                participants: vec![host(1, ParticipantPermission::Submission, false)],
                party_signing_keys: party_keys(),
            },
        };
        let Ok(mapping) = add_hosts_mapping(
            &current.mapping,
            &[participant(3)],
            HostPermission::Submission,
        ) else {
            panic!("building must succeed");
        };
        // One unmarked Submission host covers a threshold of 1.
        if let Err(e) =
            validate_add_hosts_topology(&test_config(3), &current, &bundle_of(mapping, 5))
        {
            panic!("a lone unmarked Submission host must satisfy threshold 1: {e}");
        }
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

    /// A signature attributed to anything but the party's own namespace cannot
    /// authorize its topology. Caught here so it reads as the caller's mistake
    /// rather than as this host being unhealthy.
    #[test]
    fn rejects_a_signed_by_that_is_not_the_party_namespace() {
        let mut bundle = bundle_of(built(), 5);
        bundle.signed_by = "1220ff".to_string();
        let Err(e) = validate_add_hosts_topology(&test_config(3), &current(), &bundle) else {
            panic!("a signature attributed to another key must be refused");
        };
        assert!(e.to_string().contains("is not"), "{e}");
        assert!(e.to_string().contains("namespace"), "{e}");
    }

    #[test]
    fn rejects_a_signature_that_is_not_64_bytes() {
        for len in [0usize, 1, 63, 65, 128] {
            let mut bundle = bundle_of(built(), 5);
            bundle.signatures = vec![vec![0u8; len]];
            let Err(e) = validate_add_hosts_topology(&test_config(3), &current(), &bundle) else {
                panic!("a {len}-byte signature must be refused");
            };
            assert!(
                e.to_string().contains("Ed25519") || e.to_string().contains("index-aligned"),
                "{e}"
            );
        }
    }

    /// The length rule follows the party key. A secp256k1 party may sign in
    /// DER, which is seldom 64 bytes, and the mapping is what says so.
    #[test]
    fn accepts_a_der_signature_for_a_secp256k1_party() {
        let key = SigningPublicKey {
            format: CryptoKeyFormat::DerX509SubjectPublicKeyInfo as i32,
            public_key: vec![9u8; 88],
            key_spec: SigningKeySpec::EcSecp256k1 as i32,
            usage: vec![
                SigningKeyUsage::Namespace as i32,
                SigningKeyUsage::Protocol as i32,
            ],
            ..Default::default()
        };
        let fingerprint = crate::utils::compute_fingerprint(&key);
        let party = format!("alice::{fingerprint}");
        let current = CurrentPartyTopology {
            serial: 4,
            mapping: PartyToParticipant {
                party: party.clone(),
                threshold: 1,
                participants: vec![
                    host(1, ParticipantPermission::Confirmation, false),
                    host(2, ParticipantPermission::Confirmation, false),
                ],
                party_signing_keys: Some(SigningKeysWithThreshold {
                    keys: vec![key],
                    threshold: 1,
                }),
            },
        };
        let mapping = match add_hosts_mapping(
            &current.mapping,
            &[participant(3)],
            HostPermission::Confirmation,
        ) {
            Ok(m) => m,
            Err(e) => panic!("{e}"),
        };
        let mut der = vec![0x30, 0x45, 0x02, 0x21, 0x00];
        der.extend([0x80; 32]);
        der.extend([0x02, 0x20]);
        der.extend([0x7f; 32]);
        let bundle = ExternalPartyAddHostsPayload {
            party_id: party,
            base_serial: 4,
            topology_transactions: vec![serialize(mapping, 5)],
            signatures: vec![der],
            signed_by: fingerprint.into(),
        };

        let result = validate_add_hosts_topology(&test_config(3), &current, &bundle);

        assert!(result.is_ok(), "{result:?}");
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
