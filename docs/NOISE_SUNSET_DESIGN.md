# Noise sunset: Canton-native coordination

**Status:** implementation spec, 2026-09-14.
**Scope:** remove the Noise P2P transport from decman. Nodes coordinate only through
Canton: the synchronizer topology store, Daml contracts, and the Ledger API.

This document is the shared contract for the implementation. Every section states a
decision. The research that grounds each decision is summarized in section 12.

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
the node's participant with Confirmation permission. It has a Ledger API user with
`CanActAs` and `CanReadAs` on itself. It exists before any decentralized party exists.

Storage: a `party_credentials` row with `kind = 'node'` and `dec_party_id = <node party>`.
`AuthRegistry::get(node_party)` returns its token manager unchanged. Migration `000020`
adds `kind TEXT NOT NULL DEFAULT 'decparty'`. Decparty-oriented views filter `kind = 'decparty'`.

HTTP: `GET /node-identity` and `PUT /node-identity` (admin; the PUT is bootstrap-exempt while no
node identity exists, using the existing bootstrap mutex). Body = `PartyConfigRequest` shape
without `dec_party_id` (member party id becomes the node party id). The existing
`POST /party-config/discover-member-party` pre-fills it.

The node party signs: the registry entry, workflow proposals, acceptances, declines,
submission rounds, signatures, ACS manifests, and heartbeats.

### D2. Discovery: one string out of band

Operators exchange one string per peer: `participant_id,node_party_id,name`. The UI button
"Share my identity" copies it. "Add peer" pastes it. The `peers` table keeps
`(participant_id PK, name, party)` and drops `address`, `port`, `public_key`.

Before a node writes any contract that names a peer node party as observer, it verifies
through the tokenless Admin API that the party is hosted on the claimed participant
(`ListPartyToParticipant(filter_party = node_party)` head state contains `participant_id`
with Confirmation or Submission permission). This is the only trust check; the security
boundary stays the operator's own co-signature.

### D3. Registry: `DecmanNode`

Every node publishes one `DecmanNode` contract (signatory = node party, observers = the peer
node parties from its `peers` table). Fields: `participantId`, `displayName`, `version`,
`buildVersion`, `coordinationVersion`, `peers`, `lastActiveAt`, `heartbeatIntervalSecs`,
`minHeartbeatIntervalSecs`. Choices: `DecmanNode_Heartbeat` (re-create with `lastActiveAt = now`,
guarded by the minimum interval), `DecmanNode_Update` (peers, names, versions), `DecmanNode_Retire`.

The registry entry is (re)published at startup and whenever the peers table changes.
Heartbeat cadence is `DECPM_HEARTBEAT_INTERVAL_SECS` (default 3600). A peer is `Stale` when
`now - lastActiveAt > 3 × heartbeatIntervalSecs`. Tests set the interval to 5 s.

Peer health: `/participants-status` is served from an in-memory snapshot that the observer
loop refreshes. `ConnectionStatus` becomes `CurrentNode | Active | Stale | Unknown`.
`latency_ms` is removed. New fields: `last_seen_at`, `heartbeat_age_secs`, `node_party`.
`version` and `build_version` come from the registry entry.

Version gate: a proposer refuses to start a workflow (HTTP 409, naming the participants)
unless every invitee has a registry entry with `coordinationVersion >= COORDINATION_VERSION`.
`MIN_PEER_VERSION` and `WIRE_VERSION` are deleted. `COORDINATION_VERSION = 1`.
The decman crate version becomes `2.0.0`.

### D4. Key model: one dual-usage key per member per decentralized party

For every NEW decentralized party each member generates ONE vault key named
`{prefix}-key` with usages `[Namespace, Protocol]`. Its self-signed root `NamespaceDelegation`
puts the full `SigningPublicKey` into the synchronizer store. The proposer reads it with
`ListNamespaceDelegation(store = Synchronizer, filter_namespace = fp, filter_target_key_fingerprint = fp)`
and copies `target_key` into `PartyToParticipant.party_signing_keys`. The DND owner
fingerprint equals the party signing-key fingerprint, so kick attribution is on-chain.

Legacy parties (two keys `{prefix}-namespace` + `{prefix}-daml-transactions`) keep working
unchanged: kick, add-party, change-threshold sign with `signed_by = []` and Canton
auto-selects the legacy namespace key. Own-Daml-key lookup is on-chain first
(`party_signing_keys ∩ ListMyKeys`), then `dec_party_identity`, then the legacy vault name.

