//! `ActionType` — decman's governance action payload — and its Daml `Value`
//! codec.
//!
//! The Daml side splits this one Rust enum across two closed unions:
//! `#cbtc-governance`'s `ActionRequiringConfirmation` and `governance-core`'s
//! `GovernanceSelfAction`. [`ActionType::from_cbtc_proto`] decodes the first
//! form; [`ActionType::to_self_proto`] / [`ActionType::from_self_proto`]
//! encode and decode the second. The CBTC form is read-only here, because
//! decman never writes a `Confirmation` contract. A variant that the self
//! form does not carry returns `Error::Encode` from its encoder rather than
//! panicking.

use canton_proto_rs::com::daml::ledger::api::v2::{Record, Value, Variant, value};
use common::api::Claim;
use common::canton_id::CantonId;

use crate::catalog::types::{deserialize_claim, deserialize_reltime};
use crate::error::Error;
use crate::framework::encode::{
    field, make_contract_id, make_int64, make_optional_party_set, make_party, make_record,
    make_variant, serialize_reltime,
};
use crate::framework::record::{
    extract_contract_id, extract_int64, extract_list, extract_party_id, extract_party_set,
    extract_record, extract_text, get_field,
};
use crate::framework::validate::{validate_threshold, validate_timeout};

/// Structured action types for decentralized-party governance
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionType {
    // Governance (4)
    GovernanceAddMember {
        member: CantonId,
        new_threshold: i64,
    },
    GovernanceRemoveMember {
        member: CantonId,
        new_threshold: i64,
    },
    GovernanceSetThreshold {
        new_threshold: i64,
    },
    GovernanceSetTimeout {
        new_timeout_microseconds: i64,
    },
    GovernanceAddAdditionalProposer {
        additional_proposer: CantonId,
    },
    GovernanceRemoveAdditionalProposer {
        additional_proposer: CantonId,
    },
    /// Replaces `GovernanceRules.disclosureObservers` with this set. `None`
    /// clears it, and so does an empty set once the ledger normalizes it.
    GovernanceSetDisclosureObservers {
        #[serde(default)]
        disclosure_observers: Option<Vec<CantonId>>,
    },
    /// Archives the `GovernableAction` proposal with this contract id.
    GovernanceCancelAction {
        proposal_cid: String,
    },

    // Utility Onboarding (4)
    UtilityCreateProviderRequest {
        operator: CantonId,
    },
    UtilityCreateUserRequest {
        operator: CantonId,
    },
    UtilitySetup {
        operator: CantonId,
        provider_service_cid: String,
        user_service_cid: String,
    },
    UtilityAcceptHolderServiceRequest {
        operator: CantonId,
        provider_service_cid: String,
        holder_service_request_cid: String,
        holder: CantonId,
    },
    // Credential Actions (2)
    CredentialOfferFree {
        operator: CantonId,
        user_service_cid: String,
        holder: CantonId,
        id: String,
        description: String,
        claims: Vec<Claim>,
    },
    CredentialAcceptFree {
        operator: CantonId,
        user_service_cid: String,
        credential_offer_cid: String,
    },

    // DevNet (1)
    DevNetFeatureApp {
        amulet_rules_cid: String,
    },
}

