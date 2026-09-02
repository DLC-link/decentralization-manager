//! `governance-rewards` proposal payloads.

use canton_proto_rs::com::daml::ledger::api::v2::{Value, value};
use common::canton_id::CantonId;

use crate::catalog::types::{RewardBeneficiary, serialize_reward_beneficiary};
use crate::error::Error;
use crate::framework::encode::{
    field, make_contract_id, make_int64, make_list, make_optional_contract_id, make_party,
    make_record, make_text,
};
use crate::framework::validate::{validate_future_micros, validate_reward_beneficiaries};
use crate::framework::{
    DamlProtoEncode, PackageResolver, TemplateId, TemplateInfo, Validate, ValidationCtx,
};

/// Create (or replace) the decparty's on-ledger CouponReassignmentDelegation.
/// `prior_delegation` is the cid of the delegation being replaced (None for the first).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct SetupCouponReassignmentDelegation {
    /// The DSO whose coupons the delegation may assign. Fixed by this vote
    /// so the automation can tell the decparty's real coupons from ones a
    /// stranger minted naming itself `dso`.
    pub dso: CantonId,
    pub assigners: Vec<CantonId>,
    /// The split, baked into the delegation and enforced in DAML. Two
    /// things surprise proposers, and both reject the vote at execute:
    ///
    /// 1. The percentages must sum to **exactly** 1.0, compared as exact
    ///    Decimal. An even 3-way split is therefore not expressible —
    ///    `0.3333333333` three times is not 1.0. Balance the last entry by
    ///    hand (`0.3333333333`, `0.3333333333`, `0.3333333334`).
    /// 2. Nothing is implicitly left to the decparty. To keep a remainder
    ///    for itself, the decparty must appear here as its own beneficiary
    ///    with an explicit percentage.
    pub new_beneficiaries: Vec<RewardBeneficiary>,
    #[serde(default)]
    pub prior_delegation: Option<String>,
}

impl SetupCouponReassignmentDelegation {
    pub const MODULE: &'static str = "Governance.Rewards.SetupCouponReassignmentDelegation";
    pub const ENTITY: &'static str = "SetupCouponReassignmentDelegation";
}

impl TemplateInfo for SetupCouponReassignmentDelegation {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_rewards")
            .ok_or(Error::PackageNotConfigured("governance_rewards"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for SetupCouponReassignmentDelegation {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field(
                "priorDelegation",
                make_optional_contract_id(&self.prior_delegation),
            ),
            field("dso", make_party(&self.dso)),
            field(
                "assigners",
                make_list(self.assigners.iter().map(make_party).collect()),
            ),
            field(
                "beneficiaries",
                make_list(
                    self.new_beneficiaries
                        .iter()
                        .map(serialize_reward_beneficiary)
                        .collect(),
                ),
            ),
        ]))
    }
}

impl Validate for SetupCouponReassignmentDelegation {
    fn validate(&self, _ctx: &ValidationCtx) -> Result<(), Error> {
        if self.assigners.is_empty() {
            return Err(Error::Validation("assigners must not be empty".to_string()));
        }
        let mut seen = std::collections::HashSet::new();
        for a in &self.assigners {
            if !seen.insert(a) {
                return Err(Error::Validation(format!(
                    "duplicate assigner not allowed: {a}"
                )));
            }
        }
        validate_reward_beneficiaries(&self.new_beneficiaries)
    }
}

/// Revoke (archive) the decparty's CouponReassignmentDelegation.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct RevokeCouponReassignmentDelegation {
    pub delegation: String,
}

impl RevokeCouponReassignmentDelegation {
    pub const MODULE: &'static str = "Governance.Rewards.RevokeCouponReassignmentDelegation";
    pub const ENTITY: &'static str = "RevokeCouponReassignmentDelegation";
}

impl TemplateInfo for RevokeCouponReassignmentDelegation {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_rewards")
            .ok_or(Error::PackageNotConfigured("governance_rewards"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for RevokeCouponReassignmentDelegation {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![field(
            "delegation",
            make_contract_id(&self.delegation),
        )]))
    }
}

impl Validate for RevokeCouponReassignmentDelegation {
    fn validate(&self, _ctx: &ValidationCtx) -> Result<(), Error> {
        if self.delegation.trim().is_empty() {
            return Err(Error::Validation(
                "delegation must not be empty".to_string(),
            ));
        }
        Ok(())
    }
}

