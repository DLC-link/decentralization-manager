# Token Issuance Plugin — Design

**Status:** design agreed with the team. Ready to proceed to the implementation plan.

**Terminology note.** Strictly, "issuance" means the creation of new tokens (minting) — burning is its opposite, and "supply management" is the umbrella term covering both. This document uses "issuance" colloquially, following the name of the plugin, to refer to the full mint + burn lifecycle that the plugin governs. Wherever the distinction matters, the specific operation ("mint" or "burn") is named explicitly.

## Overview

A Daml package that plugs into `governance-core`. Each plugin template implements the `GovernableAction` interface (see [GOVERNANCE_PLUGIN_ARCHITECTURE.md](GOVERNANCE_PLUGIN_ARCHITECTURE.md)) and wraps a privileged Daml operation: the mint and burn templates call `AllocationFactory_OfferMint` / `AllocationFactory_OfferBurn` from `utility-registry-app-v0` (creating offer contracts the recipient / holder later accepts); the setup template wraps the Utility-Registry onboarding chain (`ProviderService` / `RegistrarService` calls); the factory-rotation template exercises an update choice on `IssuanceConfig`.

**Goal.** Let a decentralized governance party (the signatory on `GovernanceRules`) mint and burn its own token instrument, with each mint or burn gated by a threshold of committee confirmations.

**Contrast with `canton-vault`.** Canton-vault bundles each mint with an atomic asset swap, so Canton transaction validation alone guarantees the economic invariant (see [TOKEN_ISSUANCE_IN_CANTON_VAULT.md](TOKEN_ISSUANCE_IN_CANTON_VAULT.md)). This plugin has **no swap**. Each mint or burn is an independent privileged action. The committee attests off-ledger to whatever event justifies it and votes on each individual issuance. The underlying Splice / Utility-Registry primitives are the same ones canton-vault uses (the `BurnMintFactory_BurnMint` choice, the instrument-admin role, the `AllocationFactory` setup); this plugin just sits on top at a higher level — calling `AllocationFactory_OfferMint` / `OfferBurn` rather than `BurnMintFactory_BurnMint` directly — so that the primitives are invoked only at accept time when the recipient or holder concurs. What changes vs. canton-vault is that there is no on-ledger trigger and the authority for each proposal comes from a governance confirmation threshold instead of a user's `TransferInstruction_Accept`.

---

## Out-of-scope prerequisite: attestation protocol

How committee members learn about the external event they're voting on — a bridge oracle, a signed attestation chain, a manual evidence process — is out of scope for the plugin itself, but the plugin assumes *some* such protocol exists. The structure of the evidence it delivers determines whether the free-form `description` field (the current v1 default) is enough or whether we need typed fields on `MintProposal` / `BurnProposal`. Choosing that protocol is a parallel decision that should happen alongside this plugin design, before any structured proposal schema is fixed.

---

## Decisions

Items agreed with the team.

