# Add Party — Redevelopment Spec

Adding a new member to an existing decentralized party. This document collects
everything from the original implementation (PR #27, branch `feat/add-party`,
closed unmerged 2026-06-04) and maps it onto the current workflow architecture
(`feat/server/concurrent-multi-instance-workflows`, PR #191) so the feature can
be rebuilt from the ground up.

Companion reference: [CANTON_PARTY_REPLICATION.md](CANTON_PARTY_REPLICATION.md)
(ported from the old branch) — Canton's party replication methods, ACS sync
requirements, and the gRPC API surface.

---

## 1. History

- PR #27 "feat: add party to existing dec party" (author: schronck, opened
  2026-02-05, closed unmerged 2026-06-04). Branch `feat/add-party` still exists
  on origin but shares no ancestor with current `main` — the repo history was
  rewritten during open-sourcing (PR #186), so the branch cannot be rebased.
- The feature was ~3,800 insertions across 51 files: a full
  `src/workflow/add_party/` module, HTTP endpoints, Noise message types, a
  frontend dialog, and shell-based integration test phases.
- The codebase has since been heavily refactored: generic
  `WorkflowState<S>`/`WorkflowStep` state machine, persistent `workflow_runs` /
  `workflow_artifacts` storage with restart recovery, an invite
  accept/decline/retry system, concurrent multi-instance execution with a
  `WorkflowRegistry` and per-instance message routing, a shared topology-retry
  helper, and a Rust integration test harness (`tests/`) replacing
  `run-integration-tests.sh`.

## 2. What the feature did (old implementation)

### 2.1 Roles

- **Coordinator** — an existing member; initiates via HTTP.
- **Existing members** — sign the updated topology transactions.
- **New member** — generates keys, signs, and (when the party has contracts)
  imports the ACS snapshot.

### 2.2 End-to-end flow

1. Coordinator receives `POST /add-party`
   `{ decentralized_party_id, new_participant_id, new_threshold }`.
2. Coordinator sends `InviteAddParty` over Noise to all peers (existing
   members + the new member), waits for them to connect.
3. **GenerateNewMemberKeys** (peer-gated, new member only): the new member
   generates two signing keys via Vault — namespace key
   (`{prefix}-namespace-key`, usage `Namespace`) and DAML key
   (`{prefix}-daml-key`, usage `Protocol`) — proposes its own
   `NamespaceDelegation` to the Authorized store and the synchronizer store
   (restriction `CanSignAllMappings`), and uploads public keys + participant ID
   to the coordinator.
4. **ExportState** (coordinator only): reads current
   `DecentralizedNamespaceDefinition` and `PartyToParticipant` from the
   synchronizer; validates the new participant is not already in the mapping
   and that `1 <= new_threshold <= current_owners + 1`.
5. **CreateProposals** (coordinator only): builds
   - new `DecentralizedNamespaceDefinition`: same namespace hash, existing
     owners + new member's namespace fingerprint (sorted), new threshold;
   - new `PartyToParticipant`: existing participants + new participant with
     `Confirmation` permission, party signing keys merged with the new
     member's DAML key, new threshold;
   both submitted as unsigned proposals (`AddReplace`, `serial: 0`,
   `must_fully_authorize: false`).
6. **SignProposals** (peer-gated, all peers including the new member): each
   signs both proposals via `TopologyManagerWriteService.sign_transactions()`
   and returns them; coordinator aggregates signatures, deduplicating by
   `signed_by`.
7. **SubmitAddParty** (coordinator only): submits DNS first
   (`wait_to_become_effective` 30 s), polls topology until the owner count
   matches, then submits P2P and polls until the participant count matches,
   then waits a configurable propagation delay.
8. **SyncAcs** (conditional): coordinator checks
   `party_has_active_contracts()`. If none — skip to Complete. If contracts
   exist: coordinator exports the ACS via
   `ParticipantRepairServiceClient.export_acs()` at the current ledger end
   (streamed gzip chunks, up to 512 MB), ships it over Noise; the **new member
   only** imports it via `import_acs()` with `ContractImportMode::Validation`
   — which requires `canton.features.enable-repair-commands = true` on the new
   member's participant.
9. **Complete**: coordinator broadcasts `Disconnect`.

### 2.3 HTTP API

- `POST /add-party` → 202 with `{status: "inprogress"}`; 409 if a workflow is
  already in flight.
- `GET /add-party/status` → `{status: inprogress|completed|failed, error}`;
  frontend polled every 2 s.

### 2.4 Noise messages (old numbering)

| Type | Code | Purpose |
|------|------|---------|
| `GenerateAddPartyKeys` | 0x000B | command: new member generates keys |
| `SignAddParty` | 0x000C | command: all peers sign proposals |
| `ImportAcs` | 0x000D | command: new member imports ACS |
| `InviteAddParty` | 0x0013 | invite (now taken by `InviteDars`) |
| `AddPartyKeysUpload` | 0x0206 | data: new member keys → coordinator |
| `AddPartySignatures` | 0x0207 | data: signed proposals → coordinator |
| `AcsSnapshot` / `AcsImportComplete` | 0x0208/0x0209 | reserved, unused |

### 2.5 Persistence (old, file-based)

Per-instance dirs: `current_config_dir/` (existing namespace def, new
threshold), `add_party_proposals_dir/` (unsigned DNS/P2P protos),
`add_party_signed_dir/` (per-peer signed proposals), `keys_dir/`, `ids_dir/`.
The current codebase replaces all of this with `workflow_artifacts` rows.

### 2.6 Frontend

`AddPartyDialog.tsx` opened from `PartyCard`: read-only party ID, peer
dropdown (filtered to peers not already in the party), new-threshold number
input (suggested `ceil((owners + 1) / 2)`), status polling with
spinner/completed/error states.

### 2.7 Integration tests (old shell harness)

Onboard with P1+P2 only → add P3 (verify 3 participants) → kick P3 (verify 2)
→ add P3 again (verify 3). Plus `verify_participant_count()` helper. A manual
test guide existed at `docs/TESTING_ADD_PARTY.md` (key checks: new member sees
existing contracts, can sign, threshold enforced, can't add a member twice).

## 3. Known gaps in the old implementation

From the old branch's own docs and code review:

1. **ACS export used the current ledger end**, not the party's activation
   offset on the new member (Canton's "Method A" prescribes
   `find_party_max_activation_offset`). Contracts created between topology
   effectiveness and export could be duplicated or missed.
2. **No repair-mode preflight** — `check_repair_mode_enabled()` only verified
   the service was reachable; failure surfaced at import time.
3. **No package-vetting check** on the new member before ACS import.
4. **No Observation-first permission path** — the member was added directly
   with `Confirmation`, so between topology effectiveness and ACS import the
   new member could be asked to confirm transactions on contracts it can't
   see.
5. **Topology polling checked counts, not thresholds**, and could not recover
   a proposal stuck in the proposals queue.
6. **No crash/partition recovery** during the (potentially large) ACS
   transfer; old architecture had no persistent workflow state at all.
7. Coordinator started after a blind `sleep(2)` post-invite rather than an
   acceptance handshake.

## 4. Target architecture mapping (current base)

The current framework gives us almost everything the old branch hand-rolled.
A new `add_party` workflow plugs into these extension points:

| Concern | Where it plugs in |
|---------|-------------------|
| Workflow type | `WorkflowType` (`src/workflow/mod.rs`), `WorkflowKind` (`src/server/types.rs`), `ActiveWorkflow` (`src/noise/server.rs`) |
| Step machine | new `AddPartyStep` enum implementing `WorkflowStep` (`src/workflow/state.rs`), generic `WorkflowState<S>` handles peer gating, progress, persistence |
| Module | `src/workflow/add_party/` mirroring `kick/` (config, coordinator, peer, steps/) |
| Noise messages | new command codes (generate keys, sign), data-transfer codes (keys upload, signatures), `InviteAddParty` (note: 0x0013 is now `InviteDars` — new codes required), routed per-instance via `instance_id` |
| Invites | the real invite system: invite cards, accept/decline (`DeclineInvitationPayload` carries `workflow_instance`), `CancelInvite`, `RetryWorkflow`, decline-retry budget |
| Persistence | `workflow_runs` + `workflow_artifacts` rows (no new tables expected); `dec_party_identity` for keys that must survive dismissal |
| Topology submit | `sign_transactions_with_topology_retry` (`src/workflow/topology.rs`) |
| HTTP | `POST /add-party`, `GET` status, `POST` cancel in `src/server/handlers/workflows.rs`, with `preflight_busy_peers()` and `insert_coordinator_run()` |
| Restart recovery | `recover_in_progress_workflows()` resumes both roles |
| Frontend | dialog from the party detail page, same pattern as kick |
| Tests | new phase in `tests/common/phases/` using the `Scenario` DSL |

### Design deltas vs. the old implementation (intentional)

- **Asymmetric peer roles.** The new member does key generation; existing
  members only sign. The old code multicast every command to everyone and
  filtered by participant ID on the receiving end. The new design should
  express this in the step machine (e.g. per-peer command targeting or
  explicit role in the invite config) rather than receiver-side filtering.
- **Invites instead of sleep.** Use invitation accept before the coordinator
  proceeds; the new member's invite must clearly say "you are being added to
  party X", existing members' invites say "member Y is being added".
- **Kick is the structural template** (ExportState → CreateProposals → Sign →
  Submit), but inverted: the added participant must NOT be in the excluded-
  participants set (kick excludes its target; add-party requires its target
  to connect).
- **Threshold validation as in kick**, plus the "already a member" 409 at the
  HTTP layer, mirroring kick's owner-key preflight.
- **Serial handling**: follow the current kick implementation for serial
  bumps on `DecentralizedNamespaceDefinition` / `PartyToParticipant` rather
  than the old `serial: 0` approach.

## 5. Agreed scope (2026-06-12)

Decision: **full parity in one branch** — topology add plus conditional ACS
synchronization, fixing the old implementation's documented gaps along the
way; and **hard fail** whenever the party has active contracts but the ACS
prerequisites cannot be met (repair mode unavailable on the new member),
detected in preflight before any topology change is submitted.

Concretely:

- Topology path (§2.2 steps 1–7 + 9) rebuilt on the current framework:
  workflow module, noise codes, registry/dispatch wiring, HTTP endpoints,
  invite cards, frontend dialog, integration test phases (add P3 to a P1+P2
  party; kick + re-add; already-member 409; contract-guard hard fail).
- ACS path (§2.2 step 8), gap fixes:
  - export at the party's activation offset (Canton 3.4 `ExportPartyAcs`
    if exposed by canton-proto-rs) instead of the current ledger end;
  - repair-mode preflight on the new member before topology submission —
    hard fail with a clear error if unavailable;
  - chunked transfer over Noise reusing the current chunked-transfer
    machinery for large snapshots.
- No contracts → ACS steps are skipped (simple replication), as before.

## 6. As built (2026-06-12)

The rebuild deviates from §5 in one major, deliberate way: the pinned
canton-proto-rs exposes Canton 3.4's **offline party replication** endpoints
(`PartyManagementService.ExportPartyAcs` / `ImportPartyAcs` /
`ClearPartyOnboardingFlag` and the `HostingParticipant.Onboarding` marker),
which supersede the old branch's raw `ParticipantRepairService` approach.
Consequences:

- **No repair mode, no restart.** Canton does still require the new member
  to be disconnected from synchronizers for the duration of the ACS import
  (observed live: `IMPORT_ACS_ERROR: There are still synchronizers
  connected`), but the workflow automates the disconnect/reconnect bracket
  over the admin API — no operator action, no config change, no process
  restart. The repair-mode preflight from §5 became moot — the planned
  hard-fail guard has nothing left to guard. Any remaining ACS-sync failure
  simply fails the workflow with the Canton error, leaving the party safely
  suspended on the new member (Onboarding marker still set), retryable via
  the standard retry machinery.
- **Activation-offset gap fixed by Canton itself**: the coordinator captures
  its ledger offset before submitting topology (admin-API
  `GetHighestOffsetByTimestamp`, no ledger token needed) and passes it to
  `ExportPartyAcs`, which locates the activation and produces a consistent
  snapshot.
- **The new member is suspended until the import lands**: the P2P proposal
  adds it with `Confirmation` permission plus the Onboarding marker; the
  marker is removed by a third topology round after the import, once Canton's
  computed safe time has passed (`ClearPartyOnboardingFlag` polled by the new
  member; clearing proposal authorized by the coordinator and threshold-signed
  by all peers). This is strictly safer than both old options in §6 of the
  original plan (direct `Confirmation` vs `Observation`-first).

Step machine (13 steps, `src/workflow/add_party/`):

```
WaitingForPeers → GenerateNewMemberKeys* → ExportState → CreateProposals
→ SignProposals* → SubmitProposals → SyncAcs* → PrepareClearOnboarding
→ ProposeClearOnboarding* → PrepareClearSign → SignClearOnboarding*
→ SubmitClearOnboarding → Complete          (* = peer-gated)
```

New-member-only commands (GenerateAddPartyKeys, ImportAcs,
ClearOnboardingFlag) are multicast like every command; non-addressed peers
recognise themselves from the config payload and reply with a skip status so
the all-peers-complete gate still fires. The two `Prepare*` coordinator
beats exist because the generic state machine auto-advances out of a
peer-gated step, so consecutive peer-gated steps need a payload swap in
between.

Wire/protocol: commands 0x0020–0x0024, invite 0x0017, data transfers
0x0206–0x0208; `WorkflowKind::AddParty` everywhere the other kinds appear
(registry routing, invites with accept/decline/cancel/retry, restart
recovery, run feed). HTTP: `POST /add-party`, `GET /add-party/status`,
`POST /add-party/cancel`, plus the generic per-instance endpoints.
Persistence: `workflow_artifacts` rows (`add_party_*` kinds), migration
000015 (`pending_invitations.new_participant`). New-member identity rows are
copied into `dec_party_identity` on both the coordinator and the new member,
so post-add kicks/contracts see the grown member set.

Known limitations / IT verification points:

1. The ACS snapshot rides the existing chunked Noise transfer, capped at
   16 MiB (`MAX_CHUNKED_TOTAL_SIZE`). Parties with a larger ACS need that
   cap raised (one constant, with memory-bound review).
2. The full protocol is verified live by the integration phases
   (`tests/common/phases/add_party_edge_cases.rs` then `add_party.rs`,
   wired after `kick`). Hard-won facts from that stabilization, now encoded
   in the implementation:
   - `PartyManagementService` wants the LOGICAL synchronizer id
     (`alias::fingerprint`, no protocol-version suffix);
   - the begin-offset for the activation finders must postdate any EARLIER
     activation of the same (party, participant) pair — the finders take
     the FIRST activation after the offset, so kick-then-re-add breaks
     with an early offset (capture order: authenticated `GetLedgerEnd`
     via WorkflowAuth → admin `GetHighestOffsetByTimestamp` → offset 1,
     loudly warned);
   - `ImportPartyAcs` requires a synchronizer disconnect for the import
     window (automated bracket, reconnect guaranteed);
   - the flag-clearing transaction must be AUTHORED by the onboarding
     participant itself (the coordinator's authorize gets
     `TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE`); the new member
     authors it after Canton's safe time and ships it for the
     threshold-signing round.
   - because the new member (not the coordinator) authors the flag-clear,
     the coordinator adds its OWN signature to the clearing proposal before
     submitting it — the add proposals get the coordinator's signature for
     free via authoring, the clear does not. This raises the clear round's
     fault tolerance (below full threshold the clear no longer has to
     exclude the coordinator) and is a prerequisite for ever supporting a
     full `new_threshold == owner count` party (see open item below).
   Edge-case coverage: validation 400s/409s (thresholds, self, unknown
   participant/party), decline cascade (new member declines → coordinator
   fails fast, sibling card dropped), cancel cascade (un-accepted card
   dropped + accepted peer run cancelled), plus the already-member and
   same-party guards in the happy-path phase.
3. `new_threshold` stays a free input with a majority suggestion in the UI
   (parity with the old dialog).
4. Open items surfaced by the 2026-06-15 audit (against this doc + the
   "Decentralized Parties in Practice" note), NOT yet addressed:
   - **`new_threshold == owner count` (unanimity) is unsupported.** Observed
     in CI (2026-06-16): with the threshold equal to the post-add owner
     count, the P2P add transaction never becomes effective — the new
     participant "did not appear in the P2P mapping after 30 attempts", so
     the run fails at SubmitProposals, before the clear round. The HTTP /
     ExportState validation still ALLOWS `1 <= new_threshold <= owners`;
     callers must keep the threshold strictly below the owner count until
     the full-threshold add is understood/fixed (candidate guard: cap
     validation at `owners - 1`). The happy-path phase therefore re-adds P3
     at threshold 2 of 3, not 3 of 3.
   - **Fresh-node add is unverified.** That note records two restrictions:
     a node added to a dec party "must be a fresh, empty node", and a
     removed node "currently breaks and cannot perform other operations
     afterwards". The integration suite only re-adds a previously-kicked
     P3 — the non-fresh path the offset tiers (`current_ledger_offset`,
     the `INVALID_STATE` export retry) exist to work around. Adding a
     never-before-member node via this workflow has no coverage (the
     fixture has only P1–P3, all founding members). The offset-1 fallback
     is correct only for a genuinely fresh participant; confirm DA's
     current stance for Canton 3.5.3 before relying on re-add in prod.
   - **No `manual_connect` during the import window.** `import_party_acs`
     disconnects → imports → reconnects in-process; a crash mid-import
     would let the half-imported node auto-reconnect. Canton's procedure
     sets `manual_connect=true` for the window, but that needs a
     connection-config read-modify-write round-trip (deferred — not worth
     shipping unverified for a sub-second crash window).
   - **ACS re-import idempotency unconfirmed.** A transient mid-import
     failure retries the whole `ImportAcs`; whether `ImportPartyAcs`
     (Validation mode) is idempotent after a partial import is unverified.
   - **No coordinator-side timeout on peer-gated steps.** If the new
     member can never clear the flag, the coordinator idles until
     cancelled (framework-wide, not add-party-specific).
   - **Test gaps:** the empty-ACS / simple-replication path (the party
     always has contracts on re-add), add-party restart/crash recovery,
     and an existing-member (not new-member) decline cascade.
