# Token-Custody Plugin — Governance Action Inputs

This is a focused, semantic reference for the **`governance-token-custody`** plugin: what each input field *means*, where the value comes from, and how it maps from the DAML on-chain template to the field you fill in on the UI.

It is **not** a fields-and-types table (that is in [docs/UI_INPUTS.md](UI_INPUTS.md)). It is a "what do I put here, and where do I get it" reference for someone configuring these actions for the first time.

> **Demo values:** every action has a *Demo value* column left blank. Fill in the values used in the existing demo so the table doubles as a copy-paste cheat sheet.

---

## Where things live

| Layer | Location |
|---|---|
| DAML package | [daml/governance-token-custody/](../daml/governance-token-custody/) (`governance-token-custody-v0-rc4`) |
| UI | The four "Setup CC Preapproval", "Setup Token Preapproval", "Transfer", "Accept Transfer" entries under **New Proposal** in [frontend/src/components/GovernanceSection.tsx](../frontend/src/components/GovernanceSection.tsx) (visible only when `governanceType === "core_self"`). |
| Submission | The on-chain template name shown below is created with the user-supplied parameters; the governance party then runs the executeImpl when the proposal reaches its threshold. |

The four actions:

| UI label | DAML template | DAML source |
|---|---|---|
| Setup CC Preapproval | `SetupCcPreapprovalProposal` | [SetupCcPreapproval.daml](../daml/governance-token-custody/daml/Governance/TokenCustody/SetupCcPreapproval.daml) |
| Setup Token Preapproval | `SetupTokenPreapprovalProposal` | [SetupTokenPreapproval.daml](../daml/governance-token-custody/daml/Governance/TokenCustody/SetupTokenPreapproval.daml) |
| Transfer | `TransferProposal` | [TransferProposal.daml](../daml/governance-token-custody/daml/Governance/TokenCustody/TransferProposal.daml) |
| Accept Transfer | `AcceptTransferProposal` | [AcceptTransfer.daml](../daml/governance-token-custody/daml/Governance/TokenCustody/AcceptTransfer.daml) |

---

## Glossary (read this first)

These concepts recur across the four actions. Understanding them makes every field self-explanatory.

| Term | What it is | Where its value comes from |
|---|---|---|
| **Governance party** | The on-chain party whose authority is required to execute the proposal. *Not entered in the UI* — taken from the *Governance Contract ID* selected in the **New Governance Action** form. | The "party id" of the decentralized party, e.g. `decentralized-party::1220abc…`. Listed in the party detail view. |
| **Proposer** | The member who submits the proposal. *Not entered in the UI* — auto-filled from the logged-in user's member party. | The currently authenticated member's party id. Visible on the auth-status panel. |
| **Provider** *(CC pre-approval)* | The Splice Wallet / Canton Coin "provider" party that will *pay the fees* for the receiving pre-approval. **This is the field a casual reviewer might call "utility provider"** — it isn't a utility-registry provider, it's a CC fee provider. | Provided by the team operating the Splice / Canton-Coin instance. |
| **Expected DSO** *(CC pre-approval)* | The DSO party expected on the resulting `TransferPreapproval` contract. Used by the on-chain code to check the proposal will create a preapproval against the right DSO. Optional in DAML, but the UI requires it. | Look up the DSO party id of the synchronizer / Canton Coin instance you are connecting to. |
| **Operator** *(token pre-approval)* | The Utility-Registry **operator** party — the entity running the registry on which the token instruments are issued. Becomes an *observer* on the resulting `TransferPreapproval`. | Same operator party that appears in your Utility-Registry deployment. Often the same as the operator party in `Setup Utility` / `Create Provider Service Request`. |
| **Instrument admin** | The party that *administers / issues* the instrument(s) being preapproved or transferred. Not the same as the operator. | The admin party of the token (e.g. for `CBTC` it is the CBTC admin party). |
| **Instrument id** | The string identifier of the token instrument, scoped to its admin. | E.g. `CBTC`. |
| **Transfer factory** *(transfer)* | A contract id that *describes how* the transfer will be executed. Two valid shapes: <br>• **TransferPreapproval CID** → one-step direct transfer (the receiver has already preapproved). <br>• **AllocationFactory CID** → two-step transfer (creates a pending offer the receiver must accept). | For one-step, use a previously-created `TransferPreapproval` CID for that receiver/admin pair. For two-step, use the `AllocationFactory` CID published by the registrar service. |
| **Expected admin** *(transfer)* | The admin party that *should be* on the transfer factory. The on-chain code refuses if the actual admin disagrees — this is a safety check. | Same as the **Instrument admin** for the token being transferred. |
| **TransferInstruction** *(accept-transfer)* | A pending incoming transfer (a "transfer offer") sitting on the ledger waiting for the receiver to accept. | Returned to the receiver when the sender created a two-step transfer. Fetch the CID by listing pending TransferInstructions for the governance party (receiver). |
| **Input holdings** *(transfer)* | The specific holding contracts you are spending to fund the transfer. Empty list = backend auto-selects holdings of the right (admin, id) pair. | List CIDs of the holdings the governance party owns. UTXO timing risk: if a holding is consumed before the proposal executes, execution will fail. |
| **ExtraArgs** | An on-chain envelope for context/metadata (Daml `ExtraArgs` from the splice token API). Currently *not exposed in the UI* — sent as empty. | n/a — defaulted by the backend. |

