# Contributing to Canton Decentralized Party Manager

Thanks for your interest in contributing! This document explains how to get set
up, the conventions we follow, and how to get your change merged.

By participating in this project you agree to abide by our
[Code of Conduct](CODE_OF_CONDUCT.md). Contributions are accepted under the
project's [Apache License 2.0](../LICENSE).

## Table of contents

- [Reporting bugs & requesting features](#reporting-bugs--requesting-features)
- [Reporting security issues](#reporting-security-issues)
- [Development setup](#development-setup)
- [Building, testing, and linting](#building-testing-and-linting)
- [Coding standards](#coding-standards)
- [Commit & branch conventions](#commit--branch-conventions)
- [Submitting a pull request](#submitting-a-pull-request)

## Reporting bugs & requesting features

Use the GitHub [issue templates](../.github/ISSUE_TEMPLATE) — they prompt for the
details we need (reproduction steps, environment, logs). Before opening a new
issue, please search existing issues to avoid duplicates.

## Reporting security issues

**Do not open a public issue for security vulnerabilities.** Please follow the
process in [SECURITY.md](SECURITY.md).

## Development setup

### Prerequisites

- **Rust** — recent stable toolchain (the crate uses edition 2024, Rust ≥ 1.85).
- **Node.js** — `^20.19.0 || >=22.12.0` (the React frontend is built automatically by `build.rs`).
- **Docker** — required to run the full integration test (boots a Splice localnet).
- **Daml** — the `dpm` CLI (Daml SDK `3.4.11`); **Java 17+** is required for Daml tests.
- Access to the Canton ledger/admin APIs of a participant node for running the app against a live network (optional for most code changes).

> **Note:** some Rust dependencies (`canton-common`, `canton-proto-rs`,
> `canton-registry`, `keycloak`) are currently consumed from a separate
> repository. See the README for the dependency source and access requirements.

### Repository layout

- `crates/decman/` — Rust server crate (HTTP server, Noise P2P, Canton gRPC,
  workflows); binary `dec-party-manager`.
  - `crates/decman/src/` — server source.
  - `crates/decman/frontend/` — React + Vite UI (embedded into the binary at build time).
  - `crates/decman/migrations/` — SQLx database migrations.
- `crates/common/` — shared wire DTOs + Canton-ID helpers (used by `decman` and `decman-cli`).
- `crates/decman-cli/` — terminal UI client.
- `daml/` — Daml governance packages and tests.
- `integration-tests/` — end-to-end test harness and scripts.
- `docs/` — architecture, integration, and use-case documentation.

## Building, testing, and linting

```bash
# Debug / release build (the frontend is built automatically via build.rs)
cargo build
cargo build --release

# Run unit tests (includes the integration harness helpers; the e2e test
# itself is #[ignore]'d and runs via the script below)
cargo test

# Lint — must be clean with no warnings
cargo clippy --all-targets --all-features -- -D warnings

# Format
cargo fmt
```

### Integration tests

The full suite boots a Splice localnet in Docker, spawns three
`dec-party-manager` instances, and runs an end-to-end governance workflow:

```bash
./integration-tests/run.sh            # quiet mode (Given-When-Then trace)
./integration-tests/run.sh --verbose  # full logs
./integration-tests/run.sh --help
```

## Coding standards

All Rust changes must pass CI before review:

- `cargo fmt -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`
- No compiler warnings.

Key conventions:

- **Imports:** `mod` declarations first, then `use` groups ordered std →
  third-party → local, separated by blank lines; merge multiple imports from the
  same module with braces.
- **Format strings:** prefer inline captures — `format!("{path}")` — over
  positional args, but don't introduce a variable just to inline it.
- **Error handling:** use `anyhow::Result` for application errors and
  `thiserror` for custom error types. **Avoid `unwrap()` / `expect()`** —
  including in tests; return `Result` and use `?`.
- **Logging:** use `tracing` (`info!`/`warn!`/`error!`), never `println!`/`eprintln!`.
- **Cargo.toml:** dependencies and their features are kept in strict
  alphabetical order.
- **Comments:** only where the logic isn't self-evident; remove dead code rather
  than commenting it out.

## Commit & branch conventions

Commit messages (past tense) and branch names follow:

```
<type>(<scope>): <subject>      # commit
<type>/<scope>/<subject>        # branch
```

- **type:** `feat`, `fix`, `docs`, `style`, `refact`, `perf`, `test`, `chore`
- **scope:** the area touched, e.g. `api`, `ui`, `core`, `server`, `workflow`

Examples:

```
feat(ui): added peer version column to the peers table
fix(workflow): handled durable cancel during topology retry
```

Keep commits small, focused, and atomic. Stage files intentionally
(`git add <file>`), not `git add .`.

## Submitting a pull request

1. Fork the repo and create a branch following the naming convention above.
2. Make your change with tests, and ensure `fmt`, `clippy`, and `test` all pass
   locally.
3. Keep the PR focused; large unrelated changes are harder to review.
4. Fill out the [pull request template](../.github/PULL_REQUEST_TEMPLATE.md) and
   link any related issues (e.g. `Closes #123`).
5. A maintainer will review; please be responsive to feedback.

Thank you for contributing! 🎉