Key generation is idempotent by name (existing `get_or_create_signing_key`).

### D5. Topology coordination: proposals in the synchronizer store

Proposer:
1. Read the accepted mapping (`proposals = false`, `Snapshot(MaxValue)`) to get serial `S`.
2. `Authorize { proposal { ADD_REPLACE, serial = S + 1 (or 1), mapping }, must_fully_authorize = false, signed_by = [], store = Synchronizer(physical_id) }`.
3. Never pass `FORCE_FLAG_ALLOW_UNVALIDATED_SIGNING_KEYS`. Never use the Authorized store.
4. Keep the returned `transaction_hash` on the run row.

Members (observer loop, every `DECPM_OBSERVER_POLL_SECS`, default 3):
1. `ListDecentralizedNamespaceDefinition` and `ListPartyToParticipant` with
   `BaseQuery { store = Synchronizer, proposals = true, time_query = Snapshot(MaxValue) }`.
2. Keep proposals that name this node (owner fingerprint in `owners`, or participant in
   `participants`) and that this node has not signed (`signed_by_fingerprints`).
3. Match each proposal to an accepted `WorkflowProposal` (D6). Unmatched proposals are
   listed in the UI as "unsolicited"; they are never signed automatically.
4. Validate against the accepted proposal and head state (section 5).
5. Guard: `proposal.serial == accepted.serial + 1` (or `1` with no accepted mapping).
6. Co-sign: `Authorize { transaction_hash = hex(BaseResult.transaction_hash), must_fully_authorize = false, signed_by = [], store = Synchronizer }`.
   Retry `TOPOLOGY_TRANSACTION_NOT_FOUND` on the next poll. Treat
   `TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE` with the existing retry budget.
7. Completion: poll `proposals = false` until `serial == S + 1` and `valid_from <= now`.

Order for onboarding: DND first. Wait until the DND is effective in the synchronizer store
on this node. Then P2P. Before proposing a DND, wait until every owner's root
`NamespaceDelegation` is effective in the synchronizer store.

Cancel: a topology proposal cannot be withdrawn. Cancel archives the `WorkflowProposal`;
members stop co-signing because the match disappears. A stale proposal without the members'
signatures can never become effective.

The clearing of a joiner's Onboarding flag needs only the joiner's participant signature.
The joiner calls `ClearPartyOnboardingFlag` and polls until `onboarded == true`. No
co-sign round exists for it.

### D6. Workflow intent: `WorkflowProposal`

Every run starts with a `WorkflowProposal` (signatory = proposer node party, observers =
invitee node parties). It carries the human-readable intent the invitation card shows today
and the reference set the member validates against. Invitees `Accept` (non-consuming, creates
`WorkflowAcceptance`) or `Decline` (creates `WorkflowDecline`). The proposer `Cancel`s or
`Finish`es it. Every `WorkflowAcceptance` carries the acceptor's `participantId` and, for
onboarding and add-party, its fresh key material: `namespaceFingerprint` and
`signingPublicKeyHex`. Acceptances are visible to the proposer and to all invitees, so every
member can verify the full owner set before co-signing.

Consent model: the operator accepts once. After that the node co-signs every topology
proposal that matches the accepted `WorkflowProposal` and passes validation. This is the same
model as today, with a stronger reference set.

### D7. Contracts workflow: `SubmissionRound`

The proposer runs `PrepareSubmission` once per contract definition with
`max_record_time = preparation_time + 20h` and creates a `SubmissionRound` per prepared
transaction (bytes as lowercase hex, hash, hashing scheme version, deadline). Members that
accepted the `WorkflowProposal` re-hash the bytes with `canton_hash`, check `act_as`, sign
with their per-decparty key (vault export or KMS), and exercise `SubmissionRound_Sign`. When
the proposer sees `>= party_signing_keys.threshold` distinct fingerprints it executes one
`ExecuteSubmissionAndWait` with all signatures and closes the round. Deadline is 24 h minus a
margin. Expired rounds are re-prepared and re-signed.

### D8. DARs: hash-pinned local upload

