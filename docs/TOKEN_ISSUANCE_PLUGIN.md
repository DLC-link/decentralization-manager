# Token Issuance Plugin — Design

**Status:** design in progress. Decisions so far and open questions below.

## Overview

A Daml package that plugs into `governance-core`. Each plugin template implements the `GovernableAction` interface (see [GOVERNANCE_PLUGIN_ARCHITECTURE.md](GOVERNANCE_PLUGIN_ARCHITECTURE.md)) and wraps a single privileged Daml operation — here, a call to `BurnMintFactory_BurnMint` from the Splice token-issuance API.

**Goal.** Let a decentralized governance party (the signatory on `GovernanceRules`) mint and burn its own token instrument, with each mint or burn gated by a threshold of committee confirmations.

**Contrast with `canton-vault`.** Canton-vault bundles each mint with an atomic asset swap, so Canton transaction validation alone guarantees the economic invariant (see [TOKEN_ISSUANCE_IN_CANTON_VAULT.md](TOKEN_ISSUANCE_IN_CANTON_VAULT.md)). This plugin has **no swap**. Each mint or burn is an independent privileged action. The committee attests off-ledger to whatever event justifies it and votes on each individual issuance. The issuance mechanics (the `BurnMintFactory_BurnMint` primitive, the instrument-admin role, the `AllocationFactory` setup) are reused from canton-vault; what changes is that there is no on-ledger trigger, and the authority for each mint/burn comes from a governance confirmation threshold instead of a user's `TransferInstruction_Accept`.

---

## Decisions so far

### Two plugin templates: `MintProposal` and `BurnProposal` (A1)

Each template wraps one call to `BurnMintFactory_BurnMint` — in mint shape (empty inputs, non-empty outputs) or burn shape (non-empty inputs, empty outputs) respectively. A single combined template would need an awkward internal mint-vs-burn switch; two templates give the committee clearer intent at proposal time and cleaner validation per action.

### Factory cid stored on a shared `IssuanceConfig` contract (B2)

An `IssuanceConfig` contract, signed by the governance party, holds the `allocationFactoryCid` (and any related instrument metadata). Proposers reference the config by ContractId rather than repeating the factory cid on every proposal. Updating the config (e.g. rotating the factory) is itself a governance action.

---

## Open design questions

### A2. Scope per plugin instance — single instrument or many?

Does one deployment of this plugin govern issuance for a single instrument (fixed at setup), or for multiple instruments under the same governance party?

- **Single-instrument.** Each instrument gets its own plugin deployment with its own `IssuanceConfig`. Clean separation; more overhead if the governance party issues several tokens.
- **Multi-instrument.** One plugin, one `IssuanceConfig` per instrument, each proposal references the instrument it applies to. Scales more cleanly when several tokens are in scope.

### B1. Is `AllocationFactory` setup in scope for the plugin?

Canton-vault provisions the factory via Utility-Registry onboarding (`ProviderService_CreateProviderConfiguration` → `ProviderService_AcceptRegistrarServiceRequest` → `RegistrarService_CreateAllocationFactory`, plus `RegistrarService_CreateInstrumentConfiguration`). Options:

- **Out of scope (operator-side).** Operators run the onboarding separately using existing utility-registry templates, and then the governance party creates an `IssuanceConfig` with the resulting cid. The plugin stays minimal.
- **In scope (governance-driven).** Additional plugin templates (e.g. `SetupIssuanceProposal`) run the onboarding under committee control. Heavier plugin; no pre-deployment manual setup.

### C1. `extraActors` on the mint — the recipient-authority question

This is the biggest architectural decision. The Splice `AllocationFactory` implementation requires the owner of a newly-minted holding to appear in `BurnMintFactory_BurnMint.extraActors`. In canton-vault the owner is the depositing user, already a signatory on `DepositRequest` — their authority is naturally present. For a governance-initiated mint to an arbitrary recipient, the recipient's authority is not normally in scope inside `executeImpl`. Options:

- **(a) Recipient co-signs the `MintProposal`.** `signatory proposer, recipient`. The recipient must explicitly accept before the committee can execute. Flow: proposer drafts → recipient countersigns → committee confirms → execute. Good for consent-based issuance; heavy for bridge-style flows where the recipient may not be online or may not exist at propose time.
- **(b) Mint-preapproval contract.** A separate `MintPreapproval` contract signed by the recipient permits the governance party to mint to them (possibly up to a limit). `executeImpl` fetches the preapproval and uses it for authority. Lets the committee mint autonomously once the recipient has opted in.
- **(c) Use a different factory implementation.** One that does not demand owner authority. Requires an alternative to `AllocationFactory`; may not be available off the shelf.
- **(d) Treasury-first.** Mint to the governance party itself (`extraActors = []`, since the governance party is admin and owner). A separate subsequent step — a `TransferProposal` from the existing `governance-token-custody` plugin — delivers tokens to the final recipient. Splits issuance and distribution; requires the custody plugin in place and a treasury model.

The answer shapes `MintProposal`, the flow, and the plugin's dependencies.

