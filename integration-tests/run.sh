#!/bin/bash

# Integration test orchestrator.
# Downloads and starts a Splice localnet, spins up 3 dec-party-manager
# instances, and runs the full workflow suite against them.

set -eu

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
the governance workflow e2e (cargo test --profile release-ci).

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
        #
        # `hyper_noise::server` is pinned to ERROR rather than WARN: it logs
        # one warning per failed Noise handshake, and during the
        # configure_peers restart window stale clients spam ~20 of these
        # over ~50s while the mesh converges. They're not actionable for
        # readers of a passing test; --verbose surfaces them.
        export RUST_LOG="warn,hyper_noise::server=error,governance_workflows::common::scenario=info,governance_workflows::common::phases=info"
    fi
fi

# Build
#
# Integration tests run against a permissive `MockValidator` so the test
# binary needs to be compiled with the `test-mode` Cargo feature. Production
# builds intentionally omit this feature so a release binary cannot select
# mock auth at runtime.
#
# Uses the release-ci profile (release without LTO, codegen-units=16) so CI
# build time stays low. The shipped release profile is unchanged.
log_phase "Building release-ci binary (with test-mode feature)"
cargo build --profile release-ci --features test-mode

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
export DEV_DIR

# Export per-node PIDs and canton ports so the chaos phases (G1-G9, P1-P2)
# can kill and respawn dec-party-manager instances. Restart events append the
# new PID to $DEV_DIR/restarted-pids so the cleanup() trap can SIGKILL them
# even if cargo test panics.
export P1_PID="${PIDS[0]}"
export P2_PID="${PIDS[1]}"
export P3_PID="${PIDS[2]}"
export P1_CANTON_LEDGER P2_CANTON_LEDGER P3_CANTON_LEDGER
export P1_CANTON_ADMIN P2_CANTON_ADMIN P3_CANTON_ADMIN
export BINARY

# Pass the test-mode feature here too. Without it, `cargo test` may rebuild
# the bin under a different feature unification, overwriting the test-mode
# binary at target/release-ci/dec-party-manager. Chaos phases that respawn
# the bin (G1/G2/G7/G9/G3/G4/P1/P2) would then spawn a non-test-mode binary
# that uses the real JwtValidator and 401s on every API call with
# "missing bearer token".
cargo test --profile release-ci --features test-mode --test governance_workflows -- --ignored --nocapture

echo ""
echo "=========================================="
echo "Integration tests completed successfully!"
echo "=========================================="
