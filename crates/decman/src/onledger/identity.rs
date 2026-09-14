//! The node identity (design D1) and the hosting check (design D2).
//!
//! A node identity is the `party_credentials` row with `kind = 'node'`. Its
//! node party signs every coordination contract this node publishes. The
//! token comes from the same `AuthRegistry` the decparty rows use, keyed by
//! the node party, so no second auth path exists.

use std::{fmt, sync::Arc};

use anyhow::{Context, Result, anyhow, bail};
use canton_proto_rs::com::digitalasset::canton::protocol::v30::PartyToParticipant;
use common::{canton_id::CantonId, types::Permission};

use crate::{
    auth::{MockTokenManager, TokenManager, WorkflowAuth},
    config::{CredentialKind, NodeConfig, PartyCredentials},
    server::permission_from_proto,
    workflow::external_party::add_hosts::read_party_to_participant,
};

/// Where the node party's Ledger API token comes from.
#[derive(Clone)]
pub enum NodeTokenSource {
    /// A real IdP (Keycloak or Auth0) through the shared `AuthRegistry`.
    Keycloak(Arc<TokenManager>),
    /// The unsafe HS256 token of insecure/test mode.
    Mock(Arc<MockTokenManager>),
}

impl NodeTokenSource {
    /// A fresh bearer token for the node party.
    ///
    /// # Errors
    /// Returns an error when the IdP refuses to mint or refresh.
    pub async fn token(&self) -> Result<String> {
        match self {
            Self::Keycloak(tm) => tm.get_token().await.context("node identity token"),
            Self::Mock(mm) => Ok(mm.get_token()),
        }
    }
}

/// This node's on-ledger identity: the node party, the participant that
/// hosts it, and a token source that can act as it.
#[derive(Clone)]
pub struct NodeIdentity {
    pub node_party: CantonId,
    pub participant_id: CantonId,
    /// The Ledger API user behind the token; informational.
    pub user_id: String,
    tokens: NodeTokenSource,
}

impl NodeIdentity {
    /// Assemble an identity from its parts. Callers outside this module
    /// normally go through [`load_node_identity`].
    pub fn new(
        node_party: CantonId,
        participant_id: CantonId,
        user_id: String,
        tokens: NodeTokenSource,
    ) -> Self {
        Self {
            node_party,
            participant_id,
            user_id,
            tokens,
        }
    }

    /// A fresh bearer token for the node party.
    ///
    /// # Errors
    /// Returns an error when the IdP refuses to mint or refresh.
    pub async fn token(&self) -> Result<String> {
        self.tokens.token().await
    }

    pub fn tokens(&self) -> &NodeTokenSource {
        &self.tokens
    }
}

impl fmt::Debug for NodeIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeIdentity")
            .field("node_party", &self.node_party)
            .field("participant_id", &self.participant_id)
            .field("user_id", &self.user_id)
            .finish_non_exhaustive()
    }
}

/// The `kind = 'node'` row, if any. At most one is expected; the first wins.
pub fn node_credentials(rows: &[PartyCredentials]) -> Option<&PartyCredentials> {
    rows.iter().find(|r| r.kind == CredentialKind::Node)
}

/// Build the node identity from the credentials rows and the live auth
/// registry.
///
/// Returns `Ok(None)` when no `kind = 'node'` row exists. Returns an error
/// when a row exists but no token manager backs it, because a node that
/// believes it has an identity but cannot act as it must fail loudly.
///
/// # Errors
/// See above; also when `config` has no resolved participant id.
pub async fn load_node_identity(
    config: &NodeConfig,
    rows: &[PartyCredentials],
    auth: Option<&WorkflowAuth>,
) -> Result<Option<NodeIdentity>> {
    let Some(row) = node_credentials(rows) else {
        return Ok(None);
    };
    if row.member_party_id != row.dec_party_id {
        tracing::warn!(
            dec_party_id = %row.dec_party_id,
            member_party_id = %row.member_party_id,
            "node identity row has differing dec_party_id and member_party_id; \
             using member_party_id as the node party"
        );
    }
    let Some(participant_id) = config.node.participant_id.clone() else {
        bail!("participant id not resolved; cannot build the node identity");
    };
    let tokens = match auth {
        Some(WorkflowAuth::Keycloak(registry)) => {
            let tm = registry.get(&row.dec_party_id).ok_or_else(|| {
                anyhow!(
                    "node identity {} has no token manager; check its IdP credentials",
                    row.dec_party_id
                )
            })?;
            NodeTokenSource::Keycloak(tm)
        }
        Some(WorkflowAuth::Mock(registry)) => {
            NodeTokenSource::Mock(registry.get(&row.dec_party_id).await)
        }
        None => bail!(
            "node identity {} is configured but the auth registry is not initialised",
            row.dec_party_id
        ),
    };
    Ok(Some(NodeIdentity::new(
        row.member_party_id.clone(),
        participant_id,
        row.user_id.clone(),
        tokens,
    )))
}

/// The identity, or the one error message every caller shows when it is
/// missing.
///
/// # Errors
/// Returns an error when `identity` is `None`.
pub fn require_node_identity(identity: Option<&NodeIdentity>) -> Result<&NodeIdentity> {
    identity.ok_or_else(|| {
        anyhow!("node identity not configured; set one with PUT /node-identity first")
    })
}

/// How one participant hosts one party, read from the synchronizer head state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostingCheck {
    /// Whether an authorized `PartyToParticipant` exists at all.
    pub mapping_exists: bool,
    /// Whether the participant appears among the party's hosts.
    pub hosted: bool,
    /// The participant's permission, when hosted.
    pub permission: Option<Permission>,
    /// Whether the participant still carries the `onboarding` marker.
    pub onboarding: bool,
    /// The party's hosting threshold (0 when no mapping exists).
    pub threshold: u32,
}