/// Delegate minting of the governance party's CIP-104 reward coupons to a
/// validator node's `delegate` party via a `MintingDelegationProposal`.
/// The delegation beneficiary is always the governance party; the delegate
/// accepts the proposal out-of-band via the wallet API.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct SetupMintingDelegation {
    pub delegate: CantonId,
    pub dso: CantonId,
    /// Delegation expiry as microseconds since epoch.
    pub expires_at_micros: i64,
    /// Auto-merge target for the beneficiary's amulets. Must be positive.
    pub amulet_merge_limit: i64,
    pub description: String,
}

impl SetupMintingDelegation {
    pub const MODULE: &'static str = "Governance.Rewards.SetupMintingDelegation";
    pub const ENTITY: &'static str = "SetupMintingDelegation";
}

impl TemplateInfo for SetupMintingDelegation {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_rewards")
            .ok_or(Error::PackageNotConfigured("governance_rewards"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for SetupMintingDelegation {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field("delegate", make_party(&self.delegate)),
            field("dso", make_party(&self.dso)),
            field(
                "expiresAt",
                Value {
                    sum: Some(value::Sum::Timestamp(self.expires_at_micros)),
                },
            ),
            field("amuletMergeLimit", make_int64(self.amulet_merge_limit)),
            field("description", make_text(&self.description)),
        ]))
    }
}

impl Validate for SetupMintingDelegation {
    fn validate(&self, ctx: &ValidationCtx) -> Result<(), Error> {
        if self.amulet_merge_limit <= 0 {
            return Err(Error::Validation(
                "amulet_merge_limit must be greater than 0".to_string(),
            ));
        }
        validate_future_micros(self.expires_at_micros, ctx.now_micros, "expires_at_micros")
    }
}

/// Accept a validator-created `ExternalPartySetupProposal` on behalf of the
/// governance party, creating its `ValidatorRight` + `TransferPreapproval`.
/// This is the missing prerequisite that makes the validator's built-in
/// `MintingDelegationCollectRewardsTrigger` start collecting the party's
/// CIP-104 reward coupons via the established `MintingDelegation`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AcceptExternalPartySetup {
    /// Contract id of the ExternalPartySetupProposal to accept (from the
    /// validator's POST /v0/admin/external-party/setup-proposal).
    pub proposal_cid: String,
}

impl AcceptExternalPartySetup {
    pub const MODULE: &'static str = "Governance.Rewards.AcceptExternalPartySetup";
    pub const ENTITY: &'static str = "AcceptExternalPartySetup";
}

impl TemplateInfo for AcceptExternalPartySetup {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_rewards")
            .ok_or(Error::PackageNotConfigured("governance_rewards"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for AcceptExternalPartySetup {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field("proposalCid", make_contract_id(&self.proposal_cid)),
            field(
                "description",
                make_text(&format!(
                    "Accept external party setup (ValidatorRight + TransferPreapproval) for proposal {}",
                    self.proposal_cid
                )),
            ),
        ]))
    }
}