**Product-level:**
- [Mint via `AllocationFactory_OfferMint`](#mint-via-allocationfactory_offermint)
- [Burn via `AllocationFactory_OfferBurn`](#burn-via-allocationfactory_offerburn)
- [Setup runs through the governance committee, not as an out-of-band script](#setup-runs-through-the-governance-committee-not-as-an-out-of-band-script)
- [Single-instrument plugin deployment](#single-instrument-plugin-deployment)
- [Token UX is decided at setup](#token-ux-is-decided-at-setup)
- [Amount source: trusted plaintext](#amount-source-trusted-plaintext)
- [Replay protection: committee diligence only](#replay-protection-committee-diligence-only)
- [No on-chain supply accounting](#no-on-chain-supply-accounting)
- [No plugin-specific audit record](#no-plugin-specific-audit-record)
- [Pause / resume — considered, not included](#pause--resume--considered-not-included)
- [Factory rotation](#factory-rotation)

**Technical implementation:**
- [Two plugin templates: `MintProposal` and `BurnProposal`](#two-plugin-templates-mintproposal-and-burnproposal)
- [Setup runs the whole onboarding chain in one `SetupIssuanceProposal`](#setup-runs-the-whole-onboarding-chain-in-one-setupissuanceproposal)
- [`IssuanceConfig` schema](#issuanceconfig-schema)
- [`MintProposal` and `BurnProposal` carry no instrument selector](#mintproposal-and-burnproposal-carry-no-instrument-selector)
- [One Mint per `MintProposal`, one Burn per `BurnProposal`](#one-mint-per-mintproposal-one-burn-per-burnproposal)
- [Burn target holdings are supplied by the proposer](#burn-target-holdings-are-supplied-by-the-proposer)
- [External-event metadata in the `description` field](#external-event-metadata-in-the-description-field)
- [Proposer: committee members only](#proposer-committee-members-only)
- [`SetupIssuanceProposal` is one-shot (enforced by committee diligence)](#setupissuanceproposal-is-one-shot-enforced-by-committee-diligence)
- [`IssuanceConfig` owns its own update choice](#issuanceconfig-owns-its-own-update-choice)
- [`actionLabel` values](#actionlabel-values)

### Product-level decisions

Scope, policy, and feature choices. These define what the plugin does and doesn't do.

#### Mint via `AllocationFactory_OfferMint`

**Decision.** `MintProposal.executeImpl` calls `AllocationFactory_OfferMint` (controller: `registrar`, i.e. the governance party; defined in `utility-registry-app-v0-0.7.0`, module `Utility.Registry.App.V0.Service.AllocationFactory`, lines 479–494) to create a `MintOffer` contract addressed to the intended recipient. The recipient later exercises the accept choice on `MintOffer` to materialise the new holding. `AllocationFactory_RequestMint` (controller: `mint.holder`) is the reverse direction — the recipient requests a mint and the committee approves off the resulting `MintRequest`. The plugin's v1 shape is **OfferMint-first**, i.e. committee-initiated offers; `RequestMint` approval can be added later as its own proposal template if needed.

**Why.** Both are standard utility-registry templates, so we do not need a custom `MintPreapproval` or any treasury indirection. The recipient's authority enters at accept time, not at propose or execute time — solving the recipient-authority problem that `BurnMintFactory_BurnMint` alone imposes (see the appendix ["What `extraActors` means"](#appendix-what-extraactors-means) for the underlying mechanism).

**Trade-off.** Minting is a two-step flow: committee offers, recipient accepts. Not atomic. If the recipient never accepts (lost keys, abandonment), the `MintOffer` sits on the ledger until it expires or is withdrawn — standard CIP-56 flow. The plugin does not model the accept step; that happens on the recipient side with no further governance involvement.

**Alternative considered.** Treasury-first mint — mint into a governance-party-owned pool via `BurnMintFactory_BurnMint`, deliver to the final recipient in a separate governance action through the custody plugin's `TransferProposal`. Rejected: two governance votes per delivery, and a runtime dependency on the custody plugin being deployed and configured with the same `AllocationFactory` cid.

#### Burn via `AllocationFactory_OfferBurn`

**Decision.** Symmetric with mint. `BurnProposal.executeImpl` calls `AllocationFactory_OfferBurn` (controller: `registrar`, i.e. the governance party; same source module as above) to create a `BurnOffer` contract addressed to a specific holder. The holder later accepts, at which point their holdings are burnt. `AllocationFactory_RequestBurn` (controller: `burn.holder`) is the reverse direction — a holder requests their own holdings be burnt and the committee approves off the resulting `BurnRequest`. v1 shape is **OfferBurn-first**; `RequestBurn` approval can be added later.

**Why.** Standard utility-registry templates; no custom contract; holder authority lands at accept time, same pattern as mint.

**Trade-off.** Burn requires the holder's cooperation — the committee cannot unilaterally destroy holdings without the holder accepting the offer. This is a feature for token-as-bearer-asset models (the governance party cannot arbitrarily seize someone's balance), but it means burns for uncooperative holders are not possible through this path.

**Treasury-retirement gap.** Retiring governance-party-owned tokens (holder == admin == `governanceParty`) is **not** covered by the plugin as specified: `OfferBurn` creates a `BurnOffer` that still requires an accept step, and the plugin currently has no `AcceptBurnOfferProposal` template to governance-gate that accept. If treasury retirement is a real v1 need, two ways forward: (i) add an `AcceptBurnOfferProposal` template — two governance votes per retirement but stays in the OfferBurn model; (ii) add a direct-burn path via `BurnMintFactory_BurnMint` with `extraActors = []` alongside OfferBurn — one vote, works only for governance-owned tokens, no user-redemption. Not in v1 scope today; flag for the team to confirm.

#### Setup runs through the governance committee, not as an out-of-band script

The Utility-Registry onboarding that produces the `AllocationFactory` and registers the instrument is performed under committee control, via a governance-gated proposal, rather than by a sysadmin running a one-off deployment script with the right keys. This keeps the full plugin lifecycle (setup → mint / burn → rotate) within the same governance committee that signs the governance party.

(Unrelated: the Splice utility-registry has its own concept of an "operator" *Party* — a Daml party that represents the registry app itself and appears as a co-signatory on `ProviderService`, `RegistrarService`, etc. That operator party shows up as a field on `SetupIssuanceProposal` in the technical section below. The "operator-driven" we were ruling out here is the sysadmin sense, not the Splice operator-party sense.)

#### Single-instrument plugin deployment

Each deployment of this plugin governs issuance for exactly one instrument, fixed at setup time. If the governance party issues several tokens, each gets its own plugin deployment with its own `IssuanceConfig`. Trade-off: more deployment overhead when several tokens are in play, in exchange for cleaner per-instrument state and simpler per-proposal schemas.

#### Token UX is decided at setup

Display name, symbol, decimals are inputs to `SetupIssuanceProposal`, set once by the committee, and recorded on `IssuanceConfig`. `RegistrarService_CreateInstrumentConfiguration` accepts only `instrumentId` and identifier / requirements lists — it does not carry wallet-display metadata.

*On wallet discoverability.* The utility-registry's `AllocationFactory` implementation hardcodes `meta = emptyMetadata` in all three of its interface views (`AllocationFactoryView`, `BurnMintFactoryView`, `TransferFactoryView` in `Utility.Registry.App.V0.Service.AllocationFactory`), so there is no on-chain factory-level slot for display metadata that a generic Splice-aware wallet could read. Display metadata therefore lives on `IssuanceConfig` only — matching canton-vault's equivalent pattern (name/symbol on `VaultConfig`). Bespoke UIs (backend, committee UI) read it from there; generic wallets will see raw instrument ids until an off-chain distribution mechanism (e.g. a wallet-facing manifest) is added, which is a separate concern outside this plugin.

#### Amount source: trusted plaintext

The proposer writes the mint amount on the proposal as a `Decimal` field; the committee verifies against off-chain evidence before confirming. No oracle contract or on-chain amount computation in v1.

#### Replay protection: committee diligence only

No `ProcessedEventLog` contract or audit-log scanning in v1. Committee members are responsible for refusing duplicate proposals for the same external event. If double-execution becomes a real problem in practice, a log can be added later — see the appendix ["Sketch of on-chain replay protection"](#appendix-sketch-of-on-chain-replay-protection-if-we-decide-we-need-it) for what that would concretely look like and the associated contention / growth trade-offs.

#### No on-chain supply accounting

Ground-truth supply is the sum of live `Holding`s for the token instrument. The per-execution `GovernanceExecutionResult` records produced by `governance-core` are the natural supply event log. No running supply contract analogous to canton-vault's `YieldEpoch`.

#### No plugin-specific audit record

The `GovernanceExecutionResult` that `governance-core` already emits per execution (`actionLabel`, `description`, `confirmers`, `executedAt`) is the plugin's audit trail. No additional structured fields (event id, recipient, amount) captured in a plugin template.

#### Pause / resume — considered, not included

We considered adding a governance-level pause toggle: a `paused : Bool` flag on `IssuanceConfig` checked by `MintProposal` and `BurnProposal` and toggled via a `SetPauseProposal { newPaused : Bool }` template. **Decision: not included in v1.**

*Rationale.* The committee can already halt issuance during an incident by refusing to confirm incoming proposals — the same off-chain-discipline mechanism the plugin already relies on for replay protection, one-shot setup, and duplicate-event detection. A dedicated pause toggle would duplicate that discipline without a compelling added benefit in v1.

#### Factory rotation

The committee can replace the `AllocationFactory` cid on `IssuanceConfig` via a `RotateFactoryProposal` without redeploying the plugin. Needed if the Utility Registry ever issues a replacement factory for the same registrar (version upgrade, migration) — rotating in place is far cheaper than redeploying the plugin, and the instrument's identity on the Splice `InstrumentConfiguration` is untouched, so wallets and external observers tracking the token by `InstrumentId` see no change. Implementation (the choice on `IssuanceConfig`, the proposal wrapper, and cost discussion) is in the technical section below.

### Technical implementation decisions

Internal structure that follows from the product decisions above and from the `GovernableAction` plugin pattern in `governance-core`.

#### Two plugin templates: `MintProposal` and `BurnProposal`

`MintProposal.executeImpl` calls `AllocationFactory_OfferMint`; `BurnProposal.executeImpl` calls `AllocationFactory_OfferBurn`. A single combined template would need an awkward internal mint-vs-burn switch; two templates give the committee clearer intent at proposal time and cleaner validation per action.

#### Setup runs the whole onboarding chain in one `SetupIssuanceProposal`

Given the product-level decision that setup is governance-driven, the implementation wraps the Utility-Registry onboarding — `ProviderService_CreateProviderConfiguration` → `ProviderService_AcceptRegistrarServiceRequest` → `RegistrarService_CreateAllocationFactory` → `RegistrarService_CreateInstrumentConfiguration` — in a *single* `SetupIssuanceProposal` plugin template that implements `GovernableAction`. One committee vote runs the whole chain and produces the `IssuanceConfig` contract in the same transaction, mirroring canton-vault's [`VaultGovernanceRules_SetupUtility`](https://github.com/DLC-link/canton-vault/blob/main/vault-daml/governance/daml/BitsafeVault/VaultGovernance.daml#L469-L514). This is a deliberate departure from the usual "one plugin template wraps exactly one external Daml operation" rule (see [GOVERNANCE_PLUGIN_ARCHITECTURE.md](GOVERNANCE_PLUGIN_ARCHITECTURE.md)): one governance action is cleaner than four for what is operationally a single decision.

**Prerequisites.** `SetupIssuanceProposal.executeImpl` consumes an existing `ProviderService` — it does not create one. This utility-registry contract must be provisioned for the governance party before setup can run, e.g. via a `UtilityCreateProviderRequest` flow added to the existing `governance-token-custody` plugin, or an equivalent path. (V1 uses `registrarRequirements = []` and therefore does not need a `UserService` / credentials side-car; if a later version tightens the registry's requirements, see canton-vault's [`VaultGovernanceRules_SetupUtility`](https://github.com/DLC-link/canton-vault/blob/main/vault-daml/governance/daml/BitsafeVault/VaultGovernance.daml#L469-L514) for the pattern that pulls in a `UserService` from `utility-credential-app` and the associated credential-offer / credential-accept choices.)

**Input fields on `SetupIssuanceProposal`.** `providerServiceCid : ContractId ProviderService`, `operator : Party` (the Splice utility-registry operator party — the party representing the registry app; not a human role), `instrumentIdText : Text` (the `Text` half of the eventual `InstrumentId`; the `admin` half is the governance party), the token-UX metadata (display name, symbol, decimals), and any issuer/holder requirements lists the registry needs. The specific schema is finalised in the implementation plan.

#### `IssuanceConfig` schema

Fields: `governanceParty : Party` (signatory), `instrumentId : InstrumentId`, `allocationFactoryCid : ContractId AllocationFactory`, `instrumentConfigurationCid : ContractId InstrumentConfiguration`, plus the instrument-UX metadata set at setup time. Exactly one `IssuanceConfig` should exist per plugin deployment, for the plugin's lifetime — see the one-shot setup note below for how that is enforced. (The cid is stored as the concrete `AllocationFactory` template type because the plugin calls template-level choices `AllocationFactory_OfferMint` / `AllocationFactory_OfferBurn` on it; the same contract can always be cast to the `BurnMintFactory` or `TransferFactory` interfaces when needed. The `instrumentConfigurationCid` is kept because `AllocationFactory_OfferMint` / `OfferBurn` require the current `InstrumentConfiguration` via `extraArgs.context[instrumentConfigurationContextKey]`, and there is no on-chain lookup primitive to recover it from the allocation factory alone.)

Choices (currently just one; future mutation actions would be added here):
- `IssuanceConfig_RotateFactory { newFactoryCid : ContractId AllocationFactory }` — controller `governanceParty`. Validates the new factory's admin, archives `this`, creates a fresh `IssuanceConfig` with all other fields preserved. Called by `RotateFactoryProposal` (see below).

**Identification pattern.** `IssuanceConfig` is identified by `ContractId`. Proposals that operate on the config (rotate) pass its current cid as a field; `executeImpl` fetches by cid and validates the fetched payload matches expectations (the `fetchChecked` idiom from `splice-util`) — in particular that its `governanceParty` equals the proposal's. Off-chain callers locate the current config by querying the ACS for `IssuanceConfig` contracts; the filter is `(governanceParty, instrumentId)`, not `governanceParty` alone, because a single governance party running multiple single-instrument plugin deployments will have several `IssuanceConfig` contracts live at once, one per instrument. There is no on-chain lookup primitive.

#### `MintProposal` and `BurnProposal` carry no instrument selector

They reference the `IssuanceConfig` by ContractId. `executeImpl` fetches the config and reads the `instrumentId`, `allocationFactoryCid`, and `instrumentConfigurationCid`. The `instrumentConfigurationCid` is placed under `extraArgs.context[instrumentConfigurationContextKey]` on the `AllocationFactory_OfferMint` / `OfferBurn` call; `issuerCredentialsContextKey` carries `[] : [ContractId Credential]` given v1's empty `issuerRequirements`. Validation: the config's `governanceParty` must match the proposal's `governanceParty`.

**Passed arguments to `AllocationFactory_OfferMint` / `AllocationFactory_OfferBurn`.** `expectedAdmin = governanceParty` (read from the config). The proposal-specific payload is a `Mint` (for mint) or `Burn` (for burn) record built by `executeImpl` from the proposal's fields — recipient party, amount, instrument id (from the config), any deadlines. `extraArgs.meta` carries the proposal's `description` under the standard Splice reason key (`splice.lfdecentralizedtrust.org/reason`) so the reason surfaces in wallet UIs that parse factory metadata.

#### One Mint per `MintProposal`, one Burn per `BurnProposal`

Each mint proposal produces exactly one `MintOffer` for one recipient; each burn proposal produces exactly one `BurnOffer` against one holder. No batching in v1; keeps committee review focused on one recipient / holder at a time.

#### Burn target holdings are supplied by the proposer

The proposer drafts `BurnProposal` with the holder party and the holdings to burn (the shape matches the `Burn` record expected by `AllocationFactory_OfferBurn`). They obtain the candidate holding cids by querying the holder's active-contract set (or the governance party's, for treasury retirement) and selecting cids of the chosen instrument. `executeImpl` packages them into the `Burn` record and calls `AllocationFactory_OfferBurn`. Validation is the factory implementation's job — it checks the admin, the instrument, and produces a `BurnOffer` for the holder to accept. The plugin does not maintain any treasury inventory contract.

#### External-event metadata in the `description` field

Evidence of the off-ledger event that justifies each mint or burn (bridge tx hash, oracle quote id, bank wire ref, etc.) goes into the free-form `description : Text` field of `GovernableActionView`. It is surfaced on the `GovernanceExecutionResult` audit record. No typed event-id field on the proposal template in v1; structured metadata can be added later if needed.

#### Proposer: committee members only

With OfferMint-first and OfferBurn-first, all v1 proposals are committee-initiated — a committee member drafts a `MintProposal` or `BurnProposal`, the committee votes, a member executes. The plugin does not need to accommodate non-member proposers in v1. If `RequestMint` / `RequestBurn` approval flows are added later (recipients / holders initiate; committee approves off the resulting `MintRequest` / `BurnRequest`), those would be new plugin templates where the proposer's identity is distinct from the committee, and the governance-core flow against a non-member proposer would need explicit validation.

#### `SetupIssuanceProposal` is one-shot (enforced by committee diligence)

A Daml choice body can only operate on contracts whose `ContractId` is passed in, and there is no on-chain "does a contract of this template exist?" primitive. So `SetupIssuanceProposal.executeImpl` has no cheap way to self-detect as a duplicate. Enforcement is therefore off-chain: the committee refuses to confirm a second `SetupIssuanceProposal` after the first has executed. Consistent with other off-chain-discipline decisions in this plugin (replay protection, event-id uniqueness). A dedicated "setup marker" template that setup would archive is possible if stronger enforcement becomes necessary (the marker's absence signals "already used"), but adds a bootstrap step we have otherwise kept out of the plugin.

#### `IssuanceConfig` owns its own update choice

The archive-and-recreate for factory rotation lives on `IssuanceConfig` as a choice `IssuanceConfig_RotateFactory { newFactoryCid : ContractId AllocationFactory }`, controller `governanceParty`. The choice body fetches `this`, validates the new factory's admin, archives, and creates a new `IssuanceConfig` copying every field unchanged except `allocationFactoryCid`.

The plugin-side `RotateFactoryProposal` template is a thin wrapper. It carries `issuanceConfigCid : ContractId IssuanceConfig` and `newFactoryCid : ContractId AllocationFactory` as proposal fields; its `executeImpl` verifies the fetched config's `governanceParty` matches the proposal's (the `fetchChecked` idiom from `splice-util`) and then calls `exercise issuanceConfigCid IssuanceConfig_RotateFactory with newFactoryCid`. Putting the mutation on the state contract rather than in `executeImpl` keeps the "how to update `IssuanceConfig`" knowledge co-located with the template itself; if more mutation choices are added later (e.g. re-introducing pause), they go on `IssuanceConfig` alongside this one.

*Cost of this pattern.* Extra indirection: one more choice body to read through, one more jump in the call stack. The `IssuanceConfig` template file grows with update logic instead of staying a pure record. Worth accepting if we expect more than one mutation action in the plugin's lifetime; for a single mutator (just rotate) it is a minor tax.

The recreated `IssuanceConfig` has a new ContractId; holders re-acquire it via the identification pattern (see schema section above). The instrument identity on the Splice `InstrumentConfiguration` is untouched, so external observers tracking the instrument by `InstrumentId` see no change.

**In-flight proposals.** `MintProposal` and `BurnProposal` contracts already on the ledger point at the old `IssuanceConfig` cid; after a rotate, those cids are archived and the pending proposals will fail at execute time when `executeImpl` tries to fetch the config. The committee must re-draft any pending mint or burn proposals against the new config cid. Not a defect — just an operational consequence of the archive-and-recreate pattern, worth naming so it's not a surprise during a rotation.

#### `actionLabel` values

`"SetupIssuance"`, `"Mint"`, `"Burn"`, `"RotateFactory"`. Short and human-readable; they surface on `GovernanceExecutionResult` records and in any UI.

---

## Next step

The next artefact is an implementation plan: concrete template fields and choices for `IssuanceConfig`, `SetupIssuanceProposal`, `MintProposal`, `BurnProposal`, and (conditionally) `RotateFactoryProposal`; `executeImpl` bodies; and a test plan.

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
- Inside each `executeImpl`, before calling `AllocationFactory_OfferMint` / `AllocationFactory_OfferBurn`, exercise `ProcessedEventLog_Record` on the log with the proposal's `eventId`. The record choice fails fast (with `"event already processed"`) if the id is already in the set, which aborts the whole execution.
- The choice returns the new log cid; it can be discarded locally (the next proposer re-acquires via ACS).

### Setup and lifecycle

- `SetupIssuanceProposal.executeImpl` creates a fresh `ProcessedEventLog` alongside `IssuanceConfig`, with empty `processedEventIds`.
- `ProcessedEventLog` is located off-chain via an ACS query on `(governanceParty, instrumentId)` — the same pattern as `IssuanceConfig`.
- `RotateFactoryProposal` does not touch the log; it persists across config changes.

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

In the v1 OfferMint-first / OfferBurn-first design, `MintProposal` and `BurnProposal` do not call `BurnMintFactory_BurnMint` directly. They call the higher-level `AllocationFactory_OfferMint` / `AllocationFactory_OfferBurn`, which create `MintOffer` / `BurnOffer` contracts. The `extraActors` concern is therefore handled inside the utility-registry's implementation, not inside the plugin's `executeImpl`: when the recipient / holder later exercises the accept choice on the offer, the registry internally performs the underlying mint or burn with the appropriate controller set — the offer-accept flow is what contributes the recipient's / holder's authority.

In the treasury-* alternatives considered and rejected for mint and burn (documented under the Mint and Burn decisions above), the plugin would have called `BurnMintFactory_BurnMint` directly with `extraActors = []`: in those models the governance party is both the admin and the holder of the new (mint) or existing (burn) holding, so no additional signature would be needed.

### Summary

| Field | Meaning | Who supplies it | Enforced by |
|---|---|---|---|
| `expectedAdmin` | The admin party the caller expects (defends against malicious substitute factories) | Caller | Factory impl validates `expectedAdmin == (view this).admin` |
| `extraActors` | Additional parties whose authority is required alongside the admin | Caller | Daml's authorization rules (controller must have authority in scope); factory impl *may* demand a specific shape |
| (factory's `admin`) | The instrument admin, always required | Stored on the factory | Daml's authorization rules |

In short: `extraActors` is how the factory interface lets *anyone else's consent* into the mint/burn operation without baking a specific "who" into the interface. Implementations make it concrete.

---

## Appendix: utility-registry async mint / burn workflows

The `AllocationFactory` template (in `utility-registry-app-v0`) exports a set of `Request*` / `Offer*` choices that provide two-step (async) mint and burn workflows as a core utility-registry feature — no custom `MintPreapproval` contract needed:

- `AllocationFactory_OfferMint` → produces a `MintOffer` contract the recipient accepts on their own time.
- `AllocationFactory_RequestMint` → produces a `MintRequest` contract the admin approves.
- `AllocationFactory_OfferBurn` → analogous for burn (holder-facing offer).
- `AllocationFactory_RequestBurn` → analogous (holder requests their own holdings be burnt).

The async property comes from the two steps being authorized separately: whichever party initiates needs only their own authority to create the offer / request contract; the other party's authority enters later at accept time. See the Mint and Burn decisions above for how the plugin uses the `Offer*` half; the `Request*` half could be added later as its own proposal template if user-initiated redemption or mint-request flows are needed.
