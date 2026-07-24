# CIP-104 Coupon-Reassignment Automation (Mode A) — M1+M2 Implementation Plan

> **⚠️ Superseded for the delegation pivot (2026-07-21).** This plan describes the auto-confirmation-engine model that was built (green on `feat/governance/coupon-reassignment-automation`) and is now being reworked to the **delegation model** (spec rev. 2026-07-20). The delta is in `delegation-migration-plan.md` (this directory). Retained as the record of what was built — do **not** implement from this plan.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the on-ledger `AssignRewardBeneficiaries` governance action and its Rust proposal plumbing, so a decparty can — through the normal propose → confirm → execute flow — assign governance-chosen beneficiaries to its CIP-104 `RewardCouponV2` coupons.

**Architecture:** A new DAML `GovernableAction` template (`AssignRewardBeneficiaries`) whose `executeImpl` exercises splice's `RewardCoupon_AssignBeneficiaries`, plus a matching `ProposalType` variant + `validate()` arm + `action_serializer` mapping in the Rust backend. This mirrors, almost line-for-line, the just-merged PR #248 (`SetupMintingDelegation`) and the existing `SetProviderAppRewardBeneficiaries`. After M1+M2 an operator can drive the action manually via `/governance/propose|confirm|execute`; M3 (the auto-confirmation engine + automation that submit these calls automatically) and M4 (devnet IT) are a **separate follow-up plan**, gated on the spec's §14.1 config-source decision.

**Tech Stack:** DAML — built with `dpm` (v3.4.11), Rust (actix-web backend, `cargo`), splice DARs — **`splice-api-reward-assignment-v1-1.0.0`** and **`splice-amulet-0.1.19`** (versions verified against devnet — see Global Constraints), copied from `/Users/gyorgybalazsi/splice/daml/dars/`.

## Global Constraints

