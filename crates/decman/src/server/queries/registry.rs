//! Utility registry: registrar services, provider configurations, block lists, instruments.

use canton_proto_rs::com::daml::ledger::api::v2::{CreatedEvent, Record, value};
use common::api::{
    InstrumentConfigurationFields, InstrumentIdentifier, PartyCredentialRequirement, RequiredClaim,
};
use decman_lib::framework::record::{field_party, field_text};

use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
    error::Result,
};

use super::templates::{
    TemplateId, fetch_by_template, provider_configuration_template,
    registrar_service_request_template, registrar_service_template,
};
use crate::server::{
    record::{field_record, record_field},
    types::{
        BlockListShardInfo, InstrumentInfo, ProviderConfigurationInfo, RegistrarServiceInfo,
        RegistrarServiceRequestInfo,
    },
};

// ============================================================================
// Registrar Service Queries
// ============================================================================

/// Get all RegistrarService contracts for a party
pub async fn get_registrar_services(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
) -> Result<Vec<RegistrarServiceInfo>> {
    fetch_by_template(
        config,
        party_id,
        token,
        registrar_service_template(packages),
        extract_registrar_service_info,
    )
    .await
}

/// Extract RegistrarServiceInfo from a RegistrarService created event
fn extract_registrar_service_info(created: &CreatedEvent) -> Option<RegistrarServiceInfo> {
    let record = created.create_arguments.as_ref()?;

    let operator: CantonId = field_party(record, "operator").and_then(|p| p.parse().ok())?;
    let registrar: CantonId = field_party(record, "registrar").and_then(|p| p.parse().ok())?;

    Some(RegistrarServiceInfo {
        contract_id: created.contract_id.clone(),
        operator,
        registrar,
    })
}

// ============================================================================
// Registrar Service Request Queries
// ============================================================================

/// Get all RegistrarServiceRequest contracts visible to a party. The
/// OnboardRegistrar form lists these so the request backing the onboard can
/// be picked instead of pasted in by hand.
pub async fn get_registrar_service_requests(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
) -> Result<Vec<RegistrarServiceRequestInfo>> {
    fetch_by_template(
        config,
        party_id,
        token,
        registrar_service_request_template(packages),
        extract_registrar_service_request_info,
    )
    .await
}

/// Read an `Optional Bool` field. An absent field or a `None` value reads as
/// `false`, matching the SDK's treatment of the request's flags.
fn field_optional_bool_or_false(record: &Record, label: &str) -> bool {
    match record_field(record, label) {
        Some(value::Sum::Optional(opt)) => {
            opt.value.as_deref().and_then(|inner| match &inner.sum {
                Some(value::Sum::Bool(b)) => Some(*b),
                _ => None,
            })
        }
        _ => None,
    }
    .unwrap_or(false)
}

/// Extract RegistrarServiceRequestInfo from a RegistrarServiceRequest
/// created event.
fn extract_registrar_service_request_info(
    created: &CreatedEvent,
) -> Option<RegistrarServiceRequestInfo> {
    let record = created.create_arguments.as_ref()?;

    let operator: CantonId = field_party(record, "operator")?.parse().ok()?;
    let provider: CantonId = field_party(record, "provider")?.parse().ok()?;
    let registrar: CantonId = field_party(record, "registrar")?.parse().ok()?;

    Some(RegistrarServiceRequestInfo {
        contract_id: created.contract_id.clone(),
        operator,
        provider,
        registrar,
        create_transfer_rule: field_optional_bool_or_false(record, "createTransferRule"),
        create_allocation_factory: field_optional_bool_or_false(record, "createAllocationFactory"),
    })
}

// ============================================================================
// Provider Configuration Queries
// ============================================================================

/// Get all ProviderConfiguration contracts visible to a party. The
/// OnboardRegistrar form lists these so the configuration backing the
/// onboard can be picked instead of pasted in by hand.
pub async fn get_provider_configurations(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    packages: &PackageConfig,
) -> Result<Vec<ProviderConfigurationInfo>> {
    fetch_by_template(
        config,
        party_id,
        token,
        provider_configuration_template(packages),
        extract_provider_configuration_info,
    )
    .await
}

