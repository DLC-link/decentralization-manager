# Token Issuance Plugin — Design

**Status:** draft design. All items are proposer-made defaults, open to team review; any can be reopened on request.

**Terminology note.** Strictly, "issuance" means the creation of new tokens (minting) — burning is its opposite, and "supply management" is the umbrella term covering both. This document uses "issuance" colloquially, following the name of the plugin, to refer to the full mint + burn lifecycle that the plugin governs. Wherever the distinction matters, the specific operation ("mint" or "burn") is named explicitly.

## Overview

A Daml package that plugs into `governance-core`. Each plugin template implements the `GovernableAction` interface (see [GOVERNANCE_PLUGIN_ARCHITECTURE.md](GOVERNANCE_PLUGIN_ARCHITECTURE.md)) and wraps a privileged Daml operation: the mint and burn templates call `AllocationFactory_OfferMint` / `AllocationFactory_OfferBurn` from `utility-registry-app-v0` (creating offer contracts the recipient / holder later accepts); the setup template wraps the Utility-Registry onboarding chain (`ProviderService` / `RegistrarService` calls); the factory-rotation template archives-and-recreates `IssuanceConfig`.

**Goal.** Let a decentralized governance party (the signatory on `GovernanceRules`) mint and burn its own token instrument, with each mint or burn gated by a threshold of committee confirmations.

**Contrast with `canton-vault`.** Canton-vault bundles each mint with an atomic asset swap, so Canton transaction validation alone guarantees the economic invariant (see [TOKEN_ISSUANCE_IN_CANTON_VAULT.md](TOKEN_ISSUANCE_IN_CANTON_VAULT.md)). This plugin has **no swap**. Each mint or burn is an independent privileged action. The committee attests off-ledger to whatever event justifies it and votes on each individual issuance. The issuance mechanics (the `BurnMintFactory_BurnMint` primitive, the instrument-admin role, the `AllocationFactory` setup) are the same Splice / Utility-Registry primitives canton-vault uses; what changes is that there is no on-ledger trigger, and the authority for each mint/burn comes from a governance confirmation threshold instead of a user's `TransferInstruction_Accept`.

---

## Out-of-scope prerequisite: attestation protocol

How committee members learn about the external event they're voting on — a bridge oracle, a signed attestation chain, a manual evidence process — is out of scope for the plugin itself, but the plugin assumes *some* such protocol exists. The structure of the evidence it delivers determines whether the free-form `description` field (the current v1 default) is enough or whether we need typed fields on `MintProposal` / `BurnProposal`. Choosing that protocol is a parallel decision that should happen alongside this plugin design, before any structured proposal schema is fixed.

---

## Proposed decisions

Items the proposer has made an initial call on. They are working defaults for the plugin's design and are open to team review; any of them can be reopened on request.

### Product-level decisions

Scope, policy, and feature choices. These define what the plugin does and doesn't do.

#### Mint via `AllocationFactory_OfferMint`

**Decision.** `MintProposal.executeImpl` calls `AllocationFactory_OfferMint` (controller: `registrar`, i.e. the governance party; defined in `utility-registry-app-v0-0.7.0`, module `Utility.Registry.App.V0.Service.AllocationFactory`, lines 479–494) to create a `MintOffer` contract addressed to the intended recipient. The recipient later exercises the accept choice on `MintOffer` to materialise the new holding. `AllocationFactory_RequestMint` (controller: `mint.holder`) is the reverse direction — the recipient requests a mint and the committee approves off the resulting `MintRequest`. The plugin's v1 shape is **OfferMint-first**, i.e. committee-initiated offers; `RequestMint` approval can be added later as its own proposal template if needed.

**Why.** Both are standard utility-registry templates, so we do not need a custom `MintPreapproval` or any treasury indirection. The recipient's authority enters at accept time, not at propose or execute time — solving the recipient-authority problem that `BurnMintFactory_BurnMint` alone imposes (see the appendix ["What `extraActors` means"](#appendix-what-extraactors-means) for the underlying mechanism).

