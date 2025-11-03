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

### List Available Services

#### Participant Admin API (Port 5012)

This is the primary endpoint for participant operations.

Command

```sh
grpcurl -plaintext localhost:5012 list
```

Response

```
com.digitalasset.canton.admin.health.v30.StatusService
com.digitalasset.canton.admin.participant.v30.EnterpriseParticipantReplicationService
com.digitalasset.canton.admin.participant.v30.InspectionService
com.digitalasset.canton.admin.participant.v30.PackageService
com.digitalasset.canton.admin.participant.v30.PartyManagementService
com.digitalasset.canton.admin.participant.v30.PingPongService
com.digitalasset.canton.admin.participant.v30.PruningService
com.digitalasset.canton.admin.participant.v30.RepairService
com.digitalasset.canton.admin.participant.v30.ResourceManagementService
com.digitalasset.canton.admin.participant.v30.TrafficControlService
com.digitalasset.canton.connection.v30.ApiInfoService
com.digitalasset.canton.crypto.admin.v30.VaultService
com.digitalasset.canton.topology.admin.v30.IdentityInitializationService
com.digitalasset.canton.topology.admin.v30.TopologyAggregationService
com.digitalasset.canton.topology.admin.v30.TopologyManagerReadService
com.digitalasset.canton.topology.admin.v30.TopologyManagerWriteService
grpc.reflection.v1alpha.ServerReflection
```

#### Participant Ledger API (Port 5011)

Used for submitting commands and reading ledger state.

Command

```sh
grpcurl -plaintext localhost:5011 list
```

Response

```
com.daml.ledger.api.v2.CommandCompletionService
com.daml.ledger.api.v2.CommandService
com.daml.ledger.api.v2.CommandSubmissionService
com.daml.ledger.api.v2.EventQueryService
com.daml.ledger.api.v2.PackageService
com.daml.ledger.api.v2.StateService
com.daml.ledger.api.v2.UpdateService
com.daml.ledger.api.v2.VersionService
com.daml.ledger.api.v2.admin.CommandInspectionService
com.daml.ledger.api.v2.admin.IdentityProviderConfigService
com.daml.ledger.api.v2.admin.MeteringReportService
com.daml.ledger.api.v2.admin.PackageManagementService
com.daml.ledger.api.v2.admin.ParticipantPruningService
com.daml.ledger.api.v2.admin.PartyManagementService
com.daml.ledger.api.v2.admin.UserManagementService
com.daml.ledger.api.v2.testing.TimeService
com.digitalasset.canton.admin.participant.v30.InteractivePrepareAndSubmitService
grpc.health.v1.Health
grpc.reflection.v1alpha.ServerReflection
```

#### Sequencer Admin API (Port 5002)

Used for sequencer-level operations.

- **Automated Multi-Party Onboarding**: Orchestrates the complete workflow for setting up decentralized party participation
- **gRPC Integration**: Native Canton Admin and Ledger API integration using Protocol Buffers
- **Cryptographic Key Management**: Automated generation and management of cryptographic keys for secure party identification
- **Topology Management**: Handles DNS, P2P, and Participant Topology Key (PTK) proposal creation, signing, and submission
- **Ledger Operations**: Manages preparation, signing, and execution of ledger submissions
- **Configuration-Driven**: Flexible TOML-based configuration for different Canton environments
- **Production Ready**: Includes comprehensive error handling, logging, and code quality tooling

## Architecture

This tool implements a port of Canton Scala scripts to Rust, providing a more performant and memory-safe alternative for automated party onboarding. It follows a step-based workflow that ensures proper ordering of operations and handles the complex interdependencies between topology changes and ledger state modifications.

## Table of Contents

- [Project Overview](#project-overview)
- [Documentation](#documentation)
- [Setup](#setup)
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

## Project Overview

This project ports Canton Scala scripts to Rust, implementing a multi-party decentralized namespace setup for CBTC (Canton-based Bitcoin) governance. The workflow includes:

1. **Step 1**: Upload DARs and generate cryptographic keys
2. **Step 1a**: Create topology proposals (DNS, P2P, PTK)
3. **Steps 2-3a**: Multi-party signing and submission of topology proposals
4. **Steps 3b-5**: Prepare, sign, and execute ledger submissions

**Status**: All implementation phases complete. For detailed documentation, see [docs/TODO.md](./docs/TODO.md).

## Documentation

- **[docs/TODO.md](./docs/TODO.md)** - Detailed implementation plan, API mappings, and step-by-step breakdown
- **[docs/CODING-STANDARDS.md](./docs/CODING-STANDARDS.md)** - Project coding standards and style guide
- **[config.example.toml](./config.example.toml)** - Example configuration file

## Setup

## Configuration

Create a configuration file based on the example:

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

#### Sequencer Public API (Port 5001)

Used for sequencer client connections.

Command

```sh
grpcurl -plaintext localhost:5001 list
```

Response

```
com.digitalasset.canton.connection.v30.ApiInfoService
com.digitalasset.canton.sequencer.api.v30.SequencerAuthenticationService
com.digitalasset.canton.sequencer.api.v30.SequencerConnectService
com.digitalasset.canton.sequencer.api.v30.SequencerService
grpc.health.v1.Health
grpc.reflection.v1alpha.ServerReflection
```

### Clone Canton APIs

```sh
mkdir -p proto/canton
git clone git@github.com:hyperledger-labs/splice.git
cp -r ../splice/canton/community/ledger-api/src/main/protobuf proto/canton
cp -r ../splice/canton/community/admin-api/src/main/protobuf proto/canton
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
