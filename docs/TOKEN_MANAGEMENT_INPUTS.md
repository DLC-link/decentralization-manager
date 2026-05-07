# Token Management — Governance Action Inputs

This is the focused, semantic reference for the **token-management** governance actions: what each input field *means*, where the value comes from, and how it maps from the on-chain DAML template to the field you fill in on the UI.

Token management is implemented as **two governance plugins** that cooperate end-to-end:

| Plugin (DAML package) | Concern |
|---|---|
| [`governance-utility-onboarding`](../daml/governance-utility-onboarding/) | Onboard the governance party as a Provider/Registrar in the [Canton Network Utility Registry](https://docs.digitalasset.com/utilities/mainnet/index.html), tune its on-chain configuration, and *issue* tokens through the registry's `AllocationFactory` (Mint / Burn). |
| [`governance-token-custody`](../daml/governance-token-custody/) | Stand up incoming-transfer pre-approvals for the governance party (Canton Coin and utility tokens) and *move* tokens (Transfer / Accept Transfer). |

Together they cover thirteen governance actions. The rest of this document explains each one.

> **Demo values:** every action has a *Demo value* column left blank. Fill in the values used in the existing demo so the table doubles as a copy-paste cheat sheet.

> **Vocabulary warning:** several proper-noun terms (especially **Operator** and **Provider**) mean *different* things in the Utility Registry, in Splice / Canton Coin, and in the broader Canton governance context. The [Glossary](#glossary) and the [field-name disambiguation table](#field-name-disambiguation) at the end are essential reading before completing any form.

> **Reading the input tables.** Each action lists its inputs in a single table. The **UI field** column shows the on-screen label, with inline annotations in italics that describe the UI mechanics:
>
> - `(text, required)` — free-text input that blocks submission if empty.
> - `(text, optional)` — free-text input; empty is accepted.
> - `(decimal, required, > 0)` — numeric text input; the listed validation is enforced (here, on-chain via a DAML `ensure`).
> - `(checkbox, default ✓)` / `(checkbox, default ☐)` — boolean toggle and its initial state.
> - `(select: A / B / C, required)` — drop-down with the listed options.
> - `(textarea, multiline, …)` — multi-line input, plus any conditional-display rule.
> - `_(implicit)_` — set automatically by the form (e.g. the governance party from the *Governance Contract ID*); the user does not type a value.
> - `_(not in UI)_` — present in the DAML template but not exposed by the form; the backend supplies a default.
>
> The UI does **not** apply format checks to party-id / contract-id text fields — non-empty is the only client-side check. Substantive validation happens on the ledger when the proposal executes.

---

## Contents

- [Where things live](#where-things-live)
- [The thirteen actions, at a glance](#the-thirteen-actions-at-a-glance)
- [Glossary](#glossary)
  - [What the Registry Utility is](#what-the-registry-utility-is)
  - [The four Registry roles](#the-four-registry-roles)
  - [The onboarding contracts](#the-onboarding-contracts)
  - [The instrument-level contracts](#the-instrument-level-contracts)
  - [Reward / activity contracts](#reward--activity-contracts)
  - [Identifiers used in mint / burn / transfer](#identifiers-used-in-mint--burn--transfer)
  - [Cross-system terms (Splice / Canton Coin)](#cross-system-terms-splice--canton-coin)
- [Lifecycle: how the actions fit together](#lifecycle-how-the-actions-fit-together)
- [Phase 1 — Onboard as a registry participant](#phase-1--onboard-as-a-registry-participant)
- [Phase 2 — Tune the registry configuration](#phase-2--tune-the-registry-configuration)
- [Phase 3 — Set up incoming token paths](#phase-3--set-up-incoming-token-paths)
- [Phase 4 — Issue tokens](#phase-4--issue-tokens)
- [Phase 5 — Move tokens](#phase-5--move-tokens)
- [Field-name disambiguation](#field-name-disambiguation)

---

## Where things live

| Layer | Location |
|---|---|
| DAML package — onboarding & issuance | [daml/governance-utility-onboarding/](../daml/governance-utility-onboarding/) (`governance-utility-onboarding-v0-rc4`) |
| DAML package — pre-approvals & transfers | [daml/governance-token-custody/](../daml/governance-token-custody/) (`governance-token-custody-v0-rc4`) |
| UI | The actions and proposals in [frontend/src/components/GovernanceSection.tsx](../frontend/src/components/GovernanceSection.tsx). Most surface in the **Proposals** panel that is visible only when `governanceType === "core_self"`. |
| Upstream DARs | `utility-registry-v0`, `utility-registry-app-v0`, `utility-commercials-v0`, `utility-credential-app-v0`, `splice-api-token-*`, `splice-wallet`, `splice-api-featured-app-v1`. |
| Reference docs | [Canton Utilities — Mainnet](https://docs.digitalasset.com/utilities/mainnet/index.html); in particular the [Registry user guide](https://docs.digitalasset.com/utilities/mainnet/overview/registry-user-guide/index.html) and [Featured App Activity Markers](https://docs.digitalasset.com/utilities/mainnet/overview/registry-user-guide/activity-markers.html). |

---

## The thirteen actions, at a glance

Grouped by [lifecycle phase](#lifecycle-how-the-actions-fit-together).

| Phase | UI label | DAML template | DAML source |
|---|---|---|---|
| 1 — Onboard | Provision Provider Service | `ProvisionProviderService` | [ProvisionProviderService.daml](../daml/governance-utility-onboarding/daml/Governance/UtilityOnboarding/ProvisionProviderService.daml) |
| 1 — Onboard | Create Provider Service Request | `CreateProviderServiceRequest` | [CreateProviderServiceRequest.daml](../daml/governance-utility-onboarding/daml/Governance/UtilityOnboarding/CreateProviderServiceRequest.daml) |
| 1 — Onboard | Create User Service Request | `CreateUserServiceRequest` | [CreateUserServiceRequest.daml](../daml/governance-utility-onboarding/daml/Governance/UtilityOnboarding/CreateUserServiceRequest.daml) |
| 1 — Onboard | Create Delegated Batched Markers Proxy | `CreateDelegatedBatchedMarkersProxy` | [CreateDelegatedBatchedMarkersProxy.daml](../daml/governance-utility-onboarding/daml/Governance/UtilityOnboarding/CreateDelegatedBatchedMarkersProxy.daml) |
| 1 — Onboard | Setup Utility | `SetupUtility` (composite) | [SetupUtility.daml](../daml/governance-utility-onboarding/daml/Governance/UtilityOnboarding/SetupUtility.daml) |
| 2 — Tune | Set Enable Result Contracts | `SetEnableResultContracts` | [SetEnableResultContracts.daml](../daml/governance-utility-onboarding/daml/Governance/UtilityOnboarding/SetEnableResultContracts.daml) |
| 2 — Tune | Set Provider App Reward Beneficiaries | `SetProviderAppRewardBeneficiaries` | [SetProviderAppRewardBeneficiaries.daml](../daml/governance-utility-onboarding/daml/Governance/UtilityOnboarding/SetProviderAppRewardBeneficiaries.daml) |
| 3 — Receive | Setup CC Preapproval | `SetupCcPreapprovalProposal` | [SetupCcPreapproval.daml](../daml/governance-token-custody/daml/Governance/TokenCustody/SetupCcPreapproval.daml) |
| 3 — Receive | Setup Token Preapproval | `SetupTokenPreapprovalProposal` | [SetupTokenPreapproval.daml](../daml/governance-token-custody/daml/Governance/TokenCustody/SetupTokenPreapproval.daml) |
| 4 — Issue | Mint | `MintProposal` | [MintProposal.daml](../daml/governance-utility-onboarding/daml/Governance/TokenIssuance/MintProposal.daml) |
| 4 — Issue | Burn | `BurnProposal` | [BurnProposal.daml](../daml/governance-utility-onboarding/daml/Governance/TokenIssuance/BurnProposal.daml) |
| 5 — Move | Transfer | `TransferProposal` | [TransferProposal.daml](../daml/governance-token-custody/daml/Governance/TokenCustody/TransferProposal.daml) |
| 5 — Move | Accept Transfer | `AcceptTransferProposal` | [AcceptTransfer.daml](../daml/governance-token-custody/daml/Governance/TokenCustody/AcceptTransfer.daml) |

---

## Glossary

Quoted material is verbatim from the [Canton Utilities docs](https://docs.digitalasset.com/utilities/mainnet/index.html) unless marked otherwise.

### What the Registry Utility is

The Registry Utility is a "core application for asset tokenization, leveraging Canton and Daml technologies to securely register and manage assets." Unlike traditional closed registries that need an operator to mediate every action, the registry "enables users to perform actions—such as transfers, deposits, or withdrawals—directly through their local participant node, eliminating the need for operator intermediation."

Think of it as the on-chain plumbing for issuing and moving tokenized instruments — once a Registrar has set up an instrument, holders can mint, transfer, and burn through their own participant nodes, subject to credential checks.

### The four Registry roles

The same on-ledger party can play several of these roles at once.

| Role | Definition (verbatim, condensed) | Plain-English read |
|---|---|---|
| **Operator** | "Maintains Daml models and application configuration … provides various API endpoints for users … onboards Providers to the Registry Utility according to predefined criteria." | The platform admin running the Registry app. Gates who is allowed to be a Provider. |
| **Provider** | "Onboards Registrars to the Registry Utility according to predefined criteria … can provide automation services." Example: a financial-infrastructure provider. | A middleman that sits between the Operator and a Registrar. Often the legal entity offering the registry as a product. |
| **Registrar** | "Maintains ownership records for assets on behalf of Issuers … maintains criteria for holding, minting, and burning asset tokens." Example: SS&C, CSDs, custodians. | The party with day-to-day responsibility for a particular instrument's books. |
| **Holder** | "Any ledger party can send, receive, mint, and burn tokens, provided they fulfill the predefined criteria set by the registrar." | An end-user / treasury / issuer of the token. |

> In the actions below, **the decentralized governance party is being onboarded as a Provider and Registrar simultaneously**. The composite `SetupUtility` action runs the full chain: the governance party becomes its own provider *and* registrar of a single instrument.

### The onboarding contracts

| Term | What it is |
|---|---|
| **`ProviderServiceRequest`** | A request, created by a prospective provider (the user), to be onboarded by the Operator. Per the docs, "a user initiates by requesting a `ProviderService` contract … the Operator may accept the request, provided that the user's credentials satisfy their onboarding Credential requirements." Cancellable by the user; rejectable by the Operator. |
| **`ProviderService`** | The accepted contract. "Both parties become signatories." May be terminated at any time by either side. The durable on-chain record that the user is now a Provider under that Operator. |
| **`ProviderConfiguration`** | Created by exercising `ProviderService_CreateProviderConfiguration`. Holds the credential requirements that *this provider* will impose on Registrars and Holders it onboards. Created with empty requirements lists by `SetupUtility`. |
| **`RegistrarServiceRequest`** | The same pattern, one level down: a prospective registrar asks the provider to onboard them. Carries optional flags `createTransferRule` and `createAllocationFactory` that let the provider stand up the on-chain machinery the registrar will need at the same time. |
| **`RegistrarService`** | The accepted registrar contract. Has choices for setting flags (`RegistrarService_Set`) and creating per-instrument configuration (`RegistrarService_CreateInstrumentConfiguration`). |
| **`UserServiceRequest`** *(from `Utility.Credential.App.V0.Service.User`)* | The credential-utility analogue: a request to onboard an end-user under an Operator. Lives in the credentials package, not the registry package, but the governance plugin exposes it the same way. |

### The instrument-level contracts

| Term | What it is |
|---|---|
| **`InstrumentConfiguration`** | Created by a Registrar, "for each supported instrument," and "establishes an identifier for the instrument and defines the credential requirements for holding, minting, and burning its tokens." Carries `holderRequirements` (credentials needed for transfers) and `issuerRequirements` (credentials needed for mint/burn). "Once created, the configuration is explicitly disclosed by the operator backend." |
| **`TransferRule`** | A registrar-issued contract that authorizes the registry's transfer machinery for that instrument. Per the workflows page: "the registrar must have created a `TransferRule` contract instance" before any transfer can succeed. Created when the registrar accepts a `RegistrarServiceRequest` with `createTransferRule = Some True`. |
| **`AllocationFactory`** | A factory contract used to *issue* mint/burn offers and two-step transfers. Created when the registrar accepts a `RegistrarServiceRequest` with `createAllocationFactory = Some True`. Both `MintProposal` and `BurnProposal` call into `AllocationFactory_OfferMint` / `AllocationFactory_OfferBurn`. |
| **`TransferPreapproval`** *(from the Utility-Registry app)* | A receiver-issued contract that lets specific instrument transfers complete in one step instead of two. Per the registry docs, a preapproval covers "up to 10 instrument IDs." Created via the [Setup Token Preapproval](#setup-token-preapproval) action below. |
| **`TransferPreapproval`** *(from `Splice.Wallet`)* | The Canton-Coin equivalent: a receiver-issued contract specifically for Canton Coin (`splice-wallet`). Created via the two-step proposal flow that begins with [Setup CC Preapproval](#setup-cc-preapproval). The on-chain template is different (different package) — same name, different namespace. |

### Reward / activity contracts

| Term | What it is |
|---|---|
| **Featured App Activity Markers** | "The Utility Registry supports featured app activity markers, which reward parties for their participation in asset activity on the network." Concretely, `Splice.Amulet:FeaturedAppActivityMarker` contracts are minted when transfers / mints / burns occur, distributing a reward share to providers and operators by configured weights. |
| **`FeaturedAppRight`** | A prerequisite credential. "A provider in the Utility Registry is featured, i.e., holds a `Splice.Amulet:FeaturedAppRight`." A provider must hold one of these before it can earn markers. |
| **Provider App Reward Beneficiaries** | "A provider can further refine reward sharing for a given instrument by specifying beneficiaries in the instrument's `InstrumentConfiguration`." A list of `(party, weight)` pairs that splits the provider's reward share. |
| **`DelegatedBatchedMarkersProxy`** | An authorization contract whose comment in the codebase reads: "authorizes the operator to create batched activity markers on behalf of the provider." Lets the operator emit markers in batches without round-tripping authority back to the provider for each one. |
| **Result contracts** *(toggled via `enableResultContracts`)* | A registrar can opt in to having explicit *result* contracts emitted for completed registry operations (in addition to ledger events). Toggled `Some True` / `Some False` / `None` via `RegistrarService_Set`. The official docs do not yet have a glossary entry for this flag — confirm payload expectations with the integration team before relying on it. |

### Identifiers used in mint / burn / transfer

| Term | Type | Notes |
|---|---|---|
| **`InstrumentId`** | Record `{ admin : Party, id : Text }` | Splits in the UI into two fields: *Instrument Admin* (the issuing party) and *Instrument ID* (e.g. `CBTC`). |
| **`Mint` / `Burn`** records | `instrumentId`, `amount`, `holder`, `reference`, `requestedAt`, `executeBefore`, `meta` | Per the workflows page, mint/burn are "request/accept" workflows: the proposer asks; "only the registrar is authorized to accept or reject Mint requests" (resp. Burn). The proposer "has the ability to cancel it before acceptance." |
| **`Transfer`** record *(splice token API)* | `sender`, `receiver`, `instrument`, `amount`, `inputHoldingCids`, `requestedAt`, `executeBefore`, `meta` | Captured at proposal time, executed by `TransferFactory_Transfer` at execution time. |
| **`TransferInstruction`** *(splice token API)* | A pending incoming-transfer offer | Produced by the two-step transfer path; accepted by the receiver via [Accept Transfer](#accept-transfer). |
| **`ExtraArgs`** *(splice token API)* | An on-chain envelope for context/metadata | Currently *not exposed in the UI* for any action — sent as empty defaults. |

### Cross-system terms (Splice / Canton Coin)

These appear only in the [Setup CC Preapproval](#setup-cc-preapproval) flow and have nothing to do with the Utility Registry — but the field labels overlap with registry terms, which is the single most common source of confusion in this plugin.

| Term | What it is |
|---|---|
| **CC "provider"** | The Splice Wallet / Canton-Coin party that "provides and pays fees" for an incoming `TransferPreapproval`. **Not** a Utility-Registry Provider. The CC provider must accept the `TransferPreapprovalProposal` separately after the governance party creates it. |
| **DSO** | Decentralized Synchronizer Operator — the party operating the Splice synchronizer / Canton-Coin instance. Used as a safety check on the resulting CC `TransferPreapproval`. |

---

## Lifecycle: how the actions fit together

A common end-to-end path for a decentralized governance party that wants to issue, hold, and move a tokenized instrument:

1. **Onboard as a registry participant** — either run [Provision Provider Service](#provision-provider-service) (direct, when the proposer can act as Operator) or run [Create Provider Service Request](#create-provider-service-request) and have an external Operator accept off-plugin. Optionally create a [Delegated Batched Markers Proxy](#create-delegated-batched-markers-proxy) so the Operator can mint reward markers in batches.
2. **Stand up the registry machinery** — run the composite [Setup Utility](#setup-utility) (with `createTransferRule = True` and `createAllocationFactory = True`) to produce the `RegistrarService`, `TransferRule`, `AllocationFactory`, and a fresh `InstrumentConfiguration` in one proposal.
3. **Tune the on-chain configuration** as needed: toggle [Set Enable Result Contracts](#set-enable-result-contracts), distribute marker rewards via [Set Provider App Reward Beneficiaries](#set-provider-app-reward-beneficiaries).
4. **Open incoming-transfer paths**: stand up a CC pre-approval ([Setup CC Preapproval](#setup-cc-preapproval)) and a utility-token pre-approval ([Setup Token Preapproval](#setup-token-preapproval)) so that other parties can send tokens to the governance party in one step.
5. **Issue tokens** through the `AllocationFactory` via [Mint](#mint) and [Burn](#burn) proposals (these produce *offers* that the recipient/holder accepts separately).
6. **Move tokens** via [Transfer](#transfer) (governance party as sender) and [Accept Transfer](#accept-transfer) (governance party as receiver of a two-step offer).

The actions can be run independently in any order; the lifecycle above is just the canonical happy path.

---

## Phase 1 — Onboard as a registry participant

### Provision Provider Service

**Effect on success:** creates a `ProviderService` directly with `operator = proposer` and `provider = governanceParty`. This is **not** the same as accepting a `ProviderServiceRequest` — it short-circuits the request/accept dance because the proposer (a member that controls the registry-app) and the governance party are jointly creating the contract inside the executed proposal. Use this when the governance committee has already agreed offline that the proposer is the Operator.

> The DAML comment explains why this wrapper exists at all: a governance party is externally signed (threshold > 1), so a plain `create ProviderService` from a single submitter would fail authorization. The proposal carries both authorities into one transaction.

**UI form:** *New Proposal → Provision Provider Service*  
**Submit button:** *Submit Proposal*  
**Inputs:** none. The form shows a fixed message: "Provisions a Utility-Registry ProviderService with operator = proposer and provider = governance party. No parameters required."

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| _(implicit)_ Governance party | `governanceParty` | — | Auto from the *Governance Contract ID*. | n/a |
| _(implicit)_ Proposer | `proposer` | — | Auto from the logged-in member. The proposer **is** the resulting `operator` — they must control the Registry app. | n/a |

---

### Create Provider Service Request

**Effect on success:** creates a `ProviderServiceRequest` with the supplied operator + provider. The Operator must then accept it (off-plugin) to materialise a `ProviderService`. Use this when the governance party wants to be onboarded *as a provider* under an external Operator (the request/accept path).

**UI form:** *New Proposal → Create Provider Service Request*  
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| **Operator Party** *(text, required)* | `operator` | The Utility-Registry **Operator** party that will receive the request. | The party id of the entity running the Registry app you want to be onboarded into. | _(blank)_ |
| **Provider Party** *(text, required)* | `provider` | The party that will become a Provider on acceptance. Often the same as the governance party, but kept separate so the committee can request onboarding for any party. | Use the governance party id unless onboarding a third party. | _(blank)_ |
| _(implicit)_ Governance party | `governanceParty` | — | auto | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

> The UI's **New Governance Action → Create Provider Service** form (a single-input variant with only Operator) calls into a different code path that auto-fills `provider = governanceParty`. Both forms ultimately produce the same on-chain `ProviderServiceRequest`.

---

### Create User Service Request

**Effect on success:** creates a `UserServiceRequest` (from the **credential** utility, not the registry) with the supplied operator + user. The Operator can then accept it to onboard the user into the credential utility — the prerequisite for issuing/holding credentials.

**UI form:** *New Proposal → Create User Service Request*  
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| **Operator Party** *(text, required)* | `operator` | The Operator of the **credential utility** (typically the same as the registry operator, but conceptually distinct). | The party id of the entity running the credential app. | _(blank)_ |
| **User Party** *(text, required)* | `user` | The party that will become a User on acceptance. | Free choice — the end-user / holder party id. | _(blank)_ |
| _(implicit)_ Governance party | `governanceParty` | — | auto | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

---

### Create Delegated Batched Markers Proxy

**Effect on success:** creates a `DelegatedBatchedMarkersProxy` with `provider = governanceParty` and the supplied `operator`. After this contract exists, the operator may emit *batched* `FeaturedAppActivityMarker` contracts on the provider's behalf without having to obtain authority transaction-by-transaction. This is purely a delegation — it does not by itself produce any markers.

**UI form:** *New Proposal → Create Delegated Batched Markers Proxy*  
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| **Operator Party** *(text, required)* | `operator` | The party authorised to mint batched markers for the governance party. | Typically the same Operator party used in the Provider onboarding. | _(blank)_ |
| _(implicit)_ Provider | `provider` | — | Always set to `governanceParty` by the template. | n/a |
| _(implicit)_ Governance party | `governanceParty` | — | auto | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

---

### Setup Utility

*(composite onboarding action)*

**Effect on success:** runs the *entire* registry-onboarding chain in a single proposal. In order, `executeImpl` performs:

1. `ProviderService_CreateProviderConfiguration` — creates an empty-requirements `ProviderConfiguration` for the supplied `providerService`.
2. `create RegistrarServiceRequest` — proposes the governance party as both `provider` and `registrar`, with optional flags for `createTransferRule` and `createAllocationFactory`.
3. `ProviderService_AcceptRegistrarServiceRequest` — the same provider immediately accepts that request, producing a `RegistrarService` (and, depending on flags, a `TransferRule` and `AllocationFactory`).
4. `RegistrarService_CreateInstrumentConfiguration` — creates an `InstrumentConfiguration` for the supplied `instrumentIdText` with empty `issuerRequirements` / `holderRequirements`.

After this proposal executes, the governance party owns a fresh registrar service and a configured instrument; if the flags were on, it can also receive transfers (via `TransferRule`) and issue mint/burn offers (via `AllocationFactory`).

**UI form:** *New Proposal → Setup Utility*  
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| **ProviderService Contract ID** *(text, required)* | `providerServiceCid` | The CID of an existing `ProviderService` whose `provider = governanceParty`. Without one, the chain cannot start (step 1 needs it). | Created earlier by either *Provision Provider Service* (direct) or by *Create Provider Service Request* + Operator acceptance. | _(blank)_ |
| **Operator Party** *(text, required)* | `operator` | The party recorded as `operator` on the resulting `RegistrarServiceRequest` and downstream contracts. **In this composite the Operator is not the actor — the proposer is — but the Operator party is still pinned to the registrar.** | Typically the same Operator that issued the `ProviderService`. | _(blank)_ |
| **Instrument ID** *(text, required)* | `instrumentIdText` | The plain-text id for the new `InstrumentConfiguration` (e.g. `CBTC`). | Free choice; conventionally an uppercase token symbol. | _(blank)_ |
| **Create TransferRule** *(checkbox, default ✓)* | `createTransferRule` | `True` ⇒ the registrar service request is accepted with `createTransferRule = Some True`, producing a `TransferRule` for the instrument. | Default `True`; required for any transfers later. | `True` |
| **Create AllocationFactory** *(checkbox, default ✓)* | `createAllocationFactory` | `True` ⇒ also produces an `AllocationFactory`, which is required for mint, burn, and two-step transfers. | Default `True`; required for [Mint](#mint), [Burn](#burn), and the two-step path of [Transfer](#transfer). | `True` |
| _(implicit)_ Governance party | `governanceParty` | — | auto; will be both `provider` and `registrar` on the resulting contracts. | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

> **Trap:** the `issuerRequirements` and `holderRequirements` on the resulting `InstrumentConfiguration` are **empty**. That means no credential checks are enforced for mint / burn / hold of this instrument. If you want gated instruments, do *not* use this composite — drive each step manually so you can pass non-empty requirements.

---

## Phase 2 — Tune the registry configuration

### Set Enable Result Contracts

**Effect on success:** exercises `RegistrarService_Set` to flip the registrar's `enableResultContracts` flag. `Some True` switches result-contract emission on, `Some False` switches it off, `None` clears the field (reverting to whatever the default is at that level).

**UI form:** *New Proposal → Set Enable Result Contracts*  
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| **RegistrarService Contract ID** *(text, required)* | `registrarServiceCid` | The CID of the registrar service whose flag should be flipped. | Produced by `SetupUtility` (or by manually accepting a `RegistrarServiceRequest`). | _(blank)_ |
| **Enable Result Contracts** *(select: Enable / Disable / Clear (None), required)* | `enableResultContracts` | `Enable` → `Some True`; `Disable` → `Some False`; `Clear (None)` → `None`. | Operational decision — flip on if the integrator wants explicit result contracts. The Canton Utilities docs do not yet have a public glossary entry for this flag, so confirm payload expectations with the integration team before enabling in production. | `Some True` |
| _(implicit)_ Governance party | `governanceParty` | — | auto | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

---

### Set Provider App Reward Beneficiaries

**Effect on success:** exercises `InstrumentConfiguration_SetProviderAppRewardBeneficiaries` to (re)configure the list of parties (and weights) that will receive the *provider*'s share of `FeaturedAppActivityMarker`s for this instrument. Per the registry docs, "a provider can further refine reward sharing for a given instrument by specifying beneficiaries in the instrument's `InstrumentConfiguration`."

The DAML field is `Optional [AppRewardBeneficiary]`:

- `None` → clear the beneficiary list (the provider keeps the entire share).
- `Some []` → explicit empty list (semantically the same as `None`, but recorded as a deliberate choice).
- `Some [(party₁, w₁), (party₂, w₂), …]` → split the provider's share by weight.

**UI form:** *New Proposal → Set Provider App Reward Beneficiaries*  
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| **InstrumentConfiguration CID** *(text, required)* | `instrumentConfigurationCid` | CID of the `InstrumentConfiguration` whose beneficiaries you are tuning. | Produced by `SetupUtility` (step 4) or by `RegistrarService_CreateInstrumentConfiguration` directly. | _(blank)_ |
| **Clear beneficiaries (set to None)** *(checkbox, default ☐)* | `providerAppRewardBeneficiaries = None` | Tick to send `None` and hide the beneficiaries field. | Operational decision; tick when consolidating the provider's share back to itself. | unchecked |
| **Beneficiaries** *(textarea, multiline, hidden when "Clear" is ticked; required otherwise)* | `Some [AppRewardBeneficiary]` | One per line in the form `<party>,<weight>`. The UI parses each line into a beneficiary record. | Free choice; weights are decimals. The on-chain code does not enforce a weight sum, but downstream marker calculation expects them to be normalised. | `<party_a>,0.5`<br>`<party_b>,0.5` |
| _(implicit)_ Governance party | `governanceParty` | — | auto | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

> **Prerequisite:** the provider party must already hold a `Splice.Amulet:FeaturedAppRight`, otherwise no markers will be emitted regardless of beneficiaries.

---

## Phase 3 — Set up incoming token paths

### Setup CC Preapproval

**Effect on success:** creates a `TransferPreapprovalProposal` (from `Splice.Wallet`) where the governance party is the *receiver*. The CC **provider** must then separately accept that proposal, which is when fees are paid. After acceptance, anyone can transfer Canton Coin to the governance party in one step.

> This action lives in the **token-custody** package and uses Splice / Canton Coin contracts, not the Utility Registry. The "Provider Party" field below has nothing to do with a Utility-Registry [Provider](#the-four-registry-roles).

**UI form:** *New Proposal → Setup CC Preapproval*  
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| **Provider Party** *(text, required)* | `provider` | The Canton-Coin provider party that will pay the preapproval fee. *Not* a utility-registry provider. | Provided by the team operating the Splice / Canton-Coin instance. | _(blank)_ |
| **Expected DSO Party** *(text, required in UI; `Optional Party` in DAML)* | `expectedDso` | The DSO party id of the synchronizer / CC instance. UI marks this required even though DAML allows `None` — supply it. | DSO of the synchronizer; look up in `NetworkConfigAccordion` or the synchronizer admin docs. | _(blank)_ |
| _(implicit)_ Governance party | `governanceParty` | — | Decentralized-party id; auto-filled from the *Governance Contract ID*. | n/a |
| _(implicit)_ Proposer | `proposer` | — | Logged-in member; auto. | n/a |

> **Trap:** "Provider Party" is **not** the utility-registry provider used elsewhere in the governance UI; it is the Canton-Coin fee provider. If you copy the operator from `Setup Utility` here, the proposal will execute but the resulting `TransferPreapprovalProposal` will be rejected by that party.

---

### Setup Token Preapproval

**Effect on success:** creates a `TransferPreapproval` (from `utility-registry-app`) directly — no separate accept step — where the governance party is the *receiver*. After that, the registry permits incoming utility-token transfers to the governance party for the listed instruments — or for *all* instruments of `instrumentAdmin` if the allowance list is empty.

**UI form:** *New Proposal → Setup Token Preapproval*  
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| **Operator Party** *(text, required)* | `operator` | The utility-registry **Operator** party — the entity running the registry. Becomes an observer on the resulting `TransferPreapproval`. | Same Operator used in [Provision Provider Service](#provision-provider-service) / [Create Provider Service Request](#create-provider-service-request). | _(blank)_ |
| **Instrument Admin** *(text, required)* | `instrumentAdmin` | The party that *issues / administers* the token instruments. | E.g. the CBTC admin party. | _(blank)_ |
| _(not in UI)_ Instrument allowances | `instrumentAllowances` | A list of specific instruments to preapprove. Empty list = all instruments of the admin. | UI currently sends an empty list, so the preapproval covers **every** instrument issued by `instrumentAdmin`. | n/a |
| _(implicit)_ Governance party | `governanceParty` | — | auto | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

> **Gap:** the UI does not expose `instrumentAllowances`; the resulting preapproval is always for *all* instruments of the admin.

---

## Phase 4 — Issue tokens

### Mint

**Effect on success:** calls `AllocationFactory_OfferMint`, producing a `MintOffer` for the recipient. The recipient (a party with valid Issuer credentials *or* the registrar acting on their behalf) accepts the offer separately to actually create the holding. The proposer "has the ability to cancel it before acceptance by the registrar" (per the workflows doc).

The `ChoiceContext` includes `instrumentConfigurationContextKey → instrumentConfigurationCid` and `issuerCredentialsContextKey → []` (empty issuer-credentials list — the on-chain code will reject if the instrument requires non-empty credentials).

**UI form:** *New Proposal → Mint*  
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| **AllocationFactory CID** *(text, required)* | `allocationFactoryCid` | CID of the `AllocationFactory` for the registrar issuing this instrument. | Produced by [Setup Utility](#setup-utility) when `createAllocationFactory = True`, or by manually accepting a `RegistrarServiceRequest`. | _(blank)_ |
| **Instrument Admin** *(text, required)* | `instrumentId.admin` | The admin party of the instrument (the `admin` half of `InstrumentId`). | The same admin recorded on the `InstrumentConfiguration`. | _(blank)_ |
| **Instrument ID** *(text, required)* | `instrumentId.id` | The id half of `InstrumentId`. | Same as the `instrumentIdText` used in `SetupUtility`. | e.g. `CBTC` |
| **InstrumentConfiguration CID** *(text, required)* | `instrumentConfigurationCid` | CID of the `InstrumentConfiguration` for this instrument. Embedded in the choice context so the registry can validate against it. | Produced by [Setup Utility](#setup-utility) step 4 or by direct `RegistrarService_CreateInstrumentConfiguration`. | _(blank)_ |
| **Recipient Party** *(text, required)* | `recipient` | The party that will *hold* the new tokens. | Free choice — the holder. | _(blank)_ |
| **Amount** *(decimal text, required, > 0)* | `amount` | Decimal, > 0 (enforced on-chain via `ensure amount > 0.0`). | Free choice. | `1000` |
| **Description** *(text, required)* | `description` | Free-form text used both as the action's description label and as the `reference` on the resulting `Mint`. | Free text. | `Mint 1000 CBTC for liquidity pool` |
| _(not in UI)_ Requested at | `requestedAt` | Timestamp on the `Mint`. UI sets this to the proposal-submission time. | n/a — backend default | n/a |
| _(not in UI)_ Execute before | `executeBefore` | Deadline after which the mint cannot complete. UI sets this to a backend default (typically a few minutes after `requestedAt`). | n/a — backend default | n/a |
| _(not in UI)_ Meta / extra-args meta | `meta`, `extraArgsMeta` | Optional metadata maps. UI sends empty. | n/a | n/a |
| _(implicit)_ Governance party | `governanceParty` | — | auto; passed as `expectedAdmin` to the `AllocationFactory_OfferMint` choice. The on-chain code asserts the factory's admin matches. | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

> **Pre-flight:** issuer credentials are sent empty. If the `InstrumentConfiguration` was created with non-empty `issuerRequirements` (i.e. *not* via `SetupUtility`'s default), the mint offer will be rejected at acceptance time. Either keep the configuration's requirements empty or extend the plugin to accept credentials.

---

### Burn

**Effect on success:** calls `AllocationFactory_OfferBurn`, producing a `BurnOffer` for the holder. The holder accepts (separately) by supplying concrete `Holding` contract ids — the offer step itself does not consume holdings. Per the workflows doc, on holder acceptance "the specified amount of a Holding gets locked … and tokens are permanently removed from the registry"; cancellable by the proposer before acceptance, in which case "locked holdings are released back."

**UI form:** *New Proposal → Burn*  
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| **AllocationFactory CID** *(text, required)* | `allocationFactoryCid` | CID of the `AllocationFactory` for the registrar issuing this instrument. | Same as Mint. | _(blank)_ |
| **Instrument Admin** *(text, required)* | `instrumentId.admin` | Admin half of the instrument id. | Same as Mint. | _(blank)_ |
| **Instrument ID** *(text, required)* | `instrumentId.id` | Id half. | Same as Mint. | e.g. `CBTC` |
| **InstrumentConfiguration CID** *(text, required)* | `instrumentConfigurationCid` | CID of the `InstrumentConfiguration`. | Same as Mint. | _(blank)_ |
| **Holder Party** *(text, required)* | `holder` | The party from which tokens will be burned. They must own at least `amount` of unlocked holdings of the same `(admin, id)` at acceptance time. | Free choice — the holder. | _(blank)_ |
| **Amount** *(decimal text, required, > 0)* | `amount` | Decimal, > 0 (enforced on-chain). | Free choice. | `500` |
| **Description** *(text, required)* | `description` | Free-form text; doubles as the action description and the `reference` on the resulting `Burn`. | Free text. | `Burn 500 CBTC from vault` |
| _(not in UI)_ Requested at / Execute before / Meta | as above | UI sends backend defaults / empty. | n/a | n/a |
| _(implicit)_ Governance party | `governanceParty` | — | auto; passed as `expectedAdmin`. | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

---

## Phase 5 — Move tokens

### Transfer

**Effect on success:** runs `TransferFactory_Transfer` from the governance party. The behaviour depends on which kind of factory CID you supplied:

- **`TransferPreapproval` CID** → completes the transfer immediately (one-step). Use this when the receiver previously created a preapproval.
- **`AllocationFactory` CID** → creates a pending `TransferOffer` / `TransferInstruction` that the receiver must accept separately (two-step).

The on-chain `ensure transfer.amount > 0.0` enforces a positive amount.

**UI form:** *New Proposal → Transfer*  
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| **TransferFactory Contract ID** *(text, required)* | `transferFactoryCid` | A `TransferPreapproval` CID (one-step) **or** an `AllocationFactory` CID (two-step). | One-step: a preapproval previously created for `(instrumentAdmin, receiver)`. Two-step: the allocation factory CID returned when the registrar service was configured. | _(blank)_ |
| **Expected Admin Party** *(text, required)* | `expectedAdmin` | The admin party that should be on the chosen transfer factory. Safety check: on-chain code aborts if the live factory disagrees. | Same as **Instrument Admin** below. | _(blank)_ |
| **Receiver Party** *(text, required)* | `transfer.receiver` | The party that will receive the tokens. | The recipient's party id. | _(blank)_ |
| **Amount** *(decimal text, required, > 0)* | `transfer.amount` | Decimal amount; must be > 0. | Free choice. | e.g. `1000` |
| **Instrument Admin** *(text, required)* | `transfer.instrument.admin` | The admin party of the instrument being transferred. | Same admin used in [Setup Token Preapproval](#setup-token-preapproval) / vault. | _(blank)_ |
| **Instrument ID** *(text, required)* | `transfer.instrument.id` | Instrument identifier string. | E.g. `CBTC`. | _(blank)_ |
| **Input Holding CIDs** *(text, optional, comma-separated)* | `transfer.inputHoldingCids` | Comma-separated list of holding CIDs to spend. Leave empty to let the backend auto-select. | List the governance party's holdings of the same `(admin, id)`. | _(blank)_ |
| _(not in UI)_ Extra args | `extraArgs` | Context/metadata envelope. | UI sends empty; backend default. | n/a |
| _(implicit)_ Governance party | `governanceParty` | — | auto | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

> **UTXO timing risk** (per the DAML comment): the input holdings are captured at *proposal* time. If any of them gets spent before the proposal reaches its threshold and executes, execution fails. Mitigations:
> - reserve dedicated holdings for governance proposals,
> - keep the governance confirmation timeout short,
> - re-propose if holdings change.

---

### Accept Transfer

**Effect on success:** exercises `TransferInstruction_Accept` on the supplied `TransferInstruction`. The governance party must be the *receiver* on that instruction — the on-chain authority chain requires it.

**UI form:** *New Proposal → Accept Transfer*  
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| **TransferInstruction Contract ID** *(text, required)* | `transferInstructionCid` | The CID of the pending `TransferInstruction` whose receiver is the governance party. | Find by listing pending TransferInstructions visible to the governance party (e.g. via the `ContractsDialog` or by querying the ledger). | _(blank)_ |
| _(not in UI)_ Extra args | `extraArgs` | Context/metadata for the accept choice. | UI sends empty; backend default. | n/a |
| _(implicit)_ Governance party | `governanceParty` | — | auto | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

> **Trap:** the sender of a two-step transfer can withdraw the `TransferInstruction` before the governance accept proposal completes. If they do, execution fails with a contract-not-found error. Reproduce this for QA by withdrawing the offer between *propose* and *threshold reached*.

---

## Field-name disambiguation

The same word is used in different roles across these actions. Use the following table as a tie-breaker when reading or completing a form.

| Term in conversation | Meaning here | Action(s) where it appears |
|---|---|---|
| "Operator" | The **Utility-Registry Operator** — the entity running the Registry app. **Not** the Splice DSO operator. | [Provision Provider Service](#provision-provider-service) (proposer = operator), [Create Provider Service Request](#create-provider-service-request), [Create User Service Request](#create-user-service-request), [Create Delegated Batched Markers Proxy](#create-delegated-batched-markers-proxy), [Setup Utility](#setup-utility), [Setup Token Preapproval](#setup-token-preapproval) |
| "Provider" *(registry)* | The **Utility-Registry Provider** — onboarded by an Operator; onboards Registrars. | [Provision Provider Service](#provision-provider-service), [Create Provider Service Request](#create-provider-service-request), [Create Delegated Batched Markers Proxy](#create-delegated-batched-markers-proxy) |
| "Provider" *(Canton Coin)* | The **Splice Wallet / CC fee provider** — pays for the receiving preapproval. **Different concept, different package.** | [Setup CC Preapproval](#setup-cc-preapproval) |
| "User" | The **Credential-utility user** — onboarded by a credential-app Operator. Distinct from the Registry's "Holder." | [Create User Service Request](#create-user-service-request) |
| "Holder" | A registry party that owns / mints / burns tokens of an instrument. Subject to the instrument's `holderRequirements`. | [Burn](#burn) (the holder is the source of the burn) |
| "Recipient" | The receiving party for a Mint. The newly minted holding will be created for them. | [Mint](#mint) |
| "Receiver" | The receiving party of a Transfer. | [Transfer](#transfer) |
| "Beneficiary" | A `(party, weight)` entry on `InstrumentConfiguration.providerAppRewardBeneficiaries`. Splits the provider's marker reward share. | [Set Provider App Reward Beneficiaries](#set-provider-app-reward-beneficiaries) |
| "Service contract id" | Depending on the action: a `ProviderService` CID ([Setup Utility](#setup-utility)), a `RegistrarService` CID ([Set Enable Result Contracts](#set-enable-result-contracts)), an `InstrumentConfiguration` CID ([Set Provider App Reward Beneficiaries](#set-provider-app-reward-beneficiaries) / [Mint](#mint) / [Burn](#burn)), an `AllocationFactory` or `TransferPreapproval` CID ([Transfer](#transfer)), or a `TransferInstruction` CID ([Accept Transfer](#accept-transfer)). | Most actions that take a CID input |
| "AllocationFactory" | A registry-side factory used to produce mint/burn offers and two-step transfers. | [Mint](#mint), [Burn](#burn), [Transfer](#transfer) (two-step path) |
| "TransferRule" | A registrar-issued contract that enables transfers of a given instrument. Created by [Setup Utility](#setup-utility) when `createTransferRule = True`. | Setup Utility (flag), referenced indirectly by every transfer downstream |
| "TransferPreapproval" *(registry)* | A receiver-issued contract enabling one-step utility-token transfers. | [Setup Token Preapproval](#setup-token-preapproval) (creates), [Transfer](#transfer) (consumes via factory CID) |
| "TransferPreapproval" *(Canton Coin)* | The Splice-Wallet equivalent for Canton Coin; **different on-chain template**. | [Setup CC Preapproval](#setup-cc-preapproval) (creates the proposal; the CC provider accepts it separately to materialise the actual `TransferPreapproval`) |
| "Markers" | `FeaturedAppActivityMarker` reward contracts emitted per qualifying activity. | [Create Delegated Batched Markers Proxy](#create-delegated-batched-markers-proxy) (delegation), [Set Provider App Reward Beneficiaries](#set-provider-app-reward-beneficiaries) (split rules) |
| "Result contracts" | An optional alternative to ledger-event-only signalling on a registrar service. The official docs do not yet expose a glossary entry — confirm semantics with the integration team before relying on this flag. | [Set Enable Result Contracts](#set-enable-result-contracts) |
| "DSO" | Decentralized Synchronizer Operator — the party operating the Splice synchronizer. **Not** related to the Utility Registry. | [Setup CC Preapproval](#setup-cc-preapproval) |

> If a UI label and a DAML field name disagree, the DAML field name in the linked source files is authoritative.