**Trade-off.** Minting is a two-step flow: committee offers, recipient accepts. Not atomic. If the recipient never accepts (lost keys, abandonment), the `MintOffer` sits on the ledger until it expires or is withdrawn — standard CIP-56 flow. The plugin does not model the accept step; that happens on the recipient side with no further governance involvement.

**Alternative considered.** Treasury-first mint — `MintProposal` mints into a treasury pool owned by the governance party itself (`BurnMintOutput.owner = governanceParty`, `extraActors = []`), with delivery to the final recipient as a separate governance action via the custody plugin's `TransferProposal`. Keeps mint atomic-with-governance (no recipient accept dependency) but adds a second governance vote per delivery and depends on the custody plugin being deployed alongside. The two plugins would need to share the same `AllocationFactory` cid rather than each running their own onboarding.

#### Burn via `AllocationFactory_OfferBurn`

**Decision.** Symmetric with mint. `BurnProposal.executeImpl` calls `AllocationFactory_OfferBurn` (controller: `registrar`, i.e. the governance party; same source module as above) to create a `BurnOffer` contract addressed to a specific holder. The holder later accepts, at which point their holdings are burnt. `AllocationFactory_RequestBurn` (controller: `burn.holder`) is the reverse direction — a holder requests their own holdings be burnt and the committee approves off the resulting `BurnRequest`. v1 shape is **OfferBurn-first**; `RequestBurn` approval can be added later.

**Why.** Standard utility-registry templates; no custom contract; holder authority lands at accept time, same pattern as mint.

**Trade-off.** Burn requires the holder's cooperation — the committee cannot unilaterally destroy holdings without the holder accepting the offer. This is a feature for token-as-bearer-asset models (the governance party cannot arbitrarily seize someone's balance), but it means burns for uncooperative holders are not possible through this path. For retirement of governance-party-owned tokens (holder == admin == governanceParty), both OfferBurn and its accept step need the governance party's authority — doable as two separate governance actions, but unwieldy compared to a direct `BurnMintFactory_BurnMint`.

**Alternative considered.** Treasury-only burn — `BurnProposal.executeImpl` calls `BurnMintFactory_BurnMint` directly on holdings owned by the governance party, `extraActors = []`. No holder cooperation needed; works only for governance-owned tokens. Simpler for pure de-issuance of a treasury pool, but no user-redemption path.

#### Setup runs through the governance committee, not as an out-of-band script

The Utility-Registry onboarding that produces the `AllocationFactory` and registers the instrument is performed under committee control, via a governance-gated proposal, rather than by a sysadmin running a one-off deployment script with the right keys. This keeps the full plugin lifecycle (setup → mint / burn → rotate) within the same governance committee that signs the governance party.

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

#### Pause / resume — considered, not included

We considered adding a governance-level pause toggle: a `paused : Bool` flag on `IssuanceConfig` checked by `MintProposal` and `BurnProposal` and toggled via a `SetPauseProposal { newPaused : Bool }` template. **Decision: not included in v1.**

*Rationale.* The committee can already halt issuance during an incident by refusing to confirm incoming proposals — the same off-chain-discipline mechanism the plugin already relies on for replay protection, one-shot setup, and duplicate-event detection. A dedicated pause toggle would duplicate that discipline without a compelling added benefit in v1.

*What would change our mind.* If external systems (wallets, dashboards, backend integrations) need a programmatically-readable on-chain "we are stopped" signal that "no proposals are being confirmed" doesn't provide, or if committee-vigilance failure modes show up in practice, pause can be added back later. The implementation cost is small: one `Bool` field on `IssuanceConfig` and one new proposal template.

#### Advanced feature: factory rotation (conditional)

Beyond strict minimalism; recommended if factory rotation is a realistic operational scenario. Shape: `IssuanceConfig` carries a choice `IssuanceConfig_RotateFactory { newFactoryCid : ContractId AllocationFactory }` (controller: `governanceParty`) that archives `this` and creates a new `IssuanceConfig` with the new `allocationFactoryCid`, leaving every other field unchanged (`instrumentId`, instrument-UX metadata). The choice body validates that the new factory's admin equals `governanceParty` before recreating — e.g. via `BurnMintFactory_PublicFetch` through the `BurnMintFactory` interface. A thin `RotateFactoryProposal { issuanceConfigCid, newFactoryCid }` plugin template wraps the choice: its `executeImpl` is just `exercise issuanceConfigCid IssuanceConfig_RotateFactory with newFactoryCid`. The update logic lives on `IssuanceConfig` itself; the proposal template is the governance-gate.

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

