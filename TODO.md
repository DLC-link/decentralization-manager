# Canton to Rust Port - Implementation Plan

## Overview

This document outlines the plan to port Canton Scala scripts from `../canton/releases/0.0.1/` to Rust in the `grpc-test` repository. The Canton scripts implement a multi-party decentralized namespace setup for CBTC (Canton-based Bitcoin) governance.

## Workflow Architecture

The workflow implements a multi-attestor (3+ participants) decentralized party setup with threshold signatures. The process has 5 main phases:

### Phase 1: Setup and Key Generation (Step 1)
Multiple attestors independently generate cryptographic keys and upload DAR files.

### Phase 2: Proposal Creation (Step 1a)
A coordinator collects all attestor keys and creates topology proposals for the decentralized namespace.

### Phase 3: Topology Signing and Submission (Steps 2-3a)
Attestors sign topology proposals, coordinator aggregates signatures and submits to Canton.

### Phase 4: Ledger Submission Preparation (Step 3b)
Coordinator prepares ledger submissions for governance contracts.

### Phase 5: Ledger Signing and Execution (Steps 4-5)
Attestors sign ledger submissions, coordinator executes them on the ledger.

---

## Detailed Step Breakdown

### Step 1: Initial Setup

#### 00_UploadDars.sc - Upload DAR Files
**Purpose**: Upload CBTC application DARs to Canton participant nodes

**Operations**:
- Upload `cbtc-1.0.0.dar` to participant
- Upload `cbtc-governance-1.0.0.dar` to participant

**Canton API**:
- Scala: `participant.dars.upload(path)`
- Rust: gRPC `ParticipantAdministrationService.UploadDar()`

**Inputs**:
- DAR files from `./dars/` directory

**Outputs**:
- None (DARs registered in participant)

**Notes**:
- Run on each attestor participant (3+ times with different configs)
- Must complete before key generation

---

#### 01_GenerateKeys.sc - Generate Cryptographic Keys
**Purpose**: Generate signing keys and export participant identity

**Operations**:
1. Generate namespace signing key (`cbtc-network-namespace`)
   - Usage: `NamespaceOnly`
2. Create namespace from key fingerprint
3. Get synchronizer ID for "global" synchronizer
4. Propose namespace delegation with generated key
5. Generate DAML signing key (`cbtc-network-daml-transactions`)
   - Usage: `ProtocolOnly`
6. Export keys to `attestor-public-keys.bin` (2 keys)
7. Export participant ID to `participant-id.bin`

**Canton API**:
- Scala: `participant.keys.secret.generate_signing_key(name, usage)`
- Rust: gRPC `KeyAdministrationService.GenerateSigningKey()`
- Scala: `participant.topology.namespace_delegations.propose_delegation()`
- Rust: gRPC `TopologyManagerWriteService.Authorize()`

**Inputs**:
- None (generates new keys)

**Outputs**:
- `attestor-public-keys.bin`: 2 protobuf SigningPublicKey messages
- `participant-id.bin`: participant ID as UTF-8 string

**Notes**:
- Run on each attestor participant independently
- Keys are stored in participant's key store
- Output files must be collected and organized by attestor index

---

### Step 1a: Proposal Creation

#### 01a_CreateProposals.sc - Create Topology Proposals
**Purpose**: Aggregate all attestor keys and create decentralized topology proposals

**Operations**:
1. Discover and load all attestor key files from `./keys/attestor-public-keys-*.bin`
2. Parse and validate key pairs (namespace key, DAML key)
3. Extract namespaces from namespace keys
4. Discover and load all participant IDs from `./ids/participant-id-*.bin`
5. Calculate threshold as majority: `max(1, (count + 1) / 2)`
6. Create Decentralized Namespace Definition (DNS):
   - Compute namespace from all individual namespaces
   - Set threshold
   - Define owner namespaces
7. Create Party-to-Participant (P2P) mapping proposal:
   - Party ID: `cbtc-network::<computed-namespace>`
   - Map to all participant IDs with `Confirmation` permission
   - Set threshold
8. Create Party-to-Key (PTK) mapping proposal:
   - Map party to all DAML keys
   - Set threshold
9. Save proposals to files

**Canton API**:
- Scala: `participant.topology.decentralized_namespaces.propose()`
- Rust: gRPC `TopologyManagerWriteService.Authorize()` with `DecentralizedNamespaceDefinition`
- Scala: `participant.topology.party_to_participant_mappings.propose()`
- Rust: gRPC `TopologyManagerWriteService.Authorize()` with `PartyToParticipant`
- Scala: `participant.topology.party_to_key_mappings.propose()`
- Rust: gRPC `TopologyManagerWriteService.Authorize()` with `PartyToKeyMapping`

