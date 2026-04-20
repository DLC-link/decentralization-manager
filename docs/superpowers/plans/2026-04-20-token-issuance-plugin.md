# Token Issuance Plugin Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `governance-token-issuance` plugin per [docs/TOKEN_ISSUANCE_PLUGIN.md](../../TOKEN_ISSUANCE_PLUGIN.md) — a Daml plugin package that plugs into `governance-core`, letting a decentralized governance party mint and burn its own token instrument. Mint/burn go through Utility-Registry two-step workflows (`AllocationFactory_OfferMint` / `AllocationFactory_OfferBurn`).

**Architecture:** Two new Daml packages inside `daml/`:

- **`governance-token-issuance`** — the plugin itself. Contains `IssuanceConfig` (state contract with a `IssuanceConfig_RotateFactory` choice) plus four plugin templates implementing the `GovernableAction` interface: `SetupIssuanceProposal`, `MintProposal`, `BurnProposal`, `RotateFactoryProposal`.
- **`governance-token-issuance-test`** — the test package. Follows the established `given_* / when_* / then_*` harness pattern used by `governance-token-custody-test`.

Both packages are registered in `daml/multi-package.yaml`. The plugin data-depends on the existing `governance-core-v0-rc2` DAR plus the Splice token APIs (`burn-mint-v1`, `holding-v1`, `metadata-v1`, `transfer-instruction-v1`), `splice-util`, and `utility-registry-app-v0-0.7.0`. The test package adds `daml-script`, `testlib-0.1.0`, and whichever utility-registry helpers we need.

**Tech Stack:** Daml 3.4.11, LF 2.2, Splice Token APIs, `utility-registry-app-v0-0.7.0`, `testlib-0.1.0`, `governance-core-v0-rc2`.

## Task 1: Scaffold the `governance-token-issuance` package

**Files:**
- Create: `daml/governance-token-issuance/daml.yaml`
- Create: `daml/governance-token-issuance/daml/Governance/TokenIssuance/.gitkeep`
- Modify: `daml/multi-package.yaml`

- [ ] **Step 1: Create the package `daml.yaml`.**

Write `daml/governance-token-issuance/daml.yaml`:

```yaml
sdk-version: 3.4.11
name: governance-token-issuance-v0-rc2
version: 0.1.0
source: daml
dependencies:
  - daml-prim
  - daml-stdlib
data-dependencies:
  - ../governance-core/.daml/dist/governance-core-v0-rc2-0.1.0.dar
  - ../dars/splice-api-token-burn-mint-v1-1.0.0.dar
  - ../dars/splice-api-token-holding-v1-1.0.0.dar
  - ../dars/splice-api-token-metadata-v1-1.0.0.dar
  - ../dars/splice-api-token-transfer-instruction-v1-1.0.0.dar
  - ../dars/splice-util-0.1.4.dar
  - ../dars/utility-registry-app-v0-0.7.0.dar
  - ../dars/utility-registry-v0-0.6.0.dar
build-options:
  - --target=2.2
  - -Wupgrade-interfaces
  - --ghc-option=-Wunused-binds
  - --ghc-option=-Wunused-matches
```

- [ ] **Step 2: Create the empty source directory.**

Run:
```sh
mkdir -p daml/governance-token-issuance/daml/Governance/TokenIssuance
touch daml/governance-token-issuance/daml/Governance/TokenIssuance/.gitkeep
```

- [ ] **Step 3: Register the package in `daml/multi-package.yaml`.**

Edit `daml/multi-package.yaml` and add both the plugin and its upcoming test package:

```yaml
packages:
  - governance-core
  - governance-core-test
  - governance-token-custody
  - governance-token-custody-test
  - governance-token-issuance
  - governance-token-issuance-test
```

- [ ] **Step 4: Verify the package builds (empty, but correctly configured).**

Run:
```sh
cd daml/governance-token-issuance && daml build
```

Expected: build succeeds, producing `.daml/dist/governance-token-issuance-v0-rc2-0.1.0.dar`. An empty source directory is a valid Daml package.

- [ ] **Step 5: Commit.**

```sh
git add daml/governance-token-issuance daml/multi-package.yaml
git commit -m "feat(token-issuance): scaffolded governance-token-issuance package"
```

---

## Task 2: Scaffold `governance-token-issuance-test` with TestUtils

**Files:**
- Create: `daml/governance-token-issuance-test/daml.yaml`
- Create: `daml/governance-token-issuance-test/daml/Governance/TokenIssuance/TestUtils.daml`

- [ ] **Step 1: Create the test package `daml.yaml`.**

Write `daml/governance-token-issuance-test/daml.yaml`:

```yaml
sdk-version: 3.4.11
name: governance-token-issuance-test
version: 0.1.0
source: daml
dependencies:
  - daml-prim
  - daml-stdlib
  - daml-script
data-dependencies:
  - ../governance-token-issuance/.daml/dist/governance-token-issuance-v0-rc2-0.1.0.dar
  - ../governance-core/.daml/dist/governance-core-v0-rc2-0.1.0.dar
  - ../dars/testlib-0.1.0.dar
  - ../dars/splice-api-token-burn-mint-v1-1.0.0.dar
  - ../dars/splice-api-token-holding-v1-1.0.0.dar
  - ../dars/splice-api-token-metadata-v1-1.0.0.dar
  - ../dars/splice-api-token-transfer-instruction-v1-1.0.0.dar
  - ../dars/splice-util-0.1.4.dar
  - ../dars/utility-credential-v0-0.1.0.dar
  - ../dars/utility-registry-app-v0-0.7.0.dar
  - ../dars/utility-registry-holding-v0-0.2.1.dar
  - ../dars/utility-registry-v0-0.6.0.dar
build-options:
  - --target=2.2
  - -Wno-upgrade-interfaces
  - -Wno-template-interface-depends-on-daml-script
  - --ghc-option=-Wunused-binds
  - --ghc-option=-Wunused-matches
```

- [ ] **Step 2: Create TestUtils with party allocation and governance-rules helpers.**

Write `daml/governance-token-issuance-test/daml/Governance/TokenIssuance/TestUtils.daml` (copied and trimmed from the custody-test version; drops the separate `registrar` party because our plugin makes `governanceParty` the registrar):

