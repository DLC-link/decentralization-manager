# Writing Custom DAML Templates Supported by DecMan

This guide explains how to author a custom DAML template that is fully governable through the Decentralized Party Manager (DPM / "DecMan") App — i.e. how to produce a template that DecMan can package, upload, deploy, propose, confirm, execute, and audit without any code changes inside DecMan itself.

The extension model is **interface-based**. DecMan's governance engine (`GovernanceRules` in `governance-core`) is a fixed contract that operates on any DAML template implementing the `GovernableAction` interface. To plug a new action into governance you write a new template, build a DAR, upload it, and drive the lifecycle through the existing REST API.

For background, read these first:

- [`docs/ARCHITECTURE.md`](ARCHITECTURE.md) — governance engine, `GovernableAction` interface, action lifecycle
- [`docs/INTEGRATION_GUIDE.md`](INTEGRATION_GUIDE.md) — `/contracts`, `/dars`, `/governance/*` endpoints and the field-type system

## What "fully supported" means

A custom template is fully supported by DecMan when:

1. It implements the `GovernableAction` interface from `governance-action-v0` (`Governance.Action`).
2. Its DAR can be distributed to every participant via `POST /dars/distribute`.
3. Its proposal contract can either be (a) created via `POST /contracts` using the field-type system, or (b) created by an external DAML script / app that the governance party can read.
4. It can be confirmed via `POST /governance/confirm` with `governance_type: "core_domain"` (no DecMan code change needed — confirmation works on the `ContractId GovernableAction` produced by step 3).
5. It can be executed via `POST /governance/execute` with `governance_type: "core_domain"`, producing a `GovernanceExecutionResult` audit record.

Anything beyond that — for example a bespoke proposal type wired into `POST /governance/propose` — requires a server-side change in `src/server/action_serializer.rs` and is out of scope for a "custom" template.

> **One `GovernanceRules` handles every custom action.** You do **not** deploy a new `GovernanceRules` per new template. A single instance bound to a `governanceParty` matches *any* `ContractId GovernableAction` whose view's `governanceParty` field equals its own — regardless of the underlying template's package, module, or entity name. Custom templates extend an existing governance domain at zero infrastructure cost; the engine is universal.

## The `GovernableAction` interface

Every governable template must implement [`Governance.Action.GovernableAction`](../daml/governance-action-v0/daml/Governance/Action.daml):

```daml
data GovernableActionView = GovernableActionView with
    governanceParty : Party  -- decentralized party whose authority is required
    proposer        : Party  -- the member or additional proposer who created this proposal
    actionLabel     : Text   -- short label, surfaces in the governance UI and audit log
    description     : Text   -- human-readable description, copied into GovernanceExecutionResult

interface GovernableAction where
  viewtype GovernableActionView

  executeImpl : Update ()

  choice GovernableAction_Execute        : ()  -- controller: governanceParty
  choice GovernableAction_Cancel         : ()  -- controller: governanceParty
  choice GovernableAction_ProposerCancel : ()  -- controller: proposer
```

Three rules follow from the engine's behavior in [`Governance.Rules`](../daml/governance-core/daml/Governance/Rules.daml):

- **Authority flow** — `GovernanceRules` (signed by `governanceParty`) exercises `GovernableAction_Execute`. Your `executeImpl` therefore runs *with* the governance party's authority and can create / exercise anything the governance party is entitled to.
- **`governanceParty` must match** — `GovernanceRules_ConfirmAction` checks that the proposal's view targets the same `governanceParty` as the rules contract.
- **`proposer` must be authorized** — at confirm time, the proposer is required to be either a member or in the `additionalProposers` allowlist on the `GovernanceRules` contract.

### Template skeleton

The minimum viable proposal template looks like this — model it after [`GenericVoteProposal`](../daml/governance-core/daml/Governance/GenericVote.daml):

```daml
module MyDomain.PauseProposal where

import Governance.Action

template PauseProposal
  with
    governanceParty : Party
    proposer        : Party
    targetCid       : ContractId MyResource
    reason          : Text
  where
    signatory proposer
    observer  governanceParty

    interface instance GovernableAction for PauseProposal where
      view = GovernableActionView with
        governanceParty
        proposer
        actionLabel = "PauseResource"
        description = "Pause " <> show targetCid <> ": " <> reason

      executeImpl = do
        exercise targetCid MyResource_Pause
        pure ()
```

