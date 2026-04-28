#!/bin/bash

# Integration test orchestrator.
# Downloads and starts a Splice localnet, spins up 3 dec-party-manager
# instances, and runs the full workflow suite against them.

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$SCRIPT_DIR/integration-tests/env.sh"

trap cleanup EXIT

check_prerequisites
check_dpm_ports_free

# Build
log_phase "Building release binary"
cargo build --release

if [ ! -f "$BINARY" ]; then
    echo "ERROR: Binary not found at $BINARY"
    exit 1
fi

# Localnet
log_phase "Starting localnet"
download_localnet
start_localnet
wait_for_localnet

# dec-party-manager instances
log_phase "Starting dec-party-manager instances"
setup_directories
start_nodes

log_phase "Configuring peers"
configure_peers

# Workflows
log_phase "Running governance workflow e2e (Rust)"

export P1_HTTP P2_HTTP P3_HTTP
export P1_NOISE P2_NOISE P3_NOISE
export P1_PARTICIPANT_ID P2_PARTICIPANT_ID P3_PARTICIPANT_ID
export MOCK_TOKEN
export RUST_LOG="${RUST_LOG:-info}"

cargo test --release --test governance_workflows -- --ignored --nocapture

echo ""
echo "=========================================="
echo "Integration tests completed successfully!"
echo "=========================================="
