//! Contracts driver (design sections 6, D7): create contracts for a party
//! through `SubmissionRound`s.
//!
//! Stub. The step bodies land with the contracts agent; every tick logs and
//! returns so the observer loop keeps running.

use std::collections::BTreeSet;

use anyhow::Result;
use common::types::{WorkflowKind, WorkflowRun};

use super::{KindDriver, MemberVariant, OnLedger, ProposalExtras, RunMeta, StartRequest, TickCtx};

pub struct Contracts;

pub const COORDINATOR_STEPS: &[&str] = &[
    "WaitingForAcceptances",
    "AwaitDars",
    "PrepareSubmissions",
    "CollectSignatures",
    "ExecuteSubmissions",
    "Complete",
];

pub const MEMBER_STEPS: &[&str] = &["UploadDars", "SignSubmissions", "Complete"];

/// The package names a contracts request creates under.
///
/// TODO(contracts agent): resolve a package hash to its name through the
/// package inventory; today a `#name` ref is stripped and a hash is kept
/// as-is.
pub fn package_names_of(req: &StartRequest) -> Vec<String> {
    let StartRequest::Contracts { contracts, .. } = req else {
        return Vec::new();
    };
    let names: BTreeSet<String> = contracts
        .iter()
        .map(|c| c.package_id.trim_start_matches('#').to_string())
        .filter(|n| !n.is_empty())
        .collect();
    names.into_iter().collect()
}

impl KindDriver for Contracts {
    fn kind() -> WorkflowKind {
        WorkflowKind::Contracts
    }

    fn coordinator_steps() -> &'static [&'static str] {
        COORDINATOR_STEPS
    }

    fn member_steps(_variant: Option<MemberVariant>) -> &'static [&'static str] {
        MEMBER_STEPS
    }

    /// The proposal names the packages members check root creates against
    /// (design D7). No proposer key material: rounds are signed with the
    /// party's `party_signing_keys`.
    async fn prepare(_ol: &OnLedger, req: &StartRequest) -> Result<ProposalExtras> {
        Ok(ProposalExtras {
            package_names: package_names_of(req),
            ..ProposalExtras::default()
        })
    }

    async fn tick_coordinator(
        _ctx: &TickCtx<'_>,
        run: &WorkflowRun,
        _meta: &RunMeta,
    ) -> Result<()> {
        super::not_implemented(run);
        Ok(())
    }

    async fn tick_member(_ctx: &TickCtx<'_>, run: &WorkflowRun, _meta: &RunMeta) -> Result<()> {
        super::not_implemented(run);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use common::{api::ContractDefinition, canton_id::CantonId};

    use super::*;

    #[test]
    fn package_names_strip_the_ref_prefix_and_dedupe() {
        let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
        let contract = |package_id: &str| ContractDefinition {
            id: "c".into(),
            name: "n".into(),
            package_id: package_id.into(),
            module_name: "M".into(),
            entity_name: "E".into(),
            fields: vec![],
        };
        let req = StartRequest::Contracts {
            dec_party_id: CantonId::parse(&format!("cbtc::{ns}")).expect("id"),
            participant_ids: vec![],
            participant_parties: vec![],
            operator_party: CantonId::parse(&format!("op::{ns}")).expect("id"),
            contracts: vec![
                contract("#governance-core-v1"),
                contract("governance-core-v1"),
                contract("#governance-action-v1"),
                contract(""),
            ],
            instance_name: "i".into(),
        };
        assert_eq!(
            package_names_of(&req),
            vec!["governance-action-v1", "governance-core-v1"]
        );
    }
}
