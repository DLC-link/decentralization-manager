#!/bin/bash

set -e

# Auto-detect llvm-profdata location
LLVM_PATH=""
if ! command -v llvm-profdata &>/dev/null; then
    # Try Homebrew LLVM installations
    for dir in /opt/homebrew/Cellar/llvm@*/*/bin /usr/local/Cellar/llvm@*/*/bin; do
        if [ -x "$dir/llvm-profdata" ]; then
            LLVM_PATH="$dir"
            break
        fi
    done
fi

# Only set RUSTFLAGS if `-C instrument-coverage` isn't already present.
# In CI it's set at the job level so clippy and this step share one
# fingerprint; overriding here would diverge them and force a full
# recompile of the dep graph (~3 min). Locally, RUSTFLAGS is usually
# unset, so we add it.
if [[ "${RUSTFLAGS:-}" != *"instrument-coverage"* ]]; then
    export RUSTFLAGS="${RUSTFLAGS:-} -C instrument-coverage"
fi

# `--all-features` keeps the cargo fingerprint aligned with the priming
# (`cargo test --no-run --all-features`) and clippy (`cargo clippy
# --all-features`) steps in CI. Without it, the workspace crate's
# feature set differs across steps and cargo rebuilds dec-party-manager
# from scratch here (~38s wasted). Functionally a no-op: the only
# feature, `test-mode`, gates code on `cfg(any(test, feature = "test-mode"))`,
# and `cargo test` already activates the `test` arm.
CARGO_INCREMENTAL=0 LLVM_PROFILE_FILE="target/coverage/%p-%m.profraw" cargo test --all-features

GRCOV_ARGS=(. --binary-path ./target/debug/ -s . -t covdir --branch --ignore-not-existing -o coverage.json)
if [ -n "$LLVM_PATH" ]; then
    GRCOV_ARGS+=(--llvm-path "$LLVM_PATH")
fi

grcov "${GRCOV_ARGS[@]}"

COVERAGE=$(jq '.coveragePercent' coverage.json)
echo "Code coverage: $COVERAGE%"

# NOTE: this measures UNIT-test coverage only. The end-to-end governance suite
# (tests/governance_workflows.rs) is `#[ignore]`d and runs separately in the
# `integration-test` CI job, so it contributes nothing to this number — do not
# read it as whole-product coverage. The threshold below is a regression floor
# for unit-tested code, not a coverage target.
if (( $(echo "$COVERAGE < 6" | bc -l) )); then
    echo "Coverage $COVERAGE% is below threshold of 6%"
    exit 1
fi
