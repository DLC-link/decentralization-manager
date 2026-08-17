#!/bin/bash

# Integration test orchestrator.
# Downloads and starts a Splice localnet, spins up 3 dec-party-manager
# instances, and runs the full workflow suite against them.

set -eu

# ---------------------------------------------------------------------------
# Flag parsing
# ---------------------------------------------------------------------------

TARGET=localnet
VERBOSE=0
while [[ "$#" -gt 0 ]]; do
    case "$1" in
        -v|--verbose)
            VERBOSE=1
            shift
            ;;
        --target)
            if [[ "$#" -lt 2 ]]; then
                echo "--target requires an argument" >&2
                exit 1
            fi
            TARGET="$2"
            shift 2
            ;;
        --target=*)
            TARGET="${1#*=}"
            shift
            ;;
        -h|--help)
            cat <<EOF
Usage: $(basename "$0") [-v|--verbose] [--target <localnet|devnet>] [-h|--help]

Boots a Splice localnet (or connects to devnet), spawns 3 dec-party-manager
instances, and runs the governance workflow e2e (cargo test --profile release-ci).

Output is filtered by default so the Given-When-Then scenario trace stays
readable. The dec-party-manager processes and Canton/Noise libraries log
only at WARN+ unless --verbose is passed.

Options:
  -v, --verbose   Show INFO output from dec-party-manager processes,
                  the cargo test runner, and the e2e crate. Useful when
                  diagnosing a stuck or failing run. Sets:
                  RUST_LOG=dec_party_manager=info,tokio_noise=error,
                          hyper_noise=error,governance_workflows=info

  --target <localnet|devnet>
                  Which target to test against (default: localnet).
                  localnet: boots a Splice docker-compose stack.
                  devnet: requires tunnel setup + ~/.config/dec-party-manager/devnet.env.

  -h, --help      Show this help and exit.

If RUST_LOG is already set in the environment, it overrides this preset.
EOF
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            echo "Run $(basename "$0") --help for usage." >&2
            exit 1
            ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Operate on THIS script's own repo, regardless of the caller's CWD. Without
# this, invoking the script by path from another checkout (e.g. a git worktree's
# run.sh called from the main clone) makes cargo build the CALLER's workspace
# into the caller's target/ — so the binary is missing from $SCRIPT_DIR/target/
# and, worse, the wrong branch's code gets tested. Every build/output/relative
# path below assumes repo root == SCRIPT_DIR.
cd "$SCRIPT_DIR"

case "$TARGET" in
    localnet)
        ENV_FILE="$SCRIPT_DIR/integration-tests/env.sh"
        ;;
    devnet)
        ENV_FILE="$SCRIPT_DIR/integration-tests/devnet.env.sh"
        ;;
    *)
        echo "Unknown --target: $TARGET (expected localnet|devnet)" >&2
        exit 1
        ;;
esac

source "$ENV_FILE"

trap cleanup EXIT

check_prerequisites

# Port-free check applies to both targets: PR #142 moved devnet to a
# bare-process bringup (no longer docker-compose), so the same 6 ports
# (8081-8083 HTTP + 9000-9002 Noise) are bound directly by DecMan on devnet
# too. An orphan DecMan from a previous run (especially from another worktree
# of the same repo) would otherwise:
#   - hold the port,
#   - silently steal the bash bringup's `wait_for_server` TCP readiness probe
#     (so the new DecMan's EADDRINUSE death is invisible),
#   - and respond to subsequent traffic with its own (stale, possibly
#     wrong-revision) Noise keys — producing peer-decrypt errors that look
#     like the new DecMan is misconfigured.
# Fail fast here instead.
check_decman_ports_free

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
# When TARGET=localnet the binary is compiled with --features test-mode so it
# can select the permissive MockValidator used by the localnet e2e suite.
# Production builds (TARGET=devnet) omit this feature so the real JwtValidator
# is used and no mock auth path is compiled in.
#
# IMPORTANT: --features test-mode (or its absence) must match between `cargo
# build` and `cargo test`. Mismatching causes cargo to rebuild the binary under
# a different feature unification, overwriting the artifact in
# target/release-ci/. Chaos phases (G1/G2/G7/G9/G3/G4/P1/P2) that respawn the
# binary would then launch whichever variant cargo last built, not the one
# configured for this run.
#
# Uses the release-ci profile (release without LTO, codegen-units=16) so CI
# build time stays low. The shipped release profile is unchanged.
FEATURES_FLAG=()
if [ "$TARGET" = "localnet" ]; then
    FEATURES_FLAG=(--features test-mode)
fi

log_phase "Building release-ci binary (target=$TARGET)"
# `-p decman` selects the server crate: the root is now a virtual workspace
# (so a bare `--features` is rejected), and this skips compiling the decman-cli TUI.
cargo build --profile release-ci -p decman ${FEATURES_FLAG[@]+"${FEATURES_FLAG[@]}"}

