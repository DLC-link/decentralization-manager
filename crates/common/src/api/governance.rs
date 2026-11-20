//! Governance building blocks (no on-chain decimals), confirmation requests,
//! governance state and coupon reassignment delegations.

use serde::{Deserialize, Serialize};

use crate::canton_id::CantonId;

/// Instrument identifier (admin + id)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct InstrumentId {
    pub admin: String,
    pub id: String,
}

/// Credential claim (subject, property, value)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct Claim {
    pub subject: String,
    pub property: String,
    pub value: String,
}

/// One claim a `PartyCredentialRequirement` demands: a credential's claims
/// must contain this `(property, value)` pair. The Daml side is a
/// `DA.Types:Tuple2 Text Text`; the wire shape names the halves instead.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct RequiredClaim {
    pub property: String,
    pub value: String,
}

/// A credential requirement on a party: `issuer` must have issued the party
/// a credential whose claims contain every entry of `required_claims`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct PartyCredentialRequirement {
    pub issuer: CantonId,
    #[serde(default)]
    pub required_claims: Vec<RequiredClaim>,
}

/// One offboarded instrument issuer and the credentials to revoke for it. The
/// Daml side checks that every claim on each credential names this issuer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct InstrumentIssuerCredentials {
    pub instrument_issuer: CantonId,
    #[serde(default)]
    pub credential_cids: Vec<String>,
}

/// Which governance system a request targets
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub enum GovernanceType {
    /// GovernanceRules self-management (GovernanceSelfAction)
    #[default]
    CoreSelf,
    /// GovernanceRules domain actions (GovernableAction proposals)
    CoreDomain,
}

/// Instrument allowance for token preapproval
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct InstrumentAllowance {
    pub id: String,
}

/// Additional identifier for an instrument, supplied at `SetupUtility` time.
/// Mirrors `Utility.Registry.Holding.V0.Types.InstrumentIdentifier` — used to
/// record standard market symbols (e.g. Ticker, ISIN) alongside the primary
/// `instrument_id_text` UUID.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct InstrumentIdentifier {
    pub source: CantonId,
    pub id: String,
    pub scheme: String,
}

/// A disclosed contract to include in the ledger submission
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct DisclosedContractInput {
    pub contract_id: String,
    pub blob: String, // base64-encoded created_event_blob
}

/// Request to expire a stale confirmation
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ExpireConfirmationRequest {
    pub party_id: CantonId,
    pub rules_contract_id: String,
    pub confirmation_cid: String,
    #[serde(default)]
    pub governance_type: GovernanceType,
}

/// Request to cancel (revoke) own confirmation
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct CancelConfirmationRequest {
    pub party_id: CantonId,
    pub confirmation_cid: String,
    #[serde(default)]
    pub governance_type: GovernanceType,
}

/// Request to retract a proposal the caller proposed.
///
/// `confirmation_cid` carries the caller's own confirmation on that proposal,
/// which `/governance/propose` always creates. The server archives it in the
/// same transaction, because a confirmation that outlives its proposal can
/// only be cleared by its own confirmer, and only `GovernanceConfirmation_Cancel`
/// clears it without waiting for the expiry time.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct CancelProposalRequest {
    pub party_id: CantonId,
    pub proposal_cid: String,
    #[serde(default)]
    pub confirmation_cid: Option<String>,
}