Fields: `governanceParty : Party` (signatory), `instrumentId : InstrumentId`, `allocationFactoryCid : ContractId AllocationFactory`, plus the instrument-UX metadata set at setup time. Exactly one `IssuanceConfig` should exist per plugin deployment, for the plugin's lifetime — see the one-shot setup note below for how that is enforced. (The cid is stored as the concrete `AllocationFactory` template type because the plugin calls template-level choices `AllocationFactory_OfferMint` / `AllocationFactory_OfferBurn` on it; the same contract can always be cast to the `BurnMintFactory` or `TransferFactory` interfaces when needed.)

Choices (currently just one; future mutation actions would be added here):
- `IssuanceConfig_RotateFactory { newFactoryCid : ContractId AllocationFactory }` — controller `governanceParty`. Validates the new factory's admin, archives `this`, creates a fresh `IssuanceConfig` with all other fields preserved. Called by `RotateFactoryProposal` (see below).

**Identification pattern.** Daml 3 / LF 2.x removed contract keys, so `IssuanceConfig` is identified purely by `ContractId`. Proposals that operate on the config (pause, rotate) pass its current cid as a field; `executeImpl` fetches by cid and validates the fetched payload matches expectations (the `fetchChecked` idiom from `splice-util`) — in particular that its `governanceParty` equals the proposal's. Off-chain callers locate the current config by querying the ACS for `IssuanceConfig` contracts; the filter is `(governanceParty, instrumentId)`, not `governanceParty` alone, because a single governance party running multiple single-instrument plugin deployments will have several `IssuanceConfig` contracts live at once, one per instrument. There is no on-chain lookup primitive.

#### `MintProposal` and `BurnProposal` carry no instrument selector

They reference the `IssuanceConfig` by ContractId. `executeImpl` fetches the config and reads the `instrumentId` and `allocationFactoryCid`. Validation: the config's `governanceParty` must match the proposal's `governanceParty`.

**Passed arguments to `AllocationFactory_OfferMint` / `AllocationFactory_OfferBurn`.** `expectedAdmin = governanceParty` (read from the config). The proposal-specific payload is a `Mint` (for mint) or `Burn` (for burn) record built by `executeImpl` from the proposal's fields — recipient party, amount, instrument id (from the config), any deadlines. `extraArgs.meta` carries the proposal's `description` under the standard Splice reason key (`splice.lfdecentralizedtrust.org/reason`) so the reason surfaces in wallet UIs that parse factory metadata.

#### One Mint per `MintProposal`, one Burn per `BurnProposal`

Each mint proposal produces exactly one `MintOffer` for one recipient; each burn proposal produces exactly one `BurnOffer` against one holder. No batching in v1; keeps committee review focused on one recipient / holder at a time.

#### Burn target holdings are supplied by the proposer

The proposer drafts `BurnProposal` with the holder party and the holdings to burn (the shape matches the `Burn` record expected by `AllocationFactory_OfferBurn`). They obtain the candidate holding cids by querying the holder's active-contract set (or the governance party's, for treasury retirement) and selecting cids of the chosen instrument. `executeImpl` packages them into the `Burn` record and calls `AllocationFactory_OfferBurn`. Validation is the factory implementation's job — it checks the admin, the instrument, and produces a `BurnOffer` for the holder to accept. The plugin does not maintain any treasury inventory contract.

#### External-event metadata in the `description` field

Evidence of the off-ledger event that justifies each mint or burn (bridge tx hash, oracle quote id, bank wire ref, etc.) goes into the free-form `description : Text` field of `GovernableActionView`. It is surfaced on the `GovernanceExecutionResult` audit record. No typed event-id field on the proposal template in v1; structured metadata can be added later if needed.

#### Proposer: committee members only

