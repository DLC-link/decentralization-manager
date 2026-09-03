//! `governance-utility-credential` proposal payloads.

use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::daml::ledger::api::v2::Value;
use common::api::Claim;
use common::canton_id::CantonId;

use crate::catalog::types::{BillingParams, serialize_billing_params};
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
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
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
        if let Some(d) = &self.deposit_initial_amount_usd {
            validate_positive_amount(d, "deposit_initial_amount_usd")?;
        }
        Ok(())
    }
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
    }
}