### C2. One recipient per proposal, or batched?

`BurnMintFactory_BurnMint.outputs : [BurnMintOutput]` supports multiple recipients in one call.

- **One per proposal.** Committee reviews one mint at a time. Many proposals for a batch.
- **Batched.** One committee vote releases N mints. Cheaper; committee must review the full list. If the batch spans recipients, each must still be authorised per C1.

### C3. Amount source — trusted plaintext or computed?

Where does the mint amount come from?

- **Trusted plaintext.** Proposer writes a `Decimal` amount; committee verifies against off-chain evidence.
- **Computed on-chain.** An oracle contract or similar produces the amount; `executeImpl` reads it at execution time. More complex; depends on oracle infrastructure.

A first version probably uses plaintext; team should flag if the use case needs otherwise.

### C4. External-event metadata

Each mint (and burn, where relevant) should reference the off-ledger event that justifies it — bridge tx hash, oracle quote id, bank wire reference, etc. Where does this evidence live?

- As typed fields on the proposal template (e.g. `eventRef : Text`, plus structured supplementary data if needed).
- In the `description : Text` field of `GovernableActionView` (free-form; appears on the `GovernanceExecutionResult` audit record).
- Inside `extraArgs.meta : Metadata` (the Splice `BurnMintFactory_BurnMint` context) — Splice suggests a `splice.lfdecentralizedtrust.org/reason` key.

Open: what typed fields does the team want on the proposal, and is there a schema to standardise?

### D1. Burn target — whose holdings get burnt?

`BurnProposal` must identify the holdings to burn (`inputHoldingCids`). Without a swap, three shapes are possible:

- **(a) Treasury-only burn.** Governance party owns a pool of shares (from a treasury mint, or received transfers); burns reduce the pool. `extraActors = []`. Typical for de-issuance reflecting off-chain unwind.
- **(b) Third-party burn / redemption.** A holder surrenders their shares. The holder's authority is required in `extraActors`; the natural proposer is the holder themselves.
- **(c) Both, via two variants.** `TreasuryBurnProposal` and `RedemptionBurnProposal`, or one template with variant fields.

### D2. Proposer identity

For committee-initiated proposals (bridge-event mint, treasury burn) the proposer is a committee member. For user-initiated redemption (D1b) the proposer is the holder — not a committee member.

The governance-core plugin pattern (`signatory proposer, observer governanceParty`) does not on its face restrict the proposer to members. Worth explicit validation during design: does the confirm-then-execute flow work cleanly when the proposer is external to the committee?

### E1. Replay / idempotency — preventing double-execution of the same event

If two proposals reference the same external event (same bridge tx, same oracle reading), both executing is a double-mint. Options:

- **(a) Committee diligence only.** Members responsible for refusing duplicates. Simplest; no on-chain protection.
- **(b) `ProcessedEventLog` contract.** Stateful contract keyed by external event id; `executeImpl` checks the log and either refuses (if present) or appends (on success). Robust; adds a new contract.
- **(c) Scan past `GovernanceExecutionResult`s.** Reuse the existing audit log, structured so event ids can be read from prior executions. No new contract; requires discipline in how evidence is recorded.

### F1. Pause / emergency stop

Should the plugin provide a governance-level pause toggle? An `IssuancePaused` flag on `IssuanceConfig` (or a separate signal contract) that `executeImpl` checks. Flipping it is itself a governance action. Lets the committee halt all issuance without revoking the factory — useful for incident response.

### F2. Supply accounting — any on-chain bookkeeping needed?

Canton-vault maintains `YieldEpoch` because share value depends on total supply. This plugin is issuance-only; ground-truth supply is the sum of live `Holding`s, and the `GovernanceExecutionResult` audit record per mint/burn is the natural supply event log.

Confirm this is sufficient, or call out specific reasons a live supply contract is needed (external systems polling, on-chain cap enforcement, etc.).

### F3. Audit expectations beyond `GovernanceExecutionResult`

`governance-core` already creates a `GovernanceExecutionResult` per execution with `actionLabel`, `description`, `confirmers`, `executedAt`. Does the team need additional structured fields (event id, recipient, amount, instrument) captured in a plugin-specific audit record?

### G1. Off-chain attestation pipeline (out of plugin scope, but shapes C4)

How committee members learn about the external event they're voting on — a bridge oracle, a signed attestation chain, a manual evidence process — is out of scope for the plugin itself. But the structure of that evidence determines the proposal fields (C4). Worth a parallel team agreement on the attestation protocol before fixing the proposal schema.

### G2. Token UX — instrument naming & wallet display

Shares appear in any Splice-compatible wallet; the `InstrumentConfiguration` sets name, symbol, decimals. Who decides these values, and when? Interacts with B1: if instrument creation is in-scope for the plugin, these are proposal fields; if out-of-scope, they are chosen during operator setup.

---

## Next step

After the open questions are answered, the next artefact is an implementation plan: concrete template fields and choices for `MintProposal`, `BurnProposal` (and any setup / auxiliary templates), `executeImpl` bodies, and a test plan.
