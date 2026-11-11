use std::collections::BTreeSet;

use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{
        DecentralizedNamespaceDefinition, PartyToParticipant, SignedTopologyTransaction,
        topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, AuthorizeResponse, StoreId, authorize_request,
        topology_manager_write_service_client::TopologyManagerWriteServiceClient,
    },
};
use tonic::transport::Channel;

use super::{
    route::{Route, rebuild_outcome, signing_route},
    seed::{ensure_seeded_to, seed},
    store::{CantonStores, in_store, next_serial, temporary_store_id},
};
use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    error::Result,
    workflow::{
        topology,
        topology_version::{ProposalFormat, protocol_version_of},
    },
};

/// A party's namespace definition and participant mapping as the synchronizer
/// holds them, each with the serial of its head transaction.
#[derive(Clone, Debug)]
pub struct PartyHead {
    pub dns_serial: u32,
    pub dns: DecentralizedNamespaceDefinition,
    pub p2p_serial: u32,
    pub p2p: PartyToParticipant,
}

impl PartyHead {
    /// Refuse to build on a namespace that changed after `ExportState` read
    /// it.
    ///
    /// The proposals are built from the exported definition but pinned to the
    /// serial read now. If the two disagree, the proposal would replace a
    /// change this run never looked at.
    ///
    /// # Errors
    ///
    /// Errors when the head namespace differs from `exported`.
    pub fn ensure_namespace_is(&self, exported: &DecentralizedNamespaceDefinition) -> Result {
        if same_namespace(&self.dns, exported) {
            return Ok(());
        }
        anyhow::bail!(
            "The namespace {namespace} changed after this run read it: it now has threshold \
             {now_threshold} and {now_owners} owner(s) at serial {serial}, against threshold \
             {then_threshold} and {then_owners} owner(s) when the run started. Start the \
             workflow again so it is built on the current state",
            namespace = exported.decentralized_namespace,
            now_threshold = self.dns.threshold,
            now_owners = self.dns.owners.len(),
            serial = self.dns_serial,
            then_threshold = exported.threshold,
            then_owners = exported.owners.len(),
        )
    }
}

/// Whether two namespace definitions are the same. Owners are a set to
/// Canton, so their order does not count.
fn same_namespace(
    a: &DecentralizedNamespaceDefinition,
    b: &DecentralizedNamespaceDefinition,
) -> bool {
    a.decentralized_namespace == b.decentralized_namespace
        && a.threshold == b.threshold
        && a.owners.iter().collect::<BTreeSet<_>>() == b.owners.iter().collect::<BTreeSet<_>>()
}

/// Read the party's [`PartyHead`] from the synchronizer.
///
/// # Errors
///
/// Errors when either mapping is missing or a serial is out of range.
pub async fn fetch_party_head(
    config: &NodeConfig,
    synchronizer_id: &str,
    party_id: &CantonId,
) -> Result<PartyHead> {
    let (dns_serial, dns) = topology::fetch_namespace_definition_at_head(
        config,
        synchronizer_id,
        &party_id.namespace.to_hex(),
    )
    .await?;
    let (p2p_serial, p2p) =
        topology::fetch_p2p_mapping_at_head(config, synchronizer_id, party_id).await?;
    Ok(PartyHead {
        dns_serial,
        dns,
        p2p_serial,
        p2p,
    })
}

/// The coordinator's signed proposals, not yet published.
pub struct SignedProposals {
    /// `None` when the run leaves the namespace as it is.
    pub dns: Option<SignedTopologyTransaction>,
    /// `None` when the run leaves the hosting mapping as it is.
    pub p2p: Option<SignedTopologyTransaction>,
}

