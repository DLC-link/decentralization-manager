# Canton Decentralized Party Onboarding Automatization

A Rust-based automation tool for multi-party decentralized namespace setup in Canton blockchain networks. This project streamlines the complex process of onboarding multiple parties to a Canton-based Central Bank Digital Currency (CBTC) governance system by automating topology management, cryptographic key generation, and ledger operations.

## Key Features

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