**Inputs**:
- `./keys/attestor-public-keys-{0,1,2,...}.bin`: Key pairs from each attestor
- `./ids/participant-id-{0,1,2,...}.bin`: Participant IDs

**Outputs**:
- `../step_2/dns_proto.bin`: DNS proposal (SignedTopologyTransaction)
- `../step_3/p2p_proto.bin`: P2P proposal (SignedTopologyTransaction)
- `../step_3/ptk_proto.bin`: PTK proposal (SignedTopologyTransaction)
- `../step_2a/namespaceDef.bin`: Namespace definition (for verification)

**Notes**:
- Run once by coordinator on any participant with access to all keys
- `mustFullyAuthorize = false` because we need multi-party signatures

---

### Step 2: DNS Proposal Signing

#### 02_SignProposals.sc - Sign DNS Proposal
**Purpose**: Each attestor signs the DNS proposal with their namespace key

**Operations**:
1. Get synchronizer ID for "global"
2. Read DNS proposal from `dns_proto.bin`
3. Deserialize `SignedTopologyTransaction`
4. Extract `DecentralizedNamespaceDefinition` mapping
5. Sign the transaction with participant's keys
6. Save signed transaction

**Canton API**:
- Scala: `participant.topology.transactions.sign(Seq(transaction), store)`
- Rust: gRPC `TopologyManagerReadService.ExportTopologySnapshot()` + crypto signing

**Inputs**:
- `dns_proto.bin`: Unsigned DNS proposal

**Outputs**:
- `signed-dns-proposal.bin`: DNS proposal with attestor's signature

**Notes**:
- Run on each attestor participant (except coordinator who created it)
- Each attestor produces their own signed version
- Files must be collected with unique names: `signed-dns-proposal-{1,2,...}.bin`

---

### Step 2a: DNS Proposal Submission

#### 02a_SubmitProposals.sc - Submit Aggregated DNS Proposal
**Purpose**: Aggregate all DNS signatures and submit to topology

**Operations**:
1. Get synchronizer ID for "global"
2. Read original DNS proposal from `dns_proto.bin`
3. Discover all signed DNS proposal files in `./signed-proposals/`
4. Load and deserialize all signed DNS proposals
5. Aggregate all signatures into one transaction: `dns_final = dns + sig1 + sig2 + ...`
6. Submit aggregated transaction to topology store
7. Wait until DNS appears in topology (retry until visible)

**Canton API**:
- Scala: `transaction.addSignaturesFromTransaction(other)`
- Rust: Manual signature aggregation in protobuf
- Scala: `participant.topology.transactions.load(Seq(transaction), store)`
- Rust: gRPC `TopologyManagerWriteService.Authorize()`
- Scala: `participant.topology.decentralized_namespaces.list()`
- Rust: gRPC `TopologyManagerReadService.ListDecentralizedNamespaceDefinition()`

**Inputs**:
- `dns_proto.bin`: Original proposal
- `./signed-proposals/signed-dns-proposal-{1,2,...}.bin`: Signed proposals from attestors
- `namespaceDef.bin`: For verification

**Outputs**:
- None (topology updated in Canton)

**Notes**:
- Run once by coordinator
- Must wait for topology propagation before proceeding

---

### Step 3: P2P/PTK Proposal Signing

#### 03_SignP2PPTKProposals.sc - Sign P2P and PTK Proposals
**Purpose**: Each attestor signs both P2P and PTK proposals

**Operations**:
1. Get synchronizer ID for "global"
2. Read P2P proposal from `p2p_proto.bin`
3. Deserialize and extract `PartyToParticipant` mapping
4. Read PTK proposal from `ptk_proto.bin`
5. Deserialize and extract `PartyToKeyMapping` mapping
6. Sign both transactions
7. Save both signed transactions to one file

**Canton API**:
- Scala: `participant.topology.transactions.sign(Seq(p2p, ptk), store)`
- Rust: gRPC signing for both transactions

**Inputs**:
- `p2p_proto.bin`: Unsigned P2P proposal
- `ptk_proto.bin`: Unsigned PTK proposal

**Outputs**:
- `signed-p2p-ptk-proposals.bin`: Both proposals with attestor's signatures (2 messages)