- **Package placement:** the new DAML lives in the `governance-rewards` package (`daml/governance-rewards/`) created by PR #248. That PR is a **prerequisite** — it also added `ProposalPackage::GovernanceRewards`, the `governance_rewards` `PackageConfig` field + default `#governance-rewards-v1`, and the `propose_action` handler arm for `GovernanceRewards`. **Do not re-add those; reuse them.**
- **Actor-model precision (spec §2):** a party id is never an actor. `governanceParty == provider` (spec §4.1, verified for CBTC). The governed execute carries the decparty's own authority — no extra delegation contract.
- **Splice assign semantics (verified in `splice/daml/splice-api-reward-assignment-v1/daml/Splice/Api/RewardAssignmentV1.daml`):** `RewardBeneficiary { beneficiary : Party, percentage : Decimal }`; `RewardCoupon_AssignBeneficiaries` takes `additionalCoupons : [ContractId RewardCoupon]`, `newBeneficiaries : [RewardBeneficiary]`, `extraArgs : ExtraArgs`; percentages must be in (0.0, 1.0], sum to 1.0, ≤ `maxNumNewBeneficiaries` (≤20), and every target coupon MUST have no beneficiary yet. `Splice.Api.Token.MetadataV1` exports **no** ready-made `emptyExtraArgs`; build `ExtraArgs` locally from `emptyChoiceContext` + `emptyMetadata`.
- **DAR versions (verified against devnet 2026-07-15):** vendor `splice-amulet-0.1.19.dar` (package-id `90987abe…`) and `splice-api-reward-assignment-v1-1.0.0.dar` (id `6f7b7236…`) from `/Users/gyorgybalazsi/splice/daml/dars/` into `daml/dars/`. These package-ids byte-match the packages devnet's DSO issues `RewardCouponV2` under (queried live: 842 of `cbtc-network`'s coupons are amulet `0.1.19`), so the assign will actually apply to the live coupons. **Do NOT bump the whole repo's amulet.** The `AssignRewardBeneficiaries` template imports only the `RewardCoupon` *interface* (`reward-assignment-v1`, which has no amulet dependency), so the `governance-rewards` package's closure is unaffected. `splice-amulet-0.1.19` is needed ONLY by the DAML test, to instantiate concrete `RewardCouponV2` coupons.
- **`dpm` invocation:** this `dpm` (v1.0.10) has no `--package` flag. Build a single package by running `dpm build` from inside its directory (e.g. `cd daml/governance-rewards && dpm build`); `dpm build --all` builds the whole `multi-package.yaml`.
- **Backend boundary validation (spec §9):** `validate()` must reject an empty coupon set, percentages outside (0.0, 1.0], a percentage sum ≠ 1.0 (within tolerance), and > 20 beneficiaries — so a raw API caller fails fast instead of wasting a governance round (the gap PR #248 left for `expires_at_micros`).
- **Golden references** — read before writing, do not invent:
  - `daml/governance-rewards/daml/Governance/Rewards/SetupMintingDelegation.daml` (PR #248) — the `GovernableAction` template shape to copy.
  - `daml/governance-rewards-test/daml/Governance/Rewards/{TestSetupMintingDelegation,TestUtils}.daml` (PR #248) — the propose→confirm→execute test harness (`allocateRewardsTestParties`, `createTestGovernance`, `confirmAndExecute`).
  - `splice/daml/splice-amulet-test/daml/Splice/Scripts/TestRewardAccountingV2.daml` (~line 337) — golden example that creates `RewardCouponV2` coupons and exercises `RewardCoupon_AssignBeneficiaries`, incl. `submitMustFail` cases. Copy its coupon-creation and import lines.
  - `crates/decman/src/server/types.rs` `ProposalType::SetProviderAppRewardBeneficiaries` (~line 627) + `AppRewardBeneficiary` struct (~line 332) + its `validate()` arm.
  - `crates/decman/src/server/action_serializer.rs` `SetProviderAppRewardBeneficiaries` arm (~line 1163), `make_optional_beneficiaries` / `serialize_app_reward_beneficiary`, and helpers `make_party` / `make_numeric` / `make_contract_id` / `make_list` / `make_record` / `field`.
- TDD, DRY, YAGNI, frequent commits. `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test` must stay clean.

## Prerequisites (confirm before Task 1)

1. **PR #248 merged to `main` on 2026-07-15** — so `main` already contains the `governance-rewards` + `governance-rewards-test` packages and the `GovernanceRewards` backend wiring this plan builds on. Base off `main`, not off the rewards-plugin feature branch (`feat/governance/rewards-plugin`).
2. Branch off `main` (your local checkout is on a different branch, so pull first): `git checkout main && git pull && git checkout -b feat/governance/coupon-reassignment-automation`.
3. `dpm` and the Rust toolchain build the repo as-is (`dpm build --all` and `cargo build` succeed before you start).

## File Structure

- **Create** `daml/governance-rewards/daml/Governance/Rewards/AssignRewardBeneficiaries.daml` — the `GovernableAction` template. One responsibility: wrap `RewardCoupon_AssignBeneficiaries`.
- **Modify** `daml/governance-rewards/daml.yaml` — add `../dars/splice-api-reward-assignment-v1-1.0.0.dar` (Task 0). No amulet here — the template uses only the interface.
- **Create** `daml/dars/splice-api-reward-assignment-v1-1.0.0.dar` and `daml/dars/splice-amulet-0.1.19.dar` — copied from the splice checkout (Task 0).
- **Create** `daml/governance-rewards-assign-test/` (new test package: `daml.yaml` + `daml/Governance/Rewards/TestAssignRewardBeneficiaries.daml`) — a **separate** test package that depends on `splice-amulet-0.1.19.dar` for `RewardCouponV2`. Kept separate from PR #248's `governance-rewards-test` (which pulls amulet `0.1.17` via `splice-wallet-0.1.18`) to avoid mixing two amulet versions. Add it to `daml/multi-package.yaml`.
- **Modify** `crates/decman/src/server/types.rs` — add `ProposalType::AssignRewardBeneficiaries` + a `RewardBeneficiary` struct + a `validate()` arm.
- **Modify** `crates/decman/src/server/action_serializer.rs` — add the `build_proposal_create_args` arm + a `serialize_reward_beneficiary` helper.

---

### Task 0: Vendor the reward-assignment + amulet DARs (versions verified against devnet)

The version question is already resolved (see Global Constraints): devnet issues `RewardCouponV2` under `splice-amulet-0.1.19` + `splice-api-reward-assignment-v1-1.0.0`, and those exact DARs are prebuilt in `/Users/gyorgybalazsi/splice/daml/dars/` with package-ids matching devnet. This task just vendors them and wires the main package.

**Files:**
- Create: `daml/dars/splice-api-reward-assignment-v1-1.0.0.dar`, `daml/dars/splice-amulet-0.1.19.dar`
- Modify: `daml/governance-rewards/daml.yaml`

**Interfaces:**
- Produces: a `governance-rewards` package that compiles `import Splice.Api.RewardAssignmentV1 (RewardCoupon, RewardBeneficiary(..), RewardCoupon_AssignBeneficiaries(..))`. (The test package's amulet dep is wired in Task 1.)

- [ ] **Step 1: Copy the two verified DARs into the repo.**

```bash
cp /Users/gyorgybalazsi/splice/daml/dars/splice-api-reward-assignment-v1-1.0.0.dar daml/dars/
cp /Users/gyorgybalazsi/splice/daml/dars/splice-amulet-0.1.19.dar daml/dars/
```

- [ ] **Step 2: Confirm the package-ids match devnet** (guards against a stale/rebuilt DAR).

Run: `dpm inspect-dar daml/dars/splice-api-reward-assignment-v1-1.0.0.dar | grep -c 6f7b72361bc2039369651b4195315a2a5849babafec67b3c96e66ea6e560ec35`
Expected: ≥ 1.
Run: `dpm inspect-dar daml/dars/splice-amulet-0.1.19.dar | grep -c 90987abecbcb1d004b063ddfe3b4b5d46cf3814ce89114a86c8cd75ff3cb8a4b`
Expected: ≥ 1.

- [ ] **Step 3: Add reward-assignment to the main package's data-dependencies.** In `daml/governance-rewards/daml.yaml`, under `data-dependencies`, add:
```yaml
  - ../dars/splice-api-reward-assignment-v1-1.0.0.dar
```
Its only transitive dep, `splice-api-token-metadata-v1-1.0.0.dar`, is already vendored. **Do NOT add amulet here** — the template needs only the interface.

- [ ] **Step 4: Probe-build the main package to confirm the import resolves.** Create a throwaway `daml/governance-rewards/daml/Governance/Rewards/Probe.daml` (module segment must start uppercase):

```haskell
module Governance.Rewards.Probe where
import Splice.Api.RewardAssignmentV1 (RewardCoupon, RewardBeneficiary(..), RewardCoupon_AssignBeneficiaries(..))
probe : RewardBeneficiary -> Party
probe b = b.beneficiary
```

Run: `cd daml/governance-rewards && dpm build`
Expected: PASS. If it fails "module Splice.Api.RewardAssignmentV1 not found," the data-dependency in Step 3 didn't attach — fix and rebuild.

- [ ] **Step 5: Delete the probe and commit.**

```bash
rm daml/governance-rewards/daml/Governance/Rewards/Probe.daml
git add daml/dars/splice-api-reward-assignment-v1-1.0.0.dar daml/dars/splice-amulet-0.1.19.dar daml/governance-rewards/daml.yaml
git commit -m "build(governance-rewards): vendor reward-assignment 1.0.0 + amulet 0.1.19 (devnet-matched)"
```

---

### Task 1: DAML `AssignRewardBeneficiaries` GovernableAction + tests

**Files:**
- Create: `daml/governance-rewards/daml/Governance/Rewards/AssignRewardBeneficiaries.daml` (the template).
- Create: `daml/governance-rewards-assign-test/daml.yaml` (new test package).
- Create: `daml/governance-rewards-assign-test/daml/Governance/Rewards/TestAssignRewardBeneficiaries.daml`.
- Create: `daml/governance-rewards-assign-test/daml/Governance/Rewards/AssignTestUtils.daml` (copy the wallet-free helpers — `allocateRewardsTestParties`, `createTestGovernance`, `submitConfirmations`, `confirmAndExecute` — from PR #248's `governance-rewards-test/.../TestUtils.daml`; they depend only on governance-core/action, no wallet).
- Modify: `daml/multi-package.yaml` (register `governance-rewards-assign-test`).

**Interfaces:**
- Consumes: `Governance.Action (GovernableAction, GovernableActionView(..))`; `Splice.Api.RewardAssignmentV1`; `Splice.Api.Token.MetadataV1 (ExtraArgs(..), emptyChoiceContext, emptyMetadata)`; the copied `AssignTestUtils`.
- Produces: `template AssignRewardBeneficiaries with governanceParty : Party; proposer : Party; primaryCoupon : ContractId RewardCoupon; additionalCoupons : [ContractId RewardCoupon]; newBeneficiaries : [RewardBeneficiary]` — a `GovernableAction`. This exact field set + order is what Task 3's serializer must reproduce.

**Why a separate test package (and the one build risk):** PR #248's `governance-rewards-test` depends on `splice-wallet-0.1.18` (→ amulet `0.1.17`). This test needs amulet `0.1.19` (for `RewardCouponV2`). To keep the two amulet versions apart, the assign tests get their own package that depends on amulet `0.1.19` (not wallet). **Risk:** this test package must also depend on `governance-rewards-v1` (which contains `SetupMintingDelegation` and so transitively pulls `splice-wallet-0.1.18` → amulet `0.1.17`). If `dpm build` reports an amulet-version conflict between `0.1.17` and `0.1.19`, resolve it by **moving `AssignRewardBeneficiaries` into its own wallet-free package** (e.g. `governance-reward-assign-v1`, depending only on `governance-action-v1` + `reward-assignment-v1`) so nothing drags amulet `0.1.17` into the test closure — and update Task 2/3's package references accordingly. If neither coexistence nor the split resolves it, report BLOCKED with the exact `dpm` error.

- [ ] **Step 0: Scaffold the test package.** Create `daml/governance-rewards-assign-test/daml.yaml` (daml.yaml pinned to v3.4.11, matching PR #248's test package) with `data-dependencies`: `../governance-action-v1/.daml/dist/governance-action-v1-0.1.0.dar`, `../governance-core/.daml/dist/governance-core-v1-0.1.0.dar`, `../governance-rewards/.daml/dist/governance-rewards-v1-0.1.0.dar`, `../dars/splice-amulet-0.1.19.dar`, `../dars/splice-api-reward-assignment-v1-1.0.0.dar`, `../dars/testlib-0.1.0.dar` (copy the exact dep paths/build-options from `daml/governance-rewards-test/daml.yaml`, swapping `splice-wallet-0.1.18.dar` → `splice-amulet-0.1.19.dar`). Copy the four helpers into `AssignTestUtils.daml`. Register the package in `daml/multi-package.yaml`.

- [ ] **Step 1: Write the failing happy-path test.** Create `TestAssignRewardBeneficiaries.daml`. Model it on PR #248's `TestSetupMintingDelegation.daml`; for creating `RewardCouponV2` coupons and the assign result, mirror `splice/daml/splice-amulet-test/daml/Splice/Scripts/TestRewardAccountingV2.daml` (~line 337). Test: allocate parties (reuse `allocateRewardsTestParties`), create governance (`createTestGovernance`, threshold 2), the DSO creates two `RewardCouponV2` coupons with `provider = governanceParty`, `beneficiary = None`; propose `AssignRewardBeneficiaries` (primaryCoupon + one additionalCoupon; `newBeneficiaries = [RewardBeneficiary alice 0.8, RewardBeneficiary bob 0.2]`); `confirmAndExecute`; assert the original coupons are archived and one `RewardCouponV2` per beneficiary now exists with the expected `beneficiary` set.

```haskell
module Governance.Rewards.TestAssignRewardBeneficiaries where

import DA.Time (addRelTime, hours)
import Daml.Script

import Splice.Amulet (RewardCouponV2(..))
-- Confirm the `Round` import path at first build; TestRewardAccountingV2 shows it.
import Splice.Api.RewardAssignmentV1 (RewardCoupon, RewardBeneficiary(..))

import Governance.Action (GovernableAction)
import Governance.Rules
import Governance.Rewards.AssignRewardBeneficiaries
import Governance.Rewards.AssignTestUtils

import TestHarness

-- create a RewardCouponV2 owned (provider) by `provider`, unassigned, as an interface CID
mkUnassignedCoupon : Party -> Party -> Decimal -> Script (ContractId RewardCoupon)
mkUnassignedCoupon dso provider amount = do
  now <- getTime
  cid <- submit dso $ createCmd RewardCouponV2 with
    dso
    provider
    round = Round 1                 -- confirm Round ctor at first build
    amount
    expiresAt = now `addRelTime` hours 36
    providerIsObserver = True
    beneficiary = None
  pure (toInterfaceContractId @RewardCoupon cid)

test_assign_happy_path = script do
  parties <- allocateRewardsTestParties
  rulesCid <- createTestGovernance parties
  let gp = parties.governanceParty
  c1 <- mkUnassignedCoupon parties.dso gp 100.0
  c2 <- mkUnassignedCoupon parties.dso gp 100.0
  proposalCid <- submit parties.member1 $ createCmd AssignRewardBeneficiaries with
    governanceParty = gp
    proposer = parties.member1
    primaryCoupon = c1
    additionalCoupons = [c2]
    newBeneficiaries =
      [ RewardBeneficiary with beneficiary = parties.member2; percentage = 0.8
      , RewardBeneficiary with beneficiary = parties.member3; percentage = 0.2 ]
  let ifaceCid : ContractId GovernableAction = toInterfaceContractId proposalCid
  _ <- confirmAndExecute parties rulesCid ifaceCid [parties.member1, parties.member2] parties.member1
  -- original coupons consumed; per-beneficiary coupons now exist
  coupons <- query @RewardCouponV2 gp
  assertMsg "two beneficiary coupons created" (length coupons == 2)
  pure ()
```

- [ ] **Step 2: Run it; verify it fails because the template doesn't exist.**

Run: `cd daml/governance-rewards-assign-test && dpm test`
Expected: FAIL — `AssignRewardBeneficiaries` not in scope.

- [ ] **Step 3: Write the template** `AssignRewardBeneficiaries.daml`:

```haskell
-- Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- | GovernableAction that, when confirmed by governance, assigns
-- governance-chosen beneficiaries to the decparty's CIP-104 reward coupons
-- so they can be minted before they expire.
module Governance.Rewards.AssignRewardBeneficiaries where

import Splice.Api.RewardAssignmentV1
  (RewardCoupon, RewardBeneficiary, RewardCoupon_AssignBeneficiaries(..))
import Splice.Api.Token.MetadataV1 (ExtraArgs(..), emptyChoiceContext, emptyMetadata)

import Governance.Action

-- `Splice.Api.Token.MetadataV1` exports no ready-made `emptyExtraArgs`; build it locally.
emptyExtraArgs : ExtraArgs
emptyExtraArgs = ExtraArgs with
  context = emptyChoiceContext
  meta = emptyMetadata

template AssignRewardBeneficiaries
  with
    governanceParty : Party
      -- ^ The decparty; equals the coupons' provider (spec §4.1).
    proposer : Party
    primaryCoupon : ContractId RewardCoupon
      -- ^ First coupon to assign; the choice is exercised on this one.
    additionalCoupons : [ContractId RewardCoupon]
      -- ^ Further coupons of the same provider, batched into one tx.
    newBeneficiaries : [RewardBeneficiary]
      -- ^ Beneficiary + percentage; percentages in (0,1], sum 1.0, <= maxNumNewBeneficiaries.
  where
    signatory proposer
    observer governanceParty

    interface instance GovernableAction for AssignRewardBeneficiaries where
      view = GovernableActionView with
        governanceParty
        proposer
        actionLabel = "AssignRewardBeneficiaries"
        description = "Assign governance-configured beneficiaries to reward coupons."

      executeImpl = do
        _ <- exercise primaryCoupon RewardCoupon_AssignBeneficiaries with
               additionalCoupons
               newBeneficiaries
               extraArgs = emptyExtraArgs
        pure ()
```

- [ ] **Step 4: Run the happy-path test; verify it passes.**

Run: `cd daml/governance-rewards-assign-test && dpm test`
Expected: PASS.

- [ ] **Step 5: Add the negative tests** to `TestAssignRewardBeneficiaries.daml` (the splice choice enforces these; assert we surface them through the governed execute). Mirror the `submitMustFail` cases in `TestRewardAccountingV2.daml`:

```haskell
test_assign_already_assigned_fails = script do
  parties <- allocateRewardsTestParties
  rulesCid <- createTestGovernance parties
  let gp = parties.governanceParty
  c1 <- mkUnassignedCoupon parties.dso gp 100.0
  -- assign once (happy), then a second proposal over the same coupon must fail at execute
  let mkProp p = submit parties.member1 $ createCmd AssignRewardBeneficiaries with
        governanceParty = gp; proposer = parties.member1
        primaryCoupon = c1; additionalCoupons = []
        newBeneficiaries = [RewardBeneficiary with beneficiary = p; percentage = 1.0]
  prop1 <- mkProp parties.member2
  _ <- confirmAndExecute parties rulesCid (toInterfaceContractId prop1)
         [parties.member1, parties.member2] parties.member1
  prop2 <- mkProp parties.member3
  -- second execute over the now-consumed coupon must fail all-or-nothing
  confs <- submitConfirmations gp rulesCid (toInterfaceContractId prop2) [parties.member1, parties.member2]
  submitMustFail (actAs parties.member1 <> readAs gp <> readAs parties.member3) $
    exerciseCmd rulesCid GovernanceRules_ExecuteConfirmedAction with
      executor = parties.member1
      actionProposalCid = toInterfaceContractId prop2
      confirmations = confs

test_assign_percentages_must_sum_to_one_fails = script do
  parties <- allocateRewardsTestParties
  rulesCid <- createTestGovernance parties
  let gp = parties.governanceParty
  c1 <- mkUnassignedCoupon parties.dso gp 100.0
  prop <- submit parties.member1 $ createCmd AssignRewardBeneficiaries with
    governanceParty = gp; proposer = parties.member1
    primaryCoupon = c1; additionalCoupons = []
    newBeneficiaries = [RewardBeneficiary with beneficiary = parties.member2; percentage = 0.5]
  confs <- submitConfirmations gp rulesCid (toInterfaceContractId prop) [parties.member1, parties.member2]
  submitMustFail (actAs parties.member1 <> readAs gp <> readAs parties.member2) $
    exerciseCmd rulesCid GovernanceRules_ExecuteConfirmedAction with
      executor = parties.member1
      actionProposalCid = toInterfaceContractId prop
      confirmations = confs
```

(`submitConfirmations` / `confirmAndExecute` come from the copied `AssignTestUtils`, Step 0.)

- [ ] **Step 6: Run the full DAML suite; verify all pass.**

Run: `(cd daml/governance-rewards-assign-test && dpm test) && (cd daml && dpm build --all)`
Expected: all scripts PASS; all packages build.

- [ ] **Step 7: Commit.**

```bash
git add daml/governance-rewards/daml/Governance/Rewards/AssignRewardBeneficiaries.daml \
        daml/governance-rewards-assign-test/ daml/multi-package.yaml
git commit -m "feat(governance-rewards): AssignRewardBeneficiaries governable action"
```

---

### Task 2: Rust `ProposalType::AssignRewardBeneficiaries` + `validate()`

**Files:**
- Modify: `crates/decman/src/server/types.rs`
- Test: `crates/decman/src/server/types.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: existing `CantonId`, `DamlDecimal`, the `ProposalType` enum (`types.rs:351`), and the `validate()` method's `match self` (`_ => Ok(())` default arm).
- Produces: `ProposalType::AssignRewardBeneficiaries { governance_party_unused: (), primary_coupon: CantonId, additional_coupons: Vec<CantonId>, new_beneficiaries: Vec<RewardBeneficiary> }` — **actually** carry only what the serializer needs: `primary_coupon: CantonId`, `additional_coupons: Vec<CantonId>`, `new_beneficiaries: Vec<RewardBeneficiary>` (governanceParty/proposer are supplied by the serializer's `governance_party`/`proposer` args, exactly as every other variant). Plus `pub struct RewardBeneficiary { pub beneficiary: CantonId, pub percentage: DamlDecimal }`. Task 4's serializer consumes these field names verbatim.

- [ ] **Step 1: Write the failing validation unit tests.** In the `types.rs` tests module:

```rust
#[test]
fn assign_reward_beneficiaries_validate_rejects_empty_coupons() {
    let p = ProposalType::AssignRewardBeneficiaries {
        primary_coupon: None,               // see Step 3: model "no coupons" as empty
        additional_coupons: vec![],
        new_beneficiaries: vec![rb("alice::1220aa", "1.0")],
    };
    assert!(p.validate().is_err());
}

#[test]
fn assign_reward_beneficiaries_validate_rejects_bad_percentages() {
    // sum != 1.0
    let p = ProposalType::AssignRewardBeneficiaries {
        primary_coupon: Some(cid("c1")),
        additional_coupons: vec![],
        new_beneficiaries: vec![rb("alice::1220aa", "0.5")],
    };
    assert!(p.validate().is_err());
    // percentage out of (0,1]
    let p2 = ProposalType::AssignRewardBeneficiaries {
        primary_coupon: Some(cid("c1")),
        additional_coupons: vec![],
        new_beneficiaries: vec![rb("alice::1220aa", "0.0"), rb("bob::1220bb", "1.0")],
    };
    assert!(p2.validate().is_err());
}

#[test]
fn assign_reward_beneficiaries_validate_accepts_valid() {
    let p = ProposalType::AssignRewardBeneficiaries {
        primary_coupon: Some(cid("c1")),
        additional_coupons: vec![cid("c2")],
        new_beneficiaries: vec![rb("alice::1220aa", "0.8"), rb("bob::1220bb", "0.2")],
    };
    assert!(p.validate().is_ok());
}
```

Add small local test helpers next to the tests (or reuse existing `party_id()`/`cid()` if present): `fn rb(p: &str, pct: &str) -> RewardBeneficiary` and `fn cid(s: &str) -> CantonId`. Match the coupon-set representation you pick in Step 3.

- [ ] **Step 2: Run to verify they fail (variant not defined).**

Run: `cargo test -p decman assign_reward_beneficiaries_validate -- --nocapture`
Expected: FAIL — no `AssignRewardBeneficiaries` variant.

- [ ] **Step 3: Add the struct and enum variant.** Place the variant next to `SetProviderAppRewardBeneficiaries` in `ProposalType`, and the struct next to `AppRewardBeneficiary`. Represent the coupon set as a single required `primary_coupon: CantonId` plus `additional_coupons: Vec<CantonId>` — this mirrors the DAML template exactly and makes "at least one coupon" a type-level guarantee, so the "empty coupons" test in Step 1 becomes a `new_beneficiaries`-empty test instead. **Update Step 1's first test accordingly** (rename to `..._rejects_empty_beneficiaries`, drop the `primary_coupon: None` case).

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RewardBeneficiary {
    pub beneficiary: CantonId,
    pub percentage: DamlDecimal,
}

// in `pub enum ProposalType`:
/// Assign governance-configured beneficiaries to the decparty's CIP-104
/// reward coupons via `RewardCoupon_AssignBeneficiaries`. governanceParty ==
/// the coupons' provider (spec §4.1). Percentages sum to 1.0, <= 20 entries.
AssignRewardBeneficiaries {
    primary_coupon: CantonId,
    additional_coupons: Vec<CantonId>,
    new_beneficiaries: Vec<RewardBeneficiary>,
},
```

- [ ] **Step 4: Add the `validate()` arm.** Reuse the numeric-parse pattern already in `types.rs` (`DamlDecimal`/`validate_beneficiary_weights` show it). Add before the `_ => Ok(())` default:

```rust
ProposalType::AssignRewardBeneficiaries { new_beneficiaries, .. } => {
    if new_beneficiaries.is_empty() {
        return Err("new_beneficiaries must not be empty".to_string());
    }
    if new_beneficiaries.len() > 20 {
        return Err("at most 20 beneficiaries per coupon".to_string());
    }
    let mut sum = 0.0_f64;
    for b in new_beneficiaries {
        let p: f64 = b.percentage.to_string().parse()
            .map_err(|_| "percentage is not a number".to_string())?;
        if p <= 0.0 || p > 1.0 {
            return Err("each percentage must be in (0.0, 1.0]".to_string());
        }
        sum += p;
    }
    if (sum - 1.0).abs() > 1e-9 {
        return Err("percentages must sum to 1.0".to_string());
    }
    Ok(())
}
```

(If `DamlDecimal` exposes a typed accessor rather than `to_string()`, use it — match how `validate_beneficiary_weights` reads `AppRewardBeneficiary::weight`.)

- [ ] **Step 5: Run the validation tests; verify they pass.**

Run: `cargo test -p decman assign_reward_beneficiaries_validate`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit.**

```bash
git add crates/decman/src/server/types.rs
git commit -m "feat(decman): ProposalType::AssignRewardBeneficiaries + boundary validation"
```

---

### Task 3: Rust `action_serializer` mapping + round-trip test

**Files:**
- Modify: `crates/decman/src/server/action_serializer.rs`
- Test: `crates/decman/src/server/action_serializer.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `ProposalType::AssignRewardBeneficiaries` + `RewardBeneficiary` (Task 2); helpers `make_party`, `make_numeric`, `make_contract_id`, `make_list`, `make_record`, `field`; `ProposalPackage::GovernanceRewards` (PR #248).
- Produces: a `build_proposal_create_args` arm returning `(ProposalPackage::GovernanceRewards, "Governance.Rewards.AssignRewardBeneficiaries", "AssignRewardBeneficiaries", Record{...})` whose field order is exactly `governanceParty, proposer, primaryCoupon, additionalCoupons, newBeneficiaries` — matching the DAML template from Task 1.

- [ ] **Step 1: Write the failing serialization round-trip test** (mirror `build_proposal_setup_minting_delegation_shape` from PR #248):

```rust
#[test]
fn build_proposal_assign_reward_beneficiaries_shape() -> Result {
    let proposal = ProposalType::AssignRewardBeneficiaries {
        primary_coupon: party_id_str("c1"),          // reuse the test's CantonId ctor
        additional_coupons: vec![party_id_str("c2")],
        new_beneficiaries: vec![
            RewardBeneficiary { beneficiary: party_id(), percentage: dec("0.8") },
            RewardBeneficiary { beneficiary: party_id(), percentage: dec("0.2") },
        ],
    };
    let (package, module, entity, record) =
        build_proposal_create_args("gov", "proposer", &proposal, None, None)?;

    assert_eq!(package, ProposalPackage::GovernanceRewards);
    assert_eq!(module, "Governance.Rewards.AssignRewardBeneficiaries");
    assert_eq!(entity, "AssignRewardBeneficiaries");
    assert_eq!(
        owned_labels(&record),
        ["governanceParty", "proposer", "primaryCoupon", "additionalCoupons", "newBeneficiaries"]
    );
    // additionalCoupons is a list of one; newBeneficiaries is a list of two records
    assert!(matches!(field_value(&record, "additionalCoupons").sum, Some(value::Sum::List(_))));
    assert!(matches!(field_value(&record, "newBeneficiaries").sum, Some(value::Sum::List(_))));
    Ok(())
}
```

(Reuse the existing test helpers `owned_labels`, `field_value`, `party_id`; add `dec(&str) -> DamlDecimal` and a `CantonId` ctor if not present — match the PR #248 test module.)

- [ ] **Step 2: Run to verify it fails (arm missing).**

Run: `cargo test -p decman build_proposal_assign_reward_beneficiaries_shape`
Expected: FAIL — non-exhaustive match / arm missing.

- [ ] **Step 3: Add the `serialize_reward_beneficiary` helper** (next to `serialize_app_reward_beneficiary`):

```rust
fn serialize_reward_beneficiary(b: &RewardBeneficiary) -> Value {
    make_record(vec![
        field("beneficiary", make_party(&b.beneficiary)),
        field("percentage", make_numeric(&b.percentage.to_string())),
    ])
}
```

- [ ] **Step 4: Add the `build_proposal_create_args` arm** (next to the `SetProviderAppRewardBeneficiaries` arm):

```rust
ProposalType::AssignRewardBeneficiaries {
    primary_coupon,
    additional_coupons,
    new_beneficiaries,
} => (
    ProposalPackage::GovernanceRewards,
    "Governance.Rewards.AssignRewardBeneficiaries",
    "AssignRewardBeneficiaries",
    Record {
        record_id: None,
        fields: vec![
            field("governanceParty", make_party(governance_party)),
            field("proposer", make_party(proposer)),
            field("primaryCoupon", make_contract_id(primary_coupon)),
            field(
                "additionalCoupons",
                make_list(additional_coupons.iter().map(make_contract_id).collect()),
            ),
            field(
                "newBeneficiaries",
                make_list(new_beneficiaries.iter().map(serialize_reward_beneficiary).collect()),
            ),
        ],
    },
),
```

(If `make_contract_id` takes `&str`, pass `primary_coupon.as_ref()` / map with a closure — match its signature at `action_serializer.rs:56`.)

- [ ] **Step 5: Run the round-trip test; verify it passes.**

Run: `cargo test -p decman build_proposal_assign_reward_beneficiaries_shape`
Expected: PASS.

- [ ] **Step 6: Full backend gate.**

Run: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p decman`
Expected: all clean/pass.

- [ ] **Step 7: Commit.**

```bash
git add crates/decman/src/server/action_serializer.rs
git commit -m "feat(decman): serialize AssignRewardBeneficiaries proposal args"
```

---

### Task 4: End-to-end smoke check (manual, no new code)

**Files:** none (verification only).

**Interfaces:** Consumes everything above; confirms the action is drivable through the existing `/governance/propose|confirm|execute` endpoints with `"type": "assign_reward_beneficiaries"`.

- [ ] **Step 1: Confirm the whole repo builds and tests green.**

Run: `(cd daml && dpm build --all) && (cd daml/governance-rewards-assign-test && dpm test) && cargo test -p decman`
Expected: all pass.

- [ ] **Step 2: Confirm the propose payload shape** an operator (or, later, the automation) will send — record it in the PR description for reviewers:

```json
{
  "party_id": "cbtc-network::1220...",
  "rules_contract_id": "<governance-rules-cid>",
  "proposal": {
    "type": "assign_reward_beneficiaries",
    "primary_coupon": "<reward-coupon-cid>",
    "additional_coupons": ["<reward-coupon-cid>", "..."],
    "new_beneficiaries": [
      { "beneficiary": "cbtc-beneficiary::1220...", "percentage": "0.8" },
      { "beneficiary": "operator::1220...", "percentage": "0.2" }
    ]
  }
}
```

- [ ] **Step 3: Open the PR.** Title: `feat(governance-rewards): AssignRewardBeneficiaries action + backend plumbing`. In the body, state that this is M1+M2 of the Mode A coupon-reassignment automation (spec `docs/superpowers/specs/2026-07-14-cip104-coupon-reassignment-design.md`); the automation that *submits* these proposals/confirmations (M3) and the devnet IT (M4) follow once §14.1 (the split-source interface) is pinned in team coordination.

---

## What this plan intentionally does NOT cover (next plan: M3+M4)

- The **auto-confirmation engine** and the **automation** (proposer + confirmer background loops) — spec §5, §9. **Blocked** on spec §14.1: how the *effective split* is sourced on-ledger (a shared config template vs. compose from `InstrumentConfiguration` + `AppRewardConfiguration`). Design M3 against a `SplitSource` trait so the source is swappable.
- **Devnet integration test** (multi-node propose → auto-confirm → execute against live `cbtc-network` coupons) — spec §13, needs multiple DecMan instances.
- **Frontend** form — not needed; the action is automation-driven, not human-entered.

## Self-review notes (already reconciled)

- **Spec coverage:** M1 → Task 1 (DAML action + the spec's §7 template, incl. execute-time all-or-nothing behavior tested); M2 → Tasks 2–3 (`ProposalType` + `validate()` boundary checks from spec §9 + serializer). §6.2's "new plumbing" list is satisfied minus the items PR #248 already added (`ProposalPackage`, handler arm, `PackageConfig`) — called out in Global Constraints.
- **Type consistency:** the DAML template field order (`governanceParty, proposer, primaryCoupon, additionalCoupons, newBeneficiaries`, Task 1) equals the serializer's field order (Task 3) equals the round-trip test's `owned_labels` assertion. `RewardBeneficiary { beneficiary, percentage }` is identical in DAML (splice), the Rust struct (Task 2), and the serializer (Task 3).
- **Assumptions surfaced:** Task 0 exists precisely because the reward-assignment / `RewardCouponV2` DAML availability is unverified (spec §4.5) — it fails loudly and vendors if needed rather than assuming.
