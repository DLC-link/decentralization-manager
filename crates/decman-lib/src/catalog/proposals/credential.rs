//! `governance-utility-credential` proposal payloads.

use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::daml::ledger::api::v2::Value;
use common::api::Claim;
use common::canton_id::CantonId;

use crate::catalog::types::{
    AppTransferContextInput, BillingParams, serialize_app_transfer_context,
    serialize_billing_params,
};
use crate::error::Error;
use crate::framework::encode::{
    field, make_contract_id, make_list, make_optional_numeric, make_party, make_record, make_text,
    serialize_claim,
};
use crate::framework::validate::validate_positive_amount;
use crate::framework::{
    DamlProtoEncode, PackageResolver, TemplateId, TemplateInfo, Validate, ValidationCtx,
};

/// Offer a free credential to a holder via the governance party's
/// `UserService`. Wraps `UserService_OfferFreeCredential` from the
/// Utility Credential App.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct OfferFreeCredential {
    pub user_service_cid: String,
    pub holder: CantonId,
    pub id: String,
    pub description: String,
    pub claims: Vec<Claim>,
}

impl OfferFreeCredential {
    pub const MODULE: &'static str = "Governance.UtilityCredential.OfferFreeCredential";
    pub const ENTITY: &'static str = "OfferFreeCredential";
}

impl TemplateInfo for OfferFreeCredential {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_credential")
            .ok_or(Error::PackageNotConfigured("governance_utility_credential"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for OfferFreeCredential {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field("userServiceCid", make_contract_id(&self.user_service_cid)),
            field("holder", make_party(&self.holder)),
            field("id", make_text(&self.id)),
            field("description", make_text(&self.description)),
            field(
                "claims",
                make_list(self.claims.iter().map(serialize_claim).collect()),
            ),
        ]))
    }
}

impl Validate for OfferFreeCredential {}

/// Offer a paid credential to a holder via the governance party's
/// `UserService`. Wraps `UserService_OfferPaidCredential`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct OfferPaidCredential {
    pub user_service_cid: String,
    pub holder: CantonId,
    pub id: String,
    pub description: String,
    pub claims: Vec<Claim>,
    pub billing_params: BillingParams,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    #[cfg_attr(feature = "typegen", ts(as = "Option<String>"))]
    pub deposit_initial_amount_usd: Option<DamlDecimal>,
}

impl OfferPaidCredential {
    pub const MODULE: &'static str = "Governance.UtilityCredential.OfferPaidCredential";
    pub const ENTITY: &'static str = "OfferPaidCredential";
}

impl TemplateInfo for OfferPaidCredential {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_credential")
            .ok_or(Error::PackageNotConfigured("governance_utility_credential"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for OfferPaidCredential {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field("userServiceCid", make_contract_id(&self.user_service_cid)),
            field("holder", make_party(&self.holder)),
            field("id", make_text(&self.id)),
            field("description", make_text(&self.description)),
            field(
                "claims",
                make_list(self.claims.iter().map(serialize_claim).collect()),
            ),
            field(
                "billingParams",
                serialize_billing_params(&self.billing_params),
            ),
            field(
                "depositInitialAmountUsd",
                make_optional_numeric(&self.deposit_initial_amount_usd),
            ),
        ]))
    }
}

impl Validate for OfferPaidCredential {
    fn validate(&self, _ctx: &ValidationCtx) -> Result<(), Error> {
        validate_billing_params(&self.billing_params)?;
        if let Some(d) = &self.deposit_initial_amount_usd {
            validate_positive_amount(d, "deposit_initial_amount_usd")?;
        }
        Ok(())
    }
}