```daml
-- Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- | Test utilities for the token issuance governance plugin.
module Governance.TokenIssuance.TestUtils where

import DA.Set as Set hiding (filter)
import DA.Time (minutes)
import Daml.Script

import Governance.Action (GovernableAction)
import Governance.Confirmation (GovernanceConfirmation)
import Governance.ExecutionResult
import Governance.Rules

-- | Standard test party allocation for token issuance tests.
-- NB: no separate `registrar` field — the governance party itself is the registrar.
data IssuanceTestParties = IssuanceTestParties
  with
    governanceParty : Party
    member1 : Party
    member2 : Party
    member3 : Party
    outsider : Party
    operator : Party
    dso : Party
  deriving (Show, Eq)

-- | Allocate standard test parties.
allocateIssuanceTestParties : Script IssuanceTestParties
allocateIssuanceTestParties = do
  governanceParty <- allocateParty "GovernanceParty"
  member1 <- allocateParty "Member1"
  member2 <- allocateParty "Member2"
  member3 <- allocateParty "Member3"
  outsider <- allocateParty "Outsider"
  operator <- allocateParty "Operator"
  dso <- allocateParty "DSO"
  pure IssuanceTestParties with ..

-- | Standard member set.
getMembers : IssuanceTestParties -> Set Party
getMembers parties = Set.fromList [parties.member1, parties.member2, parties.member3]

-- | Build submission context for a member with readAs on governance party.
memberContext : Party -> Party -> SubmitOptions
memberContext member gp = actAs member <> readAs gp

-- | Create governance rules with standard test configuration (threshold 2 of 3).
createTestGovernance : IssuanceTestParties -> Script (ContractId GovernanceRules)
createTestGovernance parties =
  submit parties.governanceParty $ createCmd GovernanceRules with
    governanceParty = parties.governanceParty
    members = getMembers parties
    threshold = 2
    actionConfirmationTimeout = minutes 30

-- | Collect confirmations from a list of members for an interface action.
submitConfirmations : Party -> ContractId GovernanceRules -> ContractId GovernableAction -> [Party] -> Script [ContractId GovernanceConfirmation]
submitConfirmations governanceParty rulesCid proposalCid confirmers =
  mapA (\confirmer ->
    submit (memberContext confirmer governanceParty) $
      exerciseCmd rulesCid GovernanceRules_ConfirmAction with
        confirmer
        actionProposalCid = proposalCid
  ) confirmers

-- | Confirm and execute an interface action in one go.
confirmAndExecute : IssuanceTestParties -> ContractId GovernanceRules -> ContractId GovernableAction -> [Party] -> Party -> Script (ContractId GovernanceExecutionResult)
confirmAndExecute parties rulesCid proposalCid confirmers executor = do
  confirmationCids <- submitConfirmations parties.governanceParty rulesCid proposalCid confirmers
  submit (actAs executor
    <> readAs parties.governanceParty
    <> readAs parties.outsider
    <> readAs parties.operator
    <> readAs parties.dso) $
    exerciseCmd rulesCid GovernanceRules_ExecuteConfirmedAction with
      executor
      actionProposalCid = proposalCid
      confirmations = confirmationCids
```

- [ ] **Step 3: Verify the test package compiles.**

Run:
```sh
cd daml/governance-token-issuance-test && daml build
```

Expected: build succeeds. The package has no tests yet — just TestUtils compiling against the empty plugin package.

- [ ] **Step 4: Commit.**

```sh
git add daml/governance-token-issuance-test
git commit -m "feat(token-issuance): scaffolded test package with TestUtils"
```

---

## Task 3: Define `IssuanceConfig` state contract (no choices yet)

**Files:**
- Create: `daml/governance-token-issuance/daml/Governance/TokenIssuance/IssuanceConfig.daml`

> **No dedicated test in this task.** `IssuanceConfig.allocationFactoryCid : ContractId AllocationFactory` is a strict field, and a real `AllocationFactory` can only be produced by running the full Utility-Registry onboarding — which is what `SetupIssuanceProposal` does in Task 5. Unit-testing `IssuanceConfig` creation in isolation would require fabricating a `ContractId AllocationFactory`, for which Daml has no clean primitive. Coverage of `IssuanceConfig` creation lands in Task 5, when the end-to-end setup test asserts that the expected `IssuanceConfig` fields are present after the governance flow executes.

- [ ] **Step 1: Write the `IssuanceConfig` template.**

Write `daml/governance-token-issuance/daml/Governance/TokenIssuance/IssuanceConfig.daml`:

```daml
-- Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- | State contract for the token-issuance plugin.
-- Holds the AllocationFactory cid and instrument metadata.
-- See docs/TOKEN_ISSUANCE_PLUGIN.md for the design.
module Governance.TokenIssuance.IssuanceConfig where

import Splice.Api.Token.BurnMintV1 (BurnMintFactory)
import Splice.Api.Token.HoldingV1 (InstrumentId)

import Utility.Registry.App.V0.Service.AllocationFactory (AllocationFactory)

-- | One-per-plugin-deployment state contract.
-- Created by SetupIssuanceProposal. Updated in place by IssuanceConfig_RotateFactory.
template IssuanceConfig
  with
    governanceParty : Party
      -- ^ Signatory; the committee's on-chain identity.
    instrumentId : InstrumentId
      -- ^ The token instrument this plugin deployment issues.
    allocationFactoryCid : ContractId AllocationFactory
      -- ^ The AllocationFactory used for mint/burn. Cast to BurnMintFactory /
      -- TransferFactory interfaces as needed inside executeImpl bodies.
    displayName : Text
    symbol : Text
    decimals : Int
  where
    signatory governanceParty
```

- [ ] **Step 2: Verify the plugin package builds.**

Run:
```sh
cd daml/governance-token-issuance && daml build
```

Expected: build succeeds; the `.dar` produced by Task 1 is regenerated with `IssuanceConfig` inside.

- [ ] **Step 3: Commit.**

```sh
git add daml/governance-token-issuance/daml/Governance/TokenIssuance/IssuanceConfig.daml
git commit -m "feat(token-issuance): added IssuanceConfig state template"
```

---

## Task 4: Add `IssuanceConfig_RotateFactory` choice

**Files:**
- Modify: `daml/governance-token-issuance/daml/Governance/TokenIssuance/IssuanceConfig.daml`

> **No dedicated test in this task.** Same reason as Task 3: exercising the rotate choice requires real `AllocationFactory` cids on both ends. End-to-end coverage lands in Task 8, where `RotateFactoryProposal`'s test rotates a real config produced by `SetupIssuanceProposal` to a spare factory produced by `initSpareFactory`.

- [ ] **Step 1: Add the rotate choice to `IssuanceConfig`.**

Replace the `IssuanceConfig` template body in `daml/governance-token-issuance/daml/Governance/TokenIssuance/IssuanceConfig.daml` with:

```daml
template IssuanceConfig
  with
    governanceParty : Party
    instrumentId : InstrumentId
    allocationFactoryCid : ContractId AllocationFactory
    displayName : Text
    symbol : Text
    decimals : Int
  where
    signatory governanceParty

    -- | Replace the AllocationFactory cid, preserving all other fields.
    -- Controller is the governance party, exercised from
    -- RotateFactoryProposal.executeImpl via the governance exercise chain.
    -- NB: we do NOT validate the new factory's admin here; the calling
    -- proposal is responsible for that via BurnMintFactory_PublicFetch.
    -- Keeping the choice body minimal makes it easy to audit.
    choice IssuanceConfig_RotateFactory : ContractId IssuanceConfig
      with
        newFactoryCid : ContractId AllocationFactory
      controller governanceParty
      do
        create this with
          allocationFactoryCid = newFactoryCid
```

> **Why validation lives in the proposal, not the choice.** `BurnMintFactory_PublicFetch` is a non-consuming choice on `BurnMintFactory`; it has side-effect-free semantics but still requires authority. Putting the validation in `RotateFactoryProposal.executeImpl` (Task 8) means the proposal body orchestrates fetch-then-exercise-choice explicitly, which is easier to test in isolation.

- [ ] **Step 2: Verify the plugin package builds.**

Run:
```sh
cd daml/governance-token-issuance && daml build
```

Expected: build succeeds; `IssuanceConfig` now exposes the `IssuanceConfig_RotateFactory` choice.

- [ ] **Step 3: Commit.**

```sh
git add daml/governance-token-issuance/daml/Governance/TokenIssuance/IssuanceConfig.daml
git commit -m "feat(token-issuance): added IssuanceConfig_RotateFactory choice"
```

---

## Task 5: Implement `SetupIssuanceProposal`

**Files:**
- Create: `daml/governance-token-issuance/daml/Governance/TokenIssuance/SetupIssuance.daml`
- Modify: `daml/governance-token-issuance-test/daml/Governance/TokenIssuance/TestUtils.daml` (add `initUtilityPrereqs` helper)
- Create: `daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Test/SetupIssuanceTest.daml`

- [ ] **Step 1: Add `initUtilityPrereqs` helper to TestUtils.**