**Notes**:
- Run on each attestor participant (except coordinator)
- Files collected as: `signed-p2p-ptk-proposals-{1,2,...}.bin`

---

### Step 3a: Submit Final Proposals

#### 03a_SubmitFinalProposals.sc - Submit Aggregated P2P and PTK Proposals
**Purpose**: Similar to Step 2a but for P2P and PTK proposals

**Operations**:
1. Read original P2P and PTK proposals
2. Discover signed proposal files in `./signed-proposals/`
3. Load and parse all signed proposals (2 transactions per file)
4. Aggregate signatures for P2P proposal
5. Aggregate signatures for PTK proposal
6. Submit both aggregated transactions
7. Wait for both to appear in topology

**Canton API**:
- Similar to Step 2a but for different topology types
- Scala: `participant.topology.party_to_participant_mappings.list()`
- Rust: gRPC `TopologyManagerReadService.ListPartyToParticipant()`
- Scala: `participant.topology.party_to_key_mappings.list()`
- Rust: gRPC `TopologyManagerReadService.ListPartyHostingLimits()` (check Canton docs for correct method)

**Inputs**:
- `p2p_proto.bin`: Original P2P proposal
- `ptk_proto.bin`: Original PTK proposal
- `./signed-proposals/signed-p2p-ptk-proposals-{1,2,...}.bin`: Signed proposals
- `namespaceDef.bin`: For party ID construction

**Outputs**:
- None (topology updated)

**Notes**:
- Run once by coordinator
- At this point, the decentralized party is fully configured in topology

---

### Step 3b: Prepare Ledger Submissions

#### 03b_PrepareSubmissions.sc - Prepare Ledger Submissions
**Purpose**: Prepare 3 contract creation submissions for the decentralized party

**Operations**:
1. Find party ID for "cbtc-network" (the decentralized party)
2. Find party IDs for attestors (attestor-1, attestor-2, attestor-3)
3. Find operator party ID
4. Define threshold (e.g., 2 of 3)
5. Build Daml `Record` arguments for contracts:
   - **Submission 1**: Create `CBTCGovernanceRules` contract
     - Fields: registrar (decentralized party), operator, instrument, attestors map, threshold
   - **Submission 2**: Create `CBTCDepositAccountRules` contract
     - Fields: registrar, operator, instrument
   - **Submission 3**: Create `CBTCWithdrawAccountRules` contract
     - Fields: registrar, operator, instrument
6. Call `prepare()` for each submission to get prepared transactions
7. Save prepared submissions to files

**Canton API**:
- Scala: `participant.ledger_api.interactive_submission.prepare(actAs, commands, commandId, userId)`
- Rust: gRPC `InteractiveSubmissionService.PrepareSubmission()`

**Inputs**:
- Party information from ledger
- Hardcoded template IDs and contract structures

**Outputs**:
- `../step_4/subs/prepared-submission-1.bin`: PrepareSubmissionResponse for governance rules
- `../step_4/subs/prepared-submission-2.bin`: PrepareSubmissionResponse for deposit rules
- `../step_4/subs/prepared-submission-3.bin`: PrepareSubmissionResponse for withdraw rules

**Notes**:
- Run by coordinator with CoordinatorUser token
- User must have `actAs` and `readAs` rights for decentralized party
- Prepared submissions contain transaction hashes to be signed

---

### Step 4: Sign Ledger Submissions

#### 04_SignSubmissions.sc - Sign Prepared Submissions
**Purpose**: Each attestor signs the prepared ledger submissions with their DAML key

**Operations**:
1. Find the DAML key fingerprint (`cbtc-network-daml-transactions`)
2. Load 3 prepared submissions from `./subs/prepared-submission-*.bin`
3. Parse `PrepareSubmissionResponse` protobuf messages
4. Initialize in-memory crypto store (MemoryStorage)
5. Create Canton Crypto instance
6. Download private key from participant
7. Import private key into crypto store
8. For each prepared submission:
   - Extract `preparedTransactionHash`
   - Sign hash with DAML key using `privateCrypto.signBytes()`
9. Save all 3 signatures to one file

**Canton API**:
- Scala: `participant.keys.secret.list()` - find key
- Rust: gRPC `KeyAdministrationService.ListMyKeys()`
- Scala: `participant.keys.secret.download(fingerprint)` - export key
- Rust: gRPC `KeyAdministrationService.ExportKeyPair()`
- Scala: `Crypto.create()` + `privateCrypto.signBytes()` - sign
- Rust: Native Rust crypto library (need to implement Canton-compatible signing)

