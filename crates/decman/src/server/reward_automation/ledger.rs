// Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! The shared decoded active-contract read.

use canton_proto_rs::com::daml::ledger::api::v2::{
    GetActiveContractsRequest, GetLedgerEndRequest, Identifier, Record,
    get_active_contracts_response::ContractEntry,
};

use crate::{canton_id::CantonId, config::NodeConfig, utils};

use crate::server::event_filters::{
    interface_filter, party_event_format, template_filter, wildcard_filter,
};

// ============================================================================
// Shared decoded ACS read
// ============================================================================

/// A Daml module path, e.g. `Splice.Amulet`.
pub(crate) struct Module<'a>(pub &'a str);

/// A Daml template or interface name, e.g. `RewardCouponV2`.
pub(crate) struct Entity<'a>(pub &'a str);

/// What a [`for_each_active_created`] read selects.
///
/// One value rather than four positional arguments, and the two same-typed
/// halves are wrapped: `package_id`, module and entity were all `&str`, so a
/// transposition compiled, passed clippy, and yielded a filter matching
/// nothing — `Ok(vec![])`, which a caller reads as "the party holds no such
/// contracts". [`Module`] and [`Entity`] make each of those swaps a type
/// error instead, so the hazard is closed rather than narrowed.
pub(crate) struct ContractFilter<'a> {
    pub package_id: &'a str,
    pub module: &'a str,
    pub entity: &'a str,
    /// Read the interface view instead of the concrete template's arguments.
    pub interface_view: bool,
    /// Interface reads only: keep a contract only if this concrete template
    /// created it.
    pub implementer: Option<(&'a str, &'a str)>,
}

impl<'a> ContractFilter<'a> {
    /// A concrete-template read.
    pub fn template(package_id: &'a str, module: Module<'a>, entity: Entity<'a>) -> Self {
        Self {
            package_id,
            module: module.0,
            entity: entity.0,
            interface_view: false,
            implementer: None,
        }
    }

    /// An interface read, admitting every implementation until narrowed by
    /// [`Self::implemented_by`].
    pub fn interface(package_id: &'a str, module: Module<'a>, entity: Entity<'a>) -> Self {
        Self {
            package_id,
            module: module.0,
            entity: entity.0,
            interface_view: true,
            implementer: None,
        }
    }

    /// Keep only contracts created by this concrete template.
    pub fn implemented_by(mut self, module: Module<'a>, entity: Entity<'a>) -> Self {
        self.implementer = Some((module.0, entity.0));
        self
    }
}