`POST /dars/distribute` creates a `WorkflowProposal(kind = Dars)` with `DarPin { filename,
sha256Hex, mainPackageId, sizeBytes }` per file. The proposer uploads locally. Each invitee's
UI shows the pins; the operator uploads the same files through `POST /dars/upload`, which
refuses a file whose sha256 does not match a pending pin when a `pin` id is supplied. The
proposer observes every participant's `VettedPackages` in the synchronizer store
(`ListVettedPackages(filter_participant)`) and completes the run when all pins are vetted
everywhere. `/packages/compare-peers` reads vetted packages per participant from the
topology store. No DAR bytes travel over any decman channel.

The coordination DAR (`releases/v1/decman-coordination-v1-0.1.0.dar`) is embedded with
`include_bytes!` and uploaded and vetted at startup (`DECPM_AUTO_UPLOAD_COORDINATION_DAR`,
default true). `POST /dars/upload` remains the manual fallback.

### D9. ACS synchronization: offline replication, operator-moved file

Canton online party replication is alpha and unusable on protocol version 35. Offline
replication stays. When a current host observes the P2P that marks the joiner `Onboarding`
become effective, it exports the snapshot to a spool file and publishes an `AcsManifest`
(party, target participant, activation serial, exporter participant, size, sha256,
package ids). If `sizeBytes == 0` the joiner skips the import and clears its flag. Otherwise:

* `GET /acs-export/{party}/{target}?serial=N` (admin JWT, streamed gzip) on any current host.
* `POST /acs-import/{party}?serial=N` (admin JWT, streamed body) on the joiner. The joiner
  verifies: head state marks itself Onboarding at that serial; a manifest from a
  non-onboarding host exists; sha256 and size match that manifest; every manifest package id
  is vetted locally. Then it runs the existing disconnect / `ImportPartyAcs` / reconnect
  bracket and clears the flag.

The add-party run on the joiner waits in step `SyncAcs` until the import completes or the
empty fast path fires.

### D10. Cancel, decline, retry

* Cancel (proposer): exercise `WorkflowProposal_Cancel`, mark the run `Cancelled`. Members
  see the proposal vanish and mark their peer rows `Cancelled`.
* Decline (invitee): exercise `WorkflowProposal_Decline`. The proposer marks the run `Failed`
  ("Peer X declined the invitation"). Sibling invitees mark their rows `Cancelled`.
* Retry: idempotent "ensure". The proposer re-reads the accepted serial and re-proposes only
  if its proposal is gone. Members re-issue `Authorize { transaction_hash }`. No broadcast.
* Dismiss: unchanged.

### D11. Projection tables

`workflow_runs` stays as the UI projection with the same status vocabulary. New columns:
`proposal_cid`, `coordinator_participant` (renamed from `coordinator_pubkey`),
`coordinator_party`, `topology_hashes_json`. `pending_invitations` stays as the local cache
of unaccepted `WorkflowProposal`s that name this node: `coordinator_pubkey` becomes
`coordinator_participant`, plus `proposal_cid`, `coordinator_party`. `workflow_artifacts`
stays for local-only artefacts (keys, offsets, spool file paths).

### D12. Rollout

Old builds refuse to start a workflow when any invitee's Noise listener is gone. New builds
refuse until every invitee has a registry entry. So a mixed window is safe. Migration 000020
fail-marks every `inprogress` run with an explanatory error. Runbook: drain, upgrade each
node inside the window, configure the node identity, verify registry entries, then start
workflows.

## 3. Daml package `decman-coordination-v1`

Location: `daml/decman-coordination/` (`name: decman-coordination-v1`, `version: 0.1.0`,
`sdk-version: 3.4.11`, `--target=2.2`, dependencies: `daml-prim`, `daml-stdlib` only).
Test package: `daml/decman-coordination-test/` with `daml-script` and `../dars/testlib-0.1.0.dar`.
Both go into `daml/multi-package.yaml`. The built DAR is committed to
`releases/v1/decman-coordination-v1-0.1.0.dar`. CI gets a `dpm test` step for the test package.

Text encoding: fingerprints and hashes are Canton hex strings (`1220...`). Public keys are the
lowercase hex of the serialized protobuf `SigningPublicKey`. Signatures are lowercase hex of
the raw signature bytes. Every hex field is validated with `DA.Crypto.Text.isHex`
(build option `-Wno-crypto-text-is-alpha`).

