# Noise sunset: Canton-native coordination

**Status:** implementation spec v2, 2026-09-14 (after adversarial review).
**Scope:** remove the Noise P2P transport from decman. Nodes coordinate only through
Canton: the synchronizer topology store, Daml contracts, and the Ledger API.

This document is the shared contract for the implementation. Every section states a
decision. Section 13 summarizes the research that grounds each decision.

---

## 1. Thesis

Canton is already a peer-to-peer network. Every operation decman coordinates over Noise
has a native Canton equivalent:

| Noise did this | Canton does this |
|---|---|
| Ship a topology proposal to peers for signing | Partially-signed proposals propagate through the synchronizer store to every member (`proposal = true`) |
| Collect signatures and resubmit | Each member co-signs by `Authorize { transaction_hash }`; Canton merges signatures by fingerprint |
| Announce a node, its version, its keys | A self-signed Daml registry contract (`DecmanNode`) plus the self-signed root `NamespaceDelegation` in the topology store |
| Invite a peer to a workflow | A Daml `WorkflowProposal` with the invitee as observer |
| Accept or decline | A Daml choice on that proposal |
| Collect Daml transaction signatures | A Daml `SubmissionRound` with `SubmissionSignature` records |
| Push DAR bytes | A hash-pinned `DarPin`; each operator uploads locally; vetting is visible in the topology store |
| Ship an ACS snapshot | Offline party replication stays; the operator moves the file; an on-ledger `AcsManifest` pins it |
| Ping for liveness | A Daml heartbeat on `DecmanNode` |

No decman node ever opens a connection to another decman node.

## 2. Decisions

### D1. Node identity: a node party

Each decman node has one **node party**. The node party is a normal Canton party hosted on
the node's participant with **Submission permission and confirmation threshold 1**. A party
with Confirmation permission only cannot submit Daml commands, so it cannot serve as a node
party. The node party has a Ledger API user with `CanActAs` and `CanReadAs` on itself. It
exists before any decentralized party exists.

Storage: a `party_credentials` row with `kind = 'node'` and `dec_party_id = <node party>`.
`AuthRegistry::get(node_party)` returns its token manager unchanged. Migration `000020`
adds `kind TEXT NOT NULL DEFAULT 'decparty'`. Decparty-oriented views (`GET /auth/status`,
party lists) filter `kind = 'decparty'`. Rows with `kind = 'node'` are **excluded** from the
inbound JWT trusted-issuer set (`find_trusted`), so a node identity never widens who can log in.

HTTP: `GET /node-identity` and `PUT /node-identity`. The PUT requires the admin role. It is
exempt from authentication only while `party_credentials` is **entirely empty**, under the
same predicate and mutex as today's `PUT /party-config`. An upgraded node that already holds
decparty rows sets its node identity with an admin JWT. Body = `PartyConfigRequest` without
`dec_party_id` (the member party id field carries the node party). The handler reads the head
`PartyToParticipant` of the node party and rejects it unless this participant hosts it with
Submission permission. `POST /party-config/discover-member-party` pre-fills the form.

The node party signs: the registry entry, workflow proposals, acceptances, declines,
submission rounds, signatures, ACS manifests, and heartbeats.

### D2. Discovery: one string out of band

Operators exchange one string per peer: `participant_id,node_party_id,name`. The UI button
"Share my identity" copies it. "Add peer" pastes it. The `peers` table keeps
`(participant_id PK, name, party)` and drops `address`, `port`, `public_key`. `party` is
now mandatory for a usable peer.

Before a node names a peer node party as observer of any contract, it verifies through the
tokenless Admin API that the party is hosted on the claimed participant with **Submission**
permission (`ListPartyToParticipant(filter_party = node_party)`, head state, `mapping.party ==
node_party`, participant present with `Submission`). Every registry read is keyed by the
contract **signatory** (`node == peers.party`); the `participantId` field is a claim that is
cross-checked with the same hosting read before use. This is the only trust check; the
security boundary stays the operator's own co-signature.

### D3. Registry: `DecmanNode`

Every node publishes one `DecmanNode` contract (signatory = node party, observers = peer node
parties). Fields: `participantId`, `displayName`, `version`, `buildVersion`,
`coordinationVersion`, `peers`, `lastActiveAt`, `heartbeatIntervalSecs`,
`minHeartbeatIntervalSecs`. Choices: `DecmanNode_Heartbeat` (re-create with
`lastActiveAt = now`, guarded by the minimum interval), `DecmanNode_Update`, `DecmanNode_Retire`.

Observers are only those peers whose participant has **vetted the coordination package**
(`ListVettedPackages(filter_participant)`, validity window applied). Canton rejects a create
whose informee participant has not vetted the package, so an unvetted peer is left out and
retried on the next observer tick. Publication at startup is a background task and never
blocks `start_server`. `DecmanNode_Update` runs only when the on-ledger fields differ from the
desired state. `/participants-status` and `GET /registry` report "peer has not vetted the
coordination DAR" explicitly.

Heartbeat cadence is `DECPM_HEARTBEAT_INTERVAL_SECS` (default 3600). The template floor
`minHeartbeatIntervalSecs` is written as `min(DECPM_HEARTBEAT_MIN_INTERVAL_SECS (default 60),
DECPM_HEARTBEAT_INTERVAL_SECS)`. A peer is `Stale` when `now - lastActiveAt >
DECPM_PEER_STALE_FACTOR (default 3) × heartbeatIntervalSecs`. The UI labels the value
"last heartbeat N ago", not liveness. Tests set the interval to 5 s and the floor to 1 s.

Peer health: `/participants-status` is served from an in-memory snapshot the observer loop
refreshes. `ConnectionStatus` becomes `CurrentNode | Active | Stale | Unknown | Unvetted`.
`latency_ms` and `workflow` are removed. New fields: `last_seen_at`, `heartbeat_age_secs`,
`node_party`. `version` and `build_version` come from the registry entry. Without a node
identity the endpoint returns every peer as `Unknown` and the UI shows a "configure node
identity" banner. `DecmanNode` contracts whose signatory is not in the peers table appear
only in the `inbound` bucket of `GET /registry`.

Version gate: a proposer refuses to start a workflow (HTTP 409 naming the participants) unless
every invitee (a) has vetted the coordination package, (b) has a visible registry entry, and
(c) reports `coordinationVersion >= COORDINATION_VERSION`. The 409 text distinguishes "no
entry visible (peer has not added you, or has not vetted)" from "entry too old".
`MIN_PEER_VERSION` and `WIRE_VERSION` are deleted. `COORDINATION_VERSION = 1`. The decman
crate version becomes `2.0.0`.

