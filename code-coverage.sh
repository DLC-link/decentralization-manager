#!/bin/bash

set -e

CARGO_INCREMENTAL=0 RUSTFLAGS="-C instrument-coverage" LLVM_PROFILE_FILE="target/coverage/%p-%m.profraw" cargo test

grcov . --binary-path ./target/debug/ -s . -t covdir --branch --ignore-not-existing -o coverage.json

COVERAGE=$(jq '.coveragePercent' coverage.json)
echo "Code coverage: $COVERAGE%"

if (( $(echo "$COVERAGE < 50" | bc -l) )); then
    echo "Coverage $COVERAGE% is below threshold of 50%"
    exit 1
fi