```daml
module Decman.Coordination.Types where
data WorkflowKind = Onboarding | AddParty | Kick | ChangeThreshold | Contracts | Dars
  deriving (Eq, Show)
data DarPin = DarPin with filename : Text; sha256Hex : Text; mainPackageId : Text; sizeBytes : Int
  deriving (Eq, Show)
```

```daml
module Decman.Coordination.Node where
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
    ensure heartbeatIntervalSecs > 0 && minHeartbeatIntervalSecs >= 0 && coordinationVersion >= 1
    choice DecmanNode_Heartbeat : ContractId DecmanNode
      controller node
      do now <- getTime
         assertMsg "heartbeat too soon" (now >= addRelTime lastActiveAt (seconds minHeartbeatIntervalSecs))
         create this with lastActiveAt = now
    choice DecmanNode_Update : ContractId DecmanNode
      with newPeers : [Party]; newDisplayName : Text; newVersion : Text; newBuildVersion : Text
           newCoordinationVersion : Int; newHeartbeatIntervalSecs : Int; newMinHeartbeatIntervalSecs : Int
      controller node
      do now <- getTime
         create this with peers = newPeers; displayName = newDisplayName; version = newVersion
                          buildVersion = newBuildVersion; coordinationVersion = newCoordinationVersion
                          heartbeatIntervalSecs = newHeartbeatIntervalSecs
                          minHeartbeatIntervalSecs = newMinHeartbeatIntervalSecs; lastActiveAt = now
    choice DecmanNode_Retire : ()
      controller node
      do pure ()
```

```daml
module Decman.Coordination.Workflow where
template WorkflowProposal
  with
    proposer : Party
    proposerParticipant : Text
    instance : Text
    kind : WorkflowKind
    invitees : [Party]
    participants : [Text]
    decPartyId : Optional Text
    prefix : Optional Text
    threshold : Optional Int
    previousThreshold : Optional Int
    newParticipant : Optional Text
    kickedParticipant : Optional Text
    darPins : [DarPin]
    packageNames : [Text]
    description : Text
    createdAt : Time
  where
    signatory proposer
    observer invitees
    nonconsuming choice WorkflowProposal_Accept : ContractId WorkflowAcceptance
      with acceptor : Party; participantId : Text; namespaceFingerprint : Optional Text
           signingPublicKeyHex : Optional Text; memberParty : Optional Party
      controller acceptor
      do assertMsg "not invited" (acceptor `elem` invitees)
         now <- getTime
         create WorkflowAcceptance with proposal = self; proposer; acceptor; observers = invitees
           instance; participantId; namespaceFingerprint; signingPublicKeyHex; memberParty; acceptedAt = now
    nonconsuming choice WorkflowProposal_Decline : ContractId WorkflowDecline
      with decliner : Party; reason : Text
      controller decliner
      do assertMsg "not invited" (decliner `elem` invitees)
         now <- getTime
         create WorkflowDecline with proposal = self; proposer; decliner; observers = invitees; instance; reason; declinedAt = now
    choice WorkflowProposal_Cancel : ()
      controller proposer
      do pure ()
    choice WorkflowProposal_Finish : ContractId WorkflowOutcome
      with succeeded : Bool; error : Optional Text
      controller proposer
      do now <- getTime
         create WorkflowOutcome with proposer; instance; kind; observers = invitees; succeeded; error; finishedAt = now

template WorkflowAcceptance
  with proposal : ContractId WorkflowProposal; proposer : Party; acceptor : Party; observers : [Party]
       instance : Text; participantId : Text; namespaceFingerprint : Optional Text
       signingPublicKeyHex : Optional Text; memberParty : Optional Party; acceptedAt : Time
  where
    signatory acceptor
    observer proposer, observers
    choice WorkflowAcceptance_Archive : () controller acceptor do pure ()

template WorkflowDecline
  with proposal : ContractId WorkflowProposal; proposer : Party; decliner : Party; observers : [Party]
       instance : Text; reason : Text; declinedAt : Time
  where
    signatory decliner
    observer proposer, observers
    choice WorkflowDecline_Archive : () controller decliner do pure ()

template WorkflowOutcome
  with proposer : Party; instance : Text; kind : WorkflowKind; observers : [Party]
       succeeded : Bool; error : Optional Text; finishedAt : Time
  where
    signatory proposer
    observer observers
    choice WorkflowOutcome_Archive : () controller proposer do pure ()
```

