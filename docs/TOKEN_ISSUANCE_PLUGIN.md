# Token Issuance Plugin — Design

**Status:** draft design. Most items are decided; two architectural proposals require team consensus before being locked in.

## Overview

A Daml package that plugs into `governance-core`. Each plugin template implements the `GovernableAction` interface (see [GOVERNANCE_PLUGIN_ARCHITECTURE.md](GOVERNANCE_PLUGIN_ARCHITECTURE.md)) and wraps a privileged Daml operation — typically a call to `BurnMintFactory_BurnMint` from the Splice token-issuance API.

**Goal.** Let a decentralized governance party (the signatory on `GovernanceRules`) mint and burn its own token instrument, with each mint or burn gated by a threshold of committee confirmations.

**Contrast with `canton-vault`.** Canton-vault bundles each mint with an atomic asset swap, so Canton transaction validation alone guarantees the economic invariant (see [TOKEN_ISSUANCE_IN_CANTON_VAULT.md](TOKEN_ISSUANCE_IN_CANTON_VAULT.md)). This plugin has **no swap**. Each mint or burn is an independent privileged action. The committee attests off-ledger to whatever event justifies it and votes on each individual issuance. The issuance mechanics (the `BurnMintFactory_BurnMint` primitive, the instrument-admin role, the `AllocationFactory` setup) are reused from canton-vault; what changes is that there is no on-ledger trigger, and the authority for each mint/burn comes from a governance confirmation threshold instead of a user's `TransferInstruction_Accept`.

---

## Decisions so far

### Two plugin templates: `MintProposal` and `BurnProposal`

Each wraps one call to `BurnMintFactory_BurnMint` — in mint shape (empty inputs, non-empty outputs) or burn shape (non-empty inputs, empty outputs) respectively. A single combined template would need an awkward internal mint-vs-burn switch; two templates give the committee clearer intent at proposal time and cleaner validation per action.

### Factory cid stored on a shared `IssuanceConfig` contract

An `IssuanceConfig` contract, signed by the governance party, holds the `allocationFactoryCid` and instrument metadata. Proposers reference the config by ContractId rather than repeating the factory cid on every proposal.

### Setup is governance-driven, as a single `SetupIssuanceProposal`

The Utility-Registry onboarding — `ProviderService_CreateProviderConfiguration` → `ProviderService_AcceptRegistrarServiceRequest` → `RegistrarService_CreateAllocationFactory` → `RegistrarService_CreateInstrumentConfiguration` — is wrapped in a `SetupIssuanceProposal` plugin template that implements `GovernableAction`. One committee vote runs the whole chain and produces the `IssuanceConfig` contract in the same transaction, mirroring canton-vault's [`VaultGovernanceRules_SetupUtility`](https://github.com/DLC-link/canton-vault/blob/main/vault-daml/governance/daml/BitsafeVault/VaultGovernance.daml#L469-L514).

This is a deliberate departure from the usual "one plugin template wraps exactly one external Daml operation" rule (see [GOVERNANCE_PLUGIN_ARCHITECTURE.md](GOVERNANCE_PLUGIN_ARCHITECTURE.md)): the setup chain is a multi-step sequence that is operationally a single decision, so one governance action is cleaner than four.

### Single-instrument plugin deployment

Each deployment of this plugin governs issuance for exactly one instrument, fixed at setup time. If the governance party issues several tokens, each gets its own plugin deployment with its own `IssuanceConfig`. Trade-off: more deployment overhead when several tokens are in play, in exchange for cleaner per-instrument state and simpler per-proposal schemas (no instrument-selector field on `MintProposal` / `BurnProposal`).

### `IssuanceConfig` schema

Fields: `governanceParty : Party` (signatory), `instrumentId : InstrumentId`, `allocationFactoryCid : ContractId BurnMintFactory`, `paused : Bool`, plus the instrument-UX metadata set at setup time (display name, symbol, decimals, etc.). Exactly one `IssuanceConfig` exists per plugin deployment, for the plugin's lifetime. `paused` is `False` at setup.