/// A single decoded `GetActiveContracts` read, handed to `visit` one contract
/// at a time.
///
/// For `interface_view = false` this uses a `TemplateFilter` (or, under
/// `test_mode`, a `WildcardFilter` with in-memory template matching, since mock
/// auth lacks `TemplateFilter` permission) and visits each created event's
/// `create_arguments` `Record`. For `interface_view = true` it uses an
/// `InterfaceFilter { include_interface_view: true }` and visits the decoded
/// interface-view `Record`. Field labels are populated because the request is
/// `verbose`.
///
/// `visit` receives `(contract_id, created-event offset, &Record)`. The offset
/// orders contracts by creation, which [`active_delegation`] uses to pick the
/// newest of several. An `Err` from `visit` ends the read and propagates.
///
/// **Streaming is the point.** A verbose `Record` carries every field label as
/// well as its values, and a decparty's coupon ACS is unbounded, so collecting
/// the records first and narrowing afterwards makes peak memory track the whole
/// ACS rather than the result. That OOMKilled a devnet node at a 2Gi limit
/// every few minutes (#413). Each `Record` is dropped as soon as `visit`
/// returns, so a caller that folds into its own target type — a
/// [`CouponInfo`], a party id, a running `min` — holds only that.
///
/// `implementer` applies to interface reads only: `Some((module, entity))`
/// keeps a contract only if that concrete template created it. An interface
/// read otherwise admits **every** implementation, which is wrong whenever the
/// consumer needs a specific one — see [`unassigned_coupons`].
///
/// Modeled on `queries::fetch_proposal_infos`.
pub(crate) async fn for_each_active_created<F>(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    test_mode: bool,
    filter: ContractFilter<'_>,
    mut visit: F,
) -> anyhow::Result<()>
where
    F: FnMut(&str, i64, &Record) -> anyhow::Result<()>,
{
    let ContractFilter {
        package_id,
        module,
        entity,
        interface_view,
        implementer,
    } = filter;
    let mut state_client = utils::create_state_client(config, token).await?;

    let ledger_end = state_client
        .get_ledger_end(tonic::Request::new(GetLedgerEndRequest {
            synchronizer_id: vec![],
        }))
        .await?
        .into_inner()
        .offset;

    let identifier = || Identifier {
        package_id: package_id.to_string(),
        module_name: module.to_string(),
        entity_name: entity.to_string(),
    };
    let filter = if interface_view {
        interface_filter(identifier(), false)
    } else if test_mode {
        wildcard_filter(false)
    } else {
        template_filter(identifier(), false)
    };

    let acs_request = GetActiveContractsRequest {
        include_incomplete_reassignments: false,
        active_at_offset: ledger_end,
        event_format: Some(party_event_format(party_id, vec![filter], true)),
        stream_continuation_token: None,
    };

    let mut stream = state_client
        .get_active_contracts(tonic::Request::new(acs_request))
        .await?
        .into_inner();

    while let Some(response) = stream.message().await? {
        if let Some(ContractEntry::ActiveContract(active)) = response.contract_entry
            && let Some(created) = active.created_event
        {
            if interface_view {
                // An interface read admits every implementation. Keep only the
                // one the caller named, so the reader cannot hand downstream a
                // contract the exercise will refuse.
                if let Some((impl_module, impl_entity)) = implementer
                    && !created.template_id.as_ref().is_some_and(|t| {
                        t.module_name == impl_module && t.entity_name == impl_entity
                    })
                {
                    continue;
                }
                let Some(view) = created.interface_views.iter().find(|v| {
                    v.interface_id
                        .as_ref()
                        .is_some_and(|id| id.module_name == module && id.entity_name == entity)
                }) else {
                    continue;
                };
                if let Some(rec) = view.view_value.as_ref() {
                    visit(&created.contract_id, created.offset, rec)?;
                }
            } else {
                // Wildcard (test mode) returns every template; keep only the
                // requested one. Match on module/entity — the package alias
                // (`#…`) is resolved to a concrete hash on the wire.
                if test_mode
                    && !created
                        .template_id
                        .as_ref()
                        .is_some_and(|t| t.module_name == module && t.entity_name == entity)
                {
                    continue;
                }
                if let Some(rec) = created.create_arguments.as_ref() {
                    visit(&created.contract_id, created.offset, rec)?;
                }
            }
        }
    }

    Ok(())
}

/// Collect a **bounded** read into memory: every entry as
/// `(contract_id, created-event offset, Record)`.
///
/// Only for a read whose result size is bounded by construction — a
/// per-decparty template read such as `CouponReassignmentDelegation`, where the
/// ledger holds one contract and the propose-time guard keeps it that way.
/// Anything that scales with a decparty's activity must use
/// [`for_each_active_created`] and fold as it streams; see its docs for why
/// (#413).
pub(super) async fn bounded_created_records(
    config: &NodeConfig,
    party_id: &CantonId,
    token: Option<String>,
    test_mode: bool,
    filter: ContractFilter<'_>,
) -> anyhow::Result<Vec<(String, i64, Record)>> {
    let mut out = Vec::new();
    for_each_active_created(
        config,
        party_id,
        token,
        test_mode,
        filter,
        |cid, offset, rec| {
            out.push((cid.to_string(), offset, rec.clone()));
            Ok(())
        },
    )
    .await?;
    Ok(out)
}
