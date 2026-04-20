# Token Issuance Plugin — Design

**Status:** draft design. Most items are proposed defaults (open to team review); two architectural proposals are flagged as explicitly requiring team consensus before being locked in.

**Terminology note.** Strictly, "issuance" means the creation of new tokens (minting) — burning is its opposite, and "supply management" is the umbrella term covering both. This document uses "issuance" colloquially, following the name of the plugin, to refer to the full mint + burn lifecycle that the plugin governs. Wherever the distinction matters, the specific operation ("mint" or "burn") is named explicitly.

## Overview

A Daml package that plugs into `governance-core`. Each plugin template implements the `GovernableAction` interface (see [GOVERNANCE_PLUGIN_ARCHITECTURE.md](GOVERNANCE_PLUGIN_ARCHITECTURE.md)) and wraps a privileged Daml operation: the mint and burn templates call `BurnMintFactory_BurnMint` from the Splice Token API (package `splice-api-token-burn-mint-v1`); the setup template wraps the Utility-Registry onboarding chain (`ProviderService` / `RegistrarService` calls); the pause and rotate templates both archive-and-recreate `IssuanceConfig`.

**Goal.** Let a decentralized governance party (the signatory on `GovernanceRules`) mint and burn its own token instrument, with each mint or burn gated by a threshold of committee confirmations.

**Contrast with `canton-vault`.** Canton-vault bundles each mint with an atomic asset swap, so Canton transaction validation alone guarantees the economic invariant (see [TOKEN_ISSUANCE_IN_CANTON_VAULT.md](TOKEN_ISSUANCE_IN_CANTON_VAULT.md)). This plugin has **no swap**. Each mint or burn is an independent privileged action. The committee attests off-ledger to whatever event justifies it and votes on each individual issuance. The issuance mechanics (the `BurnMintFactory_BurnMint` primitive, the instrument-admin role, the `AllocationFactory` setup) are the same Splice / Utility-Registry primitives canton-vault uses; what changes is that there is no on-ledger trigger, and the authority for each mint/burn comes from a governance confirmation threshold instead of a user's `TransferInstruction_Accept`.

---

## Proposals requiring team consensus

The following two choices keep the plugin self-contained and simple, but they have real operational implications and foreclose certain use cases. They are proposed as the default but should be confirmed by the team before being locked in.

### Treasury-first mint

