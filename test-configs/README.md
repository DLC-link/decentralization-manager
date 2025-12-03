# Test Configuration Files

This directory contains test configuration files for running a 3-participant Canton onboarding setup with Noise protocol secure communication.

## Structure

```
test-configs/
├── network.toml              # Shared network topology configuration
├── node-1.toml               # Node config for Participant 1 (Coordinator)
├── node-2.toml               # Node config for Participant 2
├── node-3.toml               # Node config for Participant 3
└── README.md                 # This file

keys/                         # Noise protocol static keypairs (generated)
├── participant-1.key         # Private key for Participant 1 (Coordinator)
├── participant-2.key         # Private key for Participant 2
└── participant-3.key         # Private key for Participant 3
```

## Network Topology

- **Participant 1 (app-user)**: Coordinator running on port 9001 (Canton Admin: 2902, Ledger: 2901)
- **Participant 2 (app-provider)**: Attestor running on port 9002 (Canton Admin: 3902, Ledger: 3901)
- **Participant 3 (sv)**: Attestor running on port 9003 (Canton Admin: 4902, Ledger: 4901)

All participants connect to `localhost` for testing.

## Setup

### First Time Setup: Generate Noise Keys

Before running the nodes for the first time, you must generate Noise protocol keypairs:

```bash
# Create keys directory
mkdir -p keys

# Generate keypairs for all participants
cargo run -- keygen -o keys/participant-1.key
cargo run -- keygen -o keys/participant-2.key
cargo run -- keygen -o keys/participant-3.key
```

Each `keygen` command will output the public key. You must update `test-configs/network.toml` with these public keys in the corresponding `[[participants]]` sections.

**Example output:**
```
INFO  Public key (hex): 0352217b145e2f5434bd320309b59c02073a5d28d04d4c7b0b254e74b26a3f1950
```

Copy each public key to the `public_key` field in `network.toml` for the corresponding participant.

## Usage

The application has two main workflows: `onboarding` and `contracts`. Each workflow requires all participants to run simultaneously.

### Running the Onboarding Workflow

The onboarding workflow creates the decentralized party by:
1. Generating cryptographic keys (namespace and DAML transaction keys)
2. Creating and signing DNS proposals
3. Creating and signing P2P proposals (with embedded signing keys for Canton 3.4+)
4. Submitting all proposals to Canton

**Start the coordinator first (Participant 1):**
```bash
cargo run -- -c test-configs/node-1.toml onboarding
```

**Then start the attestors in separate terminals:**
```bash
# Terminal 2
cargo run -- -c test-configs/node-2.toml onboarding

# Terminal 3
cargo run -- -c test-configs/node-3.toml onboarding
```

The coordinator will wait for all attestors to connect before proceeding through the workflow steps.

### Running the Contracts Workflow

After completing onboarding, run the contracts workflow to:
1. Upload DAR files from `dars/` directory
2. Prepare interactive submissions for governance contracts
3. Sign submissions with all participants
4. Execute signed submissions on the ledger

**Start the coordinator first (Participant 1):**
```bash
cargo run -- -c test-configs/node-1.toml contracts
```

**Then start the attestors in separate terminals:**
```bash
# Terminal 2
cargo run -- -c test-configs/node-2.toml contracts

# Terminal 3
cargo run -- -c test-configs/node-3.toml contracts
```

## Security Notes

**Important:**

- The private keys in `keys/` directory should be kept secure
- **DO NOT commit private keys to version control** (they are gitignored)
- **DO NOT use these test keys in production**
- In production, each participant should generate their own keys securely using:
  ```bash
  cargo run -- keygen -o path/to/private-key.key
  ```
- Each participant should share only their public key with the network administrator to update `network.toml`

## Network Configuration

The `network.toml` file defines:
- Network name and protocol version
- All participants with their Noise protocol public keys and addresses
- Coordinator selection strategy (`explicit` - Participant 1 is designated)
- Timeout settings for handshakes and messages
- Application configuration (party ID prefix, key names, contracts to create)

### Required Application Configuration

The `[application]` section in `network.toml` is required and defines:
- `party_id_prefix`: Prefix for decentralized party identifiers
- `namespace_key_name`: Name prefix for namespace signing keys
- `daml_key_name`: Name prefix for DAML transaction signing keys
- `operator_party_hint`: Hint for operator party allocation
- `contracts`: List of contract definitions to create on the ledger

## Node Configuration

Each `node-X.toml` file contains:
- **Node identity**: `node_id` must match a participant ID in `network.toml`
- **Static key file**: Path to this node's Noise protocol private key
- **Network config**: Reference to shared `network.toml`
- **Canton settings**:
  - `admin_api_host` / `admin_api_port`: Canton Admin API endpoint
  - `ledger_api_host` / `ledger_api_port`: Canton Ledger API endpoint
  - `synchronizer`: Canton synchronizer name (default: "global")
  - `ledger_api_user_id`: User ID for ledger submissions (required)
  - `ledger_api_token`: Optional JWT token for authentication

## Coordinator Selection

The network is configured with `coordinator_strategy = "explicit"`, meaning:
- Participant 1 is explicitly designated as the coordinator (via `role = "coordinator"`)
- The coordinator also participates as an attestor (generates keys, signs proposals)
- Other strategies available: `"first"` (first in list) or `"election"` (leader election)