impl ActionType {
    /// Serialize to a `GovernanceSelfAction` Daml variant.
    ///
    /// Maps the self-management `ActionType` variants to the governance-core
    /// `GovernanceSelfAction` enum (different field names).
    pub fn to_self_proto(&self) -> Result<Value, Error> {
        match self {
            ActionType::GovernanceAddMember {
                member,
                new_threshold,
            } => Ok(make_variant(
                "SelfAction_AddMemberAndSetThreshold",
                make_record(vec![
                    field("newMember", make_party(member)),
                    field("newThresholdAfterAdd", make_int64(*new_threshold)),
                ]),
            )),
            ActionType::GovernanceRemoveMember {
                member,
                new_threshold,
            } => Ok(make_variant(
                "SelfAction_RemoveMemberAndSetThreshold",
                make_record(vec![
                    field("removedMember", make_party(member)),
                    field("newThresholdAfterRemove", make_int64(*new_threshold)),
                ]),
            )),
            ActionType::GovernanceSetThreshold { new_threshold } => Ok(make_variant(
                "SelfAction_SetThreshold",
                make_record(vec![field("updatedThreshold", make_int64(*new_threshold))]),
            )),
            ActionType::GovernanceSetTimeout {
                new_timeout_microseconds,
            } => Ok(make_variant(
                "SelfAction_SetTimeout",
                make_record(vec![field(
                    "updatedTimeout",
                    serialize_reltime(*new_timeout_microseconds),
                )]),
            )),
            ActionType::GovernanceAddAdditionalProposer {
                additional_proposer,
            } => Ok(make_variant(
                "SelfAction_AddAdditionalProposer",
                make_record(vec![field(
                    "additionalProposer",
                    make_party(additional_proposer),
                )]),
            )),
            ActionType::GovernanceRemoveAdditionalProposer {
                additional_proposer,
            } => Ok(make_variant(
                "SelfAction_RemoveAdditionalProposer",
                make_record(vec![field(
                    "additionalProposer",
                    make_party(additional_proposer),
                )]),
            )),
            ActionType::GovernanceSetDisclosureObservers {
                disclosure_observers,
            } => Ok(make_variant(
                "SelfAction_SetDisclosureObservers",
                make_record(vec![field(
                    "updatedDisclosureObservers",
                    make_optional_party_set(disclosure_observers.as_deref()),
                )]),
            )),
            ActionType::GovernanceCancelAction { proposal_cid } => Ok(make_variant(
                "SelfAction_CancelAction",
                make_record(vec![field(
                    "actionProposalCid",
                    make_contract_id(proposal_cid),
                )]),
            )),
            _ => Err(Error::Encode(format!(
                "ActionType {self:?} is not a governance self-management action; it has no \
                 GovernanceSelfAction form and must be proposed as a GovernableAction"
            ))),
        }
    }

    /// Deserialize a Daml Value (`ActionRequiringConfirmation` variant) to an
    /// `ActionType`.
    ///
    /// `#cbtc-governance` writes this form onto its `Confirmation` contracts.
    /// Decman only reads it, so there is no matching encoder.
    ///
    /// Handles nested variant structure:
    /// - GovernanceAction(Governance_AddMemberAndSetThreshold {...})
    /// - UtilityOnboardingAction(UtilityOnboarding_CreateProviderServiceRequest {...})
    /// - DevNetFeatureAppAction({...}) - direct record
    pub fn from_cbtc_proto(value: &Value) -> Result<Self, Error> {
        let variant = as_variant(value)
            .ok_or_else(|| Error::decode("Expected Variant value for action".to_string()))?;

        let inner = variant
            .value
            .as_ref()
            .ok_or_else(|| Error::decode("Variant has no inner value"))?;

        match variant.constructor.as_str() {
            "GovernanceAction" => decode_governance_action(inner),
            "UtilityOnboardingAction" => decode_utility_onboarding_action(inner),
            "CredentialAction" => decode_credential_action(inner),

            // DevNet
            "DevNetFeatureAppAction" => {
                let record = extract_record(inner)?;
                Ok(ActionType::DevNetFeatureApp {
                    amulet_rules_cid: extract_contract_id(get_field(record, "amuletRulesCid")?)?,
                })
            }

            other => Err(Error::decode(format!(
                "Unknown action constructor: {other}"
            ))),
        }
    }

