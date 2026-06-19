#!/bin/bash
# Bring up (or tear down) the 3 dec-party-manager instances for the Playwright
# e2e suite, against the devnet backend. Reuses run.sh's helper functions;
# unlike run.sh it does NOT run `cargo test` — it leaves the nodes running and
# records their PIDs so global-teardown can stop them.
#
# Usage:
#   ./integration-tests/bring-up.sh             # bring up
#   ./integration-tests/bring-up.sh --teardown  # tear down
#
# The PID file path defaults to integration-tests/.e2e-pids and can be
# overridden via the E2E_PID_FILE environment variable.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PID_FILE="${E2E_PID_FILE:-$SCRIPT_DIR/.e2e-pids}"

teardown() {
    if [[ -f "$PID_FILE" ]]; then
        while read -r pid; do
            [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true
        done < "$PID_FILE"
        rm -f "$PID_FILE"
    fi
    # Kill kubectl port-forward grandchildren that were reparented to init when
    # the retry-loop subshells above were killed. Mirrors stop_canton_tunnels in
    # devnet.env.sh (see #142 for why bare `kill` on the loop PID is not enough).
    # KUBE_CONTEXT_DEVNET default must match devnet.env.sh line 188.
    local _ctx="${KUBE_CONTEXT_DEVNET:-ieu-devnet}"
    pkill -f "kubectl --context=$_ctx port-forward svc/participant-ibtc-devnet" 2>/dev/null || true
}

if [[ "${1:-}" == "--teardown" ]]; then
    teardown
    exit 0
fi

# ---------------------------------------------------------------------------
# Build the binary first (devnet uses no --features flag; test-mode is
# localnet-only). Mirrors run.sh lines 151-157 exactly, omitting the
# FEATURES_FLAG block since bring-up is devnet-only.
# ---------------------------------------------------------------------------
cargo build --profile release-ci

# ---------------------------------------------------------------------------
# Source devnet.env.sh — it sources common.sh itself (line 13 of devnet.env.sh),
# validates Keycloak credentials, exports all per-participant port variables
# (P{1,2,3}_HTTP, P{1,2,3}_NOISE, P{1,2,3}_CANTON_*), exports BINARY and
# DEV_DIR, and defines start_canton_tunnels / stop_canton_tunnels,
# download_localnet / start_localnet / stop_localnet, and cleanup.
# common.sh defines setup_directories, start_nodes, configure_peers, stop_nodes,
# and wait_for_server. All of these are used below.
# shellcheck source=integration-tests/devnet.env.sh
source "$SCRIPT_DIR/devnet.env.sh"

# PIDS is appended to by common.sh:start_nodes. Declare it here (devnet.env.sh
# does not declare it; env.sh localnet does — but we are not sourcing env.sh).
PIDS=()

# ---------------------------------------------------------------------------
# Failure trap: clean up tunnels and nodes if the bring-up aborts before
# the PID file is written. Without this, start_localnet's kubectl port-forwards
# and any DPM processes spawned by start_nodes leak when set -e fires mid-run
# (e.g. participant-1 fails to start, configure_peers HTTP error).
#
# _BRING_UP_COMPLETE is set to 1 just before the PID file write. The trap
# checks this flag so it is a no-op on the normal success path (where we
# intentionally leave the stack running for Playwright) and only cleans up on
# a failed/early exit. The explicit --teardown path is unaffected.
#
# stop_nodes is defined in common.sh (line 219); stop_canton_tunnels is defined
# in devnet.env.sh (line 253). Both are sourced transitively above.
# ---------------------------------------------------------------------------
_BRING_UP_COMPLETE=0
_bring_up_failure_cleanup() {
    if [[ "$_BRING_UP_COMPLETE" -eq 0 ]]; then
        echo "bring-up.sh: bring-up failed — cleaning up tunnels and nodes" >&2
        stop_nodes 2>/dev/null || true
        stop_canton_tunnels 2>/dev/null || true
    fi
}
trap _bring_up_failure_cleanup EXIT

# ---------------------------------------------------------------------------
# Mirror run.sh's bring-up sequence (lines 160-170):
#   download_localnet  → no-op on devnet
#   start_localnet     → opens kubectl port-forwards to Canton participants
#   setup_directories  → mkdirs $DEV_DIR/participant-{1,2,3}
#   start_nodes        → spawns 3 DPM processes, populates PIDS[]
#   configure_peers    → posts peer config + restarts nodes
# ---------------------------------------------------------------------------
download_localnet
start_localnet

setup_directories
start_nodes

configure_peers

# ---------------------------------------------------------------------------
# Write all process PIDs to the PID file so --teardown can stop them.
# Both DPM PIDs (PIDS[]) and Canton tunnel PIDs (CANTON_TUNNEL_PIDS[]) are
# included so a single --teardown call cleans up the entire stack.
# Mark bring-up complete BEFORE writing the PID file so the failure trap
# becomes a no-op from this point forward.
# ---------------------------------------------------------------------------
_BRING_UP_COMPLETE=1
: > "$PID_FILE"
for pid in "${PIDS[@]}"; do echo "$pid" >> "$PID_FILE"; done
for pid in "${CANTON_TUNNEL_PIDS[@]+"${CANTON_TUNNEL_PIDS[@]}"}"; do echo "$pid" >> "$PID_FILE"; done

echo "e2e stack up: P1=:$P1_HTTP P2=:$P2_HTTP P3=:$P3_HTTP (pids in $PID_FILE)"
