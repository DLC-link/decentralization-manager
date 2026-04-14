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

CARGO_INCREMENTAL=0 RUSTFLAGS="-C instrument-coverage" LLVM_PROFILE_FILE="target/coverage/%p-%m.profraw" cargo test

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
