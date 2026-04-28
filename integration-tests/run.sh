#!/bin/bash

# Integration test orchestrator.
# Downloads and starts a Splice localnet, spins up 3 dec-party-manager
# instances, and runs the full workflow suite against them.

set -e

# ---------------------------------------------------------------------------
# Flag parsing
# ---------------------------------------------------------------------------

VERBOSE=0
for arg in "$@"; do
    case "$arg" in
        -v|--verbose)
            VERBOSE=1
            ;;
        -h|--help)
            cat <<EOF
Usage: $(basename "$0") [-v|--verbose] [-h|--help]

Boots a Splice localnet, spawns 3 dec-party-manager instances, and runs
the governance workflow e2e (cargo test --release).

Output is filtered by default so the Given-When-Then scenario trace stays
readable. The dec-party-manager processes and Canton/Noise libraries log
only at WARN+ unless --verbose is passed.

Options:
  -v, --verbose   Show INFO output from dec-party-manager processes,
                  the cargo test runner, and the e2e crate. Useful when
                  diagnosing a stuck or failing run. Sets:
                  RUST_LOG=dec_party_manager=info,tokio_noise=error,
                          hyper_noise=error,governance_workflows=info

  -h, --help      Show this help and exit.

If RUST_LOG is already set in the environment, it overrides this preset.
EOF
            exit 0
            ;;
        *)
            echo "Unknown argument: $arg" >&2
            echo "Run $(basename "$0") --help for usage." >&2
            exit 1
            ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$SCRIPT_DIR/integration-tests/env.sh"

trap cleanup EXIT

check_prerequisites
check_dpm_ports_free

# ---------------------------------------------------------------------------
# Verbosity preset
# ---------------------------------------------------------------------------
# Applies to BOTH the 3 dec-party-manager subprocesses (env.sh:start_nodes
# uses ${RUST_LOG:-...}, so an exported value wins) AND the cargo test
# process. An externally-set RUST_LOG always overrides this preset.
if [ -z "${RUST_LOG:-}" ]; then
    if [ "$VERBOSE" = 1 ]; then
        export RUST_LOG="dec_party_manager=info,tokio_noise=error,hyper_noise=error,governance_workflows=info"
    else
        # Quiet default: only WARN+ from everything except the GWT scenario
        # DSL output and per-phase headers from the test crate. The test
        # crate's helpers (invitations, http) stay at WARN — readers see the
        # scenario structure without helper-internal chatter. Pass --verbose
        # to surface the helpers and the dec-party-manager INFO stream.
        export RUST_LOG="warn,governance_workflows::common::scenario=info,governance_workflows::common::phases=info"
    fi
fi

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

cargo test --release --test governance_workflows -- --ignored --nocapture

echo ""
echo "=========================================="
echo "Integration tests completed successfully!"
echo "=========================================="