**Proposal.** Every `MintProposal` mints into a treasury pool owned by the governance party itself. `BurnMintOutput.owner = governanceParty`, `extraActors = []` (see the appendix ["What `extraActors` means"](#appendix-what-extraactors-means) if the term needs unpacking). Delivery to the final recipient is a *separate* governance action, via the custody plugin's `TransferProposal`.

**Why this simplifies the plugin.** The Splice `AllocationFactory` implementation requires the owner of a newly-minted holding to appear in `BurnMintFactory_BurnMint.extraActors`. A mint directly to an arbitrary third party would therefore need that party's authority in scope inside `executeImpl` — which forces either a recipient countersignature on every `MintProposal`, a pre-existing `MintPreapproval` contract, or a non-`AllocationFactory` implementation. Treasury-first sidesteps all three, because the governance party is both the admin and the owner of the new holding.

**Trade-off.** "Mint and deliver to Alice" becomes two governance votes (one `MintProposal`, then one `TransferProposal` out of the custody plugin). This is fine for lower-frequency governance-controlled issuance, and potentially expensive for high-throughput bridge-style flows where every external event produces a mint.

**Deployment coordination.** `TransferProposal` in the custody plugin wraps `TransferFactory_Transfer` on a `TransferFactory` cid. Since the `AllocationFactory` provisioned by this plugin implements both `BurnMintFactory` and `TransferFactory` as the same on-ledger contract, the same cid can serve both plugins. The custody plugin and the issuance plugin should share the factory cid rather than each running their own Utility-Registry onboarding — otherwise the governance party ends up with two `AllocationFactory` contracts for the same registrar role, doubling the Utility-Registry onboarding surface area without adding any capability. Worth explicit coordination between the two plugin deployments.

**Alternative if rejected.** Use the utility-registry's built-in two-step mint workflows on `AllocationFactory` — `AllocationFactory_OfferMint` (controller: `registrar`, i.e. the governance party; defined in `utility-registry-app-v0-0.7.0`, module `Utility.Registry.App.V0.Service.AllocationFactory`, lines 479–494) lets the committee offer a mint that the recipient later accepts, and `AllocationFactory_RequestMint` (controller: `mint.holder`) lets the recipient request a mint the committee approves. Both land the recipient's authority at accept time, not at propose or execute time, and create standard registry templates (`MintOffer` / `MintRequest`) instead of anything we'd have to invent. This is the core-utility answer to the recipient-authority problem.

### Treasury-only burn

**Proposal.** Every `BurnProposal` consumes holdings from the same treasury pool — i.e. holdings owned by the governance party. `extraActors = []`. The governance party's authority is already present inside `executeImpl` via the governance exercise chain (`GovernanceRules` signed by `governanceParty` → `GovernanceRules_ExecuteConfirmedAction` → `GovernableAction_Execute` whose controller is the view's `governanceParty`); no third-party signatures required.

**Why this simplifies the plugin.** No redemption flow to model, no holder authority to collect, no proposer-outside-the-committee case to handle. Symmetric with treasury-first mint.

**Trade-off.** User-initiated redemption — where a holder hands their tokens back in exchange for something — is not supported. Burns reflect off-chain unwinds decided by the committee, not user surrender.

**Alternative if rejected.** Add a `RedemptionBurnProposal` template whose signatories include the holder, with `extraActors = [holder]` in the burn call. This also means the proposer may be a user rather than a committee member, which is worth explicit validation against the governance-core flow.

---

## Out-of-scope prerequisite: attestation protocol

How committee members learn about the external event they're voting on — a bridge oracle, a signed attestation chain, a manual evidence process — is out of scope for the plugin itself, but the plugin assumes *some* such protocol exists. The structure of the evidence it delivers determines whether the free-form `description` field (the current v1 default) is enough or whether we need typed fields on `MintProposal` / `BurnProposal`. Choosing that protocol is a parallel decision that should happen alongside this plugin design, before any structured proposal schema is fixed.

---

## Proposed decisions

Items the proposer has made an initial call on. They are working defaults for the plugin's design and are open to team review — this is not the "requires explicit consensus" tier above, but any of them can be reopened on request.

### Product-level decisions

Scope, policy, and feature choices. These define what the plugin does and doesn't do, and each could be reopened without changing the plugin's internal wiring.

#### Setup runs through the governance committee, not as an out-of-band script

The Utility-Registry onboarding that produces the `AllocationFactory` and registers the instrument is performed under committee control, via a governance-gated proposal, rather than by a sysadmin running a one-off deployment script with the right keys. This keeps the full plugin lifecycle (setup → mint / burn → pause / rotate) within the same governance committee that signs the governance party.

(Unrelated: the Splice utility-registry has its own concept of an "operator" *Party* — a Daml party that represents the registry app itself and appears as a co-signatory on `ProviderService`, `UserService`, `RegistrarService`, etc. That operator party shows up as a field on `SetupIssuanceProposal` in the technical section below. The "operator-driven" we were ruling out here is the sysadmin sense, not the Splice operator-party sense.)

#### Single-instrument plugin deployment

Each deployment of this plugin governs issuance for exactly one instrument, fixed at setup time. If the governance party issues several tokens, each gets its own plugin deployment with its own `IssuanceConfig`. Trade-off: more deployment overhead when several tokens are in play, in exchange for cleaner per-instrument state and simpler per-proposal schemas.

#### Token UX is decided at setup

Display name, symbol, decimals are inputs to `SetupIssuanceProposal`, set once by the committee, and recorded on `IssuanceConfig`. `RegistrarService_CreateInstrumentConfiguration` itself accepts only `instrumentId` and identifier / requirements lists — it does not carry wallet-display metadata.

*Open implementation detail:* whether the display metadata also needs to be exposed to generic Splice wallets (e.g. by writing it into the `AllocationFactory`'s `Metadata` at onboarding time) depends on the wallet-discoverability model the team targets. The spec currently holds the metadata on `IssuanceConfig` only; the implementation plan will decide whether to propagate it into Splice-visible places as well.

#### Amount source: trusted plaintext

The proposer writes the mint amount on the proposal as a `Decimal` field; the committee verifies against off-chain evidence before confirming. No oracle contract or on-chain amount computation in v1.

#### Replay protection: committee diligence only

No `ProcessedEventLog` contract or audit-log scanning in v1. Committee members are responsible for refusing duplicate proposals for the same external event. If double-execution becomes a real problem in practice, a log can be added later — see the appendix ["Sketch of on-chain replay protection"](#appendix-sketch-of-on-chain-replay-protection-if-we-decide-we-need-it) for what that would concretely look like and the associated contention / growth trade-offs.

#### No on-chain supply accounting

Ground-truth supply is the sum of live `Holding`s for the token instrument. The per-execution `GovernanceExecutionResult` records produced by `governance-core` are the natural supply event log. No running supply contract analogous to canton-vault's `YieldEpoch`.

#### No plugin-specific audit record

The `GovernanceExecutionResult` that `governance-core` already emits per execution (`actionLabel`, `description`, `confirmers`, `executedAt`) is the plugin's audit trail. No additional structured fields (event id, recipient, amount) captured in a plugin template.

#### Advanced feature: pause / resume

Beyond strict minimalism; recommended. `IssuanceConfig` carries a `paused : Bool`. `MintProposal.executeImpl` and `BurnProposal.executeImpl` check it and fail if paused. A `SetPauseProposal { newPaused : Bool }` template lets the committee toggle it.

*Why include it:* gives the committee an explicit, on-chain "we are stopped" state. External systems (wallets, dashboards, backend integrations, off-chain attestation pipelines) can observe it rather than infer. Converts ongoing per-proposal vigilance into one committed action, which is also reversible. The minimalist alternative — committee members refusing to confirm during incidents — works but relies on out-of-band signalling and per-proposal attendance.

#### Advanced feature: factory rotation (conditional)

Beyond strict minimalism; recommended if factory rotation is a realistic operational scenario. A `RotateFactoryProposal { newFactoryCid : ContractId BurnMintFactory }` template archives the current `IssuanceConfig` and recreates it with the new `allocationFactoryCid`, leaving every other field on `IssuanceConfig` unchanged (`instrumentId`, `paused`, instrument-UX metadata). `executeImpl` validates that `(view newFactory).admin == governanceParty` via `BurnMintFactory_PublicFetch` before recreating.

*Why include it:* if the Utility Registry ever issues a replacement factory for the same registrar (version upgrade, migration), rotating in place is far cheaper than redeploying the plugin. The instrument's identity on the Splice `InstrumentConfiguration` is untouched, so wallets and external observers tracking the token by `InstrumentId` see no change. Callers that held the old `IssuanceConfig` ContractId must re-acquire it via the identification pattern described in the schema section below. The minimalist alternative — redeploy the plugin — is disproportionately expensive for what is structurally a small config change.

*Open confirmation for the team:* whether factory rotation is a realistic operational scenario at all; if factories are effectively immutable once created, this template is dead code and should be dropped.

### Technical implementation decisions

Internal structure that follows from the product decisions above and from the `GovernableAction` plugin pattern in `governance-core`.

#### Two plugin templates: `MintProposal` and `BurnProposal`

Each wraps one call to `BurnMintFactory_BurnMint` — in mint shape (empty inputs, non-empty outputs) or burn shape (non-empty inputs, empty outputs) respectively. A single combined template would need an awkward internal mint-vs-burn switch; two templates give the committee clearer intent at proposal time and cleaner validation per action.

#### Factory cid stored on a shared `IssuanceConfig` contract

An `IssuanceConfig` contract, signed by the governance party, holds the `allocationFactoryCid` and instrument metadata. Proposers reference the config by ContractId rather than repeating the factory cid on every proposal.

#### Setup runs the whole onboarding chain in one `SetupIssuanceProposal`

Given the product-level decision that setup is governance-driven, the implementation wraps the Utility-Registry onboarding — `ProviderService_CreateProviderConfiguration` → `ProviderService_AcceptRegistrarServiceRequest` → `RegistrarService_CreateAllocationFactory` → `RegistrarService_CreateInstrumentConfiguration` — in a *single* `SetupIssuanceProposal` plugin template that implements `GovernableAction`. One committee vote runs the whole chain and produces the `IssuanceConfig` contract in the same transaction, mirroring canton-vault's [`VaultGovernanceRules_SetupUtility`](https://github.com/DLC-link/canton-vault/blob/main/vault-daml/governance/daml/BitsafeVault/VaultGovernance.daml#L469-L514). This is a deliberate departure from the usual "one plugin template wraps exactly one external Daml operation" rule (see [GOVERNANCE_PLUGIN_ARCHITECTURE.md](GOVERNANCE_PLUGIN_ARCHITECTURE.md)): one governance action is cleaner than four for what is operationally a single decision.

**Prerequisites.** `SetupIssuanceProposal.executeImpl` consumes an existing `ProviderService` and `UserService` — it does not create them. These utility-registry contracts must be provisioned for the governance party before setup can run, e.g. via the existing `governance-token-custody` plugin's `UtilityCreateProviderRequest` / `UtilityCreateUserRequest`, or an equivalent path.

**Input fields on `SetupIssuanceProposal`.** `providerServiceCid : ContractId ProviderService`, `userServiceCid : ContractId UserService`, `operator : Party` (the Splice utility-registry operator party — the party representing the registry app; not a human role), `instrumentIdText : Text` (the `Text` half of the eventual `InstrumentId`; the `admin` half is the governance party), the token-UX metadata (display name, symbol, decimals), and any issuer/holder requirements lists the registry needs. The specific schema is finalised in the implementation plan.

#### `IssuanceConfig` schema

Fields: `governanceParty : Party` (signatory), `instrumentId : InstrumentId`, `allocationFactoryCid : ContractId BurnMintFactory`, `paused : Bool`, plus the instrument-UX metadata set at setup time. Exactly one `IssuanceConfig` should exist per plugin deployment, for the plugin's lifetime — see the one-shot setup note below for how that is enforced. `paused` is `False` at setup.

**Identification pattern.** Daml 3 / LF 2.x removed contract keys, so `IssuanceConfig` is identified purely by `ContractId`. Proposals that operate on the config (pause, rotate) pass its current cid as a field; `executeImpl` fetches by cid and validates the fetched payload matches expectations (the `fetchChecked` idiom from `splice-util`) — in particular that its `governanceParty` equals the proposal's. Off-chain callers locate the current config by querying the ACS for `IssuanceConfig` contracts; the filter is `(governanceParty, instrumentId)`, not `governanceParty` alone, because a single governance party running multiple single-instrument plugin deployments will have several `IssuanceConfig` contracts live at once, one per instrument. There is no on-chain lookup primitive.

#### `MintProposal` and `BurnProposal` carry no instrument selector

They reference the `IssuanceConfig` by ContractId. `executeImpl` fetches the config, reads the `instrumentId` and `allocationFactoryCid`, and fails fast if `paused == True`. Validation: the config's `governanceParty` must match the proposal's `governanceParty`.

**Passed arguments to `BurnMintFactory_BurnMint`.** `expectedAdmin = governanceParty` (read from the config). `extraArgs.meta` carries the proposal's `description` under the standard Splice reason key (`splice.lfdecentralizedtrust.org/reason`) so the reason surfaces in wallet UIs that parse factory metadata. `extraArgs.context` is empty unless a specific factory implementation requires otherwise.

#### One output per `MintProposal`

Each `MintProposal` has exactly one `BurnMintOutput`. Under the treasury-first proposal (see "Proposals requiring team consensus" above) the single recipient is always the governance party, so batching has no effect anyway; even absent treasury-first, one-per-proposal keeps committee review focused on one mint at a time.

#### Burn input holdings are supplied by the proposer

Under the treasury-only burn proposal, the governance party owns the holdings to be burnt. The proposer drafts `BurnProposal` with a `holdingCids : [ContractId Holding]` field that they populate by querying the governance party's active-contract set (via their own participant, which hosts `governanceParty` with read rights) and selecting cids of the chosen instrument. `executeImpl` passes the list directly to `BurnMintFactory_BurnMint.inputHoldingCids`. Validation happens inside the factory implementation: the factory-level `expectedAdmin == (view this).admin` check, and the per-holding `instrumentId == supplied instrumentId` check. The plugin does not maintain any treasury inventory contract — the governance party's holdings are simply read from the ledger when needed.

#### External-event metadata in the `description` field

Evidence of the off-ledger event that justifies each mint or burn (bridge tx hash, oracle quote id, bank wire ref, etc.) goes into the free-form `description : Text` field of `GovernableActionView`. It is surfaced on the `GovernanceExecutionResult` audit record. No typed event-id field on the proposal template in v1; structured metadata can be added later if needed.

#### Proposer: committee members only

With treasury-first mint and treasury-only burn proposed (pending team consensus), all proposals are committee-initiated; the plugin does not need to accommodate non-member proposers. (Contingent on those two proposals — if user-initiated redemption is wanted later, this would need to relax.)

#### `SetupIssuanceProposal` is one-shot (enforced by committee diligence)

Without contract keys, a choice body in Daml 3 / LF 2.x cannot scan the ACS to discover whether an `IssuanceConfig` already exists — so there is no cheap on-chain check that a second setup is a duplicate. Enforcement is therefore off-chain: the committee is responsible for refusing to confirm a second `SetupIssuanceProposal` after the first has executed. Consistent with other off-chain-discipline decisions in this plugin (replay protection, event-id uniqueness). A dedicated "setup marker" template that setup would archive is possible if stronger enforcement becomes necessary, but adds a bootstrap step we have otherwise kept out of the plugin.

#### `SetPauseProposal` and `RotateFactoryProposal` archive-and-recreate `IssuanceConfig`

Both administrative templates mutate the config by the standard Daml idiom: they carry `issuanceConfigCid : ContractId IssuanceConfig` as a proposal field. `executeImpl` fetches the contract (using `fetchChecked` or an equivalent verification that the fetched `governanceParty` matches the proposal's), archives it, and creates a new one with the changed field (`paused` in one case, `allocationFactoryCid` in the other). All other fields are copied over unchanged. The recreated `IssuanceConfig` has a new ContractId; holders re-acquire it via the identification pattern (see schema section above). The instrument identity on the Splice `InstrumentConfiguration` is untouched, so external observers tracking the instrument by `InstrumentId` see no change.

**In-flight proposals.** `MintProposal` and `BurnProposal` contracts already on the ledger point at the old `IssuanceConfig` cid; after a pause or rotate, those cids are archived and the pending proposals will fail at execute time when `executeImpl` tries to fetch the config. The committee must re-draft any pending mint or burn proposals against the new config cid. Not a defect — just an operational consequence of archive-and-recreate without keys, worth naming so it's not a surprise during incident response.

#### `actionLabel` values

`"SetupIssuance"`, `"Mint"`, `"Burn"`, `"SetPause"`, `"RotateFactory"`. Short and human-readable; they surface on `GovernanceExecutionResult` records and in any UI.

---

## Next step

Once the two proposals above are confirmed (or replaced), the next artefact is an implementation plan: concrete template fields and choices for `IssuanceConfig`, `SetupIssuanceProposal`, `MintProposal`, `BurnProposal`, `SetPauseProposal`, and (conditionally) `RotateFactoryProposal`; `executeImpl` bodies; and a test plan.

---

## Appendix: sketch of on-chain replay protection (if we decide we need it)

The "Replay protection: committee diligence only" decision above leaves open the possibility of adding a `ProcessedEventLog` later. This appendix sketches what that would look like, so the team can judge the trade-off concretely.

### Template

```daml
template ProcessedEventLog
  with
    governanceParty : Party
    instrumentId : InstrumentId  -- scoped per plugin deployment, parallel to IssuanceConfig
    processedEventIds : Set Text
  where
    signatory governanceParty

    choice ProcessedEventLog_Record : ContractId ProcessedEventLog
      with eventId : Text
      controller governanceParty
      do
        require "event already processed"
          (not (Set.member eventId processedEventIds))
        create this with
          processedEventIds = Set.insert eventId processedEventIds
```

### Integration with mint and burn

- `MintProposal` and `BurnProposal` each gain two fields: `eventId : Text` and `processedEventLogCid : ContractId ProcessedEventLog`.
- Inside each `executeImpl`, before calling `BurnMintFactory_BurnMint`, exercise `ProcessedEventLog_Record` on the log with the proposal's `eventId`. The record choice fails fast (with `"event already processed"`) if the id is already in the set, which aborts the whole execution.
- The choice returns the new log cid; it can be discarded locally (the next proposer re-acquires via ACS).

### Setup and lifecycle

- `SetupIssuanceProposal.executeImpl` creates a fresh `ProcessedEventLog` alongside `IssuanceConfig`, with empty `processedEventIds`.
- `ProcessedEventLog` is located off-chain via an ACS query on `(governanceParty, instrumentId)` — the same pattern as `IssuanceConfig`.
- `SetPauseProposal` and `RotateFactoryProposal` do not touch the log; it persists across config changes.

### Trade-offs (recap)

- **Serialization bottleneck.** Every mint or burn archives-and-recreates the log. Concurrent proposals contend; later ones see a stale cid and have to re-draft with the fresh cid. Acceptable at low-frequency issuance, degrading as throughput rises.
- **Unbounded growth.** `processedEventIds` grows without bound. Needs an eventual cleanup story — e.g. a "roll the log" action that migrates old entries into a history contract. Additional surface.
- **Schema churn.** Every mint / burn proposal carries two more fields.
- **UX.** Proposers have to include a fresh log cid on each proposal; if another member's proposal just executed, the cid they have is stale.

### When this is actually worth adding

- Multiple committee members draft mint / burn proposals against the same event feed without coordinated off-chain deduplication.
- The attestation protocol (the out-of-scope prerequisite) exposes event ids in a way that can be forged or reused and the committee cannot reliably catch duplicates by inspection.
- Regulatory or audit requirements demand on-chain evidence that each external event was processed at most once.

If none of those apply, committee diligence (cross-referencing the proposal's `description` against recent `GovernanceExecutionResult` records) gives equivalent safety without the contention and growth costs.

---

## Appendix: what `extraActors` means

`extraActors : [Party]` is a field on `BurnMintFactory_BurnMint` (and the analogous Splice factory choices). It lets the implementation require **additional signatures** beyond the instrument admin's.

### At the Daml-interface level

Look at the controller declaration on the choice ([`BurnMintV1.daml:55`](https://github.com/hyperledger-labs/splice/blob/main/daml/splice-api-token-burn-mint-v1/daml/Splice/Api/Token/BurnMintV1.daml#L55)):

```daml
controller (view this).admin :: extraActors
```

In Daml, the controller of a choice is the set of parties whose authority must be present when the choice is exercised. Here that set is the instrument admin (always required, drawn from the factory's view) *plus* whatever parties the caller put in `extraActors`. If any party in the resulting set does not have authority in scope, the exercise fails.

### Why the interface exposes this

The Splice interface keeps admin authority and *owner consent* separable by design. The module comment spells it out:

> *"Note that this is jointly authorized by the admin and the `extraActors`. The `admin` thus controls all calls to this choice, and some implementations might require `extraActors` to be present, e.g., the owners of the minted and burnt holdings."*

The interface says nothing about who `extraActors` has to be. The **implementation** (e.g., the utility-registry `AllocationFactory`) decides what it demands — in practice, typically the owners of the new (mint) or existing (burn) holdings, to prevent the admin unilaterally creating or destroying holdings belonging to other parties.

### How canton-vault uses it

- **Mint** (`DepositRequest_Process`): `extraActors = [user]`. The new share's owner is the user; the user must co-authorize so the admin (vault manager) can't mint shares to someone without their consent.
- **Burn** (`WithdrawRequest_Process`): `extraActors = []`. The burn consumes holdings owned by `vaultManager` — who is already the admin — so no additional party needs to authorize. (The shares had moved into `vaultManager`'s custody via the preceding `TransferInstruction_Accept` step.)

### How this plugin uses it

Because of treasury-first mint and treasury-only burn (see "Proposals requiring team consensus"):

- **Mint** (`MintProposal.executeImpl`): `extraActors = []`. The recipient is the governance party itself — the same party as the admin — so no additional authority is needed.
- **Burn** (`BurnProposal.executeImpl`): `extraActors = []`. Same reason — the burnt holdings belong to the governance party, already the admin.

This is exactly why the treasury-first / treasury-only pair is so clean architecturally: it collapses every `extraActors` question into "nobody else needs to sign." If we ever drop treasury-first (e.g., mint directly to a third party), `extraActors = [recipient]` re-emerges, and the plugin has to find a way to get that recipient's authority into `executeImpl` — which is the whole point of the first proposal requiring consensus.

### Summary

| Field | Meaning | Who supplies it | Enforced by |
|---|---|---|---|
| `expectedAdmin` | The admin party the caller expects (defends against malicious substitute factories) | Caller | Factory impl validates `expectedAdmin == (view this).admin` |
| `extraActors` | Additional parties whose authority is required alongside the admin | Caller | Daml's authorization rules (controller must have authority in scope); factory impl *may* demand a specific shape |
| (factory's `admin`) | The instrument admin, always required | Stored on the factory | Daml's authorization rules |

In short: `extraActors` is how the factory interface lets *anyone else's consent* into the mint/burn operation without baking a specific "who" into the interface. Implementations make it concrete.
