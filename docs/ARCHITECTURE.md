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
| Governance actions | Configurable per `GovernanceRules` or `VaultGovernanceRules` contract |

### Key Types

The system manages three distinct key types:

| Key | Algorithm | Purpose |
|-----|-----------|---------|
| Namespace key | Ed25519 (Canton) | Signs topology proposals (DNS, P2P) |
| Daml signing key | Ed25519 (Canton) | Signs ledger transactions |
| Noise key | secp256k1 | Authenticates P2P communication between nodes |

## System Components

```
                                 Internet
                                    |
           +------------------------+------------------------+
           |                        |                        |
   +--------------+         +--------------+         +--------------+
   | Participant 1|         | Participant 2|         | Participant 3|
   |              |         |              |         |              |
   | +----------+ |  Noise  | +----------+ |  Noise  | +----------+ |
   | |HTTP :8080| |<------->| |HTTP :8080| |<------->| |HTTP :8080| |
   | |Noise:9000| |  (P2P)  | |Noise:9000| |  (P2P)  | |Noise:9000| |
   | +----+-----+ |         | +----+-----+ |         | +----+-----+ |
   |      |       |         |      |       |         |      |       |
   |      v       |         |      v       |         |      v       |
   | +----------+ |         | +----------+ |         | +----------+ |
   | |Canton    | |         | |Canton    | |         | |Canton    | |
   | |Admin API | |         | |Admin API | |         | |Admin API | |
   | |Ledger API| |         | |Ledger API| |         | |Ledger API| |
   | +----------+ |         | +----------+ |         | +----------+ |
   +--------------+         +--------------+         +--------------+
```

### HTTP Server (actix-web)

The HTTP server serves the embedded React frontend and exposes REST endpoints for managing decentralized parties. Key responsibilities:
- Serving the single-page application (embedded at compile time via `build.rs`)
- Proxying topology and governance queries to Canton APIs
- Triggering and monitoring multi-party workflows
- Managing authentication tokens via Keycloak or Auth0

Payload limit: 100 MB (for DAR file uploads).

### Noise Protocol Server

Each node runs a Noise Protocol server for secure peer-to-peer communication:

- **Handshake pattern**: `NN_PSK2` (no static keys in handshake, PSK injected at message 2)
- **PSK derivation**: ECDH shared secret from secp256k1 keys (`SharedSecret::new(peer_pubkey, our_secret)`)
- **Identity**: Peers identify via compressed secp256k1 public key (33 bytes)
- **Transport**: HTTP-over-Noise via `hyper-noise` (each message is an HTTP request/response)

The server handles two categories of connections:
1. **Heartbeat pings** -- peers ping each other every 5 seconds to track connectivity
2. **Workflow messages** -- coordinator sends commands, peers return results

### Canton gRPC Client

The application communicates with Canton via gRPC using the following services:

**Admin API services:**
| Service | Purpose |
|---------|---------|
| `TopologyManagerReadService` | Query DNS, P2P, and other topology mappings |
| `TopologyManagerWriteService` | Submit topology proposals and authorize transactions |
| `VaultService` | Manage key vaults (generate keys, sign, export) |
| `IdentityInitializationService` | Query participant ID |
| `SynchronizerConnectivityService` | Discover synchronizer IDs, disconnect and reconnect during ACS import |
| `PackageService` | Upload DAR files, list vetted packages |
| `PartyManagementService` (`com.digitalasset.canton.admin.participant.v30`) | Offline party replication: `ExportPartyAcs`, `ImportPartyAcs`, `GetHighestOffsetByTimestamp`, `ClearPartyOnboardingFlag` |

**Ledger API services:**
| Service | Purpose |
|---------|---------|
| `CommandService` | Submit and execute Daml commands |
| `StateService` | Query active contracts |
| `UserManagementService` | Query user rights |
| `PartyManagementService` (`com.daml.ledger.api.v2.admin`) | Query party metadata and annotations |
| `InteractiveSubmissionService` | Prepare and execute multi-party interactive submissions |
| `UpdateService` | Read transaction updates by offset |
| `EventQueryService` | Look up create/archive events for a contract |

### Workflow Engine

Each workflow type is modeled as a state machine with a defined step sequence. The engine:
1. Advances through steps sequentially
2. Sends commands to peers at steps that require their participation
3. Waits for all peer responses before advancing
4. Executes coordinator-only steps (proposal creation, submission) locally

