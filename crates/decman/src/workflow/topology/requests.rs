use canton_proto_rs::com::digitalasset::canton::{
    protocol::v30::{PartyToParticipant, SignedTopologyTransaction},
    topology::admin::v30::{
        AddTransactionsRequest, BaseQuery, BaseResult, StoreId, Synchronizer, base_query,
        list_party_to_participant_response::{self, result::Item as P2pItem},
        store_id, synchronizer,
    },
};

use crate::{error::Result, workflow::topology_version};

/// A [`StoreId`] targeting the physical synchronizer store — the target every
/// topology read and write in these workflows uses.
pub fn synchronizer_store_id(synchronizer_id: &str) -> StoreId {
    StoreId {
        store: Some(store_id::Store::Synchronizer(Synchronizer {
            kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id.to_string())),
        })),
    }
}

/// A head-state [`BaseQuery`] against the synchronizer store — the boilerplate
/// every topology read in these workflows shares.
pub fn head_state_query(synchronizer_id: &str) -> BaseQuery {
    head_state_query_in(synchronizer_store_id(synchronizer_id))
}

/// A head-state [`BaseQuery`] against any store, such as the temporary store
/// a proposal is signed in.
pub fn head_state_query_in(store: StoreId) -> BaseQuery {
    BaseQuery {
        store: Some(store),
        proposals: false,
        operation: 0,
        time_query: Some(base_query::TimeQuery::HeadState(())),
        filter_signed_key: String::new(),
        protocol_version: None,
        client_version: None,
    }
}

/// A [`BaseQuery`] over the synchronizer store's whole history, so a caller
/// can see superseded serials rather than only what is currently in force.
///
/// `until: None` means "up to now"; `from: None` means "from the beginning".
pub fn history_query(synchronizer_id: &str) -> BaseQuery {
    BaseQuery {
        store: Some(synchronizer_store_id(synchronizer_id)),
        proposals: false,
        operation: 0,
        time_query: Some(base_query::TimeQuery::Range(base_query::TimeRange {
            from: None,
            until: None,
        })),
        filter_signed_key: String::new(),
        protocol_version: None,
        client_version: None,
    }
}

/// The `v30` mapping of a `ListPartyToParticipant` row.
///
/// A synchronizer on protocol version 37 returns a `v31` mapping, which is
/// converted with [`topology_version::p2p_from_v31`]. Skipping such a row
/// would report a mapping as missing.
///
/// # Errors
///
/// Errors when the row holds a `v31` mapping that has no `v30` form.
pub fn p2p_v30(item: P2pItem) -> Result<PartyToParticipant> {
    match item {
        P2pItem::V30(mapping) => Ok(mapping),
        P2pItem::V31(mapping) => topology_version::p2p_from_v31(mapping),
    }
}

/// The rows of a `ListPartyToParticipant` response that carry a mapping, each
/// with its context. See [`p2p_v30`].
///
/// # Errors
///
/// Errors when a row holds a `v31` mapping that has no `v30` form.
pub fn p2p_rows(
    results: Vec<list_party_to_participant_response::Result>,
) -> Result<Vec<(Option<BaseResult>, PartyToParticipant)>> {
    results
        .into_iter()
        .filter_map(|row| Some((row.context, row.item?)))
        .map(|(context, item)| Ok((context, p2p_v30(item)?)))
        .collect()
}

/// An [`AddTransactionsRequest`] submitting a single signed transaction to the
/// synchronizer store.
///
/// `force_changes` carries the [`ForceFlag`]s the transaction needs to pass
/// Canton's validation. A party proposal is signed in a temporary store (see
/// [`super::super::proposal_store`]), so this request is the first time the
/// synchronizer store validates it.
///
/// [`ForceFlag`]: canton_proto_rs::com::digitalasset::canton::topology::admin::v30::ForceFlag
pub fn add_transactions_request(
    synchronizer_id: &str,
    transaction: SignedTopologyTransaction,
    force_changes: Vec<i32>,
) -> AddTransactionsRequest {
    AddTransactionsRequest {
        transactions: vec![transaction],
        force_changes,
        store: Some(synchronizer_store_id(synchronizer_id)),
        wait_to_become_effective: None,
    }
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::protocol::{
        v30::{enums, party_to_participant},
        v31::{self, party_to_participant as v31_p2p},
    };

    use super::*;

    #[test]
    fn head_state_query_targets_synchronizer_head_state() {
        let query = head_state_query("global::1220abcd::34-0");
        assert!(!query.proposals);
        assert!(matches!(
            query.time_query,
            Some(base_query::TimeQuery::HeadState(()))
        ));
        match query.store.and_then(|s| s.store) {
            Some(store_id::Store::Synchronizer(s)) => assert_eq!(
                s.kind,
                Some(synchronizer::Kind::PhysicalId(
                    "global::1220abcd::34-0".to_string()
                ))
            ),
            other => panic!("expected synchronizer store, got {other:?}"),
        }
    }

    fn p2p_row(serial: i32, item: Option<P2pItem>) -> list_party_to_participant_response::Result {
        list_party_to_participant_response::Result {
            context: Some(BaseResult {
                serial,
                ..Default::default()
            }),
            item,
        }
    }

    #[test]
    fn p2p_rows_keeps_v30_rows_with_their_context() {
        let mapping = PartyToParticipant {
            party: "alice::1220aa".to_string(),
            ..Default::default()
        };
        let rows = p2p_rows(vec![
            p2p_row(1, None),
            p2p_row(2, Some(P2pItem::V30(mapping.clone()))),
        ])
        .expect("v30 rows convert");
        assert_eq!(rows.len(), 1, "a row without a mapping is dropped");
        assert_eq!(rows[0].0.as_ref().map(|c| c.serial), Some(2));
        assert_eq!(rows[0].1, mapping);
    }

    fn v31_row_hosted_in(state: v31_p2p::hosting_participant::HostingState) -> Option<P2pItem> {
        Some(P2pItem::V31(v31::PartyToParticipant {
            party: "alice::1220aa".to_string(),
            threshold: 1,
            participants: vec![v31_p2p::HostingParticipant {
                participant_uid: "p1::1220bb".to_string(),
                permission: enums::ParticipantPermission::Submission as i32,
                state: state as i32,
            }],
            party_signing_keys: None,
        }))
    }

    #[test]
    fn p2p_rows_converts_a_v31_row() -> Result {
        let rows = p2p_rows(vec![p2p_row(
            2,
            v31_row_hosted_in(v31_p2p::hosting_participant::HostingState::Active),
        )])?;

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0.as_ref().map(|context| context.serial), Some(2));
        assert_eq!(
            rows[0].1,
            PartyToParticipant {
                party: "alice::1220aa".to_string(),
                threshold: 1,
                participants: vec![party_to_participant::HostingParticipant {
                    participant_uid: "p1::1220bb".to_string(),
                    permission: enums::ParticipantPermission::Submission as i32,
                    onboarding: None,
                }],
                party_signing_keys: None,
            }
        );
        Ok(())
    }

    #[test]
    fn p2p_rows_refuses_a_v31_row_it_cannot_convert() {
        let error = p2p_rows(vec![
            p2p_row(1, Some(P2pItem::V30(PartyToParticipant::default()))),
            p2p_row(
                2,
                v31_row_hosted_in(v31_p2p::hosting_participant::HostingState::Nominated),
            ),
        ])
        .expect_err("a v31 row must not be skipped")
        .to_string();
        assert!(error.contains("NOMINATED"), "{error}");
    }
}