### D4. Key model: one dual-usage key per member per decentralized party

For every NEW decentralized party each member generates ONE vault key named `{prefix}-key`
with usages `[Namespace, Protocol]`. Its self-signed root `NamespaceDelegation` (published to
the Authorized store with `must_fully_authorize = true`, exactly as `generate_keys` does
today) carries the full `SigningPublicKey` into the synchronizer store. The proposer reads it
with `ListNamespaceDelegation(store = Synchronizer, operation = ADD_REPLACE, filter_namespace =
fp, filter_target_key_fingerprint = fp)`, requires `valid_from <= now`, asserts
`fingerprint(target_key) == fp`, and copies `target_key` verbatim into
`PartyToParticipant.party_signing_keys`. The NSD `target_key` is the single source of key
bytes; the `signingPublicKeyHex` on acceptances is informational. The DND owner fingerprint
equals the party signing-key fingerprint, so kick attribution is on-chain for these parties.

Legacy parties (two keys `{prefix}-namespace` + `{prefix}-daml-transactions`) keep working:
kick, add-party, change-threshold sign with `signed_by = []` and Canton auto-selects the legacy
namespace key. Own-Daml-key lookup is on-chain first (`party_signing_keys ∩ ListMyKeys`),
then `dec_party_identity`, then the legacy vault name.

Key generation is idempotent by name (existing `get_or_create_signing_key`). A KMS spike
(generate the dual key on a KMS participant, publish its root NSD, co-sign a P2P, run one
contracts round with an ECDSA/DER signature) is a rollout precondition (section 10).

### D5. Topology coordination: proposals in the synchronizer store

Proposer:
1. Read the accepted mapping (`proposals = false`, `Snapshot(MaxValue)`) to get serial `S`.
   The `WorkflowProposal` already records `dndBaseSerial`/`p2pBaseSerial`; if `S` differs, the
   run fails ("topology moved").
2. `Authorize { proposal { ADD_REPLACE, serial = S + 1 (or 1), mapping }, must_fully_authorize =
   false, signed_by = [], store = Synchronizer(physical_id) }`. DND and P2P proposals never go
   to the Authorized store and never carry `FORCE_FLAG_ALLOW_UNVALIDATED_SIGNING_KEYS`.
3. Record the transaction hash on the run row: compute it locally as
   `multihash_sha256(HashPurpose 11 || response.transaction.transaction)` (the versioned
   envelope bytes), the same computation the external-party path already performs.

Members (observer loop, every `DECPM_OBSERVER_POLL_SECS`, default 3; mainnet guidance 10):
1. For every accepted, in-progress run: `ListDecentralizedNamespaceDefinition(filter_namespace =
   party namespace)` and `ListPartyToParticipant(filter_party = party id)` with
   `BaseQuery { store = Synchronizer, proposals = true, operation = ADD_REPLACE, time_query =
   Snapshot(MaxValue) }`. The onboarding namespace is `computeNamespace(owners)` from the
   accepted proposal; the party id is `prefix::namespace`.
2. Keep proposals with `BaseResult.operation == ADD_REPLACE`, whose `signed_by_fingerprints`
   contains the proposer's owner fingerprint and not this node's fingerprint.
3. Validate against the accepted proposal and head state (section 5).
4. Guard: `proposal.serial == accepted.serial + 1` and `accepted.serial == base serial` from
   the `WorkflowProposal` (or `1` with no accepted mapping).
5. In the same tick, immediately before signing: re-read the `WorkflowProposal` as active, the
   local run row as `inprogress`, and `proposal_decisions` for the pinned hash (or pin it now).
   Never sign from a cached match.
6. Co-sign: `Authorize { transaction_hash = hex(BaseResult.transaction_hash),
   must_fully_authorize = false, signed_by = [], store = Synchronizer }`. Retry
   `TOPOLOGY_TRANSACTION_NOT_FOUND` on the next poll. Treat
   `TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE` with the existing retry budget.
7. Completion: poll `proposals = false` until `serial == S + 1` and `valid_from <= now`.
8. Spend rule: once a matching transaction is effective at `base + 1`, or the run row is
   terminal, the proposal is spent for that mapping and never matched again.

The unfiltered scan (`proposals = true`, no filter) runs only for `GET /proposals/unsolicited`
and every 60 s for the UI; it never signs.

Order: DND first, wait until effective in the synchronizer store on this node, then P2P. This
holds for onboarding, add-party, kick, and change-threshold. Before proposing a DND, wait until
every owner's root `NamespaceDelegation` is effective in the synchronizer store.

Cancel: a topology proposal cannot be withdrawn. Cancel archives the `WorkflowProposal`;
members stop co-signing because the match disappears. A cancelled onboarding proposal can never
become effective without the invitees. A cancelled kick or change-threshold proposal that
already reached the owner threshold **still becomes effective**; the UI says so.

The clearing of a joiner's Onboarding flag needs only the joiner's participant signature. The
joiner calls `ClearPartyOnboardingFlag` and polls until `onboarded == true`. No co-sign round
exists for it.

### D6. Workflow intent: `WorkflowProposal`

Every run starts with a `WorkflowProposal` (signatory = proposer node party, observers =
invitee node parties). It carries the human-readable intent and the reference set members
validate against, plus the proposer's own key material (`proposerNamespaceFingerprint`,
`proposerSigningPublicKeyHex`, `proposerDamlKeyFingerprint`), the head serials at creation
(`dndBaseSerial`, `p2pBaseSerial`, `previousThreshold`), and `expiresAt` (default 7 days).
The coordinator therefore generates its key **before** creating the proposal.

Invitees `Accept` (non-consuming, creates `WorkflowAcceptance`) or `Decline` (creates
`WorkflowDecline`). The proposer `Cancel`s or `Finish`es it. Every `WorkflowAcceptance` carries
the acceptor's `participantId`, its `namespaceFingerprint` and `signingPublicKeyHex` (onboarding
and add-party joiner), and its `damlKeyFingerprint` for the party (all kinds; feeds
`dec_party_participant.signing_key`).

`POST /invitations/{cid}/accept` records the operator's decision locally and opens the peer
run row. The observer makes the ledger write on the next tick. Every member publishes its
acceptance, whatever the kind: a coordinator counts acceptances before it acts, so a member
that publishes none deadlocks both sides in silence. `engine::drive` therefore makes the write
for every kind. A kind whose acceptance carries key material a member step generates opts out
through `KindDriver::member_publishes_own_acceptance` and writes it at that step instead;
onboarding, add-party and kick are the three that do.

