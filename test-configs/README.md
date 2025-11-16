# Test Configuration Files

This directory contains test configuration files for running a 3-participant Canton onboarding setup with Noise protocol secure communication.

## Structure

```
test-configs/
├── keys/                      # Noise protocol static keypairs
│   ├── participant-1.key     # Private key for Participant 1 (Coordinator)
│   ├── participant-2.key     # Private key for Participant 2
│   └── participant-3.key     # Private key for Participant 3
├── network.toml              # Shared network topology configuration
├── node-1.toml              # Node config for Participant 1 (Coordinator)
├── node-2.toml              # Node config for Participant 2
└── node-3.toml              # Node config for Participant 3
```

## Network Topology

- **Participant 1**: Coordinator running on port 9001 (Canton Admin: 5012, Ledger: 5011)
- **Participant 2**: Attestor running on port 9002 (Canton Admin: 5014, Ledger: 5013)
- **Participant 3**: Attestor running on port 9003 (Canton Admin: 5016, Ledger: 5015)

All participants connect to `localhost` for testing.

## Usage

### Running the Coordinator (Participant 1)

```bash
cargo run -- start --config test-configs/node-1.toml
```

### Running Attestors (Participants 2 & 3)

In separate terminals:

```bash
# Terminal 2
cargo run -- start --config test-configs/node-2.toml

# Terminal 3
cargo run -- start --config test-configs/node-3.toml
```

### Running Individual Steps

For testing individual steps without Noise protocol:

```bash
# Example: Upload DARs for Participant 1
cargo run -- upload-dars --config test-configs/node-1.toml
```

## Security Notes

⚠️ **Important**: These are test configurations with pre-generated keys.

- **DO NOT use these keys in production**
- The private keys in `keys/` directory should be kept secure
- In production, each participant should generate their own keys using:
  ```bash
  cargo run -- keygen -o path/to/private-key.key
  ```

## Network Configuration

The `network.toml` file defines:
- Network name and protocol version
- All participants with their public keys and addresses
- Coordinator selection strategy (`explicit` - Participant 1 is designated)
- Timeout settings for handshakes and messages
- Security requirements (all 3 participants must be present)

## Node Configuration

Each `node-X.toml` file contains:
- **Node identity**: Which participant this node represents
- **Static key file**: Path to this node's private key
- **Network config**: Reference to shared `network.toml`
- **Canton settings**: Admin API and Ledger API endpoints specific to this participant

## Coordinator Selection

The network is configured with `coordinator_strategy = "explicit"`, meaning:
- Participant 1 is explicitly designated as the coordinator
- Other strategies available: `"first"` (first in list) or `"election"` (leader election)