---

## Action 1 — Setup CC Preapproval

**Effect on success:** creates a `TransferPreapprovalProposal` where the governance party is the *receiver*. The provider must then separately accept that proposal (which is when fees are paid). After that, anyone can transfer Canton Coin to the governance party in one step.

**UI form:** *New Proposal → Setup CC Preapproval*
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| Provider Party | `provider` | The Canton-Coin provider party that will pay the preapproval fee. *Not* an on-ledger utility provider. | Provided by the team operating the Splice/CC instance. | _(blank)_ |
| Expected DSO Party | `expectedDso` | The DSO party id of the synchronizer / CC instance. UI marks this required even though DAML allows None — supply it. | DSO of the synchronizer; look up in `NetworkConfigAccordion` or the synchronizer admin docs. | _(blank)_ |
| _(implicit)_ Governance party | `governanceParty` | — | Decentralized-party ID; auto-filled from the *Governance Contract ID*. | n/a |
| _(implicit)_ Proposer | `proposer` | — | Logged-in member; auto. | n/a |

> **Trap:** "Provider Party" is **not** the utility-registry provider used elsewhere in the governance UI; it is the Canton-Coin fee provider. If you copy the operator from `Setup Utility` here, the proposal will execute but the resulting `TransferPreapprovalProposal` will be rejected by that party.

---

## Action 2 — Setup Token Preapproval

**Effect on success:** creates a `TransferPreapproval` directly (no separate accept step) on the utility-registry, where the governance party is the *receiver*. After that, the registry permits incoming utility-token transfers to the governance party for the listed instruments — or for *all* instruments of `instrumentAdmin` if the allowance list is empty.

**UI form:** *New Proposal → Setup Token Preapproval*
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| Operator Party | `operator` | The utility-registry operator party — the entity running the registry. Becomes an observer on the resulting `TransferPreapproval`. | Same operator used in `Setup Utility` / `Create Provider Service Request`. | _(blank)_ |
| Instrument Admin | `instrumentAdmin` | The party that *issues / administers* the token instruments. | E.g. CBTC admin party. | _(blank)_ |
| _(not in UI)_ Instrument allowances | `instrumentAllowances` | A list of specific instruments to preapprove. Empty list = all instruments of the admin. | UI currently sends an empty list, so the preapproval covers **every** instrument issued by `instrumentAdmin`. | n/a |
| _(implicit)_ Governance party | `governanceParty` | — | auto | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

> **Gap:** the UI does not expose `instrumentAllowances`; the resulting preapproval is always for *all* instruments of the admin. Document this for QA.

---