### `MintProposal` and `BurnProposal` carry no instrument selector

They reference the `IssuanceConfig` by ContractId. `executeImpl` fetches the config, reads the `instrumentId` and `allocationFactoryCid`, and fails fast if `paused == True`. Validation: the config's `governanceParty` must match the proposal's `governanceParty`.

### Token UX is decided at setup

Display name, symbol, decimals, and any other `InstrumentConfiguration` fields are inputs to `SetupIssuanceProposal`, set once by the committee. They feed the `RegistrarService_CreateInstrumentConfiguration` call inside the onboarding chain and are recorded on the `IssuanceConfig`.

### `SetupIssuanceProposal` is one-shot

`executeImpl` must check that no `IssuanceConfig` already exists for this `governanceParty` and refuse if one does. Prevents a second setup from creating a duplicate config. A second attempt is always an error given single-instrument deployment.

### `actionLabel` values

`"SetupIssuance"`, `"Mint"`, `"Burn"`, `"SetPause"`, `"RotateFactory"`. Short and human-readable; they surface on `GovernanceExecutionResult` records and in any UI.

### Mint / burn proposal shape — minimalist defaults

**One output per `MintProposal`.** Each `MintProposal` has exactly one `BurnMintOutput`. Under the treasury-first proposal (see "Proposals requiring team consensus" below) the single recipient is always the governance party, so batching has no effect anyway; even absent treasury-first, one-per-proposal keeps committee review focused on one mint at a time.

**Amount source: plaintext `Decimal` field.** Proposer writes the amount on the proposal; the committee verifies against off-chain evidence before confirming. No oracle contract or on-chain amount computation in v1.

**External-event metadata: `description : Text` field of `GovernableActionView`.** Evidence of the off-ledger event that justifies the mint or burn (bridge tx hash, oracle quote id, bank wire ref, etc.) goes into the free-form `description`, which is surfaced on the `GovernanceExecutionResult` audit record. No typed event-id field on the proposal template in v1. Structured metadata can be added later if a real need emerges.

**Proposer: committee members only.** With treasury-first mint and treasury-only burn proposed (pending team consensus), all proposals are committee-initiated; the plugin does not need to accommodate non-member proposers. (Contingent on those two proposals — if user-initiated redemption is wanted later, this would need to relax.)

### Operational policy — minimalist defaults

**Replay protection: committee diligence only.** No `ProcessedEventLog` contract or audit-log scanning in v1. Committee members are responsible for refusing duplicate proposals for the same external event. If double-execution becomes a real problem in practice, a log can be added later.

**No on-chain supply accounting.** Ground-truth supply is the sum of live `Holding`s for the share instrument. The per-execution `GovernanceExecutionResult` records produced by `governance-core` are the natural supply event log. No running supply contract analogous to canton-vault's `YieldEpoch`.

**No plugin-specific audit record.** The `GovernanceExecutionResult` that `governance-core` already emits per execution (`actionLabel`, `description`, `confirmers`, `executedAt`) is the plugin's audit trail. No additional structured fields (event id, recipient, amount) captured in a plugin template.

### Advanced features included — beyond strict minimalism

The next two decisions go beyond what a strictly minimal design would require. We recommend including them because the cost is small and the operational value is real. A team prioritising absolute minimalism could defer either to a later version; our default is to ship both in v1.

**Pause / resume.** `IssuanceConfig` carries a `paused : Bool`. `MintProposal.executeImpl` and `BurnProposal.executeImpl` check it and fail if paused. A `SetPauseProposal { newPaused : Bool }` template lets the committee toggle it via a governance vote.

*Why include it:* gives the committee an explicit, on-chain "we are stopped" state. External systems (wallets, dashboards, backend integrations, off-chain attestation pipelines) can observe it rather than infer. Converts ongoing per-proposal vigilance into one committed action, which is also reversible. The minimalist alternative — committee members refusing to confirm during incidents — works but relies on out-of-band signalling and per-proposal attendance.