An acceptance is **counted** only if: `acceptance.proposal == proposalCid`,
`acceptance.proposer == proposal.proposer`, `acceptor ∈ proposal.invitees`,
`acceptance.participantId ∈ proposal.participants`, and the acceptor's node party is hosted
on `participantId` (D2 check). Exactly one acceptance per acceptor is allowed; conflicting key
material fails the run closed. A `WorkflowAcceptance` is created only through the `Accept`
choice; a direct create by a non-invitee fails the predicates above.

Kick: `invitees` = remaining members only. The kicked node sees the topology proposal as
unsolicited.

Consent model: the operator accepts once. After that the node co-signs every topology proposal
that matches the accepted `WorkflowProposal`, its counted acceptances, its base serials, and
section 5. The proposal is spent per mapping (D5 step 8) and expires at `expiresAt`.

Accept flow: `POST /invitations/accept` writes `proposal_decisions(proposal_cid, 'accepted')`
and the peer run row at its first step. The next observer tick generates the key (when the
kind needs one), waits for the root NSD in the synchronizer store, exercises
`WorkflowProposal_Accept` with the key material, and advances. `POST /invitations/decline`
writes `'declined'` and exercises `WorkflowProposal_Decline` at once.

### D7. Contracts workflow: `SubmissionRound`

The proposer runs `PrepareSubmission` once per contract definition with `max_record_time = now
+ 20h`, reads the live `preparationTimeRecordTimeTolerance` from the synchronizer parameters,
and creates a `SubmissionRound` per prepared transaction (bytes as lowercase hex, hash,
hashing scheme version, `preparationTime`, `maxRecordTime`, `deadline = min(maxRecordTime,
preparationTime + tolerance) − 30 min`). `signers` = invitee node parties. The proposer signs
locally and adds its own signature at execute time.

The proposer advances when the counted acceptances plus itself reach
`party_signing_keys.threshold`, not when every invitee has accepted. A contracts run needs
threshold signatures, so one silent operator must not hold up a party that has the quorum to
act. An explicit `Decline` still fails the run: silence and refusal are different answers.

A member that accepted the proposal: requires `round.decPartyId == acceptedProposal.decPartyId`,
`act_as == [that party]`, `maxRecordTime` present, `deadline > now`, its own key fingerprint
∈ `party_signing_keys` of the head P2P; recomputes the hash with `canton_hash`; decodes the
transaction and requires every root node to be a `Create` whose package resolves to a name in
`acceptedProposal.packageNames`; then signs with its per-decparty key (vault export or KMS)
and exercises `SubmissionRound_Sign`.

The proposer, before counting: `signedBy ∈ fingerprints(party_signing_keys)`, the signature
verifies locally against that key (Ed25519 CONCAT, ECDSA DER), deduped by fingerprint. When
the count reaches `party_signing_keys.threshold` it executes one `ExecuteSubmissionAndWait`
with exactly the verified set and closes the round. Expired rounds are re-prepared and
re-signed. `threshold = None` on any `WorkflowProposal` fails closed.

### D8. DARs: hash-pinned local upload

`POST /dars/distribute` creates a `WorkflowProposal(kind = Dars)` with `DarPin { filename,
sha256Hex, mainPackageId, sizeBytes }` per file and uploads locally. Each invitee's UI shows the
pins; the operator uploads the same files through `POST /dars/upload` with `pin_instance =
WorkflowProposal.runId`. The handler refuses a file whose sha256 does not match a pending pin
and always passes `expected_main_package_id = pin.mainPackageId`. The proposer observes every
participant's `VettedPackages` (`ListVettedPackages(filter_participant)`, validity window) and
completes the run when every pin is vetted everywhere. `/packages/compare-peers` reads vetted
packages per participant from the topology store. No DAR bytes travel over any decman channel.

The coordination DAR (`releases/v1/decman-coordination-v1-0.1.0.dar`) is embedded with
`include_bytes!` and uploaded and vetted by a background task at startup with retry
(`DECPM_AUTO_UPLOAD_COORDINATION_DAR`, default true; `expected_main_package_id` pinned;
`synchronizer_id` = the logical synchronizer id, which is what `UploadDarRequest` parses);
state appears in `/node-health`. This
is a policy change from operator-accepted vetting and is documented as such.
`POST /dars/upload` remains the manual fallback.

### D9. ACS synchronization: offline replication, operator-moved file

Canton online party replication is alpha and unusable on protocol version 35. Offline
replication stays. Every current host captures `ADD_PARTY_EXPORT_OFFSET` (`capture_offset_once`,
keyed by party, joiner, base serial) as the first action of `CoSignChanges`, before its
`Authorize`; the joiner captures its pre-activation offset the same way.
`persisted_or_derived_offset` stays as the fallback.

When a current host observes the P2P that marks the joiner `Onboarding` become effective, it
exports the snapshot to `DECPM_ACS_SPOOL_DIR` and publishes an `AcsManifest` (party, target
participant, activation serial, exporter participant, size, sha256, package ids). The joiner
accepts a manifest only if: (1) the signatory `exporter` is hosted on `exporterParticipant` with
Submission (D2 check); (2) `exporterParticipant` is in the decparty's head hosts with
`onboarding == None`; (3) `exporter` equals the node party recorded for that participant in the
peers table; (4) `activationSerial` equals the earliest serial whose mapping marks the joiner
Onboarding, and the head P2P still marks the joiner Onboarding. These checks also gate the
`sizeBytes == 0` fast path, which skips the import and clears the flag.

* `GET /acs-export/{party}/{target}?serial=N` (admin JWT, streamed gzip) on any current host.
* `POST /acs-import/{party}?serial=N&exporter=<participant>` (admin JWT, streamed body) on the
  joiner: match that exporter's manifest; verify sha256 and size; verify every manifest package
  id is vetted locally; then run the existing disconnect / `ImportPartyAcs` / reconnect bracket
  and clear the flag.

The joiner's run waits in `SyncAcs` until the import completes or the fast path fires. Spool
files are deleted when the joiner is observed `onboarded == true` or the run is dismissed.

### D10. Cancel, decline, retry

* Cancel (proposer): exercise `WorkflowProposal_Cancel`, mark the run `Cancelled`. Members
  see the proposal vanish and mark their peer rows `Cancelled`.
* Decline (invitee): exercise `WorkflowProposal_Decline`. For all-invitee kinds (onboarding,
  add-party, contracts with all signers required, dars) the proposer marks the run `Failed`
  ("Peer X declined the invitation") and exercises `WorkflowProposal_Finish { succeeded =
  false }`. For quorum kinds (kick, change-threshold) a decline fails the run only when the
  remaining invitees cannot reach the quorum (section 6). Sibling invitees of a failed run mark
  their rows `Cancelled`.
