use std::collections::HashMap;

use canton_proto_rs::com::daml::ledger::api::v2::{
    CumulativeFilter, EventFormat, Filters, GetActiveContractsRequest, GetLedgerEndRequest,
    WildcardFilter, cumulative_filter, get_active_contracts_response::ContractEntry,
};

use crate::{config::NodeConfig, error::Result, utils};

use super::types::{ContractInfo, PartyMetadata};

/// Get active contracts for a party
pub async fn get_contracts(config: &NodeConfig, party_id: &str) -> Result<Vec<ContractInfo>> {
    let mut state_client = utils::create_state_client(config).await?;

    // Get current ledger end
    let ledger_end = state_client
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {}))
        .await?
        .into_inner()
        .offset;

    let mut filters_by_party = HashMap::new();
    filters_by_party.insert(
        party_id.to_string(),
        Filters {
            cumulative: vec![CumulativeFilter {
                identifier_filter: Some(cumulative_filter::IdentifierFilter::WildcardFilter(
                    WildcardFilter {
                        include_created_event_blob: false,
                    },
                )),
            }],
        },
    );

    let acs_request = GetActiveContractsRequest {
        active_at_offset: ledger_end,
        event_format: Some(EventFormat {
            filters_by_party,
            filters_for_any_party: None,
            verbose: false,
        }),
    };

    let mut stream = state_client
        .get_active_contracts(tonic::Request::new(acs_request))
        .await?
        .into_inner();

    let mut contracts = Vec::new();
    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
        {
            let template_id = created
                .template_id
                .as_ref()
                .map(|t| {
                    format!(
                        "{module}:{entity}",
                        module = t.module_name,
                        entity = t.entity_name
                    )
                })
                .unwrap_or_default();
            let package_id = created
                .template_id
                .as_ref()
                .map(|t| t.package_id.clone())
                .unwrap_or_default();

            contracts.push(ContractInfo {
                contract_id: created.contract_id,
                template_id,
                package_id,
            });
        }
    }

    Ok(contracts)
}

/// Get party metadata from Ledger API
pub async fn get_party_metadata(
    config: &NodeConfig,
    party_id: &str,
) -> Result<Option<PartyMetadata>> {
    use canton_proto_rs::com::daml::ledger::api::v2::admin::ListKnownPartiesRequest;

    let mut client = utils::create_party_client(config).await?;

    let response = client
        .list_known_parties(tonic::Request::new(ListKnownPartiesRequest {
            identity_provider_id: String::new(),
            page_token: String::new(),
            page_size: 1000,
        }))
        .await?
        .into_inner();

    let party_details = response.party_details.iter().find(|p| p.party == party_id);

    let annotations = party_details
        .and_then(|d| d.local_metadata.as_ref())
        .map(|m| m.annotations.clone())
        .unwrap_or_default();

    if annotations.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PartyMetadata { annotations }))
    }
}