**Factory rotation.** A `RotateFactoryProposal { newFactoryCid : ContractId BurnMintFactory }` template archives the current `IssuanceConfig` and recreates it with the new factory cid, preserving everything else (instrument identity, pause state, UX metadata). `executeImpl` validates that `(view newFactory).admin == governanceParty` via `BurnMintFactory_PublicFetch` before recreating.

*Why include it:* if the Utility Registry ever issues a replacement factory for the same registrar (version upgrade, migration), rotating in place is far cheaper than redeploying the plugin. Instrument identity and `IssuanceConfig` cid references held by downstream integrations are preserved. The minimalist alternative — redeploy the plugin — is disproportionately expensive for what is structurally a small config change. *Open confirmation for the team:* whether factory rotation is a realistic operational scenario at all; if factories are effectively immutable once created, this template is dead code and should be dropped.

---

## Proposals requiring team consensus

The following two choices keep the plugin self-contained and simple, but they have real operational implications and foreclose certain use cases. They are proposed as the default but should be confirmed by the team before being locked in.

### Treasury-first mint

**Proposal.** Every `MintProposal` mints to the governance party itself. `BurnMintOutput.owner = governanceParty`, `extraActors = []`. Delivery to the final recipient is a *separate* governance action, via the custody plugin's `TransferProposal`.

**Why this simplifies the plugin.** The Splice `AllocationFactory` implementation requires the owner of a newly-minted holding to appear in `BurnMintFactory_BurnMint.extraActors`. A mint directly to an arbitrary third party would therefore need that party's authority in scope inside `executeImpl` — which forces either a recipient countersignature on every `MintProposal`, a pre-existing `MintPreapproval` contract, or a non-`AllocationFactory` implementation. Treasury-first sidesteps all three, because the governance party is both the admin and the owner of the new holding.

**Trade-off.** "Mint and deliver to Alice" becomes two governance votes (one `MintProposal`, then one `TransferProposal` out of the custody plugin). This is fine for lower-frequency governance-controlled issuance, and potentially expensive for high-throughput bridge-style flows where every external event produces a mint.

**Alternatives if rejected.** (a) recipient co-signs `MintProposal` — consent-based, requires recipient online at propose time; (b) `MintPreapproval` contract — recipient opts in once, committee mints autonomously thereafter; (c) different factory implementation — requires an alternative to `AllocationFactory` we'd have to source separately.

### Treasury-only burn

**Proposal.** Every `BurnProposal` burns holdings owned by the governance party. `extraActors = []`. The governance party's authority is already present via signatory inheritance through the `BurnProposal` contract; no third-party signatures required.

**Why this simplifies the plugin.** No redemption flow to model, no holder authority to collect, no proposer-outside-the-committee case to handle. Symmetric with treasury-first mint.

**Trade-off.** User-initiated redemption — where a holder hands back shares in exchange for something — is not supported. Burns reflect off-chain unwinds decided by the committee, not user surrender.

**Alternative if rejected.** Add a `RedemptionBurnProposal` template whose signatories include the holder, with `extraActors = [holder]` in the burn call. This also means the proposer may be a user rather than a committee member, which is worth explicit validation against the governance-core flow.

---

## Team coordination item (out of plugin scope)

**Attestation pipeline.** How committee members learn about the external event they're voting on — a bridge oracle, a signed attestation chain, a manual evidence process — is out of scope for the plugin itself. But the structure of that evidence determines whether the free-form `description` field (the current v1 default) is enough or whether we need typed fields on `MintProposal` / `BurnProposal`. Worth a parallel team agreement on the attestation protocol before any structured schema is fixed.

---

## Next step

Once the two proposals above are confirmed (or replaced), the next artefact is an implementation plan: concrete template fields and choices for `IssuanceConfig`, `SetupIssuanceProposal`, `MintProposal`, `BurnProposal`, `SetPauseProposal`, and (conditionally) `RotateFactoryProposal`; `executeImpl` bodies; and a test plan.
