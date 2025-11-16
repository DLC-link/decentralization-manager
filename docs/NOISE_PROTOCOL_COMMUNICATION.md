# Noise Protocol Communication Architecture

## Overview

This document describes how peers (Coordinator and Attestors) communicate securely using the Noise Protocol Framework during the multi-party decentralized namespace setup process.

## Table of Contents

1. [Introduction to Noise Protocol](#introduction-to-noise-protocol)
2. [Architecture Overview](#architecture-overview)
3. [Connection Lifecycle](#connection-lifecycle)
4. [Handshake Process](#handshake-process)
5. [Message Types](#message-types)
6. [Message Format](#message-format)
7. [Security Properties](#security-properties)
8. [Implementation Details](#implementation-details)
9. [Error Handling](#error-handling)
10. [Examples](#examples)

---

## Introduction to Noise Protocol

The **Noise Protocol Framework** is a framework for building cryptographic protocols based on Diffie-Hellman key agreement. It provides:

- **Mutual authentication**: Both parties verify each other's identity
- **Forward secrecy**: Past communications remain secure even if long-term keys are compromised
- **Encrypted channels**: All data is encrypted after handshake
- **Minimal overhead**: Efficient and lightweight
- **Pattern flexibility**: Multiple handshake patterns for different security requirements

### Why Noise for This Project?

In the multi-party Canton setup:
- **Security**: Sensitive cryptographic material (keys, signatures) must be transmitted securely
- **Authentication**: Each attestor must verify they're communicating with the legitimate coordinator
- **Privacy**: Topology proposals and signatures should not be visible to eavesdroppers
- **Simplicity**: Noise provides all this with a simple, well-tested framework

---

## Architecture Overview

### Network Topology

```
        Coordinator (Server)
              |
    +---------+---------+
    |         |         |
Attestor-1  Attestor-2  Attestor-3+
(Client)    (Client)    (Client)
```

### Role Definitions

**Coordinator (Server Role)**:
- Listens on a TCP port
- Accepts connections from attestors
- Distributes proposals and commands
- Aggregates signatures
- Submits to Canton

**Attestors (Client Role)**:
- Connect to coordinator
- Authenticate using their static keys
- Receive commands and data
- Perform operations (signing, key generation)
- Send results back to coordinator

---

## Connection Lifecycle

### Phase 1: Initialization

```rust
// Coordinator starts listening
coordinator.listen("0.0.0.0:9000");

// Each attestor connects
attestor1.connect("coordinator.example.com:9000");
attestor2.connect("coordinator.example.com:9000");
attestor3.connect("coordinator.example.com:9000");
```

### Phase 2: Handshake

Each attestor performs a Noise handshake with the coordinator:

1. **Key Exchange**: Establish ephemeral session keys
2. **Authentication**: Verify static public keys
3. **Channel Binding**: Create encrypted transport state

### Phase 3: Active Communication

Once all attestors are connected:

1. **Coordinator sends commands** (broadcast or unicast)
2. **Attestors respond** with acknowledgments or data
3. **Coordinator collects responses** and proceeds to next step

### Phase 4: Graceful Shutdown

After setup completion:

1. **Coordinator sends DISCONNECT** command
2. **Attestors acknowledge** and close connections
3. **Coordinator shuts down** listener

---

## Handshake Process

### Noise Pattern: `XX`

We use the **Noise XX** pattern, which provides:
- Mutual authentication
- Identity hiding (keys not revealed until encrypted)
- Two round trips

#### Handshake Flow

```
Attestor                           Coordinator
   |                                    |
   |  -> e (ephemeral key)              |
   |                                    |
   |  <- e, ee, s (ephem, static key)   |
   |                                    |
   |  -> s, se (static key, DH)         |
   |                                    |
   [Handshake complete, transport mode]
```

#### Step-by-Step

**Message 1 (Attestor → Coordinator)**:
```
e: Attestor generates ephemeral keypair and sends public key
```

**Message 2 (Coordinator → Attestor)**:
```
e:  Coordinator sends ephemeral public key
ee: Diffie-Hellman(AttestorEphemeral, CoordinatorEphemeral)
s:  Coordinator sends encrypted static public key
```

**Message 3 (Attestor → Coordinator)**:
```
s:  Attestor sends encrypted static public key
se: Diffie-Hellman(AttestorStatic, CoordinatorEphemeral)
```

After message 3, both parties have:
- Verified each other's static public keys
- Derived shared encryption keys
- Established a secure channel

### Static Key Management

Each party has a long-term static keypair:

```rust
// Coordinator generates/loads static key
let coordinator_static_key = KeyPair::from_file("coordinator_key.priv")?;

// Attestor generates/loads static key
let attestor_static_key = KeyPair::from_file("attestor_key.priv")?;

// Public keys are distributed out-of-band
// (e.g., during initial setup, via configuration files)
```

**Trust Establishment**:
- Attestors know coordinator's static public key (configured)
- Coordinator maintains allowlist of attestor static public keys
- Connections from unknown keys are rejected

---

## Message Types

After handshake, all messages follow a structured protocol.

### Command Messages (Coordinator → Attestor)

| Command | Description | Payload |
|---------|-------------|---------|
| `UPLOAD_DARS` | Instruct attestor to upload DAR files | None |
| `GENERATE_KEYS` | Instruct attestor to generate keys | None |
| `SIGN_DNS` | Instruct attestor to sign DNS proposal | `dns_proto.bin` |
| `SIGN_P2P_PTK` | Instruct attestor to sign P2P/PTK proposals | `p2p_proto.bin`, `ptk_proto.bin` |
| `SIGN_SUBMISSIONS` | Instruct attestor to sign ledger submissions | `prepared-submission-*.bin` (3 files) |
| `STATUS_UPDATE` | Inform attestors of progress | Status string |
| `DISCONNECT` | Graceful shutdown | None |

### Response Messages (Attestor → Coordinator)

| Response | Description | Payload |
|----------|-------------|---------|
| `ACK` | Command acknowledged | None |
| `DATA` | Sending requested data | Binary payload |
| `ERROR` | Operation failed | Error message |
| `READY` | Ready for next command | None |

### Data Transfer Messages

| Message | Direction | Description |
|---------|-----------|-------------|
| `KEYS_UPLOAD` | Attestor → Coordinator | `attestor-public-keys.bin` + `participant-id.bin` |
| `DNS_SIGNATURE` | Attestor → Coordinator | `signed-dns-proposal.bin` |
| `P2P_PTK_SIGNATURES` | Attestor → Coordinator | `signed-p2p-ptk-proposals.bin` |
| `SUBMISSION_SIGNATURES` | Attestor → Coordinator | `submission-signatures.bin` |

---

## Message Format

### Wire Format

All messages after handshake use the following format:

```
+----------------+------------------+--------------------+
| Message Type   | Payload Length   | Payload            |
| (2 bytes)      | (4 bytes)        | (variable)         |
+----------------+------------------+--------------------+
```

- **Message Type**: 16-bit unsigned integer (big-endian)
- **Payload Length**: 32-bit unsigned integer (big-endian)
- **Payload**: Variable-length binary data

### Encryption

All bytes are encrypted using the Noise transport cipher (ChaChaPoly):

```
Encrypted Packet = Encrypt(MessageType || PayloadLength || Payload)
```

### Message Type Enumeration

```rust
pub enum MessageType {
    // Commands (0x0000 - 0x00FF)
    UploadDars = 0x0001,
    GenerateKeys = 0x0002,
    SignDns = 0x0003,
    SignP2pPtk = 0x0004,
    SignSubmissions = 0x0005,
    StatusUpdate = 0x0006,
    Disconnect = 0x0007,

    // Responses (0x0100 - 0x01FF)
    Ack = 0x0101,
    Data = 0x0102,
    Error = 0x0103,
    Ready = 0x0104,

    // Data Transfers (0x0200 - 0x02FF)
    KeysUpload = 0x0201,
    DnsSignature = 0x0202,
    P2pPtkSignatures = 0x0203,
    SubmissionSignatures = 0x0204,
}
```

### Example Message Encoding

**Command: UPLOAD_DARS**
```
[0x00, 0x01] [0x00, 0x00, 0x00, 0x00]
  ^^ type      ^^ length (0 bytes)
```

**Response: DATA with 1024 bytes**
```
[0x01, 0x02] [0x00, 0x00, 0x04, 0x00] [... 1024 bytes ...]
  ^^ type      ^^ length (1024)         ^^ payload
```

---

## Security Properties

### Confidentiality

- All data after handshake is encrypted with ChaChaPoly-1305
- Eavesdroppers cannot read message contents
- Includes keys, signatures, proposals, and commands

### Authentication

- Static keys ensure both parties are who they claim to be
- Coordinator only accepts connections from known attestors
- Attestors verify coordinator's identity before proceeding

### Integrity

- AEAD cipher (ChaChaPoly) provides authentication
- Any tampering results in decryption failure
- Messages cannot be modified in transit

### Forward Secrecy

- Ephemeral keys used in handshake
- If long-term keys compromised, past sessions remain secure
- New ephemeral keys for each connection

### Replay Protection

- Noise protocol includes nonce progression
- Old messages cannot be replayed
- Session keys are unique per connection

### Protection Against

| Attack | Protection |
|--------|------------|
| Eavesdropping | Full encryption |
| Man-in-the-Middle | Mutual authentication |
| Replay | Nonce progression |
| Tampering | AEAD authentication |
| Impersonation | Static key verification |
| Connection hijacking | Session binding |

---

## Implementation Details

> **Note:** This section provides conceptual pseudocode and architectural patterns, not production-ready code. Actual implementation will require consulting specific library documentation.

### Rust Implementation Options

Several Rust crates provide Noise Protocol implementations with Tokio async support:

**Available Libraries:**
- **`snow`** - Low-level Noise protocol implementation, most widely used
- **`tokio-noise`** - Wraps Tokio TcpStream with Noise encryption
- **`snowstorm`** - Higher-level async streams/packets with Noise
- **`hyper-noise`** - Integrates Noise protocol with HTTP/Hyper stack

Choose based on your needs:
- Direct TCP with full control → `snow` + manual Tokio integration
- Simple encrypted TCP streams → `tokio-noise`
- Packet-based async communication → `snowstorm`
- HTTP over Noise → `hyper-noise`

For this project, we'll use direct `snow` + Tokio for maximum flexibility.

#### Implementation Pattern

**Core Components:**

```
NoiseConnection
├─ Tokio TcpStream (underlying transport)
├─ Snow HandshakeState → TransportState (encryption)
├─ Remote static key (for peer authentication)
└─ Message framing (length-prefix protocol)
```

**Coordinator (Server) - Accepting Connections:**

```rust
// Pseudocode - conceptual flow
async fn handshake_responder(&self, tcp_stream: TcpStream) -> Result<SecureConnection> {
    // 1. Create Noise handshake state as responder
    let handshake_state = create_responder_handshake("Noise_XX_25519_ChaChaPoly_BLAKE2s", my_static_key);

    // 2. Perform three-way handshake (read, write, read)
    let (read_buf, write_buf) = exchange_handshake_messages(tcp_stream, handshake_state);

    // 3. Transition to transport mode (encryption active)
    let transport_state = handshake_state.into_transport_mode();
    let remote_static_key = extract_remote_static_key(handshake_state);

    // 4. Verify peer is in allowlist
    if !allowlist.contains(remote_static_key) {
        return Err("Unknown peer");
    }

    Ok(SecureConnection { tcp_stream, transport_state, remote_static_key })
}
```

**Attestor (Client) - Initiating Connection:**

```rust
// Pseudocode - conceptual flow
async fn connect_to_coordinator(&mut self, addr: &str) -> Result<()> {
    // 1. Connect TCP
    let tcp_stream = TcpStream::connect(addr).await?;

    // 2. Create Noise handshake state as initiator
    let handshake_state = create_initiator_handshake(
        "Noise_XX_25519_ChaChaPoly_BLAKE2s",
        my_static_key,
        coordinator_public_key
    );

    // 3. Perform three-way handshake (write, read, write)
    let (read_buf, write_buf) = exchange_handshake_messages(tcp_stream, handshake_state);

    // 4. Transition to transport mode
    let transport_state = handshake_state.into_transport_mode();
    let remote_static_key = extract_remote_static_key(handshake_state);

    // 5. Verify coordinator identity
    if remote_static_key != coordinator_public_key {
        return Err("Coordinator key mismatch");
    }

    self.connection = Some(SecureConnection { tcp_stream, transport_state, remote_static_key });
    Ok(())
}
```

**Encrypted Communication:**

```rust
// Pseudocode - message framing over encrypted channel
struct SecureConnection {
    tcp_stream: TcpStream,
    transport_state: TransportState,  // Handles encryption/decryption
    remote_static_key: [u8; 32],
}

impl SecureConnection {
    async fn send_message(&mut self, msg: &Message) -> Result<()> {
        // 1. Serialize message
        let plaintext = msg.to_bytes();

        // 2. Encrypt with Noise transport
        let ciphertext = transport_state.write_message(&plaintext);

        // 3. Send length-prefixed
        let len = ciphertext.len() as u32;
        tcp_stream.write_all(&len.to_be_bytes()).await?;
        tcp_stream.write_all(&ciphertext).await?;
        Ok(())
    }

    async fn receive_message(&mut self) -> Result<Message> {
        // 1. Read length prefix
        let len = tcp_stream.read_u32().await? as usize;

        // 2. Read ciphertext
        let mut ciphertext = vec![0u8; len];
        tcp_stream.read_exact(&mut ciphertext).await?;

        // 3. Decrypt with Noise transport
        let plaintext = transport_state.read_message(&ciphertext)?;

        // 4. Deserialize message
        Message::from_bytes(&plaintext)
    }
}
```

**Key Points:**
- Handshake establishes secure channel before any data transfer
- Transport state handles all encryption/decryption transparently
- Message framing uses length-prefix (4 bytes) + payload
- Static keys authenticate both peers mutually
- Each connection has independent session keys (forward secrecy)

### Configuration

Instead of separate coordinator and attestor configurations, all parties use a **shared configuration file** that defines the entire network topology. Each member runs the same program, and the program determines their role based on their identity in the config.

**Shared Network Configuration (`network.toml`)**:
```toml
# Network-wide configuration shared by all participants
[network]
name = "cbtc-setup-network"
protocol_version = "1.0"
port = 9000  # Default port for all nodes

# Coordinator selection strategy
# Options: "first", "explicit", "election"
coordinator_strategy = "explicit"

# List of all participants in the setup
# Order matters if coordinator_strategy = "first"
[[participants]]
id = "coordinator-node"
name = "Coordinator Node"
role = "coordinator"  # Only used if coordinator_strategy = "explicit"
address = "10.0.1.100"
port = 9000
public_key = "c0011d1a70123456789abcdef..."  # Static Noise public key (hex)

[[participants]]
id = "attestor-1"
name = "Attestor 1"
address = "10.0.1.101"
port = 9000
public_key = "a1b2c3d4e5f6789012345678..."

[[participants]]
id = "attestor-2"
name = "Attestor 2"
address = "10.0.1.102"
port = 9000
public_key = "f6e5d4c3b2a1098765432109..."

[[participants]]
id = "attestor-3"
name = "Attestor 3"
address = "10.0.1.103"
port = 9000
public_key = "1a2b3c4d5e6f098765432109..."

[timeouts]
handshake_timeout_secs = 30
message_timeout_secs = 120
connection_retry_attempts = 3
connection_retry_delay_secs = 5

[security]
# Require all participants to be present before starting
require_all_participants = true
# Minimum number of participants (threshold)
minimum_participants = 3
```

**Individual Node Configuration (`node.toml`)**:
```toml
# Each participant has their own node configuration
# This identifies who they are in the network
[node]
# Must match one of the IDs in network.toml
participant_id = "attestor-1"

# Path to this node's static private key
static_key_file = "keys/attestor-1_static.key"

# Override listen address (default: 0.0.0.0)
listen_address = "0.0.0.0"

# Path to shared network configuration
network_config = "network.toml"

[canton]
# Canton participant configuration
admin_api_host = "localhost"
admin_api_port = 5001
ledger_api_host = "localhost"
ledger_api_port = 5002
token = "eyJ0eXAiOiJKV1QiLCJhbGc..."
```

### Coordinator Selection Strategies

The configuration supports three strategies for determining which node acts as the coordinator:

#### 1. Explicit Selection (`coordinator_strategy = "explicit"`)

**Recommended for production**. One participant is explicitly designated as coordinator in the network configuration:

```toml
[[participants]]
id = "coordinator-node"
role = "coordinator"  # Explicitly designated
```

**Advantages**:
- Clear, deterministic selection
- No ambiguity about roles
- Easy to troubleshoot

**Disadvantages**:
- Single point of failure (if coordinator node is down, setup cannot proceed)
- Requires coordination to decide who is coordinator before starting

#### 2. First Node Selection (`coordinator_strategy = "first"`)

The first participant listed in the configuration acts as coordinator:

```toml
coordinator_strategy = "first"

# The first participant becomes coordinator
[[participants]]
id = "node-1"  # This one will be coordinator
# ...

[[participants]]
id = "node-2"  # This and others are attestors
# ...
```

**Advantages**:
- Simple, deterministic
- No explicit role assignment needed

**Disadvantages**:
- Still a single point of failure
- Somewhat arbitrary

#### 3. Leader Election (`coordinator_strategy = "election"`)

**Most robust**. Nodes perform a distributed leader election if the designated coordinator is unavailable.

**Election Algorithm**:
1. All nodes attempt to connect to the designated coordinator
2. If coordinator doesn't respond within timeout, trigger election
3. Election uses **Bully Algorithm**:
   - Node with highest ID (lexicographically) becomes coordinator
   - If that node is down, next highest takes over
4. Once elected, new coordinator announces its role to all peers
5. All nodes update their local state

```
Election Algorithm (Bully Pattern):

1. Try connecting to designated coordinator first
   - If reachable → Use designated coordinator
   - If unreachable → Proceed to election

2. Sort all participant IDs lexicographically

3. Starting from highest ID, iterate downwards:
   a. If current candidate is me:
      → I am the highest available ID
      → Declare myself coordinator
      → Announce to other peers
      → Break

   b. Try connecting to candidate:
      → If reachable: Accept them as coordinator, break
      → If unreachable: Continue to next lower ID

4. If no coordinator found → Error (insufficient quorum)

Result: Deterministic coordinator selection
- Always the same coordinator given same availability
- No split-brain (highest available ID always wins)
- Automatic failover if coordinator goes down
```

**Advantages**:
- Fault-tolerant
- Automatic failover if coordinator goes down
- More resilient to network issues

**Disadvantages**:
- More complex implementation
- Potential for split-brain scenarios (mitigated by using deterministic election)

### Program Startup Flow

Each participant runs the same program binary with their own `node.toml`:

```bash
# On node 1 (10.0.1.100)
$ cargo run -- --config node.toml start

# On node 2 (10.0.1.101)
$ cargo run -- --config node.toml start

# On node 3 (10.0.1.102)
$ cargo run -- --config node.toml start

# On node 4 (10.0.1.103)
$ cargo run -- --config node.toml start
```

**Startup Sequence:**

```
Startup Flow:
1. Load node.toml (my identity + Canton config)
2. Load network.toml (shared topology + all public keys)
3. Determine coordinator:
   - Explicit: Read from network.toml role field
   - First: Take first entry in participants list
   - Election: Run Bully algorithm to elect highest available ID
4. Determine my role (am I the coordinator?)
5. Branch:
   - If coordinator: Start listening, wait for attestors
   - If attestor: Connect to coordinator
6. Begin protocol execution
```

### Initial Setup: Key Generation and Exchange

Before the distributed setup can begin, participants must perform an initial bootstrapping process to establish trust.

#### Step 1: Generate Static Keypair

Each participant generates their own Noise static keypair:

```bash
# Each participant runs this command
$ cargo run -- keygen --output keys/my_static.key

Generating Noise static keypair...
Private key saved to: keys/my_static.key
Public key (hex): a1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef123456

⚠️  Keep your private key secure! Never share it with anyone.
💡  Share your public key with other participants to add to network.toml
```

The private key is stored securely, and the public key (hex-encoded) is displayed for sharing.

#### Step 2: Exchange Public Keys

Participants exchange their public keys through a **secure out-of-band channel**:

- **In-person meeting**: Exchange keys via USB drive or QR code
- **Secure messaging**: Use Signal, PGP-encrypted email, etc.
- **Video call**: Read keys aloud and verify (for small groups)
- **Blockchain**: Post commitments to public keys on-chain (for trustless setup)

**Example Exchange (3 participants)**:

```
Alice: My public key is: a1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef123456
Bob:   My public key is: f6e5d4c3b2a1098765432109876543210fedcba098765432109876543210fed
Carol: My public key is: 1a2b3c4d5e6f098765432109876543210abcdef1234567890abcdef123456789

Alice: I propose to be the coordinator since I have the most stable connection.
Bob:   Agreed.
Carol: Agreed. Alice will be coordinator.

Alice: My IP address is 10.0.1.100
Bob:   My IP address is 10.0.1.101
Carol: My IP address is 10.0.1.102
```

#### Step 3: Create Shared Network Configuration

One participant (typically the designated coordinator) creates the `network.toml` file:

```toml
[network]
name = "cbtc-setup-network"
protocol_version = "1.0"
port = 9000
coordinator_strategy = "explicit"

[[participants]]
id = "alice"
name = "Alice (Coordinator)"
role = "coordinator"
address = "10.0.1.100"
port = 9000
public_key = "a1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef123456"

[[participants]]
id = "bob"
name = "Bob"
address = "10.0.1.101"
port = 9000
public_key = "f6e5d4c3b2a1098765432109876543210fedcba098765432109876543210fed"

[[participants]]
id = "carol"
name = "Carol"
address = "10.0.1.102"
port = 9000
public_key = "1a2b3c4d5e6f098765432109876543210abcdef1234567890abcdef123456789"

[timeouts]
handshake_timeout_secs = 30
message_timeout_secs = 120
connection_retry_attempts = 3
connection_retry_delay_secs = 5

[security]
require_all_participants = true
minimum_participants = 3
```

#### Step 4: Distribute Network Configuration

The `network.toml` file is distributed to all participants through the same secure channel:

```bash
# Coordinator creates and signs the network config
$ sha256sum network.toml
8f43e... network.toml

# Coordinator shares file and hash with all participants
# Each participant verifies the hash matches
$ sha256sum network.toml
8f43e... network.toml  ✓ Hash matches!
```

#### Step 5: Create Individual Node Configuration

Each participant creates their own `node.toml`:

```bash
# Alice (coordinator)
$ cat node.toml
[node]
participant_id = "alice"
static_key_file = "keys/my_static.key"
listen_address = "0.0.0.0"
network_config = "network.toml"

[canton]
admin_api_host = "localhost"
admin_api_port = 5001
ledger_api_host = "localhost"
ledger_api_port = 5002
token = "eyJ0eXAiOiJKV1QiLCJhbGc..."

# Bob
$ cat node.toml
[node]
participant_id = "bob"
static_key_file = "keys/my_static.key"
listen_address = "0.0.0.0"
network_config = "network.toml"

[canton]
admin_api_host = "localhost"
admin_api_port = 5011
ledger_api_host = "localhost"
ledger_api_port = 5012
token = "eyJ0eXAiOiJKV1QiLCJhbGc..."

# Carol - similar configuration
```

Now all participants are ready to run the distributed setup!

### Distributed Setup Example

**Prerequisites** (completed above):
1. ✅ All participants have agreed on the network topology
2. ✅ Each participant has generated their static Noise keypair
3. ✅ Public keys have been shared and added to `network.toml`
4. ✅ `network.toml` is distributed to all participants
5. ✅ Each participant has created their `node.toml`

**Execution**:

```bash
# Participant 1 (Coordinator - 10.0.1.100)
$ cat node.toml
[node]
participant_id = "coordinator-node"
static_key_file = "keys/coordinator_static.key"
network_config = "network.toml"

$ cargo run -- start
[INFO] I am: coordinator-node
[INFO] Coordinator: coordinator-node
[INFO] My role: COORDINATOR
[INFO] Starting as COORDINATOR
[INFO] Listening on 0.0.0.0:9000
[INFO] Expecting 3 attestors to connect
[INFO] Attestor connected: attestor-1 (10.0.1.101)
[INFO] Attestor connected: attestor-2 (10.0.1.102)
[INFO] Attestor connected: attestor-3 (10.0.1.103)
[INFO] All attestors connected, starting setup protocol
[INFO] Broadcasting command: UPLOAD_DARS
...

# Participant 2 (Attestor 1 - 10.0.1.101)
$ cat node.toml
[node]
participant_id = "attestor-1"
static_key_file = "keys/attestor-1_static.key"
network_config = "network.toml"

$ cargo run -- start
[INFO] I am: attestor-1
[INFO] Coordinator: coordinator-node
[INFO] My role: ATTESTOR
[INFO] Starting as ATTESTOR
[INFO] Connecting to coordinator at 10.0.1.100:9000
[INFO] Connected to coordinator, ready for commands
[INFO] Received command: UPLOAD_DARS
[INFO] Uploading DAR files...
[INFO] Upload complete, sending ACK
...

# Participant 3 (Attestor 2 - 10.0.1.102) and 4 (Attestor 3 - 10.0.1.103)
# Similar output to Participant 2
```

---

## Error Handling

### Connection Errors

| Error | Cause | Handling |
|-------|-------|----------|
| `HandshakeFailed` | Authentication failure | Reject connection, log |
| `UnknownPeer` | Attestor not in allowlist | Reject connection, log |
| `Timeout` | Network or peer unresponsive | Retry or abort |
| `DecryptionError` | Message tampered or corrupted | Close connection |
| `InvalidMessage` | Malformed message | Send ERROR response |

### Protocol Errors

```
Error Categories:

1. Connection Errors:
   - HandshakeFailed: Noise handshake didn't complete
   - UnknownPeer: Remote static key not in allowlist
   - ConnectionClosed: TCP connection dropped
   - Timeout: Peer not responding within deadline

2. Protocol Errors:
   - DecryptionError: Message authentication failed (tampering detected)
   - InvalidMessage: Malformed message structure
   - ProtocolViolation: Unexpected message type or sequence

3. Application Errors:
   - CantonError: Canton gRPC operation failed
   - FileNotFound: Missing keys/proposals/signatures
   - SignatureInvalid: Cryptographic signature verification failed
```

### Retry Strategy

```
Exponential Backoff with Jitter:

For transient errors (network, timeout):
  attempt = 1
  while attempt <= max_retries:
    try operation
    if success:
      return success

    delay = 2^attempt + random(0, 1) seconds
    log warning: "Attempt {attempt}/{max_retries} failed, retrying in {delay}s"
    sleep(delay)
    attempt++

  return error (exhausted retries)

For permanent errors (authentication, protocol):
  → Fail immediately, no retry
  → Log error and close connection
```

---

## Communication Patterns

### Pattern 1: Command Broadcast with Acknowledgments

```
Coordinator                          Attestor 1, 2, 3
    |                                      |
    |--- UPLOAD_DARS (broadcast) -------->|
    |                                      | (perform upload)
    |<-------- ACK ------------------------|
    |                                      |
    | (wait for all ACKs with timeout)    |
    |                                      |
    |--- GENERATE_KEYS ------------------>|
    |                                      | (generate keys)
    |<-------- DATA (keys) ---------------|
    |                                      |
    | (collect all keys)                  |
```

**Flow:**
1. Coordinator broadcasts command to all attestors
2. Each attestor processes independently
3. Each attestor sends response (ACK or DATA)
4. Coordinator waits for all responses with timeout
5. If any timeout → retry or abort
6. Once all collected → proceed to next step

### Pattern 2: File Distribution

```
Coordinator                          Attestor
    |                                   |
    |--- FILE_METADATA (size) -------->|
    |--- FILE_CHUNK (1) -------------->|
    |--- FILE_CHUNK (2) -------------->|
    |--- FILE_CHUNK (N) -------------->|
    |                                   | (reassemble file)
    |<-------- ACK ---------------------|
```

**Flow:**
1. Coordinator sends metadata (total size, chunks)
2. Coordinator sends file in 64KB chunks
3. Attestor receives and buffers all chunks
4. Attestor verifies completeness
5. Attestor sends ACK when complete

### Pattern 3: Signature Collection

```
Coordinator                          Attestor 1, 2, 3
    |                                      |
    |--- PROPOSAL_DATA ------------------>|
    |                                      | (sign proposal)
    |<-------- SIGNATURE -----------------|
    |                                      |
    | (wait for threshold signatures)     |
    |                                      |
    | (aggregate signatures)               |
    | (submit to Canton)                   |
    |                                      |
    |--- STATUS_UPDATE (success) -------->|
```

**Flow:**
1. Coordinator distributes proposal to sign
2. Each attestor signs independently with their key
3. Each attestor sends signature back
4. Coordinator collects signatures until threshold met
5. Coordinator aggregates all signatures
6. Coordinator submits to Canton
7. Coordinator broadcasts success status

### Pattern 4: Graceful Shutdown

```
Coordinator                          Attestor
    |                                   |
    |--- DISCONNECT ------------------>|
    |                                   | (cleanup resources)
    |<-------- ACK ---------------------|
    | (close connection)                | (close connection)
```

---

## Summary

The Noise Protocol provides a secure, authenticated communication channel between the coordinator and attestors during the multi-party Canton setup. Key benefits:

- ✅ **Strong security**: Mutual authentication, encryption, forward secrecy
- ✅ **Simple implementation**: Well-defined protocol with good Rust libraries
- ✅ **Low overhead**: Minimal performance impact
- ✅ **Proven design**: Used in WireGuard, Lightning Network, and other systems
- ✅ **Flexible**: Supports various network topologies and message patterns
- ✅ **Distributed setup**: Each member runs the same program independently
- ✅ **No central server**: Coordinator is just another peer with an orchestration role
- ✅ **Configurable coordination**: Multiple strategies for coordinator selection

The protocol ensures that sensitive cryptographic material (keys, signatures, proposals) is transmitted securely between peers, and that all parties are who they claim to be.

### Key Design Decisions

**1. Shared Configuration File**
- All participants use the same `network.toml`
- Contains complete network topology and all public keys
- Distributed via secure out-of-band channel
- Ensures everyone has consistent view of the network

**2. Individual Node Configuration**
- Each participant has their own `node.toml`
- Identifies which participant they are in the network
- Contains their private key path and Canton connection details
- Allows same binary to run in different roles

**3. Coordinator Selection**
- Three strategies: explicit, first-node, or election
- Explicit selection recommended for production (clear, deterministic)
- Election provides fault tolerance but adds complexity
- Coordinator is peer-elected, not a privileged central server

**4. Single Binary, Multiple Roles**
- Same program binary runs on all nodes
- Role (coordinator vs attestor) determined at runtime from configuration
- Reduces deployment complexity and ensures consistency
- Each node can potentially be coordinator (with election strategy)

**5. Zero Trust Model**
- All connections authenticated via Noise static keys
- No implicit trust based on IP addresses
- Public keys exchanged out-of-band before setup
- Unknown peers rejected immediately

### Deployment Workflow Summary

```
1. Initial Setup (One-time, Out-of-Band)
   ├─ Each participant generates Noise keypair
   ├─ Public keys exchanged securely (Signal, in-person, etc.)
   ├─ Coordinator creates network.toml with all participants
   ├─ network.toml distributed to all participants
   └─ Each participant creates their node.toml

2. Runtime (Each Setup Session)
   ├─ All participants start program: cargo run -- start
   ├─ Program reads configs and determines role
   ├─ Coordinator starts listening
   ├─ Attestors connect to coordinator
   ├─ Mutual authentication via Noise handshake
   ├─ Coordinator waits for all attestors
   └─ Setup protocol begins

3. Protocol Execution
   ├─ Coordinator sends commands via encrypted channel
   ├─ Attestors execute and respond
   ├─ Files (keys, signatures) transferred over Noise
   └─ Continue until setup complete

4. Shutdown
   ├─ Coordinator sends DISCONNECT command
   ├─ All connections gracefully closed
   └─ Session complete
```

This architecture provides a secure, decentralized approach to the multi-party Canton setup while maintaining simplicity for participants. Each member runs one program with one command, and the Noise protocol handles all the security complexity transparently.

---

## References

### Noise Protocol
- [Noise Protocol Framework Specification](https://noiseprotocol.org/noise.html) - Official spec
- [Noise Explorer - Protocol Analysis Tool](https://noiseexplorer.com/) - Security analysis

### Rust Implementations
- [snow](https://github.com/mcginty/snow) - Low-level Noise protocol implementation
- [tokio-noise](https://crates.io/crates/tokio-noise) - Tokio TcpStream wrapper with Noise
- [snowstorm](https://crates.io/crates/snowstorm) - Async streams/packets with Noise
- [hyper-noise](https://crates.io/crates/hyper-noise) - HTTP over Noise integration

### Real-world Usage
- [WireGuard Protocol](https://www.wireguard.com/protocol/) - VPN using Noise
- [Lightning Network Bolt #8](https://github.com/lightning/bolts/blob/master/08-transport.md) - Bitcoin Lightning uses Noise