```daml
module Decman.Coordination.Submission where
template SubmissionRound
  with
    proposer : Party
    instance : Text
    index : Int
    signers : [Party]
    decPartyId : Text
    actAs : Text
    description : Text
    preparedTransactionHex : Text
    preparedHashHex : Text
    hashingSchemeVersion : Int
    preparationTime : Time
    deadline : Time
  where
    signatory proposer
    observer signers
    nonconsuming choice SubmissionRound_Sign : ContractId SubmissionSignature
      with signer : Party; participantId : Text; signedBy : Text; signatureHex : Text; format : Text; algorithm : Text
      controller signer
      do assertMsg "not a signer" (signer `elem` signers)
         now <- getTime
         create SubmissionSignature with round = self; proposer; signer; observers = signers; instance; index
           participantId; signedBy; signatureHex; format; algorithm; signedAt = now
    choice SubmissionRound_Close : ()
      with result : Text
      controller proposer
      do pure ()

template SubmissionSignature
  with round : ContractId SubmissionRound; proposer : Party; signer : Party; observers : [Party]
       instance : Text; index : Int; participantId : Text; signedBy : Text; signatureHex : Text
       format : Text; algorithm : Text; signedAt : Time
  where
    signatory signer
    observer proposer, observers
    choice SubmissionSignature_Archive : () controller signer do pure ()
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
    choice AcsManifest_Archive : () controller exporter do pure ()
```

Daml tests (given/when/then, `dpm test`): heartbeat honours the minimum interval; a
non-invitee cannot accept or decline; accept is repeatable by different invitees; cancel
archives; sign requires membership in `signers`; manifest fields validate.

## 4. Rust module `crates/decman/src/onledger/`

| File | Responsibility |
|---|---|
| `mod.rs` | Module doc. `OnLedger` facade (config, db, auth, package resolver). `spawn_observer`. |
| `identity.rs` | `NodeIdentity { node_party, participant_id, token_manager }` loaded from `party_credentials` kind `node`. `require_node_identity` error when absent. Hosting verification of a peer node party via `ListPartyToParticipant`. |
| `daml/mod.rs`, `daml/templates.rs`, `daml/codec.rs`, `daml/client.rs` | Template ids via `PackageResolver` key `decman_coordination` (`#decman-coordination-v1`). Record encode/decode for every template. Create/exercise through `CommandService.submit_and_wait` as the node party. ACS reads through the existing `for_each_active_created` / `ledger_paging` helpers filtered by template, as the node party. |
| `registry.rs` | Publish/update/heartbeat `DecmanNode`. Read peers' entries. `PeerHealthSnapshot` in `AppState`. Version gate `preflight_unregistered_peers`. Vetting readiness `fetch_vetted_packages_for(participant)`. |
| `proposals.rs` | Create/accept/decline/cancel/finish `WorkflowProposal`. Read pending proposals, acceptances, declines, outcomes. Project into `pending_invitations` and `workflow_runs`. |
| `topology.rs` | `proposals_query` (`Snapshot(MaxValue)`, `proposals = true`). `list_pending_dnd`, `list_pending_p2p`. `propose_mapping` with explicit serial. `cosign_by_hash`. `wait_effective`. `wait_owner_root_delegations`. Mapping builders for bootstrap, add owner/host, remove owner/host, change threshold (byte-stable order: owners sorted, participants sorted by uid). |
| `observer.rs` | The polling loop. Each tick: refresh registry snapshot; project proposals to invitations; for accepted runs drive the member side (generate keys, co-sign, sign rounds, upload check, ACS wait); for proposer runs drive the coordinator side (await acceptances, build and submit, await effective, next step, finish). Heartbeat when due. Metrics. |
| `validation.rs` | `Expectations` built from the accepted `WorkflowProposal` + acceptances + head state. Checks per kind (section 5). |
| `submission.rs` | Contracts: prepare, open rounds, collect, execute, close. Member: verify, sign, publish. |
| `dars.rs` | Pins, upload verification, vetting observation, completion. |
| `acs.rs` | Export spool + manifest publication; import endpoint logic; empty fast path; flag clearing. |
| `engine/{onboarding,add_party,kick,change_threshold,contracts,dars}.rs` | Per-kind step machines (coordinator and member sides) as pure functions over on-ledger state plus local artefacts. Steps in section 6. |