With OfferMint-first and OfferBurn-first (pending team consensus), all v1 proposals are committee-initiated — a committee member drafts a `MintProposal` or `BurnProposal`, the committee votes, a member executes. The plugin does not need to accommodate non-member proposers in v1. If `RequestMint` / `RequestBurn` approval flows are added later (recipients / holders initiate; committee approves off the resulting `MintRequest` / `BurnRequest`), those would be new plugin templates where the proposer's identity is distinct from the committee, and the governance-core flow against a non-member proposer would need explicit validation.

#### `SetupIssuanceProposal` is one-shot (enforced by committee diligence)

Without contract keys, a choice body in Daml 3 / LF 2.x cannot scan the ACS to discover whether an `IssuanceConfig` already exists — so there is no cheap on-chain check that a second setup is a duplicate. Enforcement is therefore off-chain: the committee is responsible for refusing to confirm a second `SetupIssuanceProposal` after the first has executed. Consistent with other off-chain-discipline decisions in this plugin (replay protection, event-id uniqueness). A dedicated "setup marker" template that setup would archive is possible if stronger enforcement becomes necessary, but adds a bootstrap step we have otherwise kept out of the plugin.

#### `IssuanceConfig` owns its own update choice

The archive-and-recreate for factory rotation lives on `IssuanceConfig` as a choice `IssuanceConfig_RotateFactory { newFactoryCid : ContractId AllocationFactory }`, controller `governanceParty`. The choice body fetches `this`, validates the new factory's admin, archives, and creates a new `IssuanceConfig` copying every field unchanged except `allocationFactoryCid`.

The plugin-side `RotateFactoryProposal` template is a thin wrapper. It carries `issuanceConfigCid : ContractId IssuanceConfig` and `newFactoryCid : ContractId AllocationFactory` as proposal fields; its `executeImpl` verifies the fetched config's `governanceParty` matches the proposal's (the `fetchChecked` idiom from `splice-util`) and then calls `exercise issuanceConfigCid IssuanceConfig_RotateFactory with newFactoryCid`. Putting the mutation on the state contract rather than in `executeImpl` keeps the "how to update `IssuanceConfig`" knowledge co-located with the template itself; if more mutation choices are added later (e.g. re-introducing pause), they go on `IssuanceConfig` alongside this one.

*Cost of this pattern.* Extra indirection: one more choice body to read through, one more jump in the call stack. The `IssuanceConfig` template file grows with update logic instead of staying a pure record. Worth accepting if we expect more than one mutation action in the plugin's lifetime; for a single mutator (just rotate) it is a minor tax.

The recreated `IssuanceConfig` has a new ContractId; holders re-acquire it via the identification pattern (see schema section above). The instrument identity on the Splice `InstrumentConfiguration` is untouched, so external observers tracking the instrument by `InstrumentId` see no change.

**In-flight proposals.** `MintProposal` and `BurnProposal` contracts already on the ledger point at the old `IssuanceConfig` cid; after a rotate, those cids are archived and the pending proposals will fail at execute time when `executeImpl` tries to fetch the config. The committee must re-draft any pending mint or burn proposals against the new config cid. Not a defect — just an operational consequence of archive-and-recreate without keys, worth naming so it's not a surprise during a rotation.

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
- Inside each `executeImpl`, before calling `BurnMintFactory_BurnMint`, exercise `ProcessedEventLog_Record` on the log with the proposal's `eventId`. The record choice fails fast (with `"event already processed"`) if the id is already in the set, which aborts the whole execution.
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

If either treasury-* alternative is chosen (treasury-first mint or treasury-only burn — see "Proposals requiring team consensus"), the plugin would call `BurnMintFactory_BurnMint` directly with `extraActors = []`: in those models the governance party is both the admin and the holder of the new (mint) or existing (burn) holding, so no additional signature is needed.

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

The async property comes from the two steps being authorized separately: whichever party initiates needs only their own authority to create the offer / request contract; the other party's authority enters later at accept time. The treasury-first mint alternative ("Proposals requiring team consensus") names the mint side of this pattern explicitly. The burn side (`OfferBurn` / `RequestBurn`) is available on the same factory and would similarly support a user-initiated redemption flow if treasury-only burn is rejected.