* Retry: idempotent "ensure". The proposer re-reads the accepted serial and re-proposes only
  if its proposal is gone and the base serial still holds. Members re-issue
  `Authorize { transaction_hash }`. No broadcast.
* Dismiss: unchanged.
* Housekeeping: an archive sweep in the observer tick archives this node's acceptances,
  declines, and signatures of finished proposals.

### D11. Projection tables and concurrency

`workflow_runs` stays as the UI projection with the same status vocabulary. New columns:
`proposal_cid`, `coordinator_participant` (renamed from `coordinator_pubkey`),
`coordinator_party`, `topology_hashes_json`, `member_variant` (`Joiner`/`Member`/NULL).
The start handler persists the run row with `proposal_cid` only after `submit_and_wait`
returns; the observer ignores rows without one. Peer `instance_name =
peer-{kind}-{coordinator_participant_short}-{coordinator_runId}`.

`pending_invitations` stays as the local cache of unaccepted `WorkflowProposal`s that name
this node: `id = proposal_cid`, `coordinator_pubkey` → `coordinator_participant`, plus
`proposal_cid`, `coordinator_party`. `proposal_decisions(proposal_cid, decision, decided_at,
pinned_hashes_json)` is the idempotency guard against re-projecting dismissed or declined
proposals and the store for pinned transaction hashes. `workflow_artifacts` stays for
local-only artefacts (keys, offsets, spool paths).

Concurrency: there is no long-lived task per run. The observer holds a per-run
`tokio::Mutex` (`try_lock`; a busy run is skipped this tick) so at most one tick drives a run.
Cancel, dismiss, retry are row operations that the driver re-reads before every ledger or
topology write. `/{kind}/status` and the start-handler 409 gate read `workflow_runs` only.
`WorkflowRegistry`, `WorkflowGuard`, and `HttpWorkflowState.abort_handle` are removed.

### D12. Rollout

Old builds refuse to start a workflow when any invitee's Noise listener is gone. New builds
refuse until every invitee has vetted the package and published a registry entry. So a mixed
window is safe. Migration 000020 fail-marks every `inprogress` run.

Runbook: (1) while every node still runs 1.8.x, call `GET /decentralized-parties?refresh=true`
on every node and treat a NULL `dec_party_participant.signing_key` as a blocker; (2) drain and
verify empty `/workflows` and `/invitations` everywhere; (3) each operator, inside the window:
stop, retag to 2.0.0, start; (4) `PUT /node-identity` with an admin JWT; (5) exchange
`participant_id,node_party_id,name` with every peer and re-add every peer; (6) verify
`GET /registry` lists each peer under `peers` and `inbound`, and `/packages/vetted` lists
`decman-coordination-v1`; (7) start workflows. A 409 names the members still to upgrade.

Rollback is manual and lossy: stop the node, apply `000020_noise_sunset.down.sql` with
`sqlite3`, delete the version-20 row from `_sqlx_migrations`, re-enter Noise peers by hand,
restart 1.8.x. The new build never reads or modifies `data/noise.key`.

## 3. Daml package `decman-coordination-v1`

Location: `daml/decman-coordination/` (`name: decman-coordination-v1`, `version: 0.1.0`,
`sdk-version: 3.4.11`, `--target=2.2`, dependencies: `daml-prim`, `daml-stdlib` only).
Test package: `daml/decman-coordination-test/` with `daml-script` and
`../dars/testlib-0.1.0.dar`. Both join `daml/multi-package.yaml`. The built DAR is committed
to `releases/v1/decman-coordination-v1-0.1.0.dar`. CI gets a `dpm test` step.

Text encoding: fingerprints and hashes are Canton hex strings (`1220...`). Public keys are the
lowercase hex of the serialized protobuf `SigningPublicKey`. Signatures are lowercase hex of
the raw signature bytes. Hex validation happens in Rust; templates only bound lengths.

```daml
module Decman.Coordination.Types where

data WorkflowKind = Onboarding | AddParty | Kick | ChangeThreshold | Contracts | Dars
  deriving (Eq, Show)

data DarPin = DarPin
  with
    filename : Text
    sha256Hex : Text
    mainPackageId : Text
    sizeBytes : Int
  deriving (Eq, Show)
```

```daml
module Decman.Coordination.Node where

import DA.Time (addRelTime, seconds)

template DecmanNode
  with
    node : Party
    participantId : Text
    displayName : Text
    version : Text
    buildVersion : Text
    coordinationVersion : Int
    peers : [Party]
    lastActiveAt : Time
    heartbeatIntervalSecs : Int
    minHeartbeatIntervalSecs : Int
  where
    signatory node
    observer peers
    ensure heartbeatIntervalSecs > 0
      && minHeartbeatIntervalSecs >= 1
      && minHeartbeatIntervalSecs <= heartbeatIntervalSecs
      && coordinationVersion >= 1

    choice DecmanNode_Heartbeat : ContractId DecmanNode
      controller node
      do
        now <- getTime
        assertMsg "heartbeat too soon"
          (now >= addRelTime lastActiveAt (seconds minHeartbeatIntervalSecs))
        create this with lastActiveAt = now

    choice DecmanNode_Update : ContractId DecmanNode
      with
        newPeers : [Party]
        newDisplayName : Text
        newVersion : Text
        newBuildVersion : Text
        newCoordinationVersion : Int
        newHeartbeatIntervalSecs : Int
        newMinHeartbeatIntervalSecs : Int
      controller node
      do
        now <- getTime
        create this with
          peers = newPeers
          displayName = newDisplayName
          version = newVersion
          buildVersion = newBuildVersion
          coordinationVersion = newCoordinationVersion
          heartbeatIntervalSecs = newHeartbeatIntervalSecs
          minHeartbeatIntervalSecs = newMinHeartbeatIntervalSecs
          lastActiveAt = now

    choice DecmanNode_Retire : ()
      controller node
      do pure ()
```