`SetupIssuanceProposal` consumes an existing `ProviderService` contract for the governance party. Tests need to provision it before running the proposal. Append to `daml/governance-token-issuance-test/daml/Governance/TokenIssuance/TestUtils.daml`:

```daml
import Utility.Registry.App.V0.Service.Provider
  (ProviderService(..))

-- | Provision a ProviderService for the governance party.
-- Uses multi-party submit to stand in for the full Utility-Registry onboarding.
-- A production path would reach this via a `UtilityCreateProviderRequest` flow
-- added to the existing governance-token-custody plugin, or equivalent.
initUtilityPrereqs : IssuanceTestParties -> Script (ContractId ProviderService)
initUtilityPrereqs parties =
  submitMulti [parties.governanceParty, parties.operator] [] $
    createCmd ProviderService with
      operator = parties.operator
      provider = parties.governanceParty
```

> **If the `ProviderService` field shape in `utility-registry-app-v0-0.7.0` differs from the above (extra fields, different signatories), adjust. Confirm by reading `Utility/Registry/App/V0/Service/Provider.daml` from the locally extracted DAR bundle.**
>
> **Why no `UserService`.** V1 uses `registrarRequirements = []`, so `RegistrarServiceRequest_Accept` takes `credentialCids = []` and no credential / `UserService` path is needed. `UserService` itself lives in `utility-credential-app`, not `utility-registry-app`. If a later version tightens registrar requirements, follow canton-vault's `VaultGovernanceRules_SetupUtility` (`VaultGovernance.daml:469-514`): add `utility-credential-app-v0-*.dar` as a data-dependency, import `Utility.Credential.App.V0.Service.User (UserService)`, take a `userServiceCid` on the proposal, and wire the credential-offer / credential-accept choices before `AcceptRegistrarServiceRequest`.

- [ ] **Step 2: Write the failing test for `SetupIssuanceProposal`.**

Write `daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Test/SetupIssuanceTest.daml`:

```daml
-- Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

module Governance.TokenIssuance.Test.SetupIssuanceTest where

import Daml.Script

import Splice.Api.Token.HoldingV1 (InstrumentId(..))

import Governance.Action (GovernableAction)
import Governance.Rules (GovernanceRules)

import Governance.TokenIssuance.IssuanceConfig (IssuanceConfig)
import Governance.TokenIssuance.SetupIssuance
import Governance.TokenIssuance.TestUtils

import TestHarness

data Fixture = Fixture
  with
    parties : IssuanceTestParties
    rulesCid : ContractId GovernanceRules
  deriving (Eq, Show)

given_governance : Script Fixture
given_governance = do
  parties <- allocateIssuanceTestParties
  rulesCid <- createTestGovernance parties
  pure Fixture with ..

when_setup_executed : Fixture -> Script ()
when_setup_executed f = do
  providerCid <- initUtilityPrereqs f.parties
  proposalCid <- submit f.parties.member1 $ createCmd SetupIssuanceProposal with
    governanceParty = f.parties.governanceParty
    proposer = f.parties.member1
    providerServiceCid = providerCid
    operator = f.parties.operator
    instrumentIdText = "TEST-TOKEN"
    displayName = "Test Token"
    symbol = "TEST"
    decimals = 8
  let proposalInterfaceCid : ContractId GovernableAction = toInterfaceContractId proposalCid
  _ <- confirmAndExecute f.parties f.rulesCid proposalInterfaceCid
    [f.parties.member1, f.parties.member2] f.parties.member1
  pure ()

then_config_exists : Fixture -> () -> Script Failures
then_config_exists f _ = do
  configs <- query @IssuanceConfig f.parties.governanceParty
  case configs of
    [(_, config)] -> pure $
      shouldBe "governanceParty" f.parties.governanceParty config.governanceParty <>
      shouldBe "instrumentId.admin" f.parties.governanceParty config.instrumentId.admin <>
      shouldBe "instrumentId.id" "TEST-TOKEN" config.instrumentId.id <>
      shouldBe "displayName" "Test Token" config.displayName <>
      shouldBe "symbol" "TEST" config.symbol <>
      shouldBe "decimals" 8 config.decimals
    _ -> pure $ shouldBe "IssuanceConfig count" 1 (length configs)

test_when_setup_proposed_then_config_created = script do
  run Test with
    given = given_governance
    when = when_setup_executed
    then_ = then_config_exists
```

- [ ] **Step 3: Run the test to verify it fails.**

Run:
```sh
cd daml/governance-token-issuance-test && daml test --files daml/Governance/TokenIssuance/Test/SetupIssuanceTest.daml
```

Expected: FAIL — `SetupIssuanceProposal` not defined.

- [ ] **Step 4: Implement `SetupIssuanceProposal`.**

Write `daml/governance-token-issuance/daml/Governance/TokenIssuance/SetupIssuance.daml`:

```daml
-- Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- | Plugin template for the initial governance-driven onboarding of the plugin.
-- Runs the full utility-registry onboarding chain in one committee vote and
-- produces the IssuanceConfig. One-shot per deployment (enforced by committee
-- diligence — no on-chain existence primitive available).
module Governance.TokenIssuance.SetupIssuance where

import Splice.Api.Token.HoldingV1 (InstrumentId(..))

import Utility.Registry.App.V0.Service.Provider
  (ProviderService, ProviderService_CreateProviderConfiguration(..),
   ProviderService_AcceptRegistrarServiceRequest(..))
import Utility.Registry.App.V0.Service.Registrar
  (RegistrarService_CreateAllocationFactory(..),
   RegistrarService_CreateInstrumentConfiguration(..),
   RegistrarServiceRequest(..),
   RegistrarServiceRequest_Accept(..))

import Governance.Action

import Governance.TokenIssuance.IssuanceConfig

-- | The initial plugin-setup proposal.
-- Consumes an existing ProviderService for the governance party (provisioned
-- beforehand via governance-token-custody or equivalent), runs the four
-- Utility-Registry onboarding steps, and creates IssuanceConfig. V1 uses
-- empty registrar/holder requirements, so no UserService / credential path.
template SetupIssuanceProposal
  with
    governanceParty : Party
    proposer : Party
    providerServiceCid : ContractId ProviderService
    operator : Party
    instrumentIdText : Text
    displayName : Text
    symbol : Text
    decimals : Int
  where
    signatory proposer
    observer governanceParty

    interface instance GovernableAction for SetupIssuanceProposal where
      view = GovernableActionView with
        governanceParty
        actionLabel = "SetupIssuance"
        description = "Initial setup: register instrument " <> instrumentIdText
                       <> " and create AllocationFactory"

      executeImpl = do
        -- 1. Create the provider configuration.
        providerConfigurationCid <- (.providerConfigurationCid) <$>
          exercise providerServiceCid ProviderService_CreateProviderConfiguration with
            registrarRequirements = []  -- empty for v1; tighten if the registry requires
            holderRequirements = []

        -- 2. Obtain a RegistrarService with registrar = governanceParty.
        --    Follows canton-vault's VaultGovernanceRules_SetupUtility pattern
        --    (see VaultGovernance.daml lines 481-506).
        registrarServiceRequestCid <- create RegistrarServiceRequest with
          operator
          provider = governanceParty
          registrar = governanceParty
          createTransferRule = None
          createAllocationFactory = None
        registrarServiceCid <- (.registrarServiceCid) <$>
          exercise providerServiceCid ProviderService_AcceptRegistrarServiceRequest with
            cid = registrarServiceRequestCid
            payload = RegistrarServiceRequest_Accept with
              providerConfigurationCid
              credentialCids = []  -- registrarRequirements was empty above; no credentials needed

        -- 3. Create the AllocationFactory.
        allocationFactoryCid <- (.allocationFactoryCid) <$>
          exercise registrarServiceCid RegistrarService_CreateAllocationFactory

        -- 4. Register the instrument.
        _ <- exercise registrarServiceCid RegistrarService_CreateInstrumentConfiguration with
          instrumentId = instrumentIdText
          additionalIdentifiers = []
          issuerRequirements = []
          holderRequirements = []

        -- 5. Create the IssuanceConfig.
        _ <- create IssuanceConfig with
          governanceParty
          instrumentId = InstrumentId with admin = governanceParty, id = instrumentIdText
          allocationFactoryCid
          displayName
          symbol
          decimals

        pure ()
```