### Conventions enforced by the governance engine

| Concern | Convention | Why |
|---|---|---|
| Signatory | `proposer` only | Lets a single member submit the proposal without a multi-party preparation step. |
| Observer | `governanceParty` (and any other reading party that needs visibility) | Required so `GovernanceRules` can fetch the proposal during confirm/execute. |
| `governanceParty` field | Must equal the `GovernanceRules.governanceParty` | Enforced in `GovernanceRules_ConfirmAction`. |
| `proposer` field | Must be in `members ∪ additionalProposers` of the targeted `GovernanceRules` | Enforced in `GovernanceRules_ConfirmAction`. The submitting participant must also `actAs` this party. |
| `actionLabel` | Short PascalCase string (e.g. `"PauseResource"`) | Copied verbatim into `GovernanceConfirmation` and `GovernanceExecutionResult`. UI groups confirmations by label. |
| `description` | Human-readable string | Copied into the on-chain audit trail (`GovernanceExecutionResult.description`). Keep it self-explanatory; it is the permanent record of what the vote was about. |
| Preconditions | Use template `ensure` clauses | They fail fast at proposal creation, before any votes are cast. See `TransferProposal` (`ensure transfer.amount > 0.0`). |

### What `executeImpl` may and may not do

- ✅ Exercise any choice on a contract where `governanceParty` is a controller / signatory / observer with the necessary visibility.
- ✅ Create new contracts whose required authorizations are a subset of `{proposer, governanceParty}`.
- ✅ Return `pure ()` for pure "intent-only" actions (see `GenericVoteProposal`) — the audit trail is still recorded.
- ❌ Require the authority of any party that is *not* `governanceParty` or `proposer`. Such an action will fail at execute time. If you need a third-party signature, model it as a two-step flow (first action creates an offer; second party accepts off-chain).

## Package layout

Custom templates live in their own DAML package. Place it under `daml/<your-package>/` alongside the existing packages:

```
daml/
├── multi-package.yaml          # add your package here
├── my-package/
│   ├── daml.yaml
│   └── daml/
│       └── MyDomain/
│           └── PauseProposal.daml
└── my-package-test/
    ├── daml.yaml
    └── daml/
        └── MyDomain/Test/
            └── PauseProposalTest.daml
```

### `daml.yaml`

Match the SDK version, target, and build options used by the existing packages — for example [`governance-utility-onboarding/daml.yaml`](../daml/governance-utility-onboarding/daml.yaml):

```yaml
sdk-version: 3.4.11
name: my-package-v0
version: 0.1.0
source: daml
dependencies:
  - daml-prim
  - daml-stdlib
data-dependencies:
  - ../governance-action-v0/.daml/dist/governance-action-v0-0.1.0.dar
  # add any other DARs whose contracts you exercise from executeImpl
build-options:
  - --target=2.2
  - --ghc-option=-Wunused-binds
  - --ghc-option=-Wunused-matches
```

Notes:

- The `governance-action-v0` DAR is the only **required** data-dependency. It exports the `GovernableAction` interface; `governance-core` is *not* a build-time dependency of your package.
- Use `data-dependencies` (not `dependencies`) for every governance / utility-registry DAR — they ship as pre-built DARs, not source.
- Keep the package name suffixed with a version qualifier (`-v0`, `-v1`, …). The DAML resolver treats package names with different version suffixes as distinct, which lets you ship a breaking change without touching deployed instances of the old version.

### `multi-package.yaml`

Add your package so `daml build --all` picks it up:

```yaml
packages:
  - governance-action-v0
  - governance-core
  - governance-core-test
  # …existing entries…
  - my-package
  - my-package-test
```

## Build, distribute, upload

### 1. Build the DAR

```bash
cd daml
daml build --all
# DAR will be at daml/my-package/.daml/dist/my-package-v0-0.1.0.dar
```

### 2. Distribute it to every participant

DecMan provides two endpoints for getting a DAR onto Canton:

- `POST /dars/upload` — uploads the DAR to *this node only*. Accepts `DarsRequest` with `peer_ids` ignored.
- `POST /dars/distribute` — runs a multi-party workflow that uploads the DAR to every participant. Requires `peer_ids` to be **non-empty**; the handler rejects an empty array with 400.

