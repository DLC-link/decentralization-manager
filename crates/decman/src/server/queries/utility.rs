//! Utility provider and user services, credential offers and credentials.

use canton_common::decimal::DamlDecimal;
use canton_proto_rs::com::daml::ledger::api::v2::{CreatedEvent, Identifier, Record, value};
use decman_lib::framework::record::{field_int64, field_numeric, field_party, field_text};

use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
    error::Result,
};

use super::templates::{
    TemplateId, credential_offer_template, credential_template, provider_service_template,
    user_service_template,
};
use crate::server::{
    event_filters::{party_event_format, template_filter},
    ledger_paging::fetch_active_contracts_filtered,
    record::{field_record, record_field},
    types::{
        BillingParams, Claim, CredentialInfo, CredentialOfferInfo, ProviderServiceInfo,
        UserServiceInfo,
    },
};

// ============================================================================
// Utility Service Queries
// ============================================================================

/// Get all ProviderService contracts for a party
pub async fn get_provider_services(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
) -> Result<Vec<ProviderServiceInfo>> {
    match provider_service_template(packages) {
        Some(template) => {
            fetch_provider_services_for_template(config, party_id, token, &template).await
        }
        None => Ok(Vec::new()),
    }
}

/// Fetch provider services using TemplateFilter
async fn fetch_provider_services_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
) -> Result<Vec<ProviderServiceInfo>> {
    let event_format = party_event_format(
        party_id,
        vec![template_filter(
            Identifier {
                package_id: template.package_id.clone(),
                module_name: template.module_name.to_string(),
                entity_name: template.entity_name.to_string(),
            },
            false,
        )],
        true,
    );

    fetch_active_contracts_filtered(config, token, event_format, |created| {
        extract_provider_service_info(&created)
    })
    .await
}

/// Extract ProviderServiceInfo from a ProviderService created event
fn extract_provider_service_info(created: &CreatedEvent) -> Option<ProviderServiceInfo> {
    let record = created.create_arguments.as_ref()?;

    let operator: CantonId = field_party(record, "operator").and_then(|p| p.parse().ok())?;
    let provider: CantonId = field_party(record, "provider").and_then(|p| p.parse().ok())?;

    Some(ProviderServiceInfo {
        contract_id: created.contract_id.clone(),
        operator,
        provider,
    })
}

/// Get all UserService contracts for a party
pub async fn get_user_services(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
) -> Result<Vec<UserServiceInfo>> {
    match user_service_template(packages) {
        Some(template) => {
            fetch_user_services_for_template(config, party_id, token, &template).await
        }
        None => Ok(Vec::new()),
    }
}

/// Fetch user services using TemplateFilter
async fn fetch_user_services_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
) -> Result<Vec<UserServiceInfo>> {
    let event_format = party_event_format(
        party_id,
        vec![template_filter(
            Identifier {
                package_id: template.package_id.clone(),
                module_name: template.module_name.to_string(),
                entity_name: template.entity_name.to_string(),
            },
            false,
        )],
        true,
    );

    fetch_active_contracts_filtered(config, token, event_format, |created| {
        extract_user_service_info(&created)
    })
    .await
}

/// Extract UserServiceInfo from a UserService created event
fn extract_user_service_info(created: &CreatedEvent) -> Option<UserServiceInfo> {
    let record = created.create_arguments.as_ref()?;

    let operator: CantonId = field_party(record, "operator").and_then(|p| p.parse().ok())?;
    let user: CantonId = field_party(record, "user").and_then(|p| p.parse().ok())?;

    Some(UserServiceInfo {
        contract_id: created.contract_id.clone(),
        operator,
        user,
    })
}

// ============================================================================
// Credential Offer Queries
// ============================================================================

/// Get all CredentialOffer contracts visible to a party. Includes offers in
/// both directions (party as `holder` or as `issuer`); the caller filters for
/// the side it needs.
pub async fn get_credential_offers(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
) -> Result<Vec<CredentialOfferInfo>> {
    match credential_offer_template(packages) {
        Some(template) => {
            fetch_credential_offers_for_template(config, party_id, token, &template).await
        }
        None => Ok(Vec::new()),
    }
}

/// Fetch credential offers using TemplateFilter
async fn fetch_credential_offers_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
) -> Result<Vec<CredentialOfferInfo>> {
    let event_format = party_event_format(
        party_id,
        vec![template_filter(
            Identifier {
                package_id: template.package_id.clone(),
                module_name: template.module_name.to_string(),
                entity_name: template.entity_name.to_string(),
            },
            false,
        )],
        true,
    );

    fetch_active_contracts_filtered(config, token, event_format, |created| {
        extract_credential_offer_info(&created)
    })
    .await
}

