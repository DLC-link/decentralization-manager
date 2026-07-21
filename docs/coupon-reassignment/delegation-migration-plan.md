# CIP-104 Coupon-Reassignment — Delegation-Model Migration Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate the *built, green* CIP-104 Mode-A automation from the auto-confirmation-engine model to the **delegation model** (spec rev. 2026-07-20): ONE governance vote creates a `CouponReassignmentDelegation` (split baked in), and a per-node loop thereafter reassigns each round's coupons with a plain 1-of-n `Delegation_Assign` exercise — no per-round propose/confirm/execute.

**Architecture:** This is a **delta on top of the existing branch** `feat/governance/coupon-reassignment-automation`, not a fresh build. Recon (2026-07-20) confirmed the reusable read side (`active_created_records`, `unassigned_coupons`, `CouponInfo`, `select_batch`, field decoders, governance plumbing) shares **no types** with the auto-confirm engine — `PendingAssign` is confined to engine fns and `select_batch` has zero engine coupling. So we **add** the delegation DAML + one setup/revoke `GovernableAction` + a `CouponReassignmentDelegation` reader + a `Delegation_Assign` assigner, **rewrite** the single wiring fn `run_once_for_party`, then **delete** the engine (proposer/confirmer/`is_confirmable`/`split_matches`/`PendingAssign`/…) and the now-dead DAML (`AssignRewardBeneficiaries`, `RewardSplitConfig`, `SetRewardSplit`). Task order keeps the build green at every step: additions first (old + new coexist and compile), removals last.

**Tech Stack:** DAML — built with `dpm` (v3.4.11, LF `--target=2.2`), Rust (actix-web backend, `tokio`, `tonic`/`CommandServiceClient`), splice DARs `splice-api-reward-assignment-v1-1.0.0` + `splice-amulet` + token-metadata (already vendored). Package `governance-rewards` (currently `0.1.2`).

## Global Constraints

