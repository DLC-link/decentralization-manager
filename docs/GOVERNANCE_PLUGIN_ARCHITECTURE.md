# Governance Plugin Architecture

How `governance-core` supports plugins, and how the `governance-token-custody` plugin integrates with it.

## Package layout

- [daml/governance-core/daml.yaml](../daml/governance-core/daml.yaml) — package `governance-core-v0-rc2`. Depends only on `daml-prim`, `daml-stdlib`, and `splice-util`.
- [daml/governance-token-custody/daml.yaml](../daml/governance-token-custody/daml.yaml) — package `governance-token-custody-v0-rc2`. Data-depends on the compiled `governance-core` DAR plus the Splice token APIs, Splice wallet, and utility-registry DARs.

A plugin is just a separate Daml package that data-depends on `governance-core` and adds new templates. No code in `governance-core` changes when a plugin is added.

## The extension point: the `GovernableAction` interface

Defined in [Governance/Action.daml](../daml/governance-core/daml/Governance/Action.daml):

- `viewtype GovernableActionView { governanceParty, actionLabel, description }`
- Abstract method `executeImpl : Update ()` — the plugin-specific side-effect.
- Choice `GovernableAction_Execute` — controller `governanceParty`; body just calls `executeImpl`. This is what core calls once the threshold is met.
- Choice `GovernableAction_Cancel` — controller `governanceParty`; no-op hook for cleanup.

## Two parallel flows in `GovernanceRules`

[Governance/Rules.daml](../daml/governance-core/daml/Governance/Rules.daml) template `GovernanceRules` (signatory `governanceParty`; fields `members`, `threshold`, `actionConfirmationTimeout`) exposes two distinct machines:

**1. Self-management (closed, not extensible).** A closed enum `GovernanceSelfAction` covers add/remove member, set threshold, set timeout. Confirmations are `GovernanceSelfConfirmation` contracts matched by value equality on the enum. `GovernanceRules_ExecuteGovernanceAction` validates confirmations and dispatches to consuming choices on `self` that build the new `GovernanceRules` contract.

**2. Domain actions via `GovernableAction` (extensible — this is the plugin path).**

- `GovernanceRules_ConfirmAction` (nonconsuming, controller = a member): fetches the proposal's interface view, checks `actionView.governanceParty == governanceParty`, and creates a `GovernanceConfirmation` pinned to the proposal's **`ContractId GovernableAction`** (not value equality). It copies `actionLabel` in for UI, and sets `expiresAt = now + actionConfirmationTimeout`.
- `GovernanceRules_ExecuteConfirmedAction` (nonconsuming, controller = a member): `exercise` each confirmation's `GovernanceConfirmation_Consume` to pull the records, then requires: none expired, all reference the same `actionProposalCid`, all confirmers are current members, confirmers unique, count ≥ threshold. It then `exercise`s `GovernableAction_Execute` on the proposal. Finally it creates a `GovernanceExecutionResult` audit record.
- `GovernanceRules_ExpireConfirmation` (nonconsuming, controller = a member): lets members GC expired confirmations.

A known-issue TODO in `GovernanceRules_ConfirmAction` notes that matching solely on `governanceParty` doesn't distinguish multiple `GovernanceRules` instances for the same party; a static governance ID is planned.

Proposal cancellation is deliberately **not** a governance choice — proposers cancel their own proposals via template-specific choices; governance-level cleanup is expected to itself go through the domain-action flow.

## Authority chain

This is the key trick that lets a plugin perform privileged operations without giving the plugin author the governance party's keys.

`GovernanceRules` is signed by `governanceParty`. When `GovernanceRules_ExecuteConfirmedAction` does `exercise actionProposalCid GovernableAction_Execute`, Daml's authorization rules require the controller of that choice — which `GovernableAction` declares as `(view this).governanceParty` — so the exercise is authorized by `governanceParty` flowing down the exercise chain. Inside `executeImpl` the governance party's authority is available for creates, exercises, and archives where `governanceParty` is signatory or a required controller.

## Plugin template shape

Every plugin template in [governance-token-custody](../daml/governance-token-custody/daml/Governance/TokenCustody/) follows the same recipe:

- `signatory proposer` — the member drafting the proposal.
- `observer governanceParty` — so governance can see and fetch it.
- `interface instance GovernableAction for MyTemplate where ...` — fills in the view (with a short `actionLabel` and human `description`) and an `executeImpl` that performs the privileged work.

## What `executeImpl` actually does in the current plugin

| Template | executeImpl | How governance-party authority is used |
|---|---|---|
| [TransferProposal](../daml/governance-token-custody/daml/Governance/TokenCustody/TransferProposal.daml) | `exercise transferFactoryCid TransferFactory_Transfer {...}` | Governance party is the `sender` on the transfer |
| [AcceptTransferProposal](../daml/governance-token-custody/daml/Governance/TokenCustody/AcceptTransfer.daml) | `exercise transferInstructionCid TransferInstruction_Accept {...}` | Governance party must be `receiver` on the instruction |
| [SetupTokenPreapprovalProposal](../daml/governance-token-custody/daml/Governance/TokenCustody/SetupTokenPreapproval.daml) | `create TransferPreapproval { receiver = governanceParty, ... }` | Governance party is the signatory receiver |
| [SetupCcPreapprovalProposal](../daml/governance-token-custody/daml/Governance/TokenCustody/SetupCcPreapproval.daml) | `create TransferPreapprovalProposal { receiver = governanceParty, ... }` | Governance party is the signatory receiver; provider accepts separately |
| [GenericVoteProposal](../daml/governance-core/daml/Governance/GenericVote.daml) *(lives in core)* | `pure ()` | None — the execution record is the only on-chain artifact |

## Net summary of the plugin contract

A plugin gives governance-core the *what* (the `executeImpl`) plus light metadata (label, description) via the `GovernableAction` interface. Governance-core supplies the *when* (threshold of live confirmations against the specific proposal cid) and the *authority* (governance party signing the outer `GovernanceRules` exercise chain). The only cross-package touchpoints are the `GovernableAction` interface, the `GovernanceConfirmation` / `GovernanceExecutionResult` templates, and the implicit invariant that plugin proposals must have `governanceParty` as an observer so confirmations can fetch the view.
