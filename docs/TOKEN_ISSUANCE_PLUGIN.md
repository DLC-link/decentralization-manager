# Token Issuance Plugin — Design

**Status:** design in progress. Decisions so far and open questions below.

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

### Advanced features included — beyond strict minimalism

The next two decisions go beyond what a strictly minimal design would require. We recommend including them because the cost is small and the operational value is real. A team prioritising absolute minimalism could defer either to a later version; our default is to ship both in v1.

**Pause / resume.** `IssuanceConfig` carries a `paused : Bool`. `MintProposal.executeImpl` and `BurnProposal.executeImpl` check it and fail if paused. A `SetPauseProposal { newPaused : Bool }` template lets the committee toggle it via a governance vote.

*Why include it:* gives the committee an explicit, on-chain "we are stopped" state. External systems (wallets, dashboards, backend integrations, off-chain attestation pipelines) can observe it rather than infer. Converts ongoing per-proposal vigilance into one committed action, which is also reversible. The minimalist alternative — committee members refusing to confirm during incidents — works but relies on out-of-band signalling and per-proposal attendance.

**Factory rotation.** A `RotateFactoryProposal { newFactoryCid : ContractId BurnMintFactory }` template archives the current `IssuanceConfig` and recreates it with the new factory cid, preserving everything else (instrument identity, pause state, UX metadata). `executeImpl` validates that `(view newFactory).admin == governanceParty` via `BurnMintFactory_PublicFetch` before recreating.

*Why include it:* if the Utility Registry ever issues a replacement factory for the same registrar (version upgrade, migration), rotating in place is far cheaper than redeploying the plugin. Instrument identity and `IssuanceConfig` cid references held by downstream integrations are preserved. The minimalist alternative — redeploy the plugin — is disproportionately expensive for what is structurally a small config change. *Open confirmation for the team:* whether factory rotation is a realistic operational scenario at all; if factories are effectively immutable once created, this template is dead code and should be dropped.

---

## Open design questions

### Q1. `extraActors` on the mint — the recipient-authority question

This is the biggest architectural decision. The Splice `AllocationFactory` implementation requires the owner of a newly-minted holding to appear in `BurnMintFactory_BurnMint.extraActors`. In canton-vault the owner is the depositing user, already a signatory on `DepositRequest` — their authority is naturally present. For a governance-initiated mint to an arbitrary recipient, the recipient's authority is not normally in scope inside `executeImpl`. Options:

- **(a) Recipient co-signs the `MintProposal`.** `signatory proposer, recipient`. The recipient must explicitly accept before the committee can execute. Flow: proposer drafts → recipient countersigns → committee confirms → execute. Good for consent-based issuance; heavy for bridge-style flows where the recipient may not be online or may not exist at propose time.
- **(b) Mint-preapproval contract.** A separate `MintPreapproval` contract signed by the recipient permits the governance party to mint to them (possibly up to a limit). `executeImpl` fetches the preapproval and uses it for authority. Lets the committee mint autonomously once the recipient has opted in.
- **(c) Use a different factory implementation.** One that does not demand owner authority. Requires an alternative to `AllocationFactory`; may not be available off the shelf.
- **(d) Treasury-first.** Mint to the governance party itself (`extraActors = []`, since the governance party is admin and owner). A separate subsequent step — a `TransferProposal` from the existing `governance-token-custody` plugin — delivers tokens to the final recipient. Splits issuance and distribution; requires the custody plugin in place and a treasury model.

The answer shapes `MintProposal`, the flow, and the plugin's dependencies.

### Q2. One recipient per proposal, or batched?

`BurnMintFactory_BurnMint.outputs : [BurnMintOutput]` supports multiple recipients in one call.

- **One per proposal.** Committee reviews one mint at a time. Many proposals for a batch.
- **Batched.** One committee vote releases N mints. Cheaper; committee must review the full list. If the batch spans recipients, each must still be authorised per Q1.

### Q3. Amount source — trusted plaintext or computed?

Where does the mint amount come from?

- **Trusted plaintext.** Proposer writes a `Decimal` amount; committee verifies against off-chain evidence.
- **Computed on-chain.** An oracle contract or similar produces the amount; `executeImpl` reads it at execution time. More complex; depends on oracle infrastructure.

