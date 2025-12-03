# Canton Decentralized Party Onboarding Automation

A Rust-based automation tool for multi-party decentralized namespace setup in Canton blockchain networks. This project streamlines the complex process of onboarding multiple parties to a Canton-based Bitcoin (CBTC) governance system by automating topology management, cryptographic key generation, and ledger operations.

## Key Features

- **Automated Multi-Party Onboarding**: Orchestrates the complete workflow for setting up decentralized party participation
- **Dynamic Participant Support**: Supports any number of participants (N >= 3), with automatic majority threshold calculation
- **Secure Communication**: Noise Protocol Framework for encrypted, authenticated peer-to-peer communication
- **gRPC Integration**: Native Canton Admin and Ledger API integration using Protocol Buffers
- **Cryptographic Key Management**: Automated generation and management of cryptographic keys for secure party identification
- **Topology Management**: Handles DNS and P2P proposal creation, signing, and submission (Canton 3.4+: signing keys embedded in P2P)
- **Ledger Operations**: Manages preparation, signing, and execution of ledger submissions
- **Distributed Architecture**: Coordinator-attestor model with no single point of trust
- **Configuration-Driven**: Flexible TOML-based configuration for different Canton environments
- **Production Ready**: Includes comprehensive error handling, logging, and code quality tooling

## Architecture

This tool implements a port of Canton Scala scripts to Rust, providing a more performant and memory-safe alternative for automated party onboarding. It follows a step-based workflow that ensures proper ordering of operations and handles the complex interdependencies between topology changes and ledger state modifications.

### Communication Model

All coordination between participants uses the **Noise Protocol Framework** for secure, encrypted communication:
- **Coordinator** acts as server, orchestrating the workflow (also participates as an attestor)
- **Attestors** connect as clients, executing commands and returning results
- **Mutual Authentication** via static keypairs (secp256k1)
- **Encrypted Channels** using ChaChaPoly-1305 AEAD cipher

See the [Noise Protocol Communication Architecture](docs/NOISE_PROTOCOL_COMMUNICATION.md) for detailed information.

## Table of Contents