/// State of a GovernanceRules contract
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct GovernanceState {
    pub contract_id: String,
    pub governance_party: CantonId,
    pub members: Vec<CantonId>,
    pub threshold: i64,
    /// Parties that may propose governance actions but not confirm or execute
    /// them (`GovernanceRules.additionalProposers`). Empty when the rules
    /// contract sets none; `default` accepts a response from a node that
    /// predates the field.
    #[serde(default)]
    pub additional_proposers: Vec<CantonId>,
    /// Parties that observe the rules contract without hosting the governance
    /// party (`GovernanceRules.disclosureObservers`). Empty when the rules
    /// contract sets none or predates the field; `default` accepts a response
    /// from a node that predates it.
    #[serde(default)]
    pub disclosure_observers: Vec<CantonId>,
    // Optional on the wire: older governance rules contracts predate this field.
    // `skip_serializing_if` omits it when None and `default` accepts it missing;
    // ts-rs renders it as an optional TS property via the type's `optional_fields`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_confirmation_timeout_microseconds: Option<i64>,
    /// The package-name ref the active rules contract actually lives under,
    /// e.g. `#governance-core-v0-rc4`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_ref: Option<String>,
    /// True when the active governance-core rules contract was found under an
    /// older package than the configured `governance_core` ref (a fallback
    /// hit) — the party should be migrated to the latest package.
    #[serde(default)]
    pub out_of_date: bool,
}

/// Response for the governance state endpoint
#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct GovernanceStateResponse {
    pub state: Option<GovernanceState>,
}

/// A decparty's active `CouponReassignmentDelegation`.
#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct CouponReassignmentDelegationSummary {
    pub cid: String,
    /// The DSO whose coupons this delegation may assign.
    pub dso: CantonId,
    pub assigners: Vec<CantonId>,
    pub beneficiary_count: usize,
}

/// A decparty's active `CouponReassignmentDelegation` contracts, newest first.
///
/// The vote forms read this so a human never pastes a contract id that a typo
/// would invalidate for the delegation's whole life. Setup prefills
/// `prior_delegation` from it; revoke prefills the contract to archive.
///
/// **Normally this holds zero or one entry.** It can hold more: the template is
/// keyless, so Canton cannot enforce the per-decparty singleton, and the
/// propose-time guard is best-effort. Two proposals racing, or a direct ledger
/// submit, can leave several active. The list reports what is really there, and
/// the **first entry is the one the automation acts on**.
///
/// Contains only the configured package's delegations. One in a superseded
/// package is not exerciseable by the actions this build proposes.
#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ActiveCouponReassignmentDelegation {
    /// Newest first. Empty when this decparty has none, in which case there is
    /// nothing to replace and nothing to revoke.
    pub delegations: Vec<CouponReassignmentDelegationSummary>,
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::*;
    use crate::api::fixtures::party;

    /// The response DTOs are deserialized by the integration-test harness and
    /// the CLI, not just serialized by the server. A `skip_serializing_if`
    /// without a matching `default` makes the omitted field a hard
    /// "missing field" error on the way back in, so round-trip the shape the
    /// server actually emits when every optional field is absent.
    #[test]
    fn governance_state_round_trips_with_every_optional_field_omitted() -> Result<()> {
        let state = GovernanceState {
            contract_id: "00rules".to_owned(),
            governance_party: party("mgr")?,
            members: vec![party("m1")?],
            threshold: 2,
            additional_proposers: vec![],
            disclosure_observers: vec![],
            action_confirmation_timeout_microseconds: None,
            package_ref: None,
            out_of_date: false,
        };
        let json = serde_json::to_string(&state)?;
        assert!(!json.contains("package_ref"), "omitted on the wire: {json}");
        let back: GovernanceState = serde_json::from_str(&json)?;
        assert_eq!(back.contract_id, state.contract_id);
        assert_eq!(back.package_ref, None);

        // A node that predates `additional_proposers` or
        // `disclosure_observers` omits it entirely.
        let mut older = serde_json::to_value(&state)?;
        let fields = older.as_object_mut().expect("an object");
        fields.remove("additional_proposers");
        fields.remove("disclosure_observers");
        let back: GovernanceState = serde_json::from_value(older)?;
        assert!(back.additional_proposers.is_empty());
        assert!(back.disclosure_observers.is_empty());

        let json = serde_json::to_string(&GovernanceStateResponse { state: None })?;
        let back: GovernanceStateResponse = serde_json::from_str(&json)?;
        assert!(back.state.is_none());
        Ok(())
    }
}