```daml
module Decman.Coordination.Workflow where

import Decman.Coordination.Types

template WorkflowProposal
  with
    proposer : Party
    proposerParticipant : Text
    proposerNamespaceFingerprint : Optional Text
    proposerSigningPublicKeyHex : Optional Text
    proposerDamlKeyFingerprint : Optional Text
    runId : Text
    kind : WorkflowKind
    invitees : [Party]
    participants : [Text]
    decPartyId : Optional Text
    prefix : Optional Text
    threshold : Optional Int
    previousThreshold : Optional Int
    dndBaseSerial : Optional Int
    p2pBaseSerial : Optional Int
    newParticipant : Optional Text
    kickedParticipant : Optional Text
    darPins : [DarPin]
    packageNames : [Text]
    description : Text
    createdAt : Time
    expiresAt : Time
  where
    signatory proposer
    observer invitees
    ensure expiresAt > createdAt

    nonconsuming choice WorkflowProposal_Accept : ContractId WorkflowAcceptance
      with
        acceptor : Party
        participantId : Text
        namespaceFingerprint : Optional Text
        signingPublicKeyHex : Optional Text
        damlKeyFingerprint : Optional Text
        memberParty : Optional Party
      controller acceptor
      do
        assertMsg "not invited" (acceptor `elem` invitees)
        now <- getTime
        assertMsg "proposal expired" (now < expiresAt)
        create WorkflowAcceptance with
          proposal = self
          proposer
          acceptor
          observers = invitees
          runId
          participantId
          namespaceFingerprint
          signingPublicKeyHex
          damlKeyFingerprint
          memberParty
          acceptedAt = now

    nonconsuming choice WorkflowProposal_Decline : ContractId WorkflowDecline
      with
        decliner : Party
        reason : Text
      controller decliner
      do
        assertMsg "not invited" (decliner `elem` invitees)
        now <- getTime
        create WorkflowDecline with
          proposal = self
          proposer
          decliner
          observers = invitees
          runId
          reason
          declinedAt = now

    choice WorkflowProposal_Cancel : ()
      controller proposer
      do pure ()

    choice WorkflowProposal_Finish : ContractId WorkflowOutcome
      with
        succeeded : Bool
        error : Optional Text
      controller proposer
      do
        now <- getTime
        create WorkflowOutcome with
          proposer
          runId
          kind
          observers = invitees
          succeeded
          error
          finishedAt = now

template WorkflowAcceptance
  with
    proposal : ContractId WorkflowProposal
    proposer : Party
    acceptor : Party
    observers : [Party]
    runId : Text
    participantId : Text
    namespaceFingerprint : Optional Text
    signingPublicKeyHex : Optional Text
    damlKeyFingerprint : Optional Text
    memberParty : Optional Party
    acceptedAt : Time
  where
    signatory acceptor
    observer proposer, observers

    choice WorkflowAcceptance_Archive : ()
      controller acceptor
      do pure ()

template WorkflowDecline
  with
    proposal : ContractId WorkflowProposal
    proposer : Party
    decliner : Party
    observers : [Party]
    runId : Text
    reason : Text
    declinedAt : Time
  where
    signatory decliner
    observer proposer, observers

    choice WorkflowDecline_Archive : ()
      controller decliner
      do pure ()

template WorkflowOutcome
  with
    proposer : Party
    runId : Text
    kind : WorkflowKind
    observers : [Party]
    succeeded : Bool
    error : Optional Text
    finishedAt : Time
  where
    signatory proposer
    observer observers

    choice WorkflowOutcome_Archive : ()
      controller proposer
      do pure ()
```

```daml
module Decman.Coordination.Submission where

template SubmissionRound
  with
    proposer : Party
    runId : Text
    index : Int
    signers : [Party]
    decPartyId : Text
    actAs : Text
    description : Text
    preparedTransactionHex : Text
    preparedHashHex : Text
    hashingSchemeVersion : Int
    preparationTime : Time
    maxRecordTime : Time
    deadline : Time
  where
    signatory proposer
    observer signers
    ensure deadline <= maxRecordTime

    nonconsuming choice SubmissionRound_Sign : ContractId SubmissionSignature
      with
        signer : Party
        participantId : Text
        signedBy : Text
        signatureHex : Text
        format : Text
        algorithm : Text
      controller signer
      do
        assertMsg "not a signer" (signer `elem` signers)
        now <- getTime
        assertMsg "round expired" (now < deadline)
        create SubmissionSignature with
          round = self
          proposer
          signer
          observers = signers
          runId
          index
          participantId
          signedBy
          signatureHex
          format
          algorithm
          signedAt = now

    choice SubmissionRound_Close : ()
      with
        result : Text
      controller proposer
      do pure ()

template SubmissionSignature
  with
    round : ContractId SubmissionRound
    proposer : Party
    signer : Party
    observers : [Party]
    runId : Text
    index : Int
    participantId : Text
    signedBy : Text
    signatureHex : Text
    format : Text
    algorithm : Text
    signedAt : Time
  where
    signatory signer
    observer proposer, observers

    choice SubmissionSignature_Archive : ()
      controller signer
      do pure ()
```

```daml
module Decman.Coordination.Acs where

template AcsManifest
  with
    exporter : Party
    exporterParticipant : Text
    observers : [Party]
    decPartyId : Text
    targetParticipant : Text
    activationSerial : Int
    sizeBytes : Int
    sha256Hex : Text
    packageIds : [Text]
    exportedAt : Time
  where
    signatory exporter
    observer observers
    ensure sizeBytes >= 0 && activationSerial >= 1

    choice AcsManifest_Archive : ()
      controller exporter
      do pure ()
```

Daml tests (given/when/then, `dpm test`): heartbeat honours the minimum interval; a
non-invitee cannot accept or decline; accept is repeatable by different invitees; accept fails
after `expiresAt`; cancel archives; sign requires membership in `signers` and fails after
`deadline`; `DecmanNode` rejects a floor above the interval; manifest fields validate.

## 4. Rust module `crates/decman/src/onledger/`