- [Project Overview](#project-overview)
- [CLI Commands](#cli-commands)
- [Workflows](#workflows)
- [Configuration](#configuration)
- [Quick Start](#quick-start)
- [Development](#development)
- [Code Quality](#code-quality)

## Project Overview

This project ports Canton Scala scripts to Rust, implementing a multi-party decentralized namespace setup for CBTC (Canton-based Bitcoin) governance. The tool provides two main workflows:

### Onboarding Workflow
Creates the decentralized party namespace:
1. Generate cryptographic keys (namespace and DAML transaction keys)
2. Create topology proposals (DNS and P2P with embedded signing keys)
3. Multi-party signing of all proposals
4. Submit proposals to Canton

### Contracts Workflow
Sets up governance contracts:
1. Upload DAR files from `dars/` directory
2. Prepare interactive submissions for governance contracts
3. Multi-party signing of ledger submissions
4. Execute signed submissions on the ledger

**Canton 3.4 Change**: The separate `PartyToKeyMapping` transaction has been deprecated. Signing keys are now embedded directly in the `PartyToParticipant` (P2P) mapping.

## CLI Commands

The application provides three commands:

```bash
# Generate a Noise protocol keypair for secure communication
cargo run -- keygen -o <output_file>

# Run the onboarding workflow (create decentralized party)
cargo run -- -c <config_file> onboarding

# Run the contracts workflow (upload DARs and create contracts)
cargo run -- -c <config_file> contracts
```

### Get Help

```bash
cargo run -- --help
cargo run -- keygen --help
cargo run -- onboarding --help
cargo run -- contracts --help
```

## Workflows

Both workflows require all participants to run simultaneously. The coordinator waits for all attestors to connect before proceeding.

### Running a Workflow

**Terminal 1 - Coordinator:**
```bash
cargo run -- -c test-configs/node-1.toml onboarding
```

**Terminal 2 - Attestor 2:**
```bash
cargo run -- -c test-configs/node-2.toml onboarding
```

**Terminal 3 - Attestor 3:**
```bash
cargo run -- -c test-configs/node-3.toml onboarding
```

After onboarding completes, run the contracts workflow with the same pattern:
```bash
cargo run -- -c test-configs/node-1.toml contracts  # Coordinator
cargo run -- -c test-configs/node-2.toml contracts  # Attestor 2
cargo run -- -c test-configs/node-3.toml contracts  # Attestor 3
```

## Configuration

This project uses a distributed configuration system for multi-party setups:

- **Network Configuration** (`network.toml`): Shared topology with all participants, Noise protocol keys, and application settings
- **Node Configuration** (`node-X.toml`): Individual node settings with Canton connection details

### Using Test Configurations

Pre-configured test setups are available in `test-configs/`. See [test-configs/README.md](./test-configs/README.md) for details.

### Creating Custom Configuration

1. **Generate Noise keypairs** for secure communication:
```bash
cargo run -- keygen -o keys/participant-1.key
cargo run -- keygen -o keys/participant-2.key
cargo run -- keygen -o keys/participant-3.key
```

2. **Create network.toml** based on `network.example.toml`:
```toml
[network]
name = "my-network"
protocol_version = "1.0"
port = 9000
coordinator_strategy = "explicit"

[[participants]]
id = "participant-1"
name = "Participant 1"
role = "coordinator"
address = "10.0.1.100"
port = 9001
public_key = "<hex-encoded-public-key-from-keygen>"

[[participants]]
id = "participant-2"
name = "Participant 2"
address = "10.0.1.101"
port = 9002
public_key = "<hex-encoded-public-key-from-keygen>"

# Add more participants as needed (minimum 3 required)

[timeouts]
handshake_timeout_secs = 30
message_timeout_secs = 120
connection_retry_attempts = 3
connection_retry_delay_secs = 5

[application]
party_id_prefix = "my-network"
namespace_key_name = "my-network-namespace"
daml_key_name = "my-network-daml-transactions"
operator_party_hint = "operator"

# Define contracts to create (optional)
# [[application.contracts]]
# id = "my-contract"
# name = "MyContract"
# package_id = "#my-package"
# module_name = "My.Module"
# entity_name = "MyTemplate"
# fields = [...]
```

3. **Create node-X.toml** for each participant based on `node.example.toml`:
```toml
network_config = "network.toml"

[node]
node_id = "participant-1"
static_key_file = "keys/participant-1.key"
listen_address = "0.0.0.0"

[canton]
admin_api_host = "localhost"
admin_api_port = 5001
ledger_api_host = "localhost"
ledger_api_port = 5002
synchronizer = "global"
ledger_api_user_id = "ledger-api-user"
# ledger_api_token = "your-jwt-token-here"  # Optional
```

## Quick Start

```bash
# 1. Generate keys for all participants
mkdir -p keys
cargo run -- keygen -o keys/participant-1.key
cargo run -- keygen -o keys/participant-2.key
cargo run -- keygen -o keys/participant-3.key

# 2. Update test-configs/network.toml with the generated public keys

# 3. Run onboarding (in 3 separate terminals)
cargo run -- -c test-configs/node-1.toml onboarding  # Terminal 1
cargo run -- -c test-configs/node-2.toml onboarding  # Terminal 2
cargo run -- -c test-configs/node-3.toml onboarding  # Terminal 3

# 4. After onboarding completes, run contracts workflow
cargo run -- -c test-configs/node-1.toml contracts   # Terminal 1
cargo run -- -c test-configs/node-2.toml contracts   # Terminal 2
cargo run -- -c test-configs/node-3.toml contracts   # Terminal 3
```

## Documentation

- **[docs/NOISE_PROTOCOL_COMMUNICATION.md](./docs/NOISE_PROTOCOL_COMMUNICATION.md)** - Comprehensive guide to secure peer-to-peer communication architecture
- **[docs/CODING-STANDARDS.md](./docs/CODING-STANDARDS.md)** - Project coding standards and style guide
- **[network.example.toml](./network.example.toml)** - Example network topology configuration
- **[node.example.toml](./node.example.toml)** - Example node configuration
- **[test-configs/](./test-configs/)** - Pre-configured test setup for 3 participants

## Development

### Run Tests

```bash
cargo test
```

### Run Tests with Output

```bash
cargo test -- --nocapture
```

### Run Specific Test

```bash
cargo test test_name
```

## Code Quality

### Run Clippy (Strict Mode)

This project uses strict clippy settings. Run clippy to check for warnings:

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

### Auto-fix Clippy Issues

```bash
cargo clippy --fix --all-targets --all-features -- -D warnings
```

### Format Code

```bash
cargo fmt
```

### Check Formatting Without Modifying Files

```bash
cargo fmt -- --check
```
