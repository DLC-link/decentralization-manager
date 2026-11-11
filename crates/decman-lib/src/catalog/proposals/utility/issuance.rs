//! Token issuance: Mint, Burn and the accepts of mint and burn requests.

use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::daml::ledger::api::v2::{Value, value};
use common::api::InstrumentId;
use common::canton_id::CantonId;

use crate::error::Error;
use crate::framework::ValidationCtx;
use crate::framework::encode::serialize_instrument_id;
use crate::framework::encode::{field, make_contract_id, make_empty_metadata, make_numeric};
use crate::framework::encode::{
    make_optional_contract_id_list, make_party, make_record, make_text,
};
use crate::framework::validate::validate_positive_amount;
use crate::framework::{DamlProtoEncode, PackageResolver, TemplateId, TemplateInfo, Validate};

/// Offer a mint of `amount` tokens to `recipient` via
/// `AllocationFactory_OfferMint`. The resulting `MintOffer` is accepted
/// later by the recipient, outside this plugin.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct Mint {
    pub allocation_factory_cid: String,
    pub instrument_id: InstrumentId,
    pub instrument_configuration_cid: String,
    pub recipient: CantonId,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub amount: DamlDecimal,
    pub description: String,
    /// `StoredBlockListPartyShas` contract ids of the registrar's block list.
    /// The registry reads them when the instrument configuration checks a
    /// block list, and they must cover the recipient. Empty sends none.
    #[serde(default)]
    pub block_list_shard_cids: Vec<String>,
}

impl Mint {
    pub const MODULE: &'static str = "Governance.TokenIssuance.MintProposal";
    pub const ENTITY: &'static str = "MintProposal";
}

impl TemplateInfo for Mint {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        TemplateId::for_package(pkgs, super::PACKAGE, Self::MODULE, Self::ENTITY)
    }
}

impl DamlProtoEncode for Mint {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field(
                "allocationFactoryCid",
                make_contract_id(&self.allocation_factory_cid),
            ),
            field("instrumentId", serialize_instrument_id(&self.instrument_id)),
            field(
                "instrumentConfigurationCid",
                make_contract_id(&self.instrument_configuration_cid),
            ),
            field("recipient", make_party(&self.recipient)),
            field("amount", make_numeric(&self.amount.to_string())),
            field("description", make_text(&self.description)),
            field(
                "requestedAt",
                Value {
                    sum: Some(value::Sum::Timestamp(0)),
                },
            ),
            field(
                "executeBefore",
                Value {
                    sum: Some(value::Sum::Timestamp(i64::MAX / 1000)),
                },
            ),
            field("meta", make_empty_metadata()),
            field("extraArgsMeta", make_empty_metadata()),
            field(
                "blockListShardCids",
                make_optional_contract_id_list(&self.block_list_shard_cids),
            ),
        ]))
    }
}

impl Validate for Mint {
    fn validate(&self, _ctx: &ValidationCtx) -> Result<(), Error> {
        validate_positive_amount(&self.amount, "amount")
    }
}

/// Offer a burn of `amount` tokens held by `holder` via
/// `AllocationFactory_OfferBurn`. Holdings are supplied by the holder at
/// `BurnOffer_Accept` time, not here.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct Burn {
    pub allocation_factory_cid: String,
    pub instrument_id: InstrumentId,
    pub instrument_configuration_cid: String,
    pub holder: CantonId,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub amount: DamlDecimal,
    pub description: String,
    /// `StoredBlockListPartyShas` contract ids of the registrar's block list.
    /// The registry reads them when the instrument configuration checks a
    /// block list, and they must cover the burn holder. Empty sends none.
    #[serde(default)]
    pub block_list_shard_cids: Vec<String>,
}

impl Burn {
    pub const MODULE: &'static str = "Governance.TokenIssuance.BurnProposal";
    pub const ENTITY: &'static str = "BurnProposal";
}

impl TemplateInfo for Burn {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        TemplateId::for_package(pkgs, super::PACKAGE, Self::MODULE, Self::ENTITY)
    }
}