| File | Responsibility |
|---|---|
| `mod.rs` | Module doc. `OnLedger` facade (config, db, auth, package resolver). `spawn_observer`. |
| `identity.rs` | `NodeIdentity { node_party, participant_id, token_manager }` from `party_credentials` kind `node`. `require_node_identity`. `verify_hosting(party, participant) -> Submission/Confirmation/None` via `ListPartyToParticipant`. |
| `daml/mod.rs`, `daml/templates.rs`, `daml/codec.rs`, `daml/client.rs` | Template ids via `PackageResolver` key `decman_coordination` (`#decman-coordination-v1`). Record encode/decode for every template (unit-tested round trips). Create/exercise through `CommandService.submit_and_wait` as the node party. ACS reads through `for_each_active_created` / `ledger_paging` filtered by template, as the node party. |
| `registry.rs` | Publish/update/heartbeat `DecmanNode` (vetted observers only). Read peers' entries keyed by signatory. `PeerHealthSnapshot` in `AppState`. `preflight_unready_peers` (vetting + entry + version). `fetch_vetted_packages_for(participant)`. |
| `proposals.rs` | Create/accept/decline/cancel/finish `WorkflowProposal`. Read proposals, counted acceptances (D6 predicates), declines, outcomes. Project into `pending_invitations` and `workflow_runs`. Archive sweep. |
| `topology.rs` | `proposals_query` (`Snapshot(MaxValue)`, `proposals = true`, `ADD_REPLACE`). `list_pending_dnd(namespace)`, `list_pending_p2p(party)`. `propose_mapping(serial)`. `transaction_hash_of`. `cosign_by_hash`. `wait_effective`. `wait_owner_root_delegations`. `read_root_delegation_key(fp)`. Mapping builders (owners sorted, participants sorted by uid, hosts as `(uid, permission, onboarding)` tuples). |
| `observer.rs` | The polling loop with the per-run `try_lock`. Each tick: refresh registry snapshot; project proposals; drive member runs and proposer runs; heartbeat when due; unsolicited scan every 60 s; archive sweep; metrics. |
| `validation.rs` | `Expectations` from the accepted `WorkflowProposal`, counted acceptances, head state, local identity. Checks per kind (section 5). |
| `submission.rs` | Contracts: prepare, open rounds, verify and count signatures, execute, close. Member: verify, sign, publish. |
| `dars.rs` | Pins, upload verification with `expected_main_package_id`, vetting observation, completion. Startup coordination-DAR upload task. |
| `acs.rs` | Offset capture on co-sign, export spool + manifest, import endpoint logic, manifest verification, empty fast path, flag clearing, spool cleanup. |
| `engine/{onboarding,add_party,kick,change_threshold,contracts,dars}.rs` | Per-kind step machines (coordinator and member sides) as functions over on-ledger state and local artefacts (section 6). |

Removed: `crates/decman/src/noise/` (all), `server/peer_status.rs`, `server/health.rs`,
`server/handlers/keys.rs`, `workflow/{onboarding,add_party,kick,change_threshold,contracts,dars}/{coordinator,peer}.rs`
and their `steps/proposals/{create,sign,submit}.rs`, `workflow::start_peer`, `WorkflowState`
peer-connection gates, `WorkflowStep::to_command`, `WorkflowRegistry`, `WorkflowGuard`,
`HttpWorkflowState.abort_handle`, `topology::{sign_dns_p2p_proposals, aggregate_dns_p2p_signatures,
dedupe_signatures, submit_dns_then_p2p, DnsP2pArtifactKinds}`, `pipe.rs::{encode_block, decode_data,
decode_end}` (keep `ExportSession`), all `*InvitePayload` DTOs, `MissingPeerEdge`,
`OnboardingMeshErrorResponse`, `KeyStatusResponse`, Cargo deps `hyper-noise`, `tokio-noise`
(and its patch), `secp256k1`, dev-dep `static_assertions` (`hyper` and `http` stay: other
code uses them), the `Timeouts` and `NoiseRetryConfig` config structs,
`NodeInfo.{listen_address,port,public_address}`, CLI flags `--listen-address --noise-port
--public-address --timeout-* --noise-retry-*`, `data/noise.key` handling, `MIN_PEER_VERSION`,
`WIRE_VERSION`, `DECPM_PEER_WAIT_POLL_DELAY_MS`, `DECPM_ACS_BLOCK_BYTES`,
`integration-tests/smoke-noise-errors.sh`, the `deny.toml` rationale that names `hyper-noise`.

Kept and reused: `workflow/topology.rs` retry helpers and readers, `workflow/signing_keys.rs`
(on-chain-first lookup), `workflow/party_replication/*` (export session, import bracket,
offsets, flag clearing), `canton_hash`, `signing/*`, `workflow/external_party/*` (unchanged),
`workflow/storage.rs`, `workflow/state.rs` reduced to the run projection,
`server/reward_automation.rs` loop shape as the observer template.

## 5. Validation before a member co-signs

Inputs: the accepted `WorkflowProposal` (active, not expired), its counted acceptances (D6),
head state, local identity.