impl Validate for AcceptExternalPartySetup {
    fn validate(&self, _ctx: &ValidationCtx) -> Result<(), Error> {
        if self.proposal_cid.trim().is_empty() {
            return Err(Error::Validation(
                "proposal_cid must not be empty".to_string(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Any valid `CantonId` — the exact value is irrelevant to these
    /// encode-shape snapshots.
    fn cid(prefix: &str) -> CantonId {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        CantonId::parse(&format!("{prefix}::{ns}")).unwrap()
    }

    fn ctx(governance_party: &CantonId, now_micros: i64) -> ValidationCtx<'_> {
        ValidationCtx {
            governance_party,
            now_micros,
        }
    }

    #[test]
    fn setup_delegation_validate() {
        let gov = cid("gov");
        let execs = vec![cid("m1"), cid("m2")];
        let ok = SetupCouponReassignmentDelegation {
            dso: cid("dso"),
            assigners: execs.clone(),
            new_beneficiaries: vec![
                RewardBeneficiary {
                    beneficiary: cid("a"),
                    percentage: "0.8".parse().expect("valid decimal"),
                },
                RewardBeneficiary {
                    beneficiary: cid("b"),
                    percentage: "0.2".parse().expect("valid decimal"),
                },
            ],
            prior_delegation: None,
        };
        assert!(ok.validate(&ctx(&gov, 0)).is_ok());

        let no_exec = SetupCouponReassignmentDelegation {
            dso: cid("dso"),
            assigners: vec![],
            new_beneficiaries: vec![RewardBeneficiary {
                beneficiary: cid("a"),
                percentage: "1.0".parse().expect("valid decimal"),
            }],
            prior_delegation: None,
        };
        assert!(no_exec.validate(&ctx(&gov, 0)).is_err());

        let bad_sum = SetupCouponReassignmentDelegation {
            dso: cid("dso"),
            assigners: execs,
            new_beneficiaries: vec![RewardBeneficiary {
                beneficiary: cid("a"),
                percentage: "0.5".parse().expect("valid decimal"),
            }],
            prior_delegation: None,
        };
        assert!(bad_sum.validate(&ctx(&gov, 0)).is_err());

        let revoke = RevokeCouponReassignmentDelegation {
            delegation: "00abc".into(),
        };
        assert!(revoke.validate(&ctx(&gov, 0)).is_ok());

        // An empty delegation cid is rejected at the boundary (not left to fail
        // only at ledger submission).
        let revoke_empty = RevokeCouponReassignmentDelegation {
            delegation: "  ".into(),
        };
        assert!(revoke_empty.validate(&ctx(&gov, 0)).is_err());
    }

    fn minting_delegation(
        expires_at_micros: i64,
        amulet_merge_limit: i64,
    ) -> SetupMintingDelegation {
        SetupMintingDelegation {
            delegate: cid("delegate"),
            dso: cid("dso"),
            expires_at_micros,
            amulet_merge_limit,
            description: "test".to_string(),
        }
    }

    #[test]
    fn setup_minting_delegation_rejects_a_non_future_expiry() {
        let gov = cid("gov");
        let hour_micros = 3_600_000_000i64;
        // Fixed `now` computed once, here — the lib's `validate` itself never
        // reads the clock, it only compares against `ctx.now_micros`.
        let now = chrono::Utc::now().timestamp_micros();

        // An expiry in the future is the only accepted shape.
        assert!(
            minting_delegation(now + hour_micros, 10)
                .validate(&ctx(&gov, now))
                .is_ok()
        );

        // Zero and negative are the raw-caller mistakes the DAML assert would
        // otherwise catch only at execute time, after a full governance round.
        assert!(minting_delegation(0, 10).validate(&ctx(&gov, now)).is_err());
        assert!(
            minting_delegation(-1, 10)
                .validate(&ctx(&gov, now))
                .is_err()
        );

        // Positive but already past is the same waste, and `> 0` alone misses it.
        assert!(
            minting_delegation(now - hour_micros, 10)
                .validate(&ctx(&gov, now))
                .is_err()
        );

        // The pre-existing amulet_merge_limit guard still fires when the expiry
        // is valid, so the new arm did not displace it.
        assert!(
            minting_delegation(now + hour_micros, 0)
                .validate(&ctx(&gov, now))
                .is_err()
        );
    }

    #[test]
    fn encode_snapshots() {
        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_path(crate::catalog::proposals::SNAPSHOT_PATH);
        let _guard = settings.bind_to_scope();

        insta::assert_debug_snapshot!(
            "setup_coupon_reassignment_delegation_no_prior",
            SetupCouponReassignmentDelegation {
                dso: cid("dso"),
                assigners: vec![cid("m1"), cid("m2")],
                new_beneficiaries: vec![
                    RewardBeneficiary {
                        beneficiary: cid("a"),
                        percentage: "0.8".parse().expect("valid decimal"),
                    },
                    RewardBeneficiary {
                        beneficiary: cid("b"),
                        percentage: "0.2".parse().expect("valid decimal"),
                    },
                ],
                prior_delegation: None,
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "setup_coupon_reassignment_delegation_with_prior",
            SetupCouponReassignmentDelegation {
                dso: cid("dso"),
                assigners: vec![cid("m1"), cid("m2")],
                new_beneficiaries: vec![
                    RewardBeneficiary {
                        beneficiary: cid("a"),
                        percentage: "0.8".parse().expect("valid decimal"),
                    },
                    RewardBeneficiary {
                        beneficiary: cid("b"),
                        percentage: "0.2".parse().expect("valid decimal"),
                    },
                ],
                prior_delegation: Some("00old".to_string()),
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "revoke_coupon_reassignment_delegation",
            RevokeCouponReassignmentDelegation {
                delegation: "00abc".to_string(),
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "setup_minting_delegation",
            SetupMintingDelegation {
                delegate: cid("delegate"),
                dso: cid("dso"),
                expires_at_micros: 1_800_000_000_000_000,
                amulet_merge_limit: 10,
                description: "collect CIP-104 rewards".to_string(),
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "accept_external_party_setup",
            AcceptExternalPartySetup {
                proposal_cid: "00abc123".to_string(),
            }
            .to_daml_proto()
            .unwrap()
        );
    }
}