/// Extract ProviderConfigurationInfo from a ProviderConfiguration created
/// event. The requirement lists stay behind: the picker labels
/// configurations by contract id alone.
fn extract_provider_configuration_info(
    created: &CreatedEvent,
) -> Option<ProviderConfigurationInfo> {
    let record = created.create_arguments.as_ref()?;

    let operator: CantonId = field_party(record, "operator")?.parse().ok()?;
    let provider: CantonId = field_party(record, "provider")?.parse().ok()?;

    Some(ProviderConfigurationInfo {
        contract_id: created.contract_id.clone(),
        operator,
        provider,
    })
}

// ============================================================================
// Block List Shard Queries
// ============================================================================

/// StoredBlockListPartyShas template identifier. Hard-coded
/// `#utility-registry-v0` for the same reason as
/// [`instrument_configuration_template`].
fn block_list_shard_template() -> TemplateId {
    TemplateId {
        package_id: "#utility-registry-v0".to_string(),
        module_name: "Utility.Registry.V0.Rule.BlockList",
        entity_name: "StoredBlockListPartyShas",
    }
}

/// Get all StoredBlockListPartyShas contracts visible to a party. The
/// registrar signs its shards, so a registrar decparty sees its own.
pub async fn get_block_list_shards(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
) -> Result<Vec<BlockListShardInfo>> {
    fetch_by_template(
        config,
        party_id,
        token,
        Some(block_list_shard_template()),
        extract_block_list_shard_info,
    )
    .await
}

/// Extract BlockListShardInfo from a StoredBlockListPartyShas created event.
/// The blocked parties stay behind: the picker labels shards by prefix.
fn extract_block_list_shard_info(created: &CreatedEvent) -> Option<BlockListShardInfo> {
    let record = created.create_arguments.as_ref()?;

    Some(BlockListShardInfo {
        contract_id: created.contract_id.clone(),
        operator: field_party(record, "operator")?.parse().ok()?,
        registrar: field_party(record, "registrar")?.parse().ok()?,
        prefix: field_text(record, "prefix")?,
    })
}

// ============================================================================
// InstrumentConfiguration Queries
// ============================================================================

/// InstrumentConfiguration template identifier. Hard-coded `#utility-registry-v0`
/// because it lives in a different package than `utility_registry`
/// (= `#utility-registry-app-v0`) and PackageConfig has no separate field for
/// it. Canton resolves the `#name-version` selector at query time.
fn instrument_configuration_template() -> TemplateId {
    TemplateId {
        package_id: "#utility-registry-v0".to_string(),
        module_name: "Utility.Registry.V0.Configuration.Instrument",
        entity_name: "InstrumentConfiguration",
    }
}

/// Get all InstrumentConfiguration contracts for a party. Each one represents
/// one token the governance party can mint/burn against.
pub async fn get_instruments(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
) -> Result<Vec<InstrumentInfo>> {
    fetch_by_template(
        config,
        party_id,
        token,
        Some(instrument_configuration_template()),
        extract_instrument_info,
    )
    .await
}

/// Extract InstrumentInfo from an InstrumentConfiguration created event.
/// Reads `instrument_admin` and `instrument_id` from the contract's
/// `defaultIdentifier` record (fields `source` and `id` respectively, per
/// `Utility.Registry.Holding.V0.Types.InstrumentIdentifier`).
fn extract_instrument_info(created: &CreatedEvent) -> Option<InstrumentInfo> {
    let record = created.create_arguments.as_ref()?;

    let default_identifier = field_record(record, "defaultIdentifier")?;

    let instrument_admin: CantonId =
        field_party(default_identifier, "source").and_then(|p| p.parse().ok())?;

    let instrument_id: String = field_text(default_identifier, "id")?;

    Some(InstrumentInfo {
        contract_id: created.contract_id.clone(),
        instrument_admin,
        instrument_id,
        configuration: extract_instrument_configuration_fields(record),
    })
}

/// Read the fields an update replaces. `None` if any of them has an
/// unexpected shape, so the form never starts from a partial read. A
/// configuration created before checkBlockList existed has no such field,
/// which reads as unset.
fn extract_instrument_configuration_fields(
    record: &Record,
) -> Option<InstrumentConfigurationFields> {
    Some(InstrumentConfigurationFields {
        additional_identifiers: list_field_records(record, "additionalIdentifiers")?
            .into_iter()
            .map(|identifier| {
                Some(InstrumentIdentifier {
                    source: field_party(identifier, "source")?.parse().ok()?,
                    id: field_text(identifier, "id")?,
                    scheme: field_text(identifier, "scheme")?,
                })
            })
            .collect::<Option<_>>()?,
        issuer_requirements: credential_requirements(record, "issuerRequirements")?,
        holder_requirements: credential_requirements(record, "holderRequirements")?,
        check_block_list: match record_field(record, "checkBlockList") {
            Some(value::Sum::Optional(opt)) => match opt.value.as_deref() {
                None => None,
                Some(inner) => match inner.sum {
                    Some(value::Sum::Bool(b)) => Some(b),
                    _ => return None,
                },
            },
            None => None,
            _ => return None,
        },
    })
}