if [ ! -f "$BINARY" ]; then
    echo "ERROR: Binary not found at $BINARY"
    exit 1
fi

# Canton bring-up. Both hooks are target-polymorphic: devnet.env.sh redefines
# download_localnet as a no-op and start_localnet as start_canton_tunnels.
log_phase "Starting Canton ($TARGET)"
download_localnet
start_localnet

# dec-party-manager instances
log_phase "Starting dec-party-manager instances"
setup_directories
start_nodes

log_phase "Configuring peers"
configure_peers

# Workflows
log_phase "Running e2e: ${DECPM_E2E_TEST:-governance_workflows_e2e} (Rust)"

export P1_HTTP P2_HTTP P3_HTTP
export P1_NOISE P2_NOISE P3_NOISE
export P1_PARTICIPANT_ID P2_PARTICIPANT_ID P3_PARTICIPANT_ID
export DEV_DIR

# PIDs are populated by start_nodes after the env file is sourced, so they
# can only be exported here. Chaos phases (G1-G9, P1-P2) read them on both
# targets to kill and respawn the bare DecMan binary.
export P1_PID="${PIDS[0]}"
export P2_PID="${PIDS[1]}"
export P3_PID="${PIDS[2]}"

# The remaining exports are localnet-only:
# - MOCK_TOKEN: hardcoded test JWT. Devnet's Rust runner uses
#   KeycloakRefresher (reads DECPM_KEYCLOAK_* directly) instead.
# - P{1,2,3}_CANTON_LEDGER/ADMIN and BINARY: env.sh declares these without
#   exporting; the chaos respawn path needs them in child env. devnet.env.sh
#   already exports its own values, so this block leaves devnet alone.
if [ "$TARGET" = "localnet" ]; then
    export MOCK_TOKEN
    export P1_CANTON_LEDGER P2_CANTON_LEDGER P3_CANTON_LEDGER
    export P1_CANTON_ADMIN P2_CANTON_ADMIN P3_CANTON_ADMIN
    export BINARY
fi

# $FEATURES_FLAG must match the value used in `cargo build` above to avoid
# cargo rebuilding the binary under a different feature unification and
# overwriting the artifact. See the build comment block for full details.
#
# DECPM_E2E_TEST picks which #[ignore]d test in the governance_workflows binary
# to run. Defaults to the full suite (governance_workflows_e2e); the wrapper
# run-external-party.sh sets it to external_party_e2e to exercise only that one
# feature. Run with `--exact` so the name matches exactly one test, never a
# substring set.
E2E_TEST="${DECPM_E2E_TEST:-governance_workflows_e2e}"

# Run the selected e2e test against the already-running stack.
run_e2e() {
    cargo test --profile release-ci -p decman ${FEATURES_FLAG[@]+"${FEATURES_FLAG[@]}"} \
        --test governance_workflows -- --ignored --nocapture --exact "$E2E_TEST"
}

# DECPM_E2E_HOLD=1 switches to interactive mode: bring the stack up, wait for the
# operator to press ENTER before running the test, then hold everything up for
# manual inspection until Ctrl-C tears it down. Unset (the default) preserves the
# original run-and-exit behaviour used by CI.
if [ -n "${DECPM_E2E_HOLD:-}" ]; then
    # Ctrl-C -> INT trap -> exit -> the already-installed EXIT trap (cleanup)
    # runs exactly once.
    trap 'exit 0' INT

    echo ""
    echo "=========================================="
    echo "Nodes are UP. HTTP endpoints:"
    echo "  P1  http://localhost:${P1_HTTP}"
    echo "  P2  http://localhost:${P2_HTTP}"
    echo "  P3  http://localhost:${P3_HTTP}"
    echo "=========================================="
    read -r -p "Press ENTER to run ${E2E_TEST}  (or Ctrl-C to quit)... " _ || true

    # Don't let a test failure abort before the hold — the operator still wants
    # the stack up to inspect what happened.
    set +e
    run_e2e
    test_rc=$?
    set -e
    echo ""
    if [ "$test_rc" -eq 0 ]; then
        echo "✓ ${E2E_TEST} passed."
    else
        echo "✗ ${E2E_TEST} failed (exit ${test_rc})."
    fi

    echo ""
    echo "Stack still running for inspection (endpoints above). Press Ctrl-C to tear down."
    # Block until Ctrl-C. Portable busy-wait (macOS/BSD sleep has no 'infinity').
    while true; do sleep 1; done
else
    run_e2e
    echo ""
    echo "=========================================="
    echo "Integration tests completed successfully!"
    echo "=========================================="
fi