A first version probably uses plaintext; team should flag if the use case needs otherwise.

### Q4. External-event metadata

Each mint (and burn, where relevant) should reference the off-ledger event that justifies it — bridge tx hash, oracle quote id, bank wire reference, etc. Where does this evidence live?

- As typed fields on the proposal template (e.g. `eventRef : Text`, plus structured supplementary data if needed).
- In the `description : Text` field of `GovernableActionView` (free-form; appears on the `GovernanceExecutionResult` audit record).
- Inside `extraArgs.meta : Metadata` (the Splice `BurnMintFactory_BurnMint` context) — Splice suggests a `splice.lfdecentralizedtrust.org/reason` key.

Open: what typed fields does the team want on the proposal, and is there a schema to standardise?

### Q5. Burn target — whose holdings get burnt?

`BurnProposal` must identify the holdings to burn (`inputHoldingCids`). Without a swap, three shapes are possible:

- **(a) Treasury-only burn.** Governance party owns a pool of shares (from a treasury mint, or received transfers); burns reduce the pool. `extraActors = []`. Typical for de-issuance reflecting off-chain unwind.
- **(b) Third-party burn / redemption.** A holder surrenders their shares. The holder's authority is required in `extraActors`; the natural proposer is the holder themselves.
- **(c) Both, via two variants.** `TreasuryBurnProposal` and `RedemptionBurnProposal`, or one template with variant fields.

### Q6. Proposer identity

For committee-initiated proposals (bridge-event mint, treasury burn) the proposer is a committee member. For user-initiated redemption (Q5b) the proposer is the holder — not a committee member.

The governance-core plugin pattern (`signatory proposer, observer governanceParty`) does not on its face restrict the proposer to members. Worth explicit validation during design: does the confirm-then-execute flow work cleanly when the proposer is external to the committee?

### Q7. Replay / idempotency — preventing double-execution of the same event

If two proposals reference the same external event (same bridge tx, same oracle reading), both executing is a double-mint. Because each plugin deployment covers a single instrument, the replay-protection scope is naturally per-deployment — there's no "which instrument" key to carry. Options:

- **(a) Committee diligence only.** Members responsible for refusing duplicates. Simplest; no on-chain protection.
- **(b) `ProcessedEventLog` contract.** A stateful contract keyed by external event id; `executeImpl` checks the log and either refuses (if present) or appends (on success). Robust; adds a new contract to the plugin model.
- **(c) Scan past `GovernanceExecutionResult`s.** Reuse the existing audit log, structured so event ids can be read from prior executions. No new contract; requires discipline in how evidence is recorded.

### Q8. Supply accounting — any on-chain bookkeeping needed?

Canton-vault maintains `YieldEpoch` because share value depends on total supply. This plugin is issuance-only; ground-truth supply is the sum of live `Holding`s, and the `GovernanceExecutionResult` audit record per mint/burn is the natural supply event log.

Confirm this is sufficient, or call out specific reasons a live supply contract is needed (external systems polling, on-chain cap enforcement, etc.).

### Q9. Audit expectations beyond `GovernanceExecutionResult`

`governance-core` already creates a `GovernanceExecutionResult` per execution with `actionLabel`, `description`, `confirmers`, `executedAt`. Does the team need additional structured fields (event id, recipient, amount, instrument) captured in a plugin-specific audit record?

### Q10. Off-chain attestation pipeline (out of plugin scope, but shapes Q4)

How committee members learn about the external event they're voting on — a bridge oracle, a signed attestation chain, a manual evidence process — is out of scope for the plugin itself. But the structure of that evidence determines the proposal fields (Q4). Worth a parallel team agreement on the attestation protocol before fixing the proposal schema.

---

## Next step

After the open questions are answered, the next artefact is an implementation plan: concrete template fields and choices for `IssuanceConfig`, `SetupIssuanceProposal`, `MintProposal`, `BurnProposal`, `SetPauseProposal`, and (conditionally) `RotateFactoryProposal`; `executeImpl` bodies; and a test plan.