impl HostingCheck {
    /// The one predicate the design uses before naming a party as observer
    /// or accepting its acceptance: hosted with Submission permission.
    pub fn has_submission(&self) -> bool {
        self.hosted && self.permission == Some(Permission::Submission)
    }

    /// A short human-readable reason for a rejection.
    pub fn describe(&self) -> String {
        if !self.mapping_exists {
            return "no PartyToParticipant mapping in the synchronizer head state".to_string();
        }
        if !self.hosted {
            return "the participant does not host the party".to_string();
        }
        match &self.permission {
            Some(p) => format!("hosted with {} permission", p.as_str()),
            None => "hosted with unknown permission".to_string(),
        }
    }
}

/// Pure half of [`verify_hosting`]: evaluate a mapping against a participant.
pub fn hosting_check_from_mapping(
    mapping: Option<&PartyToParticipant>,
    participant: &CantonId,
) -> HostingCheck {
    let Some(mapping) = mapping else {
        return HostingCheck {
            mapping_exists: false,
            hosted: false,
            permission: None,
            onboarding: false,
            threshold: 0,
        };
    };
    let participant_uid = participant.to_string();
    let host = mapping
        .participants
        .iter()
        .find(|h| h.participant_uid == participant_uid);
    HostingCheck {
        mapping_exists: true,
        hosted: host.is_some(),
        permission: host.map(|h| permission_from_proto(h.permission)),
        onboarding: host.is_some_and(|h| h.onboarding.is_some()),
        threshold: mapping.threshold,
    }
}

/// Read the head-state `PartyToParticipant` of `party` over the tokenless
/// Admin API and report how `participant` hosts it.
///
/// `read_party_to_participant` already requires `mapping.party == party`, so
/// a prefix match on another party can never pass.
///
/// # Errors
/// Returns an error when the topology read fails.
pub async fn verify_hosting(
    config: &NodeConfig,
    party: &CantonId,
    participant: &CantonId,
) -> Result<HostingCheck> {
    let current = read_party_to_participant(config, &party.to_string())
        .await
        .with_context(|| format!("read PartyToParticipant for {party}"))?;
    Ok(hosting_check_from_mapping(
        current.as_ref().map(|c| &c.mapping),
        participant,
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::v30::{
        enums::ParticipantPermission, party_to_participant::HostingParticipant,
    };

    use super::*;

    const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

    fn id(prefix: &str) -> CantonId {
        CantonId::parse(&format!("{prefix}::{NS}")).expect("valid id")
    }

    /// An identity backed by the mock token, for tests in sibling modules.
    pub(crate) async fn mock_identity(node_prefix: &str, participant: CantonId) -> NodeIdentity {
        let registry = crate::auth::MockAuthRegistry::new(Default::default());
        NodeIdentity::new(
            id(node_prefix),
            participant,
            "ledger-api-user".into(),
            NodeTokenSource::Mock(registry.get_by_str("").await),
        )
    }

    fn mapping(hosts: &[(&str, ParticipantPermission)]) -> PartyToParticipant {
        PartyToParticipant {
            party: id("node-a").to_string(),
            threshold: 1,
            participants: hosts
                .iter()
                .map(|(p, perm)| HostingParticipant {
                    participant_uid: id(p).to_string(),
                    permission: *perm as i32,
                    onboarding: None,
                })
                .collect(),
            party_signing_keys: None,
        }
    }

    #[test]
    fn submission_host_passes_the_check() {
        let m = mapping(&[("participant1", ParticipantPermission::Submission)]);
        let check = hosting_check_from_mapping(Some(&m), &id("participant1"));
        assert!(check.has_submission());
        assert_eq!(check.permission, Some(Permission::Submission));
        assert_eq!(check.threshold, 1);
    }

    #[test]
    fn confirmation_only_host_fails_the_check() {
        let m = mapping(&[("participant1", ParticipantPermission::Confirmation)]);
        let check = hosting_check_from_mapping(Some(&m), &id("participant1"));
        assert!(check.hosted);
        assert!(!check.has_submission());
        assert!(check.describe().contains("confirmation"));
    }

    #[test]
    fn a_different_participant_is_not_hosted() {
        let m = mapping(&[("participant1", ParticipantPermission::Submission)]);
        let check = hosting_check_from_mapping(Some(&m), &id("participant2"));
        assert!(check.mapping_exists);
        assert!(!check.hosted);
        assert!(!check.has_submission());
    }

    #[test]
    fn no_mapping_is_not_hosted() {
        let check = hosting_check_from_mapping(None, &id("participant1"));
        assert!(!check.mapping_exists);
        assert!(!check.has_submission());
        assert!(check.describe().contains("no PartyToParticipant"));
    }

    #[test]
    fn node_credentials_picks_the_node_row_only() {
        let mut dec = PartyCredentials {
            kind: CredentialKind::Decparty,
            dec_party_id: id("cbtc"),
            member_party_id: id("member"),
            user_id: "u".into(),
            keycloak: Default::default(),
            auth0: None,
            packages: Default::default(),
        };
        assert!(node_credentials(std::slice::from_ref(&dec)).is_none());
        dec.kind = CredentialKind::Node;
        assert_eq!(
            node_credentials(std::slice::from_ref(&dec)).map(|r| &r.dec_party_id),
            Some(&id("cbtc"))
        );
    }

    #[test]
    fn require_node_identity_names_the_fix() {
        let err = require_node_identity(None).expect_err("missing");
        assert!(err.to_string().contains("PUT /node-identity"));
    }
}