**Inputs**:
- `./subs/prepared-submission-{1,2,3}.bin`: Prepared submissions from step 3b

**Outputs**:
- `submission-signatures.bin`: 3 Signature protobuf messages

**Notes**:
- Run on each attestor participant
- Files collected as: `submission-signatures-{1,2,3}.bin`
- This is the most complex step due to crypto operations
- May need to use Canton crypto libraries or reimplement signing in Rust

---

### Step 5: Execute Ledger Submissions

#### 05_ExecuteSubmissions.sc - Execute Signed Submissions
**Purpose**: Aggregate signatures and execute all ledger submissions

**Operations**:
1. Discover all prepared submission files in `./subs/`
2. Load and parse all `PrepareSubmissionResponse` messages
3. Discover all signature files in `./signatures/`
4. Load signatures from each attestor (3 signatures per attestor)
5. Validate signature counts (must be 3 per attestor)
6. For each submission (1, 2, 3):
   - Collect signature at index from each attestor
   - Map: `partyId -> [sig_from_attestor1, sig_from_attestor2, sig_from_attestor3]`
   - Execute submission with signature map
7. Wait for contracts to appear in ACS (Active Contract Set)

**Canton API**:
- Scala: `participant.ledger_api.interactive_submission.execute(preparedTx, signatures, uuid, hashingScheme, userId)`
- Rust: gRPC `InteractiveSubmissionService.ExecuteSubmission()`
- Scala: `participant.ledger_api.state.acs.of_party(party, filterTemplates)`
- Rust: gRPC Ledger API `StateService.GetActiveContracts()`

**Inputs**:
- `./subs/prepared-submission-{1,2,3}.bin`: Prepared submissions
- `./signatures/submission-signatures-{1,2,3}.bin`: Signatures from attestors

**Outputs**:
- None (contracts created on ledger)

**Notes**:
- Run by coordinator with CoordinatorUser token
- Final step completes the full setup
- Can verify success by querying ACS for `CBTCGovernanceRules` contract

---

## Key Challenges for Rust Port

### 1. Canton API Translation
**Challenge**: Scala console functions must be mapped to gRPC API calls

**Solution**:
- Study Canton protobuf definitions in `ledger_proto/`
- Use `tonic` for gRPC client generation
- Map each Scala function to corresponding gRPC method

### 2. Cryptography
**Challenge**: Canton uses custom crypto with specific key formats and signing schemes

**Solution Options**:
- **Option A**: Use Canton's Java/Scala crypto libraries via JNI
- **Option B**: Reimplement Canton crypto in pure Rust (complex)
- **Option C**: Call Canton Admin API for crypto operations when possible
- **Recommended**: Start with Option C, fall back to Option A if needed

### 3. Protobuf I/O
**Challenge**: Read/write Canton protobuf messages to `.bin` files

**Solution**:
- Use `prost` for protobuf serialization/deserialization
- Create utility functions: `read_messages_from_file()`, `write_messages_to_file()`
- Handle both single and multi-message files

### 4. Multi-Participant Coordination
**Challenge**: Orchestrate operations across 3+ participants with different configs

**Solution**:
- Support multiple configuration files (like `remote-connect.conf`, `remote-connect-2.conf`)
- CLI commands to specify which participant to use
- File naming conventions to track attestor indices

### 5. Topology Management
**Challenge**: Complex topology operations with proposals, signatures, aggregation

**Solution**:
- Build strongly-typed Rust structs for topology mappings
- Implement signature aggregation logic
- Create retry/polling utilities for topology propagation

### 6. Interactive Submissions
**Challenge**: Multi-step prepare-sign-execute workflow

**Solution**:
- Store prepared submissions as intermediate files
- Aggregate signatures from multiple attestors
- Build signature map structure for execution

### 7. Error Handling
**Challenge**: Many failure points (network, parsing, crypto, topology conflicts)

**Solution**:
- Use `anyhow` or `thiserror` for rich error types
- Add retry logic with timeouts
- Provide clear error messages with context

---

## Implementation Tasks

### Phase 1: Infrastructure Setup
- [x] Set up Rust project structure for all steps (step_1 through step_5)
- [x] Add dependencies: `tonic`, `prost`, `tokio`, cryptography libraries
- [x] Create common utilities module:
  - [x] Protobuf file I/O utilities
  - [x] Configuration loading (connection strings, OAuth tokens)
  - [x] Retry/polling utilities
  - [x] Error types
