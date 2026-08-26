//! `governance-token-custody` proposal payloads.
//!
//! `Transfer` and `AcceptTransfer` implement only [`Validate`] +
//! [`TemplateInfo`] — encoding a transfer needs runtime data (the registry
//! choice context, the validity window, and for `Transfer` the sender party)
//! that isn't part of the HTTP payload, so [`DamlProtoEncode`] lives on the
//! wrapper structs [`TransferWithContext`] / [`AcceptTransferWithContext`]
//! instead. This forces the caller to decide about the registry context at
//! the call site rather than defaulting to an empty one that only fails much
//! later, on-ledger, at execute.

use canton_common::decimal::DamlDecimal;
use canton_common::transfer_factory::Context as ChoiceContext;
use canton_proto_rs::com::daml::ledger::api::v2::{Optional, Value, value};
use common::api::{InstrumentAllowance, InstrumentId};
use common::canton_id::CantonId;

use crate::error::Error;
use crate::framework::encode::{
    TransferValidity, field, make_contract_id, make_empty_extra_args, make_empty_metadata,
    make_extra_args_from_context, make_list, make_numeric, make_party, make_record, make_text,
    serialize_instrument_id,
};
use crate::framework::validate::validate_positive_amount;
use crate::framework::{
    DamlProtoEncode, PackageResolver, TemplateId, TemplateInfo, Validate, ValidationCtx,
};

/// Set up a Canton Coin `TransferPreapproval` for the governance party.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct SetupCcPreapproval {
    pub provider: CantonId,
    pub expected_dso: CantonId,
}

impl SetupCcPreapproval {
    pub const MODULE: &'static str = "Governance.TokenCustody.SetupCcPreapproval";
    pub const ENTITY: &'static str = "SetupCcPreapprovalProposal";
}

impl TemplateInfo for SetupCcPreapproval {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_token_custody")
            .ok_or(Error::PackageNotConfigured("governance_token_custody"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl DamlProtoEncode for SetupCcPreapproval {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field("provider", make_party(&self.provider)),
            field(
                "expectedDso",
                Value {
                    sum: Some(value::Sum::Optional(Box::new(Optional {
                        value: Some(Box::new(make_party(&self.expected_dso))),
                    }))),
                },
            ),
        ]))
    }
}

impl Validate for SetupCcPreapproval {}

/// Set up a utility-token `TransferPreapproval` for the governance party.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct SetupTokenPreapproval {
    pub operator: CantonId,
    pub instrument_admin: CantonId,
    #[serde(default)]
    pub instrument_allowances: Vec<InstrumentAllowance>,
}

impl SetupTokenPreapproval {
    pub const MODULE: &'static str = "Governance.TokenCustody.SetupTokenPreapproval";
    pub const ENTITY: &'static str = "SetupTokenPreapprovalProposal";
}

impl TemplateInfo for SetupTokenPreapproval {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_token_custody")
            .ok_or(Error::PackageNotConfigured("governance_token_custody"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

fn serialize_instrument_allowances(allowances: &[InstrumentAllowance]) -> Value {
    make_list(
        allowances
            .iter()
            .map(|a| make_record(vec![field("id", make_text(&a.id))]))
            .collect(),
    )
}

impl DamlProtoEncode for SetupTokenPreapproval {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field("operator", make_party(&self.operator)),
            field("instrumentAdmin", make_party(&self.instrument_admin)),
            field(
                "instrumentAllowances",
                serialize_instrument_allowances(&self.instrument_allowances),
            ),
        ]))
    }
}

impl Validate for SetupTokenPreapproval {}

/// Transfer tokens via a `TransferFactory`. Encoding needs the registry
/// choice context and the validity window, which aren't payload fields —
/// see [`TransferWithContext`].
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct Transfer {
    pub transfer_factory_cid: String,
    pub expected_admin: CantonId,
    pub receiver: CantonId,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub amount: DamlDecimal,
    pub instrument_id: InstrumentId,
    #[serde(default)]
    pub input_holding_cids: Vec<String>,
    /// How long the transfer (and, for two-step transfers, the resulting
    /// offer) stays valid, in hours. `None` uses the default window. A
    /// bounded window lets an unaccepted offer expire and release escrow.
    #[serde(default)]
    pub validity_window_hours: Option<u32>,
}

impl Transfer {
    pub const MODULE: &'static str = "Governance.TokenCustody.TransferProposal";
    pub const ENTITY: &'static str = "TransferProposal";
}