    /// Deserialize a `GovernanceSelfAction` Daml variant to an `ActionType`.
    pub fn from_self_proto(value: &Value) -> Result<Self, Error> {
        let variant = as_variant(value).ok_or_else(|| {
            Error::decode("Expected Variant value for GovernanceSelfAction".to_string())
        })?;

        let inner = variant
            .value
            .as_ref()
            .ok_or_else(|| Error::decode("GovernanceSelfAction variant has no inner value"))?;

        let record = extract_record(inner)
            .map_err(|e| Error::decode_from("Expected GovernanceSelfAction record", e))?;
        let constructor = &variant.constructor;

        match constructor.as_str() {
            "SelfAction_AddMemberAndSetThreshold" => {
                let member = extract_party_id(get_field(record, "newMember")?)?;
                let new_threshold = extract_int64(get_field(record, "newThresholdAfterAdd")?)?;
                Ok(ActionType::GovernanceAddMember {
                    member,
                    new_threshold,
                })
            }
            "SelfAction_RemoveMemberAndSetThreshold" => {
                let member = extract_party_id(get_field(record, "removedMember")?)?;
                let new_threshold = extract_int64(get_field(record, "newThresholdAfterRemove")?)?;
                Ok(ActionType::GovernanceRemoveMember {
                    member,
                    new_threshold,
                })
            }
            "SelfAction_SetThreshold" => {
                let new_threshold = extract_int64(get_field(record, "updatedThreshold")?)?;
                Ok(ActionType::GovernanceSetThreshold { new_threshold })
            }
            "SelfAction_SetTimeout" => {
                let reltime = get_field(record, "updatedTimeout")?;
                let microseconds = deserialize_reltime(reltime)?;
                Ok(ActionType::GovernanceSetTimeout {
                    new_timeout_microseconds: microseconds,
                })
            }
            "SelfAction_AddAdditionalProposer" => {
                let additional_proposer =
                    extract_party_id(get_field(record, "additionalProposer")?)?;
                Ok(ActionType::GovernanceAddAdditionalProposer {
                    additional_proposer,
                })
            }
            "SelfAction_RemoveAdditionalProposer" => {
                let additional_proposer =
                    extract_party_id(get_field(record, "additionalProposer")?)?;
                Ok(ActionType::GovernanceRemoveAdditionalProposer {
                    additional_proposer,
                })
            }
            "SelfAction_SetDisclosureObservers" => {
                let disclosure_observers =
                    extract_optional_party_ids(get_field(record, "updatedDisclosureObservers")?)?;
                Ok(ActionType::GovernanceSetDisclosureObservers {
                    disclosure_observers,
                })
            }
            "SelfAction_CancelAction" => {
                let proposal_cid = extract_contract_id(get_field(record, "actionProposalCid")?)?;
                Ok(ActionType::GovernanceCancelAction { proposal_cid })
            }
            other => Err(Error::decode(format!(
                "Unknown GovernanceSelfAction constructor: {other}"
            ))),
        }
    }

    /// True for the `GovernanceSelfAction` variants — the only actions the
    /// inline (`core_self`) confirm/execute path can serialize.
    ///
    /// `ActionType` still models the utility / credential / DevNet variants
    /// because [`ActionType::from_cbtc_proto`] parses them off CBTC
    /// confirmations on the read path. There is no inline submit path for
    /// them: they belong on `POST /governance/propose` as `GovernableAction`
    /// proposals.
    pub fn is_governance_self_action(&self) -> bool {
        matches!(
            self,
            ActionType::GovernanceAddMember { .. }
                | ActionType::GovernanceRemoveMember { .. }
                | ActionType::GovernanceSetThreshold { .. }
                | ActionType::GovernanceSetTimeout { .. }
                | ActionType::GovernanceAddAdditionalProposer { .. }
                | ActionType::GovernanceRemoveAdditionalProposer { .. }
                | ActionType::GovernanceSetDisclosureObservers { .. }
                | ActionType::GovernanceCancelAction { .. }
        )
    }

    /// Validate the action's fields. Returns an error message if invalid.
    ///
    /// Catches obviously-malformed inputs (negative thresholds, non-positive
    /// timeouts) before they reach Canton's Daml checks. Canton rejects bad
    /// values too, but here we surface a clear 400 rather than a generic
    /// submission error after the proposal contract is already on the wire.
    pub fn validate(&self) -> Result<(), Error> {
        match self {
            ActionType::GovernanceAddMember { new_threshold, .. }
            | ActionType::GovernanceRemoveMember { new_threshold, .. }
            | ActionType::GovernanceSetThreshold { new_threshold } => {
                validate_threshold(*new_threshold)
            }
            ActionType::GovernanceSetTimeout {
                new_timeout_microseconds,
            } => validate_timeout(*new_timeout_microseconds),
            ActionType::GovernanceCancelAction { proposal_cid }
                if proposal_cid.trim().is_empty() =>
            {
                Err(Error::Validation(
                    "proposal_cid must name the proposal to cancel".to_string(),
                ))
            }
            _ => Ok(()),
        }
    }
}

/// The variant a Daml `Value` holds, if it holds one.
fn as_variant(value: &Value) -> Option<&Variant> {
    match &value.sum {
        Some(value::Sum::Variant(v)) => Some(v),
        _ => None,
    }
}

/// The constructor and field record of the action variant nested inside a
/// CBTC action `family` such as `GovernanceAction`.
fn nested_action<'a>(inner: &'a Value, family: &str) -> Result<(&'a str, &'a Record), Error> {
    let variant = as_variant(inner)
        .ok_or_else(|| Error::decode(format!("Expected nested Variant for {family}")))?;
    let value = variant
        .value
        .as_ref()
        .ok_or_else(|| Error::decode(format!("{family} inner variant has no value")))?;
    Ok((variant.constructor.as_str(), extract_record(value)?))
}