/// Extract CredentialOfferInfo from a CredentialOffer created event. An offer
/// is free when its `billingParams : Optional BillingParams` field is `None` —
/// only those can be taken via `CredentialOffer_AcceptFree`.
fn extract_credential_offer_info(created: &CreatedEvent) -> Option<CredentialOfferInfo> {
    let record = created.create_arguments.as_ref()?;

    let operator: CantonId = field_party(record, "operator")?.parse().ok()?;
    let issuer: CantonId = field_party(record, "issuer")?.parse().ok()?;
    let holder: CantonId = field_party(record, "holder")?.parse().ok()?;
    let credential_id = field_text(record, "id")?;
    let description = field_text(record, "description").unwrap_or_default();

    let has_billing_params = match record_field(record, "billingParams") {
        Some(value::Sum::Optional(opt)) => opt.value.is_some(),
        _ => false,
    };

    Some(CredentialOfferInfo {
        contract_id: created.contract_id.clone(),
        operator,
        issuer,
        holder,
        credential_id,
        description,
        is_free: !has_billing_params,
        billing_params: offer_billing_params(record),
        deposit_initial_amount_usd: field_optional_decimal(record, "depositInitialAmountUsd"),
    })
}

/// The offer's `billingParams`. `None` for a free offer, or for params in a
/// shape decman cannot read.
fn offer_billing_params(record: &Record) -> Option<BillingParams> {
    let Some(value::Sum::Optional(opt)) = record_field(record, "billingParams") else {
        return None;
    };
    let Some(value::Sum::Record(params)) = opt.value.as_deref()?.sum.as_ref() else {
        return None;
    };
    Some(BillingParams {
        fee_per_day_usd: field_record(params, "feePerDayUsd")
            .and_then(|fee| field_numeric(fee, "rate"))?
            .parse()
            .ok()?,
        billing_period_minutes: field_int64(params, "billingPeriodMinutes")?,
        deposit_target_amount_usd: field_numeric(params, "depositTargetAmountUsd")?
            .parse()
            .ok()?,
        holder_activity_weight: field_optional_decimal(params, "holderActivityWeight"),
    })
}

/// An `Optional Decimal` field. `None` when the field is `None`, absent, or
/// not a decimal.
fn field_optional_decimal(record: &Record, label: &str) -> Option<DamlDecimal> {
    let Some(value::Sum::Optional(opt)) = record_field(record, label) else {
        return None;
    };
    match opt.value.as_deref()?.sum.as_ref()? {
        value::Sum::Numeric(n) => n.parse().ok(),
        _ => None,
    }
}

/// Get all Credential contracts visible to a party
pub async fn get_credentials(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
) -> Result<Vec<CredentialInfo>> {
    match credential_template(packages) {
        Some(template) => fetch_credentials_for_template(config, party_id, token, &template).await,
        None => Ok(Vec::new()),
    }
}

/// Fetch credentials using TemplateFilter
async fn fetch_credentials_for_template(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    template: &TemplateId,
) -> Result<Vec<CredentialInfo>> {
    let event_format = party_event_format(
        party_id,
        vec![template_filter(
            Identifier {
                package_id: template.package_id.clone(),
                module_name: template.module_name.to_string(),
                entity_name: template.entity_name.to_string(),
            },
            false,
        )],
        true,
    );

    fetch_active_contracts_filtered(config, token, event_format, |created| {
        extract_credential_info(&created)
    })
    .await
}

