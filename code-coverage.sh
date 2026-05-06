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

CARGO_INCREMENTAL=0 LLVM_PROFILE_FILE="target/coverage/%p-%m.profraw" cargo test

GRCOV_ARGS=(. --binary-path ./target/debug/ -s . -t covdir --branch --ignore-not-existing -o coverage.json)
if [ -n "$LLVM_PATH" ]; then
    GRCOV_ARGS+=(--llvm-path "$LLVM_PATH")
fi

grcov "${GRCOV_ARGS[@]}"

COVERAGE=$(jq '.coveragePercent' coverage.json)
echo "Code coverage: $COVERAGE%"

if (( $(echo "$COVERAGE < 6" | bc -l) )); then
    echo "Coverage $COVERAGE% is below threshold of 6%"
    exit 1
fi