## Coordinator / Peer Model

The system uses a coordinator/peer pattern for multi-party operations. Any participant can serve either role -- it is determined per-workflow, not per-node.

### Coordinator

The coordinator is the participant that initiates a workflow. Responsibilities:
- Sends invitations to selected peers via Noise protocol
- Waits for peers to accept and connect
- Orchestrates the step sequence (sends commands, collects results)
- Performs coordinator-only operations (proposal creation, Canton submissions)
- Runs a Noise server that peers poll for commands

### Peer

An peer participates in a workflow initiated by another participant. Responsibilities:
- Receives an invitation via heartbeat connection
- User accepts/declines via UI (stored as pending invitation)
- Connects to coordinator's Noise server as a client
- Polls for commands via `GetNextCommand` message
- Executes commands locally (key generation, signing)
- Sends results back to coordinator

### Invitation Flow

```
Coordinator                           Peer
    |                                     |
    |--- InviteOnboarding (Noise) ------->|
    |<-- Ack ----------------------------|
    |                                     |
    |    [User sees pending invitation    |
    |     in UI and clicks "Accept"]      |
    |                                     |
    |<-- GetNextCommand (polling) --------|
    |--- Wait / Command ----------------->|
    |<-- Data / StatusUpdate -------------|
    |    ...                              |
    |--- Disconnect --------------------->|
```

## Communication Protocol

### Wire Format

All Noise protocol messages use a binary wire format:

```
+------------------+--------------------+------------------+
| MessageType (2B) | PayloadLength (4B) | Payload (var)    |
| big-endian u16   | big-endian u32     | raw bytes        |
+------------------+--------------------+------------------+
```

Minimum message size: 6 bytes (type + length with zero payload).

### Message Categories

**Commands (0x0001 - 0x000F, 0x0020 - 0x0025):** Sent by coordinator to peers.

| Code | Name | Payload | Description |
|------|------|---------|-------------|
| 0x0001 | UploadDars | Encoded DAR files | Upload DAR files to local Canton node |
| 0x0002 | GenerateKeys | JSON OnboardingConfig | Generate namespace + Daml keys |
| 0x0003 | SignDns | Binary DNS proposal | Sign DNS topology proposal |
| 0x0004 | SignP2p | Binary P2P proposal | Sign P2P topology proposals |
| 0x0005 | SignSubmissions | Config + prepared files | Sign ledger submissions |
| 0x0006 | StatusUpdate | UTF-8 status text | Status update from peer |
| 0x0007 | Disconnect | (empty) | Workflow complete, disconnect |
| 0x0008 | GetNextCommand | (empty) | Peer polls for next command |
| 0x0009 | SignKick | Config + kick proposals | Sign kick topology proposals |
| 0x000A | Ping | (empty) | Heartbeat ping |
| 0x000B | ListPackages | (empty) | Request peer's uploaded package list |
| 0x000C | RequestOwnerKeys | (empty) | Request a peer's namespace owner keys |
| 0x000D | ListPeers | (empty) | Request a peer's known-peer list |
| 0x000E | RequestMemberParty | (empty) | Request a peer's member party |
| 0x000F | Health | (empty) | Health probe |
| 0x0020 | GenerateAddPartyKeys | JSON add-party config | New member generates its namespace + Daml keys (others skip) |
| 0x0021 | SignAddParty | Config + add-party proposals | Sign the add-party DNS + P2P proposals |
| 0x0022 | ImportAcs | ACS snapshot | New member imports the party's ACS (others skip) |
| 0x0023 | ClearOnboardingFlag | JSON clear config | New member drives `ClearPartyOnboardingFlag` (others skip) |
| 0x0024 | SignClearOnboarding | Clearing proposal or skip marker | Sign the onboarding-flag clearing proposal |
| 0x0025 | SignChangeThreshold | Config + threshold proposals | Sign the change-threshold DNS + P2P proposals |

**Invites (0x0010 - 0x001F):** Sent during heartbeat to invite peers.

