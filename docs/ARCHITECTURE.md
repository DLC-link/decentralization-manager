# Architecture Overview

The Decentralized Party Manager (DecMan) enables multiple organizations to jointly control a single Canton party identity without any central authority. It automates the multi-party topology operations, contract deployment, and governance workflows required to create and manage shared party namespaces on Canton blockchain networks.

## Core Concepts

### Decentralized Namespace Definition (DNS)

A decentralized namespace is a Canton topology primitive that allows multiple participants to jointly own a single party identity. The namespace is computed as a **SHA-256 domain-separated hash** of the initial owners' namespace fingerprints:

```
HashPurpose = 37 (DecentralizedNamespaceNamespace)

hash = SHA-256(
    purpose_id[4 bytes, big-endian]
    for each namespace in sorted(owners):
        length(namespace_utf8)[4 bytes, big-endian]
        namespace_utf8[variable]
)

result = "1220" + hex(hash)   // Multihash SHA-256 prefix
```

Key properties:
- The hash is **immutable** -- it is computed once from the initial set of owners and never changes
- Owners are sorted lexicographically before hashing for determinism
- The threshold (minimum signers required) defaults to `ceil(n/2)` (a bare majority for even `n`), is configurable when the party is created, and can be changed later via the change-threshold workflow
- Adding or removing members, or changing the threshold, updates the `DecentralizedNamespaceDefinition` mapping but does not change the namespace hash itself

### PartyToParticipant (P2P)

The PartyToParticipant topology mapping connects a decentralized party to its hosting participants. Each entry includes:
- The party ID (derived from the decentralized namespace)
- The hosting participant's ID
- The participant's permission level (Submission, Confirmation, Observation)
- Daml signing keys embedded in the mapping (Canton 3.4+)

### Threshold Model

The system defaults to a majority threshold for both topology changes and governance actions:

| Operation | Threshold |
|-----------|-----------|
| Topology changes (DNS/P2P) | `ceil(n/2)` of namespace owners must sign by default (bare majority for even `n`); set at creation and adjustable via the change-threshold workflow |
| Governance actions | Configurable per `GovernanceRules` contract |

### Key Types

The system manages these key types:

| Key | Algorithm | Purpose |
|-----|-----------|---------|
| Party key | Ed25519 (Canton), usages `[Namespace, Protocol]` | Signs the party's topology changes and its ledger transactions |
| Legacy namespace key | Ed25519 (Canton) | Signs topology changes for a party created before 2.0 |
| Legacy Daml signing key | Ed25519 (Canton) | Signs ledger transactions for a party created before 2.0 |

For every new decentralized party each member generates one vault key named
`{prefix}-key`. That one key carries both usages, so it signs topology changes
and ledger transactions. Its self-signed root `NamespaceDelegation` publishes
the public key into the synchronizer topology store. The proposer reads the key
from that delegation and copies it into `PartyToParticipant.party_signing_keys`.

A party created before 2.0 keeps its two keys, `{prefix}-namespace` and
`{prefix}-daml-transactions`, and keeps working. Every key lookup reads the
chain first, then the local cache, then the legacy vault name.

One dual-usage key has a second effect. The namespace owner fingerprint equals
the party signing-key fingerprint, so a kick names its target on-chain.

## System Components

```
   +--------------------+   +--------------------+   +--------------------+
   |    Operator 1      |   |    Operator 2      |   |    Operator 3      |
   |  +--------------+  |   |  +--------------+  |   |  +--------------+  |
   |  | decman :8080 |  |   |  | decman :8080 |  |   |  | decman :8080 |  |
   |  | observer loop|  |   |  | observer loop|  |   |  | observer loop|  |
   |  +------+-------+  |   |  +------+-------+  |   |  +------+-------+  |
   |         |          |   |         |          |   |         |          |
   |  +------v-------+  |   |  +------v-------+  |   |  +------v-------+  |
   |  | Participant  |  |   |  | Participant  |  |   |  | Participant  |  |
   |  | Admin API    |  |   |  | Admin API    |  |   |  | Admin API    |  |
   |  | Ledger API   |  |   |  | Ledger API   |  |   |  | Ledger API   |  |
   |  +------+-------+  |   |  +------+-------+  |   |  +------+-------+  |
   +---------|----------+   +---------|----------+   +---------|----------+
             |                        |                        |
             +------------+-----------+------------+-----------+
                          |                        |
              +-----------v------------------------v-----------+
              |             Canton synchronizer                |
              |   topology store      |    Daml transactions   |
              +------------------------------------------------+
```

Each operator runs one decman process beside one Canton participant. That
process talks to its own participant only. Two decman processes never open a
connection to each other. Both write to the same synchronizer and read from it.

### HTTP Server (actix-web)

The HTTP server serves the embedded React frontend and exposes REST endpoints
for managing decentralized parties. It listens on TCP 8080, and it is the only
port decman opens. Key responsibilities:

- Serving the single-page application (embedded at compile time via `build.rs`)
- Proxying topology and governance queries to Canton APIs
- Starting, cancelling, and reporting multi-party runs
- Managing authentication tokens via Keycloak or Auth0
- Streaming an ACS snapshot out on `GET /acs-export/{party}/{target}` and in on
  `POST /acs-import/{party}`, both under the admin role

Payload limit: 100 MB (for DAR file uploads).

### Node identity

Each node has one **node party**. The node party is a normal Canton party. Its
own participant hosts it with Submission permission, so it can submit Daml
commands. decman stores it as a `party_credentials` row with `kind = 'node'`.

The node party signs the registry entry, workflow proposals, acceptances,
declines, submission rounds, signatures, ACS manifests, and heartbeats. It
exists before any decentralized party exists.

| Route | Auth | Purpose |
|---|---|---|
| `GET /node-identity` | admin | Report the node party, its participant, and its hosting permission |
| `PUT /node-identity` | admin | Set the node party |

`PUT /node-identity` needs the admin role. It skips authentication only while
`party_credentials` is entirely empty, which is the first-run case. An upgraded
node already holds decparty rows, so its operator sets the node identity with an
admin JWT. The handler reads the head `PartyToParticipant` of the party. It
rejects the party unless this participant hosts it with Submission permission.

A `kind = 'node'` row never adds a trusted JWT issuer. `GET /auth/status`, the
party lists, and the inbound trusted-issuer set all skip it.

### Peer exchange

Operators exchange one string per peer: `participant_id,node_party_id,name`.
The UI button "Share my identity" copies that string. "Add peer" pastes it.

The `peers` table holds the participant id, the name, and the node party. It
holds no address, no port, and no key, because no node dials another node. A
peer without a node party cannot join a run.

This node verifies the claim before it names a peer's node party as an
observer. It calls `ListPartyToParticipant` on the tokenless Admin API. It
requires the claimed participant to host that party with Submission permission.

### Canton gRPC Client