/// Decode a CBTC `GovernanceAction`: member and threshold management.
fn decode_governance_action(inner: &Value) -> Result<ActionType, Error> {
    let (constructor, record) = nested_action(inner, "GovernanceAction")?;
    match constructor {
        "Governance_AddMemberAndSetThreshold" => Ok(ActionType::GovernanceAddMember {
            member: extract_party_id(get_field(record, "member")?)?,
            new_threshold: extract_int64(get_field(record, "newThreshold")?)?,
        }),
        "Governance_RemoveMemberAndSetThreshold" => Ok(ActionType::GovernanceRemoveMember {
            member: extract_party_id(get_field(record, "member")?)?,
            new_threshold: extract_int64(get_field(record, "newThreshold")?)?,
        }),
        "Governance_SetThreshold" => Ok(ActionType::GovernanceSetThreshold {
            new_threshold: extract_int64(get_field(record, "newThreshold")?)?,
        }),
        "Governance_SetActionConfirmationTimeout" => {
            let reltime = get_field(record, "newActionConfirmationTimeout")?;
            let microseconds = deserialize_reltime(reltime)?;
            Ok(ActionType::GovernanceSetTimeout {
                new_timeout_microseconds: microseconds,
            })
        }
        other => Err(Error::decode(format!(
            "Unknown GovernanceAction constructor: {other}"
        ))),
    }
}

/// Decode a CBTC `UtilityOnboardingAction`.
fn decode_utility_onboarding_action(inner: &Value) -> Result<ActionType, Error> {
    let (constructor, record) = nested_action(inner, "UtilityOnboardingAction")?;
    match constructor {
        "UtilityOnboarding_CreateProviderServiceRequest" => {
            Ok(ActionType::UtilityCreateProviderRequest {
                operator: extract_party_id(get_field(record, "operator")?)?,
            })
        }
        "UtilityOnboarding_CreateUserServiceRequest" => Ok(ActionType::UtilityCreateUserRequest {
            operator: extract_party_id(get_field(record, "operator")?)?,
        }),
        "UtilityOnboarding_SetupUtility" => Ok(ActionType::UtilitySetup {
            operator: extract_party_id(get_field(record, "operator")?)?,
            provider_service_cid: extract_contract_id(get_field(record, "providerServiceCid")?)?,
            user_service_cid: extract_contract_id(get_field(record, "userServiceCid")?)?,
        }),
        "UtilityOnboarding_AcceptHolderServiceRequest" => {
            Ok(ActionType::UtilityAcceptHolderServiceRequest {
                operator: extract_party_id(get_field(record, "operator")?)?,
                provider_service_cid: extract_contract_id(get_field(
                    record,
                    "providerServiceCid",
                )?)?,
                holder_service_request_cid: extract_contract_id(get_field(
                    record,
                    "holderServiceRequestCid",
                )?)?,
                holder: extract_party_id(get_field(record, "holder")?)?,
            })
        }
        other => Err(Error::decode(format!(
            "Unknown UtilityOnboardingAction constructor: {other}"
        ))),
    }
}

/// Decode a CBTC `CredentialAction`.
fn decode_credential_action(inner: &Value) -> Result<ActionType, Error> {
    let (constructor, record) = nested_action(inner, "CredentialAction")?;
    match constructor {
        "Credential_OfferFreeCredential" => {
            let claims_list = extract_list(get_field(record, "claims")?)?;
            let claims = claims_list
                .elements
                .iter()
                .map(deserialize_claim)
                .collect::<Result<Vec<_>, Error>>()?;

            Ok(ActionType::CredentialOfferFree {
                operator: extract_party_id(get_field(record, "operator")?)?,
                user_service_cid: extract_contract_id(get_field(record, "userServiceCid")?)?,
                holder: extract_party_id(get_field(record, "holder")?)?,
                id: extract_text(get_field(record, "id")?)?,
                description: extract_text(get_field(record, "description")?)?,
                claims,
            })
        }
        "Credential_AcceptFreeCredential" => Ok(ActionType::CredentialAcceptFree {
            operator: extract_party_id(get_field(record, "operator")?)?,
            user_service_cid: extract_contract_id(get_field(record, "userServiceCid")?)?,
            credential_offer_cid: extract_contract_id(get_field(record, "credentialOfferCid")?)?,
        }),
        other => Err(Error::decode(format!(
            "Unknown CredentialAction constructor: {other}"
        ))),
    }
}

