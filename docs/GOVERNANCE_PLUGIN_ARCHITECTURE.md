# Governance Plugin Architecture

How `governance-core` supports plugins, and how the `governance-token-custody` plugin integrates with it.

## Core concepts

Terms used throughout this document. Each is the name of a specific, load-bearing concept; watch for the exact wording.

- **Governance party** — the decentralized Canton party whose authority every privileged operation consumes. It is the signatory on `GovernanceRules` and does not correspond to a single human or participant node; it is the collective on-chain identity of the committee.
- **Committee / member** — the individual parties authorised to confirm actions on behalf of the governance party. The set of members is a field on `GovernanceRules`.
- **Threshold** — the minimum number of member confirmations required before an action is executed. A field on `GovernanceRules`.
- **Privileged operation** — an operation delegated to the committee. Concretely: a Daml create where `governanceParty` is a signatory, or a Daml exercise where `governanceParty` is the controller. It is permitted to run only after the threshold is met.
- **Plugin** — a separate Daml package that data-depends on `governance-core` and adds new kinds of action proposals by defining templates that implement the `GovernableAction` interface. All such templates share the same interface viewtype (`GovernableActionView`); what distinguishes one kind of proposal from another is the template's own fields, its `executeImpl`, and the `actionLabel` it puts in the view.
- **Action proposal** — an instance of a plugin template. Each proposal represents one pending privileged operation awaiting confirmations.
- **Confirmation** — a `GovernanceConfirmation` contract representing one member's approval of one specific action proposal. Threshold-many live confirmations unlock execution.
- **Authority chain** (a.k.a. **exercise chain**) — the chain of nested `exercise` calls that carries the governance-party authority from the signed `GovernanceRules` contract down into the plugin's `executeImpl` body.

## Why — the problem plugins solve

`governance-core` implements one thing: a committee of members who must reach a threshold of confirmations before a privileged operation happens. It deliberately knows nothing about what those operations are. Hard-coding the permitted operations into core would force every new action type to ship as a core change — unacceptable for a library meant to govern arbitrary Daml contracts.

The plugin architecture lets any downstream Daml package define new action types that `governance-core` can confirm and execute, **without any change to `governance-core`**. This is what makes it possible to, for example, turn a decentralized governance party into a token custodian (via `governance-token-custody`) while keeping all token-specific logic out of the core.

## How — the mechanism

Every template in a plugin **must implement the `GovernableAction` interface** — this is the only hook `governance-core` has for recognizing the template as an action proposal. A template without this interface instance cannot be proposed through governance at all.

Given that interface, each plugin template wraps **exactly one** external Daml operation — a contract creation or a choice exercise on a template defined outside the plugin's own package — authorized by the decentralized governance party. The template's fields are the arguments of that external operation, captured at proposal time; `executeImpl` is the one-line body that invokes it at execution time. Anything more structured (multi-step workflows, state machines, balance tracking) is deliberately pushed out of plugins.

### The extension point: the `GovernableAction` interface

Defined in [Governance/Action.daml](../daml/governance-core/daml/Governance/Action.daml):

- `viewtype GovernableActionView { governanceParty, actionLabel, description }`
- Interface method `executeImpl : Update ()` — the plugin-specific side-effect.
- Choice `GovernableAction_Execute` — controller `governanceParty`; body just calls `executeImpl`. This is what `governance-core` calls once threshold is met.
- Choice `GovernableAction_Cancel` — controller `governanceParty`; no-op hook for cleanup.

Beyond the interface instance, each plugin template has the same skeleton:

- `signatory proposer` — the member drafting the proposal.
- `observer governanceParty` — so governance can see and fetch the proposal.

### The confirm-then-execute flow

[`GovernanceRules`](../daml/governance-core/daml/Governance/Rules.daml) (its `actionConfirmationTimeout` field sets how long a confirmation stays live) drives plugins through three choices:

- `GovernanceRules_ConfirmAction` (controller = a member): fetches the proposal's interface view, checks that the `governanceParty` declared in the view matches this `GovernanceRules` contract's `governanceParty`, and creates a `GovernanceConfirmation` pinned to the proposal's `ContractId GovernableAction`. Sets `expiresAt = now + actionConfirmationTimeout`.
- `GovernanceRules_ExecuteConfirmedAction` (controller = a member): consumes the confirmations, checks the invariants (none expired, all reference the same proposal cid, confirmers are current members, confirmers unique, count ≥ threshold), `exercise`s `GovernableAction_Execute` on the proposal, and writes a `GovernanceExecutionResult` audit record.
- `GovernanceRules_ExpireConfirmation` (controller = a member): lets members GC expired confirmations.

Proposal cancellation is intentionally *not* a governance choice — proposers cancel via template-specific choices, and governance-level cleanup of stale proposals is itself a `GovernableAction` proposal.

### The authority chain

`GovernanceRules` is signed by `governanceParty`. When `GovernanceRules_ExecuteConfirmedAction` does `exercise actionProposalCid GovernableAction_Execute`, the choice's controller `(view this).governanceParty` is satisfied by authority flowing down the **exercise chain** — the chain of nested `exercise` calls from `GovernanceRules` → `GovernableAction_Execute` → the external template inside `executeImpl`. Because each step is authorized by `governanceParty`, the authority remains in scope at the bottom of the chain, where `executeImpl` can use it to authorize **creates** whose signatory is `governanceParty` or **exercises** whose controller is `governanceParty`.