/// Extract CredentialInfo from a Credential created event.
fn extract_credential_info(created: &CreatedEvent) -> Option<CredentialInfo> {
    let record = created.create_arguments.as_ref()?;

    let issuer: CantonId = field_party(record, "issuer")?.parse().ok()?;
    let holder: CantonId = field_party(record, "holder")?.parse().ok()?;
    let credential_id = field_text(record, "id")?;
    let description = field_text(record, "description").unwrap_or_default();

    let claims = match record_field(record, "claims") {
        Some(value::Sum::List(l)) => Some(&l.elements),
        _ => None,
    }
    .map(|elements| {
        elements
            .iter()
            .filter_map(|v| match &v.sum {
                Some(value::Sum::Record(r)) => Some(Claim {
                    subject: field_text(r, "subject")?,
                    property: field_text(r, "property")?,
                    value: field_text(r, "value")?,
                }),
                _ => None,
            })
            .collect()
    })
    .unwrap_or_default();

    Some(CredentialInfo {
        contract_id: created.contract_id.clone(),
        issuer,
        holder,
        credential_id,
        description,
        claims,
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_values::*;
    use super::*;
    use canton_proto_rs::com::daml::ledger::api::v2::Value;

    // ------------------------------------------------------------------------
    // extract_credential_offer_info
    //
    // `Utility.Credential.App.V0.Model.Offer:CredentialOffer` carries the
    // issuer/holder parties, the credential id/description, and an optional
    // `billingParams`. Offers with `billingParams = None` are free and the only
    // kind `AcceptFreeCredential` can take, so the extractor surfaces that as
    // `is_free` for the accept-form dropdown to filter on.
    // ------------------------------------------------------------------------

    /// A CredentialOffer created event; `billing_params` is the raw value of
    /// the template's `billingParams : Optional BillingParams` field.
    fn credential_offer_event(billing_params: Value) -> CreatedEvent {
        let record = Record {
            record_id: None,
            fields: vec![
                field("operator", party_value(&format!("operator::{SR_FP}"))),
                field("issuer", party_value(&format!("issuer::{SR_FP}"))),
                field("holder", party_value(&format!("holder::{SR_FP}"))),
                field("dso", party_value(&format!("dso::{SR_FP}"))),
                field("id", text_value("provider-service-credential")),
                field("description", text_value("Provider service access")),
                field("billingParams", billing_params),
                field("depositInitialAmountUsd", optional_value(None)),
            ],
        };
        CreatedEvent {
            offset: 0,
            node_id: 0,
            contract_id: "offer-cid-1".to_string(),
            template_id: None,
            contract_key: None,
            create_arguments: Some(record),
            created_event_blob: vec![],
            interface_views: vec![],
            witness_parties: vec![],
            signatories: vec![],
            observers: vec![],
            created_at: None,
            package_name: String::new(),
            representative_package_id: String::new(),
            acs_delta: false,
            contract_key_hash: Vec::new(),
        }
    }

    #[test]
    fn extract_credential_offer_info_reads_free_offer() {
        let event = credential_offer_event(optional_value(None));
        let Some(info) = extract_credential_offer_info(&event) else {
            panic!("free offer should yield info");
        };
        assert_eq!(info.contract_id, "offer-cid-1");
        assert_eq!(info.operator.to_string(), format!("operator::{SR_FP}"));
        assert_eq!(info.issuer.to_string(), format!("issuer::{SR_FP}"));
        assert_eq!(info.holder.to_string(), format!("holder::{SR_FP}"));
        assert_eq!(info.credential_id, "provider-service-credential");
        assert_eq!(info.description, "Provider service access");
        assert!(info.is_free);
        assert_eq!(info.billing_params, None);
        assert_eq!(info.deposit_initial_amount_usd, None);
    }

    /// The ledger answers each `Numeric 10` with ten decimal places.
    #[test]
    fn extract_credential_offer_info_reads_the_billing_params_and_initial_deposit() {
        let billing = optional_value(Some(record_value(vec![
            field(
                "feePerDayUsd",
                record_value(vec![field("rate", numeric_value("1.5000000000"))]),
            ),
            field(
                "billingPeriodMinutes",
                Value {
                    sum: Some(value::Sum::Int64(60)),
                },
            ),
            field("depositTargetAmountUsd", numeric_value("30.0000000000")),
            field(
                "holderActivityWeight",
                optional_value(Some(numeric_value("0.2000000000"))),
            ),
        ])));
        let mut event = credential_offer_event(billing);
        if let Some(record) = event.create_arguments.as_mut() {
            for f in &mut record.fields {
                if f.label == "depositInitialAmountUsd" {
                    f.value = Some(optional_value(Some(numeric_value("20.0000000000"))));
                }
            }
        }
        let dec = |s: &str| -> DamlDecimal { s.parse().expect("valid decimal") };

        let Some(info) = extract_credential_offer_info(&event) else {
            panic!("paid offer should yield info");
        };

        assert!(!info.is_free);
        assert_eq!(
            info.billing_params,
            Some(BillingParams {
                fee_per_day_usd: dec("1.5"),
                billing_period_minutes: 60,
                deposit_target_amount_usd: dec("30"),
                holder_activity_weight: Some(dec("0.2")),
            })
        );
        assert_eq!(info.deposit_initial_amount_usd, Some(dec("20")));
    }

    #[test]
    fn extract_credential_offer_info_marks_paid_offer_not_free() {
        let billing = optional_value(Some(record_value(vec![field(
            "billingPeriodDuration",
            text_value("placeholder"),
        )])));
        let Some(info) = extract_credential_offer_info(&credential_offer_event(billing)) else {
            panic!("paid offer should still yield info");
        };
        assert!(!info.is_free);
        // Params decman cannot read are left out, not guessed.
        assert_eq!(info.billing_params, None);
    }

    #[test]
    fn extract_credential_offer_info_skips_event_without_holder() {
        let mut event = credential_offer_event(optional_value(None));
        if let Some(record) = event.create_arguments.as_mut() {
            record.fields.retain(|f| f.label != "holder");
        }
        assert!(extract_credential_offer_info(&event).is_none());
    }

    // ------------------------------------------------------------------------
    // extract_credential_info
    //
    // `Utility.Credential.V0.Credential:Credential` carries issuer/holder,
    // the credential id/description, and a `claims` list whose `subject`
    // names the party each claim attests for. The extractor feeds the
    // issuer-credential picker on the accept mint/burn request forms.
    // ------------------------------------------------------------------------

    fn claim_value(subject: &str, property: &str, value: &str) -> Value {
        record_value(vec![
            field("subject", text_value(subject)),
            field("property", text_value(property)),
            field("value", text_value(value)),
        ])
    }

    /// A Credential created event; `claims` is the raw value of the
    /// template's `claims : [Claim]` field.
    fn credential_event(claims: Value) -> CreatedEvent {
        let record = Record {
            record_id: None,
            fields: vec![
                field("issuer", party_value(&format!("issuer::{SR_FP}"))),
                field("holder", party_value(&format!("holder::{SR_FP}"))),
                field(
                    "id",
                    text_value("LAUNCH-TOKEN-instrument-issuer-credential/subject/0-0"),
                ),
                field("description", text_value("Governance-minted credential")),
                field("validFrom", optional_value(None)),
                field("validUntil", optional_value(None)),
                field("claims", claims),
                field("observers", list_value(vec![])),
            ],
        };
        CreatedEvent {
            offset: 0,
            node_id: 0,
            contract_id: "credential-cid-1".to_string(),
            template_id: None,
            contract_key: None,
            create_arguments: Some(record),
            created_event_blob: vec![],
            interface_views: vec![],
            witness_parties: vec![],
            signatories: vec![],
            observers: vec![],
            created_at: None,
            package_name: String::new(),
            representative_package_id: String::new(),
            acs_delta: false,
            contract_key_hash: Vec::new(),
        }
    }

    #[test]
    fn extract_credential_info_reads_credential_with_claims() {
        let claims = list_value(vec![
            claim_value("subject-party", "role", "instrument-issuer"),
            claim_value("subject-party", "kyc", "passed"),
        ]);
        let Some(info) = extract_credential_info(&credential_event(claims)) else {
            panic!("credential should yield info");
        };
        assert_eq!(info.contract_id, "credential-cid-1");
        assert_eq!(info.issuer.to_string(), format!("issuer::{SR_FP}"));
        assert_eq!(info.holder.to_string(), format!("holder::{SR_FP}"));
        assert_eq!(
            info.credential_id,
            "LAUNCH-TOKEN-instrument-issuer-credential/subject/0-0"
        );
        assert_eq!(info.description, "Governance-minted credential");
        assert_eq!(info.claims.len(), 2);
        assert_eq!(info.claims[0].subject, "subject-party");
        assert_eq!(info.claims[0].property, "role");
        assert_eq!(info.claims[0].value, "instrument-issuer");
    }

    #[test]
    fn extract_credential_info_defaults_missing_description_and_empty_claims() {
        let mut event = credential_event(list_value(vec![]));
        if let Some(record) = event.create_arguments.as_mut() {
            record.fields.retain(|f| f.label != "description");
        }
        let Some(info) = extract_credential_info(&event) else {
            panic!("claimless credential should still yield info");
        };
        assert!(info.claims.is_empty());
        assert_eq!(info.description, "");
    }

    #[test]
    fn extract_credential_info_skips_event_without_holder() {
        let mut event = credential_event(list_value(vec![]));
        if let Some(record) = event.create_arguments.as_mut() {
            record.fields.retain(|f| f.label != "holder");
        }
        assert!(extract_credential_info(&event).is_none());
    }
}