The application communicates with Canton via gRPC using the following services:

**Admin API services:**
| Service | Purpose |
|---------|---------|
| `TopologyManagerReadService` | Query DNS, P2P, namespace delegations, vetted packages, and pending proposals |
| `TopologyManagerWriteService` | Propose topology changes and co-sign them by transaction hash |
| `VaultService` | Manage key vaults (generate keys, sign, export) |
| `IdentityInitializationService` | Query participant ID |
| `SynchronizerConnectivityService` | Discover synchronizer IDs, disconnect and reconnect during ACS import |
| `PackageService` | Upload DAR files, list vetted packages |
| `PartyManagementService` (`canton.admin.participant.v30`) | Offline party replication: `ExportPartyAcs`, `ImportPartyAcs`, `GetHighestOffsetByTimestamp`, `ClearPartyOnboardingFlag` |

**Ledger API services:**
| Service | Purpose |
|---------|---------|
| `CommandService` | Create and exercise coordination contracts as the node party |
| `StateService` | Read the active coordination contracts |
| `UserManagementService` | Query user rights |
| `PartyManagementService` (`ledger.api.v2.admin`) | Query party metadata and annotations |
| `InteractiveSubmissionService` | Prepare and execute multi-party interactive submissions |
| `UpdateService` | Read transaction updates by offset |
| `EventQueryService` | Look up create/archive events for a contract |

### Observer loop and run engine