For production, always use `/dars/distribute` so every node has the same package vetted at the same time:

```bash
BASE64=$(base64 -i daml/my-package/.daml/dist/my-package-v0-0.1.0.dar)
curl -X POST http://localhost:8080/dars/distribute \
  -H 'Content-Type: application/json' \
  -d "{
    \"dar_files\": [{\"filename\":\"my-package-v0-0.1.0.dar\",\"data\":\"${BASE64}\"}],
    \"peer_ids\":  [\"node2::1220...\", \"node3::1220...\"]
  }"

# poll status
curl http://localhost:8080/dars/distribute/status
```

After distribution, verify the package id is vetted on every node:

```bash
curl http://localhost:8080/packages/vetted
curl http://localhost:8080/packages/compare-peers   # admin-only — catches missing/extra packages across peers
```

### 3. Register the package id for the party (optional)

`PUT /party-config` accepts a `packages` map that DecMan threads through governance endpoints. The keys are hard-coded in `src/config.rs::PackageConfig`:

```
governance_action, governance_core, governance_token_custody,
governance_utility_credential, governance_utility_onboarding,
utility_credential, utility_registry, vault, vault_governance
```

Custom packages are **not** in that map. That is fine — for the `core_domain` flow, only `governance_core` is dereferenced by name (to target `GovernanceRules`); the proposal's own package id is implied by the contract id and never needs to be resolved by DecMan.

You only need to register a package id under one of those slots if you are *replacing* one of the standard packages (e.g. forking `governance-token-custody`).

## Bootstrapping a `GovernanceRules` for your domain

If your decentralized party doesn't yet have a `GovernanceRules` contract, create one through the `/contracts` workflow before any custom proposal can be confirmed. The template lives in `governance-core` and takes **five** fields, in this order (see [`Governance.Rules`](../daml/governance-core/daml/Governance/Rules.daml)):

| # | Field | DAML type | Field-type JSON |
|---|---|---|---|
| 1 | `governanceParty` | `Party` | `{ "type": "decentralized_party" }` |
| 2 | `members` | `Set Party` | `{ "type": "party_set", "parties": [...] }` |
| 3 | `threshold` | `Int` | `{ "type": "int64", "value": N }` (or `{ "type": "governance_threshold" }` for the calculated majority) |
| 4 | `actionConfirmationTimeout` | `RelTime` | `{ "type": "rel_time", "microseconds": ... }` |
| 5 | `additionalProposers` | `Optional (Set Party)` | `{ "type": "none" }` (start empty; grow later via the self-action below) |

> ⚠️ Do **not** use `{ "type": "attestors_set" }` for `members`. Despite the name, that variant emits a raw `GenMap<Party, Unit>` (used by some CBTC-style templates), not the `DA.Set.Types:Set Party` record wrapper that `members` expects. Always use `party_set` for `Set Party` fields. See the field-type table below.

Minimal request — 3 members, threshold 2, 30-minute confirmation window. The DAR for `governance-core` must already be uploaded (via `/dars/distribute`) before this call; `POST /contracts` does **not** take a `dar_files` field:

```bash
curl -X POST http://coordinator:8080/contracts \
  -H 'Content-Type: application/json' \
  -d '{
    "decentralized_party_id": "my-vault-network::1220abc...",
    "participant_ids":   ["node1::1220...", "node2::1220...", "node3::1220..."],
    "participant_parties": ["member1::1220...", "member2::1220...", "member3::1220..."],
    "operator_party":    "operator::1220...",
    "contracts": [{
      "id":           "governance-rules",
      "name":         "GovernanceRules",
      "package_id":   "#governance-core-v0",
      "module_name":  "Governance.Rules",
      "entity_name":  "GovernanceRules",
      "fields": [
        { "type": "decentralized_party" },
        { "type": "party_set", "parties": ["member1::1220...", "member2::1220...", "member3::1220..."] },
        { "type": "int64", "value": 2 },
        { "type": "rel_time", "microseconds": 1800000000 },
        { "type": "none" }
      ]
    }]
  }'
```

The 5th field is **not** optional in the JSON — leaving it off produces a malformed `Optional` value at submission time. Use `{ "type": "none" }` for "no additional proposers".

Once this is on the ledger, every member node sees the `GovernanceRules` contract id under `GET /governance/state?party_id=...` and can reuse it for every subsequent custom action — see the callout near the top of this guide.