> **If `registrarRequirements` is non-empty.** Canton-vault's `VaultGovernance.daml:483-499` shows what changes when the registry insists on credentials: `CreateProviderConfiguration` is fed real `registrarRequirements`, and the setup has to offer and accept a `Credential` before `AcceptRegistrarServiceRequest` will accept the empty `credentialCids = []`. For v1 we keep `registrarRequirements = []`; if the team later tightens the registry's requirements, follow canton-vault's pattern and extend `SetupIssuanceProposal` with the credential-handling steps.

- [ ] **Step 5: Run the test until it passes.**

Run:
```sh
cd daml/governance-token-issuance && daml build
cd ../governance-token-issuance-test && daml test --files daml/Governance/TokenIssuance/Test/SetupIssuanceTest.daml
```

Expected: iterate on the `executeImpl` body and `initUtilityPrereqs` until `test_when_setup_proposed_then_config_created` passes. The passing criterion is that after `confirmAndExecute`, exactly one `IssuanceConfig` contract exists for the governance party with the expected fields.

- [ ] **Step 6: Commit.**

```sh
git add daml/governance-token-issuance/daml/Governance/TokenIssuance/SetupIssuance.daml \
        daml/governance-token-issuance-test/daml/Governance/TokenIssuance/TestUtils.daml \
        daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Test/SetupIssuanceTest.daml
git commit -m "feat(token-issuance): implemented SetupIssuanceProposal"
```

---

## Task 6: Implement `MintProposal`

**Files:**
- Create: `daml/governance-token-issuance/daml/Governance/TokenIssuance/MintProposal.daml`
- Create: `daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Test/MintProposalTest.daml`

- [ ] **Step 1: Write the failing test.**

Write `daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Test/MintProposalTest.daml`:

```daml
-- Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

module Governance.TokenIssuance.Test.MintProposalTest where

import DA.Time (addRelTime, hours)
import Daml.Script

import Utility.Registry.App.V0.Model.Mint (MintOffer)

import Governance.Action (GovernableAction)
import Governance.Rules (GovernanceRules)

import Governance.TokenIssuance.IssuanceConfig (IssuanceConfig)
import Governance.TokenIssuance.MintProposal
import Governance.TokenIssuance.TestUtils

import TestHarness

-- Fixture with governance rules + setup done; IssuanceConfig exists.
data Fixture = Fixture
  with
    parties : IssuanceTestParties
    rulesCid : ContractId GovernanceRules
    configCid : ContractId IssuanceConfig
  deriving (Eq, Show)

given_setup_done : Script Fixture
given_setup_done = do
  parties <- allocateIssuanceTestParties
  rulesCid <- createTestGovernance parties
  -- Run SetupIssuanceProposal through the governance flow to get IssuanceConfig.
  -- Use the helper below; pulls in everything from SetupIssuanceTest.
  configCid <- setupIssuanceForTest parties rulesCid
  pure Fixture with ..

when_mint_offered : Fixture -> Script ()
when_mint_offered f = do
  now <- getTime
  proposalCid <- submit f.parties.member1 $ createCmd MintProposal with
    governanceParty = f.parties.governanceParty
    proposer = f.parties.member1
    issuanceConfigCid = f.configCid
    recipient = f.parties.outsider
    amount = 100.0
    description = "Bridge tx abcd1234"
    requestedAt = now
    executeBefore = now `addRelTime` hours 24
  let proposalInterfaceCid : ContractId GovernableAction = toInterfaceContractId proposalCid
  _ <- confirmAndExecute f.parties f.rulesCid proposalInterfaceCid
    [f.parties.member1, f.parties.member2] f.parties.member1
  pure ()

then_mint_offer_exists : Fixture -> () -> Script Failures
then_mint_offer_exists f _ = do
  offers <- query @MintOffer f.parties.governanceParty
  pure $ shouldBe "MintOffer count" 1 (length offers)

test_when_mint_proposed_then_mint_offer_created = script do
  run Test with
    given = given_setup_done
    when = when_mint_offered
    then_ = then_mint_offer_exists
```

> **`setupIssuanceForTest` helper.** Add this to TestUtils.daml (or a shared `Setup.daml` helper file under the test package) as the first step of this task, before writing `MintProposalTest.daml`:
>
> ```daml
> -- In TestUtils.daml
> import Governance.Action (GovernableAction)
> import Governance.TokenIssuance.IssuanceConfig (IssuanceConfig)
> import Governance.TokenIssuance.SetupIssuance
>
> setupIssuanceForTest : IssuanceTestParties -> ContractId GovernanceRules -> Script (ContractId IssuanceConfig)
> setupIssuanceForTest parties rulesCid = do
>   providerCid <- initUtilityPrereqs parties
>   proposalCid <- submit parties.member1 $ createCmd SetupIssuanceProposal with
>     governanceParty = parties.governanceParty
>     proposer = parties.member1
>     providerServiceCid = providerCid
>     operator = parties.operator
>     instrumentIdText = "TEST-TOKEN"
>     displayName = "Test Token"
>     symbol = "TEST"
>     decimals = 8
>   let proposalInterfaceCid : ContractId GovernableAction = toInterfaceContractId proposalCid
>   confirmAndExecute parties rulesCid proposalInterfaceCid
>     [parties.member1, parties.member2] parties.member1
>   [(configCid, _)] <- query @IssuanceConfig parties.governanceParty
>   pure configCid
> ```

- [ ] **Step 2: Run the test to verify it fails.**

Run:
```sh
cd daml/governance-token-issuance-test && daml test --files daml/Governance/TokenIssuance/Test/MintProposalTest.daml
```

Expected: FAIL — `MintProposal` not defined.

- [ ] **Step 3: Implement `MintProposal`.**

Write `daml/governance-token-issuance/daml/Governance/TokenIssuance/MintProposal.daml`:

```daml
-- Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- | Plugin template that offers a mint to a specific recipient via the
-- utility-registry's AllocationFactory_OfferMint choice. The recipient accepts
-- the resulting MintOffer later (outside the plugin).
module Governance.TokenIssuance.MintProposal where

import DA.TextMap as TextMap

import Splice.Api.Token.MetadataV1 (ExtraArgs(..), Metadata(..), emptyChoiceContext)
import Splice.Util (require)

import Utility.Registry.App.V0.Model.Mint (Mint(..))
import Utility.Registry.App.V0.Service.AllocationFactory
  (AllocationFactory_OfferMint(..))

import Governance.Action

import Governance.TokenIssuance.IssuanceConfig (IssuanceConfig(..))

-- Standard Splice reason key for wallet-visible explanations.
spliceReasonKey : Text
spliceReasonKey = "splice.lfdecentralizedtrust.org/reason"

template MintProposal
  with
    governanceParty : Party
    proposer : Party
    issuanceConfigCid : ContractId IssuanceConfig
    recipient : Party
    amount : Decimal
    description : Text
    requestedAt : Time
    executeBefore : Time
  where
    signatory proposer
    observer governanceParty

    ensure amount > 0.0

    interface instance GovernableAction for MintProposal where
      view = GovernableActionView with
        governanceParty
        actionLabel = "Mint"
        description

      executeImpl = do
        config <- fetch issuanceConfigCid
        require "IssuanceConfig governanceParty matches proposal"
          (config.governanceParty == governanceParty)
        _ <- exercise config.allocationFactoryCid AllocationFactory_OfferMint with
          expectedAdmin = governanceParty
          mint = Mint with
            instrumentId = config.instrumentId
            holder = recipient
            amount
            requestedAt
            executeBefore
          extraArgs = ExtraArgs with
            context = emptyChoiceContext
            meta = Metadata with
              values = TextMap.fromList [(spliceReasonKey, description)]
        pure ()
```