| Code | Name | Description |
|------|------|-------------|
| 0x0010 | InviteOnboarding | Invite to onboarding workflow |
| 0x0011 | InviteKick | Invite to kick workflow |
| 0x0012 | InviteContracts | Invite to contracts workflow |
| 0x0013 | InviteDars | Invite to DARs upload workflow |
| 0x0014 | CancelInvite | Cancel a previously sent invitation |
| 0x0015 | RetryWorkflow | Coordinator tells peers to retry a failed run |
| 0x0016 | DeclineInvitation | Peer declines an invitation (frees coordinator's run) |
| 0x0017 | InviteAddParty | Invite to add a new member to a decentralized party |
| 0x0018 | InviteChangeThreshold | Invite to change a decentralized party's threshold |

**Responses (0x0100 - 0x01FF):** Replies from coordinator or peer.

| Code | Name | Description |
|------|------|-------------|
| 0x0101 | Ack | Acknowledgement |
| 0x0102 | Data | Generic data payload |
| 0x0103 | Error | Error message |
| 0x0104 | Ready | Peer is ready |
| 0x0105 | Wait | No command ready, poll again |
| 0x0106 | Pong | Heartbeat response |
| 0x0107 | OwnerKeys | Namespace owner keys (reply to RequestOwnerKeys) |
| 0x0108 | PeerList | Known-peer list (reply to ListPeers) |
| 0x0109 | MemberPartyResponse | Member party (reply to RequestMemberParty) |
| 0x010A | HealthResponse | Health status (reply to Health) |
| 0x010B | Busy | Node is busy with another workflow |

**Data Transfers (0x0200 - 0x02FF):** Peer data uploads to coordinator.

| Code | Name | Description |
|------|------|-------------|
| 0x0201 | KeysUpload | Generated public keys |
| 0x0202 | DnsSignature | Signed DNS proposal |
| 0x0203 | P2pSignatures | Signed P2P proposals |
| 0x0204 | SubmissionSignatures | Signed ledger submissions |
| 0x0205 | KickSignatures | Signed kick proposals |
| 0x0206 | AddPartyKeysUpload | New member's generated keys + participant id |
| 0x0207 | AddPartySignatures | Signed add-party DNS + P2P pair |
| 0x0208 | AddPartyClearSignatures | Signed onboarding-flag clearing proposal |
| 0x0209 | AddPartyClearProposal | The clearing proposal the new member authored |
| 0x020A | ChangeThresholdSignatures | Signed change-threshold DNS + P2P pair |

**Chunked Transfer (0x0300 - 0x03FF):** For payloads exceeding 1 MiB.

| Code | Name | Payload | Description |
|------|------|---------|-------------|
| 0x0300 | ChunkedCommand | command_type(2B) + total_size(4B) + chunk_count(4B) | Announce chunked transfer |
| 0x0301 | GetChunk | chunk_index(4B) | Request specific chunk |
| 0x0302 | Chunk | chunk_index(4B) + chunk_data(var) | Chunk data response |

Chunk size: 1 MiB (`CHUNK_SIZE`). Chunking is required for payloads exceeding `MAX_PAYLOAD_SIZE` (1 MiB). An assembled chunked response is capped at 16 MiB (`MAX_CHUNKED_TOTAL_SIZE`).

### Security

- **PSK derivation**: Each peer pair derives a unique PSK via secp256k1 ECDH. The coordinator's secret key and the peer's public key (or vice versa) produce a shared secret used as the Noise PSK.
- **Peer allowlist**: Only peers registered in the database can establish connections. Unknown public keys are rejected.
- **Transport encryption**: All data is encrypted by the Noise protocol after handshake completion.

## Workflows

### Onboarding (Decentralized Party Creation)

Creates a new decentralized party with multiple hosting participants.

**Steps:**

| # | Step | Actor | Description |
|---|------|-------|-------------|
| 1 | WaitingForPeers | Coordinator | Wait for all invited peers to connect |
| 2 | GenerateKeys | All | Each participant generates namespace + Daml signing keys via Canton Admin API |
| 3 | CreateProposals | Coordinator | Compute decentralized namespace hash, create DNS and P2P topology proposals |
| 4 | SignDns | All | Each participant signs the DNS proposal with their namespace key |
| 5 | SubmitDns | Coordinator | Submit signed DNS proposal to Canton, wait for topology propagation (30s) |
| 6 | SignP2p | All | Each participant signs P2P proposals with their namespace key |
| 7 | SubmitFinal | Coordinator | Re-sign the aggregate against the synchronizer store (the coordinator's namespace key only resolves once the DNS is active), verify the owner threshold is met, submit signed P2P proposals, wait for propagation |
| 8 | Complete | All | Disconnect peers, workflow finished |

**Canton API calls:**
- `VaultService.GenerateKey` -- Generate namespace and signing keys (step 2)
- `VaultService.ExportKeyPair` -- Export public keys for proposal creation (step 2)
- `TopologyManagerWriteService.Authorize` -- Sign topology proposals (steps 4, 6)
- `TopologyManagerWriteService.AddTransactions` -- Submit signed proposals (steps 5, 7)

**Minimum participants:** 2

### Kick (Remove Participant)

Removes a participant from an existing decentralized party.

**Steps:**

| # | Step | Actor | Description |
|---|------|-------|-------------|
| 1 | WaitingForPeers | Coordinator | Wait for remaining members to connect |
| 2 | ExportState | Coordinator | Export current DNS and P2P topology state |
| 3 | CreateProposals | Coordinator | Create new DNS (reduced owners) and P2P (removed member) proposals |
| 4 | SignProposals | All remaining | Each remaining member signs the kick proposals |
| 5 | SubmitKick | Coordinator | Submit signed proposals to Canton |
| 6 | Complete | All | Disconnect peers |

**Canton API calls:**
- `TopologyManagerReadService.ListDecentralizedNamespaceDefinition` -- Read current DNS (step 2)
- `TopologyManagerReadService.ListPartyToParticipant` -- Read current P2P mappings (step 2)
- `TopologyManagerWriteService.Authorize` -- Sign proposals (step 4)
- `TopologyManagerWriteService.AddTransactions` -- Submit proposals (step 5)

**Minimum participants:** 2

### Contracts (DAR Upload + Contract Creation)

Deploys DAR packages and creates Daml contracts on the ledger.

**Steps:**

| # | Step | Actor | Description |
|---|------|-------|-------------|
| 1 | WaitingForPeers | Coordinator | Wait for all participants to connect |
| 2 | UploadDars | All | Each participant uploads DAR files to their local Canton node |
| 3 | PrepareSubmissions | Coordinator | Prepare ledger command submissions from contract definitions |
| 4 | SignSubmissions | All | Each participant signs the prepared submissions |
| 5 | ExecuteSubmissions | Coordinator | Execute signed submissions on the Canton ledger |
| 6 | Complete | All | Disconnect peers |

**Canton API calls:**
- `PackageService.UploadDarFile` -- Upload DAR packages (step 2)
- `InteractiveSubmissionService.PrepareSubmission` -- Prepare ledger command submissions (step 3)
- `InteractiveSubmissionService.ExecuteSubmissionAndWaitForTransaction` -- Execute signed multi-party submissions (step 5)

**Minimum participants:** 3

### DARs (DAR Upload Only)

Uploads DAR packages to all participants without deploying contracts.

**Steps:**

| # | Step | Actor | Description |
|---|------|-------|-------------|
| 1 | WaitingForPeers | Coordinator | Wait for all participants to connect |
| 2 | UploadDars | All | Each participant uploads DAR files to their local Canton node |
| 3 | Complete | All | Disconnect peers |

**Canton API calls:**
- `PackageService.UploadDarFile` -- Upload DAR packages (step 2)

**Minimum participants:** 2

### Add Party (Add a Host to an Existing Decentralized Party)

Adds a hosting participant to a decentralized party that already exists, and
replicates the party's active contracts to it. The party keeps transacting on
its existing hosts throughout: Canton's `HostingParticipant.Onboarding` marker
suspends it only on the joining node.

**Steps:**

| # | Step | Actor | Description |
|---|------|-------|-------------|
| 1 | WaitingForPeers | Coordinator | Wait for the existing members and the joiner to connect |
| 2 | GenerateNewMemberKeys | New member | Generate namespace + Daml signing keys and upload the public halves |
| 3 | ExportState | Coordinator | Read the current DNS + P2P state, validate the add, capture the ledger offset |
| 4 | CreateProposals | Coordinator | Build the updated DNS + P2P proposals, with the joiner marked `Onboarding` |
| 5 | SignProposals | All | Every member signs both proposals with its namespace key |
| 6 | SubmitProposals | Coordinator | Submit DNS then P2P, then export the party's ACS |
| 7 | SyncAcs | New member | Disconnect from the synchronizer, import the ACS, reconnect (skipped when the ACS is empty) |
| 8 | PrepareClearOnboarding | Coordinator | Swap the command payload for the clear-flag phase |
| 9 | ProposeClearOnboarding | New member | Author the `ClearPartyOnboardingFlag` transaction past Canton's safe time |
| 10 | PrepareClearSign | Coordinator | Turn the authored proposal into a signing round (or a skip marker) |
| 11 | SignClearOnboarding | All | Every member signs the clearing proposal |
| 12 | SubmitClearOnboarding | Coordinator | Submit it and wait for the marker to drop |
| 13 | Complete | All | Disconnect peers, workflow finished |

**Canton API calls:**
- `VaultService.GenerateKey` / `ExportKeyPair` -- New member's keys (step 2)
- `TopologyManagerWriteService.Authorize` / `AddTransactions` -- Proposals (steps 5, 6, 11, 12)
- `PartyManagementService.GetHighestOffsetByTimestamp` -- Capture the export offset (step 3)
- `PartyManagementService.ExportPartyAcs` -- Export the snapshot, scoped to the joiner (step 6)
- `SynchronizerConnectivityService.DisconnectSynchronizer` / `ReconnectSynchronizers` -- Bracket the import (step 7)
- `PartyManagementService.ImportPartyAcs` -- Import the snapshot (step 7)
- `PackageService.ListPackages` -- Preflight the joiner's vetted packages before the import (step 7)
- `PartyManagementService.ClearPartyOnboardingFlag` -- Clear the marker (steps 9, 12)

**Minimum participants:** 2 (an existing member + the joiner)

**Restriction:** the workflow requires a `DecentralizedNamespaceDefinition` and a
Noise quorum of member nodes. It cannot add a host to a local party or to an
external party. It fails at `ExportState` for any other party type.

### Change Threshold

Changes the signing threshold of an existing decentralized party's namespace.

**Steps:**

| # | Step | Actor | Description |
|---|------|-------|-------------|
| 1 | WaitingForPeers | Coordinator | Wait for the party's members to connect |
| 2 | ExportState | Coordinator | Read the current DNS state and validate the new threshold |
| 3 | CreateProposals | Coordinator | Build the new-threshold DNS + P2P proposals |
| 4 | SignProposals | All | Every member signs the proposals |
| 5 | Submit | Coordinator | Submit the change and wait for propagation |
| 6 | Complete | All | Disconnect peers |

**Canton API calls:**
- `TopologyManagerWriteService.Authorize` -- Sign the proposals (step 4)
- `TopologyManagerWriteService.AddTransactions` -- Submit the change (step 5)

**Minimum participants:** 2 (party members)

### External Party Onboarding (Tenant API)

Creates a **co-validated** party: hosted on several participants at once but
controlled by a single Ed25519 key its owner holds. This is not a Noise
workflow. It is stateless HTTP, driven by the wallet, with Canton itself as the
only coordination store.

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

A legacy `VaultGovernanceRules` contract (from the `bitsafe-vault-governance` package) is also supported for backward compatibility with existing vault deployments.

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

**Token issuance:**

| Template | Action Label | Description |
|----------|-------------|-------------|
| `MintProposal` | `Mint` | Offer a mint to a specific recipient via `AllocationFactory_OfferMint`. The recipient accepts the resulting `MintOffer` outside the plugin. Proposal carries `allocationFactoryCid`, `instrumentId`, and `instrumentConfigurationCid` directly. |
| `BurnProposal` | `Burn` | Offer a burn against a specific holder via `AllocationFactory_OfferBurn`. The holder accepts the resulting `BurnOffer` outside the plugin. Same CID fields as `MintProposal`. |

`MintProposal` and `BurnProposal` enforce `amount > 0.0` at the template-precondition level.

**Prerequisite.** `SetupUtility` consumes an existing `ProviderService` for the governance party. Use `ProvisionProviderService` to create one through the governance flow — direct creation of `ProviderService` via `POST /contracts` or a multi-party daml-script submit fails on Canton when the governance party is externally signed, because `ProviderService` has two signatories (`operator, provider`).

### Legacy: VaultGovernanceRules

The `VaultGovernanceRules` contract (from `BitsafeVault.VaultGovernance`, package `#bitsafe-vault-governance-v0-rc8`) is the original governance primitive used for vault operations. It remains supported for backward compatibility with existing vault deployments.

```
VaultGovernanceRules {
    vaultManager : Party            -- The decentralized party
    members      : [Party]          -- Member parties authorized to vote
    threshold    : Int              -- Minimum confirmations required
    actionConfirmationTimeout : Optional RelTime  -- Auto-expiry for stale confirmations
}
```

Unlike the modular `GovernanceRules`, `VaultGovernanceRules` uses a monolithic design where all action types are encoded as variants of a closed `ActionRequiringConfirmation` enum. Confirmations are matched by value equality rather than `ContractId`.

Choices on `VaultGovernanceRules`:
- `VaultGovernanceRules_ConfirmAction` -- Submit a confirmation for an action
- `VaultGovernanceRules_ExecuteConfirmedAction` -- Execute when threshold is met
- `VaultGovernanceRules_ExpireConfirmation` -- Remove a stale confirmation

#### Vault Action Types

The vault governance system supports 21 action types across 7 categories:

**Governance (6 actions):**

| Action | Parameters | Description |
|--------|------------|-------------|
| `GovernanceAddMember` | member, new_threshold | Add a new governance member |
| `GovernanceRemoveMember` | member, new_threshold | Remove a governance member |
| `GovernanceSetThreshold` | new_threshold | Change the approval threshold |
| `GovernanceSetTimeout` | new_timeout_microseconds | Set confirmation expiry timeout |
| `GovernanceAddAdditionalProposer` | additional_proposer | Grant propose-only rights to a non-member party |
| `GovernanceRemoveAdditionalProposer` | additional_proposer | Revoke propose-only rights from a party |

**Vault Deployment (2 actions):**

| Action | Parameters | Description |
|--------|------------|-------------|
| `VaultDeployment` | vault_rules_cid, vault_name, share_symbol, asset_instrument_id, limits, vault_backend_signatory, vault_far_config, allocation_factory_cid, registrar_service_cid | Deploy a new BitsafeVault |
| `YieldEpochDeployment` | vault_rules_cid, vault_cid, asset_instrument_id, vault_backend_signatory | Deploy a yield epoch |

**Vault Operations (5 actions):**

| Action | Parameters | Description |
|--------|------------|-------------|
| `VaultPause` | vault_id | Pause vault operations |
| `VaultUnpause` | vault_id | Resume vault operations |
| `VaultUpdateLimits` | vault_id, new_limits | Update deposit/withdrawal limits |
| `VaultUpdateBackend` | vault_id, new_backend_signatory | Change backend signatory |
| `VaultUpdateFarBeneficiaries` | vault_id, new_beneficiaries | Update FAR reward distribution |

**Processor (1 action):**

| Action | Parameters | Description |
|--------|------------|-------------|
| `ProcessorDeploymentRequest` | vault_processor_rules_cid, vault_backend_signatory, allocation_factory_cid, processor_far_config, initial_supported_vaults | Deploy a vault processor |

**Utility Onboarding (4 actions):**

| Action | Parameters | Description |
|--------|------------|-------------|
| `UtilityCreateProviderRequest` | operator | Create a ProviderService |
| `UtilityCreateUserRequest` | operator | Create a UserService |
| `UtilitySetup` | operator, provider_service_cid, user_service_cid | Link provider and user services |
| `UtilityAcceptHolderServiceRequest` | operator, provider_service_cid, holder_service_request_cid, holder | Accept a holder service request |

**Credentials (2 actions):**

| Action | Parameters | Description |
|--------|------------|-------------|
| `CredentialOfferFree` | operator, user_service_cid, holder, id, description, claims | Offer a free credential |
| `CredentialAcceptFree` | operator, user_service_cid, credential_offer_cid | Accept a free credential |

**DevNet (1 action):**

| Action | Parameters | Description |
|--------|------------|-------------|
| `DevNetFeatureApp` | amulet_rules_cid | Register as featured app in Amulet ecosystem |

### Featured App Rewards (FAR)

FAR is a reward distribution mechanism for featured application participants in the Amulet ecosystem:

```json
{
    "featured_app_right_cid": "<contract-id>",
    "beneficiaries": [
        { "beneficiary": "party::1220abc...", "weight": "0.50" },
        { "beneficiary": "party::1220def...", "weight": "0.30" },
        { "beneficiary": "party::1220ghi...", "weight": "0.20" }
    ]
}
```

FAR configuration is used in:
- `VaultDeployment` -- initial FAR setup for a new vault
- `ProcessorDeploymentRequest` -- FAR for processor rewards
- `VaultUpdateFarBeneficiaries` -- update beneficiaries and weights for an existing vault

## Technical Constraints

### Infrastructure Requirements

- **Canton Admin API access required**: The application needs access to privileged Admin API endpoints (topology management, key vaults, package upload). This is not the public Ledger API -- it requires high node-level privileges.
- **7 Admin API services used**: TopologyManagerRead, TopologyManagerWrite, Vault, IdentityInitialization, SynchronizerConnectivity, PackageService, PartyManagement
- **Canton protocol version**: 35 (hardcoded for key export and topology operations)
- **Network ports**: TCP 8080 (HTTP server) + TCP 9000 (Noise P2P)

### Timing Constraints

- **Topology propagation delay**: 30 seconds after the effective time of a topology change before it can be used. Without this wait, transactions may be rejected with `LOCAL_VERDICT_TIMEOUT`.
- **Topology retry settings**: 30 attempts with 2-second delays when polling for topology state changes
- **Heartbeat interval**: 5-second ping cycle for peer connectivity monitoring
- **Noise timeouts**: 10-second request timeout, 25-second chunk-fetch timeout, 45-second handler timeout, 30-second handshake timeout (configurable), 120-second message timeout (configurable)

### Participant Minimums

| Workflow | Minimum Participants |
|----------|---------------------|
| Onboarding | 2 |
| Kick | 2 (remaining members) |
| Contracts | 3 |
| DARs | 2 |
| Add Party | 2 (an existing member + the joiner) |
| Change Threshold | 2 (party members) |

### Known Limitations

- **ACS sync for existing contracts**: Adding a new member to a party that already has active contracts requires Active Contract Set (ACS) export/import. The add-party workflow does this with Canton offline party replication (`ExportPartyAcs` / `ImportPartyAcs` plus the `HostingParticipant.Onboarding` marker), so no repair mode and no participant restart are needed. The importing node does disconnect from the synchronizer for the duration of the import, which briefly pauses that node; the party keeps transacting on its other hosts. If the party has no active contracts, the workflow skips the sync.
- **Add-party is decentralized-party only**: The workflow requires a `DecentralizedNamespaceDefinition` and a Noise quorum of member nodes. It cannot add a host to a local or an already-onboarded external party. See [Canton Party Replication](CANTON_PARTY_REPLICATION.md).
- **External parties are onboarded fresh, not decentralized in place**: The tenant API creates a co-validated party from a key the wallet already holds. It replicates identity only, never state, so it cannot take over a party that already holds contracts.
- **ACS transfer size**: The snapshot travels over the Noise chunked-transfer path, which caps an assembled response at 16 MiB (`MAX_CHUNKED_TOTAL_SIZE`). A party with a large ACS exceeds this.
- **Coordinator single point of progress**: A workflow makes no progress while its coordinator is offline. The run is persisted, so the coordinator resumes it on restart; peers retry 3 times before aborting.

### Daml Package Dependencies

The system depends on the following Daml packages:

| Package ID | Purpose |
|------------|---------|
| `#governance-core-<version>` | GovernanceRules, GovernableAction interface, GenericVoteProposal |
| `#governance-token-custody-<version>` | TransferProposal, AcceptTransferProposal, preapproval proposals |
| `#governance-utility-onboarding-<version>` | SetupUtility, six granular onboarding proposals, MintProposal, BurnProposal |
| `#governance-utility-credential-<version>` | Credential domain: offer/accept free and paid credentials |
| `#bitsafe-vault-governance-v0-rc8` | Legacy VaultGovernanceRules contract templates |
| `#bitsafe-vault-v0-rc8` | VaultRules and Vault contract templates |
| `#utility-registry-app-v0` | ProviderService, UserService, AllocationFactory |
| `#utility-credential-app-v0` | Credential offer/accept templates |
| `#utility-commercials-v0` | DelegatedBatchedMarkersProxy (required by `CreateDelegatedBatchedMarkersProxy`) |

Package IDs prefixed with `#` use symbolic lookup (resolved at runtime by Canton).
