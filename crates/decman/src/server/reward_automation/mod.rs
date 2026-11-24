// Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! CIP-104 Mode A reward-assignment automation (delegation model).
//!
//! A decparty earns traffic-based app rewards as `RewardCouponV2` coupons that
//! carry `beneficiary = None` and expire unclaimed unless automation
//! reassigns them. Under the delegation model, a single threshold governance
//! vote sets the split and the authorized assigners once, into a
//! `CouponReassignmentDelegation`; from then on, any one listed assigner
//! executes each periodic reassignment directly — no per-round voting. This
//! module holds that automation:
//!
//! * [`for_each_active_created`] — the one shared **decoded** ACS read. Unlike
//!   `queries::query_contracts_by_template` (cid + base64 blob, no fields) and
//!   `queries::get_contracts` (metadata only), this issues a direct
//!   `StateServiceClient` `GetActiveContracts` — modeled on
//!   `queries::fetch_proposal_infos` — and visits the decoded create-arguments
//!   `Record` (template reads) or the decoded interface-view `Record`
//!   (interface reads), one contract at a time. Callers fold each record into
//!   their own type as it arrives; a coupon ACS does not fit in memory as
//!   `Record`s (#413).
//! * [`active_delegation`] — reads the decparty's active
//!   `CouponReassignmentDelegation` (its cid, authorized `assigners`, and how
//!   many beneficiaries the split names, to size a chunk); the split's contents
//!   are not read here — `Delegation_Assign` enforces them in DAML.
//! * [`unassigned_coupons`] — reads the decparty's unassigned `RewardCoupon`
//!   interface views.
//! * [`run_reassign_once`] — one reassign sweep: assigns every assignable
//!   unassigned coupon via successive chunked `Delegation_Assign` transactions.
//! * [`reward_status`] — what the dec party page shows: whether the rewards DAR
//!   resolves, the active delegation, and whether it names this node.
//!
//! The pure record decoders, selection and chunk sizing ([`select_assignable`],
//! [`chunk_size`]) are unit-tested here; the gRPC reads and command submission
//! are exercised by the localnet and devnet integration tests.

mod assign;
mod coupons;
mod delegation;
mod ledger;
mod metrics;
mod scheduler;
#[cfg(test)]
mod test_support;

pub(crate) use delegation::{active_delegation, active_delegations, reward_status};
pub(crate) use metrics::register_metrics;
pub(crate) use scheduler::run_reward_automation_loop;