## Creating a proposal contract

There are two supported paths, depending on whether the proposal's fields fit DecMan's field-type system.

### Path A — `POST /contracts` (no code change)

Use this when every field on your template maps to one of the variants of [`FieldDefinition`](../src/workflow/contracts/config.rs). The full list:

| `type` | JSON shape | DAML target type |
|---|---|---|
| `decentralized_party` | `{ "type": "decentralized_party" }` | `Party` (the dec party) |
| `operator_party` | `{ "type": "operator_party" }` | `Party` (the operator) |
| `participant_party` | `{ "type": "participant_party", "id": "..." }` | `Party` |
| `text` | `{ "type": "text", "value": "..." }` | `Text` |
| `int64` | `{ "type": "int64", "value": 42 }` | `Int` |
| `bool` | `{ "type": "bool", "value": true }` | `Bool` |
| `instrument` | `{ "type": "instrument", "id": "..." }` | `InstrumentId` (record `{ admin = dec-party, id = ... }`) |
| `attestors_set` | `{ "type": "attestors_set" }` | raw `GenMap<Party, Unit>` populated from every participant party (CBTC-style templates). **Not** `DA.Set.Types:Set Party` — use `party_set` for that. |
| `party_set` | `{ "type": "party_set", "parties": [...] }` | `DA.Set.Types:Set Party` (record-wrapped `GenMap<Party, Unit>`) — this is what `Set Party` means in `daml-stdlib`. |
| `rel_time` | `{ "type": "rel_time", "microseconds": 86400000000 }` | `RelTime` |
| `optional` | `{ "type": "optional", "inner": { ... } }` | `Some <inner>` |
| `none` | `{ "type": "none" }` | `None` |
| `record` | `{ "type": "record", "fields": [...] }` | nested record |
| `governance_threshold` | `{ "type": "governance_threshold" }` (calculated majority) or `{ "type": "governance_threshold", "value": N }` (explicit) | `Int` |

The serializer is defined at [`src/workflow/contracts/steps/prepare.rs:212`](../src/workflow/contracts/steps/prepare.rs). Field order in the JSON must match the field order in the DAML template.

Example body for the `PauseProposal` template above (instantiated as a proposal — note that proposals are usually created by a single party, so a dedicated multi-party `/contracts` workflow is overkill; this path is mainly for the *infrastructure* contracts a custom package ships with — `GovernanceRules`-style admin templates, configuration contracts, etc.):

DARs must be uploaded ahead of this call via `/dars/distribute`; `POST /contracts` itself takes no `dar_files` field.

```bash
curl -X POST http://localhost:8080/contracts \
  -H 'Content-Type: application/json' \
  -d '{
    "decentralized_party_id": "my-vault-network::1220abc...",
    "participant_ids": ["node1::1220...", "node2::1220...", "node3::1220..."],
    "participant_parties": ["member1::1220...", "member2::1220...", "member3::1220..."],
    "operator_party": "operator::1220...",
    "contracts": [
      {
        "id": "my-admin-contract",
        "name": "MyAdminContract",
        "package_id": "#my-package-v0",
        "module_name": "MyDomain.Admin",
        "entity_name": "MyAdminContract",
        "fields": [
          { "type": "decentralized_party" },
          { "type": "operator_party" },
          { "type": "party_set", "parties": ["member1::1220...", "member2::1220...", "member3::1220..."] },
          { "type": "governance_threshold" },
          { "type": "rel_time", "microseconds": 86400000000 }
        ]
      }
    ]
  }'
```

The `/contracts` workflow runs `InteractiveSubmissionService.PrepareSubmission` → multi-party signing → `ExecuteSubmissionAndWaitForTransaction`. Minimum 3 participants. See `ARCHITECTURE.md → Workflows → Contracts`.

### Path B — submit the proposal via DAML directly

For the common case — a member proposing an action — the proposer alone is the signatory, so you don't need the multi-party `/contracts` ceremony. Submit a normal `CreateCommand` through any Ledger API client (gRPC, JSON API, daml-script, your own backend) using the member's credentials:

```daml
proposalCid <- submit aliceMemberParty $ createCmd PauseProposal with
  governanceParty = decParty
  proposer        = aliceMemberParty
  targetCid       = someResourceCid
  reason          = "Suspicious activity flagged at 2026-05-21T09:14:00Z"
```

