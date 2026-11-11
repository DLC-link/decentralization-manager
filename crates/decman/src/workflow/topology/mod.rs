//! Cross-workflow Canton topology helpers.
//!
//! Workflows (onboarding, kick, …) that submit signed topology transactions
//! to Canton share the same write path
//! ([`TopologyManagerWriteServiceClient::sign_transactions`]) and the same
//! transient failure mode while a freshly-restarted participant's local
//! topology store is reconciling. This module owns the retry policy so
//! callers don't reach across workflow boundaries to share it.
//!
//! [`TopologyManagerWriteServiceClient::sign_transactions`]: canton_proto_rs::com::digitalasset::canton::topology::admin::v30::topology_manager_write_service_client::TopologyManagerWriteServiceClient::sign_transactions

mod dns_p2p;
mod legacy_keys;
mod poll;
mod reads;
mod requests;
mod retry;
mod signed_pair;

pub use dns_p2p::{
    DnsP2pArtifactKinds, DnsP2pSubmission, Standing, aggregate_dns_p2p_signatures,
    check_proposal_standing, dedupe_signatures, party_proposal_force_flags, proposal_standing,
    sign_dns_p2p_proposals, submit_dns_then_p2p, submit_p2p,
};
pub use legacy_keys::{retire_legacy_keys, retire_legacy_keys_and_wait};
pub use poll::{PollLog, PollText, Polled, poll_until_logged};
pub(crate) use poll::{poll_log, poll_until};
pub use reads::{
    check_added_signing_keys_signed, fetch_namespace_definition,
    fetch_namespace_definition_at_head, fetch_namespace_definition_at_head_in, fetch_p2p_history,
    fetch_p2p_mapping, fetch_p2p_mapping_at_head, fetch_p2p_mapping_at_head_in,
    fetch_party_to_key_mapping, fetch_signed_namespace_definition,
    fetch_signed_party_to_participant,
};
pub use requests::{
    add_transactions_request, head_state_query, head_state_query_in, history_query, p2p_rows,
    p2p_v30, synchronizer_store_id,
};
pub use retry::{authorize_with_topology_retry, sign_transactions_with_topology_retry};
pub use signed_pair::{send_signed_pair, store_signed_pairs};