impl TemplateInfo for Transfer {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_token_custody")
            .ok_or(Error::PackageNotConfigured("governance_token_custody"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl Validate for Transfer {
    fn validate(&self, _ctx: &ValidationCtx) -> Result<(), Error> {
        validate_positive_amount(&self.amount, "amount")?;
        if self.validity_window_hours == Some(0) {
            return Err(Error::Validation(
                "validity_window_hours must be greater than 0".to_string(),
            ));
        }
        Ok(())
    }
}

/// Accept an incoming token transfer. Encoding needs the registry choice
/// context, which isn't a payload field — see [`AcceptTransferWithContext`].
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AcceptTransfer {
    pub transfer_instruction_cid: String,
}

impl AcceptTransfer {
    pub const MODULE: &'static str = "Governance.TokenCustody.AcceptTransfer";
    pub const ENTITY: &'static str = "AcceptTransferProposal";
}

impl TemplateInfo for AcceptTransfer {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        let pkg = pkgs
            .package_ref("governance_token_custody")
            .ok_or(Error::PackageNotConfigured("governance_token_custody"))?;
        Ok(TemplateId::new(pkg, Self::MODULE, Self::ENTITY))
    }
}

impl Validate for AcceptTransfer {}

/// Encode input for a `TransferProposal`. The registry choice context and
/// the validity window are runtime data, not HTTP payload fields, and the
/// on-chain `transfer.sender` is the governance party.
pub struct TransferWithContext<'a> {
    pub transfer: &'a Transfer,
    /// The governance party — the on-chain `transfer.sender` field.
    pub sender: &'a CantonId,
    /// None = empty extraArgs. Correct only in tests; a real transfer of a
    /// utility-registry instrument needs the registrar's choice context.
    pub context: Option<&'a ChoiceContext>,
    pub validity: TransferValidity,
}

impl DamlProtoEncode for TransferWithContext<'_> {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        let transfer_record = make_record(vec![
            field("sender", make_party(self.sender)),
            field("receiver", make_party(&self.transfer.receiver)),
            field("amount", make_numeric(&self.transfer.amount.to_string())),
            field(
                "instrumentId",
                serialize_instrument_id(&self.transfer.instrument_id),
            ),
            field(
                "requestedAt",
                Value {
                    sum: Some(value::Sum::Timestamp(self.validity.requested_at_micros)),
                },
            ),
            field(
                "executeBefore",
                Value {
                    sum: Some(value::Sum::Timestamp(self.validity.execute_before_micros)),
                },
            ),
            field(
                "inputHoldingCids",
                make_list(
                    self.transfer
                        .input_holding_cids
                        .iter()
                        .map(|cid| make_contract_id(cid))
                        .collect(),
                ),
            ),
            field("meta", make_empty_metadata()),
        ]);
        let extra_args = match self.context {
            Some(ctx) => make_extra_args_from_context(ctx)?,
            None => make_empty_extra_args(),
        };
        Ok(make_record(vec![
            field(
                "transferFactoryCid",
                make_contract_id(&self.transfer.transfer_factory_cid),
            ),
            field("expectedAdmin", make_party(&self.transfer.expected_admin)),
            field("transfer", transfer_record),
            field("extraArgs", extra_args),
        ]))
    }
}

impl TemplateInfo for TransferWithContext<'_> {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        self.transfer.template_id(pkgs)
    }
}

impl Validate for TransferWithContext<'_> {
    fn validate(&self, ctx: &ValidationCtx) -> Result<(), Error> {
        self.transfer.validate(ctx)
    }
}

/// Encode input for an `AcceptTransferProposal`. The registry choice context
/// is runtime data, not an HTTP payload field.
pub struct AcceptTransferWithContext<'a> {
    pub accept: &'a AcceptTransfer,
    /// None = empty extraArgs (legacy/test callers only).
    pub context: Option<&'a ChoiceContext>,
}

impl DamlProtoEncode for AcceptTransferWithContext<'_> {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        let extra_args = match self.context {
            Some(ctx) => make_extra_args_from_context(ctx)?,
            None => make_empty_extra_args(),
        };
        Ok(make_record(vec![
            field(
                "transferInstructionCid",
                make_contract_id(&self.accept.transfer_instruction_cid),
            ),
            field("extraArgs", extra_args),
        ]))
    }
}

impl TemplateInfo for AcceptTransferWithContext<'_> {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        self.accept.template_id(pkgs)
    }
}

