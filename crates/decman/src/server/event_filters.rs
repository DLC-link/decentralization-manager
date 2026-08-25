//! Builders for the ledger-API `EventFormat` every ACS, update and
//! by-contract-id read carries.
//!
//! A read is "filter + extractor". These cover the filter half so each fetcher
//! is left with only the part specific to it: which templates it wants and what
//! it pulls out of the events.

pub(crate) use decman_lib::framework::filters::*;
