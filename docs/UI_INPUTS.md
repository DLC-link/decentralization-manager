# UI Inputs Reference (QA)

This document is a QA-oriented reference for every user-fillable input in the dec-party-manager web frontend (`frontend/`). Each action lists its trigger, the fields the user fills in (with type, required/optional, validation, and a concrete example), the submit button, and the success behavior to verify.

The doc is organized by dialog/component, not by user journey, so QA can look up "the X dialog" directly.

> **Source of truth:** the fields and validations below were extracted from the React components in `frontend/src/components/`. When the UI changes, this doc must be updated. Fields hidden in the source but submitted to the backend (e.g. invitation IDs auto-filled from props) are noted.

---

## Table of contents

- [Conventions](#conventions)
- [Authentication](#authentication)
  - [Login (SSO)](#login-sso)
  - [Test Authentication](#test-authentication)
  - [Grant Rights (admin credentials)](#grant-rights-admin-credentials)
- [Configuration](#configuration)
  - [Configure Party (auth + identity)](#configure-party-auth--identity)
  - [Configure Network Peers](#configure-network-peers)
  - [Node Configuration](#node-configuration)
- [Onboarding & Membership](#onboarding--membership)
  - [Create Decentralized Party (Onboarding)](#create-decentralized-party-onboarding)
  - [Accept / Decline Invitation](#accept--decline-invitation)
  - [Kick Participant](#kick-participant)
- [DARs & Packages](#dars--packages)
  - [Upload DARs (local node)](#upload-dars-local-node)
  - [Distribute DARs to Peers](#distribute-dars-to-peers)
  - [Search Packages](#search-packages)
  - [Check Peer DARs](#check-peer-dars)
- [Contracts deployment (ContractsDialog)](#contracts-deployment-contractsdialog)
  - [Step 1 — Choose contract type](#step-1--choose-contract-type)
  - [Configure CBTC party IDs](#configure-cbtc-party-ids)
  - [Configure Governance-Core contracts](#configure-governance-core-contracts)
  - [Configure Vault contracts](#configure-vault-contracts)
  - [Contract definition (reusable form)](#contract-definition-reusable-form)
  - [Field-type variants](#field-type-variants)
- [Execute Governance Action (ExecuteDialog)](#execute-governance-action-executedialog)
- [Governance Actions (GovernanceSection)](#governance-actions-governancesection)
  - [Add Governance Member](#add-governance-member)
  - [Remove Governance Member](#remove-governance-member)
  - [Set Governance Threshold](#set-governance-threshold)
  - [Set Governance Timeout](#set-governance-timeout)
  - [Pause Vault](#pause-vault)
  - [Unpause Vault](#unpause-vault)
  - [Update Vault Limits](#update-vault-limits)
  - [Update Vault Backend](#update-vault-backend)
  - [Deploy Vault](#deploy-vault)
  - [Deploy YieldEpoch](#deploy-yieldepoch)
  - [Update FAR Beneficiaries](#update-far-beneficiaries)
  - [Request Processor Deployment](#request-processor-deployment)
  - [Create Provider Service Request (Action)](#create-provider-service-request-action)
  - [Create User Service Request (Action)](#create-user-service-request-action)
  - [Setup Utility (Action)](#setup-utility-action)
  - [Accept Holder Service Request](#accept-holder-service-request)
  - [Offer Free Credential](#offer-free-credential)
  - [Accept Free Credential](#accept-free-credential)
  - [DevNet: Feature App](#devnet-feature-app)
- [Domain Proposals (core-self governance)](#domain-proposals-core-self-governance)
  - [Generic Vote](#generic-vote)
  - [Setup CC Preapproval](#setup-cc-preapproval)
  - [Setup Token Preapproval](#setup-token-preapproval)
  - [Transfer](#transfer)
  - [Accept Transfer](#accept-transfer)
  - [Provision Provider Service](#provision-provider-service)
  - [Setup Utility (Proposal)](#setup-utility-proposal)
  - [Create Provider Service Request (Proposal)](#create-provider-service-request-proposal)
  - [Create User Service Request (Proposal)](#create-user-service-request-proposal)
  - [Set Provider App Reward Beneficiaries](#set-provider-app-reward-beneficiaries)
  - [Set Enable Result Contracts](#set-enable-result-contracts)
  - [Create Delegated Batched Markers Proxy](#create-delegated-batched-markers-proxy)
  - [Mint](#mint)
  - [Burn](#burn)

---

## Conventions

These shapes appear repeatedly. Where a field below is described as e.g. "Party ID (text)", the format below applies.

| Concept | Format | Example |
|---|---|---|
| Party ID | Canton party identifier; opaque text containing `::`-separated fingerprint | `PSK_12345abcde` or `decentralized-party::1220a1b2c3...` |
| Participant ID | Opaque text identifier of a participant node | `PartyID::ABC123DEF456789...` |
| Contract ID | Opaque hex string identifying an on-ledger contract | `0123456789abc...` |
| Public key | Base64 / hex encoded key string | (long opaque blob) |
| Microseconds | Integer microsecond duration (1 hour = 3 600 000 000 µs) | `3600000000` |
| Weight (FAR beneficiary) | Decimal; sum of all weights in a list **must equal exactly 1.0** | `0.5` |
| File (DAR) | `.dar` extension only | `governance-core-1.0.dar` |

**Verifying success:** most action dialogs poll a status endpoint (typically every 2 s) and transition `inprogress → completed` (or `failed`). On `completed` the dialog shows a success alert and a parent callback refreshes the affected list. Errors are shown inline and the form remains open for retry.

---

## Authentication

### Login (SSO)

| Where | Login screen, shown when no session is present. |
|---|---|
| Inputs (in this app) | None — the form has no fields. |
| Button | **Log in** — initiates the OAuth/SSO redirect. |
| Verify on success | Page transitions to the authenticated app shell; party list loads. |

> The actual username/password is collected by the external SSO provider, not by this UI.

### Test Authentication

Triggered from **Test Auth** in the per-party `AuthSection`, or **Test Authentication** in the global `AuthCheckAccordion`. No user inputs.

| Button | Result on success |
|---|---|
| **Test Auth** / **Test Authentication** | `POST /auth/test`. The status row updates to pass/fail per party; party list refreshes. |

### Grant Rights (admin credentials)

Triggered from **Grant Rights** in `AuthSection` (visible only when the party is authenticated but is missing some on-ledger rights).

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Client ID | text | Yes | Trim non-empty | `admin-client-id` |
| Client Secret | password | Yes | Non-empty | `admin-secret-abc123` |

- Both fields are cleared each time the dialog opens.
- The secret is wiped on close and on error before re-display.
- **Submit:** **Grant** (disabled until both fields non-empty).
- **Verify on success:** dialog closes, `onGranted` fires, the missing rights disappear from the auth status panel.
- The helper text reminds the user the credentials are used once for `actAs` + `readAs` and not stored.

---

## Configuration

### Configure Party (auth + identity)

Triggered by the **gear** icon in `PartyDetail` header. Opens `PartyConfigDialog`.

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Member Party ID | text | Yes (for Save) | Non-empty | `party::participant01::member` |
| User ID | text | Yes (for Save) | Non-empty | `user123` |
| Keycloak URL | text | Required for *Discover* | URL form | `https://keycloak.example.com` |
| Keycloak Realm | text | Required for *Discover* | Non-empty | `my-realm` |
| Credentials Type | toggle | Required for *Discover* | One of: `Client ID + Secret`, `Username + Password` | `Client ID + Secret` |
| Client ID | text | Required if creds type = `Client ID + Secret` | Non-empty | `my-client` |
| Client Secret | password | Required if creds type = `Client ID + Secret` | Non-empty (placeholder hints "leave empty to keep existing") | `secret123` |
| Username | text | Required if creds type = `Username + Password` | Non-empty | `keycloak_user` |
| Password | password | Required if creds type = `Username + Password` | Non-empty (placeholder "leave empty to keep existing") | `pass123` |

- **Discover Member Party** button (only enabled when the auth fields above are filled) auto-fills *Member Party ID* and *User ID* by querying Keycloak.
- **Save** is disabled unless both *Member Party ID* and *User ID* are present.
- Secret/password placeholders indicate update-only semantics — leave blank to keep the stored value.
- **Verify on success:** confirmation message; the auth panel for this party reflects the new identity.

### Configure Network Peers

Triggered by the pencil icon next to **Peers:** in `NetworkConfigAccordion`. Each peer is one row; you can add, edit, or delete rows.

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Participant ID | text | Yes | Non-empty (no format check in UI) | `PartyID::ABC123DEF456789...` |
| Name | text | No | — | `Party A` |
| Address | text | Yes | Non-empty (no format check) | `192.168.1.100` or `localhost` |
| Port | number | Yes | Parsed as integer; **0 is accepted on bad input**, no range check (negatives accepted) | `9000` |
| Public Key | text | Yes | Non-empty | (long opaque key) |

Per-form controls:

- **Add Peer** — appends a row with defaults `address: "localhost"`, `port: 9000`, all other fields empty.
- **Paste from Clipboard** — expects CSV `participant_id,name,address,port,public_key` (≥5 fields). Note the matching **Copy** action emits 6 fields with a trailing comma; field 6 is silently ignored.
- **Remove** (trash icon per row) — deletes that peer.
- **Save** — applies all edits at once via `onSave(editedPeers)`.
- **Cancel** — discards all edits.

> ⚠ Known QA traps: port accepts negatives and 0; required-vs-optional is not signalled visually; copy/paste field-count mismatch (6 vs 5) is tolerated but undocumented.

### Node Configuration

Read-only. `NodeConfigAccordion` shows Participant ID, Admin API `host:port`, Ledger API `host:port`, and Synchronizer; no inputs.

---

## Onboarding & Membership

### Create Decentralized Party (Onboarding)

Triggered from **Start Onboarding** in `OnboardingDialog`.

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Party ID Prefix | text | Yes | Non-empty after trim | `my-network` |
| Select Peers to Invite | checkbox group | Yes | At least one selected; peers fetched from `/network-config`; all peers except self pre-checked | tick `Peer1 (192.168.1.10:9000)` |

- **Submit:** **Start Onboarding** (disabled while either input is invalid or while a workflow is running).
- The dialog polls `/onboarding/status`; status transitions through `inprogress → completed` (or `failed`).
- **Verify on success:** spinner stops, success alert shown, dialog closes; new decentralized party visible in the party list.
- **Mesh-hole error path (HTTP 422 with `missing_edges`):** UI splits errors into "Coordinator can't reach these peers" (`unreachable_from_coordinator`) and "Update network configs" (`mesh_hole`) and lists exactly which peer needs to add which other peer. Reproduce by selecting peers that aren't fully connected.

### Accept / Decline Invitation

Auto-opens (`InvitationModal`) when a `PendingInvitation` is delivered to the app. **No user-editable fields** — the invitation ID is auto-filled from props and hidden.

Read-only display:

- Invitation Type (`Onboarding` | `Kick` | `Contracts` | `Dars`)
- Title and description (derived from type)
- Coordinator Name (or first 32 chars of `coordinator_pubkey` + `…`)
- Received timestamp (Unix seconds → locale string)

| Buttons | Verify on success |
|---|---|
| **Accept** → `POST /invitations/accept` | Snackbar: "Invitation accepted - workflow started"; modal closes. |
| **Decline** → `POST /invitations/decline` | Snackbar: "Invitation declined"; modal closes. |

### Kick Participant

Triggered by the *person-remove* icon in the Participants table of `PartyDetail`. Opens `KickDialog`.

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Decentralized Party ID | text (read-only) | Auto | Pre-filled from row | `party::operator::dec-party-1` |
| Participant ID to Kick | text (read-only) | Auto | Pre-filled from row | `party::participant01::uid` |
| Namespace Fingerprint (Owner Key) | text (read-only) | Auto-resolved (polled every 2 s if absent) | **Must resolve before Kick is enabled** | `dns-owner-key-abc123…` |
| New Threshold | number | Yes | `1 ≤ value ≤ remainingOwners`; default = `ceil(remainingOwners / 2)` (helper text shows suggested + max) | `2` |

- **Submit:** **Kick Participant** (disabled while owner key is unresolved or threshold is out of range).
- Polls `/kick/status` every 2 s; on `completed`, success alert is shown, `onKickComplete` fires, party list refreshes. On `failed`, error alert and form re-enabled for retry.
- **Verify on success:** participant disappears from the party's Participants table; threshold value matches what was submitted.

---

## DARs & Packages

### Upload DARs (local node)

In `DarsDialog`, the **Upload to local node** mode.

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| DAR Files | file (multiple) | Yes (≥1) | Only `.dar` extension is accepted by the file picker; no explicit size limit in UI | `governance-core-1.0.dar` |

- **Submit:** **Upload DARs**.
- **Verify on success:** alert "DARs uploaded to this node successfully!"; new packages appear in `PackagesPanel` after refresh.
- Error if no files selected.

### Distribute DARs to Peers

`DarsDialog`, **Distribute** mode.

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| DAR Files | file (multiple) | Yes (≥1) | `.dar` only | `governance-core-1.0.dar` |
| Peer Selection | checkbox (multi) | Yes (≥1) | Peers fetched from network config; the local node is excluded; checkboxes disabled while distributing | tick `Peer1 (192.168.1.10:9000)` |

- If no peers are configured, the message reads "No peers configured. Add peers in the Network Configuration first." and submit is unavailable.
- **Submit:** **Distribute DARs**. UI then shows "Distributing DARs to selected peers…" while polling status every 2 s.
- **Verify on success:** alert "DARs distributed to selected peers successfully!"; each selected peer's `PackagesPanel` shows the new packages after refresh.

### Search Packages

In `PackagesPanel`, the search field.

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Search query | text | No | Case-insensitive substring match against package **name** or **package ID** | `daml-stdlib` or `314e5faef7` |

- No submit button — filtering is live as the user types.
- Filtered count is displayed (`X of Y packages`).
- The same field also filters the *Check Peer DARs* comparison view.

### Check Peer DARs

`PackagesPanel`, **Check Peer DARs** button. No user inputs.

- Button label switches to **Checking…** with a spinner during the fetch.
- **Verify on success:** package table is replaced with a comparison matrix (one column per peer): green check = match, red X = mismatch, dash = unreachable. Unreachable peer column headers are dimmed (opacity 0.5) and show an offline icon.

---

## Contracts deployment (ContractsDialog)

`ContractsDialog` is a multi-step deployment form. The first step picks a *contract type*; later steps configure parties and contract field values.

### Step 1 — Choose contract type

Click one of the cards. Card click acts as the next-step trigger; **there is no Submit button on this step.**

| Card | Selectable? | Notes |
|---|---|---|
| Governance Core | Only when the governance-core DAR is uploaded | otherwise card is disabled |
| Token Custody | No | "Coming soon" |
| CBTC | No | "Coming soon" |
| Vault | No | "Coming soon" |
| Utility | No | "Coming soon" |

> Today only the **Governance Core** path is exercisable end-to-end via this dialog. CBTC and Vault forms exist in the codebase (described below) but their cards are disabled.

### Configure CBTC party IDs

Shown after picking the (currently disabled) **CBTC** type.

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Operator Party ID | text | Yes | Non-empty | `operator::1220a1b2c3...` |
| Participant Party IDs | multi-text (Enter to add) | Yes | Count must equal number of participants (e.g. 3) | `participant1::5678...` |

- **Submit:** **Deploy Contracts** (bottom-right).
- Polls deployment status every 2 s; success alert on completion.

### Configure Governance-Core contracts

Shown after picking **Governance Core**. The contract structure is locked; you can edit only the values.

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Decentralized Party | display only | — | Auto from `partyId` prop | `decentralized-party::abc123...` |
| Operator Party | display only | — | Always "(auto-allocated)" | — |
| Member Set | multi-text (Enter to add) | Optional | Pre-filled from `/governance/known-members`; user may add/remove (no duplicate enforcement) | `member1::xyz...` |
| Governance Threshold | number | Optional | Integer; default ≈ `ceil(2/3 × participantCount)` | `2` |
| Action Confirmation Timeout | select | Optional | One of: 3 min (`180000000`), 10 min (`600000000`), 30 min (`1800000000`), 1 hour (`3600000000`, **default**), 2 hours (`7200000000`), 24 hours (`86400000000`) — all µs | `3600000000` |

- **Submit:** **Deploy Contracts** → `POST /contracts`. Polls deployment status; success/failure alert.

> Hidden behavior: in governance-core mode the *Member Set* field doubles as the participant-parties source for the backend submission, even though no operator/participant party UI is shown. QA should ensure the Member Set is non-empty before submitting.

### Configure Vault contracts

Shown after picking **Vault**. Skips the operator/participant party section. Uses the generic *Contract definition* form (below) — multiple contracts may be added or it may be left empty.

### Contract definition (reusable form)

Used by the contract-type configuration steps. Each contract is one accordion entry with these top-level fields, plus a list of *fields*.

Top-level inputs per contract:

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Package Name / ID | autocomplete (free-solo) | Yes | Suggestions merged from current config and currently deployed contracts | `#cbtc` or a UUID |
| Module Name | text | Yes | Non-empty | `CBTC.Governance` |
| Entity Name | text | Yes | Non-empty | `CBTCGovernanceRules` |

Per field row:

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Field Type | select | Yes | One of: Dec. Party, Operator, Party, Member Set, Attestors Set, Threshold, Proposal Timeout, Optional, Instrument, Text, Integer, Boolean, Record | `party_set` |
| Field Value | varies by type | Yes | See per-type rules below | varies |
| Delete Field | trash icon | — | Removes the row | — |

Buttons:

- **Add Field** — appends a new field defaulting to *Text*.
- **Add Contract** — appends another contract definition.
- **Delete Contract** — trash icon on the contract accordion header.
- When `lockStructure === true` (governance-core), the field-type dropdowns become read-only labels: values are still editable but types are not.

### Field-type variants

| Variant | Inputs | Notes / Example |
|---|---|---|
| Decentralized Party | display only | Shows the dialog's `partyId` |
| Operator Party | display only | Shows "(auto-allocated)" |
| Attestors Set | display only | Shows "(all N participants)" |
| Party (single) | text · required | Placeholder "Paste party ID"; e.g. `participant::5678...` |
| Party Set (multiple) | multi-text · optional | No duplicate enforcement |
| Governance Threshold | number · required | Default `ceil(2/3 × participantCount)` |
| Relative Time (Proposal Timeout) | select · required | Same µs options as the governance-core timeout above; default 1 hour (`3600000000`) |
| Optional (wrapper) | inner-type select + inner value | Inner-type cannot be Optional or Record; changing inner type resets the value to that type's default |
| Instrument | `id` (text · required) | e.g. `CBTC` |
| Text | text · required | Any string |
| Integer (int64) | number · required | Parsed as integer; default `0` |
| Boolean | select · required | `True` / `False` |
| Record | nested fields | Each nested field has its own type select + value; nested fields cannot themselves be Record |

---

## Execute Governance Action (ExecuteDialog)

Opens after a governance action reaches its confirmation threshold. The dialog asks for any *disclosed contracts* needed to execute the action.

Read-only display:

- Action — formatted action type, e.g. `Deploy Vault`
- Confirmations — current count, e.g. `3 confirmation(s)`

Disclosed-contracts table (one row per contract, repeatable):

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Contract ID | text | Yes (per row) | Pre-filled by `getRequiredContractIds()` for `dev_net_feature_app`, `vault_deployment`, `processor_deployment_request` | `vault_rules_cid` or `amulet-rules-contract::abc123` |
| Blob (base64) | textarea (2–4 rows) | Yes (per row) | Pre-filled from a merge of `STATIC_BLOB_MAP` (hard-coded) and the `blobMap` prop (parent overrides static) | `ZXhhbXBsZWJhc2U2…` |

Controls:

- **Add** — appends an empty disclosed-contract row.
- Trash icon per row — deletes that row.
- **Submit:** **Execute** (right side, green) — calls `onExecute()` with the array of disclosed contracts.
- **Verify on success:** parent UI shows the action as executed; subsequent governance-actions table reflects the new state.

---

## Governance Actions (GovernanceSection)

All governance actions share one wrapper: the **New Governance Action** form. The user first selects an *Action Type*, then fills in fields specific to that action.

**Common required field on every action form:**

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Governance Contract ID | autocomplete (free-solo) | Yes | Disabled unless user has `ADMIN_ACCESS`; suggestions from `governanceContractIds` prop | `0123456789abc...` |

**Common submit:** **Submit Confirmation**. On success, the form closes, governance data refreshes, and `onAfterAction` fires.

The action-specific fields follow.

### Add Governance Member

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Member Party ID | text | Yes | — | `PSK_12345abcde` |
| New Threshold | number | Yes | Integer ≥ 1 | `2` |

### Remove Governance Member

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Member Party ID | text | Yes | — | `PSK_12345abcde` |
| New Threshold | number | Yes | Integer ≥ 1 | `1` |

### Set Governance Threshold

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| New Threshold | number | Yes | Integer ≥ 1 | `3` |

### Set Governance Timeout

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Timeout (microseconds) | number | Yes | Non-negative integer (helper text: "1 hour = 3 600 000 000 µs") | `3600000000` |

### Pause Vault

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Vault | select | Yes | From available vaults; paused vaults shown as `name (symbol) [Paused]` | (chosen from list) |

### Unpause Vault

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Vault | select | Yes | From available vaults | (chosen from list) |

### Update Vault Limits

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Vault | select | Yes | — | (chosen from list) |
| Max Total Deposit | text (numeric) | No | Empty = no limit | `1000000` |
| Min Deposit Amount | text (numeric) | No | Empty = no limit | `100` |
| Min Withdrawal Amount | text (numeric) | No | Empty = no limit | `50` |

### Update Vault Backend

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Vault | select | Yes | — | (chosen from list) |
| New Backend Signatory | text | Yes | — | `PSK_backend_party_123` |

### Deploy Vault

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Vault Rules | select | Yes | Contract CID from template; refreshable | (auto-selected if available) |
| Vault Name | text | Yes | — | `cbtc-vault-v0-rc1` |
| Share Symbol | text | Yes | — | `CBTCV0RC1` |
| Asset Instrument · Admin Party | text | Yes | — | `PSK_12345abcde` |
| Asset Instrument · ID | text | Yes | — | `CBTC` |
| Vault Limits · Max Total Deposit | text (numeric) | No | Empty = no limit | `1000000` |
| Vault Limits · Min Deposit | text (numeric) | No | Empty = no limit | `100` |
| Vault Limits · Min Withdrawal | text (numeric) | No | Empty = no limit | `50` |
| Vault Backend Signatory Party | text | Yes | — | `PSK_backend_party_123` |
| Featured App Right (FAR) | select | No | Contract CID from template | (auto-selected if available) |
| FAR Beneficiaries | dynamic list | No | **Sum of weights must equal exactly 1.0** | see below |
| Allocation Factory | select | Yes | Contract CID from template | (auto-selected) |
| Registrar Service | select | Yes | Contract CID from template | (auto-selected) |

FAR Beneficiaries (each row):

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Beneficiary Party | text | Yes | — | `PSK_party_123` |
| Weight | text (numeric) | Yes | Sum across rows = 1.0 | `0.5` |

### Deploy YieldEpoch

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Vault Rules | select | Yes | Contract CID from template | (auto-selected) |
| Vault | select | Yes | From available vaults; auto = first | (auto-selected) |
| Asset Instrument · Admin Party | text | Yes | — | `PSK_12345abcde` |
| Asset Instrument · ID | text | Yes | — | `CBTC` |
| Vault Backend Signatory Party | text | Yes | — | `PSK_backend_party_123` |

### Update FAR Beneficiaries

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Vault | select | Yes | — | (chosen from list) |
| FAR Beneficiaries | dynamic list | Yes | Sum of weights = 1.0 | (see Deploy Vault) |

### Request Processor Deployment

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Vault Processor Rules CID | text | Yes | — | `0123456789abc...` |
| Vault Backend Signatory Party | text | Yes | — | `PSK_backend_party_123` |
| Burn Mint Factory | text | Yes | Auto-fetched (refreshable) | (from external API) |
| Featured App Right | select | No | — | (auto-selected if available) |
| FAR Beneficiaries | dynamic list | No | Sum of weights = 1.0 | (see Deploy Vault) |
| Initial Supported Vaults | checkbox list | Yes | At least one checked; defaults to all available | tick one or more |

### Create Provider Service Request (Action)

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Operator Party | text | Yes | — | `PSK_12345abcde` |

### Create User Service Request (Action)

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Operator Party | text | Yes | — | `PSK_12345abcde` |

### Setup Utility (Action)

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Operator Party | text | Yes | — | `PSK_12345abcde` |
| Provider Service | select | Yes | From available services; default = first | (chosen from list) |
| User Service | select | Yes | From available services; default = first | (chosen from list) |

### Accept Holder Service Request

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Operator Party | text | Yes | — | `PSK_12345abcde` |
| Provider Service | select | Yes | From available services; default = first | (chosen from list) |
| Holder Service Request CID | text | Yes | — | `0123456789abc...` |
| Holder Party | text | Yes | — | `PSK_holder_party` |

### Offer Free Credential

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Operator Party | text | Yes | — | `PSK_12345abcde` |
| User Service | select | Yes | Default = first | (chosen from list) |
| Holder Party | text | Yes | — | `PSK_holder_party` |
| Credential ID | text | Yes | — | `credential-v1` |
| Credential Description | text | Yes | — | `A credential for accessing the vault` |
| Claims | dynamic list | No | — | see below |

Claim row (each):

| Field | Type | Example |
|---|---|---|
| Subject | text | `identity` |
| Property | text | `kyc_status` |
| Value | text | `verified` |

### Accept Free Credential

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Operator Party | text | Yes | — | `PSK_12345abcde` |
| User Service | select | Yes | Default = first | (chosen from list) |
| Credential Offer CID | text | Yes | — | `0123456789abc...` |

### DevNet: Feature App

Visible only on devnet. Includes a Refresh button to fetch the CID from network info.

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Amulet Rules CID | text | Yes | — | `0123456789abc...` |

---

## Domain Proposals (core-self governance)

Visible only when `governanceType === "core_self"`. Opened from **New Proposal** in the Proposals panel of `GovernanceSection`. The user first selects a *Proposal Type*, then fills in the fields specific to that type. **Submit:** **Submit Proposal**.

### Generic Vote

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Vote Description | textarea (multiline, 2–6 rows) | Yes | — | `Should we update the governance threshold to 4?` |

### Setup CC Preapproval

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Provider Party | text | Yes | — | `PSK_provider_party` |
| Expected DSO Party | text | Yes | — | `PSK_dso_party` |

### Setup Token Preapproval

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Operator Party | text | Yes | — | `PSK_operator_party` |
| Instrument Admin | text | Yes | — | `PSK_admin_party` |

### Transfer

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| TransferFactory CID | text | Yes | — | `0123456789abc...` |
| Expected Admin Party | text | Yes | — | `PSK_admin_party` |
| Receiver Party | text | Yes | — | `PSK_receiver_party` |
| Amount | text (numeric) | Yes | Numeric | `1000` |
| Instrument Admin | text | Yes | — | `PSK_admin_party` |
| Instrument ID | text | Yes | — | `CBTC` |
| Input Holding CIDs | text (comma-separated) | No | Empty → backend auto-selects | `cid1, cid2, cid3` |

### Accept Transfer

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| TransferInstruction CID | text | Yes | — | `0123456789abc...` |

### Provision Provider Service

No inputs. The form shows: "Provisions a Utility-Registry ProviderService with operator = proposer and provider = governance party. No parameters required."

### Setup Utility (Proposal)

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| ProviderService CID | text | Yes | — | `0123456789abc...` |
| Operator Party | text | Yes | — | `PSK_operator_party` |
| Instrument ID | text | Yes | — | `CBTC` |
| Create TransferRule | checkbox | No | Default checked | ☑ |
| Create AllocationFactory | checkbox | No | Default checked | ☑ |

### Create Provider Service Request (Proposal)

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Operator Party | text | Yes | — | `PSK_operator_party` |
| Provider Party | text | Yes | — | `PSK_provider_party` |

### Create User Service Request (Proposal)

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Operator Party | text | Yes | — | `PSK_operator_party` |
| User Party | text | Yes | — | `PSK_user_party` |

### Set Provider App Reward Beneficiaries

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| InstrumentConfiguration CID | text | Yes | — | `0123456789abc...` |
| Clear beneficiaries (set to None) | checkbox | No | Default unchecked; when checked, hides Beneficiaries field | ☐ |
| Beneficiaries | textarea (multiline) | Yes if not clearing | One per line, format `<party>,<weight>` | `PSK_party1,0.5`<br>`PSK_party2,0.5` |

### Set Enable Result Contracts

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| RegistrarService CID | text | Yes | — | `0123456789abc...` |
| Enable Result Contracts | select | Yes | One of: `Enable`, `Disable`, `Clear (None)` | `Enable` |

### Create Delegated Batched Markers Proxy

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| Operator Party | text | Yes | — | `PSK_operator_party` |

### Mint

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| AllocationFactory CID | text | Yes | — | `0123456789abc...` |
| Instrument Admin | text | Yes | — | `PSK_admin_party` |
| Instrument ID | text | Yes | — | `CBTC` |
| InstrumentConfiguration CID | text | Yes | — | `0123456789abc...` |
| Recipient Party | text | Yes | — | `PSK_recipient_party` |
| Amount | text (numeric) | Yes | Numeric | `1000` |
| Description | text | Yes | — | `Mint 1000 CBTC for liquidity pool` |

### Burn

| Field | Type | Required | Validation | Example |
|---|---|---|---|---|
| AllocationFactory CID | text | Yes | — | `0123456789abc...` |
| Instrument Admin | text | Yes | — | `PSK_admin_party` |
| Instrument ID | text | Yes | — | `CBTC` |
| InstrumentConfiguration CID | text | Yes | — | `0123456789abc...` |
| Holder Party | text | Yes | — | `PSK_holder_party` |
| Amount | text (numeric) | Yes | Numeric | `500` |
| Description | text | Yes | — | `Burn 500 CBTC from vault` |

---

## Loading & error patterns to verify

These behaviors are shared across most action forms and should be checked while QA-ing any of them:

- Submit buttons disable and show a spinner while a request is in flight.
- Long-running flows (Onboarding, Distribute DARs, Kick) poll a `/<flow>/status` endpoint every 2 s; the form stays mounted, with status transitioning `inprogress → completed | failed`.
- Service / vault / contract `select` dropdowns show a loading state while their list is being fetched, and many have a Refresh icon to re-fetch on demand.
- Errors render in a dismissible `Alert`; the form remains open so the user can correct inputs and retry.
- Beneficiary lists with weights display the running sum live; submit is blocked unless the sum equals 1.0.