Removed: `crates/decman/src/noise/` (all), `server/peer_status.rs`, `server/health.rs`,
`server/handlers/keys.rs`, `workflow/{onboarding,add_party,kick,change_threshold,contracts,dars}/{coordinator,peer}.rs`,
`workflow::start_peer`, `WorkflowState` peer-connection gates, `WorkflowStep::to_command`,
all `*InvitePayload` DTOs, `MissingPeerEdge`/`OnboardingMeshErrorResponse`, `KeyStatusResponse`,
Cargo deps `hyper-noise`, `tokio-noise` (and its patch), `secp256k1`, `hyper`, `http` if unused,
the `Timeouts` and `NoiseRetryConfig` config structs, `NodeInfo.{listen_address,port,public_address}`,
CLI flags `--listen-address --noise-port --public-address --timeout-* --noise-retry-*`,
`data/noise.key` handling, `MIN_PEER_VERSION`, `WIRE_VERSION`, `integration-tests/smoke-noise-errors.sh`.

Kept and reused: `workflow/topology.rs` helpers (`sign_transactions_with_topology_retry`,
`authorize_with_topology_retry`, `synchronizer_store_id`, `head_state_query`, `fetch_*`),
`workflow/signing_keys.rs` (with on-chain-first lookup), `workflow/party_replication/*`
(export session, import bracket, offsets, flag clearing), `canton_hash`, `signing/*`,
`workflow/external_party/*` (unchanged), `workflow/storage.rs`, `workflow/state.rs` reduced
to the run projection, `server/reward_automation.rs` loop shape as the observer template.

## 5. Validation before a member co-signs

Built from: the accepted `WorkflowProposal`, all `WorkflowAcceptance`s for it, the head
state, and local identity.

Common: the proposal names this node (owner fingerprint or participant); this node has not
signed it; `serial == accepted + 1` (or `1`).

Onboarding DND: `owners` == set of acceptances' `namespaceFingerprint` ∪ proposer's own
fingerprint (the proposer publishes its key in its own acceptance record, created by itself);
`|owners| == |participants|`; `threshold == proposal.threshold`; `namespace == computeNamespace(owners)`.
Onboarding P2P: `party == prefix::namespace`; hosts == `participants` all Confirmation, no
Onboarding marker; `party_signing_keys.keys` fingerprints == owners; both thresholds ==
`proposal.threshold`.

Add-party DND: owners == head owners ∪ {joiner fingerprint from its acceptance}; threshold
== `proposal.threshold`. Add-party P2P: hosts == head hosts ∪ {joiner, Confirmation,
Onboarding}; keys == head keys ∪ {joiner key}; thresholds == `proposal.threshold`.

Kick DND: owners == head owners − {kicked owner}; threshold == `proposal.threshold`.
Kick P2P: hosts == head hosts − {kicked}; keys == head keys − {kicked key}; thresholds.

Change threshold: mapping equal to head except both thresholds == `proposal.threshold`.

Contracts round: recomputed hash == `preparedHashHex`; `act_as == [decPartyId]`;
`deadline > now`; the round belongs to the accepted proposal instance.

DAR upload: sha256 of the uploaded file == a pending pin for this node.

Any mismatch: do not sign; mark the peer run `Failed` with the reason; show it in the UI.

## 6. Steps per kind (UI projection)

Names are the `current_step` values. `step_total` is the list length. The frontend
`workflowSteps.ts` mirrors these lists.

| Kind | Coordinator | Member |
|---|---|---|
| Onboarding | `WaitingForAcceptances`, `GenerateKeys`, `ProposeNamespace`, `AwaitNamespace`, `ProposeParty`, `AwaitParty`, `Complete` | `GenerateKeys`, `CoSignNamespace`, `CoSignParty`, `Complete` |
| AddParty | `WaitingForAcceptances`, `ExportState`, `ProposeChanges`, `AwaitChanges`, `AwaitReplication`, `Complete` | joiner: `GenerateKeys`, `CoSignChanges`, `SyncAcs`, `ClearOnboarding`, `Complete`; others: `CoSignChanges`, `PublishManifest`, `Complete` |
| Kick | `WaitingForAcceptances`, `ExportState`, `ProposeChanges`, `AwaitChanges`, `Complete` | `CoSignChanges`, `Complete` |
| ChangeThreshold | `WaitingForAcceptances`, `ExportState`, `ProposeChanges`, `AwaitChanges`, `Complete` | `CoSignChanges`, `Complete` |
| Contracts | `WaitingForAcceptances`, `AwaitDars`, `PrepareSubmissions`, `CollectSignatures`, `ExecuteSubmissions`, `Complete` | `UploadDars`, `SignSubmissions`, `Complete` |
| Dars | `WaitingForAcceptances`, `AwaitVetting`, `Complete` | `UploadDars`, `Complete` |

