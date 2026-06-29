# Canton Party Replication & ACS Synchronization

> **Historical reference.** This survey was collected from the abandoned
> PR #27 (February 2026) and reflects the Canton 3.3-era guidance, under
> which ACS import required repair mode, a participant restart, and a
> synchronizer disconnect. **The add-party workflow implemented in this
> repository does NOT work that way**: it uses Canton 3.4 offline party
> replication (`PartyManagementService.ExportPartyAcs` / `ImportPartyAcs`)
> with the `HostingParticipant.Onboarding` marker and
> `ClearPartyOnboardingFlag` — no repair mode, no restart, and the brief
> synchronizer disconnect the import still requires is automated by the
> workflow. See
> [ADD_PARTY_REDEVELOPMENT.md](ADD_PARTY_REDEVELOPMENT.md) §6 for the
> implemented design; keep this document only as background on the
> replication problem space.

## Overview

This document details the requirements and procedures for adding new members to a decentralized party in Canton, including Active Contract Set (ACS) synchronization.

**Critical Finding**: According to Canton documentation, adding a member to `PartyToParticipant` requires more than just topology transactions:

> "Adding a member to `PartyToParticipant` requires not just a topology transaction but a **full party migration including an ACS export and import**."
>
> — [Decentralized party overview](https://docs.digitalasset.com/operate/3.3/howtos/operate/parties/decentralized_parties.html)

---

## Table of Contents

1. [When is ACS Sync Required?](#when-is-acs-sync-required)
2. [Replication Methods](#replication-methods)
3. [Simple Replication (No ACS Required)](#simple-replication-no-acs-required)
4. [Offline Party Replication (ACS Required)](#offline-party-replication-acs-required)
5. [Live Sync Alternatives](#live-sync-alternatives)
6. [gRPC API Reference](#grpc-api-reference)
7. [Implementation Considerations](#implementation-considerations)
8. [Decision Matrix](#decision-matrix)

---

## When is ACS Sync Required?

| Scenario | ACS Sync Required | Method |
|----------|-------------------|--------|
| Party has **no active contracts** | No | Simple Replication |
| Party has **existing contracts** | **Yes** | Offline Party Replication |
| Multi-hosting from initial creation | No | Simple Replication |
| Adding member to existing party with contracts | **Yes** | Offline Party Replication |

### Why ACS Sync is Necessary

When a new participant joins a decentralized party that already has active contracts:

1. The new participant has **no visibility** into existing contracts
2. Cannot participate in transactions involving those contracts
3. Cannot properly confirm/reject transactions
4. Will have an **inconsistent ledger view**

---

## Replication Methods

Canton supports two primary replication approaches:

### 1. Simple Replication
- For parties **before** they become stakeholders in any contract
- No ACS export/import needed
- No repair mode required
- No participant restart needed

### 2. Offline Party Replication
- For parties that have **already participated** in Daml transactions
- Requires ACS export from source → import to target
- Target participant must run in **repair mode**
- Target must disconnect from synchronizers during import

---

## Simple Replication (No ACS Required)

Use this method when adding a member to a decentralized party that has **no existing contracts**.

### Procedure

1. **Generate keys** for the new member (namespace + DAML signing keys)
2. **Create namespace delegation** for the new member
3. **Update DecentralizedNamespaceDefinition** (add new owner)
4. **Update PartyToParticipant** (add new participant)
5. **Collect threshold signatures** from existing members
6. **Submit topology transactions**

### When to Use

- Initial party creation with multiple hosts
- Adding member before any contracts are deployed
- Adding member to a party that only has vetting/topology state but no contracts

---

## Offline Party Replication (ACS Required)

Use this method when adding a member to a party with **existing active contracts**.

### Prerequisites

1. **Enable repair mode** on target participant:
   ```toml
   # In canton config
   canton.features.enable-repair-commands = true
   ```
   ⚠️ **Requires participant restart**

2. **Vet all packages** on target participant where the party is a stakeholder

3. **Backup target participant** after replication completes (critical!)

### Method A: Permission Change Procedure (Recommended)

This is the **least disruptive** method for live systems.

#### Step 1: Authorize with Observation Permission Only
```scala
source.topology.party_to_participant_mappings.propose_delta(
  party = decentralizedParty,
  adds = Seq(target.id -> ParticipantPermission.Observation),
  store = synchronizerId,
)
```

#### Step 2: Wait for Topology to Become Effective
Wait for the decision timeout (sum of `confirmationResponseTimeout` + `mediatorReactionTimeout`).

#### Step 3: Find Activation Offset
```scala
val activationOffset = source.parties.find_party_max_activation_offset(
  partyId = decentralizedParty,
  participantId = target.id,
  synchronizerId = synchronizerId,
  beginOffsetExclusive = beforeActivationOffset,
  completeAfter = PositiveInt.one,
)
```

#### Step 4: Export ACS from Source
```scala
source.parties.export_acs(
  parties = Set(decentralizedParty),
  exportFilePath = "party_replication.acs.gz",
  ledgerOffset = activationOffset,
)
```

#### Step 5: Transfer ACS File
Securely transfer the ACS snapshot file to the target participant.

#### Step 6: Disconnect Target from Synchronizers
```scala
target.synchronizers.disconnect_all()
```

#### Step 7: Import ACS to Target
```scala
target.repair.import_acs("party_replication.acs.gz")
```

#### Step 8: Reconnect Target
```scala
target.synchronizers.reconnect_local("synchronizerId")
```

#### Step 9: Upgrade Permission to Confirmation
```scala
source.topology.party_to_participant_mappings.propose_delta(
  party = decentralizedParty,
  adds = Seq(target.id -> ParticipantPermission.Confirmation),
  store = synchronizerId,
)
```

### Method B: Silent Synchronizer Procedure (Safest, Requires Downtime)

This method requires a **maintenance window** and is suitable for private/controlled synchronizers.

#### Step 1: Silence the Synchronizer
```scala
sequencer.topology.synchronizer_parameters.propose_update(
  synchronizerId,
  _.update(confirmationRequestsMaxRate = NonNegativeInt.zero)
)
```

#### Step 2: Enable Repair Mode on Both Participants
Both source and target must have:
```toml
canton.features.enable-repair-commands = true
canton.features.enable-testing-commands = true
```

#### Step 3: Use Repair Macros
```scala
// Step 1: Hold and export ACS
repair.party_replication.step1_hold_and_store_acs(
  partyId = decentralizedParty,
  synchronizerId = synchronizerId,
  sourceParticipant = source,
  targetParticipant = target,
  exportFilePath = "acs.gz",
  beginOffsetExclusive = beforeActivationOffset
)

// Step 2: Import ACS
repair.party_replication.step2_import_acs(
  partyId = decentralizedParty,
  synchronizerId = synchronizerId,
  targetParticipant = target,
  importFilePath = "acs.gz"
)
```

#### Step 4: Unsilence Synchronizer
```scala
sequencer.topology.synchronizer_parameters.propose_update(
  synchronizerId,
  _.update(confirmationRequestsMaxRate = NonNegativeInt.maxValue)
)
```

---

## Live Sync Alternatives

### Can We Avoid Repair Mode / Restart?

**Short answer: No, for parties with existing contracts.**

According to Canton documentation:

> "Canton's facilities for importing an ACS are only available when the target participant runs in **repair mode**, and switching a participant's repair mode requires a participant restart."
>
> — [Party replication](https://docs.digitalasset.com/operate/3.3/howtos/operate/parties/party_replication.html)

### Available Alternatives

#### 1. Pre-emptive Multi-Hosting (Best Option)

**Host the party on multiple participants from the start**, before any contracts are created.

- No ACS sync ever needed
- All participants have full visibility from the beginning
- Recommended for production deployments where future scaling is anticipated

#### 2. Observation-First Approach

Add new members with **Observation permission** initially:

- New member can see new contracts as they're created
- Cannot see pre-existing contracts without ACS import
- Can upgrade to Confirmation later after ACS import
- Useful if partial visibility is acceptable initially

#### 3. Contract Migration via Daml

Instead of ACS import, **archive and recreate contracts**:

- Create new contracts with the new participant as stakeholder
- Archive old contracts
- Requires application-level orchestration
- Only works if contract migration is semantically acceptable

### Canton 3.4+ Improvements

Canton 3.4 introduced `ExportPartyAcs` endpoint with improvements:

- Automatically finds correct ledger offset (party activation on target)
- Excludes contracts where stakeholders already exist on target (prevents duplication)
- Better filtering and validation during import

However, **repair mode is still required** for ACS import.

---

## gRPC API Reference

The `canton-proto-rs` crate provides these services:

### ParticipantRepairServiceClient

Located at: `com.digitalasset.canton.admin.participant.v30`

#### Export ACS
```rust
// Request
struct ExportAcsRequest {
    parties: Vec<String>,
    filter_synchronizer_id: String,
    timestamp: Option<Timestamp>,
    contract_synchronizer_renames: HashMap<String, String>,
    parties_offboarding: bool,
}

// Response
struct ExportAcsResponse {
    chunk: Vec<u8>,  // Streamed chunks of gzipped ACS data
}
```

#### Import ACS
```rust
// Request (streamed)
struct ImportAcsRequest {
    acs_chunk: Vec<u8>,
    workflow_id_prefix: String,
    allow_contract_id_suffix_recomputation: bool,
}

// Response
struct ImportAcsResponse {
    contract_id: HashMap<String, String>,
}
```

#### Export Party ACS (Canton 3.4+)
```rust
// New party-focused export
struct ExportPartyAcsRequest {
    parties: Vec<String>,
    target_participant_id: String,
    synchronizer_id: String,
    ledger_offset: Option<i64>,
}

struct ExportPartyAcsResponse {
    chunk: Vec<u8>,
}
```

#### Import Party ACS (Canton 3.4+)
```rust
struct ImportPartyAcsRequest {
    acs_chunk: Vec<u8>,
    workflow_id_prefix: String,
    representative_package_id_override: HashMap<String, String>,
    contract_import_mode: i32,  // 0: no validation, 1: full, 2: recompute
}

struct ImportPartyAcsResponse {
    contract_id: HashMap<String, String>,
}
```

### PartyManagementServiceClient

For querying party information and activation offsets.

```rust
// Add party async (for observation-first approach)
struct AddPartyAsyncRequest {
    party_id: String,
    participant_id: String,
    synchronizer_id: String,
}

// Get status of async party addition
struct GetAddPartyStatusRequest {
    request_id: String,
}
```

---

## Implementation Considerations

### For dec-party-manager Add Party Workflow

#### Current Implementation
1. ✅ Generate keys for new member
2. ✅ Create namespace delegation
3. ✅ Update DecentralizedNamespaceDefinition
4. ✅ Update PartyToParticipant
5. ✅ Collect threshold signatures
6. ✅ Submit topology transactions

#### Missing Steps (For Parties with Contracts)
7. ❌ Check if party has existing contracts
8. ❌ If yes, perform ACS export from coordinator
9. ❌ Transfer ACS snapshot to new member
10. ❌ New member: enable repair mode (requires restart)
11. ❌ New member: disconnect from synchronizers
12. ❌ New member: import ACS
13. ❌ New member: reconnect to synchronizers

### Recommended Approach

```
┌─────────────────────────────────────────────────────────────────┐
│                    Add Party Workflow                           │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  1. Check if party has active contracts                         │
│     │                                                           │
│     ├─── NO contracts ──► Simple Replication (current impl)     │
│     │                                                           │
│     └─── HAS contracts ──► ACS Replication Flow:                │
│                            │                                    │
│                            ├── a) New member enables repair     │
│                            │      mode and restarts             │
│                            │                                    │
│                            ├── b) Topology transactions         │
│                            │      (Observation permission)      │
│                            │                                    │
│                            ├── c) Wait for activation           │
│                            │                                    │
│                            ├── d) Export ACS from coordinator   │
│                            │                                    │
│                            ├── e) Transfer via Noise protocol   │
│                            │                                    │
│                            ├── f) New member disconnects        │
│                            │                                    │
│                            ├── g) Import ACS                    │
│                            │                                    │
│                            ├── h) Reconnect                     │
│                            │                                    │
│                            └── i) Upgrade to Confirmation       │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

### User Experience Implications

1. **Restart Required**: New member must restart with repair mode
2. **Coordination**: Must coordinate timing of ACS export/import
3. **Downtime**: New member experiences brief downtime during import
4. **File Transfer**: ACS snapshot must be securely transferred

---

## Decision Matrix

| Factor | Simple Replication | Permission Change | Silent Synchronizer |
|--------|-------------------|-------------------|---------------------|
| Party has contracts | ❌ Not supported | ✅ Supported | ✅ Supported |
| Requires restart | ❌ No | ✅ Target only | ✅ Both |
| Requires downtime | ❌ No | ⚠️ Target brief | ✅ Full sync window |
| Synchronizer control needed | ❌ No | ❌ No | ✅ Yes |
| Complexity | Low | Medium | High |
| Best for | New parties | Production | Private sync |

---

## References

- [Party Replication Documentation](https://docs.digitalasset.com/operate/3.3/howtos/operate/parties/party_replication.html)
- [Decentralized Party Overview](https://docs.digitalasset.com/operate/3.3/howtos/operate/parties/decentralized_parties.html)
- [Topology Management](https://docs.digitalasset.com/overview/3.4/explanations/canton/topology.html)
- [Repairing Participant Nodes](https://docs.digitalasset.com/operate/3.4/howtos/recover/repairing.html)
- [Canton 3.4 Release Notes](https://blog.digitalasset.com/developers/release-notes/canton-3.4-release-notes-for-splice-0.5.0)
- [ParticipantPartiesAdministrationGroup](https://docs.digitalasset.com/operate/3.4/scaladoc/com/digitalasset/canton/console/commands/ParticipantPartiesAdministrationGroup.html)

---

## Appendix: Canton Proto Structures

### Available in canton-proto-rs v3.4.0

```
com.digitalasset.canton.admin.participant.v30:
├── participant_repair_service_client
├── party_management_service_client
├── ExportAcsRequest / ExportAcsResponse
├── ImportAcsRequest / ImportAcsResponse
├── ExportAcsOldRequest / ExportAcsOldResponse (deprecated)
├── ImportAcsOldRequest / ImportAcsOldResponse (deprecated)
├── ExportPartyAcsRequest / ExportPartyAcsResponse (Canton 3.4+)
├── ImportPartyAcsRequest / ImportPartyAcsResponse (Canton 3.4+)
├── ExportAcsTargetSynchronizer
├── AddPartyAsyncRequest / AddPartyAsyncResponse
├── GetAddPartyStatusRequest / GetAddPartyStatusResponse
├── RepairCommitmentsStatus
└── RepairCommitmentsUsingAcsRequest / RepairCommitmentsUsingAcsResponse
```

---

*Last updated: 2026-02-05*
*Based on Canton 3.4 documentation*