That's all the on-chain work needed. The proposal is now visible to the governance party and can be confirmed and executed via DecMan.

## Confirm and execute via DecMan

These endpoints are package-agnostic for `core_domain` — they only need the proposal contract id. No code change is required to confirm or execute a custom `GovernableAction`.

### Confirm (per member, until threshold is met)

```bash
curl -X POST http://localhost:8080/governance/confirm \
  -H 'Content-Type: application/json' \
  -d '{
    "party_id":           "my-vault-network::1220abc...",
    "rules_contract_id":  "<governance-rules-cid>",
    "action":             { "type": "generic_vote", "description": "placeholder" },
    "governance_type":    "core_domain",
    "proposal_cid":       "<pause-proposal-cid>"
  }'
```

The `action` field is required by the request schema but is **not used** when `governance_type` is `core_domain` — the `proposal_cid` is what identifies the work item. Any well-formed `action` value works as a placeholder.

### List outstanding confirmations

```bash
curl 'http://localhost:8080/governance/confirmations?party_id=my-vault-network::1220abc...'
```

The response groups domain actions under `domain_actions[]`, keyed by `proposal_cid`, with `action_label`, `description`, the per-member `confirmations[]`, and a `can_execute` boolean.

### Execute (any member, once `can_execute` is true)

```bash
curl -X POST http://localhost:8080/governance/execute \
  -H 'Content-Type: application/json' \
  -d '{
    "party_id":           "my-vault-network::1220abc...",
    "rules_contract_id":  "<governance-rules-cid>",
    "action":             { "type": "generic_vote", "description": "placeholder" },
    "confirmation_cids":  ["<conf-cid-1>", "<conf-cid-2>"],
    "disclosed_contracts": [],
    "governance_type":    "core_domain",
    "proposal_cid":       "<pause-proposal-cid>"
  }'
```

If your `executeImpl` exercises a choice on a contract that the governance party can't see by default (typically choice-context entries from an external registry — Canton Coin transfer rules are the canonical case), populate `disclosed_contracts` with the relevant blobs. The standard transfer / accept-transfer proposal paths fetch these from the network registry automatically; bespoke executions need to pass them explicitly.

The wire shape — defined as `DisclosedContractInput` in [`src/server/types.rs`](../src/server/types.rs) — is just `contract_id` plus the base64-encoded `created_event_blob` under the key `blob`. The template id is recovered from the blob server-side; you do not pass it:

```json
"disclosed_contracts": [
  {
    "contract_id": "00abc123...",
    "blob":        "CgQI..."
  }
]
```

You typically obtain `blob` (the `created_event_blob`) from your registry's HTTP endpoint at execute time — DPM does this for the token-standard flows in `maybe_fetch_for_proposal`. If your custom domain has its own off-chain registry, the caller — your backend, a daml-script, or a CLI — is responsible for fetching the blob and threading it into the `/governance/execute` call.

### Granting propose-only rights to non-members

If a non-member party needs to file proposals — an admin tool, a regulatory officer, an off-chain bot — add them to `additionalProposers` via the standard self-management flow. This is a `core_self` action, executed once like any other governance change:

```bash
# 1. Each member confirms the addition
curl -X POST http://node:8080/governance/confirm \
  -H 'Content-Type: application/json' \
  -d '{
    "party_id":          "my-vault-network::1220abc...",
    "rules_contract_id": "<governance-rules-cid>",
    "action":            {
      "type": "governance_add_additional_proposer",
      "additional_proposer": "compliance-bot::1220..."
    },
    "governance_type":   "core_self"
  }'

# 2. Once threshold is met, any member executes
curl -X POST http://node:8080/governance/execute \
  -H 'Content-Type: application/json' \
  -d '{
    "party_id":          "my-vault-network::1220abc...",
    "rules_contract_id": "<governance-rules-cid>",
    "action":            {
      "type": "governance_add_additional_proposer",
      "additional_proposer": "compliance-bot::1220..."
    },
    "confirmation_cids": ["<conf-cid-1>", "<conf-cid-2>"],
    "governance_type":   "core_self"
  }'
```

After execution the `additionalProposers` field is `Some {compliance-bot::...}`, and a proposal where `proposer = compliance-bot::...` passes the proposer-authorization check in `GovernanceRules_ConfirmAction`. Remove with `governance_remove_additional_proposer` — when the set becomes empty it normalizes back to `None`.