/// Sign the party's new DNS (when `new_dns` is set) and new P2P (when
/// `new_p2p` is set) at the serial after `head`'s.
///
/// Canton refuses a mapping equal to the one in force at any other serial, so
/// a run that leaves a mapping as it is must pass `None` for it.
///
/// `head` must be what the proposals were built from. The proposals are
/// signed in a temporary store seeded until its heads equal `head`'s serials,
/// and nothing is published. When the replay cannot rebuild those heads,
/// `signing_route` decides: above namespace threshold 1 the proposals are
/// signed and published with `Authorize` against the synchronizer store, as
/// before #448; at threshold 1 the run stops.
///
/// # Errors
///
/// Errors when the store cannot be created, when the participant cannot be
/// reached, when a threshold-1 party's history does not replay, or when
/// Canton refuses to sign.
#[allow(clippy::too_many_arguments)]
pub async fn sign_party_proposals(
    config: &NodeConfig,
    synchronizer_id: &str,
    instance_name: &str,
    party_id: &CantonId,
    head: &PartyHead,
    new_dns: Option<DecentralizedNamespaceDefinition>,
    new_p2p: Option<PartyToParticipant>,
    force_flags: Vec<i32>,
) -> Result<SignedProposals> {
    // A temporary store is created at the protocol version of the store its
    // transactions are published to.
    let protocol_version = protocol_version_of(synchronizer_id)?;
    let format = ProposalFormat::for_synchronizer(synchronizer_id);
    let serials = (
        next_serial("DNS", head.dns_serial)?,
        next_serial("P2P", head.p2p_serial)?,
    );
    let channel = config.admin_channel().await?;
    let stores = CantonStores {
        channel: channel.clone(),
    };
    let proposals = (new_dns, new_p2p);

    let attempt = {
        let config = config.clone();
        let synchronizer_id = synchronizer_id.to_string();
        let party_id = party_id.clone();
        let head = head.clone();
        let proposals = proposals.clone();
        let force_flags = force_flags.clone();
        in_store(
            stores,
            instance_name,
            protocol_version,
            move |store| async move {
                let rebuilt = async {
                    let replayed = seed(&channel, &synchronizer_id, &store, &party_id).await?;
                    ensure_seeded_to(&config, &store, &party_id, &head).await?;
                    Ok(replayed)
                }
                .await;
                let route = signing_route(head.dns.threshold, rebuild_outcome(&rebuilt));
                match (route, rebuilt) {
                    (Route::TemporaryStore, Ok(replayed)) => {
                        tracing::info!(
                            replayed,
                            %party_id,
                            %store,
                            dns_serial = serials.0,
                            p2p_serial = serials.1,
                            "Replayed the party's topology history into a temporary store; \
                             signing there"
                        );
                        let signed = sign(
                            &channel,
                            &temporary_store_id(&store),
                            proposals,
                            serials,
                            force_flags,
                            format,
                        )
                        .await?;
                        Ok(Attempt::Signed(signed))
                    }
                    (Route::Synchronizer, Err(e)) => Ok(Attempt::FallBack(format!("{e:#}"))),
                    (Route::Refuse, Err(e)) => Err(e.context(format!(
                        "Could not rebuild {party_id}'s topology history in a temporary store, \
                         and its namespace threshold is {threshold}. Signing against the \
                         synchronizer would put the namespace change in force on this node's \
                         signature alone, so nothing was signed or published",
                        threshold = head.dns.threshold
                    ))),
                    // `Route::Retry`: the participant could not be reached.
                    (_, Err(e)) => Err(e),
                    (route, Ok(_)) => anyhow::bail!("{route:?} chosen for a rebuilt store"),
                }
            },
        )
        .await?
    };

    match attempt {
        Attempt::Signed(signed) => Ok(signed),
        Attempt::FallBack(reason) => {
            tracing::warn!(
                %party_id,
                threshold = head.dns.threshold,
                reason,
                "Could not rebuild the party's topology history in a temporary store; signing \
                 against the synchronizer store instead. Above threshold 1 the proposals stay \
                 pending until the members sign"
            );
            let store = topology::synchronizer_store_id(synchronizer_id);
            let signed =
                sign_on_synchronizer(config, &store, proposals, serials, force_flags, format)
                    .await?;
            Ok(signed)
        }
    }
}

/// What signing in the temporary store came to.
enum Attempt {
    Signed(SignedProposals),
    /// The replay failed above threshold 1; the reason, for the log.
    FallBack(String),
}

/// Sign the DNS and then the P2P in `store`, each when there is one.
async fn sign(
    channel: &Channel,
    store: &StoreId,
    (new_dns, new_p2p): (
        Option<DecentralizedNamespaceDefinition>,
        Option<PartyToParticipant>,
    ),
    (dns_serial, p2p_serial): (u32, u32),
    force_flags: Vec<i32>,
    format: ProposalFormat,
) -> Result<SignedProposals> {
    let dns = match new_dns {
        Some(definition) => Some(
            authorize(
                channel,
                proposal_request(
                    store.clone(),
                    topology_mapping::Mapping::DecentralizedNamespaceDefinition(definition),
                    dns_serial,
                    force_flags.clone(),
                    format,
                ),
            )
            .await?,
        ),
        None => None,
    };
    let p2p = match new_p2p {
        Some(mapping) => Some(
            authorize(
                channel,
                proposal_request(
                    store.clone(),
                    topology_mapping::Mapping::PartyToParticipant(mapping),
                    p2p_serial,
                    force_flags,
                    format,
                ),
            )
            .await?,
        ),
        None => None,
    };
    Ok(SignedProposals { dns, p2p })
}

