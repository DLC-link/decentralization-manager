#!/bin/bash

# Integration test orchestrator.
# Downloads and starts a Splice localnet, spins up 3 dec-party-manager
# instances, and runs the full workflow suite against them.

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$SCRIPT_DIR/integration-tests/env.sh"

trap cleanup EXIT

check_prerequisites

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
log_phase "Creating decentralized party"
source "$SCRIPT_DIR/integration-tests/create-dec-party.sh"

log_phase "Distributing DARs"
source "$SCRIPT_DIR/integration-tests/distribute-dars.sh"

log_phase "Deploying governance core contract"
source "$SCRIPT_DIR/integration-tests/deploy-gov-core.sh"

log_phase "Testing governance token custody flow"
source "$SCRIPT_DIR/integration-tests/governance-token-custody.sh"

log_phase "Kicking participant-3"
source "$SCRIPT_DIR/integration-tests/kick.sh"

echo ""
echo "=========================================="
echo "Integration tests completed successfully!"
echo "=========================================="
