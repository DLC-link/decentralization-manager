//! Builders for the ledger-API `EventFormat` every ACS, update and
//! by-contract-id read carries.
//!
//! A read is "filter + extractor". These cover the filter half so each fetcher
//! is left with only the part specific to it: which templates it wants and what
//! it pulls out of the events.

use std::collections::HashMap;

use canton_proto_rs::com::daml::ledger::api::v2::{
    CumulativeFilter, EventFormat, Filters, Identifier, InterfaceFilter, TemplateFilter,
    WildcardFilter, cumulative_filter,
};

/// An `EventFormat` applying `cumulative` to a single party.
///
/// `verbose` asks Canton for field labels in the returned records; readers that
/// look values up by label need it, ones that only read a contract id do not.
pub fn party_event_format(
    party_id: impl std::fmt::Display,
    cumulative: Vec<CumulativeFilter>,
    verbose: bool,
) -> EventFormat {
    let mut filters_by_party = HashMap::new();
    filters_by_party.insert(party_id.to_string(), Filters { cumulative });

    EventFormat {
        filters_by_party,
        filters_for_any_party: None,
        verbose,
    }
}

/// Every contract the party can see — for reads that narrow client-side because
/// Canton can't express the filter, or that run against a test participant.
pub fn wildcard_filter(include_created_event_blob: bool) -> CumulativeFilter {
    CumulativeFilter {
        identifier_filter: Some(cumulative_filter::IdentifierFilter::WildcardFilter(
            WildcardFilter {
                include_created_event_blob,
            },
        )),
    }
}

/// One template, narrowed Canton-side.
pub fn template_filter(
    template_id: Identifier,
    include_created_event_blob: bool,
) -> CumulativeFilter {
    CumulativeFilter {
        identifier_filter: Some(cumulative_filter::IdentifierFilter::TemplateFilter(
            TemplateFilter {
                template_id: Some(template_id),
                include_created_event_blob,
            },
        )),
    }
}

/// One interface, with the computed view — the shape the token-standard
/// registry contracts are read through.
pub fn interface_filter(
    interface_id: Identifier,
    include_created_event_blob: bool,
) -> CumulativeFilter {
    CumulativeFilter {
        identifier_filter: Some(cumulative_filter::IdentifierFilter::InterfaceFilter(
            InterfaceFilter {
                interface_id: Some(interface_id),
                include_interface_view: true,
                include_created_event_blob,
            },
        )),
    }
}