/// Sign the pair against the synchronizer store, as the coordinator did before
/// #448. `Authorize` there publishes each proposal as it signs it. The serials
/// stay pinned to `head` + 1, so a head that moved in the meantime is refused
/// rather than replaced.
async fn sign_on_synchronizer(
    config: &NodeConfig,
    store: &StoreId,
    (new_dns, new_p2p): (
        Option<DecentralizedNamespaceDefinition>,
        Option<PartyToParticipant>,
    ),
    (dns_serial, p2p_serial): (u32, u32),
    force_flags: Vec<i32>,
    format: ProposalFormat,
) -> Result<SignedProposals> {
    let signed = |response: AuthorizeResponse| {
        response
            .transaction
            .ok_or_else(|| anyhow::anyhow!("Authorize returned no transaction"))
    };
    let dns = match new_dns {
        Some(definition) => Some(signed(
            topology::authorize_with_topology_retry(
                config,
                proposal_request(
                    store.clone(),
                    topology_mapping::Mapping::DecentralizedNamespaceDefinition(definition),
                    dns_serial,
                    force_flags.clone(),
                    format,
                ),
                "DNS proposal",
            )
            .await?,
        )?),
        None => None,
    };
    let p2p = match new_p2p {
        Some(mapping) => Some(signed(
            topology::authorize_with_topology_retry(
                config,
                proposal_request(
                    store.clone(),
                    topology_mapping::Mapping::PartyToParticipant(mapping),
                    p2p_serial,
                    force_flags,
                    format,
                ),
                "P2P proposal",
            )
            .await?,
        )?),
        None => None,
    };
    Ok(SignedProposals { dns, p2p })
}

/// An `Authorize` that proposes `mapping` at `serial` in `store`.
///
/// `must_fully_authorize` stays false: this node's signature alone is not the
/// party's authorization, and the peers' signatures are merged in before
/// submit. The serial is explicit: a temporary store is not where the
/// transaction is published, and against the synchronizer store it refuses a
/// head that moved. `format` writes the mapping as the synchronizer's
/// protocol version needs it.
fn proposal_request(
    store: StoreId,
    mapping: topology_mapping::Mapping,
    serial: u32,
    force_flags: Vec<i32>,
    format: ProposalFormat,
) -> AuthorizeRequest {
    AuthorizeRequest {
        r#type: Some(authorize_request::Type::Proposal(
            format.proposal(serial, mapping),
        )),
        must_fully_authorize: false,
        force_changes: force_flags,
        signed_by: vec![],
        store: Some(store),
        wait_to_become_effective: None,
    }
}

/// Sign in the temporary store.
///
/// No retry on `TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE`, unlike the
/// synchronizer-store paths: the store holds exactly what the replay put in,
/// so a missing key will not appear by waiting.
async fn authorize(
    channel: &Channel,
    request: AuthorizeRequest,
) -> Result<SignedTopologyTransaction> {
    TopologyManagerWriteServiceClient::new(channel.clone())
        .authorize(tonic::Request::new(request))
        .await?
        .into_inner()
        .transaction
        .ok_or_else(|| anyhow::anyhow!("Authorize returned no transaction"))
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::{
        protocol::v30::enums, topology::admin::v30::ForceFlag,
    };

    use super::*;

    #[test]
    fn proposals_are_signed_in_the_temporary_store_at_the_pinned_serial() -> Result {
        let request = proposal_request(
            temporary_store_id("decman-proposals-run~1"),
            topology_mapping::Mapping::DecentralizedNamespaceDefinition(Default::default()),
            7,
            vec![ForceFlag::AllowUnvalidatedSigningKeys as i32],
            ProposalFormat::for_synchronizer("global-domain::1220ab::37-0"),
        );

        assert!(!request.must_fully_authorize);
        assert!(request.signed_by.is_empty());
        assert_eq!(
            request.store,
            Some(temporary_store_id("decman-proposals-run~1"))
        );
        let Some(authorize_request::Type::Proposal(proposal)) = request.r#type else {
            anyhow::bail!("expected a proposal");
        };
        assert_eq!(proposal.serial, 7);
        assert_eq!(proposal.change, enums::TopologyChangeOp::AddReplace as i32);
        assert_eq!(proposal.protocol_version, Some(37));
        Ok(())
    }

    #[test]
    fn a_changed_namespace_is_refused_and_a_reordered_one_is_not() -> Result {
        let exported = DecentralizedNamespaceDefinition {
            decentralized_namespace: "ns".into(),
            threshold: 1,
            owners: vec!["a".into(), "b".into()],
        };
        let mut head = PartyHead {
            dns_serial: 3,
            dns: exported.clone(),
            p2p_serial: 5,
            p2p: PartyToParticipant::default(),
        };
        head.dns.owners.reverse();
        head.ensure_namespace_is(&exported)?;

        head.dns.threshold = 2;
        let error = head
            .ensure_namespace_is(&exported)
            .err()
            .ok_or_else(|| anyhow::anyhow!("a raised threshold went unnoticed"))?;
        assert!(
            format!("{error}").contains("changed after this run read it"),
            "{error}"
        );
        Ok(())
    }
}
