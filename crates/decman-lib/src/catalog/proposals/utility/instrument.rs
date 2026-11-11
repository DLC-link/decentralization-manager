//! Instrument provisioning and configuration, instrument-issuer onboarding
//! and offboarding, and the credential-id nonce the minting proposals carry.

use std::collections::HashSet;

use canton_proto_rs::com::daml::ledger::api::v2::{RecordField, Value, value};
use common::{
    api::{InstrumentIdentifier, InstrumentIssuerCredentials, PartyCredentialRequirement},
    canton_id::CantonId,
};

use super::OnboardRegistrar;
use crate::{
    error::Error,
    framework::{
        DamlProtoEncode, GrpcPayload, PackageResolver, TemplateId, TemplateInfo, Validate,
        ValidationCtx,
        encode::{
            field, make_contract_id, make_contract_id_list, make_list, make_optional_bool,
            make_optional_text, make_optional_timestamp, make_party, make_record, make_text,
            serialize_instrument_identifier, serialize_party_credential_requirements,
        },
        validate::{
            validate_block_list_supported, validate_credential_validity,
            validate_self_issued_requirements_have_claims, validate_unique_issuers,
        },
    },
};

/// Create an `InstrumentConfiguration` on the registrar decparty and
/// credential the initial instrument issuers against its issuer
/// requirements. Executed once per instrument.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ProvisionInstrument {
    pub registrar_service_cid: String,
    pub instrument_id_text: String,
    #[serde(default)]
    pub additional_identifiers: Vec<InstrumentIdentifier>,
    #[serde(default)]
    pub issuer_requirements: Vec<PartyCredentialRequirement>,
    #[serde(default)]
    pub holder_requirements: Vec<PartyCredentialRequirement>,
    #[serde(default)]
    pub initial_instrument_issuers: Vec<CantonId>,
    /// `validFrom` of every credential this action mints, as microseconds
    /// since epoch. None: valid from creation.
    #[serde(default)]
    pub credential_valid_from_micros: Option<i64>,
    /// `validUntil` of every credential this action mints, as microseconds
    /// since epoch. None: never expires, so ending it takes an
    /// `OffboardInstrumentIssuers` vote.
    #[serde(default)]
    pub credential_valid_until_micros: Option<i64>,
    /// `checkBlockList` of the created `InstrumentConfiguration`.
    /// `Some(true)` is refused until every mint, burn and transfer path
    /// sends block list shards (#566).
    #[serde(default)]
    pub use_block_list: Option<bool>,
}

impl ProvisionInstrument {
    pub const MODULE: &'static str = "Governance.UtilityOnboarding.ProvisionInstrument";
    pub const ENTITY: &'static str = "ProvisionInstrument";
}

impl TemplateInfo for ProvisionInstrument {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        TemplateId::for_package(pkgs, super::PACKAGE, Self::MODULE, Self::ENTITY)
    }
}

impl DamlProtoEncode for ProvisionInstrument {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field(
                "registrarServiceCid",
                make_contract_id(&self.registrar_service_cid),
            ),
            field("instrumentIdText", make_text(&self.instrument_id_text)),
            field(
                "additionalIdentifiers",
                make_list(
                    self.additional_identifiers
                        .iter()
                        .map(serialize_instrument_identifier)
                        .collect(),
                ),
            ),
            field(
                "issuerRequirements",
                serialize_party_credential_requirements(&self.issuer_requirements),
            ),
            field(
                "holderRequirements",
                serialize_party_credential_requirements(&self.holder_requirements),
            ),
            field(
                "initialInstrumentIssuers",
                make_list(
                    self.initial_instrument_issuers
                        .iter()
                        .map(make_party)
                        .collect(),
                ),
            ),
        ]))
    }
}

impl Validate for ProvisionInstrument {
    fn validate(&self, ctx: &ValidationCtx) -> Result<(), Error> {
        validate_self_issued_requirements_have_claims(
            &self.issuer_requirements,
            ctx.governance_party,
            "issuer_requirements",
        )?;
        validate_unique_issuers(
            &self.initial_instrument_issuers,
            "initial_instrument_issuers",
        )?;
        validate_credential_validity(
            self.credential_valid_from_micros,
            self.credential_valid_until_micros,
            ctx.now_micros,
        )?;
        validate_block_list_supported(self.use_block_list)
    }
}

/// Replace a live `InstrumentConfiguration` on the registrar decparty. The
/// registry's only update path archives the configuration and creates a new
/// one from a full payload, so every field here is required: a field left
/// out would be reset on the ledger. The action sets the provider app reward
/// beneficiaries again after the replace.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS))]
pub struct UpdateInstrumentConfiguration {
    pub registrar_service_cid: String,
    pub instrument_configuration_cid: String,
    /// Must equal the configuration's default identifier id. The action
    /// refuses to change it, because holdings name the instrument by it.
    pub instrument_id_text: String,
    pub additional_identifiers: Vec<InstrumentIdentifier>,
    pub issuer_requirements: Vec<PartyCredentialRequirement>,
    pub holder_requirements: Vec<PartyCredentialRequirement>,
    /// `checkBlockList` of the new configuration. The key is required and
    /// may be `null`, so a request that leaves it out cannot turn a block
    /// list check off by accident. `Some(true)` is refused until every mint,
    /// burn and transfer path sends block list shards (#566).
    #[serde(deserialize_with = "serde::Deserialize::deserialize")]
    pub use_block_list: Option<bool>,
}