## Action 3 — Transfer

**Effect on success:** runs `TransferFactory_Transfer` from the governance party. The behavior depends on which kind of factory CID you supplied:

- **TransferPreapproval CID** → completes the transfer immediately (one-step).
- **AllocationFactory CID** → creates a pending `TransferOffer` / `TransferInstruction` that the receiver must accept separately (two-step).

The on-chain `ensure transfer.amount > 0.0` enforces a positive amount.

**UI form:** *New Proposal → Transfer*
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| TransferFactory Contract ID | `transferFactoryCid` | A `TransferPreapproval` CID (one-step) **or** an `AllocationFactory` CID (two-step). | One-step: a preapproval previously created for `(instrumentAdmin, receiver)`. Two-step: the allocation factory CID returned when the registrar service was configured. | _(blank)_ |
| Expected Admin Party | `expectedAdmin` | The admin party that should be on the chosen transfer factory. Safety check: on-chain code aborts if the live factory disagrees. | Same as **Instrument Admin** below. | _(blank)_ |
| Receiver Party | `transfer.receiver` | The party that will receive the tokens. | The recipient's party id. | _(blank)_ |
| Amount | `transfer.amount` | Decimal amount; must be > 0. | Free choice. | e.g. `1000` |
| Instrument Admin | `transfer.instrument.admin` | The admin party of the instrument being transferred. | Same admin used in setup-token-preapproval / vault. | _(blank)_ |
| Instrument ID | `transfer.instrument.id` | Instrument identifier string. | E.g. `CBTC`. | _(blank)_ |
| Input Holding CIDs | `transfer.inputHoldingCids` | Comma-separated list of holding CIDs to spend. Leave empty to let the backend auto-select. | List the governance party's holdings of the same `(admin, id)`. | _(blank)_ |
| _(not in UI)_ Extra args | `extraArgs` | Context/metadata envelope. | UI sends empty; backend default. | n/a |
| _(implicit)_ Governance party | `governanceParty` | — | auto | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

> **UTXO timing risk** (per the DAML comment): the input holdings are captured at *proposal* time. If any of them gets spent before the proposal reaches its threshold and executes, execution fails. Mitigations:
> - reserve dedicated holdings for governance proposals,
> - keep the governance confirmation timeout short,
> - re-propose if holdings change.

---

## Action 4 — Accept Transfer

**Effect on success:** exercises `TransferInstruction_Accept` on the supplied `TransferInstruction`. The governance party must be the *receiver* on that instruction — the on-chain authority chain requires it.

**UI form:** *New Proposal → Accept Transfer*
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| TransferInstruction Contract ID | `transferInstructionCid` | The CID of the pending `TransferInstruction` whose receiver is the governance party. | Find by listing pending TransferInstructions visible to the governance party (e.g. via the `ContractsDialog` or by querying the ledger). | _(blank)_ |
| _(not in UI)_ Extra args | `extraArgs` | Context/metadata for the accept choice. | UI sends empty; backend default. | n/a |
| _(implicit)_ Governance party | `governanceParty` | — | auto | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

> **Trap:** the sender of a two-step transfer can withdraw the `TransferInstruction` before the governance accept proposal completes. If they do, execution fails with a contract-not-found error. Reproduce this for QA by withdrawing the offer between *propose* and *threshold reached*.

---

## Field-name disambiguation

A handful of similar-sounding terms cause confusion across the four actions:

| Term in conversation | Likely UI field | Action |
|---|---|---|
| "utility provider" | **Provider Party** in *Setup CC Preapproval* | The CC fee provider (not a utility-registry provider). |
| "service contract ID" | **TransferInstruction Contract ID** in *Accept Transfer* (or **TransferFactory Contract ID** in *Transfer*) | Always an on-ledger contract id, but which contract depends on the action. |
| "operator party" | **Operator Party** in *Setup Token Preapproval* | The utility-registry operator. |

When in doubt, look at the DAML source (linked above) — the field names there are authoritative; the UI labels are a humanised alias.