One background task drives every run. It replaces the command channel that
decman 1.x ran between nodes. The [observer tick](#the-observer-tick) section
lists what it does each time it wakes.

Each workflow kind has one step machine with a proposer side and a member side.
The engine reads state and writes the next action; it holds no long-lived task
per run. The observer calls it once per tick under a per-run lock, and skips a
run that another tick still holds.

`workflow_runs` is the UI projection. Each row names its `proposal_cid`, the
proposer's party and participant, the member variant, and the pinned topology
hashes. Cancel, dismiss, and retry are row operations, and the engine re-reads
the row before every ledger write.

## Coordination through Canton

decman nodes coordinate through two Canton facilities. The synchronizer topology
store carries partially signed topology proposals. Daml contracts carry the
workflow intent and every operator decision.

### The topology-proposal path

A topology change needs signatures from several namespace owners. Canton merges
those signatures itself, so decman never collects them over a channel of its own.

| # | Actor | Action |
|---|-------|--------|
| 1 | Proposer | Reads the accepted mapping from the synchronizer store at serial `S` |
| 2 | Proposer | Calls `Authorize` with the new mapping at serial `S + 1` and `must_fully_authorize = false` |
| 3 | Synchronizer | Stores the transaction as a proposal and propagates it to every member |
| 4 | Member | Lists the pending proposals for that mapping and validates the content |
| 5 | Member | Calls `Authorize { transaction_hash }`, which adds its own signature |
| 6 | Canton | Merges the signatures by fingerprint and applies the change once the threshold is met |
| 7 | Everyone | Polls the accepted state until serial `S + 1` is effective |

Every read targets the synchronizer store with `operation = ADD_REPLACE` and
`time_query = Snapshot(MaxValue)`. The proposer computes the transaction hash
locally as `multihash_sha256(be32(11) || versioned transaction bytes)`. A member
signs that hash and never resubmits the mapping.

Order matters. The proposer proposes the `DecentralizedNamespaceDefinition`
first and waits until it is effective. Only then does it propose the
`PartyToParticipant`. Canton rejects a `PartyToParticipant` under a
decentralized namespace whose definition is not yet effective. Before it
proposes a namespace definition, the proposer waits until every owner's root
`NamespaceDelegation` is effective in the synchronizer store.

Canton offers no way to withdraw a topology proposal. Cancel therefore archives
the `WorkflowProposal` only. Members then stop co-signing, because they no
longer find the accepted proposal. A cancelled onboarding never becomes
effective, because it still needs the invitees. A cancelled kick or
change-threshold that already reached the threshold **still becomes effective**,
and the UI says so.

`GET /proposals/unsolicited` lists the pending topology proposals that no
accepted `WorkflowProposal` explains. The observer refreshes that list every 60
seconds for the UI, and it never signs from it.

### The Daml coordination package

The `decman-coordination-v1` package holds every contract that the nodes
exchange. One node party signs each template, and the nodes that need it observe
it.

| Template | Signatory | Observers | Purpose |
|----------|-----------|-----------|---------|
| `DecmanNode` | node party | peer node parties | The registry entry: participant id, display name, version, coordination version, peer list, and last heartbeat |
| `WorkflowProposal` | proposer | invitees | One run's intent: kind, participants, party, threshold, base serials, DAR pins, package names, and expiry |
| `WorkflowAcceptance` | acceptor | proposer, invitees | One invitee's consent, with its participant id and its key fingerprints |
| `WorkflowDecline` | decliner | proposer, invitees | One invitee's refusal and its reason |
| `WorkflowOutcome` | proposer | invitees | The run's final result, success or failure |
| `SubmissionRound` | proposer | signers | One prepared ledger transaction: hex bytes, hash, hashing scheme, preparation time, max record time, and deadline |
| `SubmissionSignature` | signer | proposer, signers | One member's signature over a round, with its key fingerprint, format, and algorithm |
| `AcsManifest` | exporter | party members | One ACS snapshot's pin: party, target participant, activation serial, size, sha256, and package ids |

Choices follow the same shape. `DecmanNode` offers `Heartbeat`, `Update`, and
`Retire`. `WorkflowProposal` offers `Accept`, `Decline`, `Cancel`, and `Finish`.
`SubmissionRound` offers `Sign` and `Close`. Each record template offers an
`Archive` choice for its own signatory, and the observer sweeps finished records.

Canton rejects a create whose observer participant has not vetted the package.
A node therefore names a peer as observer only after that peer's participant
vets `decman-coordination-v1`. It retries the unvetted peer on the next tick. A
startup task uploads and vets the embedded DAR locally, unless
`DECPM_AUTO_UPLOAD_COORDINATION_DAR` is `false`. `POST /dars/upload` remains the
manual path.

### The observer tick

One background task ticks every `DECPM_OBSERVER_POLL_SECS` seconds. The default
is 3 seconds, and mainnet guidance is 10. Each tick does this, in order:

1. Loads the node identity, and idles when the operator has not set one.
2. Refreshes the peer health snapshot, publishes or updates this node's `DecmanNode`, and heartbeats when due.
3. Reads the in-progress rows of `workflow_runs`.
4. Reads every proposal, acceptance, decline, and outcome this node can see.
5. Projects the undecided proposals into `pending_invitations` and the UI cache.
6. Drives each run under a per-run `try_lock`, and skips a run that another tick still holds.
7. Every 60 seconds, scans the unfiltered topology proposals and archives this node's finished records.

The loop exports metrics under the `decman_observer_*` prefix. They cover a tick
counter, a tick-duration histogram, and a last-tick gauge. They also cover a
driven-runs counter, an error counter by stage, and a no-identity counter.

A member never signs from a cached match. It re-reads the `WorkflowProposal` as
active, its own run row as in-progress, and the pinned hash, in the same tick as
the signature.

## Node Registry and Peer Health

Every node publishes one `DecmanNode` contract and names its peers as observers.
The contract carries the node's participant id, display name, decman version,
build version, coordination version, peer list, and last heartbeat time.

The node exercises `DecmanNode_Heartbeat` on a timer. The cadence is
`DECPM_HEARTBEAT_INTERVAL_SECS`, and the template refuses a heartbeat that
arrives sooner than `minHeartbeatIntervalSecs`. The node exercises
`DecmanNode_Update` only when a published field differs from the desired value.

`GET /registry` lists three groups: this node's own entry, the entries signed by
a configured peer, and the inbound entries. An inbound entry is one whose
signatory is not in the peers table, which means that operator added this node
first.

`GET /participants-status` reports one status per configured peer, from the
snapshot that the observer refreshes:

| Status | Meaning |
|--------|---------|
| `CurrentNode` | This node |
| `Active` | An entry is visible and its last heartbeat is recent |
| `Stale` | An entry is visible, but its age exceeds `DECPM_PEER_STALE_FACTOR` times the peer's heartbeat interval |
| `Unknown` | The peer has vetted the coordination package, but no entry signed by its node party is visible |
| `Unvetted` | The peer's participant has not vetted the coordination package |

The value is a heartbeat age, not a liveness probe. The UI labels it "last
heartbeat N ago". An entry counts for a peer only when its signatory equals the
peer's node party and its participant claim equals the peer's participant. The
hosting check must also report Submission permission.

A proposer refuses to start a run unless every invitee passes three tests. The
invitee has vetted the coordination package. Its registry entry is visible. Its
`coordinationVersion` is at least the proposer's. The start handler returns 409
and names each participant that fails, and it distinguishes "no entry visible"
from "entry too old".

## Proposer and Members

One node proposes a run; the other nodes are its members. Any node can take
either role, and each run decides the roles anew. The HTTP and database layers
still name the proposer role `Coordinator`.

### The proposer

The proposer starts a run. It generates its own key when the kind needs one,
creates the `WorkflowProposal`, and names the invitees as observers. It then
waits for acceptances, writes each topology change into the synchronizer store,
and polls until the change is effective. For a contracts run it prepares each
transaction, opens a `SubmissionRound`, counts the verified signatures, and
executes. It exercises `WorkflowProposal_Finish` at the end.

### A member

A member sees the proposal as an invitation card. Its operator accepts or
declines once. On accept, decman writes the decision locally, generates the key
when the kind needs one, and exercises `WorkflowProposal_Accept` with its key
material. After that the node co-signs every topology proposal that matches what
its operator accepted.

The operator consents once per run, not once per signature. The proposal expires
at `expiresAt`, which defaults to seven days. The node marks a mapping spent
once its change is effective at `base + 1`.

### Trust model

The proposer decides the order of a run. It does not decide what a member
signs. A member's participant holds keys that authorize topology changes for the
decentralized party, so the member checks the content itself.

**What a member checks before it co-signs a topology proposal:**

- **The accepted proposal.** The `WorkflowProposal` is still active on the
  ledger, has not expired, and its kind matches the mapping at hand. The member
  re-reads it in the same tick as the signature.
- **The counted acceptances.** Each acceptance names this proposal and this
  proposer. Its acceptor is an invitee, and its `participantId` is one of the
  proposal's participants. That participant hosts the acceptor with Submission
  permission. One acceptor may contribute one acceptance, and a second one fails
  the run closed.
- **The base serial.** The accepted mapping's serial equals the proposal's
  `dndBaseSerial` or `p2pBaseSerial`, and the pending serial is exactly one
  higher. A different serial means the topology changed during the run, so the
  member fails the run.
- **The `ADD_REPLACE` operation.** A `REMOVE` with unchanged content falls back
  to party-namespace authorization, so the member pins the operation and refuses
  anything else.
- **The signature set.** The proposer's own owner fingerprint is among the
  signers, and this node's fingerprint is not yet there.
- **The mapping contents**, per kind:

| Kind | Namespace definition | Party mapping |
|------|----------------------|---------------|
| Onboarding | The owners are the proposer's fingerprint plus one per counted acceptance, the namespace is the hash of that owner set, and the threshold matches the proposal | The party is `prefix::namespace`, each host is a counted participant at Confirmation, and each key is the owner's root-delegation key |
| Add party | The owners are the head owners plus the joiner's fingerprint | The hosts are the head hosts plus the joiner at Confirmation and `Onboarding`, every head host is unchanged, and the keys gain the joiner's root-delegation key |
| Kick | The owners are the head owners minus exactly the kicked fingerprint | The hosts are the head hosts minus the kicked participant, every survivor is unchanged, and exactly one key leaves |
| Change threshold | Equal to the head, except the threshold | Equal to the head, except both thresholds |

The member compares hosts as full `(participant_uid, permission, onboarding)`
tuples, and keys as full `SigningPublicKey` byte sets. It refuses to sign on any
mismatch. It then fails its run with the reason and shows that reason in the UI.

**What a member checks before it signs a contracts round.** It requires the
round's party to be the accepted party and `act_as` to name that party. It
requires its own key fingerprint to be in the head `party_signing_keys`. It
recomputes the hash from the transaction with `canton_hash` and compares. It
decodes the transaction and requires every root node to be a `Create` whose
package name is in the accepted `packageNames`. It refuses a round past its
deadline, and it refuses a hashing scheme that it cannot reproduce.

**What a member checks before it imports an ACS snapshot.** It requires the
named participant to host the manifest's exporter with Submission permission. That participant is a current host of the party and is not itself
onboarding. The exporter equals the node party recorded for that participant.
The activation serial is the earliest serial that marks the joiner `Onboarding`.
The file's size and sha256 match the manifest, and this node has vetted every
package that the manifest names.

**What a member still cannot check:**

- **Legacy kick attribution.** On a party created before 2.0, the namespace
  owner fingerprints differ from the party signing keys. A member then proves
  only that exactly one key leaves, that the key is not its own, and that no
  survivor claims it. It reads the link between the removed key and the named
  participant from its own `dec_party_participant` cache, not from the chain.
- **The arguments of a contract.** A member confirms that every root node
  creates a contract of an accepted package, acting as the accepted party. It
  does not compare the field values against anything that its operator approved.
- **The completeness of an ACS snapshot.** The joiner verifies the manifest and
  the file. It cannot prove that the snapshot holds every contract that the party
  held at the activation serial.
- **The proposer's code.** The checks bound what a signature can authorize. They
  do not make the proposer honest.

Governance confirm and execute stay outside this model. A member builds those
commands locally, and the Daml layer re-validates them against the on-ledger
proposal.

## Workflows

Six run kinds share the shape that the previous chapter describes. The proposer
creates a `WorkflowProposal`, the invitees accept or decline, and the proposer
drives the rest. The step names below are the `current_step` values that the UI
shows.

### Onboarding (Decentralized Party Creation)

This run creates a new decentralized party across several participants.

**Proposer steps:**

| # | Step | Description |
|---|------|-------------|
| 1 | GenerateKeys | Generate the dual-usage party key and publish its root `NamespaceDelegation` |
| 2 | WaitingForAcceptances | Wait until every invitee has a counted acceptance |
| 3 | ProposeNamespace | Read each owner's root delegation, then propose the namespace definition at serial 1 |
| 4 | AwaitNamespace | Poll until the namespace definition is effective |
| 5 | ProposeParty | Propose the party mapping at serial 1, with every owner's key |
| 6 | AwaitParty | Poll until the party mapping is effective |
| 7 | Complete | Exercise `WorkflowProposal_Finish` and close the run |

**Member steps:**

| # | Step | Description |
|---|------|-------------|
| 1 | GenerateKeys | Generate the dual-usage party key, wait for its root delegation, then accept the proposal |
| 2 | CoSignNamespace | Validate the pending namespace definition and co-sign it by hash |
| 3 | CoSignParty | Validate the pending party mapping and co-sign it by hash |
| 4 | Complete | Close the run |

**Canton API calls:**
- `VaultService.GenerateKey` -- the `{prefix}-key` vault key (step 1)
- `TopologyManagerWriteService.Authorize` -- publish the root `NamespaceDelegation` to the Authorized store (step 1)
- `CommandService.SubmitAndWaitForTransaction` -- create, accept, and finish the proposal
- `TopologyManagerReadService.ListNamespaceDelegation` -- read each owner's public key (proposer step 3)
- `TopologyManagerWriteService.Authorize` -- propose and co-sign both mappings
- `TopologyManagerReadService.ListDecentralizedNamespaceDefinition` / `ListPartyToParticipant` -- poll for the effect

**Quorum:** every invitee must accept.

**Minimum participants:** 2

### Kick (Remove Participant)

This run removes a participant from an existing decentralized party. The
invitees are the remaining members. The kicked node sees the topology proposal as unsolicited.

**Proposer steps:**

| # | Step | Description |
|---|------|-------------|
| 1 | WaitingForAcceptances | Wait until enough members accept to reach the quorum |
| 2 | ProposeChanges | Propose the reduced namespace definition, then the reduced party mapping |
| 3 | AwaitChanges | Poll until both mappings are effective |
| 4 | Complete | Finish the proposal and close the run |

**Member steps:** `CoSignChanges`, then `Complete`. The member validates each
mapping against the head state and co-signs it by hash.

**Canton API calls:**
- `TopologyManagerReadService.ListDecentralizedNamespaceDefinition` / `ListPartyToParticipant` -- read the head state and poll for the effect
- `TopologyManagerWriteService.Authorize` -- propose and co-sign both mappings
- `CommandService.SubmitAndWaitForTransaction` -- the proposal, the acceptances, and the outcome

**Quorum:** `max(previous threshold, new threshold)` owner signatures per
mapping, counting the proposer. The proposer leaves `WaitingForAcceptances` one
acceptance short of that number, because its own signature counts. Later
acceptances still co-sign.

**Minimum participants:** 2 (remaining members)

### Contracts (Contract Creation)

This run creates Daml contracts under a decentralized party. Each member signs
the prepared transaction with its own party key.

**Proposer steps:**

| # | Step | Description |
|---|------|-------------|
| 1 | WaitingForAcceptances | Wait until every invitee has a counted acceptance |
| 2 | AwaitDars | Wait until every participant has vetted the contracts' packages |
| 3 | PrepareSubmissions | Prepare one transaction per contract definition and open one `SubmissionRound` for each |
| 4 | CollectSignatures | Verify each `SubmissionSignature` against the head `party_signing_keys` and dedupe by fingerprint |
| 5 | ExecuteSubmissions | Execute each round once the party's signing threshold is met, then close it |
| 6 | Complete | Finish the proposal and close the run |

**Member steps:** `UploadDars`, `SignSubmissions`, then `Complete`. The member
uploads the packages locally, checks each round, signs it with its party key,
and exercises `SubmissionRound_Sign`.

**Canton API calls:**
- `PackageService.UploadDarFile` -- upload the packages locally (member step 1)
- `TopologyManagerReadService.ListVettedPackages` -- observe vetting per participant (proposer step 2)
- `InteractiveSubmissionService.PrepareSubmission` -- prepare each transaction (proposer step 3)
- `VaultService.Sign` -- sign a round with the party key (member step 2)
- `InteractiveSubmissionService.ExecuteSubmissionAndWaitForTransaction` -- execute one round with the verified signatures (proposer step 5)

**The signing window.** The proposer prepares with `max_record_time = now + 20h`
and reads the synchronizer's `preparationTimeRecordTimeTolerance`. The round's
deadline is `min(maxRecordTime, preparationTime + tolerance)` minus 30 minutes.
The proposer re-prepares an expired round, and the members sign it again.

**Quorum:** the party's `party_signing_keys.threshold` verified signatures. The
proposer refuses a start when the participants in the run cannot reach that
number.

### DARs (Package Distribution)

This run distributes DAR packages to every participant. decman never sends DAR
bytes to another node.

**Proposer steps:**

| # | Step | Description |
|---|------|-------------|
| 1 | WaitingForAcceptances | Wait until every invitee has a counted acceptance |
| 2 | AwaitVetting | Poll each participant's vetted packages until every pin is vetted everywhere |
| 3 | Complete | Finish the proposal and close the run |

**Member steps:** `UploadDars`, then `Complete`.

`POST /dars/distribute` pins each file by filename, sha256, main package id, and
size. The proposal carries those pins, and the proposer uploads the files to its
own participant. Each other operator obtains the same files and uploads them
through `POST /dars/upload` with `pin_instance` set to the run id. The handler
refuses a file whose sha256 matches no pending pin, and it always passes the
pinned main package id to Canton.

**Canton API calls:**
- `PackageService.UploadDarFile` -- upload a pinned file locally
- `TopologyManagerReadService.ListVettedPackages` -- observe vetting per participant

**Quorum:** every invitee must accept.

**Minimum participants:** 2

### Add Party (Add a Host to an Existing Decentralized Party)

This run adds a hosting participant to a decentralized party that already
exists, and it replicates the party's active contracts to that participant. The party keeps transacting on
its existing hosts throughout: Canton's `HostingParticipant.Onboarding` marker
suspends it only on the joining node.

**Proposer steps:**

| # | Step | Description |
|---|------|-------------|
| 1 | GenerateKeys | Nothing to generate; the proposer is a current owner and advertised its key in the proposal |
| 2 | WaitingForAcceptances | Wait until every invitee has a counted acceptance |
| 3 | ProposeChanges | Propose the widened namespace definition, then the party mapping with the joiner marked `Onboarding` |
| 4 | AwaitChanges | Poll until both mappings are effective |
| 5 | AwaitReplication | Wait until the joiner clears its `Onboarding` marker |
| 6 | Complete | Finish the proposal and close the run |

**Joiner steps:**

| # | Step | Description |
|---|------|-------------|
| 1 | GenerateKeys | Generate the dual-usage party key, wait for its root delegation, then accept the proposal |
| 2 | CoSignChanges | Capture the pre-activation offset, validate both mappings, and co-sign them |
| 3 | SyncAcs | Import the snapshot, or skip it when the manifest reports zero bytes |
| 4 | ClearOnboarding | Exercise `ClearPartyOnboardingFlag` and poll until the party reports `onboarded` |
| 5 | Complete | Close the run |

**Member steps (every other current host):**

| # | Step | Description |
|---|------|-------------|
| 1 | CoSignChanges | Capture the export offset first, then validate and co-sign both mappings |
| 2 | PublishManifest | Export the snapshot to the spool directory and publish an `AcsManifest` |
| 3 | Complete | Close the run |

**The snapshot handoff.** decman never sends ACS bytes to another node. A current
host exports the snapshot to `DECPM_ACS_SPOOL_DIR` and publishes the manifest.
The joining operator downloads the file with
`GET /acs-export/{party}/{target}?serial=N` from that host and uploads it with
`POST /acs-import/{party}?serial=N&exporter=<participant>` on its own node. Both
routes need an admin JWT and stream gzip. The joiner skips the transfer when the
manifest reports zero bytes, and it clears the flag at once. decman deletes the
spool files once the joiner reports `onboarded`, or once the operator dismisses
the run.

**Canton API calls:**
- `VaultService.GenerateKey` -- the joiner's party key (joiner step 1)
- `TopologyManagerWriteService.Authorize` -- propose and co-sign both mappings
- `PartyManagementService.GetHighestOffsetByTimestamp` -- capture the export offset (member step 1)
- `PartyManagementService.ExportPartyAcs` -- export the snapshot, scoped to the joiner (member step 2)
- `PackageService.ListPackages` -- check the joiner's vetted packages before the import (joiner step 3)
- `SynchronizerConnectivityService.DisconnectSynchronizer` / `ReconnectSynchronizers` -- bracket the import (joiner step 3)
- `PartyManagementService.ImportPartyAcs` -- import the snapshot (joiner step 3)
- `PartyManagementService.ClearPartyOnboardingFlag` -- clear the marker (joiner step 4)

**Quorum:** every invitee must accept.

**Minimum participants:** 2 (an existing member and the joiner)

**Restriction:** the run needs a `DecentralizedNamespaceDefinition`. It cannot
add a host to a local party or to an external party. The preflight refuses any
other party type.

### Change Threshold

This run changes the signing threshold of an existing decentralized party.

**Proposer steps:** `WaitingForAcceptances`, `ProposeChanges`, `AwaitChanges`,
then `Complete`. Both mappings equal the head state, except the threshold.

**Member steps:** `CoSignChanges`, then `Complete`.

**Canton API calls:**
- `TopologyManagerReadService.ListDecentralizedNamespaceDefinition` / `ListPartyToParticipant` -- read the head state and poll for the effect
- `TopologyManagerWriteService.Authorize` -- propose and co-sign both mappings

**Quorum:** `max(previous threshold, new threshold)` owner signatures per
mapping, counting the proposer.

**Minimum participants:** 2 (party members)

### Cancel, decline, and retry

| Action | Actor | Effect |
|--------|-------|--------|
| Cancel | Proposer | Exercises `WorkflowProposal_Cancel` and marks the run `Cancelled`. Members see the proposal vanish and cancel their rows. A topology proposal that already reached the threshold still becomes effective |
| Decline | Invitee | Exercises `WorkflowProposal_Decline`. For onboarding, add-party, contracts, and DARs this fails the run for everyone. For kick and change-threshold it fails the run only when the remaining invitees cannot reach the quorum |
| Retry | Proposer or member | Re-runs the same ensure loop. The proposer re-reads the accepted serial and re-proposes only when its proposal is gone and the base serial still holds. A member re-issues `Authorize { transaction_hash }` |
| Dismiss | Either | Removes the card. The run is already terminal |

### External Party Onboarding (Tenant API)

Creates a **co-validated** party: hosted on several participants at once but
controlled by a single Ed25519 key its owner holds. This run uses no
`WorkflowProposal`. It is stateless HTTP, driven by the wallet, with Canton
itself as the only coordination store.

**Steps:**

| # | Step | Actor | Description |
|---|------|-------|-------------|
| 1 | prepare | Wallet -> every host | `POST /v0/tenant/prepare`. Each host independently builds the serial-1 `PartyToParticipant` and returns it with the hash to sign |
| 2 | compare | Wallet | Require every host's bytes to be identical; reject the onboarding otherwise |
| 3 | sign | Wallet | Sign the hashes locally. The private key never leaves the wallet process |
| 4 | onboard | Wallet -> every host | `POST /v0/tenant/onboard`. Each host re-validates the bytes, co-signs with its own topology key, and submits |
| 5 | poll | Wallet | `GET /v0/tenant/{party}/status` on every host until each reports the party hosted |

Canton promotes the mapping once every host has authorized. The party's signing
key rides inside the mapping as `party_signing_keys`, so the party transacts
through `InteractiveSubmissionService` rather than plain submission.

**Authentication:** the `/v0/tenant/` prefix uses a separate tenant API key and
bypasses the operator JWT. Read-only `/external-parties` lists the external
parties a node hosts.

**Scope:** this flow replicates identity only, never state. It creates a new
party; it cannot decentralize a party that already holds contracts. See
[crates/decman-wallet/README.md](../crates/decman-wallet/README.md).

## Governance System

The governance system provides multi-party approval workflows built on Daml smart contracts. It uses a **modular, interface-based architecture** where a single `GovernanceRules` contract handles consensus logic (threshold validation, confirmation lifecycle) while domain-specific actions are defined as separate templates implementing the `GovernableAction` interface.

The system is split into the following Daml packages:

| Package | Purpose |
|---------|---------|
| `governance-core` | Core governance engine, interfaces, confirmation lifecycle, generic voting |
| `governance-token-custody` | Token transfer and preapproval actions |
| `governance-utility-onboarding` | Utility-registry onboarding actions and token mint / burn |
| `governance-utility-credential` | Credential domain: offer/accept free and paid credentials |

### GovernableAction Interface

The `GovernableAction` interface (from `Governance.Action`) is the single extension point for all domain-specific governance actions. Any Daml template implementing this interface can be governed without modifying the core governance contracts.

```
interface GovernableAction where
  viewtype GovernableActionView

  executeImpl : Update ()

  choice GovernableAction_Execute : ()
    controller (view this).governanceParty
  choice GovernableAction_Cancel : ()
    controller (view this).governanceParty
  choice GovernableAction_ProposerCancel : ()
    controller (view this).proposer

data GovernableActionView = GovernableActionView with
    governanceParty : Party    -- The party whose authority is required
    proposer        : Party    -- The party that proposed this action (required for proposer authorization)
    actionLabel     : Text     -- Human-readable label (e.g., "Transfer", "GenericVote")
    description     : Text     -- Description recorded in the execution result
```

Key design properties:
- **Authority propagation**: When `GovernanceRules` executes a `GovernableAction`, the governance party's authority flows through the exercise chain, allowing domain actions to create contracts or exercise choices that require governance party authorization
- **Open for extension**: New action types are added by creating new templates that implement the interface -- no changes to `GovernanceRules` required
- **Permissionless proposals**: Anyone can create a proposal template instance, but only governance members can confirm and execute it

### GovernanceRules Contract

The `GovernanceRules` contract (from `Governance.Rules`) is the core governance engine:

```
GovernanceRules {
    governanceParty          : Party                  -- The decentralized governance party
    members                  : Set Party              -- Committee members authorized to vote
    threshold                : Int                    -- Minimum confirmations required (1 <= threshold <= |members|)
    actionConfirmationTimeout : RelTime               -- Confirmation validity period (minimum 10 seconds)
    additionalProposers      : Optional (Set Party)   -- Allowlist of non-member proposers; None means "no allowlist"
}
```

The `additionalProposers` field (added in `v1`) lets a committee grant propose-only rights to parties that are not full voting members — for example, an admin console, a monitoring script, or a regulatory officer. The authoritative on-chain proposer set is `members ∪ fromOptional Set.empty additionalProposers`. `GovernanceRules_ConfirmAction` enforces that every proposal's `proposer` is in this set; outsider proposals are rejected at confirm time even if a member tries to confirm them. The two `SelfAction_*AdditionalProposer` variants below mutate this allowlist under the same threshold consensus as committee changes.

The contract provides two distinct paths for governance actions:

#### Self-Management Path (Closed Enum)

Self-management actions modify the `GovernanceRules` contract itself. They use a closed `GovernanceSelfAction` enum with value-based matching:

| Variant | Fields | Description |
|---------|--------|-------------|
| `SelfAction_AddMemberAndSetThreshold` | newMember, newThresholdAfterAdd | Add a governance member |
| `SelfAction_RemoveMemberAndSetThreshold` | removedMember, newThresholdAfterRemove | Remove a governance member |
| `SelfAction_SetThreshold` | updatedThreshold | Change the approval threshold |
| `SelfAction_SetTimeout` | updatedTimeout | Change the confirmation expiry timeout |
| `SelfAction_AddAdditionalProposer` | additionalProposer | Grant propose-only rights to a non-member party |
| `SelfAction_RemoveAdditionalProposer` | additionalProposer | Revoke propose-only rights from a party (normalizes the allowlist back to `None` when it becomes empty) |

Choices on `GovernanceRules` for self-management:
- `GovernanceRules_ConfirmGovernanceAction` -- Submit a self-action confirmation
- `GovernanceRules_ExecuteGovernanceAction` -- Execute when threshold is met (returns new `GovernanceRules`)
- `GovernanceRules_ExpireGovernanceSelfConfirmation` -- Remove a stale self-confirmation

Self-confirmations are stored as `GovernanceSelfConfirmation` contracts, matched by value equality on the `GovernanceSelfAction` data.

#### Domain Action Path (Interface-Based)

Domain actions are governed via `GovernableAction` proposals. Each proposal is a separate contract matched by `ContractId` (globally unique):

```
Proposer creates GovernableAction proposal
        |
        v
Members call GovernanceRules_ConfirmAction
        |
        v
GovernanceConfirmation created (per member)
        |
        v
Threshold met? ----No----> Wait for more / Expire stale
        |
       Yes
        |
        v
Member calls GovernanceRules_ExecuteConfirmedAction
        |
        v
GovernableAction_Execute fires (domain logic runs)
        |
        v
GovernanceExecutionResult created (immutable audit record)
```

Choices on `GovernanceRules` for domain actions:
- `GovernanceRules_ConfirmAction` -- Submit a confirmation for a proposal (args: `confirmer`, `actionProposalCid`)
- `GovernanceRules_ExecuteConfirmedAction` -- Execute when threshold is met (args: `executor`, `actionProposalCid`, `confirmations`)
- `GovernanceRules_ExpireConfirmation` -- Remove a stale confirmation

### GovernanceConfirmation

The `GovernanceConfirmation` contract (from `Governance.Confirmation`) represents a single member's approval of a domain action proposal:

```
GovernanceConfirmation {
    governanceParty   : Party                      -- The governance party
    confirmer         : Party                      -- The member who confirmed
    actionProposalCid : ContractId GovernableAction -- The proposal being confirmed
    actionLabel       : Text                       -- Label from the proposal (for UI/audit)
    expiresAt         : Time                       -- When this confirmation becomes invalid
}
```

Choices:
- `GovernanceConfirmation_Consume` -- Used during execution (consumes the confirmation)
- `GovernanceConfirmation_Expire` -- Remove if past `expiresAt`
- `GovernanceConfirmation_Cancel` -- Confirmer revokes their own confirmation

### Cancelling a proposal

The proposer retracts their own proposal with `GovernableAction_ProposerCancel`, which needs no vote. `POST /governance/cancel-proposal` exercises it, and the UI offers it as "Cancel proposal" on the proposer's card.

A cancel archives the proposal, and it leaves the confirmations behind. Those confirmations are inert, because execution fetches the proposal and fails without it. Decman marks such a card `orphaned` and shows the stranded contracts.

Each member clears their own confirmation whenever they want, through `GovernanceConfirmation_Cancel` ("Revoke"). Nobody clears another member's confirmation early: `GovernanceConfirmation_Expire` requires the confirmation to be past `expiresAt`, which is `actionConfirmationTimeout` after the vote. That time lock is deliberate, because the same choice would otherwise let one member strip another member's live vote.

The cancel endpoint therefore archives the proposer's own confirmation in the same transaction as the proposal. `POST /governance/propose` always creates that confirmation, so a cancel would otherwise strand a contract that only the proposer could clear.

### GovernanceExecutionResult

The `GovernanceExecutionResult` contract (from `Governance.ExecutionResult`) provides an immutable on-chain audit trail. It is created automatically when a domain action is executed:

```
GovernanceExecutionResult {
    governanceParty : Party    -- The governance party that executed this action
    actionLabel     : Text     -- The type of action (e.g., "Transfer", "GenericVote")
    description     : Text     -- Human-readable description of what was executed
    executor        : Party    -- The member who triggered execution
    confirmers      : [Party]  -- All members who confirmed
    executedAt      : Time     -- Ledger effective time
}
```

### Domain Action Templates

#### governance-core Actions

| Template | Action Label | Description |
|----------|-------------|-------------|
| `GenericVoteProposal` | `GenericVote` | Free-text governance vote with no on-chain side effect -- the vote outcome is recorded solely via the `GovernanceExecutionResult` |

The `GenericVoteProposal` template lives in module `Governance.GenericVote` (`daml/governance-core/daml/Governance/GenericVote.daml`), while the `GovernableAction` interface it implements is defined in module `Governance.Action`.

#### governance-token-custody Actions

| Template | Action Label | Description |
|----------|-------------|-------------|
| `TransferProposal` | `Transfer` | Transfer tokens from governance party via `TransferFactory` |
| `AcceptTransferProposal` | `AcceptTransfer` | Accept an incoming token transfer instruction |
| `SetupTokenPreapprovalProposal` | `SetupTokenPreapproval` | Set up utility token `TransferPreapproval` (one-step) |
| `SetupCcPreapprovalProposal` | `SetupCcPreapproval` | Set up Canton Coin `TransferPreapproval` (two-step, requires provider acceptance) |

#### governance-utility-onboarding Actions

The governance party bootstraps itself as a utility-registry provider and registrar via these actions, then mints and burns its own token instrument once onboarded. All contract IDs that the templates operate on are passed directly as fields — there is no intermediate state contract.

**Composite onboarding:**

| Template | Action Label | Description |
|----------|-------------|-------------|
| `SetupUtility` | `SetupUtility` | Runs the full onboarding chain in one vote: creates a `ProviderConfiguration`, accepts a `RegistrarServiceRequest`, and registers the instrument. Flags `createTransferRule` and `createAllocationFactory` drive optional artifact creation during the registrar-service-request accept. |

**Granular onboarding:**

| Template | Action Label | Description |
|----------|-------------|-------------|
| `ProvisionProviderService` | `ProvisionProviderService` | Create a `ProviderService` with `operator = proposer` and `provider = governanceParty`. Wraps a two-signatory create in a governance action so the operator's and governance party's authorities land in one transaction — direct creation fails on Canton when the governance party is externally signed. |
| `CreateProviderServiceRequest` | `CreateProviderServiceRequest` | Create a `ProviderServiceRequest` for a given `operator` and `provider` |
| `CreateUserServiceRequest` | `CreateUserServiceRequest` | Create a `UserServiceRequest` for a given `operator` and `user` |
| `SetProviderAppRewardBeneficiaries` | `SetProviderAppRewardBeneficiaries` | Set the provider-app reward beneficiaries on an `InstrumentConfiguration` |
| `SetEnableResultContracts` | `SetEnableResultContracts` | Toggle result-contract emission on a `RegistrarService` |
| `CreateDelegatedBatchedMarkersProxy` | `CreateDelegatedBatchedMarkersProxy` | Authorize the operator to create batched activity markers on behalf of the governance party |
| `RequestDevNetFeaturedAppRight` | `RequestDevNetFeaturedAppRight` | Self-grant a `FeaturedAppRight` to the governance party by exercising `AmuletRules_DevNet_FeatureApp`. DevNet only: the choice refuses when `AmuletRules.isDevNet` is false. Only the DSO sees `AmuletRules`, so the execute submission must disclose it. DecMan's execute handler fetches the current `AmuletRules` from the DSO scan API and discloses it automatically |

**Token issuance:**

| Template | Action Label | Description |
|----------|-------------|-------------|
| `MintProposal` | `Mint` | Offer a mint to a specific recipient via `AllocationFactory_OfferMint`. The recipient accepts the resulting `MintOffer` outside the plugin. Proposal carries `allocationFactoryCid`, `instrumentId`, and `instrumentConfigurationCid` directly. |
| `BurnProposal` | `Burn` | Offer a burn against a specific holder via `AllocationFactory_OfferBurn`. The holder accepts the resulting `BurnOffer` outside the plugin. Same CID fields as `MintProposal`. |

`MintProposal` and `BurnProposal` enforce `amount > 0.0` at the template-precondition level.

**Prerequisite.** `SetupUtility` consumes an existing `ProviderService` for the governance party. Use `ProvisionProviderService` to create one through the governance flow — direct creation of `ProviderService` via `POST /contracts` or a multi-party daml-script submit fails on Canton when the governance party is externally signed, because `ProviderService` has two signatories (`operator, provider`).

### Featured App Rewards (FAR)

FAR is a reward distribution mechanism for featured application participants in the Amulet ecosystem. The beneficiaries live on the governance party's `InstrumentConfiguration`:

```json
{
    "beneficiaries": [
        { "beneficiary": "party::1220abc...", "weight": "0.50" },
        { "beneficiary": "party::1220def...", "weight": "0.30" },
        { "beneficiary": "party::1220ghi...", "weight": "0.20" }
    ]
}
```

Weights are decimal strings and must sum to exactly 1.0; `SetProviderAppRewardBeneficiaries` is the governance proposal that sets or clears them. `DevNetFeatureApp` registers the party as a featured app on DevNet, which is the prerequisite for holding the `FeaturedAppRight` the rewards are paid against.

## Technical Constraints

### Infrastructure Requirements

- **Canton Admin API access required**: The application needs access to privileged Admin API endpoints (topology management, key vaults, package upload). This is not the public Ledger API -- it requires high node-level privileges.
- **7 Admin API services used**: TopologyManagerRead, TopologyManagerWrite, Vault, IdentityInitialization, SynchronizerConnectivity, PackageService, PartyManagement
- **Canton protocol version**: 35 (hardcoded for key export and topology operations)
- **Network ports**: TCP 8080 for the HTTP server. decman opens no other port, and it accepts no inbound connection from another decman node.
- **Coordination package**: every participant must vet `decman-coordination-v1` before it can take part in a run.
- **Node party**: every node needs one node party that its own participant hosts with Submission permission.

### Timing Constraints

- **Observer tick**: 3 seconds by default, raised to 10 on mainnet (`DECPM_OBSERVER_POLL_SECS`)
- **Heartbeat interval**: 3600 seconds by default (`DECPM_HEARTBEAT_INTERVAL_SECS`), with a template floor of 60 seconds (`DECPM_HEARTBEAT_MIN_INTERVAL_SECS`)
- **Staleness**: a peer reads `Stale` after 3 heartbeat intervals (`DECPM_PEER_STALE_FACTOR`)
- **Proposal lifetime**: 7 days (`DECPM_PROPOSAL_TTL_SECS`)
- **Topology propagation delay**: 30 seconds after the effective time of a topology change before it can be used. Without this wait, transactions may be rejected with `LOCAL_VERDICT_TIMEOUT`.
- **Topology retry settings**: 30 attempts with 2-second delays when polling for topology state changes
- **Contracts signing window**: `max_record_time` is 20 hours out, and the round's deadline is 30 minutes before the earlier of that time and `preparationTime + preparationTimeRecordTimeTolerance`
- **Unsolicited scan**: every 60 seconds, alongside the archive sweep
- **Step failure budget**: 6 consecutive failures of one step before the observer fails the run

### Configuration

| Variable | Default | Purpose |
|----------|---------|---------|
| `DECPM_OBSERVER_POLL_SECS` | `3` | Seconds between observer ticks |
| `DECPM_HEARTBEAT_INTERVAL_SECS` | `3600` | Seconds between heartbeats |
| `DECPM_HEARTBEAT_MIN_INTERVAL_SECS` | `60` | The floor the template enforces, clamped to the interval |
| `DECPM_PEER_STALE_FACTOR` | `3` | Heartbeat intervals before a peer reads `Stale` |
| `DECPM_PROPOSAL_TTL_SECS` | `604800` | Lifetime of a `WorkflowProposal`, in seconds |
| `DECPM_AUTO_UPLOAD_COORDINATION_DAR` | `true` | Whether a startup task uploads and vets the embedded coordination DAR |
| `DECPM_ACS_SPOOL_DIR` | `{data}/acs` | Where add-party snapshots are spooled |
| `DECPM_COORDINATION_PACKAGE_REF` | `#decman-coordination-v1` | The coordination package reference |

### Participant Minimums

| Workflow | Minimum Participants |
|----------|---------------------|
| Onboarding | 2 |
| Kick | 2 (remaining members) |
| Contracts | Enough hosts to reach the party's signing threshold |
| DARs | 2 |
| Add Party | 2 (an existing member + the joiner). The threshold may not count the joiner: it carries Canton's onboarding marker until its ACS import completes, and a write at threshold = post-add member count never becomes effective |
| Change Threshold | 2 (party members) |

Onboarding, add-party, and DARs need every invitee to accept. Kick and
change-threshold need `max(previous threshold, new threshold)` owner signatures,
counting the proposer. Contracts needs the party's
`party_signing_keys.threshold` verified signatures.

### Known Limitations

- **ACS sync for existing contracts**: Adding a new member to a party that already has active contracts requires Active Contract Set (ACS) export and import. The add-party run uses Canton offline party replication (`ExportPartyAcs` / `ImportPartyAcs` plus the `HostingParticipant.Onboarding` marker), so no repair mode and no participant restart are needed. The importing node disconnects from the synchronizer for the duration of the import, which briefly pauses that node. The party keeps transacting on its other hosts. If the party has no active contracts, the run skips the sync.
- **An operator moves the ACS snapshot**: decman never sends ACS bytes to another node. One operator downloads the file from a current host and uploads it to the joiner. Both nodes need spool space for the snapshot, so a party with a large ACS needs a sized `DECPM_ACS_SPOOL_DIR`.
- **Add-party is decentralized-party only**: The run requires a `DecentralizedNamespaceDefinition`. It cannot add a host to a local or an already-onboarded external party. See [Canton Party Replication](CANTON_PARTY_REPLICATION.md).
- **A local party cannot be decentralized in place**: its namespace is its participant's root key, and a party id embeds its namespace permanently. The tenant API can give such a party an owner-held signing key, which makes it externally signed and co-validatable (see [External Party Onboarding](#external-party-onboarding-tenant-api)). Its source node still keeps sole control of its topology forever. An existing *external* party can gain hosts and have its contracts replicated to them.
- **A run advances only while the proposer's node runs**: members co-sign on their own, so their signatures still reach the synchronizer. Nobody else proposes the next mapping or executes a contracts round. decman persists the run, so the observer resumes it when the proposer's node restarts.
- **Every invitee must be ready before a run starts**: the proposer returns 409 unless each invitee passes three tests. The invitee has vetted the coordination package, published a registry entry, and reported a high enough coordination version.
- **Legacy kick attribution**: on a party created before 2.0, a member cannot prove on-chain that the removed key belongs to the named participant. It falls back to its own cache. A party created on 2.0 uses one dual-usage key, which closes the gap.
- **Contract arguments are unchecked**: a member confirms that every root node creates a contract of an accepted package, acting as the accepted party (DLC-link/decentralization-manager#423). It does not compare the field values against anything that its operator approved.

### Daml Package Dependencies

The system depends on the following Daml packages:

| Package ID | Purpose |
|------------|---------|
| `#decman-coordination-v1` | DecmanNode, WorkflowProposal and its records, SubmissionRound, AcsManifest |
| `#governance-core-<version>` | GovernanceRules, GovernableAction interface, GenericVoteProposal |
| `#governance-token-custody-<version>` | TransferProposal, AcceptTransferProposal, preapproval proposals |
| `#governance-utility-onboarding-<version>` | SetupUtility, six granular onboarding proposals, MintProposal, BurnProposal |
| `#governance-utility-credential-<version>` | Credential domain: offer/accept free and paid credentials |
| `#utility-registry-app-v0` | ProviderService, UserService, AllocationFactory |
| `#utility-credential-app-v0` | Credential offer/accept templates |
| `#utility-commercials-v0` | DelegatedBatchedMarkersProxy (required by `CreateDelegatedBatchedMarkersProxy`) |

Package IDs prefixed with `#` use symbolic lookup (resolved at runtime by Canton).

## History

decman 1.x ran a Noise transport on TCP 9000. Nodes dialled each other, and a
coordinator pushed commands, proposals, DAR bytes, and ACS snapshots over that
channel. decman 2.0 removed it. Every operation now runs through the
synchronizer topology store and the Daml coordination package, which this
document describes.

The removal deleted these environment variables: `DECPM_LISTEN_ADDRESS`,
`DECPM_NOISE_PORT`, `DECPM_PUBLIC_ADDRESS`, `DECPM_TIMEOUT_*`,
`DECPM_NOISE_RETRY_*`, `DECPM_PEER_WAIT_POLL_DELAY_MS`, and
`DECPM_ACS_BLOCK_BYTES`. It deleted the matching CLI flags, and clap rejects an
unknown flag, so an old deployment manifest fails to start. It also deleted the
`address`, `port`, and `public_key` columns of the `peers` table, and
`GET /keys/status`.

Upgrading operators set a node identity, re-exchange peer strings, and confirm
that `GET /registry` lists each peer. See the runbook in
[NOISE_SUNSET_DESIGN.md](NOISE_SUNSET_DESIGN.md), section D12.