Common: `BaseResult.operation == ADD_REPLACE`; the proposer's owner fingerprint ∈
`signed_by_fingerprints`; this node's fingerprint ∉ `signed_by_fingerprints`; `serial ==
accepted + 1` and `accepted == base serial` (or `1`); DND requires own fingerprint ∈
`owners`; P2P requires own participant ∈ hosts at Confirmation (or as the Onboarding joiner).
Hosts compare as full `(participant_uid, permission, onboarding)` tuples; keys compare as full
`SigningPublicKey` byte sets.

Onboarding DND: `owners == {proposerNamespaceFingerprint} ∪ {a.namespaceFingerprint | counted
a}`; `|owners| == |participants|`; `threshold == proposal.threshold`; `namespace ==
computeNamespace(owners)`.
Onboarding P2P: `party == prefix::namespace`; `hosts == {(proposerParticipant, Confirmation,
None)} ∪ {(a.participantId, Confirmation, None)}`; `proposerParticipant` hosts `proposer`;
keys == the root-NSD `target_key` of every owner; both thresholds == `proposal.threshold`.

Add-party DND: `owners == head owners ∪ {joiner.namespaceFingerprint}`; `threshold ==
proposal.threshold`. Add-party P2P: `hosts == head hosts ∪ {(joiner, Confirmation, Onboarding)}`
with every head host unchanged; `keys == head keys ∪ {joiner key from its root NSD}`;
thresholds == `proposal.threshold`.

Kick DND: `owners == head owners − {kicked fingerprint}`; `|owners| == |head owners| − 1`;
`threshold == proposal.threshold`. The kicked fingerprint comes from this node's local
`dec_party_participant.owner_key` for `kickedParticipant`, recorded from that node's own
acceptance when it joined, never from the proposer. Kick P2P: `hosts == head hosts −
{kicked}` with every survivor unchanged; `|keys| == |head keys| − 1`; the removed key is not
this node's key and is not claimed by any survivor; thresholds == `proposal.threshold`.
Legacy branch (party_signing_keys fingerprints ≠ DND owners): the proposer uses
`known_signing_keys_by_member` plus elimination; members check "exactly one key removed, not
mine, not claimed by a survivor" and refuse otherwise with the existing message.

Change threshold: DND equal to head except `threshold == proposal.threshold`; P2P equal to
head except `threshold` and `party_signing_keys.threshold` both == `proposal.threshold`.

Contracts round and DAR upload: as in D7 and D8.

Any mismatch: do not sign; mark the peer run `Failed` with the reason; show it in the UI.

## 6. Steps per kind (UI projection)

Names are the `current_step` values; `step_total` is the list length. The frontend
`WORKFLOW_STEPS` becomes `Record<WorkflowKind, { Coordinator; Member; Joiner? }>` and
`stepsForRun` selects by `run.role` and `run.member_variant`. `WaitingForPeers` is renamed
`WaitingForAcceptances` everywhere, including `WAITING_FOR_PEERS_STEP`.

| Kind | Coordinator | Member |
|---|---|---|
| Onboarding | `GenerateKeys`, `WaitingForAcceptances`, `ProposeNamespace`, `AwaitNamespace`, `ProposeParty`, `AwaitParty`, `Complete` | `GenerateKeys`, `CoSignNamespace`, `CoSignParty`, `Complete` |
| AddParty | `GenerateKeys`, `WaitingForAcceptances`, `ProposeChanges`, `AwaitChanges`, `AwaitReplication`, `Complete` | Joiner: `GenerateKeys`, `CoSignChanges`, `SyncAcs`, `ClearOnboarding`, `Complete`. Member: `CoSignChanges` (captures the export offset first), `PublishManifest`, `Complete` |
| Kick | `WaitingForAcceptances`, `ProposeChanges`, `AwaitChanges`, `Complete` | `CoSignChanges`, `Complete` |
| ChangeThreshold | `WaitingForAcceptances`, `ProposeChanges`, `AwaitChanges`, `Complete` | `CoSignChanges`, `Complete` |
| Contracts | `WaitingForAcceptances`, `AwaitDars`, `PrepareSubmissions`, `CollectSignatures`, `ExecuteSubmissions`, `Complete` | `UploadDars`, `SignSubmissions`, `Complete` |
| Dars | `WaitingForAcceptances`, `AwaitVetting`, `Complete` | `UploadDars`, `Complete` |

Quorum: onboarding, add-party, and dars need every invitee. Kick and change-threshold need
`max(previousThreshold, proposal.threshold)` owner signatures per mapping, counting the
proposer; the coordinator leaves `WaitingForAcceptances` when counted acceptances `>=
max(previousThreshold, proposal.threshold) − 1`, and later acceptances still co-sign.
`previousThreshold` is the head DND threshold read at proposal creation. Preflight 409: refuse
a kick when `previousThreshold > |owners| − 1`; refuse any `proposal.threshold > |resulting
owners|`. Contracts needs `party_signing_keys.threshold` verified signatures.

## 7. HTTP surface

Unchanged paths and shapes: all `/governance/*`, `/auth/*`, `/party-config*`, `/v0/tenant/*`,
token-standard routes, `/workflows*`, `/onboarding*`, `/kick*`, `/add-party*`,
`/change-threshold*`, `/contracts*`, `/dars/distribute*`, `/dars/cancel`, `/invitations*`,
`/decentralized-parties`, `/packages/vetted`, `/node-health`, `/healthz`, `/metrics`.

Changed DTOs:

* `Peer { participant_id, name, party: Option<CantonId> }` (`GET/POST /network-config`).
* `NodeInfo { participant_id }`; `NodeConfig` drops `timeouts`, `noise_retry` (`GET /node-config`).
* `ConnectionStatus { CurrentNode, Active, Stale, Unknown, Unvetted }`.
* `ParticipantStatus { id, status, node_party?, last_seen_at?, heartbeat_age_secs?, version?, build_version? }` (`workflow` and `latency_ms` removed).
* `PendingInvitation`: `id = proposal_cid`; `coordinator_pubkey` → `coordinator_participant`; add `coordinator_party`, `proposal_cid`, `expires_at`.
* `WorkflowRun`: `coordinator_pubkey` → `coordinator_participant`; add `coordinator_party`, `proposal_cid`, `member_variant`; `connected_peers` keeps its name and means "invitees that accepted".
* `PeerPackageResult { participant_id, name, reachable, error_kind?, packages }` stays; `PeerErrorKind { TopologyReadFailed, NoVettedPackages, Other }`.
* `KnownMember` unchanged; populated from `WorkflowAcceptance.memberParty` and `party_credentials`.
* `POST /dars/upload` accepts optional `pin_instance`.
* `OnboardingRequest.peer_ids` keeps its name.

New: `GET /node-identity`, `PUT /node-identity`, `GET /registry`, `GET /acs-export/{party}/{target}`,
`POST /acs-import/{party}`, `GET /acs-manifests/{party}`, `GET /proposals/unsolicited`.

Removed: `GET /keys/status`; the 422 mesh error on `POST /onboarding`.

Consumers that change: frontend (`NetworkConfigAccordion`, `App.tsx`, `OnboardingDialog`,
`NotificationsView`, `StatusDot`, `NodeHealthCard`, `AddPartyDialog`, `DarsDialog`,
`PackagesPanel`, `KickDialog` copy, `workflowSteps.ts` + test, mocks), `decman-cli` (`api.rs`,
`app.rs` `peers_to_json`/`validate_peer_form`, `ui.rs` status/peers table/node info),
`bin/gen_types.rs`, `integration-tests/*.sh`, `crates/decman/tests/common/*`, e2e fixtures
and the governance spec (documented as follow-up).

## 8. Database migrations `000020_node_identity` and `000021_noise_sunset`

`000020_node_identity` (shipped with the on-ledger foundation; compatible with the Noise code):
1. `ALTER TABLE party_credentials ADD COLUMN kind TEXT NOT NULL DEFAULT 'decparty';`
8. `CREATE TABLE proposal_decisions (proposal_cid TEXT PRIMARY KEY NOT NULL, decision TEXT NOT NULL, decided_at INTEGER NOT NULL, pinned_hashes_json TEXT);`

`000021_noise_sunset` (shipped with the Noise removal), in this order:
2. `UPDATE workflow_runs SET status = 'failed', error = 'Interrupted by the Noise-sunset upgrade. Dismiss this card and start the operation again.', updated_at = strftime('%s','now') WHERE status = 'inprogress';`
3. `UPDATE workflow_runs SET coordinator_pubkey = (SELECT participant_id FROM peers WHERE peers.public_key = workflow_runs.coordinator_pubkey) WHERE coordinator_pubkey IS NOT NULL AND EXISTS (SELECT 1 FROM peers WHERE peers.public_key = workflow_runs.coordinator_pubkey);`
4. `ALTER TABLE workflow_runs RENAME COLUMN coordinator_pubkey TO coordinator_participant;`
5. `ALTER TABLE workflow_runs ADD COLUMN coordinator_party TEXT; ALTER TABLE workflow_runs ADD COLUMN proposal_cid TEXT; ALTER TABLE workflow_runs ADD COLUMN topology_hashes_json TEXT; ALTER TABLE workflow_runs ADD COLUMN member_variant TEXT;`
6. `DELETE FROM pending_invitations; ALTER TABLE pending_invitations RENAME COLUMN coordinator_pubkey TO coordinator_participant; ALTER TABLE pending_invitations ADD COLUMN coordinator_party TEXT; ALTER TABLE pending_invitations ADD COLUMN proposal_cid TEXT; ALTER TABLE pending_invitations ADD COLUMN expires_at INTEGER;`
7. `ALTER TABLE peers DROP COLUMN address; ALTER TABLE peers DROP COLUMN port; ALTER TABLE peers DROP COLUMN public_key;`

Down re-adds the three peer columns with defaults, drops the new columns and table, and
renames `coordinator_participant` back. It is applied manually (D12).

## 9. Configuration

Removed env: `DECPM_LISTEN_ADDRESS`, `DECPM_NOISE_PORT`, `DECPM_PUBLIC_ADDRESS`,
`DECPM_TIMEOUT_*`, `DECPM_NOISE_RETRY_*`, `DECPM_PEER_WAIT_POLL_DELAY_MS`, `DECPM_ACS_BLOCK_BYTES`.

Added env: `DECPM_HEARTBEAT_INTERVAL_SECS` (3600), `DECPM_HEARTBEAT_MIN_INTERVAL_SECS` (60,
clamped to the interval), `DECPM_PEER_STALE_FACTOR` (3), `DECPM_OBSERVER_POLL_SECS` (3),
`DECPM_AUTO_UPLOAD_COORDINATION_DAR` (true), `DECPM_ACS_SPOOL_DIR` (`{dir}/data/acs`),
`DECPM_PROPOSAL_TTL_SECS` (604800).

The coordination package is node-level, not per party: `consts::COORDINATION_PACKAGE_REF =
"#decman-coordination-v1"`, overridable with `DECPM_COORDINATION_PACKAGE_REF`.

## 10. Tests

Unit (cargo): codec round trips for every template; mapping builders are byte-stable; serial
and base-serial guards; acceptance counting predicates (foreign acceptance rejected,
duplicate acceptor rejected); validation per kind (positive and each negative, including a
REMOVE proposal and a host permission flip); quorum arithmetic (4 owners, previous 3, new 2,
kick one → wait for 3 signatures); signature verification before counting; registry
staleness and signatory keying; hash co-sign idempotency (mocked client); migration 000020 on a
seeded DB; DTO snapshots.

Daml (`dpm test`): section 3.

Integration harness (`integration-tests/*.sh`): drop Noise ports and key reads; readiness =
`/node-config` + `/healthz`; wait until `/packages/vetted` lists the coordination package on all
nodes; allocate a node party per node on the JSON Ledger API, grant `ledger-api-user`
CanActAs/CanReadAs, `PUT /node-identity`; configure peers with `party`; wait for each node's
`DecmanNode` to be visible on its peers; set `DECPM_HEARTBEAT_INTERVAL_SECS=5`,
`DECPM_HEARTBEAT_MIN_INTERVAL_SECS=1`, `DECPM_OBSERVER_POLL_SECS=1`. Phases: keep the
ledger-only ones; rewrite the coordination ones to the new steps; delete `peer_health_flip`
(replaced by a staleness flip), `peer_3_strikes_abort`, `retry_with_offline_peer`,
`invite_cap`, `check_peer_dars` (replaced by a vetted-packages comparison). Playwright
`bring-up.sh`, `data-status="unreachable"`, and the kick owner-key wait are follow-up work.

KMS spike (manual, before the mainnet window): D4.

## 11. Documentation

README, ARCHITECTURE, DEPLOYMENT_GUIDE, USER_GUIDE, CUSTOM_DAML_TEMPLATES, KMS_SIGNING (new
key name, fingerprint-based discovery, the dual-usage trust implication), CONTRIBUTING: remove
every Noise reference; document the node identity, the registry, the proposal model, the DAR
pin flow and startup auto-vetting policy, the ACS handoff and spool sizing, the cutover runbook
and manual rollback, the residual legacy kick-attribution gap, gap #423 (package check) status,
and the new env vars. Kubernetes manifests lose containerPort 9000, the ClusterIP `noise`
port, the `dec-party-manager-noise` LoadBalancer, and the removed CLI args (clap rejects
unknown args). `Dockerfile` exposes 8080 only.

## 12. Identifiers

* `pin_instance` = `WorkflowProposal.runId`.
* `pending_invitations.id` = `proposal_cid`.
* Peer `instance_name` = `peer-{kind}-{coordinator_participant_short}-{coordinator_runId}`.
* Coordinator `instance_name` unchanged (`{prefix}-creation`, `{party}-kick-{n}`, ...).

## 13. Evidence summary

* Partial-authorization proposals propagate and merge by fingerprint; co-sign by hash; no
  expiry, no cancel; `Snapshot(MaxValue)` for discovery. (Canton `TopologyManager.scala`,
  `TopologyStateProcessor.scala`, `GrpcTopologyManagerReadService.scala`; Splice
  `SvOnboardingPartyToParticipantProposalTrigger.scala`.)
* DND serial 1 needs every owner's signature and every owner's root `NamespaceDelegation` in
  the synchronizer store first; later DND/P2P changes are authorized by the **stored** DND
  threshold; a P2P under a decentralized namespace needs the DND effective first.
  (`TopologyMappingChecks.scala`, `TransactionAuthorizationCache.scala`, `AuthorizationGraph.scala`.)
* A P2P REMOVE with unchanged content falls back to party-namespace authorization
  (`TopologyMapping.scala` requiredAuth); members must pin `ADD_REPLACE`.
* A root `NamespaceDelegation` is self-authorizing and carries the full public key;
  `[Namespace, Protocol]` is a valid usage set, also on KMS. (`Signing.scala`, `KmsPrivateCrypto.scala`.)
* A locally hosted submitter needs Submission permission (`AdmissibleSynchronizersComputation.scala`).
* Clearing an Onboarding flag needs only the joiner's participant namespace.
* Interactive submission: `preparationTimeRecordTimeTolerance` (24 h Splice default) and the
  signed `max_record_time` bound the window; all signatures in one execute call.
* Online party replication is alpha with `ProtocolVersion.dev` codecs; unusable on PV 35.
* Daml visibility is stakeholder-only; a create fails when an observer's participant has not
  vetted the package (`UsableSynchronizers.scala`).