/// Mirrors `validateBillingParams` in `utility-credential-app-v0`, which the
/// `CredentialOffer` template checks in its `ensure` clause. Without this a
/// bad offer would pass the vote and then fail at execution.
fn validate_billing_params(params: &BillingParams) -> Result<(), Error> {
    validate_positive_amount(&params.fee_per_day_usd, "billing_params.fee_per_day_usd")?;
    if params.billing_period_minutes <= 0 {
        return Err(Error::Validation(format!(
            "billing_params.billing_period_minutes must be strictly positive, got {}",
            params.billing_period_minutes
        )));
    }
    validate_positive_amount(
        &params.deposit_target_amount_usd,
        "billing_params.deposit_target_amount_usd",
    )?;
    if let Some(weight) = &params.holder_activity_weight {
        let one: DamlDecimal = "1".parse().expect("'1' is a valid DamlDecimal");
        if weight.value() < DamlDecimal::ZERO.value() || weight.value() > one.value() {
            return Err(Error::Validation(format!(
                "billing_params.holder_activity_weight must be in [0.0, 1.0], got {weight}"
            )));
        }
    }
    Ok(())
}

/// Accept a free credential offered to the governance party. Wraps
/// `UserService_AcceptFreeCredentialOffer`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AcceptFreeCredential {
    pub user_service_cid: String,
    pub credential_offer_cid: String,
}

impl AcceptFreeCredential {
    pub const MODULE: &'static str = "Governance.UtilityCredential.AcceptFreeCredential";
    pub const ENTITY: &'static str = "AcceptFreeCredential";
}

impl TemplateInfo for AcceptFreeCredential {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_credential")
            .ok_or(Error::PackageNotConfigured("governance_utility_credential"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for AcceptFreeCredential {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field("userServiceCid", make_contract_id(&self.user_service_cid)),
            field(
                "credentialOfferCid",
                make_contract_id(&self.credential_offer_cid),
            ),
        ]))
    }
}

impl Validate for AcceptFreeCredential {}

/// Accept a paid credential offered to the governance party. Wraps
/// `UserService_AcceptPaidCredentialOffer`, which locks the governance
/// party's deposit Amulets against the offer's billing parameters.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AcceptPaidCredential {
    pub user_service_cid: String,
    pub credential_offer_cid: String,
    /// The governance party's unlocked `Amulet` contracts that fund the deposit.
    pub deposit_amulet_cids: Vec<String>,
    pub app_transfer_context: AppTransferContextInput,
}

impl AcceptPaidCredential {
    pub const MODULE: &'static str = "Governance.UtilityCredential.AcceptPaidCredential";
    pub const ENTITY: &'static str = "AcceptPaidCredential";
}

impl TemplateInfo for AcceptPaidCredential {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_utility_credential")
            .ok_or(Error::PackageNotConfigured("governance_utility_credential"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for AcceptPaidCredential {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field("userServiceCid", make_contract_id(&self.user_service_cid)),
            field(
                "credentialOfferCid",
                make_contract_id(&self.credential_offer_cid),
            ),
            field(
                "depositAmulets",
                make_list(
                    self.deposit_amulet_cids
                        .iter()
                        .map(|cid| make_contract_id(cid))
                        .collect(),
                ),
            ),
            field(
                "appTransferContext",
                serialize_app_transfer_context(&self.app_transfer_context),
            ),
        ]))
    }
}