Kick and change-threshold need `threshold` acceptances (quorum), not all. Contracts needs
`party_signing_keys.threshold` signatures. Onboarding and add-party need every invitee.

## 7. HTTP surface

Unchanged paths and shapes: all `/governance/*`, `/auth/*`, `/party-config*`, `/v0/tenant/*`,
token-standard routes, `/workflows*`, `/onboarding*`, `/kick*`, `/add-party*`,
`/change-threshold*`, `/contracts*`, `/dars/*`, `/invitations*`, `/decentralized-parties`,
`/packages/*`, `/network-config`, `/node-config`, `/node-health`, `/healthz`, `/metrics`.

Changed DTOs:

* `Peer { participant_id, name, party: Option<CantonId> }` (drop `address`, `port`, `public_key`).
* `NodeInfo { participant_id }` (drop listener fields). `NodeConfig` drops `timeouts`, `noise_retry`.
* `ConnectionStatus { CurrentNode, Active, Stale, Unknown }`.
* `ParticipantStatus { id, status, node_party?, last_seen_at?, heartbeat_age_secs?, workflow?, version?, build_version? }`.
* `PendingInvitation`: `coordinator_pubkey` → `coordinator_participant`; add `coordinator_party`, `proposal_cid`.
* `WorkflowRun`: `coordinator_pubkey` → `coordinator_participant`; add `coordinator_party`, `proposal_cid`; `connected_peers` keeps its name and means "invitees that accepted".
* `PeerPackageResult { participant_id, name, reachable, error_kind?, packages }` stays; `reachable` means "topology read succeeded"; `PeerErrorKind { TopologyReadFailed, NoVettedPackages, Other }`.
* `KnownMember` unchanged; populated from `WorkflowAcceptance.memberParty` records and `party_credentials`.

New:

* `GET /node-identity`, `PUT /node-identity`.
* `GET /registry` → `{ self: DecmanNodeView, peers: [DecmanNodeView], inbound: [DecmanNodeView] }`.
* `GET /acs-export/{party}/{target}?serial=` (admin, streamed), `POST /acs-import/{party}?serial=` (admin, streamed), `GET /acs-manifests/{party}`.
* `POST /dars/upload` accepts optional `pin_instance` to verify against pending pins.
* `GET /proposals/unsolicited` → topology proposals naming this node with no matching accepted `WorkflowProposal`.

Removed: `GET /keys/status`.

## 8. Database migration `000020_noise_sunset`

Up, in this order:
1. `ALTER TABLE party_credentials ADD COLUMN kind TEXT NOT NULL DEFAULT 'decparty';`
2. `UPDATE workflow_runs SET status = 'failed', error = 'Interrupted by the Noise-sunset upgrade. Dismiss this card and start the operation again.', updated_at = strftime('%s','now') WHERE status = 'inprogress';`
3. `UPDATE workflow_runs SET coordinator_pubkey = (SELECT participant_id FROM peers WHERE peers.public_key = workflow_runs.coordinator_pubkey) WHERE coordinator_pubkey IS NOT NULL AND EXISTS (SELECT 1 FROM peers WHERE peers.public_key = workflow_runs.coordinator_pubkey);`
4. `ALTER TABLE workflow_runs RENAME COLUMN coordinator_pubkey TO coordinator_participant;`
5. `ALTER TABLE workflow_runs ADD COLUMN coordinator_party TEXT; ALTER TABLE workflow_runs ADD COLUMN proposal_cid TEXT; ALTER TABLE workflow_runs ADD COLUMN topology_hashes_json TEXT;`
6. `DELETE FROM pending_invitations; ALTER TABLE pending_invitations RENAME COLUMN coordinator_pubkey TO coordinator_participant; ALTER TABLE pending_invitations ADD COLUMN coordinator_party TEXT; ALTER TABLE pending_invitations ADD COLUMN proposal_cid TEXT;`
7. `ALTER TABLE peers DROP COLUMN address; ALTER TABLE peers DROP COLUMN port; ALTER TABLE peers DROP COLUMN public_key;`
8. `CREATE TABLE proposal_decisions (proposal_cid TEXT PRIMARY KEY NOT NULL, decision TEXT NOT NULL, decided_at INTEGER NOT NULL);`

