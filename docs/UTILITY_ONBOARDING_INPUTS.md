# Utility-Onboarding Plugin — Governance Action Inputs

This is the focused, semantic reference for the **`governance-utility-onboarding`** plugin: what each input field *means*, where the value comes from, and how it maps from the on-chain DAML template to the field you fill in on the UI.

The plugin wraps two concerns that share the same DAML package:

1. **Onboarding the governance party as a participant in the [Canton Network Utility Registry](https://docs.digitalasset.com/utilities/mainnet/index.html)** — i.e. having it provisioned as a Provider, having a Registrar service stood up, and tuning the resulting on-chain configuration (the `UtilityOnboarding` namespace).
2. **Issuing tokens through that registry** — Mint and Burn proposals routed through the registry's `AllocationFactory` (the `TokenIssuance` namespace).

> **Demo values:** every action has a *Demo value* column left blank. Fill in the values used in the existing demo so the table doubles as a copy-paste cheat sheet.

> **Vocabulary:** all proper-noun terms below (Operator, Provider, Registrar, Holder, AllocationFactory, etc.) are **Utility-Registry** concepts. They mean different things from the same words in the broader Canton/governance context — see the [Glossary](#glossary-utility-registry-concepts) before reading the action tables.

---

## Where things live

| Layer | Location |
|---|---|
| DAML package | [daml/governance-utility-onboarding/](../daml/governance-utility-onboarding/) (`governance-utility-onboarding-v0-rc4`) |
| UI | The actions and proposals in [frontend/src/components/GovernanceSection.tsx](../frontend/src/components/GovernanceSection.tsx). Most of these surface in the **Proposals** panel that is visible only when `governanceType === "core_self"`. |
| Upstream DARs | `utility-registry-v0`, `utility-registry-app-v0`, `utility-commercials-v0`, `utility-credential-app-v0`, plus the splice token APIs. |
| Reference docs | [Canton Utilities — Mainnet](https://docs.digitalasset.com/utilities/mainnet/index.html); in particular the [Registry user guide](https://docs.digitalasset.com/utilities/mainnet/overview/registry-user-guide/index.html) and [Featured App Activity Markers](https://docs.digitalasset.com/utilities/mainnet/overview/registry-user-guide/activity-markers.html). |

The nine actions:

| UI label | DAML template | DAML source |
|---|---|---|
| Provision Provider Service | `ProvisionProviderService` | [ProvisionProviderService.daml](../daml/governance-utility-onboarding/daml/Governance/UtilityOnboarding/ProvisionProviderService.daml) |
| Create Provider Service Request | `CreateProviderServiceRequest` | [CreateProviderServiceRequest.daml](../daml/governance-utility-onboarding/daml/Governance/UtilityOnboarding/CreateProviderServiceRequest.daml) |
| Create User Service Request | `CreateUserServiceRequest` | [CreateUserServiceRequest.daml](../daml/governance-utility-onboarding/daml/Governance/UtilityOnboarding/CreateUserServiceRequest.daml) |
| Create Delegated Batched Markers Proxy | `CreateDelegatedBatchedMarkersProxy` | [CreateDelegatedBatchedMarkersProxy.daml](../daml/governance-utility-onboarding/daml/Governance/UtilityOnboarding/CreateDelegatedBatchedMarkersProxy.daml) |
| Setup Utility | `SetupUtility` (composite) | [SetupUtility.daml](../daml/governance-utility-onboarding/daml/Governance/UtilityOnboarding/SetupUtility.daml) |
| Set Enable Result Contracts | `SetEnableResultContracts` | [SetEnableResultContracts.daml](../daml/governance-utility-onboarding/daml/Governance/UtilityOnboarding/SetEnableResultContracts.daml) |
| Set Provider App Reward Beneficiaries | `SetProviderAppRewardBeneficiaries` | [SetProviderAppRewardBeneficiaries.daml](../daml/governance-utility-onboarding/daml/Governance/UtilityOnboarding/SetProviderAppRewardBeneficiaries.daml) |
| Mint | `MintProposal` | [MintProposal.daml](../daml/governance-utility-onboarding/daml/Governance/TokenIssuance/MintProposal.daml) |
| Burn | `BurnProposal` | [BurnProposal.daml](../daml/governance-utility-onboarding/daml/Governance/TokenIssuance/BurnProposal.daml) |

---

## Glossary (Utility-Registry concepts)

Quoted material is verbatim from the Canton Utilities docs (linked above) unless marked otherwise.

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

> In this plugin, **the decentralized governance party is being onboarded as a Provider and Registrar simultaneously**. The composite `SetupUtility` action runs the full chain: the governance party becomes its own provider *and* registrar of a single instrument.

### The onboarding contracts

| Term | What it is |
|---|---|
| **`ProviderServiceRequest`** | A request, created by a prospective provider (the user), to be onboarded by the Operator. Per the docs, "a user initiates by requesting a `ProviderService` contract … the Operator may accept the request, provided that the user's credentials satisfy their onboarding Credential requirements." Cancellable by the user; rejectable by the Operator. |
| **`ProviderService`** | The accepted contract. "Both parties become signatories." May be terminated at any time by either side. This is the durable on-chain record that the user is now a Provider under that Operator. |
| **`ProviderConfiguration`** | Created by exercising `ProviderService_CreateProviderConfiguration`. Holds the credential requirements that *this provider* will impose on Registrars and Holders it onboards. (Created with empty requirements lists by `SetupUtility`.) |
| **`RegistrarServiceRequest`** | The same pattern, one level down: a prospective registrar asks the provider to onboard them. Carries optional flags `createTransferRule` and `createAllocationFactory` that let the provider stand up the on-chain machinery the registrar will need at the same time. |
| **`RegistrarService`** | The accepted registrar contract. Has choices for setting flags (`RegistrarService_Set`) and creating per-instrument configuration (`RegistrarService_CreateInstrumentConfiguration`). |
| **`UserServiceRequest`** *(from `Utility.Credential.App.V0.Service.User`)* | The credential-utility analogue: a request to onboard an end-user under an Operator. Lives in the credentials package, not the registry package, but the governance plugin exposes it the same way. |

### The instrument-level contracts

| Term | What it is |
|---|---|
| **`InstrumentConfiguration`** | Created by a Registrar, "for each supported instrument," and "establishes an identifier for the instrument and defines the credential requirements for holding, minting, and burning its tokens." Carries `holderRequirements` (credentials needed for transfers) and `issuerRequirements` (credentials needed for mint/burn). "Once created, the configuration is explicitly disclosed by the operator backend." |
| **`TransferRule`** | A registrar-issued contract that authorizes the registry's transfer machinery for that instrument. Per the workflows page: "the registrar must have created a `TransferRule` contract instance" before any transfer can succeed. Created when the registrar accepts a `RegistrarServiceRequest` with `createTransferRule = Some True`. |
| **`AllocationFactory`** | A factory contract used to *issue* mint/burn offers and two-step transfers. Created when the registrar accepts a `RegistrarServiceRequest` with `createAllocationFactory = Some True`. The plugin's `MintProposal` and `BurnProposal` both call into `AllocationFactory_OfferMint` / `AllocationFactory_OfferBurn`. |
| **`TransferPreapproval`** | A receiver-issued contract, set up via the **token-custody** plugin (see [TOKEN_CUSTODY_INPUTS.md](TOKEN_CUSTODY_INPUTS.md)), that lets specific instrument transfers complete in one step instead of two. Per the registry docs, a preapproval covers "up to 10 instrument IDs." |

### Reward / activity contracts

| Term | What it is |
|---|---|
| **Featured App Activity Markers** | "The Utility Registry supports featured app activity markers, which reward parties for their participation in asset activity on the network." Concretely, `Splice.Amulet:FeaturedAppActivityMarker` contracts are minted when transfers / mints / burns occur, distributing a reward share to providers and operators by configured weights. |
| **`FeaturedAppRight`** | A prerequisite credential. "A provider in the Utility Registry is featured, i.e., holds a `Splice.Amulet:FeaturedAppRight`." A provider must hold one of these before it can earn markers. |
| **Provider App Reward Beneficiaries** | "A provider can further refine reward sharing for a given instrument by specifying beneficiaries in the instrument's `InstrumentConfiguration`." A list of `(party, weight)` pairs that splits the provider's reward share. |
| **`DelegatedBatchedMarkersProxy`** | An authorization contract whose comment in the codebase reads: "authorizes the operator to create batched activity markers on behalf of the provider." Lets the operator emit markers in batches without round-tripping authority back to the provider for each one. |
| **Result contracts** *(toggled via `enableResultContracts`)* | A registrar can opt in to having explicit *result* contracts emitted for completed registry operations (in addition to ledger events). Toggled `Some True` / `Some False` / `None` via `RegistrarService_Set`. The official docs do not yet have a glossary entry for this flag — use sparingly until the team has confirmed the on-chain payload shape. |

### Identifiers used in mint/burn/transfer

| Term | Type | Notes |
|---|---|---|
| **`InstrumentId`** | Record `{ admin : Party, id : Text }` | Splits in the UI into two fields: *Instrument Admin* (the issuing party) and *Instrument ID* (e.g. `CBTC`). |
| **`Mint` / `Burn`** records | `instrumentId`, `amount`, `holder`, `reference`, `requestedAt`, `executeBefore`, `meta` | Per the workflows page, mint/burn are "request/accept" workflows: the proposer asks; "only the registrar is authorized to accept or reject Mint requests" (resp. Burn). The proposer "has the ability to cancel it before acceptance." |

---

## Action 1 — Provision Provider Service

**Effect on success:** creates a `ProviderService` directly with `operator = proposer` and `provider = governanceParty`. This is **not** the same as accepting a `ProviderServiceRequest` — it short-circuits the request/accept dance because the proposer (a member that controls the registry-app) and the governance party are jointly creating the contract inside the executed proposal. Use this when the governance committee has already agreed offline that the proposer is the Operator.

> The DAML comment explains why this wrapper exists at all: a governance party is externally signed (threshold > 1), so a plain `create ProviderService` from a single submitter would fail authorization. The proposal carries both authorities into one transaction.

**UI form:** *New Proposal → Provision Provider Service*
**Submit button:** *Submit Proposal*
**Inputs:** none (shows a fixed message: "Provisions a Utility-Registry ProviderService with operator = proposer and provider = governance party. No parameters required.")

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| _(implicit)_ Governance party | `governanceParty` | — | Auto from the *Governance Contract ID*. | n/a |
| _(implicit)_ Proposer | `proposer` | — | Auto from the logged-in member. The proposer **is** the resulting `operator` — they must control the Registry app. | n/a |

---

## Action 2 — Create Provider Service Request

**Effect on success:** creates a `ProviderServiceRequest` with the supplied operator + provider. The Operator must then accept it (off-plugin) to materialise a `ProviderService`. Use this when the governance party wants to be onboarded *as a provider* under an external Operator (the request/accept path).

**UI form:** *New Proposal → Create Provider Service Request*
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| Operator Party | `operator` | The Utility-Registry **Operator** party that will receive the request. | The party id of the entity running the Registry app you want to be onboarded into. | _(blank)_ |
| Provider Party | `provider` | The party that will become a Provider on acceptance. Often the same as the governance party, but kept separate so the committee can request onboarding for any party. | Use the governance party id unless onboarding a third party. | _(blank)_ |
| _(implicit)_ Governance party | `governanceParty` | — | auto | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

> The UI's **New Governance Action → Create Provider Service** form (a single-input variant with only Operator) calls into a different code path that auto-fills `provider = governanceParty`. Both forms ultimately produce the same on-chain `ProviderServiceRequest`.

---

## Action 3 — Create User Service Request

**Effect on success:** creates a `UserServiceRequest` (from the **credential** utility, not the registry) with the supplied operator + user. The Operator can then accept it to onboard the user into the credential utility — the prerequisite for issuing/holding credentials.

**UI form:** *New Proposal → Create User Service Request*
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| Operator Party | `operator` | The Operator of the **credential utility** (typically the same as the registry operator, but conceptually distinct). | The party id of the entity running the credential app. | _(blank)_ |
| User Party | `user` | The party that will become a User on acceptance. | Free choice — the end-user / holder party id. | _(blank)_ |
| _(implicit)_ Governance party | `governanceParty` | — | auto | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

---

## Action 4 — Create Delegated Batched Markers Proxy

**Effect on success:** creates a `DelegatedBatchedMarkersProxy` with `provider = governanceParty` and the supplied `operator`. After this contract exists, the operator may emit *batched* `FeaturedAppActivityMarker` contracts on the provider's behalf without having to obtain authority transaction-by-transaction. This is purely a delegation — it does not by itself produce any markers.

**UI form:** *New Proposal → Create Delegated Batched Markers Proxy*
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| Operator Party | `operator` | The party authorised to mint batched markers for the governance party. | Typically the same Operator party used in the Provider onboarding. | _(blank)_ |
| _(implicit)_ Provider | `provider` | — | Always set to `governanceParty` by the template. | n/a |
| _(implicit)_ Governance party | `governanceParty` | — | auto | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

---

## Action 5 — Setup Utility (composite)

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
| ProviderService Contract ID | `providerServiceCid` | The CID of an existing `ProviderService` whose `provider = governanceParty`. Without one, the chain cannot start (step 1 needs it). | Created earlier by either *Provision Provider Service* (direct) or by *Create Provider Service Request* + Operator acceptance. | _(blank)_ |
| Operator Party | `operator` | The party recorded as `operator` on the resulting `RegistrarServiceRequest` and downstream contracts. **In this composite the Operator is not the actor — the proposer is — but the Operator party is still pinned to the registrar.** | Typically the same Operator that issued the `ProviderService`. | _(blank)_ |
| Instrument ID | `instrumentIdText` | The plain-text id for the new `InstrumentConfiguration` (e.g. `CBTC`). | Free choice; conventionally an uppercase token symbol. | _(blank)_ |
| Create TransferRule | `createTransferRule` | `True` ⇒ the registrar service request is accepted with `createTransferRule = Some True`, producing a `TransferRule` for the instrument. | Default `True`; required for any transfers later. | `True` |
| Create AllocationFactory | `createAllocationFactory` | `True` ⇒ also produces an `AllocationFactory`, which is required for mint, burn, and two-step transfers. | Default `True`; required for the Mint / Burn proposals below. | `True` |
| _(implicit)_ Governance party | `governanceParty` | — | auto; will be both `provider` and `registrar` on the resulting contracts. | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

> **Trap:** the `issuerRequirements` and `holderRequirements` on the resulting `InstrumentConfiguration` are **empty**. That means no credential checks are enforced for mint / burn / hold of this instrument. If you want gated instruments, do *not* use this composite — drive each step manually so you can pass non-empty requirements.

---

## Action 6 — Set Enable Result Contracts

**Effect on success:** exercises `RegistrarService_Set` to flip the registrar's `enableResultContracts` flag. `Some True` switches result-contract emission on, `Some False` switches it off, `None` clears the field (reverting to whatever the default is at that level).

**UI form:** *New Proposal → Set Enable Result Contracts*
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| RegistrarService Contract ID | `registrarServiceCid` | The CID of the registrar service whose flag should be flipped. | Produced by `SetupUtility` (or by manually accepting a `RegistrarServiceRequest`). | _(blank)_ |
| Enable Result Contracts | `enableResultContracts` | `Enable` → `Some True`; `Disable` → `Some False`; `Clear (None)` → `None`. | Operational decision — flip on if the integrator wants explicit result contracts. The Canton Utilities docs do not yet have a public glossary entry for this flag, so confirm payload expectations with the team before enabling in production. | `Some True` |
| _(implicit)_ Governance party | `governanceParty` | — | auto | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

---

## Action 7 — Set Provider App Reward Beneficiaries

**Effect on success:** exercises `InstrumentConfiguration_SetProviderAppRewardBeneficiaries` to (re)configure the list of parties (and weights) that will receive the *provider*'s share of `FeaturedAppActivityMarker`s for this instrument. Per the registry docs, "a provider can further refine reward sharing for a given instrument by specifying beneficiaries in the instrument's `InstrumentConfiguration`."

The DAML field is `Optional [AppRewardBeneficiary]`:

- `None` → clear the beneficiary list (the provider keeps the entire share).
- `Some []` → explicit empty list (semantically the same as `None`, but recorded as a deliberate choice).
- `Some [(party₁, w₁), (party₂, w₂), …]` → split the provider's share by weight.

**UI form:** *New Proposal → Set Provider App Reward Beneficiaries*
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| InstrumentConfiguration CID | `instrumentConfigurationCid` | CID of the `InstrumentConfiguration` whose beneficiaries you are tuning. | Produced by `SetupUtility` (step 4) or by `RegistrarService_CreateInstrumentConfiguration` directly. | _(blank)_ |
| Clear beneficiaries (set to None) | `providerAppRewardBeneficiaries = None` | Tick to send `None` and hide the beneficiaries field. | Operational decision; tick when consolidating the provider's share back to itself. | unchecked |
| Beneficiaries | `Some [AppRewardBeneficiary]` | One per line in the form `<party>,<weight>`. The UI parses each line into a beneficiary record. | Free choice; weights are decimals. The on-chain code does not enforce a weight sum, but downstream marker calculation expects them to be normalised. | `<party_a>,0.5`<br>`<party_b>,0.5` |
| _(implicit)_ Governance party | `governanceParty` | — | auto | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

> **Prerequisite:** the provider party must already hold a `Splice.Amulet:FeaturedAppRight`, otherwise no markers will be emitted regardless of beneficiaries.

---

## Action 8 — Mint

**Effect on success:** calls `AllocationFactory_OfferMint`, producing a `MintOffer` for the recipient. The recipient (a party with valid Issuer credentials *or* the registrar acting on their behalf) accepts the offer separately to actually create the holding. The proposer "has the ability to cancel it before acceptance by the registrar" (per the workflows doc).

The `ChoiceContext` includes `instrumentConfigurationContextKey → instrumentConfigurationCid` and `issuerCredentialsContextKey → []` (empty issuer credentials list — the on-chain code will reject if the instrument requires non-empty credentials).

**UI form:** *New Proposal → Mint*
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| AllocationFactory CID | `allocationFactoryCid` | CID of the `AllocationFactory` for the registrar issuing this instrument. | Produced by `SetupUtility` when `createAllocationFactory = True`, or by manually accepting a `RegistrarServiceRequest`. | _(blank)_ |
| Instrument Admin | `instrumentId.admin` | The admin party of the instrument (the `admin` half of `InstrumentId`). | The same admin recorded on the `InstrumentConfiguration`. | _(blank)_ |
| Instrument ID | `instrumentId.id` | The id half of `InstrumentId`. | Same as the `instrumentIdText` used in `SetupUtility`. | e.g. `CBTC` |
| InstrumentConfiguration CID | `instrumentConfigurationCid` | CID of the `InstrumentConfiguration` for this instrument. Embedded in the choice context so the registry can validate against it. | Produced by `SetupUtility` step 4 or by direct `RegistrarService_CreateInstrumentConfiguration`. | _(blank)_ |
| Recipient Party | `recipient` | The party that will *hold* the new tokens. | Free choice — the holder. | _(blank)_ |
| Amount | `amount` | Decimal, > 0 (enforced on-chain via `ensure amount > 0.0`). | Free choice. | `1000` |
| Description | `description` | Free-form text used both as the action's description label and as the `reference` on the resulting `Mint`. | Free text. | `Mint 1000 CBTC for liquidity pool` |
| _(not in UI)_ Requested at | `requestedAt` | Time stamp on the `Mint`. UI sets this to the proposal-submission time. | n/a — backend default | n/a |
| _(not in UI)_ Execute before | `executeBefore` | Deadline after which the mint cannot complete. UI sets this to a backend-default (typically a few minutes after `requestedAt`). | n/a — backend default | n/a |
| _(not in UI)_ Meta / extra-args meta | `meta`, `extraArgsMeta` | Optional metadata maps. UI sends empty. | n/a | n/a |
| _(implicit)_ Governance party | `governanceParty` | — | auto; passed as `expectedAdmin` to the `AllocationFactory_OfferMint` choice. The on-chain code asserts the factory's admin matches. | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

> **Pre-flight:** issuer credentials are sent empty. If the `InstrumentConfiguration` was created with non-empty `issuerRequirements` (i.e. *not* via `SetupUtility`'s default), the mint offer will be rejected at acceptance time. Either keep the configuration's requirements empty or extend the plugin to accept credentials.

---

## Action 9 — Burn

**Effect on success:** calls `AllocationFactory_OfferBurn`, producing a `BurnOffer` for the holder. The holder accepts (separately) by supplying concrete `Holding` contract ids — the offer step itself does not consume holdings. Per the workflows doc, on holder acceptance "the specified amount of a Holding gets locked … and tokens are permanently removed from the registry"; cancellable by the proposer before acceptance, in which case "locked holdings are released back."

**UI form:** *New Proposal → Burn*
**Submit button:** *Submit Proposal*

| UI field | DAML field | What to put in | Source / how to obtain | Demo value |
|---|---|---|---|---|
| AllocationFactory CID | `allocationFactoryCid` | CID of the `AllocationFactory` for the registrar issuing this instrument. | Same as Mint. | _(blank)_ |
| Instrument Admin | `instrumentId.admin` | Admin half of the instrument id. | Same as Mint. | _(blank)_ |
| Instrument ID | `instrumentId.id` | Id half. | Same as Mint. | e.g. `CBTC` |
| InstrumentConfiguration CID | `instrumentConfigurationCid` | CID of the `InstrumentConfiguration`. | Same as Mint. | _(blank)_ |
| Holder Party | `holder` | The party from which tokens will be burned. They must own at least `amount` of unlocked holdings of the same `(admin, id)` at acceptance time. | Free choice — the holder. | _(blank)_ |
| Amount | `amount` | Decimal, > 0 (enforced on-chain). | Free choice. | `500` |
| Description | `description` | Free-form text; doubles as the action description and the `reference` on the resulting `Burn`. | Free text. | `Burn 500 CBTC from vault` |
| _(not in UI)_ Requested at / Execute before / Meta | as above | UI sends backend defaults / empty. | n/a | n/a |
| _(implicit)_ Governance party | `governanceParty` | — | auto; passed as `expectedAdmin`. | n/a |
| _(implicit)_ Proposer | `proposer` | — | auto | n/a |

---

## Field-name disambiguation

The same word is used in different roles across this plugin. Use the following table as a tie-breaker when reading or completing a form.

| Term in conversation | Meaning here | Action(s) where it appears |
|---|---|---|
| "Operator" | The **Utility-Registry Operator** — the entity running the Registry app. **Not** the Splice DSO operator. | Provision Provider Service (proposer = operator), Create Provider Service Request, Create User Service Request, Create Delegated Batched Markers Proxy, Setup Utility |
| "Provider" | The **Utility-Registry Provider** — onboarded by an Operator; onboards Registrars. **Not** the Splice Wallet "provider" used in CC pre-approval (see [TOKEN_CUSTODY_INPUTS.md](TOKEN_CUSTODY_INPUTS.md)). | Provision Provider Service, Create Provider Service Request, Create Delegated Batched Markers Proxy |
| "User" | The **Credential-utility user** — onboarded by a credential-app Operator. Distinct from the Registry's "Holder." | Create User Service Request |
| "Holder" | A registry party that owns / mints / burns tokens of an instrument. Subject to the instrument's `holderRequirements`. | Burn (the holder is the source of the burn) |
| "Recipient" | The receiving party for a Mint. The newly minted holding will be created for them. | Mint |
| "Beneficiary" | A `(party, weight)` entry on `InstrumentConfiguration.providerAppRewardBeneficiaries`. Splits the provider's marker reward share. | Set Provider App Reward Beneficiaries |
| "Service contract id" | Depending on the action: a `ProviderService` CID (Setup Utility), a `RegistrarService` CID (Set Enable Result Contracts), or an `InstrumentConfiguration` CID (Set Provider App Reward Beneficiaries / Mint / Burn). | All actions that take a CID input |
| "AllocationFactory" | A registry-side factory used to produce mint/burn offers and two-step transfers. | Mint, Burn (also used by *Transfer* in the token-custody plugin) |
| "TransferRule" | A registrar-issued contract that enables transfers of a given instrument. Created by `SetupUtility` when `createTransferRule = True`. | Setup Utility (flag), referenced indirectly by every transfer downstream |
| "Markers" | `FeaturedAppActivityMarker` reward contracts emitted per qualifying activity. | Create Delegated Batched Markers Proxy (delegation), Set Provider App Reward Beneficiaries (split rules) |
| "Result contracts" | An optional alternative to ledger-event-only signalling on a registrar service. Toggled by `SetEnableResultContracts`. The official docs do not yet expose a glossary entry — confirm semantics with the integration team before relying on this flag. | Set Enable Result Contracts |

> If a UI label and a DAML field name disagree, the DAML field name in the linked source files is authoritative.