impl UpdateInstrumentConfiguration {
    pub const MODULE: &'static str = "Governance.UtilityOnboarding.UpdateInstrumentConfiguration";
    pub const ENTITY: &'static str = "UpdateInstrumentConfiguration";
}

impl TemplateInfo for UpdateInstrumentConfiguration {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        TemplateId::for_package(pkgs, super::PACKAGE, Self::MODULE, Self::ENTITY)
    }
}

impl DamlProtoEncode for UpdateInstrumentConfiguration {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field(
                "registrarServiceCid",
                make_contract_id(&self.registrar_service_cid),
            ),
            field(
                "instrumentConfigurationCid",
                make_contract_id(&self.instrument_configuration_cid),
            ),
            field("instrumentIdText", make_text(&self.instrument_id_text)),
            field(
                "additionalIdentifiers",
                make_list(
                    self.additional_identifiers
                        .iter()
                        .map(serialize_instrument_identifier)
                        .collect(),
                ),
            ),
            field(
                "issuerRequirements",
                serialize_party_credential_requirements(&self.issuer_requirements),
            ),
            field(
                "holderRequirements",
                serialize_party_credential_requirements(&self.holder_requirements),
            ),
            field("useBlockList", make_optional_bool(&self.use_block_list)),
        ]))
    }
}

impl Validate for UpdateInstrumentConfiguration {
    fn validate(&self, ctx: &ValidationCtx) -> Result<(), Error> {
        // Mirrors the template's `ensure`.
        validate_self_issued_requirements_have_claims(
            &self.issuer_requirements,
            ctx.governance_party,
            "issuer_requirements",
        )?;
        validate_block_list_supported(self.use_block_list)
    }
}

/// Credential new instrument issuers against an existing
/// `InstrumentConfiguration`'s issuer requirements.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct OnboardInstrumentIssuers {
    pub instrument_configuration_cid: String,
    pub instrument_issuers: Vec<CantonId>,
    /// `validFrom` of every credential this action mints, as microseconds
    /// since epoch. None: valid from creation.
    #[serde(default)]
    pub credential_valid_from_micros: Option<i64>,
    /// `validUntil` of every credential this action mints, as microseconds
    /// since epoch. None: never expires, so ending it takes an
    /// `OffboardInstrumentIssuers` vote.
    #[serde(default)]
    pub credential_valid_until_micros: Option<i64>,
}

impl OnboardInstrumentIssuers {
    pub const MODULE: &'static str = "Governance.UtilityOnboarding.OnboardInstrumentIssuers";
    pub const ENTITY: &'static str = "OnboardInstrumentIssuers";
}

impl TemplateInfo for OnboardInstrumentIssuers {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        TemplateId::for_package(pkgs, super::PACKAGE, Self::MODULE, Self::ENTITY)
    }
}

impl DamlProtoEncode for OnboardInstrumentIssuers {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![
            field(
                "instrumentConfigurationCid",
                make_contract_id(&self.instrument_configuration_cid),
            ),
            field(
                "instrumentIssuers",
                make_list(self.instrument_issuers.iter().map(make_party).collect()),
            ),
        ]))
    }
}

impl Validate for OnboardInstrumentIssuers {
    fn validate(&self, ctx: &ValidationCtx) -> Result<(), Error> {
        // Mirrors the template's `ensure not (null instrumentIssuers)` so the
        // rejection surfaces as a 400 before the ledger sees the proposal.
        if self.instrument_issuers.is_empty() {
            return Err(Error::Validation(
                "instrument_issuers must not be empty".to_string(),
            ));
        }
        validate_unique_issuers(&self.instrument_issuers, "instrument_issuers")?;
        validate_credential_validity(
            self.credential_valid_from_micros,
            self.credential_valid_until_micros,
            ctx.now_micros,
        )
    }
}

/// A proposal whose template mints governance credentials through
/// `mintCredentialsMeetingRequirements`. The template ends with
/// `credentialIdNonce : Optional Text`, which the payload's own encoding
/// leaves out. The two issuer templates follow it with
/// `credentialValidFrom : Optional Time` and
/// `credentialValidUntil : Optional Time`. `ProvisionInstrument` then
/// declares `useBlockList : Optional Bool`, which its own encoding also
/// leaves out. Submit it through [`WithCredentialIdNonce`] to set them.
pub trait MintsCredentials: GrpcPayload {
    /// The `(validFrom, validUntil)` window, in microseconds since epoch,
    /// copied onto every credential the action mints. `None` for a template
    /// that declares no window fields, so none are sent.
    fn credential_validity_micros(&self) -> Option<(Option<i64>, Option<i64>)>;

    /// The fields the template declares after the validity window, in
    /// declaration order.
    fn fields_after_validity(&self) -> Vec<RecordField> {
        Vec::new()
    }
}