## What — the `governance-token-custody` plugin

### Package layout

- [daml/governance-core/daml.yaml](../daml/governance-core/daml.yaml) — package `governance-core-v0-rc2`. Depends only on `daml-prim`, `daml-stdlib`, and `splice-util`.
- [daml/governance-token-custody/daml.yaml](../daml/governance-token-custody/daml.yaml) — package `governance-token-custody-v0-rc2`. Data-depends on the compiled `governance-core` DAR plus the Splice token APIs, Splice wallet, and utility-registry DARs.

### Custody lifecycle

The plugin turns a decentralized governance party into a **token custodian** — an on-chain identity that can hold and move tokens only when a threshold of member signatures approves each individual move. It does not introduce a "wallet" or "vault" contract; it just wraps Splice / utility-registry token operations so each must go through confirm-then-execute.

In the Splice / utility-registry token model, the complete set of things a party can do with its holdings is (a) be set up as a receiver, (b) accept inbound transfers, and (c) send outbound transfers. The plugin provides one template per operation:

| Custody phase | Plugin template | Operation performed by `executeImpl` |
|---|---|---|
| **Enrol as receiver — Canton Coin** | `SetupCcPreapprovalProposal` | `create TransferPreapprovalProposal` with `receiver = governanceParty` |
| **Enrol as receiver — utility token** | `SetupTokenPreapprovalProposal` | `create TransferPreapproval` with `receiver = governanceParty` |
| **Accept an incoming transfer** (non-preapproved two-step inbound) | `AcceptTransferProposal` | `exercise TransferInstruction_Accept` on an incoming `TransferInstruction` |
| **Send tokens out** (one-step or two-step) | `TransferProposal` | `exercise TransferFactory_Transfer` on either a `TransferPreapproval` or an `AllocationFactory` |

The standing custody policy — members, threshold, timeout — lives entirely in `GovernanceRules`. The plugin does not track balances, enforce per-action limits, whitelist counterparties, or persist its own custody state.

`GenericVoteProposal` ships with `governance-core` itself because it is not specific to any domain: it has `executeImpl = pure ()` and exists only as the degenerate "pure vote" pattern, useful for off-chain decisions where the audit trail is the product.

### Per-template mechanics

#### [TransferProposal](../daml/governance-token-custody/daml/Governance/TokenCustody/TransferProposal.daml) — outgoing transfer
- **Fields (captured at proposal time):** a `ContractId TransferFactory` (either a `TransferPreapproval` for one-step transfers or an `AllocationFactory` for two-step offer transfers), the `expectedAdmin` party, the full `Transfer` record (sender, receiver, amount, input holding cids), and `extraArgs`.
- **`executeImpl`:** `exercise transferFactoryCid TransferFactory_Transfer with {expectedAdmin; transfer; extraArgs}`.
- **Authority binding:** `TransferFactory_Transfer`'s controller is the transfer sender. The proposal is only usable if `transfer.sender == governanceParty`.
- **Failure modes:** if any `inputHoldingCids` have been spent, merged, or split between proposal creation and execution, the call fails — the proposal captures a specific UTXO snapshot.

#### [AcceptTransferProposal](../daml/governance-token-custody/daml/Governance/TokenCustody/AcceptTransfer.daml) — incoming transfer
- **Fields:** a `ContractId TransferInstruction` pointing at a pending incoming offer, plus `extraArgs`.
- **`executeImpl`:** `exercise transferInstructionCid TransferInstruction_Accept with {extraArgs}`.
- **Authority binding:** `TransferInstruction_Accept`'s controller is the instruction's receiver; must be `governanceParty`.
- **Failure modes:** the sender can withdraw the instruction before governance execution — the proposal then fails with contract-not-found.

#### [SetupTokenPreapprovalProposal](../daml/governance-token-custody/daml/Governance/TokenCustody/SetupTokenPreapproval.daml) — utility-token preapproval
- **Fields:** `operator`, `instrumentAdmin`, and a list of `instrumentAllowances` (empty = all instruments of the admin).
- **`executeImpl`:** `create TransferPreapproval with {operator; receiver = governanceParty; instrumentAdmin; instrumentAllowances}`.
- **Authority binding:** `TransferPreapproval`'s signatory is `receiver = governanceParty`. No separate accept step is needed for utility tokens.

#### [SetupCcPreapprovalProposal](../daml/governance-token-custody/daml/Governance/TokenCustody/SetupCcPreapproval.daml) — Canton Coin preapproval
- **Fields:** `provider` and `expectedDso`.
- **`executeImpl`:** `create TransferPreapprovalProposal with {receiver = governanceParty; provider; expectedDso}`.
- **Authority binding:** `TransferPreapprovalProposal`'s signatory is `receiver = governanceParty`.
- **Follow-up off-chain:** the `provider` must later accept the proposal via `TransferPreapprovalProposal_Accept` (which handles fee payment); that step is outside governance.