Additional proposers can **only propose** — they cannot confirm or execute, even with grants. Voting weight stays exclusively with `members`.

### Cancel or expire

```bash
# member revokes their own confirmation
curl -X POST http://localhost:8080/governance/cancel \
  -H 'Content-Type: application/json' \
  -d '{ "party_id": "...", "confirmation_cid": "...", "governance_type": "core_domain" }'

# any member clears a stale confirmation past expiry
# (GovernanceRules_ExpireConfirmation enforces `member ∈ members`)
curl -X POST http://localhost:8080/governance/expire \
  -H 'Content-Type: application/json' \
  -d '{ "party_id": "...", "rules_contract_id": "...", "confirmation_cid": "...", "governance_type": "core_domain" }'
```

The proposer (the original creator of the `PauseProposal`) can also retract the proposal without a vote by exercising `GovernableAction_ProposerCancel`. The governance party can clean up a stale proposal via `GovernableAction_Cancel` (typically wrapped in a `GenericVote`).

## Querying your own template

Use the generic contract query endpoint to find live instances of your template:

```bash
curl 'http://localhost:8080/contracts/query?party_id=my-vault-network::1220abc...&package_id=%23my-package-v0&module_name=MyDomain.PauseProposal&entity_name=PauseProposal&interface=false'
```

Set `interface=true` to query by interface id instead — useful for listing every active `GovernableAction` regardless of underlying template:

```
package_id=#governance-action-v0
module_name=Governance.Action
entity_name=GovernableAction
interface=true
```

## Testing

### DAML tests

Mirror the layout of `daml/governance-core-test` — a separate `<your-package>-test` package with `daml.yaml` listing your DAR as a data-dependency, and `Daml.Script` tests using the `TestHarness` pattern from [`GenericVoteTest.daml`](../daml/governance-core-test/daml/Governance/Test/GenericVoteTest.daml). Cover at minimum:

- happy path: create proposal → confirm by threshold → execute → assert the side effect and that a `GovernanceExecutionResult` was created with the correct `actionLabel`
- insufficient confirmations cannot execute
- non-member proposer is rejected at confirm time (or accepted if you added them via `SelfAction_AddAdditionalProposer`)
- `ProposerCancel` succeeds and consumes the proposal

Run from the `daml/` directory:

```bash
daml build --all && daml test --all
```

### Rust / integration tests