- [x] Set up CLI with subcommands for each step
- [x] Add gRPC client builders with authentication

### Phase 2: Step 1 Implementation
- [x] Implement `upload_dars()`: Step 1 - 00_UploadDars
  - [x] Connect to participant Admin API
  - [x] Upload DAR files via `UploadDar()` RPC
- [x] Implement `generate_keys()`: Step 1 - 01_GenerateKeys
  - [x] Generate namespace signing key
  - [x] Get synchronizer ID
  - [x] Propose namespace delegation
  - [x] Generate DAML signing key
  - [x] Export keys to `attestor-public-keys.bin`
  - [x] Export participant ID to `participant-id.bin`

### Phase 3: Step 1a Implementation
- [x] Implement `create_proposals()`: Step 1a - 01a_CreateProposals
  - [x] Load and parse all attestor key files
  - [x] Load all participant ID files
  - [x] Calculate threshold
  - [x] Create DNS proposal
  - [x] Create P2P proposal
  - [x] Create PTK proposal
  - [x] Save proposals to files
  - [x] Fixed: compute_decentralized_namespace to use length-prefixed hashing

### Phase 4: Steps 2 & 2a Implementation
- [x] Implement `sign_dns_proposals()`: Step 2 - 02_SignProposals
  - [x] Load DNS proposal
  - [x] Sign with participant's keys via Canton SignTransactions RPC
  - [x] Determine participant number by matching participant ID against ids directory
  - [x] Save signed proposal with unique filename
- [x] Implement `submit_dns_proposals()`: Step 2a - 02a_SubmitProposals
  - [x] Load all signed DNS proposals from step_2a/signed-proposals
  - [x] Aggregate signatures from all attestors
  - [x] Submit to topology via AddTransactions RPC
  - [x] Poll and wait for DNS to appear in topology (with retry logic)

### Phase 5: Steps 3 & 3a Implementation
- [x] Implement `sign_p2p_ptk_proposals()`: Step 3 - 03_SignP2PPTKProposals
  - [x] Load P2P and PTK proposals from step_3 directory
  - [x] Sign both proposals via Canton SignTransactions RPC
  - [x] Save both signed proposals to one file in step_3a/signed-proposals
  - [x] Determine participant number by matching participant ID
- [x] Implement `submit_final_proposals()`: Step 3a - 03a_SubmitFinalProposals
  - [x] Load all signed P2P/PTK proposals
  - [x] Aggregate signatures for both
  - [x] Submit to topology
  - [x] Wait for propagation

### Phase 6: Step 3b Implementation
- [ ] Implement `prepare_submissions()`: Step 3b - 03b_PrepareSubmissions
  - [ ] Query party IDs from ledger
  - [ ] Build Daml Record structures for 3 contracts
  - [ ] Call `PrepareSubmission()` RPC for each
  - [ ] Save prepared submissions

### Phase 7: Steps 4 & 5 Implementation
- [ ] Implement `sign_submissions()`: Step 4 - 04_SignSubmissions
  - [ ] Load prepared submissions
  - [ ] Find and export DAML key
  - [ ] Set up crypto (most complex part)
  - [ ] Sign each submission hash
  - [ ] Save signatures
- [ ] Implement `execute_submissions()`: Step 5 - 05_ExecuteSubmissions
  - [ ] Load prepared submissions
  - [ ] Load all attestor signatures
  - [ ] Build signature map
  - [ ] Execute each submission via RPC
  - [ ] Wait for contracts in ACS

### Phase 8: Integration & Testing
- [ ] Create end-to-end test with 3 mock participants
- [ ] Add integration tests for each step
- [ ] Write documentation for running the full workflow
- [ ] Create helper scripts similar to `run-test.sh`

---

## Configuration Requirements

### Connection Configuration
Each attestor needs a configuration file with:
- Admin API endpoint (host:port)
- Ledger API endpoint (host:port)
- OAuth token or credentials
- Synchronizer name ("global")
- Participant name

Example structure:
```toml
[connection]
admin_api_host = "localhost"
admin_api_port = 5001
ledger_api_host = "localhost"
ledger_api_port = 5002
token = "eyJ0eXAiOiJKV1QiLCJhbGc..."

[topology]
synchronizer = "global"
```