> **`Mint` record fields.** The exact field shape of the `Mint` data type lives in `utility-registry-app-v0-0.7.0/…/Utility/Registry/App/V0/Model/Mint.daml`. Verify the fields above (`instrumentId`, `holder`, `amount`, `requestedAt`, `executeBefore`) match; adjust if the record has additional fields (e.g., `context` or operator-related fields). Similarly, verify `ExtraArgs.context` is what `emptyChoiceContext` produces, or use whatever the registry expects (see `AllocationFactory.daml:489-508` for the list of required context keys).

- [ ] **Step 4: Run the test.**

Run:
```sh
cd daml/governance-token-issuance && daml build
cd ../governance-token-issuance-test && daml test --files daml/Governance/TokenIssuance/Test/MintProposalTest.daml
```

Expected: PASS. If the factory's `OfferMint` requires additional context (instrument configuration cid, issuer credentials), the test needs to pass them via `extraArgs.context`. Add whatever the factory demands; see the `AllocationFactory_OfferMint` body (line 479 onwards) for context keys like `instrumentConfigurationContextKey`, `issuerCredentialsContextKey`.

- [ ] **Step 5: Commit.**

```sh
git add daml/governance-token-issuance/daml/Governance/TokenIssuance/MintProposal.daml \
        daml/governance-token-issuance-test/daml/Governance/TokenIssuance/TestUtils.daml \
        daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Test/MintProposalTest.daml
git commit -m "feat(token-issuance): implemented MintProposal"
```

---

## Task 7: Implement `BurnProposal`

**Files:**
- Create: `daml/governance-token-issuance/daml/Governance/TokenIssuance/BurnProposal.daml`
- Create: `daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Test/BurnProposalTest.daml`

- [ ] **Step 1: Write the failing test.**

Write `daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Test/BurnProposalTest.daml`:

```daml
-- Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

module Governance.TokenIssuance.Test.BurnProposalTest where

import DA.Time (addRelTime, hours)
import Daml.Script

import Splice.Api.Token.HoldingV1 (Holding)

import Utility.Registry.App.V0.Model.Burn (BurnOffer)

import Governance.Action (GovernableAction)
import Governance.Rules (GovernanceRules)

import Governance.TokenIssuance.BurnProposal
import Governance.TokenIssuance.IssuanceConfig (IssuanceConfig)
import Governance.TokenIssuance.TestUtils

import TestHarness

data Fixture = Fixture
  with
    parties : IssuanceTestParties
    rulesCid : ContractId GovernanceRules
    configCid : ContractId IssuanceConfig
    holderHoldingCids : [ContractId Holding]
  deriving (Eq, Show)

given_holder_with_tokens : Script Fixture
given_holder_with_tokens = do
  parties <- allocateIssuanceTestParties
  rulesCid <- createTestGovernance parties
  configCid <- setupIssuanceForTest parties rulesCid
  -- Mint some tokens to an outsider so we have real holdings to burn.
  holderHoldingCids <- mintForTest parties rulesCid configCid parties.outsider 100.0
  pure Fixture with ..

when_burn_offered : Fixture -> Script ()
when_burn_offered f = do
  now <- getTime
  proposalCid <- submit f.parties.member1 $ createCmd BurnProposal with
    governanceParty = f.parties.governanceParty
    proposer = f.parties.member1
    issuanceConfigCid = f.configCid
    holder = f.parties.outsider
    holdingCids = f.holderHoldingCids
    amount = 100.0
    description = "Bridge unwind tx abcd1234"
    requestedAt = now
    executeBefore = now `addRelTime` hours 24
  let proposalInterfaceCid : ContractId GovernableAction = toInterfaceContractId proposalCid
  _ <- confirmAndExecute f.parties f.rulesCid proposalInterfaceCid
    [f.parties.member1, f.parties.member2] f.parties.member1
  pure ()

then_burn_offer_exists : Fixture -> () -> Script Failures
then_burn_offer_exists f _ = do
  offers <- query @BurnOffer f.parties.governanceParty
  pure $ shouldBe "BurnOffer count" 1 (length offers)

test_when_burn_proposed_then_burn_offer_created = script do
  run Test with
    given = given_holder_with_tokens
    when = when_burn_offered
    then_ = then_burn_offer_exists
```

> **`mintForTest` helper.** Needed to get real holdings into `outsider`'s hands. Before writing it, open `utility-registry-app-v0-0.7.0/…/Utility/Registry/App/V0/Model/Mint.daml` to confirm the `MintOffer` template's accept choice name, its controller, and what `extraArgs` it requires (most likely an `issuerCredentials` context key and possibly an `InstrumentConfiguration` cid, same shape as `AllocationFactory_OfferMint.extraArgs`).
>
> Add to TestUtils.daml (substitute `MintOffer_Accept` with the actual choice name if it differs, and fill in the correct `extraArgs`):
>
> ```daml
> import Splice.Api.Token.HoldingV1 (Holding)
> import Splice.Api.Token.MetadataV1 (ExtraArgs(..), emptyChoiceContext, emptyMetadata)
> import Utility.Registry.App.V0.Model.Mint (MintOffer, MintOffer_Accept(..))
> import Governance.TokenIssuance.MintProposal (MintProposal(..))
> import DA.Time (addRelTime, hours)
>
> mintForTest : IssuanceTestParties -> ContractId GovernanceRules -> ContractId IssuanceConfig -> Party -> Decimal -> Script [ContractId Holding]
> mintForTest parties rulesCid configCid recipient amount = do
>   now <- getTime
>   proposalCid <- submit parties.member1 $ createCmd MintProposal with
>     governanceParty = parties.governanceParty
>     proposer = parties.member1
>     issuanceConfigCid = configCid
>     recipient
>     amount
>     description = "test mint"
>     requestedAt = now
>     executeBefore = now `addRelTime` hours 24
>   let iface : ContractId GovernableAction = toInterfaceContractId proposalCid
>   confirmAndExecute parties rulesCid iface
>     [parties.member1, parties.member2] parties.member1
>   -- After confirmAndExecute, a MintOffer contract is visible to the
>   -- governance party and the recipient. Query it, then have recipient
>   -- accept it to materialise the Holding.
>   [(offerCid, _)] <- query @MintOffer recipient
>   submit (actAs recipient <> readAs parties.governanceParty <> readAs parties.operator) $
>     exerciseCmd offerCid MintOffer_Accept with
>       extraArgs = ExtraArgs with
>         context = emptyChoiceContext   -- supply real context keys if the impl rejects empty
>         meta = emptyMetadata
>   results <- queryInterface @Holding recipient
>   pure (map fst results)
> ```

- [ ] **Step 2: Run the test to verify it fails.**

Run:
```sh
cd daml/governance-token-issuance-test && daml test --files daml/Governance/TokenIssuance/Test/BurnProposalTest.daml
```