- **Branch / worktree:** work on `feat/governance/coupon-reassignment-automation`, worktree `/Users/gyorgybalazsi/dm-reward-cranker`. PR #255, base `main`. This plan **reworks** the M1–M3 engine already on this branch; it does not start a new branch. All recon line numbers below are as of 2026-07-20 — re-grep if the branch moved.
- **DAML package version bump:** changing `governance-rewards` templates requires a version bump **`0.1.2` → `0.1.3`** (`daml/governance-rewards/daml.yaml:3`). Do the bump in **Task 1** (the first template change), because the test package `governance-rewards-assign-test` resolves `governance-rewards` from the built DAR — its tests cannot see a new template until the bumped DAR is rebuilt and placed where its `data-dependency` points. **Repoint EVERY package that pins `governance-rewards`, not just the assign-test** — `rg governance-rewards-v1-0.1.2.dar daml/ -g '*.yaml'` finds them; at minimum `governance-rewards-assign-test/daml.yaml` **and `governance-rewards-test/daml.yaml`** (the sibling #256 package). These pins are `.daml/dist` build-output paths (gitignored), so on a clean checkout `dpm build --all` rebuilds `governance-rewards` first and dependents resolve the fresh `-0.1.3.dar` in dependency order; a missed pin breaks `dpm build --all`/CI on a clean clone. Every DAML task that changes template membership (**Tasks 1, 2, 8**) therefore ends by rebuilding. The version stays `0.1.3` throughout (it is unreleased pre-merge); only its contents evolve. Rust resolves the package by the version-agnostic name alias `#governance-rewards-v1` (`PackageConfig.governance_rewards`), so the bump needs no Rust change.
- **TS bindings are gitignored + generated:** `crates/decman/frontend/src/types.generated.ts` regenerates from the Rust DTOs via `cargo run --features typegen --bin gen-types` — run it after any `ProposalType` change (adds **and** removals). The propose-form `switch` in `GovernanceSection.tsx` has a `default:` that covers automation-only variants; **do not add UI form cases** for `setup_coupon_reassignment_delegation` / `revoke_coupon_reassignment_delegation`, and delete no other UI (the deleted `set_reward_split` / `assign_reward_beneficiaries` never had cases).
- **Split lives in the delegation (spec §8):** the split is a `[RewardBeneficiary]` field baked into `CouponReassignmentDelegation` — there is **no** `RewardSplitConfig`, **no** `getBeneficiaries` composition, **no** operator-cut math. "The split is whatever governance configured."
- **No mode selector (spec §14):** enablement = **presence of exactly one active `CouponReassignmentDelegation`** for the decparty (0 ⇒ off; >1 ⇒ refuse + alert). No mode flag, no DB migration. One global tick interval.
- **1-of-n, split baked in (spec §7, §12 — the security boundary):** `Delegation_Assign` takes `assigner : Party`, is `controller assigner`, and `assert (assigner `elem` assigners)`; its `newBeneficiaries` is the contract's `split` field, **never** a caller argument. This choice gets a security-focused review (Task 1).
- **Co-hosting (spec §4.6) — relied on by the assigner:** each member's participant hosts both the member party and the decparty, so the `Delegation_Assign` submitter (a member/assigner) sees the decparty's coupons + delegation locally and the decparty's signatory authority is available. The exercise command therefore sets `act_as = [assigner]`, `read_as = [decparty]` (Task 5) so the provider-owned coupons are disclosed to the nested `RewardCoupon_AssignBeneficiaries` fetch.
- **Exact `DamlDecimal` everywhere:** percentages compared/summed with exact `DamlDecimal` (no f64, no epsilon) — DAML `total == 1.0`, Rust `validate_reward_beneficiaries` (`types.rs:825`). Reuse that helper; do not reintroduce tolerance.
- **Reuse, don't duplicate (verified anchors, worktree paths):**
  - Decoded ACS read: `active_created_records` (`reward_automation/mod.rs:153`); field decoders `field_party_id` (72), `field_decimal` (82), `field_time` (92), `field_contract_id` (103), `field_contract_id_list` (111), `field_optional_is_none` (130), `parse_beneficiary_list` (266). Coupon reader `unassigned_coupons` (366); `CouponInfo` (349); `COUPON_TTL` (524); `select_batch` (535).
  - Governance plumbing (all `pub(crate)`, `handlers/governance.rs`): `submit_proposal` (1056), `resolve_active_governance_rules` (2112, returns `(rules_cid, threshold)`), `execute_confirm_action` (2130), `get_party_credentials` (2078), `packages` (2098).
  - Serializer helpers (`action_serializer.rs`): `make_party` (26), `make_contract_id` (56), `field` (62), `make_list` (88), `make_optional_contract_id` (252), `serialize_reward_beneficiary` (339).
  - Loop skeleton + spawn: `run_reward_automation_loop` (`reward_automation/mod.rs:802`) spawned at `server/mod.rs:1031–1034`; config field `reward_automation_interval_secs` (`config.rs:253`, default via `Default` impl at 279; CLI env `DECPM_REWARD_AUTOMATION_INTERVAL_SECS` at `cli.rs:202`).
  - No pooled ledger client — each submitter builds a fresh `CommandServiceClient` channel (idiom inside `submit_proposal` / `execute_confirm_action`). Do not add a pool.
- TDD, DRY, YAGNI, frequent commits. `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test -p decman`, `dpm build --all`, and `(cd daml/governance-rewards-assign-test && dpm test)` must be green at every task boundary — **except** that clippy `-D warnings` is deferred across the **Tasks 4–7** window: those tasks add `pub(crate)` fns (the reader, the assigner) *before* Task 6 wires them into the loop, and leave the old engine fns dead until Task 7 deletes them — both trip `dead_code`, which `-D warnings` promotes to errors. Tasks 4–6 therefore gate on `cargo fmt` + `cargo build` + `cargo test` (plain build/test tolerate `dead_code` as warnings; decman does not `#![deny(warnings)]` in-source — only the explicit clippy flag denies). **Task 7 Step 5 is the first point that must pass full `clippy -D warnings`**, and it does (everything wired, engine gone).

## Prerequisites (confirm before Task 1)

1. Branch `feat/governance/coupon-reassignment-automation` checked out in the worktree; the **auto-confirm engine (M1–M3) is present and green** — that is the code we migrate *from*. `cargo test -p decman` and `dpm build --all` pass on HEAD.
2. `governance-rewards` at `0.1.2` (`daml.yaml:3`); splice reward-assignment + amulet + token-metadata DARs vendored (present as `data-dependencies`).
3. The delegation-model design is authoritative: `design.md` in this directory (canonical copy in `cip-104`: `docs/superpowers/specs/2026-07-14-cip104-coupon-reassignment-design.md`, rev. 2026-07-20). This plan implements it.

## File Structure

**Create (DAML):**
- `daml/governance-rewards/daml/Governance/Rewards/CouponReassignmentDelegation.daml` — the standing delegation: template + `Delegation_Assign` (nonconsuming, 1-of-n) + `Delegation_Revoke`; carries `emptyExtraArgs` (moved from the deleted `AssignRewardBeneficiaries.daml`). One responsibility: hold the baked-in split + assigners, and reassign a caller-supplied coupon batch to that split.
- `daml/governance-rewards/daml/Governance/Rewards/SetupCouponReassignmentDelegation.daml` — the `GovernableAction` that creates (or atomically replaces) the delegation.
- `daml/governance-rewards/daml/Governance/Rewards/RevokeCouponReassignmentDelegation.daml` — the `GovernableAction` that disables (archives) the delegation.
- `daml/governance-rewards-assign-test/daml/Governance/Rewards/TestCouponReassignmentDelegation.daml` — DAML tests (reuse `AssignTestUtils`).

**Modify:**
- `crates/decman/src/server/types.rs` — add `ProposalType::SetupCouponReassignmentDelegation` + `RevokeCouponReassignmentDelegation` variants + `validate()` arms; **later** remove `AssignRewardBeneficiaries` + `SetRewardSplit` variants + arms.
- `crates/decman/src/server/action_serializer.rs` — add two serializer arms; widen `make_party`/`make_contract_id`/`make_list`/`field` to `pub(crate)`; **later** remove the two dead arms.
- `crates/decman/src/server/reward_automation/mod.rs` — add `ActiveDelegation` + `active_delegation` reader + `field_party_list` helper + `run_reassign_once` + `submit_delegation_assign`; rewrite `run_once_for_party`; **later** delete the engine fns + `effective_split` + `parse_split_record` + dead decoders + engine imports + engine tests.

**Delete (in Task 8):**
- DAML: `AssignRewardBeneficiaries.daml`, `RewardSplitConfig.daml`, `SetRewardSplit.daml`; test modules `TestAssignRewardBeneficiaries.daml`, `TestSetRewardSplit.daml`.
- Rust: engine fns/structs/tests listed in Task 7.

**Untouched:** `AcceptExternalPartySetup.daml`, `SetupMintingDelegation.daml` (the collection workstream, #256); `server/mod.rs` spawn block (1031–1034) and `config.rs` interval field — the loop fn changes body only, not its name/signature.

---

### Task 1: DAML — `CouponReassignmentDelegation` (template + `Delegation_Assign` + `Delegation_Revoke`) + tests

**Files:**
- Create: `daml/governance-rewards/daml/Governance/Rewards/CouponReassignmentDelegation.daml`
- Create: `daml/governance-rewards-assign-test/daml/Governance/Rewards/TestCouponReassignmentDelegation.daml`
- Modify: `daml/governance-rewards-assign-test/daml/Governance/Rewards/AssignTestUtils.daml` — relocate `mkUnassignedCoupon` here (shared; survives Task 8).
- Modify: `daml/governance-rewards-assign-test/daml/Governance/Rewards/TestAssignRewardBeneficiaries.daml` — import `mkUnassignedCoupon` from `AssignTestUtils` instead of defining it (keeps the M1 test green until Task 8 deletes it).

**Interfaces:**
- Consumes (template `CouponReassignmentDelegation.daml`, Step 3): `Splice.Api.RewardAssignmentV1 (RewardBeneficiary, RewardCoupon, RewardCoupon_AssignBeneficiaries(..))` + the token-metadata `ExtraArgs`/`emptyChoiceContext`/`emptyMetadata` — **copy the import lines + the `emptyExtraArgs` definition verbatim from the existing `AssignRewardBeneficiaries.daml` (lines 20–23)** before it is deleted.
- Consumes (tests — **GWT / `TestHarness` style**, matching `governance-rewards-test`): `TestHarness` (`Test{given,when,then_}`, `run`, `Failures`, `shouldBe`) + `AssignTestUtils` (`allocateRewardsTestParties`, `TestParties` with `.dso`/`.governanceParty`/`.member1..3`, and for Task 2 `createTestGovernance`/`confirmAndExecute`/`submitConfirmations`) + `Splice.Amulet (RewardCouponV2(..))` (for `query @RewardCouponV2`). **No `daml.yaml` change needed** — `testlib-0.1.0.dar` (→ `TestHarness`) and the local `AssignTestUtils` are already deps of `governance-rewards-assign-test`. `mkUnassignedCoupon` is **relocated into `AssignTestUtils`** (Step 0b — single source, no local copy) and imported via the whole-module `AssignTestUtils` import, since `TestAssignRewardBeneficiaries.daml` is deleted in Task 8. Assertions query the concrete `RewardCouponV2` (`query @RewardCouponV2`), not the interface.
- Produces: `template CouponReassignmentDelegation with decparty : Party; assigners : [Party]; split : [RewardBeneficiary]`; `nonconsuming choice Delegation_Assign with assigner : Party; primaryCoupon : ContractId RewardCoupon; additionalCoupons : [ContractId RewardCoupon]`; `choice Delegation_Revoke : ()`.

- [ ] **Step 0: Bump the package version + wire the test dependency.** In `daml/governance-rewards/daml.yaml:3`, `version: 0.1.2` → `version: 0.1.3`. Open `daml/governance-rewards-assign-test/daml.yaml`, find its `governance-rewards` `data-dependency`, and note the exact path it uses for `-0.1.2.dar` (it is a `.daml/dist` build-output path); repoint it to `-0.1.3.dar`. **Also repoint every OTHER package that pins `governance-rewards`:** `rg governance-rewards-v1-0.1.2.dar daml/ -g '*.yaml'` and change each — notably **`governance-rewards-test/daml.yaml`** (the sibling #256 package) — to `-0.1.3.dar`, or a clean `dpm build --all` (Step 6 / CI) fails to resolve the old DAR. This must happen first — the test module in Step 1 cannot compile against the new template until the `0.1.3` DAR exists where these refs point (Step 6 rebuilds it).

- [ ] **Step 0b: Relocate `mkUnassignedCoupon` into the shared test-utils.** Move the `mkUnassignedCoupon` definition (currently `TestAssignRewardBeneficiaries.daml:19`, `Party -> Party -> Decimal -> Script (ContractId RewardCoupon)`) into `daml/governance-rewards-assign-test/daml/Governance/Rewards/AssignTestUtils.daml` (survives Task 8), moving its imports with it (`Splice.Amulet (RewardCouponV2(..))`, `Splice.Types (Round(..))`, `Splice.Api.RewardAssignmentV1 (RewardCoupon)`, `DA.Time (addRelTime, hours)`). Update `TestAssignRewardBeneficiaries.daml` to import it from `AssignTestUtils` rather than define it (keeps the M1 test green until Task 8 deletes it). Single source — no duplication.

- [ ] **Step 1: Write the failing happy-path + security tests.** Create `TestCouponReassignmentDelegation.daml`:

```haskell
module Governance.Rewards.TestCouponReassignmentDelegation where

import Daml.Script
import Splice.Amulet (RewardCouponV2(..))
import Splice.Api.RewardAssignmentV1 (RewardBeneficiary(..))
import Governance.Rewards.CouponReassignmentDelegation
import Governance.Rewards.AssignTestUtils   -- allocateRewardsTestParties, TestParties (has .dso)
import TestHarness                            -- Test{given,when,then_}, run, Failures, shouldBe

-- mkUnassignedCoupon lives in AssignTestUtils (relocated in Step 0b) — shared, survives Task 8.

-- GWT fixture: parties + a delegation created DIRECTLY (in production it is created by
-- the SetupCouponReassignmentDelegation governance action — Task 2). decparty is the
-- coupons' provider; assigners are the member parties (1-of-n).
data Fixture = Fixture with
    parties : TestParties
    split   : [RewardBeneficiary]
    delId   : ContractId CouponReassignmentDelegation

given_delegation : Script Fixture
given_delegation = do
  parties <- allocateRewardsTestParties
  let gp = parties.governanceParty
  let split = [ RewardBeneficiary with beneficiary = parties.member2; percentage = 0.8
              , RewardBeneficiary with beneficiary = parties.member3; percentage = 0.2 ]
  delId <- submit gp $ createCmd CouponReassignmentDelegation with
    decparty = gp; assigners = [parties.member1, parties.member2]; split
  pure Fixture with ..

-- shared no-assertion then_ for negative cases (when : Fixture -> Script ())
then_nothing : Fixture -> a -> Script Failures
then_nothing _ _ = pure []

-- happy path: a SINGLE assigner (member1), acting alone, reassigns one coupon to the
-- baked-in split -> one RewardCouponV2 per beneficiary (original archived).
when_assigner_reassigns : Fixture -> Script Int
when_assigner_reassigns f = do
  let gp = f.parties.governanceParty
  c1 <- mkUnassignedCoupon f.parties.dso gp 100.0
  submit f.parties.member1 $ exerciseCmd f.delId Delegation_Assign with
    assigner = f.parties.member1; primaryCoupon = c1; additionalCoupons = []
  coupons <- query @RewardCouponV2 gp
  pure (length coupons)

then_two_beneficiary_coupons : Fixture -> Int -> Script Failures
then_two_beneficiary_coupons _ n = pure $ shouldBe "one coupon per beneficiary" 2 n

test_assigner_reassigns_to_baked_split = script do
  run Test with
    given = given_delegation
    when = when_assigner_reassigns
    then_ = then_two_beneficiary_coupons

-- security: a non-assigner (member3, not in `assigners`) is rejected by the elem gate.
when_non_assigner_rejected : Fixture -> Script ()
when_non_assigner_rejected f = do
  c1 <- mkUnassignedCoupon f.parties.dso f.parties.governanceParty 100.0
  submitMustFail f.parties.member3 $ exerciseCmd f.delId Delegation_Assign with
    assigner = f.parties.member3; primaryCoupon = c1; additionalCoupons = []

test_non_assigner_cannot_reassign = script do
  run Test with
    given = given_delegation
    when = when_non_assigner_rejected
    then_ = then_nothing

-- nonconsuming: the same delegation serves two rounds (2 coupons x 2 beneficiaries = 4).
when_two_rounds : Fixture -> Script Int
when_two_rounds f = do
  let gp = f.parties.governanceParty
  c1 <- mkUnassignedCoupon f.parties.dso gp 100.0
  c2 <- mkUnassignedCoupon f.parties.dso gp 50.0
  submit f.parties.member1 $ exerciseCmd f.delId Delegation_Assign with
    assigner = f.parties.member1; primaryCoupon = c1; additionalCoupons = []
  submit f.parties.member1 $ exerciseCmd f.delId Delegation_Assign with
    assigner = f.parties.member1; primaryCoupon = c2; additionalCoupons = []
  coupons <- query @RewardCouponV2 gp
  pure (length coupons)

then_four_beneficiary_coupons : Fixture -> Int -> Script Failures
then_four_beneficiary_coupons _ n = pure $ shouldBe "two rounds x two beneficiaries" 4 n

test_delegation_is_nonconsuming = script do
  run Test with
    given = given_delegation
    when = when_two_rounds
    then_ = then_four_beneficiary_coupons

-- revoke: the decparty archives the delegation.
when_revoke : Fixture -> Script Int
when_revoke f = do
  submit f.parties.governanceParty $ exerciseCmd f.delId Delegation_Revoke
  dels <- query @CouponReassignmentDelegation f.parties.governanceParty
  pure (length dels)

then_no_delegation : Fixture -> Int -> Script Failures
then_no_delegation _ n = pure $ shouldBe "delegation archived" 0 n

test_revoke_archives = script do
  run Test with
    given = given_delegation
    when = when_revoke
    then_ = then_no_delegation
```

- [ ] **Step 2: Run; verify fail (template not in scope).** Run: `cd daml/governance-rewards-assign-test && dpm test`. Expected: FAIL — `CouponReassignmentDelegation` unknown.

- [ ] **Step 3: Write `CouponReassignmentDelegation.daml`:**

```haskell
-- Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- | Standing delegation that carries the decparty's authority and the fixed
-- beneficiary split. Created ONCE via governance (SetupCouponReassignmentDelegation);
-- thereafter any single listed assigner may reassign a coupon batch to the baked-in
-- split with a plain Delegation_Assign exercise — no per-round vote (spec §5, §7, §12).
module Governance.Rewards.CouponReassignmentDelegation where

-- NB: copy these imports + emptyExtraArgs verbatim from AssignRewardBeneficiaries.daml
-- (lines 20-23) before that file is deleted in Task 8.
import Splice.Api.RewardAssignmentV1
  (RewardCoupon, RewardBeneficiary, RewardCoupon_AssignBeneficiaries(..))
import Splice.Api.Token.MetadataV1
  (ExtraArgs(..), emptyChoiceContext, emptyMetadata)

-- `Splice.Api.Token.MetadataV1` exports no ready-made `emptyExtraArgs`; build it from
-- the exported primitives (verbatim from the old `AssignRewardBeneficiaries.daml`).
emptyExtraArgs : ExtraArgs
emptyExtraArgs = ExtraArgs with
  context = emptyChoiceContext
  meta = emptyMetadata

template CouponReassignmentDelegation
  with
    decparty  : Party                 -- = governanceParty = the coupons' provider
    assigners : [Party]               -- = all member parties; any one may reassign (1-of-n)
    split     : [RewardBeneficiary]   -- BAKED IN; validated at create (Task 2 executeImpl)
  where
    signatory decparty
    observer assigners

    -- Per-round reassignment. Nonconsuming: one delegation serves every round
    -- until governance revokes/replaces it.
    nonconsuming choice Delegation_Assign : ()
      with
        assigner          : Party                     -- the submitting member; checked below
        primaryCoupon     : ContractId RewardCoupon
        additionalCoupons : [ContractId RewardCoupon]
      controller assigner                             -- 1-of-n: any listed assigner, NOT a threshold
      do
        assert (assigner `elem` assigners)            -- the caller must be a listed assigner
        -- newBeneficiaries is the contract's `split`, NEVER a caller argument (security boundary).
        _ <- exercise primaryCoupon RewardCoupon_AssignBeneficiaries with
               additionalCoupons
               newBeneficiaries = split
               extraArgs = emptyExtraArgs
        pure ()

    -- Governance-only revoke (consuming → archives). Invoked via a GovernableAction (Task 2).
    choice Delegation_Revoke : ()
      controller decparty
      do pure ()
```

**Note (security review — spec §7, §12):** `Delegation_Assign` is authority-carrying DAML. The review must confirm: (a) `newBeneficiaries = split` reads the contract field, not a choice arg; (b) `controller assigner` + `assert (assigner `elem` assigners)` restricts to a listed member and cannot be escalated by passing another party (the submitter must authorize *as* `assigner`); (c) nonconsuming, so the delegation is not spent per round; (d) the nested `RewardCoupon_AssignBeneficiaries` gets the provider (= `decparty`) authority from the delegation's signatory.

- [ ] **Step 4: Run the tests; verify pass.** Run: `cd daml/governance-rewards-assign-test && dpm test`. Expected: all four `test_*` scripts (each a `run Test with …`) PASS. `mkUnassignedCoupon` is inlined verbatim from the green M1 test, so there's no signature guesswork.

- [ ] **Step 5: Add the "already-assigned coupon rejected" test** (all-or-nothing), in the same GWT style (reuses `given_delegation` + `then_nothing`):

```haskell
when_reassign_same_coupon_fails : Fixture -> Script ()
when_reassign_same_coupon_fails f = do
  let gp = f.parties.governanceParty
  c1 <- mkUnassignedCoupon f.parties.dso gp 100.0
  submit f.parties.member1 $ exerciseCmd f.delId Delegation_Assign with
    assigner = f.parties.member1; primaryCoupon = c1; additionalCoupons = []
  -- c1 is now consumed/assigned; re-assigning the same cid fails the whole tx (all-or-nothing)
  submitMustFail f.parties.member1 $ exerciseCmd f.delId Delegation_Assign with
    assigner = f.parties.member1; primaryCoupon = c1; additionalCoupons = []

test_already_assigned_coupon_rejected = script do
  run Test with
    given = given_delegation
    when = when_reassign_same_coupon_fails
    then_ = then_nothing
```

- [ ] **Step 6: DAML gate (build + ship + test) + commit.** Run `(cd daml && dpm build --all)`, then place the freshly built `governance-rewards-v1-0.1.3.dar` where the test package's `data-dependency` points (Step 0 — e.g. `cp` into `releases/v1/` if that is the ref), then `(cd daml/governance-rewards-assign-test && dpm test)`. Then:
```bash
git add daml/governance-rewards/daml/Governance/Rewards/CouponReassignmentDelegation.daml \
        daml/governance-rewards-assign-test/daml/Governance/Rewards/TestCouponReassignmentDelegation.daml \
        daml/governance-rewards/daml.yaml daml/governance-rewards-assign-test/daml.yaml \
        releases/v1/governance-rewards-v1-0.1.3.dar
git commit -m "feat(governance-rewards): CouponReassignmentDelegation + Delegation_Assign (1-of-n); gov-rewards 0.1.3"
```

---

### Task 2: DAML — `Setup`/`Revoke` `GovernableAction`s + tests

**Files:**
- Create: `daml/governance-rewards/daml/Governance/Rewards/SetupCouponReassignmentDelegation.daml`
- Create: `daml/governance-rewards/daml/Governance/Rewards/RevokeCouponReassignmentDelegation.daml`
- Modify: `daml/governance-rewards-assign-test/daml/Governance/Rewards/TestCouponReassignmentDelegation.daml` (add governance-path tests)

**Interfaces:**
- Consumes: `Governance.Action (GovernableAction, GovernableActionView(..))`; `Governance.Rewards.CouponReassignmentDelegation`; `DA.Foldable (forA_)`; the `AssignTestUtils` `createTestGovernance` + `confirmAndExecute`. Mirror `SetRewardSplit.daml` structure (the just-superseded action) field-for-field.
- Produces: `template SetupCouponReassignmentDelegation with governanceParty : Party; proposer : Party; priorDelegation : Optional (ContractId CouponReassignmentDelegation); assigners : [Party]; beneficiaries : [RewardBeneficiary]`; `template RevokeCouponReassignmentDelegation with governanceParty : Party; proposer : Party; delegation : ContractId CouponReassignmentDelegation`. Field orders drive Task 4's serializer.

- [ ] **Step 1: Write the failing governance-path tests** (append to `TestCouponReassignmentDelegation.daml`), in the same GWT / `TestHarness` style (mirroring `TestSetupMintingDelegation.daml`'s governance-action tests):

```haskell
import Governance.Rewards.SetupCouponReassignmentDelegation
import Governance.Rewards.RevokeCouponReassignmentDelegation
import Governance.Action (GovernableAction)
import Governance.Rules   -- GovernanceRules, GovernanceRules_ExecuteConfirmedAction
-- createTestGovernance / confirmAndExecute / submitConfirmations come from the
-- AssignTestUtils import already in Step 1 (whole-module import).

-- GWT fixture for the governance-created path: parties + a GovernanceRules + the split.
data GovFixture = GovFixture with
    parties  : TestParties
    rulesCid : ContractId GovernanceRules
    split    : [RewardBeneficiary]

given_governance : Script GovFixture
given_governance = do
  parties <- allocateRewardsTestParties
  rulesCid <- createTestGovernance parties
  let split = [ RewardBeneficiary with beneficiary = parties.member2; percentage = 0.8
              , RewardBeneficiary with beneficiary = parties.member3; percentage = 0.2 ]
  pure GovFixture with ..

then_singleton : GovFixture -> Int -> Script Failures
then_singleton _ n = pure $ shouldBe "exactly one delegation" 1 n

then_zero : GovFixture -> Int -> Script Failures
then_zero _ n = pure $ shouldBe "no delegation" 0 n

then_gov_nothing : GovFixture -> () -> Script Failures
then_gov_nothing _ _ = pure []

-- one governance vote (propose -> confirm -> execute) creates the delegation
when_setup_executed : GovFixture -> Script [(ContractId CouponReassignmentDelegation, CouponReassignmentDelegation)]
when_setup_executed f = do
  let gp = f.parties.governanceParty
  actionCid <- submit f.parties.member1 $ createCmd SetupCouponReassignmentDelegation with
    governanceParty = gp; proposer = f.parties.member1; priorDelegation = None
    assigners = [f.parties.member1, f.parties.member2]; beneficiaries = f.split
  let iface : ContractId GovernableAction = toInterfaceContractId actionCid
  _ <- confirmAndExecute f.parties f.rulesCid iface [f.parties.member1, f.parties.member2] f.parties.member1
  query @CouponReassignmentDelegation gp

then_one_delegation_with_split : GovFixture -> [(ContractId CouponReassignmentDelegation, CouponReassignmentDelegation)] -> Script Failures
then_one_delegation_with_split f dels =
  pure $ shouldBe "one delegation exists" 1 (length dels)
      <> shouldBe "split baked in" [f.split] (map (\(_, d) -> d.split) dels)

test_setup_creates_delegation = script do
  run Test with
    given = given_governance; when = when_setup_executed; then_ = then_one_delegation_with_split

-- replace: a second Setup carrying priorDelegation archives the first (still singleton)
when_setup_replaces : GovFixture -> Script Int
when_setup_replaces f = do
  let gp = f.parties.governanceParty
      mk prior bene = do
        a <- submit f.parties.member1 $ createCmd SetupCouponReassignmentDelegation with
          governanceParty = gp; proposer = f.parties.member1; priorDelegation = prior
          assigners = [f.parties.member1, f.parties.member2]
          beneficiaries = [RewardBeneficiary with beneficiary = bene; percentage = 1.0]
        let iface : ContractId GovernableAction = toInterfaceContractId a
        confirmAndExecute f.parties f.rulesCid iface [f.parties.member1, f.parties.member2] f.parties.member1
  _ <- mk None f.parties.member2
  [(oldCid, _)] <- query @CouponReassignmentDelegation gp
  _ <- mk (Some oldCid) f.parties.member3
  dels <- query @CouponReassignmentDelegation gp
  pure (length dels)

test_setup_replaces_prior = script do
  run Test with
    given = given_governance; when = when_setup_replaces; then_ = then_singleton

-- revoke via governance: the RevokeCouponReassignmentDelegation action archives it
when_revoke_via_governance : GovFixture -> Script Int
when_revoke_via_governance f = do
  let gp = f.parties.governanceParty
  s <- submit f.parties.member1 $ createCmd SetupCouponReassignmentDelegation with
    governanceParty = gp; proposer = f.parties.member1; priorDelegation = None
    assigners = [f.parties.member1, f.parties.member2]; beneficiaries = f.split
  _ <- confirmAndExecute f.parties f.rulesCid (toInterfaceContractId s : ContractId GovernableAction)
         [f.parties.member1, f.parties.member2] f.parties.member1
  [(delCid, _)] <- query @CouponReassignmentDelegation gp
  r <- submit f.parties.member1 $ createCmd RevokeCouponReassignmentDelegation with
    governanceParty = gp; proposer = f.parties.member1; delegation = delCid
  _ <- confirmAndExecute f.parties f.rulesCid (toInterfaceContractId r : ContractId GovernableAction)
         [f.parties.member1, f.parties.member2] f.parties.member1
  dels <- query @CouponReassignmentDelegation gp
  pure (length dels)

test_revoke_via_governance = script do
  run Test with
    given = given_governance; when = when_revoke_via_governance; then_ = then_zero

-- empty split rejected at execute (executeImpl asserts non-empty beneficiaries)
when_empty_split_execute_fails : GovFixture -> Script ()
when_empty_split_execute_fails f = do
  let gp = f.parties.governanceParty
  actionCid <- submit f.parties.member1 $ createCmd SetupCouponReassignmentDelegation with
    governanceParty = gp; proposer = f.parties.member1; priorDelegation = None
    assigners = [f.parties.member1]; beneficiaries = []
  let iface : ContractId GovernableAction = toInterfaceContractId actionCid
  confs <- submitConfirmations gp f.rulesCid iface [f.parties.member1, f.parties.member2]
  submitMustFail (actAs f.parties.member1 <> readAs gp) $
    exerciseCmd f.rulesCid GovernanceRules_ExecuteConfirmedAction with
      executor = f.parties.member1               -- governance executor (NOT the delegation assigner)
      actionProposalCid = iface
      confirmations = confs

test_setup_empty_split_rejected = script do
  run Test with
    given = given_governance; when = when_empty_split_execute_fails; then_ = then_gov_nothing
```
(`submitConfirmations` + `GovernanceRules_ExecuteConfirmedAction` are the M1 negative-test idiom — import `Governance.Rules` and `AssignTestUtils (submitConfirmations)` as `TestSetRewardSplit.daml:45` did.)

- [ ] **Step 2: Run; verify fail.** Run: `cd daml/governance-rewards-assign-test && dpm test`. Expected: FAIL (Setup/Revoke unknown).

- [ ] **Step 3: Write `SetupCouponReassignmentDelegation.daml`** (mirror `SetRewardSplit.daml` executeImpl guards verbatim, then create the delegation):

```haskell
-- Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- | GovernableAction that creates (or atomically replaces) the decparty's
-- CouponReassignmentDelegation. The ONLY reassignment step that takes a
-- governance vote (spec §5, §6, §8). Runs with governanceParty authority.
module Governance.Rewards.SetupCouponReassignmentDelegation where

import DA.Foldable (forA_)
import Splice.Api.RewardAssignmentV1 (RewardBeneficiary(..))
import Governance.Rewards.CouponReassignmentDelegation
import Governance.Action

template SetupCouponReassignmentDelegation
  with
    governanceParty : Party
    proposer : Party
    priorDelegation : Optional (ContractId CouponReassignmentDelegation)
      -- ^ cid of the delegation to replace (None for the first create); revoked at execute.
    assigners : [Party]
    beneficiaries : [RewardBeneficiary]
  where
    signatory proposer
    observer governanceParty

    interface instance GovernableAction for SetupCouponReassignmentDelegation where
      view = GovernableActionView with
        governanceParty
        proposer
        actionLabel = "SetupCouponReassignmentDelegation"
        description = "Create (or replace) the coupon-reassignment delegation."

      executeImpl = do
        -- execute-time guards (a direct ledger submit bypasses the Rust boundary).
        -- Same checks as the Rust validate_reward_beneficiaries + a non-empty assigners set.
        assertMsg "beneficiaries must not be empty" (not (null beneficiaries))
        assertMsg "at most 20 beneficiaries" (length beneficiaries <= 20)
        assertMsg "each percentage must be in (0,1]"
          (all (\b -> b.percentage > 0.0 && b.percentage <= 1.0) beneficiaries)
        let total = sum (map (.percentage) beneficiaries)
        assertMsg "percentages must sum to 1.0" (total == 1.0)   -- exact Decimal
        assertMsg "assigners must not be empty" (not (null assigners))
        forA_ priorDelegation (`exercise` Delegation_Revoke)      -- atomic replace when Some
        _ <- create CouponReassignmentDelegation with
               decparty = governanceParty; assigners; split = beneficiaries
        pure ()
```

- [ ] **Step 4: Write `RevokeCouponReassignmentDelegation.daml`:**

```haskell
-- Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- | GovernableAction that disables (archives) the delegation (spec §8, §11).
module Governance.Rewards.RevokeCouponReassignmentDelegation where

import Governance.Rewards.CouponReassignmentDelegation
import Governance.Action

template RevokeCouponReassignmentDelegation
  with
    governanceParty : Party
    proposer : Party
    delegation : ContractId CouponReassignmentDelegation
  where
    signatory proposer
    observer governanceParty

    interface instance GovernableAction for RevokeCouponReassignmentDelegation where
      view = GovernableActionView with
        governanceParty
        proposer
        actionLabel = "RevokeCouponReassignmentDelegation"
        description = "Revoke the coupon-reassignment delegation."

      executeImpl = do
        _ <- exercise delegation Delegation_Revoke
        pure ()
```

- [ ] **Step 5: Run the tests; verify pass.** Run: `cd daml/governance-rewards-assign-test && dpm test`. Expected: all scripts PASS (incl. Task 1's). Fix `.split`/`.beneficiaries` projection syntax if the build flags it.

- [ ] **Step 6: DAML gate (build + ship + test) + commit.** Run `(cd daml && dpm build --all)`, re-place the rebuilt `0.1.3` DAR where the test ref points (Task 1 Step 0), then `(cd daml/governance-rewards-assign-test && dpm test)`. Then:
```bash
git add daml/governance-rewards/daml/Governance/Rewards/SetupCouponReassignmentDelegation.daml \
        daml/governance-rewards/daml/Governance/Rewards/RevokeCouponReassignmentDelegation.daml \
        daml/governance-rewards-assign-test/daml/Governance/Rewards/TestCouponReassignmentDelegation.daml \
        releases/v1/governance-rewards-v1-0.1.3.dar
git commit -m "feat(governance-rewards): Setup/Revoke CouponReassignmentDelegation governance actions"
```

---

### Task 3: Rust — add `ProposalType::Setup/Revoke…` variants + validate + serializer

**Files:**
- Modify: `crates/decman/src/server/types.rs` (+ `#[cfg(test)] mod tests`)
- Modify: `crates/decman/src/server/action_serializer.rs` (+ tests)

**Interfaces:**
- Consumes: `RewardBeneficiary` (`types.rs:343`); `validate_reward_beneficiaries` (`types.rs:825`, exact `DamlDecimal`); serializer helpers `make_party`, `make_contract_id`, `make_list`, `make_optional_contract_id`, `serialize_reward_beneficiary`, `field`; `ProposalPackage::GovernanceRewards`; `CantonId`.
- Produces: `ProposalType::SetupCouponReassignmentDelegation { assigners: Vec<CantonId>, new_beneficiaries: Vec<RewardBeneficiary>, prior_delegation: Option<String> }` and `ProposalType::RevokeCouponReassignmentDelegation { delegation: String }`; matching `validate()` arms and serializer arms.

- [ ] **Step 1: Write the failing validation tests** (in `types.rs` tests, next to `set_reward_split_validate` at ~1560):

```rust
#[test]
fn setup_delegation_validate() {
    // Reuse the `rb` helper from the neighboring set_reward_split_validate test;
    // `rb(..).beneficiary` yields a CantonId (there is no dedicated party-id helper).
    let execs = vec![rb("m1::1220ab", "1.0").beneficiary, rb("m2::1220cd", "1.0").beneficiary];
    let ok = ProposalType::SetupCouponReassignmentDelegation {
        assigners: execs.clone(),
        new_beneficiaries: vec![rb("a::1220aa", "0.8"), rb("b::1220bb", "0.2")],
        prior_delegation: None,
    };
    assert!(ok.validate().is_ok());
    let no_exec = ProposalType::SetupCouponReassignmentDelegation {
        assigners: vec![], new_beneficiaries: vec![rb("a::1220aa", "1.0")], prior_delegation: None };
    assert!(no_exec.validate().is_err());
    let bad_sum = ProposalType::SetupCouponReassignmentDelegation {
        assigners: execs, new_beneficiaries: vec![rb("a::1220aa", "0.5")], prior_delegation: None };
    assert!(bad_sum.validate().is_err());
    let revoke = ProposalType::RevokeCouponReassignmentDelegation { delegation: "00abc".into() };
    assert!(revoke.validate().is_ok());
}
```
(Uses only the existing `types.rs` `rb` helper that the neighboring `set_reward_split_validate` test uses — a mod-level helper, so it survives Task 8's removal of that test. Match `rb`'s actual party-id convention if the build flags it.)

- [ ] **Step 2: Run; verify fail (variants missing).** Run: `cargo test -p decman setup_delegation_validate`. Expected: FAIL.

- [ ] **Step 3: Add the variants + validate arms.** In `types.rs`, next to the existing reward variants (~647):

```rust
/// Create (or replace) the decparty's on-ledger CouponReassignmentDelegation.
/// `prior_delegation` is the cid of the delegation being replaced (None for the first).
SetupCouponReassignmentDelegation {
    assigners: Vec<CantonId>,
    new_beneficiaries: Vec<RewardBeneficiary>,
    #[serde(default)]
    prior_delegation: Option<String>,
},
/// Revoke (archive) the decparty's CouponReassignmentDelegation.
RevokeCouponReassignmentDelegation { delegation: String },
```
In `validate()` (before the `_ => Ok(())` at ~815):
```rust
ProposalType::SetupCouponReassignmentDelegation { assigners, new_beneficiaries, .. } => {
    if assigners.is_empty() {
        return Err("assigners must not be empty".to_string());
    }
    validate_reward_beneficiaries(new_beneficiaries)
}
ProposalType::RevokeCouponReassignmentDelegation { .. } => Ok(()),
```

- [ ] **Step 4: Run validate tests; verify pass.** Run: `cargo test -p decman setup_delegation_validate`. Expected: PASS.

- [ ] **Step 5: Write the failing serializer round-trip test** (`action_serializer.rs` tests, mirroring the `SetRewardSplit` shape test at ~2595):

```rust
#[test]
fn build_proposal_setup_delegation_shape() -> Result {
    let proposal = ProposalType::SetupCouponReassignmentDelegation {
        assigners: vec![party_id(), party_id()],
        new_beneficiaries: vec![
            RewardBeneficiary { beneficiary: party_id(), percentage: dec("0.8") },
            RewardBeneficiary { beneficiary: party_id(), percentage: dec("0.2") },
        ],
        prior_delegation: Some("00old".to_string()),
    };
    let (package, module, entity, record) =
        build_proposal_create_args("gov", "proposer", &proposal, None, None)?;
    assert_eq!(package, ProposalPackage::GovernanceRewards);
    assert_eq!(module, "Governance.Rewards.SetupCouponReassignmentDelegation");
    assert_eq!(entity, "SetupCouponReassignmentDelegation");
    assert_eq!(owned_labels(&record),
        ["governanceParty", "proposer", "priorDelegation", "assigners", "beneficiaries"]);
    Ok(())
}
```

- [ ] **Step 6: Add the serializer arms** (next to the `AssignRewardBeneficiaries` arm at ~1202). Field order MUST match the DAML templates (Task 2):

```rust
ProposalType::SetupCouponReassignmentDelegation { assigners, new_beneficiaries, prior_delegation } => (
    ProposalPackage::GovernanceRewards,
    "Governance.Rewards.SetupCouponReassignmentDelegation",
    "SetupCouponReassignmentDelegation",
    Record {
        record_id: None,
        fields: vec![
            field("governanceParty", make_party(governance_party)),
            field("proposer", make_party(proposer)),
            field("priorDelegation", make_optional_contract_id(prior_delegation)),
            field("assigners", make_list(assigners.iter().map(make_party).collect())),
            field("beneficiaries", make_list(
                new_beneficiaries.iter().map(serialize_reward_beneficiary).collect())),
        ],
    },
),
ProposalType::RevokeCouponReassignmentDelegation { delegation } => (
    ProposalPackage::GovernanceRewards,
    "Governance.Rewards.RevokeCouponReassignmentDelegation",
    "RevokeCouponReassignmentDelegation",
    Record {
        record_id: None,
        fields: vec![
            field("governanceParty", make_party(governance_party)),
            field("proposer", make_party(proposer)),
            field("delegation", make_contract_id(delegation)),
        ],
    },
),
```
(`make_party` takes `impl Display`; `CantonId` is `Display`, so `assigners.iter().map(make_party)` works.)

- [ ] **Step 7: Refresh TS bindings + backend gate + commit.** Run: `cargo run --features typegen --bin gen-types` (do **not** add UI form cases), then `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p decman`. Then:
```bash
git add crates/decman/src/server/types.rs crates/decman/src/server/action_serializer.rs
git commit -m "feat(decman): ProposalType Setup/Revoke CouponReassignmentDelegation + serializer"
```

---

### Task 4: Rust — `active_delegation` reader (enablement + delegation cid + assigners)

**Files:** Modify `crates/decman/src/server/reward_automation/mod.rs` (+ tests).

**Interfaces:**
- Consumes: `active_created_records` (153), `field_party_id` (72), `PackageConfig.governance_rewards`. (Note: `parse_beneficiary_list` is **not** used — the split is not read Rust-side; that helper goes dead and is deleted in Task 7.)
- Produces:
  - `pub(crate) struct ActiveDelegation { pub cid: String, pub assigners: Vec<CantonId> }` — the split is **not** read: it lives in the contract and `Delegation_Assign` enforces it by construction (spec §12), so the Rust side never needs it.
  - `fn field_party_list(rec: &Record, label: &str) -> anyhow::Result<Vec<CantonId>>` (new decoder — a list of parties; mirror `field_contract_id_list` at 111 but decode each element as a party).
  - `fn parse_delegation_record(cid: &str, rec: &Record) -> anyhow::Result<ActiveDelegation>` (pure).
  - `pub(crate) async fn active_delegation(config: &NodeConfig, packages: &PackageConfig, test_mode: bool, decparty: &CantonId, token: &str) -> anyhow::Result<Option<ActiveDelegation>>` — reads the singleton `CouponReassignmentDelegation`; 0 ⇒ `Ok(None)`, 1 ⇒ `Ok(Some(..))`, >1 ⇒ `Err(..)` + `warn!`. Replaces `effective_split` (deleted in Task 7).

- [ ] **Step 1: Write the failing parse test:**

```rust
#[test]
fn parse_delegation_record_reads_assigners_and_split() {
    // List values: `value::Sum::List(List { elements: vec![Value, ..] })` — same as the
    // existing parse_split_record test. `party(..)` returns value::Sum, so wrap each with
    // `value(..)`; `beneficiary_record(..)` already returns a Value. `field(label, sum)`
    // takes a value::Sum, so pass `value::Sum::List(..)` directly.
    let rec = record(vec![
        field("decparty", party(GOV)),
        field("assigners", value::Sum::List(List {
            elements: vec![value(party(ALICE)), value(party(BOB))],
        })),
        field("split", value::Sum::List(List {
            elements: vec![beneficiary_record(ALICE, "0.8"), beneficiary_record(BOB, "0.2")],
        })),
    ]);
    let d = parse_delegation_record("00del", &rec).unwrap();
    assert_eq!(d.cid, "00del");
    assert_eq!(d.assigners.len(), 2);
    // split is not parsed (DAML-enforced) — the record's `split` field is ignored.
}
```
(`List` = `canton_proto_rs::com::daml::ledger::api::v2::List` — add it to the test `use`. `GOV`/`ALICE`/`BOB` are the existing test constants (~967); `record`/`field`/`party`/`value`/`beneficiary_record` are the existing builders (916–949).)

- [ ] **Step 2: Run; verify fail.** Run: `cargo test -p decman parse_delegation_record`. Expected: FAIL.

- [ ] **Step 3: Implement `field_party_list`, `parse_delegation_record`, `active_delegation`.**
```rust
fn field_party_list(rec: &Record, label: &str) -> anyhow::Result<Vec<CantonId>> {
    match record_field(rec, label) {
        Some(value::Sum::List(l)) => l.elements.iter()
            .map(|e| match &e.sum {
                Some(value::Sum::Party(p)) =>
                    CantonId::parse(p).map_err(|e| anyhow!("{label}: bad party: {e}")),
                _ => Err(anyhow!("{label}: expected party in list")),
            })
            .collect(),
        _ => Err(anyhow!("{label}: expected list")),
    }
}

fn parse_delegation_record(cid: &str, rec: &Record) -> anyhow::Result<ActiveDelegation> {
    Ok(ActiveDelegation {
        cid: cid.to_string(),
        assigners: field_party_list(rec, "assigners")?,
        // NB: the `split` field is intentionally NOT read — Delegation_Assign enforces it in DAML.
    })
}

pub(crate) async fn active_delegation(config: &NodeConfig, packages: &PackageConfig,
    test_mode: bool, decparty: &CantonId, token: &str) -> anyhow::Result<Option<ActiveDelegation>> {
    let Some(pkg) = packages.governance_rewards.as_deref() else { return Ok(None); };
    let recs = active_created_records(config, decparty, Some(token.to_string()), test_mode,
        pkg, "Governance.Rewards.CouponReassignmentDelegation", "CouponReassignmentDelegation",
        /*interface_view=*/ false).await?;
    // filter to this decparty (the `decparty` field == decparty)
    let mut mine: Vec<(String, Record)> = recs.into_iter()
        .filter(|(_, r)| field_party_id(r, "decparty").map(|p| &p == decparty).unwrap_or(false))
        .collect();
    match mine.len() {
        0 => Ok(None),
        1 => { let (cid, rec) = mine.remove(0); Ok(Some(parse_delegation_record(&cid, &rec)?)) }
        n => { tracing::warn!(%decparty, count = n, "ambiguous CouponReassignmentDelegation — refusing");
               Err(anyhow!("ambiguous CouponReassignmentDelegation: {n} active — refusing")) }
    }
}
```
(Confirm the proto `value::Sum::Party` variant + `CantonId::from` against `field_party_id` at 72, which already decodes a single party — reuse its exact decode expression for each list element.)

- [ ] **Step 4: Run; verify pass.** Run: `cargo test -p decman parse_delegation_record`. Expected: PASS. (Don't gate on clippy `-D warnings` yet — `active_delegation` is unused until Task 6 wires it into the loop; see Global Constraints.)

- [ ] **Step 5: Commit.**
```bash
git add crates/decman/src/server/reward_automation/mod.rs
git commit -m "feat(decman): active CouponReassignmentDelegation reader (enablement + assigners)"
```

---

### Task 5: Rust — the per-round reassign assigner (`Delegation_Assign` exercise)

**Files:** Modify `crates/decman/src/server/reward_automation/mod.rs` (+ tests); Modify `crates/decman/src/server/action_serializer.rs` (widen helper visibility).

**Interfaces:**
- Consumes: `unassigned_coupons` (366), `select_batch` (535), `ActiveDelegation` (Task 4); `make_party`/`make_contract_id`/`make_list`/`field` (widen to `pub(crate)`); `submit_proposal`/`execute_confirm_action` as the gRPC-command construction models (`governance.rs:1056`, `2130`).
- Produces:
  - `fn build_delegation_assign_arg(assigner: &CantonId, primary: &str, additional: &[String]) -> Record` (pure — the `Delegation_Assign` choice argument).
  - `pub(crate) async fn submit_delegation_assign(config: &NodeConfig, decparty: &CantonId, assigner: &CantonId, token: &str, delegation_cid: &str, primary: &str, additional: &[String], packages: &PackageConfig) -> anyhow::Result<()>` — builds + submits the exercise command.
  - `pub(crate) async fn run_reassign_once(config: &NodeConfig, decparty: &CantonId, assigner: &CantonId, token: &str, delegation: &ActiveDelegation, test_mode: bool, packages: &PackageConfig) -> anyhow::Result<()>`.

- [ ] **Step 1: Widen serializer-helper visibility.** In `action_serializer.rs`, change `make_party` (26), `make_contract_id` (56), `make_list` (88), `field` (62) from private `fn` to `pub(crate) fn` (they build the choice-arg record). Run `cargo build -p decman` to confirm no breakage.

- [ ] **Step 2: Write the failing arg-shape test** (`reward_automation` tests):
```rust
#[test]
fn build_delegation_assign_arg_shape() {
    // rb(..).beneficiary yields a CantonId (this module has no canton_id helper);
    // rb takes a bare prefix and appends a fixed namespace itself.
    let rec = build_delegation_assign_arg(&rb("m1", "1.0").beneficiary, "00c1", &["00c2".into()]);
    let labels: Vec<&str> = rec.fields.iter().map(|f| f.label.as_str()).collect();
    assert_eq!(labels, ["assigner", "primaryCoupon", "additionalCoupons"]);
}
```

- [ ] **Step 3: Run; verify fail.** Run: `cargo test -p decman build_delegation_assign_arg`. Expected: FAIL.

- [ ] **Step 4: Implement the arg builder + submit + once.**
```rust
use super::action_serializer::{field, make_contract_id, make_list, make_party};
// Also add to the module's top-level `use` (Identifier/Record/value are already imported;
// anyhow::Context + utils are already in scope):
//   use canton_proto_rs::com::daml::ledger::api::v2::{
//       Command, Commands, ExerciseCommand, command,
//       command_service_client::CommandServiceClient, SubmitAndWaitRequest};
//   use super::queries::resolve_contract_package_ref;   // + the `uuid` crate

fn build_delegation_assign_arg(assigner: &CantonId, primary: &str, additional: &[String]) -> Record {
    Record { record_id: None, fields: vec![
        field("assigner", make_party(assigner)),
        field("primaryCoupon", make_contract_id(primary)),
        field("additionalCoupons", make_list(additional.iter().map(|c| make_contract_id(c)).collect())),
    ]}
}

// Exercise Delegation_Assign as a plain ledger command. Adapted verbatim from
// execute_confirm_action (governance.rs:2200-2252); the only differences are the target
// contract (the delegation cid), the choice ("Delegation_Assign"), the template, and
// act_as = [assigner] / read_as = [decparty] (co-hosting, spec §4.6).
pub(crate) async fn submit_delegation_assign(config: &NodeConfig, decparty: &CantonId,
    assigner: &CantonId, token: &str, delegation_cid: &str, primary: &str,
    additional: &[String], packages: &PackageConfig) -> anyhow::Result<()> {
    let choice_argument = build_delegation_assign_arg(assigner, primary, additional);
    let fallback = packages.governance_rewards.as_deref()
        .context("governance_rewards package not configured")?;
    // The delegation may live under an older package ref — resolve its actual one
    // (same as execute_confirm_action:2200-2208).
    let package_id = resolve_contract_package_ref(
        config, decparty, Some(token.to_string()), delegation_cid, fallback).await;
    let template_id = Identifier {
        package_id,
        module_name: "Governance.Rewards".to_string(),
        entity_name: "CouponReassignmentDelegation".to_string(),
    };
    let channel = tonic::transport::Channel::from_shared(config.ledger_api_url())?
        .connect().await?;
    let mut client = CommandServiceClient::new(channel)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);
    let cmd = Command { command: Some(command::Command::Exercise(ExerciseCommand {
        template_id: Some(template_id),
        contract_id: delegation_cid.to_string(),
        choice: "Delegation_Assign".to_string(),
        choice_argument: Some(choice_argument),
    }))};
    // act_as = [assigner], read_as = [decparty]. Remaining fields empty/None — copy the
    // full field list from execute_confirm_action:2226-2241 (Commands has many fields).
    let commands = Commands {
        command_id: uuid::Uuid::new_v4().to_string(),
        commands: vec![cmd],
        act_as: vec![assigner.to_string()],
        read_as: vec![decparty.to_string()],
        workflow_id: String::new(),
        user_id: String::new(),
        deduplication_period: None,
        min_ledger_time_abs: None,
        min_ledger_time_rel: None,
        submission_id: String::new(),
        disclosed_contracts: vec![],
        synchronizer_id: String::new(),
        package_id_selection_preference: vec![],
        prefetch_contract_keys: vec![],
    };
    let mut req = tonic::Request::new(SubmitAndWaitRequest { commands: Some(commands) });
    req.metadata_mut().insert("authorization", format!("Bearer {token}").parse().unwrap());
    client.submit_and_wait(req).await?;   // an assign needs no created-cid readback
    Ok(())
}

pub(crate) async fn run_reassign_once(config: &NodeConfig, decparty: &CantonId,
    assigner: &CantonId, token: &str, delegation: &ActiveDelegation, test_mode: bool,
    packages: &PackageConfig) -> anyhow::Result<()> {
    const WATERMARK: chrono::Duration = chrono::Duration::hours(6);
    const MINTING_MARGIN: chrono::Duration = chrono::Duration::hours(2);
    const MAX_BATCH: usize = 50;
    let coupons = unassigned_coupons(config, decparty, Some(token.to_string()), test_mode, packages).await?;
    let batch = select_batch(&coupons, Utc::now(), WATERMARK, MINTING_MARGIN, MAX_BATCH);
    let Some((primary, additional)) = batch.split_first() else { return Ok(()); }; // nothing ripe -> no-op
    submit_delegation_assign(config, decparty, assigner, token, &delegation.cid,
        primary, additional, packages).await?;
    tracing::info!(%decparty, %assigner, count = batch.len(), "reassigned coupon batch");
    Ok(())
}
```
The command body above is adapted from `execute_confirm_action`; at build time confirm the `Commands` proto field set + the `submit_and_wait` signature against `governance.rs:2226–2252` (proto fields can shift by version). The `WATERMARK`/`MINTING_MARGIN`/`MAX_BATCH` consts are lifted verbatim from the deleted `run_proposer_once` (581–583).

- [ ] **Step 5: Run the arg test; verify pass.** Run: `cargo test -p decman build_delegation_assign_arg`. Expected: PASS. (The gRPC submit is exercised by the Task 9 devnet IT, not a unit test — do not mock the ledger.)

- [ ] **Step 6: Backend gate + commit.** Run: `cargo fmt --check && cargo build -p decman && cargo test -p decman`. (Do **not** run clippy `-D warnings` yet — `run_reassign_once`/`submit_delegation_assign` are unused until Task 6 wires them, and `active_delegation` is likewise unwired; `dead_code` under `-D warnings` would error. The strict clippy gate is Task 7 Step 5.) Then:
```bash
git add crates/decman/src/server/reward_automation/mod.rs crates/decman/src/server/action_serializer.rs
git commit -m "feat(decman): per-round Delegation_Assign assigner (plain 1-of-n exercise)"
```

---

### Task 6: Rust — rewrite the loop body (`run_once_for_party`) to the delegation model

**Files:** Modify `crates/decman/src/server/reward_automation/mod.rs`.

**Interfaces:**
- Consumes: `get_party_credentials`, `packages`, `active_delegation` (Task 4), `run_reassign_once` (Task 5).
- Produces: rewritten `run_once_for_party`. `run_reward_automation_loop` (802) is **unchanged** (its body already just snapshots `party_credentials` and calls `run_once_for_party` per decparty — recon confirms no engine coupling).

- [ ] **Step 1: Rewrite `run_once_for_party` (826–908).** Replace the whole body with:
```rust
async fn run_once_for_party(data: &web::Data<AppState>, decparty: &CantonId) -> anyhow::Result<()> {
    let pkgs = packages();
    let Some((token, member)) = get_party_credentials(data, decparty).await else { return Ok(()); };
    // Enablement: exactly one active delegation. None => off (no-op). >1 => Err (refuse+alert).
    let Some(delegation) = active_delegation(&data.config, &pkgs, data.test_mode, decparty, &token).await?
        else { return Ok(()); };
    // This node must be a listed assigner, else it cannot reassign (spec §9, §11).
    if !delegation.assigners.contains(&member) {
        tracing::debug!(%decparty, %member, "node not an assigner on the delegation — skipping");
        return Ok(());
    }
    run_reassign_once(&data.config, decparty, &member, &token, &delegation, data.test_mode, &pkgs).await
}
```
This drops every engine call: `resolve_active_governance_rules`, `get_governance_confirmations`, `read_all_pending_assigns`, the `covered` set, `run_proposer_once`, `run_confirmer_once`. (Those fns still exist after this task but are now unused — deleted in Task 7.)

- [ ] **Step 2: Compile + test (defer the strict clippy gate).** Run: `cargo build -p decman && cargo test -p decman`. Expected: compiles, tests green. **Do NOT run `clippy -- -D warnings` here:** the engine fns are now unreachable, and `pub(crate)` unused items trip the `dead_code` lint, which `-D warnings` promotes to errors. That is expected mid-migration — the strict clippy gate is restored in Task 7 Step 5, once the dead engine is deleted.

- [ ] **Step 3: Commit.**
```bash
git add crates/decman/src/server/reward_automation/mod.rs
git commit -m "refactor(decman): rewrite reward-automation loop to the delegation model"
```

---

### Task 7: Rust — delete the auto-confirm engine

**Files:** Modify `crates/decman/src/server/reward_automation/mod.rs`.

**Interfaces:** Produces nothing; removes dead code. After this task the module contains only: field decoders still in use, `active_created_records`, `parse_beneficiary_list`, `CouponInfo`, `unassigned_coupons`, `COUPON_TTL`, `select_batch`, `ActiveDelegation`/`active_delegation`, `build_delegation_assign_arg`/`submit_delegation_assign`/`run_reassign_once`, `run_once_for_party`, `run_reward_automation_loop`, and their tests.

- [ ] **Step 1: Delete the engine fns/structs.** Remove: `PendingAssign` (413–425), `split_matches` (432–444), `is_confirmable` (451–460), `parse_assign_record` (471–479), `read_all_pending_assigns` (485–515), `submit_confirmation` (668–686), `already_confirmed_by` (690–695), `run_proposer_once` (563–651), `run_confirmer_once` (704–793). Also delete `effective_split` (301–342), `parse_split_record` (289–291), and `parse_beneficiary_list` (266–284) — all replaced by `active_delegation`, which reads only the cid + `assigners` (the split is not read; the DAML enforces it). If a final grep shows `parse_beneficiary_list` still referenced somewhere, keep it; otherwise delete.

- [ ] **Step 2: Delete now-dead field decoders.** After the engine is gone, check usage: `field_contract_id` (103) and `field_contract_id_list` (111) were consumed only by `parse_assign_record`. If nothing else references them (grep the module), delete them; otherwise keep. Keep `field_party_id`, `field_decimal`, `field_time`, `field_optional_is_none` (used by `unassigned_coupons`/`active_delegation`).

- [ ] **Step 3: Prune imports.** Remove now-unused `use` items (26–54): `HashMap`, `execute_confirm_action`, `get_governance_confirmations`, `resolve_active_governance_rules` (unless still referenced), `ActionType`, `ConfirmActionRequest`, `DomainGovernanceAction`, `GovernanceType`, `submit_proposal`. Let `cargo build` + clippy drive exactly which to drop.

- [ ] **Step 4: Delete engine tests.** In `#[cfg(test)] mod tests`: remove `split_matches_is_order_insensitive_and_exact` (1001–1017), `is_confirmable_is_default_deny` (1019–1056), `parse_assign_record_reads_coupons_and_split` (1058–1087), `already_confirmed_by_detects_this_member` (1158–1180), and — since `parse_split_record` is deleted — `parse_split_record_reads_beneficiaries` (972–993) + `parse_split_record_rejects_missing_list` (995–999). Remove helpers left unused (`gov_conf` at 1148; `beneficiary_record` only if the new `parse_delegation_record` test doesn't use it — it does, so keep it). Keep `rb`, `party`, `value`, `field`, `record`, the `GOV`/`ALICE`/`BOB` consts, `List`, and `select_batch_*`; let clippy `-D warnings` confirm nothing dead remains.

- [ ] **Step 5: Full gate.** Run: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p decman`. Expected: **zero** dead-code warnings, all tests pass.

- [ ] **Step 6: Commit.**
```bash
git add crates/decman/src/server/reward_automation/mod.rs
git commit -m "refactor(decman): delete auto-confirm engine (proposer/confirmer/split-match)"
```

---

### Task 8: Delete the dead DAML + old `ProposalType`s + version bump

**Files:**
- Delete: `daml/governance-rewards/daml/Governance/Rewards/{AssignRewardBeneficiaries,RewardSplitConfig,SetRewardSplit}.daml`
- Delete: `daml/governance-rewards-assign-test/daml/Governance/Rewards/{TestAssignRewardBeneficiaries,TestSetRewardSplit}.daml`
- Modify: `crates/decman/src/server/types.rs`, `action_serializer.rs` (remove old variants/arms/tests)
- Modify: `daml/governance-rewards/daml.yaml` (version bump); `daml/governance-rewards-assign-test/daml.yaml` (dar ref); `releases/v1/`

**Interfaces:** Removes `ProposalType::AssignRewardBeneficiaries` (`types.rs:647`) + `ProposalType::SetRewardSplit` (655) + their validate arms (809–814), serializer arms (`action_serializer.rs:1202`, 1237), and unit tests (`types.rs:1521,1560`; `action_serializer.rs:2551,2595`).

- [ ] **Step 1: Delete the old Rust variants first** (before the DAML, so no serializer arm references a missing module). Remove the two variants, their `validate()` arms, both serializer arms, and their tests. Run `cargo run --features typegen --bin gen-types` (removes them from the TS union — the `default:` case already covers nothing-to-do). Gate: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p decman`.

- [ ] **Step 2: Delete the DAML files** (the assign body + `emptyExtraArgs` already live in `CouponReassignmentDelegation.daml` from Task 1; confirm nothing else imports the deleted modules — grep `daml/`):
```bash
git rm daml/governance-rewards/daml/Governance/Rewards/AssignRewardBeneficiaries.daml \
       daml/governance-rewards/daml/Governance/Rewards/RewardSplitConfig.daml \
       daml/governance-rewards/daml/Governance/Rewards/SetRewardSplit.daml \
       daml/governance-rewards-assign-test/daml/Governance/Rewards/TestAssignRewardBeneficiaries.daml \
       daml/governance-rewards-assign-test/daml/Governance/Rewards/TestSetRewardSplit.daml
```

- [ ] **Step 3: (Version already `0.1.3` from Task 1 — no bump.)** Confirm `daml/governance-rewards/daml.yaml:3` is `version: 0.1.3` and the test package's `data-dependency` already points at `-0.1.3.dar`. Nothing to change here; the removals below just alter package contents at the same version.

- [ ] **Step 4: Rebuild + ship the DAR.** Run: `(cd daml && dpm build --all) && (cd daml/governance-rewards-assign-test && dpm test)`. Copy the freshly built `governance-rewards-v1-0.1.3.dar` into `releases/v1/`. (Confirm the build output path with `dpm build` logs; keep 0.1.0–0.1.2 DARs in place for provenance.)

- [ ] **Step 5: Full repo gate.** Run: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p decman && (cd daml && dpm build --all) && (cd daml/governance-rewards-assign-test && dpm test)`. Expected: all green; no reference to the deleted templates/variants anywhere (grep `AssignRewardBeneficiaries`, `RewardSplitConfig`, `SetRewardSplit` across `crates/` + `daml/` returns only the two superseded plan docs).

- [ ] **Step 6: Commit.**
```bash
git add -A daml/ crates/decman/src/server/types.rs crates/decman/src/server/action_serializer.rs releases/v1/
git commit -m "refactor: remove AssignRewardBeneficiaries/RewardSplitConfig/SetRewardSplit; gov-rewards 0.1.3"
```

---

### Task 9 (M4): Devnet integration test — delegation model

**Files:** **create** the delegation-model IT phase. (The old `reward_assignment.rs` phase was already removed in Task 8 — it was engine-era dead code naming the deleted proposal types — so this is a fresh create, not a rename.) No new runtime code; the shared IT harness (`common/types.rs`, `common/governance.rs`, `propose_confirm_execute`, `Scenario`, etc.) is intact and reused.
- Create: `crates/decman/tests/common/phases/coupon_reassignment.rs` — the delegation-model scenario (below).
- Modify: `crates/decman/tests/common/phases/mod.rs` — add `pub mod coupon_reassignment;` (alphabetical position).
- Modify: `crates/decman/tests/governance_workflows.rs` — add the `DECPM_IT_REWARD`-gated call into the new phase (single-node delegation scenario).

**Preconditions (operational — spec §13, handover):**
- **Pause the Mode-B collection path for `cbtc-network`** (it sweeps coupons to 0) so unassigned coupons re-accumulate — coordinate with the team. Coupons re-appear within ~one round.
- Create the delegation via ONE governance vote: propose→confirm→execute `SetupCouponReassignmentDelegation` for `cbtc-network` with `assigners = [attestor-1, attestor-2]` (the active members) and `beneficiaries = [cbtc-beneficiary 0.8, operator 0.2]`. Verify exactly one `CouponReassignmentDelegation` exists.
- **Only ONE DecMan member instance needs to run** to reassign (contrast the old multi-node confirm) — the IT no longer requires ≥2 instances for the happy path.

- [ ] **Step 0: Create + register the phase module.** Create `crates/decman/tests/common/phases/coupon_reassignment.rs`; add `pub mod coupon_reassignment;` to `crates/decman/tests/common/phases/mod.rs` (alphabetical); add a `DECPM_IT_REWARD`-gated call into it from `crates/decman/tests/governance_workflows.rs`. Model the harness usage (starting nodes, PQS `pqs_cbtc` queries, `propose_confirm_execute`, `Scenario`) on the surviving sibling phases (`utility_onboarding.rs`, `notification_feed.rs`). This phase is **devnet-only, gated behind `DECPM_IT_REWARD`** — it does not run in normal CI; verify it **compiles + is gated** with `DECPM_IT_REWARD=1 cargo test -p decman --no-run` (a live devnet run is a separate pre-merge ops step, not part of this task).

- [ ] **Step 1: Write the happy-path scenario.** With the delegation in place and collection paused: start the automation on one member node; wait up to N ticks. Assert against devnet PQS `pqs_cbtc`, in order: (a) the unassigned `RewardCouponV2` coupons (`provider = cbtc-network`, `beneficiary = null`) exist; (b) after a tick, those coupons are archived and one `RewardCouponV2` per beneficiary now exists with `beneficiary ∈ {cbtc-beneficiary, operator}` and the expected `amount` shares (0.8 / 0.2). (Beneficiary self-minting is a separate precondition — assert only if those agents run; spec §4.3.)

- [ ] **Step 2: Assert the security property.** The split is baked into the delegation and `Delegation_Assign` reads it, so a caller cannot alter it. Cover this at the DAML level (Task 1's `test_non_assigner_cannot_reassign` + the baked-split assertion) and, on devnet, assert that a **non-assigner** party's attempt to exercise `Delegation_Assign` is rejected by the ledger (submit as a party not in `assigners`; expect failure). There is no "craft a mismatched proposal" case anymore — there are no proposals.

- [ ] **Step 3: Record results in the PR.** Note whether beneficiary self-minting was observed or whether the test asserts reassignment only.

- [ ] **Step 4: Restore devnet + commit.** Un-pause the Mode-B collection path for `cbtc-network` (or per team decision).
```bash
git add crates/decman/tests/common/phases/coupon_reassignment.rs \
        crates/decman/tests/common/phases/mod.rs \
        crates/decman/tests/governance_workflows.rs
git commit -m "test(integration): delegation-model coupon reassignment on devnet"
```

---

## What this plan intentionally does NOT cover

- Mode B / the `MintingDelegation` collection path (a separate workstream; one-shot, #256).
- A shared reward-config template — the split is baked into the delegation and DAML-enforced; if a shared config template is introduced, the change is in `SetupCouponReassignmentDelegation` / the delegation template (Tasks 1–2), not the Rust reader (which never reads the split).
- Deterministic leader election / grace-window (spec §10 — a follow-up; any-assigner-reassigns is already safe and 1-of-n live).
- Beneficiary self-mint automation (spec §4.3 — the beneficiaries' own agents).
- Delegating any action other than coupon reassignment (spec §3).

## Self-review notes

- **Spec coverage:** §5 delegation (one vote vs per-round) → Tasks 2+3 (create path) + 5+6 (per-round exercise); §7 DAML template + `Delegation_Assign` 1-of-n + `Delegation_Revoke` → Task 1; §8 split baked in, replace = revoke+create → Tasks 1+2 (`priorDelegation`), Task 4 (single-delegation invariant); §9 per-tick loop (read delegation → scan → select_batch → exercise, assigner gate) → Tasks 4+5+6; §10 all-or-nothing/duplicates safe → Task 5 (batch) + Task 1 DAML (already-assigned rejected); §11 edge cases (0/>1 delegation, node-not-assigner) → Tasks 4+6; §12 trust (split in L3, 1-of-n, non-custodial, choice = security boundary) → Task 1 (+ review) + Task 9 negative; §13 testing → Tasks 1–5 units + Task 9 IT; §15 milestones M1→Task1, M2→Tasks2+3, M3→Tasks4–8, M4→Task9.
- **Green at every task:** additions (Tasks 1–6) leave the old engine + old DAML compiling; removals (Tasks 7–8) happen only after the loop stops calling the engine (Task 6) and the assign body/`emptyExtraArgs` are relocated (Task 1). No task leaves a dangling reference.
- **Type consistency:** `RewardBeneficiary { beneficiary: CantonId, percentage: DamlDecimal }` is identical across the DAML `split`/`beneficiaries` fields, `ProposalType::SetupCouponReassignmentDelegation.new_beneficiaries`, and the serializer arm. Field orders: DAML `SetupCouponReassignmentDelegation` = `governanceParty, proposer, priorDelegation, assigners, beneficiaries` = serializer `owned_labels` (Task 3 Step 5); `Delegation_Assign` = `assigner, primaryCoupon, additionalCoupons` = `build_delegation_assign_arg` (Task 5). The `CouponReassignmentDelegation` template is `decparty, assigners, split`; the reader parses only `assigners` (and filters on `decparty`) — it never reads `split` (DAML-enforced).
- **Exact `DamlDecimal`:** reused `validate_reward_beneficiaries` (Rust) + `total == 1.0` (DAML `SetupCouponReassignmentDelegation.executeImpl`) — no f64, no tolerance, consistent with the spec.
- **Reuse verified against recon (2026-07-20):** readers/`select_batch`/plumbing share no types with the engine (`PendingAssign` is engine-only; `select_batch` takes `&[CouponInfo]` + time/size params and returns `Vec<String>` cids, never `PendingAssign`), so Task 7's deletion is clean — the sole coupling was `run_once_for_party`, rewritten in Task 6.
- **First-build verifications (concrete "confirm X against the code" steps, not placeholders):** the token-metadata `ExtraArgs` imports + `emptyExtraArgs` (copy from `AssignRewardBeneficiaries.daml:20`, Task 1); the proto `value::Sum::Party` list decode (mirror `field_party_id:72`, Task 4 Step 3); the `Delegation_Assign` `ExerciseCommand` construction is provided (Task 5 Step 4, adapted from `execute_confirm_action:2200–2252`) — verify the `Commands` proto field set at build; the `dpm build` DAR output path for shipping 0.1.3 (Task 8 Step 4).
- **Known nit (not fixed here):** the DAML test package keeps its `governance-rewards-assign-test` name though it no longer tests an "assign" action — renaming a DAML package is churny and out of scope; flag for a later cosmetic pass.
