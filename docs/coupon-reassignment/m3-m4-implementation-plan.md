# CIP-104 Coupon-Reassignment Automation (Mode A) — M3+M4 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the per-node background automation that discovers a decparty's unassigned `RewardCouponV2` coupons, proposes assigning them to the decparty's governance-configured beneficiary split, and — on every member node — auto-confirms that proposal *only if* it matches the on-ledger split, so the coupons get assigned (and each beneficiary can self-mint) without any human clicking "confirm".

**Architecture:** The split lives on-ledger in a new **`RewardSplitConfig`** contract, set through governance (a `SetRewardSplit` action). A per-node background loop runs two roles against each decparty that has a config: a **proposer** (finds unassigned coupons, batches them, proposes `AssignRewardBeneficiaries`) and a **confirmer** (reads a pending proposal, checks its beneficiaries equal the configured split and its coupons are still unassigned, then submits this node's `GovernanceConfirmation`). Correctness comes from the confirmer's check against the on-ledger split (L3), never from trusting the proposer.

**Tech Stack:** DAML (SDK 3.4.11, `dpm`), Rust (actix-web backend, `tokio`, `tonic`/`CommandServiceClient`), splice DARs `splice-api-reward-assignment-v1-1.0.0` + `splice-amulet-0.1.19` (already vendored by M1). Builds on the M1+M2 branch.

## Global Constraints

- **Branch / base (DONE 2026-07-20):** the branch `feat/governance/coupon-reassignment-automation` (worktree `/Users/gyorgybalazsi/dm-reward-cranker`) is **rebased onto Robert's #256 branch `feat/accept-external-party-setup`**, and PR #255's base is retargeted to it (clean stacked diff). `governance-rewards` is now **`0.1.2`** — it contains `AcceptExternalPartySetup` (#256) + `AssignRewardBeneficiaries` (M1); `RewardSplitConfig`/`SetRewardSplit` (this plan) also land at `0.1.2`. Both test packages' dar refs point to `-0.1.2.dar`; the `0.1.2` DAR is shipped in `releases/v1/`. Rebase verified green (cargo test, tsc, dpm build --all + all reward scripts). **When #256 merges to `main`:** rebase this branch onto `main` and retarget PR #255's base back to `main` (GitHub may auto-retarget on #256 merge).
- **TS bindings are gitignored + generated:** `crates/decman/frontend/src/types.generated.ts` is **gitignored**, regenerated from the Rust DTOs by `cargo run --features typegen --bin gen-types` (a.k.a. `just gen-types`) — run it before any frontend build after changing `ProposalType`. The propose-form `switch` in `GovernanceSection.tsx` has a `default:` that throws, so automation-only variants (`assign_reward_beneficiaries`, `set_reward_split`) — which have no UI form by design — don't break the build; **do not add UI cases for them**.
- **Split source = Option B, Mode-A-owned (per Robert 2026-07-20 + Gyorgy decision):** the split is read from an on-ledger `RewardSplitConfig` contract, set by the decparty's own governance. **No** utility-registry `getBeneficiaries` composition, **no** operator-cut math — the split is exactly what governance configured (Robert: "the split will be whatever is configured"). If Robert later ships a shared reward-config template, `SplitSource` (Task 4) is the single swap point.
- **No mode selector (per Robert 2026-07-20):** there is no A/B gate. **Enablement = presence of a `RewardSplitConfig` for the decparty.** No mode flag, no DB migration. Tick cadence is one global interval from `NodeConfig`.
- **Confirmer validation is exact-match:** proposed `newBeneficiaries` == configured `[RewardBeneficiary]` (as a set, with **exact `Decimal` equality** on percentages — DAML `Decimal` is exact fixed-point, so use no float tolerance anywhere; see Tasks 2 + 5) **and** every target coupon is currently unassigned with `provider == decparty`. Anything else → refuse (log, never confirm). Auto-confirmation is **default-deny with an allowlist of one** action label (`"AssignRewardBeneficiaries"`); every other action still requires a human.
- **Reuse, don't duplicate (verified anchors, `crates/decman/src/server/`):**
  - Background loop shape: `run_peer_ping_loop` (`mod.rs:1892`), spawned via `run_heartbeat` at `start_server` (`mod.rs:948`); iterate `AppState.party_credentials: Arc<RwLock<Vec<PartyCredentials>>>` (`mod.rs:107`).
  - Confirm submission is already reusable: `execute_confirm_action(&NodeConfig, &ConfirmActionRequest, &token, &member_party_id, &PackageConfig)` (`governance.rs:2094`, private → make `pub(crate)`).
  - Propose submission is **inline** in the `propose_action` handler (`governance.rs`~1291–1470) — extract it (Task 3).
  - Proposal discovery: `get_governance_confirmations(...) -> (Vec<GovernanceAction>, Vec<DomainGovernanceAction>)` (`queries.rs:630`, pub). `DomainGovernanceAction` (`types.rs:761`) carries `proposal_cid`, `action_label`, `confirmations`, `confirmation_count`, `can_execute`, `orphaned` — **no typed action fields**, so the confirmer must read the concrete proposal separately (Task 6).
  - **Decoded ACS reads — use the `fetch_proposal_infos` pattern, NOT `query_contracts_by_template`.** Verified: `query_contracts_by_template` (`queries.rs:3044`) returns `ContractWithBlob` (cid + base64 blob, **no decoded fields**), and `get_contracts`'s `ContractInfo` (`common/src/types.rs:66`) carries **only metadata** (`contract_id`, `template_id`, `package_id`, …, **no payload**). To read decoded fields (`beneficiaries`, coupon `provider`/`beneficiary`/`amount`/`expiresAt`, assign coupons) the automation must follow `fetch_proposal_infos` (`queries.rs:1392`): a direct `StateServiceClient` `GetActiveContracts` with a `TemplateFilter` (concrete templates → read `created_event.create_arguments : Record`) or `InterfaceFilter { include_interface_view: true }` (interfaces → read the decoded interface view). Task 4 builds one shared helper for this.
  - `query_contracts_by_template` is still fine where only the cid is needed (it isn't, here).
  - Per-party creds: `get_party_credentials(&web::Data<AppState>, &CantonId) -> Option<(String, CantonId)>` (`governance.rs:2065`, private → `pub(crate)`).
  - **Governance threshold + rules cid:** the active-`GovernanceRules` resolution at `governance.rs:115–138` yields `(rules_contract_id, gov_state_threshold)`. This **governance** threshold — *not* the topology threshold from `get_party_threshold` (`governance.rs:2012`) — is what `get_governance_confirmations` and the execute gate use (spec §5). Extract it to a `pub(crate)` helper (Task 3); the automation does not call `get_party_threshold` at all.
  - No pooled ledger client; each submitter builds a fresh `tonic::transport::Channel::from_shared(config.ledger_api_url())?.connect()` then `CommandServiceClient::new(channel)`. Reuse this idiom; do not add a pool.
- **Reuse M1/M2 types:** `ProposalType::AssignRewardBeneficiaries { primary_coupon: CantonId, additional_coupons: Vec<CantonId>, new_beneficiaries: Vec<RewardBeneficiary> }` and `RewardBeneficiary { beneficiary: CantonId, percentage: DamlDecimal }` already exist on the branch (M2).
- TDD, DRY, YAGNI, frequent commits. `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test -p decman`, and `dpm build --all` must stay clean.

## Prerequisites (confirm before Task 1)

1. M1+M2 are on the branch (`AssignRewardBeneficiaries` DAML action + `ProposalType` + serializer + tests), green.
2. Branch is rebased onto Robert's #256 branch; `governance-rewards` at `0.1.2`; `dpm build --all`, `cargo test -p decman`, and frontend `tsc` all green (done 2026-07-20). After adding a new `ProposalType` variant (Task 2), run `cargo run --features typegen --bin gen-types` to refresh the gitignored TS bindings before building the frontend.
3. `splice-api-reward-assignment-v1-1.0.0.dar` + `splice-amulet-0.1.19.dar` vendored in `daml/dars/` (from M1 Task 0).

## File Structure

- **Create** `daml/governance-rewards/daml/Governance/Rewards/RewardSplitConfig.daml` — the on-ledger split, **keyless** (no governance package uses contract keys — verified `grep maintainer daml/governance-*` is empty; follow that convention). Singleton is maintained by replace-by-cid in `SetRewardSplit`, plus a defensive single-config check in the reader (Task 4). One responsibility: hold the configured `[RewardBeneficiary]`.
- **Create** `daml/governance-rewards/daml/Governance/Rewards/SetRewardSplit.daml` — the `GovernableAction` that creates/replaces the config.
- **Create** `daml/governance-rewards-assign-test/daml/Governance/Rewards/TestSetRewardSplit.daml` — DAML tests (reuses the M1 assign-test package + `AssignTestUtils`).
- **Modify** `daml/governance-rewards/daml.yaml` — version `0.1.2` (no new deps; reward-assignment already present from M1).
- **Modify** `crates/decman/src/server/types.rs` — `ProposalType::SetRewardSplit` variant + `validate()` arm; extract `validate_reward_beneficiaries` helper shared with `AssignRewardBeneficiaries`.
- **Modify** `crates/decman/src/server/action_serializer.rs` — `SetRewardSplit` arm + round-trip test.
- **Modify** `crates/decman/src/server/handlers/governance.rs` — extract `pub(crate) submit_proposal(...)` and `pub(crate) resolve_active_governance_rules(...)` (from `governance.rs:115–138`); widen `execute_confirm_action`, `get_party_credentials`, `packages()` to `pub(crate)`.
- **Create** `crates/decman/src/server/reward_automation/mod.rs` — the automation module: `SplitSource`, coupon reader, proposal parse-back, auto-confirm engine, proposer, confirmer, and the loop. (One new module directory; ask already granted via this plan.)
- **Modify** `crates/decman/src/server/mod.rs` — register the loop in `start_server` (share the existing `web::Data<AppState>`).
- **Modify** `crates/decman/src/config.rs` — add the `NodeConfig` tick-interval field (Task 9).
- **Modify** `crates/decman/src/server/mod.rs` module list (`mod reward_automation;`).

---

### Task 1: DAML — `RewardSplitConfig` + `SetRewardSplit` action + tests

**Files:**
- Create: `daml/governance-rewards/daml/Governance/Rewards/RewardSplitConfig.daml`
- Create: `daml/governance-rewards/daml/Governance/Rewards/SetRewardSplit.daml`
- Create: `daml/governance-rewards-assign-test/daml/Governance/Rewards/TestSetRewardSplit.daml`
- Modify: `daml/governance-rewards/daml.yaml` (version `0.1.2`)

**Interfaces:**
- Consumes: `Splice.Api.RewardAssignmentV1 (RewardBeneficiary(..))`; `Governance.Action (GovernableAction, GovernableActionView(..))`; the M1 assign-test helpers (`allocateRewardsTestParties`, `createTestGovernance`, `confirmAndExecute`).
- Produces: `template RewardSplitConfig with governanceParty : Party; beneficiaries : [RewardBeneficiary]` (**keyless**); `template SetRewardSplit with governanceParty : Party; proposer : Party; priorConfig : Optional (ContractId RewardSplitConfig); beneficiaries : [RewardBeneficiary]` — field order `governanceParty, proposer, priorConfig, beneficiaries` is what Task 2's serializer reproduces. `priorConfig` is the cid of the config being replaced (`None` for the first set), archived at execute so at most one config accumulates.

- [ ] **Step 1: Write the failing happy-path test.** Create `TestSetRewardSplit.daml`:

```haskell
module Governance.Rewards.TestSetRewardSplit where

import Daml.Script
import Splice.Api.RewardAssignmentV1 (RewardBeneficiary(..))
import Governance.Action (GovernableAction)
import Governance.Rules            -- GovernanceRules_ExecuteConfirmedAction (negative test), mirrors M1 test
import Governance.Rewards.SetRewardSplit
import Governance.Rewards.RewardSplitConfig
import Governance.Rewards.AssignTestUtils

test_set_reward_split_creates_config = script do
  parties <- allocateRewardsTestParties
  rulesCid <- createTestGovernance parties
  let gp = parties.governanceParty
  let split = [ RewardBeneficiary with beneficiary = parties.member2; percentage = 0.8
              , RewardBeneficiary with beneficiary = parties.member3; percentage = 0.2 ]
  propCid <- submit parties.member1 $ createCmd SetRewardSplit with
    governanceParty = gp; proposer = parties.member1; priorConfig = None; beneficiaries = split
  _ <- confirmAndExecute parties rulesCid (toInterfaceContractId propCid)
         [parties.member1, parties.member2] parties.member1
  configs <- query @RewardSplitConfig gp
  assertMsg "one config exists" (length configs == 1)
  -- `configs : [(ContractId RewardSplitConfig, RewardSplitConfig)]`
  assertMsg "split matches" (map (\(_, c) -> c.beneficiaries) configs == [split])
  pure ()

test_set_reward_split_replaces_existing = script do
  parties <- allocateRewardsTestParties
  rulesCid <- createTestGovernance parties
  let gp = parties.governanceParty
  let mk prior p = submit parties.member1 $ createCmd SetRewardSplit with
        governanceParty = gp; proposer = parties.member1; priorConfig = prior
        beneficiaries = [RewardBeneficiary with beneficiary = p; percentage = 1.0]
  p1 <- mk None parties.member2
  _ <- confirmAndExecute parties rulesCid (toInterfaceContractId p1) [parties.member1, parties.member2] parties.member1
  -- fetch the just-created config's cid to pass as priorConfig (keyless replace)
  [(oldCid, _)] <- query @RewardSplitConfig gp
  p2 <- mk (Some oldCid) parties.member3
  _ <- confirmAndExecute parties rulesCid (toInterfaceContractId p2) [parties.member1, parties.member2] parties.member1
  configs <- query @RewardSplitConfig gp
  assertMsg "still singleton after replace" (length configs == 1)
  pure ()
```

- [ ] **Step 2: Run it; verify it fails (templates don't exist).** Run: `cd daml/governance-rewards-assign-test && dpm test`. Expected: FAIL — `SetRewardSplit`/`RewardSplitConfig` not in scope. (Register the new test module by adding it to the package if the package lists modules explicitly; otherwise it's picked up automatically.)

- [ ] **Step 3: Write `RewardSplitConfig.daml`:**

```haskell
-- Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- | The decparty's governance-configured reward split, held on-ledger so the
-- auto-confirmer can validate every assignment against it (spec §2, §8, §12).
-- Keyless (no governance package uses contract keys); singleton is maintained by
-- SetRewardSplit's replace-by-cid and the reader's defensive single-config check.
module Governance.Rewards.RewardSplitConfig where

import Splice.Api.RewardAssignmentV1 (RewardBeneficiary)

template RewardSplitConfig
  with
    governanceParty : Party
    beneficiaries : [RewardBeneficiary]
      -- ^ percentages in (0,1], sum 1.0, <= 20 (guarded at set time).
  where
    signatory governanceParty
```

- [ ] **Step 4: Write `SetRewardSplit.daml`:**

```haskell
-- Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- | GovernableAction that sets (replacing any existing) the decparty's
-- RewardSplitConfig. Runs with governanceParty authority via governance execute.
module Governance.Rewards.SetRewardSplit where

import DA.Foldable (forA_)
import Splice.Api.RewardAssignmentV1 (RewardBeneficiary(..))
import Governance.Rewards.RewardSplitConfig
import Governance.Action

template SetRewardSplit
  with
    governanceParty : Party
    proposer : Party
    priorConfig : Optional (ContractId RewardSplitConfig)
      -- ^ cid of the config to replace (None for the first set); archived at execute.
    beneficiaries : [RewardBeneficiary]
  where
    signatory proposer
    observer governanceParty

    interface instance GovernableAction for SetRewardSplit where
      view = GovernableActionView with
        governanceParty
        proposer
        actionLabel = "SetRewardSplit"
        description = "Set the reward beneficiary split for the decparty."

      executeImpl = do
        -- execute-time guards (a direct ledger submit bypasses the Rust boundary)
        assertMsg "beneficiaries must not be empty" (not (null beneficiaries))
        assertMsg "at most 20 beneficiaries" (length beneficiaries <= 20)
        -- Decimal is exact fixed-point: require an exact sum (matches the Rust
        -- boundary check and split_matches, which also use exact Decimal equality).
        let total = sum (map (.percentage) beneficiaries)
        assertMsg "percentages must sum to 1.0" (total == 1.0)
        -- keyless replace: archive the prior config (if any), then create the new one.
        -- `Optional` is `Foldable`, so `forA_` runs `archive` only when `Some` (matches M1's idiom).
        forA_ priorConfig archive
        _ <- create RewardSplitConfig with governanceParty; beneficiaries
        pure ()
```

**Note (verify at first build):** `SetRewardSplit` archives `priorConfig` under `governanceParty` authority — `RewardSplitConfig`'s only signatory is `governanceParty`, which the governed execute carries, so the archive is authorized. If a stale/ wrong `priorConfig` cid is passed, the archive fails and the whole execute aborts (safe — no partial state).

- [ ] **Step 5: Run the tests; verify pass.** Run: `cd daml/governance-rewards-assign-test && dpm test`. Expected: both scripts PASS. Fix the `beneficiaries` accessor in Step 1's assert to the actual field-projection syntax if the build flags it.

- [ ] **Step 6: Add a negative test** (empty split rejected at execute):

```haskell
test_set_reward_split_empty_fails = script do
  parties <- allocateRewardsTestParties
  rulesCid <- createTestGovernance parties
  let gp = parties.governanceParty
  prop <- submit parties.member1 $ createCmd SetRewardSplit with
    governanceParty = gp; proposer = parties.member1; priorConfig = None; beneficiaries = []
  confs <- submitConfirmations gp rulesCid (toInterfaceContractId prop) [parties.member1, parties.member2]
  submitMustFail (actAs parties.member1 <> readAs gp) $
    exerciseCmd rulesCid GovernanceRules_ExecuteConfirmedAction with
      executor = parties.member1
      actionProposalCid = toInterfaceContractId prop
      confirmations = confs
```

- [ ] **Step 7: Full DAML gate + commit.** Run: `(cd daml/governance-rewards-assign-test && dpm test) && (cd daml && dpm build --all)`. Expected: all pass. Then:
```bash
git add daml/governance-rewards/daml/Governance/Rewards/RewardSplitConfig.daml \
        daml/governance-rewards/daml/Governance/Rewards/SetRewardSplit.daml \
        daml/governance-rewards-assign-test/daml/Governance/Rewards/TestSetRewardSplit.daml \
        daml/governance-rewards/daml.yaml
git commit -m "feat(governance-rewards): RewardSplitConfig + SetRewardSplit action"
```

---

### Task 2: Rust — `ProposalType::SetRewardSplit` + validate + serializer

**Files:**
- Modify: `crates/decman/src/server/types.rs` (+ `#[cfg(test)] mod tests`)
- Modify: `crates/decman/src/server/action_serializer.rs` (+ tests)

**Interfaces:**
- Consumes: `RewardBeneficiary` + the `ProposalType` enum + the `_ => Ok(())` default arm (M2); serializer helpers `make_party`, `make_numeric`, `make_list`, `make_record`, `field`; `ProposalPackage::GovernanceRewards`.
- Produces: `ProposalType::SetRewardSplit { new_beneficiaries: Vec<RewardBeneficiary>, prior_config: Option<String> }` (`prior_config` = cid of the config to replace); a shared `fn validate_reward_beneficiaries(bs: &[RewardBeneficiary]) -> Result<(), String>` (**exact `Decimal`**, no f64); a serializer arm emitting `("Governance.Rewards.SetRewardSplit", "SetRewardSplit", Record{ governanceParty, proposer, priorConfig, beneficiaries })`.

- [ ] **Step 1: Write the failing validation tests** (in `types.rs` tests):

```rust
#[test]
fn set_reward_split_validate() {
    let ok = ProposalType::SetRewardSplit {
        new_beneficiaries: vec![rb("a::1220aa", "0.8"), rb("b::1220bb", "0.2")], prior_config: None,
    };
    assert!(ok.validate().is_ok());
    let empty = ProposalType::SetRewardSplit { new_beneficiaries: vec![], prior_config: None };
    assert!(empty.validate().is_err());
    let bad_sum = ProposalType::SetRewardSplit { new_beneficiaries: vec![rb("a::1220aa", "0.5")], prior_config: None };
    assert!(bad_sum.validate().is_err());
}
```

- [ ] **Step 2: Run; verify fail (variant missing).** Run: `cargo test -p decman set_reward_split_validate`. Expected: FAIL.

- [ ] **Step 3: Extract the shared helper** and refactor `AssignRewardBeneficiaries`'s arm to call it (DRY — the checks are identical). **Use exact `Decimal`, not f64** — this refines M2's f64/`1e-9` approach so the boundary agrees with the DAML `total == 1.0` guard (Task 1) and the confirmer's `split_matches` (Task 5); the M2 assign tests (`0.8 + 0.2`) still pass because they sum exactly. In `types.rs`:

```rust
use std::str::FromStr;
use rust_decimal::Decimal; // the exact type DamlDecimal wraps; confirm the crate/path at first build

/// Non-empty, each percentage in (0.0, 1.0], sum == 1.0 exactly, <= 20 entries.
fn validate_reward_beneficiaries(bs: &[RewardBeneficiary]) -> Result<(), String> {
    if bs.is_empty() { return Err("new_beneficiaries must not be empty".into()); }
    if bs.len() > 20 { return Err("at most 20 beneficiaries".into()); }
    let (zero, one) = (Decimal::ZERO, Decimal::ONE);
    let mut sum = Decimal::ZERO;
    for b in bs {
        let p = Decimal::from_str(&b.percentage.to_string())
            .map_err(|_| "percentage is not a number".to_string())?;
        if p <= zero || p > one { return Err("each percentage must be in (0.0, 1.0]".into()); }
        sum += p;
    }
    if sum != one { return Err("percentages must sum to exactly 1.0".into()); }
    Ok(())
}
```
(If `DamlDecimal` already exposes `Decimal`/`Add`/`Ord`, use it directly instead of re-parsing from string.) Then the `AssignRewardBeneficiaries` arm becomes `=> validate_reward_beneficiaries(new_beneficiaries)`, and add:
```rust
ProposalType::SetRewardSplit { new_beneficiaries, .. } => validate_reward_beneficiaries(new_beneficiaries),
```
And the variant, next to `AssignRewardBeneficiaries`:
```rust
/// Set the decparty's on-ledger reward split (RewardSplitConfig).
/// `prior_config` is the cid of the config being replaced (None for the first set).
SetRewardSplit { new_beneficiaries: Vec<RewardBeneficiary>, prior_config: Option<String> },
```

- [ ] **Step 4: Run validation tests; verify pass** (and the M1 assign validate tests still pass). Run: `cargo test -p decman reward`. Expected: PASS.

- [ ] **Step 5: Write the failing serializer round-trip test** (`action_serializer.rs` tests), mirroring `build_proposal_assign_reward_beneficiaries_shape` (M1):

```rust
#[test]
fn build_proposal_set_reward_split_shape() -> Result {
    let proposal = ProposalType::SetRewardSplit {
        new_beneficiaries: vec![
            RewardBeneficiary { beneficiary: party_id(), percentage: dec("0.8") },
            RewardBeneficiary { beneficiary: party_id(), percentage: dec("0.2") },
        ],
        prior_config: Some("00old".to_string()),
    };
    let (package, module, entity, record) =
        build_proposal_create_args("gov", "proposer", &proposal, None, None)?;
    assert_eq!(package, ProposalPackage::GovernanceRewards);
    assert_eq!(module, "Governance.Rewards.SetRewardSplit");
    assert_eq!(entity, "SetRewardSplit");
    assert_eq!(owned_labels(&record), ["governanceParty", "proposer", "priorConfig", "beneficiaries"]);
    Ok(())
}
```

- [ ] **Step 6: Add the serializer arm** (reuse M1's `serialize_reward_beneficiary`), next to the `AssignRewardBeneficiaries` arm:

```rust
ProposalType::SetRewardSplit { new_beneficiaries, prior_config } => (
    ProposalPackage::GovernanceRewards,
    "Governance.Rewards.SetRewardSplit",
    "SetRewardSplit",
    Record {
        record_id: None,
        fields: vec![
            field("governanceParty", make_party(governance_party)),
            field("proposer", make_party(proposer)),
            field("priorConfig",
                make_optional(prior_config.as_ref().map(|c| make_contract_id(c)))),
            field("beneficiaries",
                make_list(new_beneficiaries.iter().map(serialize_reward_beneficiary).collect())),
        ],
    },
),
```
(Use the existing optional-encoding helper — grep `make_optional` in `action_serializer.rs`; the `SetProviderAppRewardBeneficiaries` arm already encodes an `Optional`, so mirror it. If the helper takes a value + tag rather than an `Option<Value>`, match its signature.)

- [ ] **Step 7: Refresh TS bindings.** Adding the `SetRewardSplit` variant changes the generated TS union, so run `cargo run --features typegen --bin gen-types` (the bindings are gitignored — this just keeps a local frontend build compiling). **No UI form is needed** (`set_reward_split` is automation-driven); the `default:` case in `GovernanceSection.tsx`'s propose switch already covers it — do not add a case.

- [ ] **Step 8: Backend gate + commit.** Run: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p decman` (and, if iterating on the frontend, `cd crates/decman/frontend && npx tsc -b`). Then:
```bash
git add crates/decman/src/server/types.rs crates/decman/src/server/action_serializer.rs
git commit -m "feat(decman): ProposalType::SetRewardSplit + shared beneficiary validation"
```

---

### Task 3: Rust — extract `pub(crate) submit_proposal` from the handler

**Files:** Modify `crates/decman/src/server/handlers/governance.rs`.

**Interfaces:**
- Produces: `pub(crate) async fn submit_proposal(config: &NodeConfig, party_id: &CantonId, rules_contract_id: &str, proposal: &ProposalType, token: &str, member_party_id: &CantonId, packages: &PackageConfig) -> anyhow::Result<String>` returning the created proposal's contract id. Task 7 (proposer) consumes this.
- Also widen to `pub(crate)`: `execute_confirm_action`, `get_party_credentials`, and **`packages()`** (`governance.rs:2095`). And **extract `pub(crate) async fn resolve_active_governance_rules(config, party, token, test_mode, packages) -> anyhow::Result<(String, usize)>`** from the block at `governance.rs:115–138` (active rules cid + governance threshold), which both the existing handler and Task 9 call. **Do not** widen/use `get_party_threshold` — that returns the topology threshold, which is the wrong number for `get_governance_confirmations`/the execute gate.

- [ ] **Step 1: Identify the exact inline block.** In `propose_action` (`governance.rs`~1056), the body from `build_proposal_create_args(...)` through the `client.submit_and_wait_for_transaction(...)` call and extraction of the created contract id (`governance.rs`~1291–1470). This is behavior-preserving refactor — no logic change.

- [ ] **Step 2: Create the function.** Move that block into `submit_proposal` with the signature above. It: calls `build_proposal_create_args(party_id, proposer=member_party_id, proposal, ...)`, resolves the package-id via the existing `resolve_contract_package_ref(...)` used in the handler, builds the `Commands`/`SubmitAndWaitForTransactionRequest` exactly as today, injects `Bearer {token}`, submits over a fresh `CommandServiceClient` channel (the idiom at `governance.rs:1321`), and returns the created contract id. Return errors as `anyhow::Result` instead of HTTP responses.

- [ ] **Step 3: Rewrite the handler to call it.** `propose_action` keeps `require_admin`, resolves `(token, member_party_id)` via `get_party_credentials`, then `let cid = submit_proposal(&data.config, &body.party_id, &body.rules_contract_id, &body.proposal, &token, &member_party_id, &packages).await` and maps `Ok/Err` to the same HTTP responses it returns today.

- [ ] **Step 4: Widen visibility + extract the rules resolver.** Change `execute_confirm_action`, `get_party_credentials`, and `packages()` from private to `pub(crate)`. Extract `resolve_active_governance_rules` from the `governance.rs:115–138` block to a `pub(crate)` helper returning `(rules_contract_id, threshold)`, and have the existing handler call it (behavior-preserving). (`get_party_credentials` keeps `&web::Data<AppState>` — Task 9 passes a clone. `get_party_threshold` is intentionally left unused by the automation.)

- [ ] **Step 5: Verify the refactor is behavior-preserving.** Run: `cargo test -p decman && cargo clippy --all-targets --all-features -- -D warnings`. Expected: all existing tests + serializer round-trips still pass (they exercise `build_proposal_create_args`, which `submit_proposal` now wraps). The gRPC submit itself is covered by the devnet IT (Task 10).

- [ ] **Step 6: Commit.**
```bash
git add crates/decman/src/server/handlers/governance.rs
git commit -m "refactor(decman): extract pub(crate) submit_proposal; widen creds/confirm helpers"
```

---

### Task 4: Rust — `SplitSource` + unassigned-coupon reader

**Files:** Create `crates/decman/src/server/reward_automation/mod.rs`; add `mod reward_automation;` in `crates/decman/src/server/mod.rs`.

**Interfaces:**
- Produces:
  - `pub(crate) async fn active_created_records(config, party, token, test_mode, package_id: &str, module: &str, entity: &str, interface_view: bool) -> anyhow::Result<Vec<(String, Record)>>` — the **one shared decoded read**: a direct `StateServiceClient` `GetActiveContracts` (modeled on `fetch_proposal_infos`, `queries.rs:1392`) returning `(contract_id, record)` where `record` is `created_event.create_arguments` for a template (`interface_view=false`) or the decoded interface view (`interface_view=true`). Used by this task and Task 6.
  - `pub(crate) struct CouponInfo { pub cid: String, pub provider: CantonId, pub amount: DamlDecimal, pub expires_at: DateTime<Utc> }`
  - `pub(crate) async fn effective_split(config: &NodeConfig, packages: &PackageConfig, test_mode: bool, decparty: &CantonId, token: &str) -> anyhow::Result<Option<Vec<RewardBeneficiary>>>` — reads the singleton `RewardSplitConfig` (defensive 0/1/>1). **Plain async fn, not a trait** — the codebase avoids `async-trait` and there is one source today; this is still the single swap point for a future shared template.
  - `pub(crate) async fn unassigned_coupons(config, decparty, token, test_mode, packages) -> anyhow::Result<Vec<CouponInfo>>`.
- Consumes: the `fetch_proposal_infos` direct-`GetActiveContracts` pattern (`queries.rs:1392`) + `StateServiceClient`; `PackageConfig.governance_rewards` (`config.rs:401`, `Option<String>`, default `#governance-rewards-v1`).

- [ ] **Step 1: Write the failing split-parse test.** Split reading is I/O, but the record→`Vec<RewardBeneficiary>` decode is pure. Extract `fn parse_split_record(rec: &Record) -> anyhow::Result<Vec<RewardBeneficiary>>` and test it against a hand-built `Record` mirroring `RewardSplitConfig` (fields `governanceParty`, `beneficiaries` = list of `{beneficiary, percentage}`):

```rust
#[test]
fn parse_split_record_reads_beneficiaries() {
    let rec = make_record(vec![
        field("governanceParty", make_party("gov::1220")),
        field("beneficiaries", make_list(vec![
            make_record(vec![field("beneficiary", make_party("a::1220")), field("percentage", make_numeric("0.8"))]),
            make_record(vec![field("beneficiary", make_party("b::1220")), field("percentage", make_numeric("0.2"))]),
        ])),
    ]);
    let split = parse_split_record(&rec).unwrap();
    assert_eq!(split.len(), 2);
    assert_eq!(split[0].percentage.to_string(), "0.8");
}
```

- [ ] **Step 2: Run; verify fail.** Run: `cargo test -p decman parse_split_record`. Expected: FAIL (fn missing).

- [ ] **Step 3: Implement `active_created_records`, `parse_split_record`, `OnLedgerSplitSource`.** First write `active_created_records` (the shared decoded read above). Then `effective_split` calls it with the `RewardSplitConfig` **template** filter (`package_id = packages.governance_rewards` — an `Option<String>`, so `else return Ok(None)` if unset; `module = "Governance.Rewards.RewardSplitConfig"`, `entity = "RewardSplitConfig"`, `interface_view = false`), keeps records whose `governanceParty == decparty`, and — since the config is a keyless singleton (Task 1) — **defends the single-config invariant**: `0` → `Ok(None)` (off for this decparty); exactly `1` → `Ok(Some(parse_split_record(&rec)?))`; `>1` → `Err("ambiguous RewardSplitConfig: N active — refusing")` + `warn!` (a duplicate slipped past replace-by-cid; the confirmer then refuses everything until cleanup — fail-safe, never mis-assigns). Decoding uses `active_created_records` (above) — no raw-blob parsing.

- [ ] **Step 4: Run split test; verify pass.** Run: `cargo test -p decman parse_split_record`. Expected: PASS.

- [ ] **Step 5: Implement `unassigned_coupons`.** Call `active_created_records(config, decparty, token, test_mode, package_id, "Splice.Api.RewardAssignmentV1", "RewardCoupon", /*interface_view=*/ true)` — returns the decoded `RewardCouponView` for every contract implementing the interface (on devnet, `RewardCouponV2`). **Verified view fields** (splice `RewardAssignmentV1.daml:11`): `dso : Party`, `provider : Party`, `beneficiary : Optional Party`, `amount : Decimal`, `expiresAt : Time`, `maxNumNewBeneficiaries : Int`. Keep records where `provider == decparty` **and** `beneficiary` is `None` (unassigned); map to `CouponInfo { cid, provider, amount, expires_at }`. **First-build:** the interface `package_id` — use the `#splice-api-reward-assignment-v1` name-alias (Canton resolves by name, like the `#splice-amulet` filters at `governance.rs:612`) or the vendored id `6f7b7236…`; confirm the alias resolves against the vetted DAR.

- [ ] **Step 6: Commit.**
```bash
git add crates/decman/src/server/reward_automation/mod.rs crates/decman/src/server/mod.rs
git commit -m "feat(decman): reward-automation SplitSource + unassigned-coupon reader"
```

---

### Task 5: Rust — the default-deny auto-confirmation engine

**Files:** Modify `crates/decman/src/server/reward_automation/mod.rs` (+ tests).

**Interfaces:**
- Produces:
  - `pub(crate) struct PendingAssign { pub proposal_cid: String, pub primary_coupon: String, pub additional_coupons: Vec<String>, pub new_beneficiaries: Vec<RewardBeneficiary> }` (populated by Task 6).
  - `pub(crate) fn split_matches(proposed: &[RewardBeneficiary], configured: &[RewardBeneficiary]) -> bool` — set-equal with **exact `Decimal`** equality on percentage (no float tolerance; matches DAML).
  - `pub(crate) fn is_confirmable(action_label: &str, proposal: &PendingAssign, configured: &[RewardBeneficiary], coupons_unassigned: bool) -> bool` — the allowlist-of-one policy: returns true iff `action_label == "AssignRewardBeneficiaries"` AND `split_matches(&proposal.new_beneficiaries, configured)` AND `coupons_unassigned`.

- [ ] **Step 1: Write the failing policy tests:**

```rust
fn rb(p: &str, pct: &str) -> RewardBeneficiary { /* reuse test helper */ }

#[test]
fn split_matches_is_order_insensitive_and_exact() {
    let cfg = vec![rb("a::1220", "0.8"), rb("b::1220", "0.2")];
    assert!(split_matches(&[rb("b::1220", "0.2"), rb("a::1220", "0.8")], &cfg));  // reordered
    assert!(!split_matches(&[rb("a::1220", "0.7"), rb("b::1220", "0.3")], &cfg)); // wrong pct
    assert!(!split_matches(&[rb("a::1220", "0.8000000001"), rb("b::1220", "0.1999999999")], &cfg)); // off by 1e-10 -> reject (exact)
    assert!(!split_matches(&[rb("a::1220", "1.0")], &cfg));                        // wrong set
    assert!(!split_matches(&[rb("a::1220","0.8"), rb("c::1220","0.2")], &cfg));    // wrong party
}

#[test]
fn is_confirmable_is_default_deny() {
    let cfg = vec![rb("a::1220", "1.0")];
    let good = PendingAssign { proposal_cid: "p".into(), primary_coupon: "c1".into(),
        additional_coupons: vec![], new_beneficiaries: vec![rb("a::1220", "1.0")] };
    assert!(is_confirmable("AssignRewardBeneficiaries", &good, &cfg, true));
    assert!(!is_confirmable("AssignRewardBeneficiaries", &good, &cfg, false)); // coupon now assigned
    assert!(!is_confirmable("SetRewardSplit", &good, &cfg, true));             // not enrolled
    let bad = PendingAssign { new_beneficiaries: vec![rb("z::1220", "1.0")], ..good.clone() };
    assert!(!is_confirmable("AssignRewardBeneficiaries", &bad, &cfg, true));   // split mismatch
}
```

- [ ] **Step 2: Run; verify fail.** Run: `cargo test -p decman -- split_matches is_confirmable`. Expected: FAIL.

- [ ] **Step 3: Implement `split_matches` + `is_confirmable`.** `split_matches`: equal length, and every configured `(beneficiary, percentage)` has a matching proposed entry — compare `beneficiary` by `CantonId` equality and `percentage` by **exact `Decimal`** equality (parse both via `Decimal::from_str`; no f64, matching the DAML `== 1.0` guard from Task 1). `is_confirmable`: the three-part conjunction above; the action-label allowlist is a literal `"AssignRewardBeneficiaries"` (default-deny for anything else).

- [ ] **Step 4: Run; verify pass.** Run: `cargo test -p decman -- split_matches is_confirmable`. Expected: PASS.

- [ ] **Step 5: Commit.**
```bash
git add crates/decman/src/server/reward_automation/mod.rs
git commit -m "feat(decman): default-deny auto-confirmation policy (split match)"
```

---

### Task 6: Rust — read a pending `AssignRewardBeneficiaries` proposal (parse-back)

**Files:** Modify `crates/decman/src/server/reward_automation/mod.rs` (+ tests).

**Interfaces:**
- Produces: `pub(crate) async fn read_pending_assign(config, decparty, proposal_cid, token, test_mode, packages) -> anyhow::Result<Option<PendingAssign>>`, and a pure `fn parse_assign_record(cid: &str, rec: &Record) -> anyhow::Result<PendingAssign>`.
- Rationale: `DomainGovernanceAction` (`types.rs:761`) exposes only `action_label`/`description` — no coupon cids or beneficiaries — so the confirmer must read the concrete `AssignRewardBeneficiaries` contract to validate it.

- [ ] **Step 1: Write the failing parse test** against a hand-built `Record` mirroring the M1 template (`governanceParty, proposer, primaryCoupon, additionalCoupons, newBeneficiaries`):

```rust
#[test]
fn parse_assign_record_reads_coupons_and_split() {
    let rec = make_record(vec![
        field("governanceParty", make_party("gov::1220")),
        field("proposer", make_party("m1::1220")),
        field("primaryCoupon", make_contract_id("c1")),
        field("additionalCoupons", make_list(vec![make_contract_id("c2")])),
        field("newBeneficiaries", make_list(vec![
            make_record(vec![field("beneficiary", make_party("a::1220")), field("percentage", make_numeric("0.8"))]),
            make_record(vec![field("beneficiary", make_party("b::1220")), field("percentage", make_numeric("0.2"))]),
        ])),
    ]);
    let pa = parse_assign_record("p1", &rec).unwrap();
    assert_eq!(pa.primary_coupon, "c1");
    assert_eq!(pa.additional_coupons, vec!["c2".to_string()]);
    assert_eq!(pa.new_beneficiaries.len(), 2);
}
```

- [ ] **Step 2: Run; verify fail.** Run: `cargo test -p decman parse_assign_record`. Expected: FAIL.

- [ ] **Step 3: Implement `parse_assign_record` + `read_pending_assign`.** `read_pending_assign` calls `active_created_records(config, decparty, token, test_mode, packages.governance_rewards, "Governance.Rewards.AssignRewardBeneficiaries", "AssignRewardBeneficiaries", /*interface_view=*/ false)` (concrete template → `create_arguments` Record), finds the record whose cid == `proposal_cid`, and decodes it with `parse_assign_record`. Return `None` if not found (proposal already executed/expired).

- [ ] **Step 4: Run; verify pass.** Run: `cargo test -p decman parse_assign_record`. Expected: PASS.

- [ ] **Step 5: Commit.**
```bash
git add crates/decman/src/server/reward_automation/mod.rs
git commit -m "feat(decman): parse-back of pending AssignRewardBeneficiaries proposals"
```

---

### Task 7: Rust — the proposer role

**Files:** Modify `crates/decman/src/server/reward_automation/mod.rs` (+ tests).

**Interfaces:**
- Produces:
  - `pub(crate) fn select_batch(coupons: &[CouponInfo], now: DateTime<Utc>, watermark: Duration, minting_margin: Duration, max_batch: usize) -> Vec<String>` — pure; returns cids to assign this tick.
  - `pub(crate) async fn run_proposer_once(config, decparty, member_party_id, token, split, test_mode, packages, covered_coupons: &HashSet<String>) -> anyhow::Result<()>` — `covered_coupons` = coupon cids already targeted by in-flight proposals (Task 9 builds it), excluded from this tick's batch (best-effort dedupe, spec §10; coupon-level so partial overlaps are handled).

- [ ] **Step 1: Write the failing `select_batch` tests** (spec §9 proposer step 2, §11 batch/margin):

```rust
#[test]
fn select_batch_respects_watermark_margin_and_cap() {
    let now = dt("2026-07-20T12:00:00Z");
    let c = |id: &str, exp: &str| CouponInfo { cid: id.into(), provider: cid_party(), amount: dec("1"), expires_at: dt(exp) };
    let coupons = vec![
        c("young", "2026-07-21T23:00:00Z"), // ~35h out: too fresh if watermark not met -> excluded
        c("ripe",  "2026-07-20T20:00:00Z"), // 8h out: past watermark, margin ok -> included
        c("urgent","2026-07-20T12:30:00Z"), // 30m out: inside minting margin -> excluded (no time to mint)
    ];
    let got = select_batch(&coupons, now, Duration::hours(6), Duration::hours(2), 100);
    assert_eq!(got, vec!["ripe".to_string()]);
}

#[test]
fn select_batch_caps_size() {
    let now = dt("2026-07-20T12:00:00Z");
    let coupons: Vec<_> = (0..10).map(|i| CouponInfo {
        cid: format!("c{i}"), provider: cid_party(), amount: dec("1"),
        expires_at: dt("2026-07-20T20:00:00Z") }).collect();
    assert_eq!(select_batch(&coupons, now, Duration::hours(6), Duration::hours(2), 3).len(), 3);
}
```

- [ ] **Step 2: Run; verify fail.** Run: `cargo test -p decman select_batch`. Expected: FAIL.

- [ ] **Step 3: Implement `select_batch`.** Keep coupons where `now - (expires_at - COUPON_TTL) >= watermark` **and** `expires_at - now >= minting_margin`; sort by `expires_at` ascending (most-urgent-but-still-mintable first); truncate to `max_batch`; return cids. (Use a `COUPON_TTL` const of 36h to derive age from expiry, matching spec §1; if the interface view exposes a `createdAt`, prefer that.)

- [ ] **Step 4: Implement `run_proposer_once`.** Read `unassigned_coupons` (Task 4); drop any whose cid is in `covered_coupons` (dedupe); `select_batch` over the remainder; if a non-empty batch remains, build `ProposalType::AssignRewardBeneficiaries { primary_coupon = batch[0], additional_coupons = batch[1..], new_beneficiaries = split.clone() }` and call `submit_proposal` (Task 3). Log the proposed batch. (If the batch is empty — nothing ripe or all covered — no-op.)

- [ ] **Step 5: Run batch tests; verify pass.** Run: `cargo test -p decman -- select_batch batch`. Expected: PASS.

- [ ] **Step 6: Commit.**
```bash
git add crates/decman/src/server/reward_automation/mod.rs
git commit -m "feat(decman): proposer role (TTL-watermark batch + dedupe + propose)"
```

---

### Task 8: Rust — the confirmer role

**Files:** Modify `crates/decman/src/server/reward_automation/mod.rs` (+ tests).

**Interfaces:**
- Produces: `pub(crate) async fn run_confirmer_once(data: &web::Data<AppState>, config, decparty, member_party_id, token, rules_contract_id: &str, split, domain: &[DomainGovernanceAction], test_mode, packages) -> anyhow::Result<()>`, plus a pure `fn already_confirmed_by(action: &DomainGovernanceAction, member: &CantonId) -> bool`. (`domain` is fetched once per tick by Task 9 and passed in — no re-fetch here; `can_execute` is already on each `DomainGovernanceAction`, so no `threshold` arg is needed.)
- Consumes: `read_pending_assign` (Task 6), `is_confirmable` (Task 5), `unassigned_coupons` (Task 4, to recheck coupons), `execute_confirm_action` (Task 3, now `pub(crate)`).

- [ ] **Step 1: Write the failing `already_confirmed_by` test** (avoid double-confirming):

```rust
#[test]
fn already_confirmed_by_detects_this_member() {
    let action = DomainGovernanceAction {
        proposal_cid: "p".into(), action_label: "AssignRewardBeneficiaries".into(),
        description: None, confirmations: vec![gov_conf("m1::1220")], confirmation_count: 1,
        can_execute: false, orphaned: false, transfer_details: None,
        accept_transfer_details: None, service_request_details: None,
    };
    assert!(already_confirmed_by(&action, &canton_id("m1::1220")));
    assert!(!already_confirmed_by(&action, &canton_id("m2::1220")));
}
```
(The confirmer-party field is `GovernanceConfirmation.confirming_party: CantonId` — `types.rs:972`; `gov_conf(p)` builds a `GovernanceConfirmation` with that set.)

- [ ] **Step 2: Run; verify fail.** Run: `cargo test -p decman already_confirmed_by`. Expected: FAIL.

- [ ] **Step 3: Implement `already_confirmed_by` + `run_confirmer_once`.** Flow:
  1. Iterate the `domain` slice passed in (fetched once by Task 9 — do not re-fetch).
  1b. Fetch the unassigned set **once** before the loop: `let live: HashSet<String> = unassigned_coupons(config, decparty, token, test_mode, packages).await?.into_iter().map(|c| c.cid).collect();` (do not re-query per proposal).
  2. For each `a in domain` with `a.action_label == "AssignRewardBeneficiaries"` and `!already_confirmed_by(a, member_party_id)` and `!a.orphaned`:
     - `let Some(pa) = read_pending_assign(config, decparty, &a.proposal_cid, token, test_mode, packages).await? else continue;`
     - `coupons_ok = std::iter::once(&pa.primary_coupon).chain(&pa.additional_coupons).all(|c| live.contains(c));` (using the hoisted `live`)
     - `if is_confirmable(&a.action_label, &pa, split, coupons_ok) { execute_confirm_action(config, &confirm_req, token, member_party_id, packages).await?; }` else `warn!`-refuse. Build `confirm_req: ConfirmActionRequest` exactly like the proposer-auto-confirm block does (see verification below), with `governance_type: GovernanceType::CoreDomain` and `proposal_cid: Some(a.proposal_cid.clone())`.
  3. (Optional, first-wins) if `a.can_execute`, call the executor; a lost race fails harmlessly (spec §10). Keep execution behind the same enrolled-label guard.
  - **First-build verification (concrete example exists):** `propose_action` already **auto-confirms as the proposer** immediately after creating a proposal — the block at ~`governance.rs:1420–1470` builds a `ConfirmActionRequest { governance_type: GovernanceType::CoreDomain, proposal_cid: Some(...), rules_contract_id, action, .. }` and calls `execute_confirm_action`. Copy that exact construction (including whatever `action: ActionType` value it uses for `CoreDomain` — `execute_confirm_action`'s CoreDomain branch builds the choice arg from `proposal_cid`, `governance.rs:2094`, so `action` is effectively inert there). The `rules_contract_id` is resolved once per tick by `resolve_active_governance_rules` (Task 3/9, extracted from `governance.rs:115–138`) and threaded in.

- [ ] **Step 4: Run; verify pass.** Run: `cargo test -p decman already_confirmed_by`. Expected: PASS.

- [ ] **Step 5: Commit.**
```bash
git add crates/decman/src/server/reward_automation/mod.rs
git commit -m "feat(decman): confirmer role (validate vs split, auto-confirm)"
```

---

### Task 9: Rust — background loop + registration

**Files:** Modify `crates/decman/src/server/reward_automation/mod.rs`, `crates/decman/src/server/mod.rs`, `crates/decman/src/config.rs`.

**Interfaces:**
- Produces: `pub(crate) async fn run_reward_automation_loop(data: web::Data<AppState>)` — the interval loop; and a `NodeConfig` field `reward_automation_interval_secs: u64` (default `300`, `#[serde(default = ...)]`).

- [ ] **Step 1: Add the config field.** In `config.rs`, add `reward_automation_interval_secs` to `NodeConfig` with a serde default of `300`. (No DB migration — enablement is on-ledger via `RewardSplitConfig`.)

- [ ] **Step 2: Write the loop**, modeled on `run_peer_ping_loop` (`mod.rs:1892`):

```rust
pub(crate) async fn run_reward_automation_loop(data: web::Data<AppState>) {
    let mut interval = tokio::time::interval(
        Duration::from_secs(data.config.reward_automation_interval_secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let parties: Vec<CantonId> =
            data.party_credentials.read().await.iter().map(|p| p.dec_party_id.clone()).collect();
        for decparty in parties {
            if let Err(e) = run_once_for_party(&data, &decparty).await {
                tracing::warn!(%decparty, error=%e, "reward automation tick failed");
            }
        }
    }
}

async fn run_once_for_party(data: &web::Data<AppState>, decparty: &CantonId) -> anyhow::Result<()> {
    let pkgs = packages();
    let Some((token, member)) = get_party_credentials(data, decparty).await else { return Ok(()); };
    // enablement: "on" iff the decparty has exactly one RewardSplitConfig (Task 4).
    // (effective_split is a plain async fn — the single-impl SplitSource trait was
    // dropped to match the codebase's no-async-trait convention.)
    let Some(split) = effective_split(&data.config, &pkgs, data.test_mode, decparty, &token).await? else { return Ok(()); }; // None => off; Err => ambiguous, propagates as a warn
    // governance rules cid + governance threshold (NOT topology) from the active GovernanceRules
    let (rules_cid, threshold) = resolve_active_governance_rules(&data.config, decparty, &token, data.test_mode, &pkgs).await?;
    // fetch pending governance actions ONCE this tick (shared by dedupe + confirmer)
    let (_, domain) = get_governance_confirmations(
        &data.config, decparty, threshold, Some(token.clone()), data.test_mode, &pkgs).await?;
    // coupons already targeted by in-flight assign proposals -> dedupe input for the proposer
    let mut covered: HashSet<String> = HashSet::new();
    for a in domain.iter().filter(|a| a.action_label == "AssignRewardBeneficiaries" && !a.orphaned) {
        if let Some(pa) = read_pending_assign(&data.config, decparty, &a.proposal_cid, &token, data.test_mode, &pkgs).await? {
            covered.insert(pa.primary_coupon);
            covered.extend(pa.additional_coupons);
        }
    }
    // proposer then confirmer (order within a tick is immaterial; see spec §10)
    run_proposer_once(&data.config, decparty, &member, &token, &split, data.test_mode, &pkgs, &covered).await?;
    run_confirmer_once(data, &data.config, decparty, &member, &token, &rules_cid, &split, &domain, data.test_mode, &pkgs).await?;
    Ok(())
}
```
(`packages()` and `resolve_active_governance_rules` are the `pub(crate)` items from Task 3. `threshold` here is the **governance-rules** threshold, which is exactly what `get_governance_confirmations` needs to compute `can_execute` — do not substitute `get_party_threshold` (topology).)

- [ ] **Step 3: Register in `start_server`.** Next to the heartbeat spawn (`mod.rs:948`), **clone the existing `web::Data<AppState>`** that `start_server` already built for actix (a `web::Data` is an `Arc` — cloning it shares the *same* `AppState`, incl. the live `party_credentials`/`auth`):
```rust
let ra_data = app_data.clone(); // app_data: web::Data<AppState> already in start_server — Arc clone, SHARED state
tokio::spawn(async move { reward_automation::run_reward_automation_loop(ra_data).await; });
```
**Do not** write `web::Data::new(app_state.clone())` — that allocates a *separate* `AppState`, so the loop would never see the real party credentials. Match the variable `start_server` actually uses for its `web::Data<AppState>`.

- [ ] **Step 4: Compile + gate.** Run: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p decman`. Expected: clean. (Loop behavior itself is exercised in Task 10.)

- [ ] **Step 5: Commit.**
```bash
git add crates/decman/src/server/reward_automation/mod.rs crates/decman/src/server/mod.rs crates/decman/src/config.rs
git commit -m "feat(decman): register per-node reward-automation loop"
```

---

### Task 10 (M4): Devnet integration test

**Files:** extend the existing devnet IT phase (`test(integration): extend IT suite to devnet`); add a reward-automation scenario. No new runtime code.

**Preconditions (operational — see spec §13 and the devnet finding below):**
- **`cbtc-network` is the only decparty we govern, and its coupons are currently swept to 0 by Robert's MintingDelegation collection path.** Before this test can observe unassigned coupons, **pause that collection** for `cbtc-network` (archive its `MintingDelegation` and/or `ValidatorRight` on `iBTC-validator-1`) — coordinate with Robert. Coupons re-accumulate unassigned within ~one round.
- Set the split on-ledger: propose→confirm→execute `SetRewardSplit` for `cbtc-network` with `[cbtc-beneficiary 0.8, operator 0.2]` (matching the configured intent). Verify one `RewardSplitConfig` exists.
- Run **≥2 DecMan member instances** for `cbtc-network` (multi-node is required to exercise threshold auto-confirm).

- [ ] **Step 1: Write the scenario.** With the split config in place and collection paused: start the automation on the member nodes; wait up to N ticks. Assert, in order (query devnet PQS `pqs_cbtc`): (a) an `AssignRewardBeneficiaries` proposal appears (`provider = cbtc-network`), (b) `GovernanceConfirmation`s from ≥ threshold distinct member nodes appear for it, (c) it executes, (d) the original unassigned coupons are archived and one `RewardCouponV2` per beneficiary now exists with `beneficiary ∈ {cbtc-beneficiary, operator}` and the expected `amount` shares.

- [ ] **Step 2: Assert the security property (negative).** The split is a single on-ledger source, so simulate a buggy/malicious proposer by **manually submitting an `AssignRewardBeneficiaries` proposal whose `newBeneficiaries` do NOT match the on-ledger `RewardSplitConfig`** (via the `/governance/propose` API on one node). Assert the honest nodes' confirmers **refuse** it — it never reaches threshold and expires unexecuted. This exercises the default-deny confirmer (`is_confirmable` → false on split mismatch) end-to-end.

- [ ] **Step 3: Record results in the PR.** Note whether beneficiary self-minting was observed (a separate precondition — only if those agents run; see spec §4.3) or whether the test asserts assignment only.

- [ ] **Step 4: Restore devnet.** Un-pause Robert's collection for `cbtc-network` (or leave per team decision). Commit the IT.
```bash
git add <it-files>
git commit -m "test(integration): Mode A propose -> auto-confirm -> execute on devnet"
```

---

## What this plan intentionally does NOT cover

- Mode B / the MintingDelegation collection path (Robert; now a one-shot, live on devnet via #256).
- A shared reward-config template — if Robert builds one, swap `OnLedgerSplitSource` (Task 4) for it; nothing else changes.
- Deterministic leader election / grace-window proposer optimisation (spec §10 — a follow-up; any-node-proposes is already safe).
- Beneficiary self-mint automation (spec §4.3 — the beneficiaries' own agents).
- Auto-confirmation for any action other than `AssignRewardBeneficiaries` (default-deny holds).
- Per-decparty on/off or per-decparty intervals (enablement is config-presence + one global interval; revisit only if a node ever runs many decparties with different needs).

## Self-review notes

- **Spec coverage:** §5 auto-confirmation → Tasks 5+8; §8 split source (Option B) → Tasks 1+4; §9 proposer/confirmer → Tasks 7+8+9; §10 idempotency (any-node propose, dedupe, first-wins execute) → Tasks 7+8; §11 edge cases (TTL/margin, split mismatch, stale proposals) → Tasks 7+8; §12 trust (validate vs L3, non-custodial, default-deny) → Tasks 4+5; §13 testing → Tasks 1–8 units + Task 10 IT.
- **Robert's answers baked in:** no `getBeneficiaries`/operator-cut math (split read verbatim from config, Task 4); no mode selector (enablement = config presence, Task 9); Option B split source is the single swap point (Task 4).
- **Type consistency:** `RewardBeneficiary { beneficiary, percentage }` (splice, `deriving (Show, Eq, Ord)` — verified) is identical across the DAML config (Task 1), `ProposalType::SetRewardSplit`/`AssignRewardBeneficiaries` (Tasks 2 + M2), the serializer (Task 2 + M1), `SplitSource`/`PendingAssign` (Tasks 4–6), and `split_matches` (Task 5). `SetRewardSplit`'s field order `governanceParty, proposer, priorConfig, beneficiaries` matches across the DAML template (Task 1), `ProposalType::SetRewardSplit { new_beneficiaries, prior_config }` (Task 2), the serializer arm, and the round-trip test's `owned_labels`. The `AssignRewardBeneficiaries` template field order drives both the M1 serializer and the Task 6 parse-back.
- **Cross-layer numeric consistency:** percentages use **exact `Decimal`** at every layer — DAML `total == 1.0` (Task 1), Rust `validate_reward_beneficiaries` (Task 2), and `split_matches` (Task 5) — so the boundary, the ledger guard, and the confirmer can never disagree (the earlier f64/`1e-9` approach could differ at the 10th decimal).
- **Contract-key convention:** `RewardSplitConfig` is **keyless** (verified no governance package uses contract keys); singleton is held by `SetRewardSplit`'s replace-by-cid plus the reader's defensive 0/1/>1 check (Task 4), which fails safe (refuse, never mis-assign) if a duplicate ever appears.
- **Decoded reads are solved, not assumed:** all three decoded reads (split config, coupon view, assign parse-back) go through one `active_created_records` helper built on the proven `fetch_proposal_infos` `GetActiveContracts` pattern — because `query_contracts_by_template` (blob-only) and `get_contracts`/`ContractInfo` (metadata-only) were verified NOT to return decoded fields.
- **Flagged first-build verifications (concrete "confirm X against the codebase" steps, not placeholders):** the exact `Decimal`/`DamlDecimal` arithmetic API (Task 2 Step 3); the optional-cid serializer helper (Task 2 Step 6); that `GetActiveContracts` exposes `create_arguments` (template) / interface view the way `fetch_proposal_infos` reads them (Task 4 Step 3); the `#splice-api-reward-assignment-v1` interface-filter alias (Task 4 Step 5); and the `CoreDomain` confirm construction — which already exists in the proposer-auto-confirm block at `governance.rs:1420–1470` (Task 8 Step 3). (The rules-cid + threshold resolution is a concrete extraction from `governance.rs:115–138`, not a guess — Task 3.)
- **Devnet reality:** `cbtc-network` is the only governed decparty and is currently swept to 0 active coupons by Robert's collection — Task 10 preconditions call out pausing it, so the IT isn't silently starved of coupons.
- **Iteration log:**
  - *Pass 1 (2026-07-20, vs. branch + splice):* fixed the contract-key assumption (F1), missing `Governance.Rules` import (F2), `packages()` visibility (F3), the `CoreDomain` example pointer + `confirming_party` (F4), exact-`Decimal` consistency (F6), fetch-once/coupon-level dedupe (F7/F8).
  - *Pass 2 (2026-07-20):* fixed the decoded-read mechanism — neither `query_contracts_by_template` nor `get_contracts` returns decoded fields; introduced the shared `active_created_records` helper on the `fetch_proposal_infos` pattern; verified `RewardCouponView` fields and `PackageConfig.governance_rewards` (F9); fixed `forA_`-over-`Optional` (dropped nonexistent `whenJust`).
  - *Pass 3 (2026-07-20):* **correctness fix (F12)** — `get_governance_confirmations`/the execute gate use the **governance-rules** threshold (`gov_state_threshold`, `governance.rs:115–138`), not the topology `get_party_threshold` the plan had used; replaced it with an extracted `resolve_active_governance_rules` (rules cid + governance threshold in one call) and dropped `get_party_threshold` from the automation. This also corrects an imprecision in spec §6.1.
  - *Pass 4 (2026-07-20):* confirmed remaining anchors — M1 `AssignRewardBeneficiaries` field order matches `parse_assign_record`; `fetch_proposal_infos` uses `GetActiveContractsRequest` + `InterfaceFilter{include_interface_view}` (so `active_created_records` is accurate); task dependency ordering is acyclic. Tightened the M4 negative test to a crafted-proposal (there is no per-node split) (F13).
  - *Pass 5 (2026-07-20, full end-to-end read):* caught seams left by the earlier surgical edits — the Step 6 negative test was missing the now-required `priorConfig` field (F15, would not compile); the loop registration snippet risked allocating a separate `AppState` via `web::Data::new(...)` instead of cloning the shared `web::Data` (F17, correctness); the confirmer re-queried `unassigned_coupons` per proposal instead of once per tick (F14, efficiency); and a File-Structure wording fix (F16). All fixed.
  - *Rebase (2026-07-20):* rebased the branch onto Robert's #256 branch (per Gyorgy), resolving 2 conflicts (`daml.yaml` → 0.1.2 + both DARs; `action_serializer.rs` → both test fns). Verified green: `cargo test -p decman`, frontend `tsc`, `dpm build --all` + all reward DAML scripts (mine + Robert's). Two rebase-surfaced fixes committed (`d0364fc`): the `0.1.2` bump propagated to both test packages + `releases/v1/`, and a `default:` case in the propose switch for automation-only variants (`types.generated.ts` is gitignored/generated, so this latent M1+M2 frontend gap only appeared once `gen-types` emitted the new union). PR #255 retargeted onto #256 (stacked).
  - *Implementation (2026-07-20):* Tasks 1–9 executed green (commits `986a3e5` → `6dff5de`); each verified with `cargo test -p decman` (293 lib + 38 integration), clippy `-D warnings`, fmt, frontend `tsc`, and DAML scripts. SplitSource simplified to a plain fn (no async-trait). Task 10 (devnet IT) remains ops-gated.
  - *Adversarial code review (2026-07-20):* reviewed the implemented M3 diff for correctness/assumptions/gaps. (Findings use the **R-N** scheme — renamed from M-N to avoid collision with the plan's M3/M4 milestones; commits `f8ef6c9` and `54d2887` predate the rename and still read "M-N".) **Found + fixed a CRITICAL liveness bug (C-1):** `run_proposer_once` discarded the created proposal cid and never self-confirmed, but `get_governance_confirmations` only surfaces proposals with ≥1 confirmation — so no confirmer ever saw a fresh proposal and nothing reached threshold. Fix: proposer self-confirms via a shared `submit_confirmation` helper (also fixes the compounding duplicate-proposal loop). Confirmer SAFETY (exact split match + coupons-unassigned + label allowlist) was verified sound. Also fixed R-1 (proposer error no longer aborts the confirmer), R-6 (confirmer verifies `governanceParty == decparty`), R-3 (DAML per-percentage (0,1] guard). **Follow-ups:** R-5 **DONE** (commit `54d2887` — `read_all_pending_assigns`: one ACS scan/tick indexed by cid, shared by proposer dedupe + confirmer). R-4 **investigated, intentionally not implemented** — the design-§11 "bounded confirmation expiresAt" is set inside the *shared* `GovernanceRules_ConfirmAction` (`now + actionConfirmationTimeout`, affects all actions); changing it is disproportionate for a cosmetic linger, and the functional intent (unmet proposals stop being executable) is already met by `actionConfirmationTimeout` — the C-1 self-confirm fix removed the accumulation concern, leaving at most an occasional harmless un-archived dead proposal. R-2 (extraction audit-log/status-code delta) — accepted, noted for PR review.