Expected: FAIL — `BurnProposal` not defined, and `mintForTest` TODO.

- [ ] **Step 3: Implement `BurnProposal`.**

Write `daml/governance-token-issuance/daml/Governance/TokenIssuance/BurnProposal.daml`:

```daml
-- Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- | Plugin template that offers a burn to a specific holder via the
-- utility-registry's AllocationFactory_OfferBurn choice. The holder accepts
-- the resulting BurnOffer later (outside the plugin).
module Governance.TokenIssuance.BurnProposal where

import DA.TextMap as TextMap

import Splice.Api.Token.HoldingV1 (Holding)
import Splice.Api.Token.MetadataV1 (ExtraArgs(..), Metadata(..), emptyChoiceContext)
import Splice.Util (require)

import Utility.Registry.App.V0.Model.Burn (Burn(..))
import Utility.Registry.App.V0.Service.AllocationFactory
  (AllocationFactory_OfferBurn(..))

import Governance.Action

import Governance.TokenIssuance.IssuanceConfig (IssuanceConfig(..))

spliceReasonKey : Text
spliceReasonKey = "splice.lfdecentralizedtrust.org/reason"

template BurnProposal
  with
    governanceParty : Party
    proposer : Party
    issuanceConfigCid : ContractId IssuanceConfig
    holder : Party
    holdingCids : [ContractId Holding]
    amount : Decimal
    description : Text
    requestedAt : Time
    executeBefore : Time
  where
    signatory proposer
    observer governanceParty

    ensure amount > 0.0

    interface instance GovernableAction for BurnProposal where
      view = GovernableActionView with
        governanceParty
        actionLabel = "Burn"
        description

      executeImpl = do
        config <- fetch issuanceConfigCid
        require "IssuanceConfig governanceParty matches proposal"
          (config.governanceParty == governanceParty)
        _ <- exercise config.allocationFactoryCid AllocationFactory_OfferBurn with
          expectedAdmin = governanceParty
          burn = Burn with
            instrumentId = config.instrumentId
            holder
            amount
            holdingCids
            requestedAt
            executeBefore
          extraArgs = ExtraArgs with
            context = emptyChoiceContext
            meta = Metadata with
              values = TextMap.fromList [(spliceReasonKey, description)]
        pure ()
```

> **`Burn` record fields.** Verify the shape against `utility-registry-app-v0-0.7.0/…/Utility/Registry/App/V0/Model/Burn.daml`. Likely fields include `instrumentId`, `holder`, `amount`, `holdingCids`, `requestedAt`, `executeBefore`. Same context-key caveats as for `Mint`.

- [ ] **Step 4: Finish `mintForTest` and run the test.**

Complete the `mintForTest` helper by implementing the `MintOffer` accept step. Then:

```sh
cd daml/governance-token-issuance && daml build
cd ../governance-token-issuance-test && daml test --files daml/Governance/TokenIssuance/Test/BurnProposalTest.daml
```

Expected: PASS — one `BurnOffer` contract for the governance party after `confirmAndExecute`.

- [ ] **Step 5: Commit.**

```sh
git add daml/governance-token-issuance/daml/Governance/TokenIssuance/BurnProposal.daml \
        daml/governance-token-issuance-test/daml/Governance/TokenIssuance/TestUtils.daml \
        daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Test/BurnProposalTest.daml
git commit -m "feat(token-issuance): implemented BurnProposal"
```

---

## Task 8: Implement `RotateFactoryProposal`

**Files:**
- Create: `daml/governance-token-issuance/daml/Governance/TokenIssuance/RotateFactoryProposal.daml`
- Create: `daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Test/RotateFactoryTest.daml`

- [ ] **Step 1: Write the failing test.**

Write `daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Test/RotateFactoryTest.daml`:

```daml
-- Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

module Governance.TokenIssuance.Test.RotateFactoryTest where

import Daml.Script

import Utility.Registry.App.V0.Service.AllocationFactory (AllocationFactory)

import Governance.Action (GovernableAction)
import Governance.Rules (GovernanceRules)

import Governance.TokenIssuance.IssuanceConfig (IssuanceConfig)
import Governance.TokenIssuance.RotateFactoryProposal
import Governance.TokenIssuance.TestUtils

import TestHarness

data Fixture = Fixture
  with
    parties : IssuanceTestParties
    rulesCid : ContractId GovernanceRules
    oldConfigCid : ContractId IssuanceConfig
    newFactoryCid : ContractId AllocationFactory
  deriving (Eq, Show)

given_setup_plus_spare_factory : Script Fixture
given_setup_plus_spare_factory = do
  parties <- allocateIssuanceTestParties
  rulesCid <- createTestGovernance parties
  oldConfigCid <- setupIssuanceForTest parties rulesCid
  -- Provision a second AllocationFactory for governance party as a rotate target.
  newFactoryCid <- initSpareFactory parties
  pure Fixture with ..

when_rotated : Fixture -> Script ()
when_rotated f = do
  proposalCid <- submit f.parties.member1 $ createCmd RotateFactoryProposal with
    governanceParty = f.parties.governanceParty
    proposer = f.parties.member1
    issuanceConfigCid = f.oldConfigCid
    newFactoryCid = f.newFactoryCid
  let proposalInterfaceCid : ContractId GovernableAction = toInterfaceContractId proposalCid
  _ <- confirmAndExecute f.parties f.rulesCid proposalInterfaceCid
    [f.parties.member1, f.parties.member2] f.parties.member1
  pure ()

then_old_archived_new_has_new_factory : Fixture -> () -> Script Failures
then_old_archived_new_has_new_factory f _ = do
  oldStillThere <- queryContractId f.parties.governanceParty f.oldConfigCid
  configs <- query @IssuanceConfig f.parties.governanceParty
  case configs of
    [(_, config)] -> pure $
      shouldBe "old archived" True (isNone oldStillThere) <>
      shouldBe "allocationFactoryCid updated" f.newFactoryCid config.allocationFactoryCid
    _ -> pure $ shouldBe "IssuanceConfig count" 1 (length configs)

test_when_rotate_proposed_then_config_replaced = script do
  run Test with
    given = given_setup_plus_spare_factory
    when = when_rotated
    then_ = then_old_archived_new_has_new_factory
```

> **`initSpareFactory` helper.** Add to TestUtils.daml — provisions a second `AllocationFactory` for the governance party (a second `RegistrarService` path, or a test-shortcut that creates the factory directly). See `canton-vault/vault-daml/governance/daml/BitsafeVault/VaultGovernance.daml:501-508` for the standard path.

- [ ] **Step 2: Run the test to verify it fails.**

Run:
```sh
cd daml/governance-token-issuance-test && daml test --files daml/Governance/TokenIssuance/Test/RotateFactoryTest.daml
```

Expected: FAIL — `RotateFactoryProposal` not defined.

- [ ] **Step 3: Implement `RotateFactoryProposal`.**

Write `daml/governance-token-issuance/daml/Governance/TokenIssuance/RotateFactoryProposal.daml`:

```daml
-- Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- | Plugin template for rotating the AllocationFactory cid on IssuanceConfig.
-- Thin wrapper around IssuanceConfig_RotateFactory: verifies the new factory's
-- admin equals the governance party, then exercises the choice on IssuanceConfig.
module Governance.TokenIssuance.RotateFactoryProposal where

import Splice.Api.Token.BurnMintV1
  (BurnMintFactory, BurnMintFactory_PublicFetch(..), BurnMintFactoryView(..))
import Splice.Util (require)

import Utility.Registry.App.V0.Service.AllocationFactory (AllocationFactory)

import Governance.Action

import Governance.TokenIssuance.IssuanceConfig
  (IssuanceConfig(..), IssuanceConfig_RotateFactory(..))

template RotateFactoryProposal
  with
    governanceParty : Party
    proposer : Party
    issuanceConfigCid : ContractId IssuanceConfig
    newFactoryCid : ContractId AllocationFactory
  where
    signatory proposer
    observer governanceParty

    interface instance GovernableAction for RotateFactoryProposal where
      view = GovernableActionView with
        governanceParty
        actionLabel = "RotateFactory"
        description = "Rotate the AllocationFactory cid on IssuanceConfig"

      executeImpl = do
        -- Validate the fetched IssuanceConfig matches the proposal.
        config <- fetch issuanceConfigCid
        require "IssuanceConfig governanceParty matches proposal"
          (config.governanceParty == governanceParty)
        -- Validate the new factory's admin via the BurnMintFactory view.
        let newFactoryBmfCid : ContractId BurnMintFactory = coerceContractId newFactoryCid
        view <- exercise newFactoryBmfCid BurnMintFactory_PublicFetch with
          expectedAdmin = governanceParty
          actor = governanceParty
        require "new factory admin is governance party"
          (view.admin == governanceParty)
        -- Perform the archive-and-recreate via the choice on IssuanceConfig.
        _ <- exercise issuanceConfigCid IssuanceConfig_RotateFactory with
          newFactoryCid
        pure ()
```

- [ ] **Step 4: Run the test.**

Run:
```sh
cd daml/governance-token-issuance && daml build
cd ../governance-token-issuance-test && daml test --files daml/Governance/TokenIssuance/Test/RotateFactoryTest.daml
```

Expected: PASS — the old `IssuanceConfig` is archived and a new one with `allocationFactoryCid == newFactoryCid` exists.

- [ ] **Step 5: Commit.**

```sh
git add daml/governance-token-issuance/daml/Governance/TokenIssuance/RotateFactoryProposal.daml \
        daml/governance-token-issuance-test/daml/Governance/TokenIssuance/TestUtils.daml \
        daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Test/RotateFactoryTest.daml
git commit -m "feat(token-issuance): implemented RotateFactoryProposal"
```

---

## Task 9: Add negative tests

**Files:**
- Modify: `daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Test/SetupIssuanceTest.daml`
- Modify: `daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Test/MintProposalTest.daml`
- Modify: `daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Test/BurnProposalTest.daml`

Three narrow negative scenarios, each wired with the same `given / when / then_` pattern as the happy-path tests. The harness's `submitMustFail` primitive is the tool for "the ledger should reject this command".

> **Why these three and not more.** The chosen tests exercise the plugin's own validation boundaries: (1) governance-core rejects non-member confirmations; (2) each proposal template enforces `ensure amount > 0`. Out of scope for v1 — nice-to-have later — are tests for a `MintProposal` / `BurnProposal` referencing an `IssuanceConfig` whose `governanceParty` doesn't match the proposal's, and tests for `RotateFactoryProposal` passed a factory with the wrong admin. Both require setting up a second governance party or a second `AllocationFactory` for a different registrar, which is disproportionately heavy.

- [ ] **Step 1: Add the non-member-confirmation test to `SetupIssuanceTest.daml`.**

Append:

```daml
-- | An outsider (not a member of governance) cannot confirm an action.
-- The rules contract rejects `GovernanceRules_ConfirmAction` from non-members.
when_non_member_confirms : Fixture -> Script ()
when_non_member_confirms f = do
  providerCid <- initUtilityPrereqs f.parties
  proposalCid <- submit f.parties.member1 $ createCmd SetupIssuanceProposal with
    governanceParty = f.parties.governanceParty
    proposer = f.parties.member1
    providerServiceCid = providerCid
    operator = f.parties.operator
    instrumentIdText = "TEST-TOKEN"
    displayName = "Test Token"
    symbol = "TEST"
    decimals = 8
  let proposalInterfaceCid : ContractId GovernableAction = toInterfaceContractId proposalCid
  submitMustFail (memberContext f.parties.outsider f.parties.governanceParty) $
    exerciseCmd f.rulesCid GovernanceRules_ConfirmAction with
      confirmer = f.parties.outsider
      actionProposalCid = proposalInterfaceCid

then_nothing : Fixture -> () -> Script Failures
then_nothing _ _ = pure []

test_when_non_member_confirms_then_rejected = script do
  run Test with
    given = given_governance
    when = when_non_member_confirms
    then_ = then_nothing
```

You'll also need `import Governance.Rules (GovernanceRules_ConfirmAction(..))` at the top of the file if not already present.

- [ ] **Step 2: Add the zero-amount-mint test to `MintProposalTest.daml`.**

Append:

```daml
-- | MintProposal has `ensure amount > 0.0`. The ledger rejects a `createCmd`
-- with `amount = 0.0` at template-precondition time, before the proposal
-- even reaches the governance flow.
when_zero_amount : Fixture -> Script ()
when_zero_amount f = do
  now <- getTime
  submitMustFail f.parties.member1 $ createCmd MintProposal with
    governanceParty = f.parties.governanceParty
    proposer = f.parties.member1
    issuanceConfigCid = f.configCid
    recipient = f.parties.outsider
    amount = 0.0
    description = "should be rejected"
    requestedAt = now
    executeBefore = now `addRelTime` hours 24

then_nothing_mint : Fixture -> () -> Script Failures
then_nothing_mint _ _ = pure []

test_when_zero_amount_then_create_rejected = script do
  run Test with
    given = given_setup_done
    when = when_zero_amount
    then_ = then_nothing_mint
```

- [ ] **Step 3: Add the zero-amount-burn test to `BurnProposalTest.daml`.**

Append:

```daml
-- | BurnProposal has `ensure amount > 0.0`. Same reasoning as for mint.
when_zero_amount_burn : Fixture -> Script ()
when_zero_amount_burn f = do
  now <- getTime
  submitMustFail f.parties.member1 $ createCmd BurnProposal with
    governanceParty = f.parties.governanceParty
    proposer = f.parties.member1
    issuanceConfigCid = f.configCid
    holder = f.parties.outsider
    holdingCids = f.holderHoldingCids
    amount = 0.0
    description = "should be rejected"
    requestedAt = now
    executeBefore = now `addRelTime` hours 24

then_nothing_burn : Fixture -> () -> Script Failures
then_nothing_burn _ _ = pure []

test_when_zero_amount_burn_then_create_rejected = script do
  run Test with
    given = given_holder_with_tokens
    when = when_zero_amount_burn
    then_ = then_nothing_burn
```

- [ ] **Step 4: Run the three new tests.**

Run:
```sh
cd daml/governance-token-issuance-test && daml test
```

Expected: all existing happy-path tests still pass, plus the three new negative tests.

- [ ] **Step 5: Commit.**

```sh
git add daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Test/SetupIssuanceTest.daml \
        daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Test/MintProposalTest.daml \
        daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Test/BurnProposalTest.daml
git commit -m "test(token-issuance): added negative tests for non-member confirmation and zero-amount create"
```

---

## Task 10: Sandbox-populating script + justfile recipe

**Files:**
- Create: `daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Scripts/PopulateSandbox.daml`
- Create: `daml/justfile`

A one-shot Daml script for local development: starts from an empty sandbox, runs the plugin through the full setup → mint → recipient-accept flow, and leaves the sandbox with a visible `Holding` for an outsider party so you can poke at it with the navigator or the ledger API. The `just populate-sandbox` recipe wraps the command so it's discoverable alongside build / test commands.