impl Validate for AcceptPaidCredential {
    /// The template refuses an empty list. A repeated Amulet would fail the
    /// deposit transfer only at execution, after the vote.
    fn validate(&self, _ctx: &ValidationCtx) -> Result<(), Error> {
        if self.deposit_amulet_cids.is_empty() {
            return Err(Error::Validation(
                "deposit_amulet_cids must name at least one Amulet".to_string(),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for cid in &self.deposit_amulet_cids {
            if !seen.insert(cid) {
                return Err(Error::Validation(format!(
                    "deposit_amulet_cids must not list {cid} more than once"
                )));
            }
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
        CantonId::parse(&format!("{prefix}::{ns}")).expect("valid canton id")
    }

    fn ctx(governance_party: &CantonId, now_micros: i64) -> ValidationCtx<'_> {
        ValidationCtx {
            governance_party,
            now_micros,
        }
    }

    fn claim() -> Claim {
        Claim {
            subject: "s".to_string(),
            property: "p".to_string(),
            value: "v".to_string(),
        }
    }

    #[test]
    fn offer_paid_credential_validate() {
        let gov = cid("gov");
        let base = OfferPaidCredential {
            user_service_cid: "usc".to_string(),
            holder: cid("holder"),
            id: "cred-1".to_string(),
            description: "paid".to_string(),
            claims: vec![claim()],
            billing_params: BillingParams {
                fee_per_day_usd: "1.5".parse().expect("valid decimal"),
                billing_period_minutes: 60,
                deposit_target_amount_usd: "10.0".parse().expect("valid decimal"),
                holder_activity_weight: Some("0.5".parse().expect("valid decimal")),
            },
            deposit_initial_amount_usd: None,
        };

        // No deposit at all is fine — the check only fires when a value is
        // supplied.
        assert!(base.validate(&ctx(&gov, 0)).is_ok());

        let with_positive_deposit = OfferPaidCredential {
            deposit_initial_amount_usd: Some("5.0".parse().expect("valid decimal")),
            ..base.clone()
        };
        assert!(with_positive_deposit.validate(&ctx(&gov, 0)).is_ok());

        let with_zero_deposit = OfferPaidCredential {
            deposit_initial_amount_usd: Some("0".parse().expect("valid decimal")),
            ..base.clone()
        };
        assert!(with_zero_deposit.validate(&ctx(&gov, 0)).is_err());

        let with_negative_deposit = OfferPaidCredential {
            deposit_initial_amount_usd: Some("-1".parse().expect("valid decimal")),
            ..base
        };
        assert!(with_negative_deposit.validate(&ctx(&gov, 0)).is_err());
    }

    fn paid_offer(billing_params: BillingParams) -> OfferPaidCredential {
        OfferPaidCredential {
            user_service_cid: "usc".to_string(),
            holder: cid("holder"),
            id: "cred-1".to_string(),
            description: "paid".to_string(),
            claims: vec![claim()],
            billing_params,
            deposit_initial_amount_usd: None,
        }
    }

    fn valid_billing_params() -> BillingParams {
        BillingParams {
            fee_per_day_usd: "1.5".parse().expect("valid decimal"),
            billing_period_minutes: 60,
            deposit_target_amount_usd: "10.0".parse().expect("valid decimal"),
            holder_activity_weight: None,
        }
    }

    #[test]
    fn offer_paid_credential_validate_refuses_billing_params_the_offer_rejects() {
        let gov = cid("gov");
        let dec = |s: &str| -> DamlDecimal { s.parse().expect("valid decimal") };
        let refused = [
            (
                BillingParams {
                    fee_per_day_usd: dec("0"),
                    ..valid_billing_params()
                },
                "billing_params.fee_per_day_usd",
            ),
            (
                BillingParams {
                    billing_period_minutes: 0,
                    ..valid_billing_params()
                },
                "billing_params.billing_period_minutes",
            ),
            (
                BillingParams {
                    deposit_target_amount_usd: dec("-1"),
                    ..valid_billing_params()
                },
                "billing_params.deposit_target_amount_usd",
            ),
            (
                BillingParams {
                    holder_activity_weight: Some(dec("1.01")),
                    ..valid_billing_params()
                },
                "billing_params.holder_activity_weight",
            ),
            (
                BillingParams {
                    holder_activity_weight: Some(dec("-0.1")),
                    ..valid_billing_params()
                },
                "billing_params.holder_activity_weight",
            ),
        ];
        for (params, field) in refused {
            let err = paid_offer(params.clone())
                .validate(&ctx(&gov, 0))
                .expect_err(&format!("{params:?} must be refused"));
            assert!(
                err.to_string().starts_with(field),
                "{params:?}: error {err} does not name {field}"
            );
        }

        // The weight bounds are inclusive.
        for weight in ["0", "1"] {
            let params = BillingParams {
                holder_activity_weight: Some(dec(weight)),
                ..valid_billing_params()
            };
            assert!(paid_offer(params).validate(&ctx(&gov, 0)).is_ok());
        }
    }

    fn paid_accept(deposit_amulet_cids: Vec<&str>) -> AcceptPaidCredential {
        AcceptPaidCredential {
            user_service_cid: "00usc".to_string(),
            credential_offer_cid: "00offer".to_string(),
            deposit_amulet_cids: deposit_amulet_cids.into_iter().map(String::from).collect(),
            app_transfer_context: AppTransferContextInput {
                amulet_rules_cid: "00rules".to_string(),
                open_mining_round_cid: "00round".to_string(),
                featured_app_right_cid: None,
            },
        }
    }

    #[test]
    fn accept_paid_credential_validate() {
        let gov = cid("gov");
        assert!(
            paid_accept(vec!["00a1", "00a2"])
                .validate(&ctx(&gov, 0))
                .is_ok()
        );

        let empty = paid_accept(vec![]).validate(&ctx(&gov, 0));
        assert_eq!(
            empty.expect_err("no Amulets is refused").to_string(),
            "deposit_amulet_cids must name at least one Amulet"
        );

        let repeated = paid_accept(vec!["00a1", "00a2", "00a1"]).validate(&ctx(&gov, 0));
        assert_eq!(
            repeated
                .expect_err("a repeated Amulet is refused")
                .to_string(),
            "deposit_amulet_cids must not list 00a1 more than once"
        );
    }

    #[test]
    fn encode_snapshots() {
        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_path(crate::catalog::proposals::SNAPSHOT_PATH);
        let _guard = settings.bind_to_scope();

        insta::assert_debug_snapshot!(
            "offer_free_credential",
            OfferFreeCredential {
                user_service_cid: "00usc".to_string(),
                holder: cid("holder"),
                id: "cred-1".to_string(),
                description: "free cred".to_string(),
                claims: vec![claim()],
            }
            .to_daml_proto()
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "offer_paid_credential_deposit_and_weight_some",
            OfferPaidCredential {
                user_service_cid: "00usc".to_string(),
                holder: cid("holder"),
                id: "cred-2".to_string(),
                description: "paid cred".to_string(),
                claims: vec![claim()],
                billing_params: BillingParams {
                    fee_per_day_usd: "1.5".parse().expect("valid decimal"),
                    billing_period_minutes: 60,
                    deposit_target_amount_usd: "30".parse().expect("valid decimal"),
                    holder_activity_weight: Some("0.5".parse().expect("valid decimal")),
                },
                deposit_initial_amount_usd: Some("10".parse().expect("valid decimal")),
            }
            .to_daml_proto()
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "offer_paid_credential_deposit_and_weight_none",
            OfferPaidCredential {
                user_service_cid: "00usc".to_string(),
                holder: cid("holder"),
                id: "cred-2".to_string(),
                description: "paid cred".to_string(),
                claims: vec![],
                billing_params: BillingParams {
                    fee_per_day_usd: "1.5".parse().expect("valid decimal"),
                    billing_period_minutes: 60,
                    deposit_target_amount_usd: "30".parse().expect("valid decimal"),
                    holder_activity_weight: None,
                },
                deposit_initial_amount_usd: None,
            }
            .to_daml_proto()
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "accept_free_credential",
            AcceptFreeCredential {
                user_service_cid: "00usc".to_string(),
                credential_offer_cid: "00offer".to_string(),
            }
            .to_daml_proto()
            .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "accept_paid_credential_featured_app_right_none",
            paid_accept(vec!["00a1", "00a2"])
                .to_daml_proto()
                .expect("payload encodes")
        );
        insta::assert_debug_snapshot!(
            "accept_paid_credential_featured_app_right_some",
            AcceptPaidCredential {
                app_transfer_context: AppTransferContextInput {
                    featured_app_right_cid: Some("00far".to_string()),
                    ..paid_accept(vec![]).app_transfer_context
                },
                ..paid_accept(vec!["00a1"])
            }
            .to_daml_proto()
            .expect("payload encodes")
        );
    }
}