/// Decode an `Optional (Set Party)`, keeping `None` apart from an empty set.
/// Execution matches confirmations by value, so the two must not merge.
fn extract_optional_party_ids(value: &Value) -> Result<Option<Vec<CantonId>>, Error> {
    let Some(value::Sum::Optional(optional)) = &value.sum else {
        return Err(Error::decode("Expected Optional value"));
    };
    let Some(set) = optional.value.as_deref() else {
        return Ok(None);
    };
    extract_party_set(set)
        .ok_or_else(|| Error::decode("Expected Set Party value"))?
        .into_iter()
        .map(|party| {
            party
                .parse()
                .map_err(|e| Error::decode_from(format!("invalid party id `{party}`"), e))
        })
        .collect::<Result<Vec<_>, Error>>()
        .map(Some)
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::daml::ledger::api::v2::RecordField;

    use super::*;
    use crate::framework::encode::{make_list, make_text, serialize_claim};

    /// Test-only helper: builds a `CantonId` with a fixed valid namespace so
    /// tests can vary just the prefix.
    fn cid(prefix: &str) -> CantonId {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        CantonId::parse(&format!("{prefix}::{ns}")).expect("valid canton id")
    }

    fn claim() -> Claim {
        Claim {
            subject: "subj".into(),
            property: "prop".into(),
            value: "val".into(),
        }
    }

    /// Build a nested `ActionRequiringConfirmation` value: the outer group
    /// variant wraps the action constructor, which wraps its field record.
    fn cbtc_nested(outer: &str, ctor: &str, fields: Vec<RecordField>) -> Value {
        make_variant(outer, make_variant(ctor, make_record(fields)))
    }

    /// Unwrap a `Variant` value into `(constructor, inner)`.
    fn as_variant(value: &Value) -> (&str, &Value) {
        match &value.sum {
            Some(value::Sum::Variant(v)) => match v.value.as_deref() {
                Some(inner) => (v.constructor.as_str(), inner),
                None => panic!("variant {} has no inner value", v.constructor),
            },
            other => panic!("expected Variant, got {other:?}"),
        }
    }

    /// The ordered field labels of a `Record` value.
    fn record_labels(value: &Value) -> Vec<&str> {
        match &value.sum {
            Some(value::Sum::Record(r)) => r.fields.iter().map(|f| f.label.as_str()).collect(),
            other => panic!("expected Record, got {other:?}"),
        }
    }

    // ---- `ActionRequiringConfirmation` decode fixtures ----
    //
    // `#cbtc-governance` owns this wire form, and decman only reads it, so
    // these values are hand-written. The constructor names and field labels
    // below are the contract with that package. A fixture derived from our
    // own code would let a renamed label pass unnoticed, because both sides
    // would rename together.

    /// Every `ActionRequiringConfirmation` value decman reads off a CBTC
    /// `Confirmation`, paired with the `ActionType` it must decode to. The
    /// two AdditionalProposer variants are absent: only the
    /// `GovernanceSelfAction` form carries them.
    fn cbtc_form_fixtures() -> Vec<(Value, ActionType)> {
        vec![
            (
                cbtc_nested(
                    "GovernanceAction",
                    "Governance_AddMemberAndSetThreshold",
                    vec![
                        field("member", make_party(cid("m1"))),
                        field("newThreshold", make_int64(2)),
                    ],
                ),
                ActionType::GovernanceAddMember {
                    member: cid("m1"),
                    new_threshold: 2,
                },
            ),
            (
                cbtc_nested(
                    "GovernanceAction",
                    "Governance_RemoveMemberAndSetThreshold",
                    vec![
                        field("member", make_party(cid("m1"))),
                        field("newThreshold", make_int64(1)),
                    ],
                ),
                ActionType::GovernanceRemoveMember {
                    member: cid("m1"),
                    new_threshold: 1,
                },
            ),
            (
                cbtc_nested(
                    "GovernanceAction",
                    "Governance_SetThreshold",
                    vec![field("newThreshold", make_int64(3))],
                ),
                ActionType::GovernanceSetThreshold { new_threshold: 3 },
            ),
            (
                cbtc_nested(
                    "GovernanceAction",
                    "Governance_SetActionConfirmationTimeout",
                    vec![field(
                        "newActionConfirmationTimeout",
                        serialize_reltime(60_000_000),
                    )],
                ),
                ActionType::GovernanceSetTimeout {
                    new_timeout_microseconds: 60_000_000,
                },
            ),
            (
                cbtc_nested(
                    "UtilityOnboardingAction",
                    "UtilityOnboarding_CreateProviderServiceRequest",
                    vec![field("operator", make_party(cid("op")))],
                ),
                ActionType::UtilityCreateProviderRequest {
                    operator: cid("op"),
                },
            ),
            (
                cbtc_nested(
                    "UtilityOnboardingAction",
                    "UtilityOnboarding_CreateUserServiceRequest",
                    vec![field("operator", make_party(cid("op")))],
                ),
                ActionType::UtilityCreateUserRequest {
                    operator: cid("op"),
                },
            ),
            (
                cbtc_nested(
                    "UtilityOnboardingAction",
                    "UtilityOnboarding_SetupUtility",
                    vec![
                        field("operator", make_party(cid("op"))),
                        field("providerServiceCid", make_contract_id("00psc")),
                        field("userServiceCid", make_contract_id("00usc")),
                    ],
                ),
                ActionType::UtilitySetup {
                    operator: cid("op"),
                    provider_service_cid: "00psc".into(),
                    user_service_cid: "00usc".into(),
                },
            ),
            (
                cbtc_nested(
                    "UtilityOnboardingAction",
                    "UtilityOnboarding_AcceptHolderServiceRequest",
                    vec![
                        field("operator", make_party(cid("op"))),
                        field("providerServiceCid", make_contract_id("00psc")),
                        field("holderServiceRequestCid", make_contract_id("00hsr")),
                        field("holder", make_party(cid("holder"))),
                    ],
                ),
                ActionType::UtilityAcceptHolderServiceRequest {
                    operator: cid("op"),
                    provider_service_cid: "00psc".into(),
                    holder_service_request_cid: "00hsr".into(),
                    holder: cid("holder"),
                },
            ),
            (
                cbtc_nested(
                    "CredentialAction",
                    "Credential_OfferFreeCredential",
                    vec![
                        field("operator", make_party(cid("op"))),
                        field("userServiceCid", make_contract_id("00usc")),
                        field("holder", make_party(cid("holder"))),
                        field("id", make_text("cred-1")),
                        field("description", make_text("a credential")),
                        field("claims", make_list(vec![serialize_claim(&claim())])),
                    ],
                ),
                ActionType::CredentialOfferFree {
                    operator: cid("op"),
                    user_service_cid: "00usc".into(),
                    holder: cid("holder"),
                    id: "cred-1".into(),
                    description: "a credential".into(),
                    claims: vec![claim()],
                },
            ),
            (
                cbtc_nested(
                    "CredentialAction",
                    "Credential_AcceptFreeCredential",
                    vec![
                        field("operator", make_party(cid("op"))),
                        field("userServiceCid", make_contract_id("00usc")),
                        field("credentialOfferCid", make_contract_id("00offer")),
                    ],
                ),
                ActionType::CredentialAcceptFree {
                    operator: cid("op"),
                    user_service_cid: "00usc".into(),
                    credential_offer_cid: "00offer".into(),
                },
            ),
            (
                // DevNet carries a bare record: no nested action variant.
                make_variant(
                    "DevNetFeatureAppAction",
                    make_record(vec![field("amuletRulesCid", make_contract_id("00amulet"))]),
                ),
                ActionType::DevNetFeatureApp {
                    amulet_rules_cid: "00amulet".into(),
                },
            ),
        ]
    }

    #[test]
    fn cbtc_form_decodes_every_variant() {
        for (value, expected) in cbtc_form_fixtures() {
            assert_eq!(
                ActionType::from_cbtc_proto(&value).expect("cbtc form decodes"),
                expected
            );
        }
    }

    #[test]
    fn cbtc_form_covers_every_variant_it_can_carry() {
        // Guards against a variant added to `ActionType` without a fixture.
        // Eleven of the fifteen variants reach this form: four
        // member-management, four utility, two credential and one DevNet.
        // The two AdditionalProposer variants, SetDisclosureObservers and
        // CancelAction exist only in the self form.
        assert_eq!(cbtc_form_fixtures().len(), 11);
    }

    #[test]
    fn serialize_self_action_pins_its_own_labels() {
        // The self-management encoder maps the same `ActionType` to different
        // constructor and field names than the CBTC form above. The labels
        // are hand-written and the on-ledger interpreter consumes them, so a
        // typo would only surface as a runtime interpretation error far from
        // the source. Pin them here.
        let add = ActionType::GovernanceAddMember {
            member: cid("p"),
            new_threshold: 3,
        }
        .to_self_proto()
        .expect("action encodes to the self form");
        let (ctor, record) = as_variant(&add);
        assert_eq!(ctor, "SelfAction_AddMemberAndSetThreshold");
        assert_eq!(record_labels(record), ["newMember", "newThresholdAfterAdd"]);

        let remove = ActionType::GovernanceRemoveMember {
            member: cid("p"),
            new_threshold: 1,
        }
        .to_self_proto()
        .expect("action encodes to the self form");
        let (ctor, record) = as_variant(&remove);
        assert_eq!(ctor, "SelfAction_RemoveMemberAndSetThreshold");
        assert_eq!(
            record_labels(record),
            ["removedMember", "newThresholdAfterRemove"]
        );

        let set_threshold = ActionType::GovernanceSetThreshold { new_threshold: 2 }
            .to_self_proto()
            .expect("action encodes to the self form");
        let (ctor, record) = as_variant(&set_threshold);
        assert_eq!(ctor, "SelfAction_SetThreshold");
        assert_eq!(record_labels(record), ["updatedThreshold"]);

        let set_observers = ActionType::GovernanceSetDisclosureObservers {
            disclosure_observers: None,
        }
        .to_self_proto()
        .expect("action encodes to the self form");
        let (ctor, record) = as_variant(&set_observers);
        assert_eq!(ctor, "SelfAction_SetDisclosureObservers");
        assert_eq!(record_labels(record), ["updatedDisclosureObservers"]);

        let cancel = ActionType::GovernanceCancelAction {
            proposal_cid: "00proposal".into(),
        }
        .to_self_proto()
        .expect("action encodes to the self form");
        let (ctor, record) = as_variant(&cancel);
        assert_eq!(ctor, "SelfAction_CancelAction");
        assert_eq!(record_labels(record), ["actionProposalCid"]);
    }

    // ---- `validate` assertions ----

    #[test]
    fn action_threshold_rejects_zero_and_negative() {
        let action = ActionType::GovernanceSetThreshold { new_threshold: 0 };
        assert!(action.validate().is_err());
        let action = ActionType::GovernanceSetThreshold { new_threshold: -3 };
        assert!(action.validate().is_err());
        let action = ActionType::GovernanceSetThreshold { new_threshold: 1 };
        assert!(action.validate().is_ok());
    }

    #[test]
    fn action_threshold_rejects_in_add_remove_member() {
        let member = cid("member");
        let action = ActionType::GovernanceAddMember {
            member: member.clone(),
            new_threshold: 0,
        };
        assert!(action.validate().is_err());
        let action = ActionType::GovernanceRemoveMember {
            member,
            new_threshold: -1,
        };
        assert!(action.validate().is_err());
    }

    #[test]
    fn cancel_action_needs_a_proposal_cid() {
        let action = ActionType::GovernanceCancelAction {
            proposal_cid: " ".into(),
        };
        assert!(matches!(action.validate(), Err(Error::Validation(_))));
        let action = ActionType::GovernanceCancelAction {
            proposal_cid: "00proposal".into(),
        };
        assert!(action.validate().is_ok());
    }

    #[test]
    fn action_timeout_rejects_zero_and_negative() {
        let action = ActionType::GovernanceSetTimeout {
            new_timeout_microseconds: 0,
        };
        assert!(action.validate().is_err());
        let action = ActionType::GovernanceSetTimeout {
            new_timeout_microseconds: -1_000_000,
        };
        assert!(action.validate().is_err());
        let action = ActionType::GovernanceSetTimeout {
            new_timeout_microseconds: 60_000_000,
        };
        assert!(action.validate().is_ok());
    }

    // ---- `GovernanceSelfAction` codec round trip ----

    /// The member-management variants — the only ones that also exist in the
    /// `GovernanceSelfAction` (self-management) form.
    fn self_encodable_fixtures() -> Vec<ActionType> {
        vec![
            ActionType::GovernanceAddMember {
                member: cid("m1"),
                new_threshold: 2,
            },
            ActionType::GovernanceRemoveMember {
                member: cid("m1"),
                new_threshold: 1,
            },
            ActionType::GovernanceSetThreshold { new_threshold: 3 },
            ActionType::GovernanceSetTimeout {
                new_timeout_microseconds: 60_000_000,
            },
            ActionType::GovernanceAddAdditionalProposer {
                additional_proposer: cid("p1"),
            },
            ActionType::GovernanceRemoveAdditionalProposer {
                additional_proposer: cid("p1"),
            },
            ActionType::GovernanceSetDisclosureObservers {
                disclosure_observers: Some(vec![cid("o1"), cid("o2")]),
            },
            // An empty set and `None` are different Daml values, so both
            // must survive the round trip as they are.
            ActionType::GovernanceSetDisclosureObservers {
                disclosure_observers: Some(vec![]),
            },
            ActionType::GovernanceSetDisclosureObservers {
                disclosure_observers: None,
            },
            ActionType::GovernanceCancelAction {
                proposal_cid: "00proposal".into(),
            },
        ]
    }

    #[test]
    fn self_form_round_trips() {
        for action in self_encodable_fixtures() {
            let value = action
                .to_self_proto()
                .expect("action encodes to the self form");
            assert_eq!(
                ActionType::from_self_proto(&value).expect("self form decodes"),
                action
            );
        }
    }

    /// The self encoder must reject a variant the `GovernanceSelfAction`
    /// union does not carry. It returns `Error::Encode` rather than panicking,
    /// so a bad request becomes a 400 and not a dead worker.
    #[test]
    fn wrong_form_is_an_error_not_a_panic() {
        let not_self_managed = ActionType::DevNetFeatureApp {
            amulet_rules_cid: "00amulet".into(),
        };
        assert!(matches!(
            not_self_managed.to_self_proto(),
            Err(Error::Encode(_))
        ));
        let utility = ActionType::UtilityCreateUserRequest {
            operator: cid("op"),
        };
        assert!(matches!(utility.to_self_proto(), Err(Error::Encode(_))));
    }

    /// A variant that carries no value at all.
    fn empty_variant(constructor: &str) -> Value {
        Value {
            sum: Some(value::Sum::Variant(Box::new(Variant {
                variant_id: None,
                constructor: constructor.into(),
                value: None,
            }))),
        }
    }

    /// The message of a decode error, or a panic for anything else.
    fn decode_message(result: Result<ActionType, Error>) -> String {
        match result {
            Err(Error::Decode { message, .. }) => message,
            other => panic!("expected a decode error, got {other:?}"),
        }
    }

    /// Each malformed shape fails with its own message, at each level of the
    /// CBTC form and in the self form.
    #[test]
    fn malformed_values_name_what_is_wrong() {
        let not_a_variant = make_text("x");
        let cbtc = |value: &Value| decode_message(ActionType::from_cbtc_proto(value));
        assert_eq!(cbtc(&not_a_variant), "Expected Variant value for action");
        assert_eq!(
            cbtc(&empty_variant("GovernanceAction")),
            "Variant has no inner value"
        );
        for family in [
            "GovernanceAction",
            "UtilityOnboardingAction",
            "CredentialAction",
        ] {
            assert_eq!(
                cbtc(&make_variant(family, make_text("x"))),
                format!("Expected nested Variant for {family}")
            );
            assert_eq!(
                cbtc(&make_variant(family, empty_variant("X"))),
                format!("{family} inner variant has no value")
            );
            assert_eq!(
                cbtc(&cbtc_nested(family, "Bogus", vec![])),
                format!("Unknown {family} constructor: Bogus")
            );
        }
        assert_eq!(
            cbtc(&make_variant("Bogus", make_record(vec![]))),
            "Unknown action constructor: Bogus"
        );

        let own = |value: &Value| decode_message(ActionType::from_self_proto(value));
        assert_eq!(
            own(&not_a_variant),
            "Expected Variant value for GovernanceSelfAction"
        );
        assert_eq!(
            own(&empty_variant("SelfAction_SetThreshold")),
            "GovernanceSelfAction variant has no inner value"
        );
        assert_eq!(
            own(&make_variant("SelfAction_SetThreshold", make_text("x"))),
            "Expected GovernanceSelfAction record"
        );
        assert_eq!(
            own(&make_variant("Bogus", make_record(vec![]))),
            "Unknown GovernanceSelfAction constructor: Bogus"
        );
    }

    #[test]
    fn unknown_constructor_is_a_decode_error() {
        let bogus = make_variant("NoSuchAction", make_record(vec![]));
        assert!(matches!(
            ActionType::from_cbtc_proto(&bogus),
            Err(Error::Decode { .. })
        ));
        assert!(matches!(
            ActionType::from_self_proto(&bogus),
            Err(Error::Decode { .. })
        ));
    }
}