impl Validate for AcceptTransferWithContext<'_> {
    fn validate(&self, ctx: &ValidationCtx) -> Result<(), Error> {
        self.accept.validate(ctx)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use canton_common::transfer_factory::ContextValue;
    use canton_proto_rs::com::daml::ledger::api::v2::Record;

    use super::*;
    use crate::framework::encode::{TRANSFER_EXECUTE_BEFORE_MICROS, TRANSFER_REQUESTED_AT_MICROS};

    /// Any valid `CantonId` — the exact value is irrelevant to these
    /// encode-shape assertions/snapshots.
    fn cid(prefix: &str) -> CantonId {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        CantonId::parse(&format!("{prefix}::{ns}")).unwrap()
    }

    /// Unwrap a `value::Sum::Record` reference (for descending into a nested
    /// record `Value`).
    fn as_record(value: &Value) -> &Record {
        match &value.sum {
            Some(value::Sum::Record(r)) => r,
            other => panic!("expected Record, got {other:?}"),
        }
    }

    /// Fetch a nested field's `Value` by label from a `Record`.
    fn field_value<'a>(record: &'a Record, label: &str) -> &'a Value {
        record
            .fields
            .iter()
            .find(|f| f.label == label)
            .and_then(|f| f.value.as_ref())
            .unwrap_or_else(|| panic!("missing field {label}"))
    }

    /// The ordered field labels of a `Record`.
    fn owned_labels(record: &Record) -> Vec<&str> {
        record.fields.iter().map(|f| f.label.as_str()).collect()
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

    fn transfer_fixture() -> Transfer {
        Transfer {
            transfer_factory_cid: "tfc".to_string(),
            expected_admin: cid("admin"),
            receiver: cid("recv"),
            amount: "1.5".parse().expect("valid decimal"),
            instrument_id: InstrumentId {
                admin: "admin::ns".to_string(),
                id: "instr-1".to_string(),
            },
            input_holding_cids: vec!["hc-1".to_string()],
            validity_window_hours: None,
        }
    }

    #[test]
    fn transfer_with_context_shape_and_nested_records() {
        let transfer = transfer_fixture();
        let sender = cid("gov");
        let wrapper = TransferWithContext {
            transfer: &transfer,
            sender: &sender,
            context: None,
            validity: TransferValidity {
                requested_at_micros: TRANSFER_REQUESTED_AT_MICROS,
                execute_before_micros: TRANSFER_EXECUTE_BEFORE_MICROS,
            },
        };
        let value = wrapper.to_daml_proto().unwrap();
        let record = as_record(&value);
        assert_eq!(
            owned_labels(record),
            [
                "transferFactoryCid",
                "expectedAdmin",
                "transfer",
                "extraArgs"
            ]
        );

        // Descend into the nested `transfer` record.
        let transfer_record = as_record(field_value(record, "transfer"));
        assert_eq!(
            owned_labels(transfer_record),
            [
                "sender",
                "receiver",
                "amount",
                "instrumentId",
                "requestedAt",
                "executeBefore",
                "inputHoldingCids",
                "meta",
            ]
        );
        assert!(
            matches!(&field_value(transfer_record, "sender").sum, Some(value::Sum::Party(p)) if p == &sender.to_string()),
            "sender must be the wrapper's sender (the governance party), not a transfer field"
        );

        // Nested `instrumentId` record.
        let instrument_id = as_record(field_value(transfer_record, "instrumentId"));
        assert_eq!(owned_labels(instrument_id), ["admin", "id"]);

        // Placeholder timestamps must be the exposed constants so propose-time
        // and execute-time payloads match (registrar resolves the context for
        // these exact choice arguments).
        assert!(matches!(
            field_value(transfer_record, "requestedAt").sum,
            Some(value::Sum::Timestamp(TRANSFER_REQUESTED_AT_MICROS)),
        ));
        assert!(matches!(
            field_value(transfer_record, "executeBefore").sum,
            Some(value::Sum::Timestamp(TRANSFER_EXECUTE_BEFORE_MICROS)),
        ));
        assert!(matches!(
            field_value(transfer_record, "amount").sum,
            Some(value::Sum::Numeric(_)),
        ));
    }

    #[test]
    fn accept_transfer_with_context_context_branches() {
        let accept = AcceptTransfer {
            transfer_instruction_cid: "tic".to_string(),
        };

        // ---- No choice context: context.values is an EMPTY TextMap ----
        let wrapper = AcceptTransferWithContext {
            accept: &accept,
            context: None,
        };
        let value = wrapper.to_daml_proto().unwrap();
        let record = as_record(&value);
        assert_eq!(
            owned_labels(record),
            ["transferInstructionCid", "extraArgs"]
        );

        let extra_args = as_record(field_value(record, "extraArgs"));
        let context = as_record(field_value(extra_args, "context"));
        let values = field_value(context, "values");
        match &values.sum {
            Some(value::Sum::TextMap(tm)) => assert!(
                tm.entries.is_empty(),
                "empty-context branch must yield an empty TextMap",
            ),
            other => panic!("expected empty TextMap for context.values, got {other:?}"),
        }

        // ---- With a choice context: one keyed AV_ContractId entry ----
        let key = "utility.digitalasset.com/transfer-rule".to_string();
        let ctx = ChoiceContext {
            values: HashMap::from([(
                key.clone(),
                ContextValue::ContractId("rule-cid".to_string()),
            )]),
        };
        let wrapper = AcceptTransferWithContext {
            accept: &accept,
            context: Some(&ctx),
        };
        let value = wrapper.to_daml_proto().unwrap();
        let record = as_record(&value);
        let extra_args = as_record(field_value(record, "extraArgs"));
        let context = as_record(field_value(extra_args, "context"));
        let values = field_value(context, "values");
        match &values.sum {
            Some(value::Sum::TextMap(tm)) => {
                assert_eq!(tm.entries.len(), 1, "exactly one context entry");
                let entry = &tm.entries[0];
                assert_eq!(entry.key, key);
                let entry_value = entry
                    .value
                    .as_ref()
                    .unwrap_or_else(|| panic!("context entry has no value"));
                let (ctor, _) = as_variant(entry_value);
                assert_eq!(ctor, "AV_ContractId");
            }
            other => panic!("expected populated TextMap for context.values, got {other:?}"),
        }
    }

    #[test]
    fn transfer_rejects_non_positive_amount_and_zero_window() {
        let ctx = ValidationCtx {
            governance_party: &cid("gov"),
            now_micros: 0,
        };
        let mk = |amount: &str, window: Option<u32>| Transfer {
            transfer_factory_cid: "tf".to_string(),
            expected_admin: cid("admin"),
            receiver: cid("recv"),
            amount: amount.parse().expect("valid decimal"),
            instrument_id: InstrumentId {
                admin: "a".into(),
                id: "i".into(),
            },
            input_holding_cids: Vec::new(),
            validity_window_hours: window,
        };
        assert!(mk("0", None).validate(&ctx).is_err());
        assert!(mk("-1.5", None).validate(&ctx).is_err());
        assert!(mk("0.0001", None).validate(&ctx).is_ok());
        // A custom (positive) window is accepted; a zero-hour window is rejected.
        assert!(mk("1.0", Some(48)).validate(&ctx).is_ok());
        assert!(mk("1.0", Some(0)).validate(&ctx).is_err());
    }

    #[test]
    fn encode_snapshots() {
        insta::assert_debug_snapshot!(
            "setup_cc_preapproval",
            SetupCcPreapproval {
                provider: cid("prov"),
                expected_dso: cid("dso"),
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "setup_token_preapproval",
            SetupTokenPreapproval {
                operator: cid("op"),
                instrument_admin: cid("iadmin"),
                instrument_allowances: vec![InstrumentAllowance { id: "TOK".into() }],
            }
            .to_daml_proto()
            .unwrap()
        );

        let transfer = transfer_fixture();
        let sender = cid("gov");
        let validity = TransferValidity {
            requested_at_micros: 1_700_000_000_000_000,
            execute_before_micros: 1_700_086_400_000_000,
        };
        insta::assert_debug_snapshot!(
            "transfer_with_context_none",
            TransferWithContext {
                transfer: &transfer,
                sender: &sender,
                context: None,
                validity,
            }
            .to_daml_proto()
            .unwrap()
        );

        let ctx = ChoiceContext {
            values: HashMap::from([(
                "utility.digitalasset.com/transfer-rule".to_string(),
                ContextValue::Text("rule-value".to_string()),
            )]),
        };
        insta::assert_debug_snapshot!(
            "transfer_with_context_populated",
            TransferWithContext {
                transfer: &transfer,
                sender: &sender,
                context: Some(&ctx),
                validity,
            }
            .to_daml_proto()
            .unwrap()
        );

        let accept = AcceptTransfer {
            transfer_instruction_cid: "ti-1".to_string(),
        };
        insta::assert_debug_snapshot!(
            "accept_transfer_with_context_none",
            AcceptTransferWithContext {
                accept: &accept,
                context: None,
            }
            .to_daml_proto()
            .unwrap()
        );
        insta::assert_debug_snapshot!(
            "accept_transfer_with_context_populated",
            AcceptTransferWithContext {
                accept: &accept,
                context: Some(&ctx),
            }
            .to_daml_proto()
            .unwrap()
        );
    }
}