/// No window: the SDK checks the registrar credential only inside the
/// accept, and the `RegistrarService` keeps working after it expires.
impl MintsCredentials for OnboardRegistrar {
    fn credential_validity_micros(&self) -> Option<(Option<i64>, Option<i64>)> {
        None
    }
}

impl MintsCredentials for ProvisionInstrument {
    fn credential_validity_micros(&self) -> Option<(Option<i64>, Option<i64>)> {
        Some((
            self.credential_valid_from_micros,
            self.credential_valid_until_micros,
        ))
    }

    fn fields_after_validity(&self) -> Vec<RecordField> {
        vec![field(
            "useBlockList",
            make_optional_bool(&self.use_block_list),
        )]
    }
}

impl MintsCredentials for OnboardInstrumentIssuers {
    fn credential_validity_micros(&self) -> Option<(Option<i64>, Option<i64>)> {
        Some((
            self.credential_valid_from_micros,
            self.credential_valid_until_micros,
        ))
    }
}

/// Encode input for a [`MintsCredentials`] proposal. The nonce is runtime
/// data, not an HTTP payload field: the caller draws a fresh one for each
/// proposal. The template folds it into every credential id it mints, so
/// two actions minting for one subject at one ledger time still produce
/// distinct ids. The template's `ensure` clause refuses an empty nonce and
/// one that contains `/`. For the issuer templates the credential validity
/// window follows the nonce, because they declare it after the nonce.
pub struct WithCredentialIdNonce<'a, P: ?Sized> {
    pub proposal: &'a P,
    pub nonce: &'a str,
}

impl<P: MintsCredentials + ?Sized> DamlProtoEncode for WithCredentialIdNonce<'_, P> {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        let Some(value::Sum::Record(mut record)) = self.proposal.to_daml_proto()?.sum else {
            return Err(Error::Encode(
                "credential-minting proposal must encode to a Record".into(),
            ));
        };
        record.fields.push(field(
            "credentialIdNonce",
            make_optional_text(Some(self.nonce)),
        ));
        if let Some((valid_from, valid_until)) = self.proposal.credential_validity_micros() {
            record.fields.push(field(
                "credentialValidFrom",
                make_optional_timestamp(valid_from),
            ));
            record.fields.push(field(
                "credentialValidUntil",
                make_optional_timestamp(valid_until),
            ));
        }
        record.fields.extend(self.proposal.fields_after_validity());
        Ok(Value {
            sum: Some(value::Sum::Record(record)),
        })
    }
}

impl<P: MintsCredentials + ?Sized> TemplateInfo for WithCredentialIdNonce<'_, P> {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        self.proposal.template_id(pkgs)
    }
}

/// Revoke the credentials the governance party issued for instrument
/// issuers, removing their issuing privileges. Each row names one issuer
/// and lists that issuer's credentials.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct OffboardInstrumentIssuers {
    pub instrument_issuers: Vec<InstrumentIssuerCredentials>,
}

impl OffboardInstrumentIssuers {
    pub const MODULE: &'static str = "Governance.UtilityOnboarding.OffboardInstrumentIssuers";
    pub const ENTITY: &'static str = "OffboardInstrumentIssuers";
}

impl TemplateInfo for OffboardInstrumentIssuers {
    fn template_id(&self, pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
        TemplateId::for_package(pkgs, super::PACKAGE, Self::MODULE, Self::ENTITY)
    }
}

impl DamlProtoEncode for OffboardInstrumentIssuers {
    fn to_daml_proto(&self) -> Result<Value, Error> {
        Ok(make_record(vec![field(
            "instrumentIssuers",
            make_list(
                self.instrument_issuers
                    .iter()
                    .map(|row| {
                        make_record(vec![
                            field("instrumentIssuer", make_party(&row.instrument_issuer)),
                            field(
                                "credentialCids",
                                make_contract_id_list(&row.credential_cids),
                            ),
                        ])
                    })
                    .collect(),
            ),
        )]))
    }
}

impl Validate for OffboardInstrumentIssuers {
    fn validate(&self, _ctx: &ValidationCtx) -> Result<(), Error> {
        // Mirrors the template's four ensure guards.
        if self.instrument_issuers.is_empty() {
            return Err(Error::Validation(
                "instrument_issuers must not be empty".to_string(),
            ));
        }
        let mut seen_parties = HashSet::new();
        let mut seen_cids = HashSet::new();
        for row in &self.instrument_issuers {
            if row.credential_cids.is_empty() {
                return Err(Error::Validation(format!(
                    "credential_cids must not be empty for issuer {}",
                    row.instrument_issuer
                )));
            }
            if !seen_parties.insert(&row.instrument_issuer) {
                return Err(Error::Validation(format!(
                    "duplicate instrument issuer not allowed: {}",
                    row.instrument_issuer
                )));
            }
            for cid in &row.credential_cids {
                if !seen_cids.insert(cid) {
                    return Err(Error::Validation(format!(
                        "duplicate credential cid not allowed: {cid}"
                    )));
                }
            }
        }
        Ok(())
    }
}