> **Why this lives in the test package.** It reuses the TestUtils helpers (`allocateIssuanceTestParties`, `createTestGovernance`, `initUtilityPrereqs`, `confirmAndExecute`, `setupIssuanceForTest`, `mintForTest`). Writing a standalone deployment-scripts package would duplicate all that plumbing. The script is a daml-script function, so it runs the same way as tests — against a live sandbox rather than an in-memory simulated ledger.

- [ ] **Step 1: Write the populate-sandbox script.**

Write `daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Scripts/PopulateSandbox.daml`:

```daml
-- Copyright (c) 2026 DLC-Link, Inc. and/or its affiliates. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- | Populates a local sandbox with a freshly-issued test token.
-- Run via: `just populate-sandbox` (see daml/justfile).
--
-- After this script runs, the sandbox contains:
--   - IssuanceTestParties (governanceParty, three members, outsider, operator, DSO)
--   - A GovernanceRules with threshold 2 of 3 members
--   - An IssuanceConfig for a "TEST-TOKEN" instrument
--   - A Holding for `outsider` of 100.0 TEST-TOKEN (minted via MintProposal +
--     recipient accept)
module Governance.TokenIssuance.Scripts.PopulateSandbox where

import Daml.Script

import Governance.TokenIssuance.TestUtils

populate : Script ()
populate = do
  parties <- allocateIssuanceTestParties
  rulesCid <- createTestGovernance parties
  configCid <- setupIssuanceForTest parties rulesCid
  holdingCids <- mintForTest parties rulesCid configCid parties.outsider 100.0
  debug $ "Populated sandbox: "
    <> show (length holdingCids) <> " Holding(s) created for outsider"
  pure ()
```

- [ ] **Step 2: Rebuild the test package so the script lands in the DAR.**

```sh
cd daml/governance-token-issuance-test && daml build
```

Expected: build succeeds; the new script module compiles against the TestUtils helpers.

- [ ] **Step 3: Write `daml/justfile` with recipes for build / test / populate-sandbox.**

Write `daml/justfile`:

```make
# justfile for the dec-party-manager Daml packages

# Default recipe: list available recipes.
default:
    @just --list

# Build all Daml packages.
build:
    daml build --all

# Build only the token-issuance plugin and its test package.
build-issuance:
    cd governance-token-issuance && daml build
    cd governance-token-issuance-test && daml build

# Run the token-issuance test suite.
test-issuance:
    cd governance-token-issuance-test && daml test

# Start a local sandbox on port 6865 (run in a separate terminal).
sandbox:
    daml sandbox --port 6865

# Populate a running sandbox with a freshly-issued test token.
# Assumes `just sandbox` (or equivalent) is running on localhost:6865.
populate-sandbox: build-issuance
    daml script \
      --dar governance-token-issuance-test/.daml/dist/governance-token-issuance-test-0.1.0.dar \
      --script-name Governance.TokenIssuance.Scripts.PopulateSandbox:populate \
      --ledger-host localhost \
      --ledger-port 6865 \
      --wall-clock-time

# Clean Daml build artefacts for token-issuance packages.
clean-issuance:
    rm -rf governance-token-issuance/.daml governance-token-issuance-test/.daml
```

- [ ] **Step 4: Smoke-test the recipe end-to-end.**

In one terminal:
```sh
cd daml && just sandbox
```

In a second terminal:
```sh
cd daml && just populate-sandbox
```

Expected: the script runs through without errors and prints the debug line `"Populated sandbox: 1 Holding(s) created for outsider"` (or similar). The sandbox now has the described contracts; they can be inspected via `daml navigator` or the ledger API.

- [ ] **Step 5: Commit.**

```sh
git add daml/governance-token-issuance-test/daml/Governance/TokenIssuance/Scripts/PopulateSandbox.daml \
        daml/justfile
git commit -m "feat(token-issuance): added populate-sandbox script and justfile recipes"
```

---

## Task 11: Run the full test suite and verify nothing regressed

**Files:** none modified.

- [ ] **Step 1: Build both packages.**

```sh
cd daml/governance-token-issuance && daml build
cd ../governance-token-issuance-test && daml build
```

Expected: both succeed with no warnings about unused imports/binds.

- [ ] **Step 2: Run all tests in the test package.**

```sh
cd daml/governance-token-issuance-test && daml test
```

Expected: all seven test scripts pass — four happy-path (setup, mint, burn, rotate) and three negative (non-member confirmation, zero-amount mint create, zero-amount burn create).

- [ ] **Step 3: Run the full multi-package build to confirm no downstream breakage.**

```sh
cd daml && daml build --all
```

Expected: all packages build. If `governance-token-custody` or `governance-core` regresses, investigate — nothing we did should touch them.

- [ ] **Step 4: Commit the plan itself to the repo.**

```sh
git add docs/superpowers/plans/2026-04-20-token-issuance-plugin.md
git commit -m "docs(token-issuance): added implementation plan"
```

(If the plan was already committed before work began, skip this.)

---

## Notes for the engineer

- **Utility-registry detail drift.** The exact shape of `ProviderService`, `RegistrarService`, `Mint`, `Burn`, and the various context keys required by `AllocationFactory`'s choice bodies is defined in `utility-registry-app-v0-0.7.0`. Module paths follow the file layout: `Utility.Registry.App.V0.Service.Provider` (file `Service/Provider.daml`), `Utility.Registry.App.V0.Service.Registrar` (file `Service/Registrar.daml`), `Utility.Registry.App.V0.Service.AllocationFactory`, `Utility.Registry.App.V0.Model.Mint`, `Utility.Registry.App.V0.Model.Burn`. Locally the source lives under `canton-network-utility-dars-0.12.0/utility-registry-app-v0-0.7.0/utility-registry-app-v0-0.7.0-<hash>/Utility/…`. When a test fails because a record constructor is wrong or a context key is missing, read the relevant source file and adjust. The design doc (`docs/TOKEN_ISSUANCE_PLUGIN.md`) cites specific lines for where `OfferMint` / `OfferBurn` are defined (`AllocationFactory.daml:479-494` and nearby).
- **`require` import.** Several templates call `require "message" condition`. Import via `import Splice.Util (require)`.
- **Test harness library.** All tests use the given / when / then_ pattern from `DLC-link/daml-test-harness`, shipped as `daml/dars/testlib-0.1.0.dar`. Key exports (`import TestHarness`): the `Test` record (`{ given, when, then_ }`), `run` to thread a fixture through the three phases, `Failures` (a monoid over failure messages — `<>` combines, `[]` is pass), and `shouldBe "label" expected actual` which yields `[]` on match or a non-empty `Failures` on mismatch. After a local `daml build`, the harness source is readable at `daml/<package>-test/.daml/dependencies/2.2/…/testlib-0.1.0-…/TestHarness.daml` if the engineer needs to check exact types. Copy the overall file shape from `governance-token-custody-test`'s tests for anything ambiguous.
- **Multi-party submit in tests.** `confirmAndExecute` uses `actAs executor <> readAs governanceParty <> readAs outsider <> readAs operator <> readAs dso`. This is a test-mode shortcut to simulate Canton's contract disclosure; it's not how the production authorization chain works. Keep it as-is for v1.
- **No on-chain uniqueness for `IssuanceConfig`.** The plan does not enforce "only one `IssuanceConfig` per governance party"; the design says committee diligence prevents running `SetupIssuanceProposal` twice. The tests accordingly only cover the happy path; a hostile double-setup would produce two `IssuanceConfig` contracts with no on-chain complaint. If the team decides to add a setup-marker template later, it's an additive change.
- **README / project docs.** Not included as a task; add a short `daml/governance-token-issuance/README.md` pointing at `docs/TOKEN_ISSUANCE_PLUGIN.md` if the team wants one.
