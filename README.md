# Canton Decentralized Party Onboarding Automatization

Canton workflow automation - porting Scala scripts to Rust for decentralized namespace setup and governance.

## Table of Contents

- [Project Overview](#project-overview)
- [Documentation](#documentation)
- [Setup](#setup)
  - [Clone Canton APIs](#clone-canton-apis)
  - [Clone Google APIs](#clone-google-apis)
  - [Configuration](#configuration)
- [Usage](#usage)
  - [Run All Steps in Sequence](#run-all-steps-in-sequence)
  - [Run Individual Steps](#run-individual-steps)
  - [Get Help](#get-help)
- [Development](#development)
  - [Run Tests](#run-tests)
  - [Run Tests with Output](#run-tests-with-output)
  - [Run Specific Test](#run-specific-test)
- [Code Quality](#code-quality)
  - [Coding Standards](#coding-standards)
  - [Run Clippy (Strict Mode)](#run-clippy-strict-mode)
  - [Auto-fix Clippy Issues](#auto-fix-clippy-issues)
  - [Format Code](#format-code)
  - [Check Formatting Without Modifying Files](#check-formatting-without-modifying-files)
- [Reference](#reference)
  - [List Services](#list-services)
    - [Admin API](#admin-api)
    - [Ledger API](#ledger-api)

## Project Overview

This project ports Canton Scala scripts to Rust, implementing a multi-party decentralized namespace setup for CBTC (Canton-based Bitcoin) governance. The workflow includes:

1. **Step 1**: Upload DARs and generate cryptographic keys
2. **Step 1a**: Create topology proposals (DNS, P2P, PTK)
3. **Steps 2-3a**: Multi-party signing and submission of topology proposals
4. **Steps 3b-5**: Prepare, sign, and execute ledger submissions

For detailed implementation plans and progress, see [TODO.md](./TODO.md).

## Documentation

- **[TODO.md](./TODO.md)** - Detailed implementation plan, API mappings, and step-by-step breakdown
- **[CODING-STANDARDS.md](./CODING-STANDARDS.md)** - Project coding standards and style guide
- **[config.example.toml](./config.example.toml)** - Example configuration file

## Setup

### List Avaliable Services

#### Admin API

- **Automated Multi-Party Onboarding**: Orchestrates the complete workflow for setting up decentralized party participation
- **Dynamic Participant Support**: Supports any number of participants (N ≥ 3), with automatic majority threshold calculation
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
- **Coordinator** acts as server, orchestrating the workflow (is also an attestor)
- **Attestors** connect as clients, executing commands and returning results
- **Mutual Authentication** via static keypairs (secp256k1)
- **Encrypted Channels** using ChaChaPoly-1305 AEAD cipher

See the [Noise Protocol Communication Architecture](docs/NOISE_PROTOCOL_COMMUNICATION.md) for detailed information.

### Visual Workflow

![Multi-Party Decentralized Setup Workflow](docs/flowchart.png)

The complete workflow diagram showing all phases, Noise protocol communications, and data flows between coordinator and attestors.

## Table of Contents

- [Project Overview](#project-overview)
- [Documentation](#documentation)
- [Setup](#setup)
  - [Configuration](#configuration)
- [Usage](#usage)
  - [Run All Steps in Sequence](#run-all-steps-in-sequence)
  - [Run Individual Steps](#run-individual-steps)
  - [Generate Noise Protocol Keys](#generate-noise-protocol-keys)
  - [Get Help](#get-help)
- [Development](#development)
  - [Run Tests](#run-tests)
  - [Run Tests with Output](#run-tests-with-output)
  - [Run Specific Test](#run-specific-test)
- [Code Quality](#code-quality)
  - [Coding Standards](#coding-standards)
  - [Run Clippy (Strict Mode)](#run-clippy-strict-mode)
  - [Auto-fix Clippy Issues](#auto-fix-clippy-issues)
  - [Format Code](#format-code)
  - [Check Formatting Without Modifying Files](#check-formatting-without-modifying-files)

## Project Overview

This project ports Canton Scala scripts to Rust, implementing a multi-party decentralized namespace setup for CBTC (Canton-based Bitcoin) governance. The workflow includes:

1. **Step 1**: Upload DARs and generate cryptographic keys
   - Automatically uploads all `.dar` files from the `dars/` directory
2. **Step 1a**: Create topology proposals (DNS, P2P with embedded keys - Canton 3.4+)
3. **Steps 2-3a**: Multi-party signing and submission of topology proposals
4. **Steps 3b-5**: Prepare, sign, and execute ledger submissions

**Canton 3.4 Change**: The separate `PartyToKeyMapping` transaction has been deprecated. Signing keys are now embedded directly in the `PartyToParticipant` (P2P) mapping.

**Status**: All implementation phases complete. For detailed documentation, see [docs/TODO.md](./docs/TODO.md).

## Documentation

- **[docs/NOISE_PROTOCOL_COMMUNICATION.md](./docs/NOISE_PROTOCOL_COMMUNICATION.md)** - Comprehensive guide to secure peer-to-peer communication architecture
- **[docs/TODO.md](./docs/TODO.md)** - Detailed implementation plan, API mappings, and step-by-step breakdown
- **[docs/CODING-STANDARDS.md](./docs/CODING-STANDARDS.md)** - Project coding standards and style guide
- **[network.example.toml](./network.example.toml)** - Example network topology configuration
- **[node.example.toml](./node.example.toml)** - Example node configuration
- **[test-configs/](./test-configs/)** - Pre-configured test setup (default: 3 participants, easily adjustable)

## Setup

## Configuration

This project uses a distributed configuration system for multi-party setups:

- **Network Configuration** (`network.toml`): Shared topology with all participants and Noise protocol keys
- **Node Configuration** (`node-X.toml`): Individual node settings with Canton connection details

### Using Test Configurations

Pre-configured test setups are available in `test-configs/`:

### Run All Steps in Sequence

```sh
# Run commands for different participants
cargo run -- -c test-configs/node-1.toml <command>  # Coordinator
cargo run -- -c test-configs/node-2.toml <command>  # Attestor 2
cargo run -- -c test-configs/node-3.toml <command>  # Attestor 3
```

Response

```
com.digitalasset.canton.admin.health.v30.StatusService
com.digitalasset.canton.admin.sequencer.v30.SequencerStatusService
com.digitalasset.canton.connection.v30.ApiInfoService
com.digitalasset.canton.crypto.admin.v30.VaultService
com.digitalasset.canton.sequencer.admin.v30.SequencerAdministrationService
com.digitalasset.canton.sequencer.admin.v30.SequencerPruningAdministrationService
com.digitalasset.canton.topology.admin.v30.IdentityInitializationService
com.digitalasset.canton.topology.admin.v30.TopologyAggregationService
com.digitalasset.canton.topology.admin.v30.TopologyManagerReadService
com.digitalasset.canton.topology.admin.v30.TopologyManagerWriteService
grpc.reflection.v1alpha.ServerReflection
```

#### Ledger API

Command

1. **Generate Noise keypairs** for secure communication:
```sh
cargo run -- keygen -o keys/participant-1.key
```

2. **Create network.toml** based on `network.example.toml`:
```toml
[network]
name = "my-network"
coordinator_strategy = "explicit"

[[participants]]
id = "participant-1"
role = "coordinator"
public_key = "<hex-encoded-public-key>"
# ... add as many participants as needed (minimum 3 required)
```

### Clone Canton APIs

[node]
node_id = "participant-1"
static_key_file = "keys/participant-1.key"
listen_address = "0.0.0.0"

[canton]
admin_api_host = "localhost"
admin_api_port = 5012
ledger_api_host = "localhost"
ledger_api_port = 5011
synchronizer = "global"
# Optional: JWT token for Ledger API authentication
# ledger_api_token = "your-jwt-token-here"
```

### Clone Google APIs

```sh
mkdir -p proto/googleapis
git clone https://github.com/googleapis/googleapis.git proto/googleapis
```

## Configuration

Create a configuration file based on the example:

### Run All Steps in Sequence

```sh
cp config.example.toml config.toml
```

Edit `config.toml` with your Canton connection details:

```toml
[connection]
admin_api_host = "localhost"
admin_api_port = 5001
ledger_api_host = "localhost"
ledger_api_port = 5002
# token = "your-oauth-token-here"  # Optional

[topology]
synchronizer = "global"
```

## Run The App

### Run All Steps in Sequence

```sh
cargo run --release -- -c config.toml all
```

### Run Individual Steps

```sh
# Step 1: Upload DARs
cargo run --release -- -c config.toml upload-dars

# Step 1: Generate keys and export participant ID
cargo run --release -- -c config.toml generate-keys

# Step 1a: Create topology proposals
cargo run --release -- -c config.toml create-proposals

# Step 2: Sign DNS proposals
cargo run --release -- -c config.toml sign-dns-proposals

# Step 2a: Submit DNS proposals
cargo run --release -- -c config.toml submit-dns-proposals

# Step 3: Sign P2P and PTK proposals
cargo run --release -- -c config.toml sign-p2p-ptk-proposals

# Step 3a: Submit final proposals
cargo run --release -- -c config.toml submit-final-proposals

# Step 3b: Prepare ledger submissions
cargo run --release -- -c config.toml prepare-submissions

# Step 4: Sign ledger submissions
cargo run --release -- -c config.toml sign-submissions

# Step 5: Execute ledger submissions
cargo run --release -- -c config.toml execute-submissions
```

### Get Help

```sh
cargo run -- --help
```

## Development

### Run Tests

```sh
cargo test
```

### Run Tests with Output

```sh
cargo test -- --nocapture
```

### Run Specific Test

```sh
cargo test test_name
```

## Code Quality

### Run Clippy (Strict Mode)

This project uses strict clippy settings. Run clippy to check for warnings:

```sh
cargo clippy --all-targets --all-features -- -D warnings
```

### Auto-fix Clippy Issues

```sh
cargo clippy --fix --all-targets --all-features -- -D warnings
```

### Format Code

```sh
cargo fmt
```

### Check Formatting Without Modifying Files

```sh
cargo fmt -- --check
```