impl DamlProtoEncode for Burn {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field(
                "allocationFactoryCid",
                make_contract_id(&self.allocation_factory_cid),
            ),
            field("instrumentId", serialize_instrument_id(&self.instrument_id)),
            field(
                "instrumentConfigurationCid",
                make_contract_id(&self.instrument_configuration_cid),
            ),
            field("holder", make_party(&self.holder)),
            field("amount", make_numeric(&self.amount.to_string())),
            field("description", make_text(&self.description)),
            field(
                "requestedAt",
                Value {
                    sum: Some(value::Sum::Timestamp(0)),
                },
            ),
            field(
                "executeBefore",
                Value {
                    sum: Some(value::Sum::Timestamp(i64::MAX / 1000)),
                },
            ),
            field("meta", make_empty_metadata()),
            field("extraArgsMeta", make_empty_metadata()),
            field(
                "blockListShardCids",
                make_optional_contract_id_list(&self.block_list_shard_cids),
            ),
        ]))
    }
}

impl Validate for Burn {
    fn validate(&self, _ctx: &ValidationCtx) -> Result<(), Error> {
        validate_positive_amount(&self.amount, "amount")
    }
}

/// Accept a holder-initiated `MintRequest` via `MintRequest_Accept`. The
/// `MintRequest` must already exist on-ledger (typically created by the
/// holder by exercising `AllocationFactory_RequestMint`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AcceptMintRequest {
    pub mint_request_cid: String,
    pub instrument_configuration_cid: String,
    /// Credential contract ids proving the mint holder meets the
    /// instrument's issuer requirements. Empty for instruments without
    /// issuer requirements.
    #[serde(default)]
    pub issuer_credential_cids: Vec<String>,
    pub description: String,
    /// `StoredBlockListPartyShas` contract ids of the registrar's block list.
    /// The registry reads them when the instrument configuration checks a
    /// block list, and they must cover the mint holder. Empty sends none.
    #[serde(default)]
    pub block_list_shard_cids: Vec<String>,
}

impl AcceptMintRequest {
    pub const MODULE: &'static str = "Governance.TokenIssuance.AcceptMintRequest";
    pub const ENTITY: &'static str = "AcceptMintRequest";
}

impl TemplateInfo for AcceptMintRequest {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        TemplateId::for_package(pkgs, super::PACKAGE, Self::MODULE, Self::ENTITY)
    }
}

impl DamlProtoEncode for AcceptMintRequest {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field("mintRequestCid", make_contract_id(&self.mint_request_cid)),
            field(
                "instrumentConfigurationCid",
                make_contract_id(&self.instrument_configuration_cid),
            ),
            field("description", make_text(&self.description)),
            field("extraArgsMeta", make_empty_metadata()),
            field(
                "issuerCredentialCids",
                make_optional_contract_id_list(&self.issuer_credential_cids),
            ),
            field(
                "blockListShardCids",
                make_optional_contract_id_list(&self.block_list_shard_cids),
            ),
        ]))
    }
}

impl Validate for AcceptMintRequest {}

/// Accept a holder-initiated `BurnRequest` via `BurnRequest_Accept`. The
/// `BurnRequest` must already exist on-ledger (typically created by the
/// holder by exercising `AllocationFactory_RequestBurn`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AcceptBurnRequest {
    pub burn_request_cid: String,
    pub instrument_configuration_cid: String,
    /// Credential contract ids proving the burn holder meets the
    /// instrument's issuer requirements. Empty for instruments without
    /// issuer requirements.
    #[serde(default)]
    pub issuer_credential_cids: Vec<String>,
    pub description: String,
    /// `StoredBlockListPartyShas` contract ids of the registrar's block list.
    /// The registry reads them when the instrument configuration checks a
    /// block list, and they must cover the burn holder. Empty sends none.
    #[serde(default)]
    pub block_list_shard_cids: Vec<String>,
}

impl AcceptBurnRequest {
    pub const MODULE: &'static str = "Governance.TokenIssuance.AcceptBurnRequest";
    pub const ENTITY: &'static str = "AcceptBurnRequest";
}

impl TemplateInfo for AcceptBurnRequest {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        TemplateId::for_package(pkgs, super::PACKAGE, Self::MODULE, Self::ENTITY)
    }
}

impl DamlProtoEncode for AcceptBurnRequest {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field("burnRequestCid", make_contract_id(&self.burn_request_cid)),
            field(
                "instrumentConfigurationCid",
                make_contract_id(&self.instrument_configuration_cid),
            ),
            field("description", make_text(&self.description)),
            field("extraArgsMeta", make_empty_metadata()),
            field(
                "issuerCredentialCids",
                make_optional_contract_id_list(&self.issuer_credential_cids),
            ),
            field(
                "blockListShardCids",
                make_optional_contract_id_list(&self.block_list_shard_cids),
            ),
        ]))
    }
}

impl Validate for AcceptBurnRequest {}