If you only added a DAML package and no DecMan code, no Rust tests are required. If you also extended `FieldDefinition` or `ProposalType` (server-side change — beyond this guide's scope), follow the project test conventions in [`CLAUDE.md`](../CLAUDE.md): `cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test`.

## Versioning and upgrades

`GovernanceRules` exercises the proposal by `ContractId GovernableAction` — the proposal's own package id is bound into the contract id at creation time. That means:

- Live proposals continue to execute against the version of your package that created them, even if you upload a newer DAR later.
- A new version of your template is a *new* DAML package (e.g. `my-package-v1`), with its own DAR and package id. Coexists with the old one until you drain in-flight proposals.
- Never silently mutate the semantics of an existing template version. Members confirming today's proposal must be able to trust that `executeImpl` will not be redefined under them.

## Self-management changes during in-flight proposals

`GovernanceRules` self-management actions and your custom proposals share state. When a member is added or removed, or the threshold is changed mid-flight, here is what happens to confirmations already on the ledger:

- **`GovernanceConfirmation` contracts survive the change.** They carry the `confirmer` party and the `actionProposalCid` they refer to; they do not pin the `GovernanceRules` contract id or its members snapshot.
- **Execution rechecks members at the new state.** `GovernanceRules_ExecuteConfirmedAction` filters confirmations through `confirmer ∈ members` *at execute time*. A removed member's confirmation is silently dropped from the count; you may dip below `threshold` and stall the proposal.
- **A lowered threshold can suddenly unlock a stalled proposal.** Existing confirmations are immediately usable against the new lower bar.
- **A raised threshold or removed proposer can strand a proposal.** Remaining members must either gather more confirmations under the new rules or expire the stale ones and re-propose.
- **`actionConfirmationTimeout` changes don't retroactively re-stamp `expiresAt`.** Each `GovernanceConfirmation` was minted with `now + oldTimeout` at confirm time; that expiry stays.
- **Removed `additionalProposers` lose confirmability of their in-flight proposals.** `GovernanceRules_ConfirmAction` re-checks `proposer ∈ members ∪ additionalProposers` against the *current* rules state at every confirm. Confirmations already on the ledger stay consumable at execute time (execute only re-checks confirmer membership), but no new confirmations can be added once the proposer's authorization is revoked. Practically: if you've removed an additional proposer, expire their in-flight proposals or have a member re-file them.

Practical implication for custom templates: prefer short `actionConfirmationTimeout` values (minutes to a few hours) for any proposal that touches mutable state. Long timeouts amplify the surface area for member changes to interleave with in-flight votes.

## Gotchas

- **Proposal visibility** — if `governanceParty` is not an observer on your template, `GovernanceRules_ConfirmAction` cannot `fetch` it and confirmation will fail with an authorization error. Always include `observer governanceParty`.
- **Multi-signatory templates can't be `POST /contracts`-created directly when one of the signatories is externally signed.** This is the constraint that motivated `ProvisionProviderService` in `governance-utility-onboarding` (see ARCHITECTURE.md → "Granular onboarding"). If your template has two signatories and one is the governance party, wrap the create in a `GovernableAction` of its own.
- **UTXO drift** — any `ContractId` field on the proposal is resolved at execute time, not at proposal time. If those contracts can be consumed between propose and execute, your action will fail. Either keep timeouts short (`actionConfirmationTimeout` on `GovernanceRules`), or use dedicated contracts that aren't consumed elsewhere. The header comment on [`TransferProposal.daml`](../daml/governance-token-custody/daml/Governance/TokenCustody/TransferProposal.daml) walks through the mitigations.
- **`actionLabel` is the audit grouping key** — pick a stable, distinct PascalCase string per template. Don't recycle `"GenericVote"`. The label is what shows up in the UI and in `GovernanceExecutionResult.actionLabel` forever.

## End-to-end example

A consolidated trace of the lifecycle for the `PauseProposal` template introduced above:

```bash
# 0. Build and distribute the DAR (peer_ids must be non-empty for /dars/distribute)
(cd daml && daml build --all)
BASE64=$(base64 -i daml/my-package/.daml/dist/my-package-v0-0.1.0.dar)
curl -X POST http://coordinator:8080/dars/distribute \
  -H 'Content-Type: application/json' \
  -d "{
    \"dar_files\": [{\"filename\":\"my-package-v0-0.1.0.dar\",\"data\":\"${BASE64}\"}],
    \"peer_ids\":  [\"node2::1220...\", \"node3::1220...\"]
  }"

# 1. Alice creates a proposal (off-DecMan, via daml-script / her own backend)
#    -> obtains proposalCid

# 2. Each member confirms
for CID in $CONF_CIDS; do
  curl -X POST http://node:8080/governance/confirm \
    -H 'Content-Type: application/json' \
    -d "{
      \"party_id\":          \"${DEC_PARTY}\",
      \"rules_contract_id\": \"${RULES_CID}\",
      \"action\":            { \"type\": \"generic_vote\", \"description\": \"x\" },
      \"governance_type\":   \"core_domain\",
      \"proposal_cid\":      \"${PROPOSAL_CID}\"
    }"
done

# 3. Once can_execute is true, any member executes
curl -X POST http://node:8080/governance/execute \
  -H 'Content-Type: application/json' \
  -d "{
    \"party_id\":           \"${DEC_PARTY}\",
    \"rules_contract_id\":  \"${RULES_CID}\",
    \"action\":             { \"type\": \"generic_vote\", \"description\": \"x\" },
    \"confirmation_cids\":  ${CONFIRMATION_CIDS_JSON},
    \"disclosed_contracts\": [],
    \"governance_type\":    \"core_domain\",
    \"proposal_cid\":       \"${PROPOSAL_CID}\"
  }"

# 4. Audit: a GovernanceExecutionResult contract now records the executed action.
curl "http://node:8080/contracts/query?party_id=${DEC_PARTY}&package_id=%23governance-core-v0&module_name=Governance.ExecutionResult&entity_name=GovernanceExecutionResult&interface=false"
```

That is the entire contract between a custom DAML template and DecMan: implement `GovernableAction`, ship the DAR, and drive the lifecycle through the existing API.