### File Organization
Follow the same structure as Canton scripts:
```
releases/0.0.1/
├── step_1/
│   ├── dars/
│   ├── attestor-public-keys.bin (generated)
│   └── participant-id.bin (generated)
├── step_1a/
│   ├── keys/
│   │   ├── attestor-public-keys-0.bin
│   │   ├── attestor-public-keys-1.bin
│   │   └── attestor-public-keys-2.bin
│   └── ids/
│       ├── participant-id-0.bin
│       ├── participant-id-1.bin
│       └── participant-id-2.bin
├── step_2/
│   ├── dns_proto.bin (generated)
│   └── signed-proposals/ (collected)
├── step_2a/
│   ├── dns_proto.bin (copied)
│   ├── namespaceDef.bin (generated)
│   └── signed-proposals/ (with all signatures)
├── step_3/
│   ├── p2p_proto.bin (generated)
│   ├── ptk_proto.bin (generated)
│   └── signed-proposals/ (generated by attestors)
├── step_3a/
│   ├── p2p_proto.bin (copied)
│   ├── ptk_proto.bin (copied)
│   ├── namespaceDef.bin (copied)
│   └── signed-proposals/ (collected)
├── step_3b/
│   └── (coordinator runs this)
├── step_4/
│   ├── subs/
│   │   ├── prepared-submission-1.bin (from step_3b)
│   │   ├── prepared-submission-2.bin
│   │   └── prepared-submission-3.bin
│   └── submission-signatures.bin (generated by each attestor)
└── step_5/
    ├── subs/ (copied from step_4)
    ├── signatures/
    │   ├── submission-signatures-1.bin
    │   ├── submission-signatures-2.bin
    │   └── submission-signatures-3.bin
    └── (coordinator runs execute)
```

---

## API Mapping Reference

### Key Management
| Scala | Rust gRPC | Proto Service |
|-------|-----------|---------------|
| `participant.keys.secret.generate_signing_key()` | `KeyAdministrationService.GenerateSigningKey()` | `com.digitalasset.canton.admin.crypto.v30` |
| `participant.keys.secret.list()` | `KeyAdministrationService.ListMyKeys()` | Same |
| `participant.keys.secret.download()` | `KeyAdministrationService.ExportKeyPair()` | Same |

### Topology Management
| Scala | Rust gRPC | Proto Service |
|-------|-----------|---------------|
| `participant.topology.namespace_delegations.propose_delegation()` | `TopologyManagerWriteService.Authorize()` | `com.digitalasset.canton.admin.participant.v30` |
| `participant.topology.decentralized_namespaces.propose()` | `TopologyManagerWriteService.Authorize()` | Same |
| `participant.topology.party_to_participant_mappings.propose()` | `TopologyManagerWriteService.Authorize()` | Same |
| `participant.topology.party_to_key_mappings.propose()` | `TopologyManagerWriteService.Authorize()` | Same |
| `participant.topology.transactions.sign()` | `TopologyManagerWriteService.SignTransactions()` | Same |
| `participant.topology.transactions.load()` | `TopologyManagerWriteService.Authorize()` | Same |

### DAR Management
| Scala | Rust gRPC | Proto Service |
|-------|-----------|---------------|
| `participant.dars.upload()` | `ParticipantAdministrationService.UploadDar()` | `com.digitalasset.canton.admin.participant.v30` |

### Interactive Submission
| Scala | Rust gRPC | Proto Service |
|-------|-----------|---------------|
| `participant.ledger_api.interactive_submission.prepare()` | `InteractiveSubmissionService.PrepareSubmission()` | `com.daml.ledger.api.v2.interactive` |
| `participant.ledger_api.interactive_submission.execute()` | `InteractiveSubmissionService.ExecuteSubmission()` | Same |

### Ledger Queries
| Scala | Rust gRPC | Proto Service |
|-------|-----------|---------------|
| `participant.parties.find()` | Ledger API `PartyManagementService.ListKnownParties()` | `com.daml.ledger.api.v2` |
| `participant.ledger_api.state.acs.of_party()` | Ledger API `StateService.GetActiveContracts()` | Same |

---

## Next Steps

1. **Review this plan** - Ensure alignment with project goals
2. **Choose starting point** - Which step to implement first
3. **Resolve crypto strategy** - How to handle Canton signing
4. **Set up development environment** - Ensure Canton instance is accessible
5. **Begin implementation** - Start with utilities and Step 1

---

## Notes

- The coordinator role can be any of the attestors or a separate participant
- All attestors must be online and accessible for the full workflow
- OAuth tokens must be valid and have appropriate permissions
- The workflow is idempotent in most steps (can be re-run safely)
- Threshold must be majority (> 50%) for security
- Step 4 is the most complex due to direct cryptographic operations