Down is documented as lossy (re-adds the three peer columns with defaults).

## 9. Configuration

Removed env: `DECPM_LISTEN_ADDRESS`, `DECPM_NOISE_PORT`, `DECPM_PUBLIC_ADDRESS`,
`DECPM_TIMEOUT_*`, `DECPM_NOISE_RETRY_*`, `DECPM_PEER_WAIT_POLL_DELAY_MS`, `DECPM_ACS_BLOCK_BYTES`.

Added env: `DECPM_HEARTBEAT_INTERVAL_SECS` (3600), `DECPM_HEARTBEAT_MIN_INTERVAL_SECS` (60),
`DECPM_PEER_STALE_FACTOR` (3), `DECPM_OBSERVER_POLL_SECS` (3),
`DECPM_AUTO_UPLOAD_COORDINATION_DAR` (true), `DECPM_ACS_SPOOL_DIR` (`{dir}/data/acs`).

`PackageConfig` gains `decman_coordination: Option<String>` (default `#decman-coordination-v1`).

## 10. Tests

Unit (cargo): codec round trips for every template; mapping builders are byte-stable;
serial guard; validation per kind (positive and each negative); registry staleness; hash
co-sign idempotency (mocked client); migration 000020 on a seeded DB; DTO snapshots.

Daml (`dpm test`): as in section 3.

Integration harness (`integration-tests/*.sh`): drop Noise ports and key reads; allocate a
node party per node on the JSON Ledger API, grant rights, `PUT /node-identity`; wait for
each node's `DecmanNode` to appear on its peers; set heartbeat interval 5 s. Phases: keep
the ledger-only ones; rewrite the coordination ones to the new steps; delete
`peer_health_flip` (replace with a staleness flip), `peer_3_strikes_abort`, `retry_with_offline_peer`,
`invite_cap`, `check_peer_dars` (replace with a vetted-packages comparison).

## 11. Documentation

README, ARCHITECTURE, DEPLOYMENT_GUIDE, USER_GUIDE, CUSTOM_DAML_TEMPLATES, KMS_SIGNING,
CONTRIBUTING: remove every Noise reference; document the node identity, the registry, the
proposal model, the DAR pin flow, the ACS handoff, the cutover runbook, and the new env vars.
Kubernetes manifests lose port 9000 and the Noise LoadBalancer. `Dockerfile` exposes 8080 only.

## 12. Evidence summary

* Partial-authorization proposals propagate and merge by fingerprint; co-sign by hash;
  no expiry, no cancel; `Snapshot(MaxValue)` for discovery. (Canton `TopologyManager.scala`,
  `TopologyStateProcessor.scala`, `GrpcTopologyManagerReadService.scala`; Splice
  `SvOnboardingPartyToParticipantProposalTrigger.scala`.)
* DND serial 1 needs every owner's signature and every owner's root `NamespaceDelegation`
  in the synchronizer store first; a P2P under a decentralized namespace needs the DND
  effective first. (`TopologyMappingChecks.scala`, `TransactionAuthorizationCache.scala`.)
* A root `NamespaceDelegation` is self-authorizing and carries the full public key;
  `[Namespace, Protocol]` is a valid usage set, also on KMS. (`Signing.scala`,
  `KmsPrivateCrypto.scala`.)
* Clearing an Onboarding flag needs only the joiner's participant namespace.
  (`TopologyMapping.scala` requiredAuth; `PartyReplicationTopologyWorkflow.scala`.)
* Interactive submission: 24 h `preparationTimeRecordTimeTolerance` on the Global
  Synchronizer; all signatures in one execute call; any node with `CanExecuteAs` may execute.
* Online party replication is alpha with `ProtocolVersion.dev` codecs; unusable on PV 35.
* Daml visibility is stakeholder-only; a registry needs explicit observers; traffic cost of a
  1 KB create with 4 observers is well under the free base rate.
* Canton requires every observer participant to have vetted the package before a create.