/// The record elements of a list field. `None` if the field is not a list
/// of records.
fn list_field_records<'a>(record: &'a Record, label: &str) -> Option<Vec<&'a Record>> {
    let Some(value::Sum::List(list)) = record_field(record, label) else {
        return None;
    };
    list.elements
        .iter()
        .map(|element| match &element.sum {
            Some(value::Sum::Record(r)) => Some(r),
            _ => None,
        })
        .collect()
}

/// Read a `[PartyCredentialRequirement]` field. Each required claim is a
/// Daml `(Text, Text)` tuple, whose fields are `_1` and `_2`.
fn credential_requirements(
    record: &Record,
    label: &str,
) -> Option<Vec<PartyCredentialRequirement>> {
    list_field_records(record, label)?
        .into_iter()
        .map(|requirement| {
            Some(PartyCredentialRequirement {
                issuer: field_party(requirement, "issuer")?.parse().ok()?,
                required_claims: list_field_records(requirement, "requiredClaims")?
                    .into_iter()
                    .map(|claim| {
                        Some(RequiredClaim {
                            property: field_text(claim, "_1")?,
                            value: field_text(claim, "_2")?,
                        })
                    })
                    .collect::<Option<_>>()?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::test_values::*;
    use super::*;
    use canton_proto_rs::com::daml::ledger::api::v2::Value;

    // ------------------------------------------------------------------------
    // extract_registrar_service_request_info
    //
    // `Utility.Registry.App.V0.Service.Registrar:RegistrarServiceRequest`
    // carries the operator/provider/registrar parties plus two
    // `Optional Bool` flags the SDK reads as `false` when absent. The
    // extractor feeds the request picker on the OnboardRegistrar form.
    // ------------------------------------------------------------------------

    fn bool_value(b: bool) -> Value {
        Value {
            sum: Some(value::Sum::Bool(b)),
        }
    }

    /// A RegistrarServiceRequest created event; the two arguments are the
    /// raw values of the template's `Optional Bool` flag fields.
    fn registrar_service_request_event(
        create_transfer_rule: Value,
        create_allocation_factory: Value,
    ) -> CreatedEvent {
        let record = Record {
            record_id: None,
            fields: vec![
                field("operator", party_value(&format!("operator::{SR_FP}"))),
                field("provider", party_value(&format!("provider::{SR_FP}"))),
                field("registrar", party_value(&format!("registrar::{SR_FP}"))),
                field("createTransferRule", create_transfer_rule),
                field("createAllocationFactory", create_allocation_factory),
            ],
        };
        CreatedEvent {
            offset: 0,
            node_id: 0,
            contract_id: "rsr-cid-1".to_string(),
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
    fn extract_registrar_service_request_info_reads_request_with_flags() {
        let event = registrar_service_request_event(
            optional_value(Some(bool_value(true))),
            optional_value(Some(bool_value(false))),
        );
        let Some(info) = extract_registrar_service_request_info(&event) else {
            panic!("request should yield info");
        };
        assert_eq!(info.contract_id, "rsr-cid-1");
        assert_eq!(info.operator.to_string(), format!("operator::{SR_FP}"));
        assert_eq!(info.provider.to_string(), format!("provider::{SR_FP}"));
        assert_eq!(info.registrar.to_string(), format!("registrar::{SR_FP}"));
        assert!(info.create_transfer_rule);
        assert!(!info.create_allocation_factory);
    }

    #[test]
    fn extract_registrar_service_request_info_defaults_absent_flags_to_false() {
        // `None` flags — and fields missing outright — read as `false`,
        // matching the SDK's treatment.
        let mut event = registrar_service_request_event(optional_value(None), optional_value(None));
        if let Some(record) = event.create_arguments.as_mut() {
            record
                .fields
                .retain(|f| f.label != "createAllocationFactory");
        }
        let Some(info) = extract_registrar_service_request_info(&event) else {
            panic!("flagless request should still yield info");
        };
        assert!(!info.create_transfer_rule);
        assert!(!info.create_allocation_factory);
    }

    #[test]
    fn extract_registrar_service_request_info_skips_event_without_registrar() {
        let mut event = registrar_service_request_event(optional_value(None), optional_value(None));
        if let Some(record) = event.create_arguments.as_mut() {
            record.fields.retain(|f| f.label != "registrar");
        }
        assert!(extract_registrar_service_request_info(&event).is_none());
    }

    // ------------------------------------------------------------------------
    // extract_provider_configuration_info
    //
    // `Utility.Registry.App.V0.Configuration.Provider:ProviderConfiguration`
    // carries the operator/provider parties plus the registrar and holder
    // requirement lists. The extractor reads the parties only — the picker
    // labels configurations by contract id — and must tolerate the
    // requirement lists it ignores. It feeds the configuration picker on the
    // OnboardRegistrar form.
    // ------------------------------------------------------------------------

    /// A ProviderConfiguration created event, with empty requirement lists.
    fn provider_configuration_event() -> CreatedEvent {
        let record = Record {
            record_id: None,
            fields: vec![
                field("operator", party_value(&format!("operator::{SR_FP}"))),
                field("provider", party_value(&format!("provider::{SR_FP}"))),
                field("registrarRequirements", list_value(vec![])),
                field("holderRequirements", list_value(vec![])),
            ],
        };
        CreatedEvent {
            offset: 0,
            node_id: 0,
            contract_id: "pc-cid-1".to_string(),
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
    fn extract_provider_configuration_info_reads_parties() {
        let Some(info) = extract_provider_configuration_info(&provider_configuration_event())
        else {
            panic!("configuration should yield info");
        };
        assert_eq!(info.contract_id, "pc-cid-1");
        assert_eq!(info.operator.to_string(), format!("operator::{SR_FP}"));
        assert_eq!(info.provider.to_string(), format!("provider::{SR_FP}"));
    }

    #[test]
    fn extract_provider_configuration_info_skips_event_without_provider() {
        let mut event = provider_configuration_event();
        if let Some(record) = event.create_arguments.as_mut() {
            record.fields.retain(|f| f.label != "provider");
        }
        assert!(extract_provider_configuration_info(&event).is_none());
    }

    // ------------------------------------------------------------------------
    // extract_block_list_shard_info
    //
    // `Utility.Registry.V0.Rule.BlockList:StoredBlockListPartyShas` carries
    // the operator and registrar parties, the hash prefix, the blocked
    // parties and the shard size. The extractor reads the parties and the
    // prefix, and must tolerate the fields it ignores. It feeds the shard
    // picker on the accept mint and burn request forms.
    // ------------------------------------------------------------------------

    /// A StoredBlockListPartyShas created event with an empty block list.
    fn block_list_shard_event() -> CreatedEvent {
        let record = Record {
            record_id: None,
            fields: vec![
                field("operator", party_value(&format!("operator::{SR_FP}"))),
                field("registrar", party_value(&format!("registrar::{SR_FP}"))),
                field("prefix", text_value("a")),
                field("blockList", record_value(vec![])),
                field(
                    "maxBlockListShardSize",
                    Value {
                        sum: Some(value::Sum::Int64(16)),
                    },
                ),
            ],
        };
        CreatedEvent {
            offset: 0,
            node_id: 0,
            contract_id: "shard-cid-1".to_string(),
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
    fn extract_block_list_shard_info_reads_parties_and_prefix() {
        let Some(info) = extract_block_list_shard_info(&block_list_shard_event()) else {
            panic!("shard should yield info");
        };
        assert_eq!(info.contract_id, "shard-cid-1");
        assert_eq!(info.operator.to_string(), format!("operator::{SR_FP}"));
        assert_eq!(info.registrar.to_string(), format!("registrar::{SR_FP}"));
        assert_eq!(info.prefix, "a");
    }

    #[test]
    fn extract_block_list_shard_info_skips_event_without_prefix() {
        let mut event = block_list_shard_event();
        if let Some(record) = event.create_arguments.as_mut() {
            record.fields.retain(|f| f.label != "prefix");
        }
        assert!(extract_block_list_shard_info(&event).is_none());
    }

    /// An InstrumentConfiguration created event in the shape the ledger
    /// sends: one additional identifier, one issuer requirement with one
    /// claim, no holder requirements, and the given block list flag.
    fn instrument_configuration_event(check_block_list: Value) -> CreatedEvent {
        let registrar = format!("registrar::{SR_FP}");
        let identifier = |id: &str, scheme: &str| {
            record_value(vec![
                field("source", party_value(&registrar)),
                field("id", text_value(id)),
                field("scheme", text_value(scheme)),
            ])
        };
        let record = Record {
            record_id: None,
            fields: vec![
                field("operator", party_value(&format!("operator::{SR_FP}"))),
                field("provider", party_value(&format!("provider::{SR_FP}"))),
                field("registrar", party_value(&registrar)),
                field(
                    "defaultIdentifier",
                    identifier("TOKEN", "RegistrarInternalScheme"),
                ),
                field(
                    "additionalIdentifiers",
                    list_value(vec![identifier("TICK", "Ticker")]),
                ),
                field(
                    "issuerRequirements",
                    list_value(vec![record_value(vec![
                        field("issuer", party_value(&registrar)),
                        field(
                            "requiredClaims",
                            list_value(vec![record_value(vec![
                                field("_1", text_value("role")),
                                field("_2", text_value("instrument-issuer")),
                            ])]),
                        ),
                    ])]),
                ),
                field("holderRequirements", list_value(vec![])),
                field("providerAppRewardBeneficiaries", optional_value(None)),
                field("checkBlockList", check_block_list),
            ],
        };
        CreatedEvent {
            offset: 0,
            node_id: 0,
            contract_id: "icc-cid-1".to_string(),
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
    fn extract_instrument_info_reads_the_replaceable_fields() {
        let registrar: CantonId = format!("registrar::{SR_FP}").parse().expect("party id");
        for (flag, expected) in [
            (optional_value(None), None),
            (optional_value(Some(bool_value(true))), Some(true)),
            (optional_value(Some(bool_value(false))), Some(false)),
        ] {
            let info = extract_instrument_info(&instrument_configuration_event(flag))
                .expect("configuration should yield info");
            assert_eq!(info.instrument_id, "TOKEN");
            assert_eq!(
                info.configuration,
                Some(InstrumentConfigurationFields {
                    additional_identifiers: vec![InstrumentIdentifier {
                        source: registrar.clone(),
                        id: "TICK".to_string(),
                        scheme: "Ticker".to_string(),
                    }],
                    issuer_requirements: vec![PartyCredentialRequirement {
                        issuer: registrar.clone(),
                        required_claims: vec![RequiredClaim {
                            property: "role".to_string(),
                            value: "instrument-issuer".to_string(),
                        }],
                    }],
                    holder_requirements: vec![],
                    check_block_list: expected,
                })
            );
        }
    }

    /// A configuration from before registry-v0 0.7.0 has no checkBlockList,
    /// which reads as unset.
    #[test]
    fn extract_instrument_info_reads_a_missing_block_list_flag_as_unset() {
        let mut without_flag = instrument_configuration_event(optional_value(None));
        if let Some(record) = without_flag.create_arguments.as_mut() {
            record.fields.retain(|f| f.label != "checkBlockList");
        }
        let info = extract_instrument_info(&without_flag).expect("instrument lists");
        let fields = info.configuration.expect("fields read");
        assert_eq!(fields.check_block_list, None);
        assert_eq!(fields.issuer_requirements.len(), 1);
    }

    /// A claim of the wrong shape cannot be read, and neither can a flag that
    /// is not an Optional Bool. The form then gets no partial values, and the
    /// instrument still lists for mint and burn.
    #[test]
    fn extract_instrument_info_leaves_out_fields_it_cannot_read() {
        let bad_flag = instrument_configuration_event(bool_value(true));
        let info = extract_instrument_info(&bad_flag).expect("instrument still lists");
        assert_eq!(info.configuration, None);

        let mut bad_claim = instrument_configuration_event(optional_value(None));
        if let Some(record) = bad_claim.create_arguments.as_mut() {
            for f in record.fields.iter_mut() {
                if f.label == "issuerRequirements" {
                    f.value = Some(list_value(vec![record_value(vec![
                        field("issuer", party_value(&format!("registrar::{SR_FP}"))),
                        field("requiredClaims", list_value(vec![text_value("role")])),
                    ])]));
                }
            }
        }
        let info = extract_instrument_info(&bad_claim).expect("instrument still lists");
        assert_eq!(info.configuration, None);
    }
}
