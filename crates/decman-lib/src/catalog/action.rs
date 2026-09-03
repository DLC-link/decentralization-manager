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

use canton_proto_rs::com::daml::ledger::api::v2::{Value, value};
use common::api::Claim;
use common::canton_id::CantonId;

use crate::catalog::types::{deserialize_claim, deserialize_reltime};
use crate::error::Error;
use crate::framework::encode::{
    field, make_int64, make_party, make_record, make_variant, serialize_reltime,
};
use crate::framework::record::{
    extract_contract_id, extract_int64, extract_list, extract_party_id, extract_record,
    extract_text, get_field,
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
    /// Maps the six member-management `ActionType` variants to the
    /// governance-core `GovernanceSelfAction` enum (different field names).
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
        let variant = match &value.sum {
            Some(value::Sum::Variant(v)) => v,
            _ => {
                return Err(Error::Decode(
                    "Expected Variant value for action".to_string(),
                ));
            }
        };

        let inner = variant
            .value
            .as_ref()
            .ok_or_else(|| Error::Decode("Variant has no inner value".to_string()))?;

        match variant.constructor.as_str() {
            // Governance Actions - nested variant structure
            "GovernanceAction" => {
                let inner_variant = match &inner.sum {
                    Some(value::Sum::Variant(v)) => v,
                    _ => {
                        return Err(Error::Decode(
                            "Expected nested Variant for GovernanceAction".to_string(),
                        ));
                    }
                };
                let inner_value = inner_variant.value.as_ref().ok_or_else(|| {
                    Error::Decode("GovernanceAction inner variant has no value".to_string())
                })?;
                let record = extract_record(inner_value)?;

                match inner_variant.constructor.as_str() {
                    "Governance_AddMemberAndSetThreshold" => Ok(ActionType::GovernanceAddMember {
                        member: extract_party_id(get_field(record, "member")?)?,
                        new_threshold: extract_int64(get_field(record, "newThreshold")?)?,
                    }),
                    "Governance_RemoveMemberAndSetThreshold" => {
                        Ok(ActionType::GovernanceRemoveMember {
                            member: extract_party_id(get_field(record, "member")?)?,
                            new_threshold: extract_int64(get_field(record, "newThreshold")?)?,
                        })
                    }
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
                    other => Err(Error::Decode(format!(
                        "Unknown GovernanceAction constructor: {other}"
                    ))),
                }
            }

            // Utility Onboarding Actions - nested variant structure
            "UtilityOnboardingAction" => {
                let inner_variant = match &inner.sum {
                    Some(value::Sum::Variant(v)) => v,
                    _ => {
                        return Err(Error::Decode(
                            "Expected nested Variant for UtilityOnboardingAction".to_string(),
                        ));
                    }
                };
                let inner_value = inner_variant.value.as_ref().ok_or_else(|| {
                    Error::Decode("UtilityOnboardingAction inner variant has no value".to_string())
                })?;
                let record = extract_record(inner_value)?;

                match inner_variant.constructor.as_str() {
                    "UtilityOnboarding_CreateProviderServiceRequest" => {
                        Ok(ActionType::UtilityCreateProviderRequest {
                            operator: extract_party_id(get_field(record, "operator")?)?,
                        })
                    }
                    "UtilityOnboarding_CreateUserServiceRequest" => {
                        Ok(ActionType::UtilityCreateUserRequest {
                            operator: extract_party_id(get_field(record, "operator")?)?,
                        })
                    }
                    "UtilityOnboarding_SetupUtility" => Ok(ActionType::UtilitySetup {
                        operator: extract_party_id(get_field(record, "operator")?)?,
                        provider_service_cid: extract_contract_id(get_field(
                            record,
                            "providerServiceCid",
                        )?)?,
                        user_service_cid: extract_contract_id(get_field(
                            record,
                            "userServiceCid",
                        )?)?,
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
                    other => Err(Error::Decode(format!(
                        "Unknown UtilityOnboardingAction constructor: {other}"
                    ))),
                }
            }

            // Credential Actions - nested variant structure
            "CredentialAction" => {
                let inner_variant = match &inner.sum {
                    Some(value::Sum::Variant(v)) => v,
                    _ => {
                        return Err(Error::Decode(
                            "Expected nested Variant for CredentialAction".to_string(),
                        ));
                    }
                };
                let inner_value = inner_variant.value.as_ref().ok_or_else(|| {
                    Error::Decode("CredentialAction inner variant has no value".to_string())
                })?;
                let record = extract_record(inner_value)?;

                match inner_variant.constructor.as_str() {
                    "Credential_OfferFreeCredential" => {
                        let claims_list = extract_list(get_field(record, "claims")?)?;
                        let claims = claims_list
                            .elements
                            .iter()
                            .map(deserialize_claim)
                            .collect::<Result<Vec<_>, Error>>()?;

                        Ok(ActionType::CredentialOfferFree {
                            operator: extract_party_id(get_field(record, "operator")?)?,
                            user_service_cid: extract_contract_id(get_field(
                                record,
                                "userServiceCid",
                            )?)?,
                            holder: extract_party_id(get_field(record, "holder")?)?,
                            id: extract_text(get_field(record, "id")?)?,
                            description: extract_text(get_field(record, "description")?)?,
                            claims,
                        })
                    }
                    "Credential_AcceptFreeCredential" => Ok(ActionType::CredentialAcceptFree {
                        operator: extract_party_id(get_field(record, "operator")?)?,
                        user_service_cid: extract_contract_id(get_field(
                            record,
                            "userServiceCid",
                        )?)?,
                        credential_offer_cid: extract_contract_id(get_field(
                            record,
                            "credentialOfferCid",
                        )?)?,
                    }),
                    other => Err(Error::Decode(format!(
                        "Unknown CredentialAction constructor: {other}"
                    ))),
                }
            }

            // DevNet
            "DevNetFeatureAppAction" => {
                let record = extract_record(inner)?;
                Ok(ActionType::DevNetFeatureApp {
                    amulet_rules_cid: extract_contract_id(get_field(record, "amuletRulesCid")?)?,
                })
            }

            other => Err(Error::Decode(format!(
                "Unknown action constructor: {other}"
            ))),
        }
    }

    /// Deserialize a `GovernanceSelfAction` Daml variant to an `ActionType`.
    pub fn from_self_proto(value: &Value) -> Result<Self, Error> {
        let variant = match &value.sum {
            Some(value::Sum::Variant(v)) => v,
            _ => {
                return Err(Error::Decode(
                    "Expected Variant value for GovernanceSelfAction".to_string(),
                ));
            }
        };

        let inner = variant.value.as_ref().ok_or_else(|| {
            Error::Decode("GovernanceSelfAction variant has no inner value".to_string())
        })?;

        let record = extract_record(inner)
            .map_err(|_| Error::Decode("Expected GovernanceSelfAction record".to_string()))?;
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
            other => Err(Error::Decode(format!(
                "Unknown GovernanceSelfAction constructor: {other}"
            ))),
        }
    }

    /// True for the six `GovernanceSelfAction` variants — the only actions the
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
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::daml::ledger::api::v2::RecordField;

    use super::*;
    use crate::framework::encode::{make_contract_id, make_list, make_text, serialize_claim};

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
        // Eleven of the thirteen variants reach this form: four
        // member-management, four utility, two credential and one DevNet.
        // The two AdditionalProposer variants exist only in the self form.
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

    /// The six member-management variants — the only ones that also exist in
    /// the `GovernanceSelfAction` (self-management) form.
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

    #[test]
    fn unknown_constructor_is_a_decode_error() {
        let bogus = make_variant("NoSuchAction", make_record(vec![]));
        assert!(matches!(
            ActionType::from_cbtc_proto(&bogus),
            Err(Error::Decode(_))
        ));
        assert!(matches!(
            ActionType::from_self_proto(&bogus),
            Err(Error::Decode(_))
        ));
    }
}
