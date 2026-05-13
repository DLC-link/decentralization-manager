#!/bin/bash

# Smoke verification for issue #79 (hybrid observation + active-Ping peer
# liveness). Focuses on acceptance criterion #1: an idle cluster produces zero
# `Noise ... input error` log lines from peer-to-peer probes.
#
# Reuses integration-tests/env.sh for localnet + dpm bootstrap; the only
# differences vs run.sh are (a) per-node logs are captured to files so we
# can grep them, and (b) we skip the governance e2e and just sit idle for 60s.
#
# Usage: integration-tests/smoke-noise-errors.sh
#
# Prerequisites: docker, docker compose v2.1.1+, jq, curl, lsof
# (same as run.sh).

set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$SCRIPT_DIR/integration-tests/env.sh"

# Capture all dpm output at debug so the previously-spammy line would be visible
# if it were still being emitted. Without this filter the default WARN drops it.
export RUST_LOG="${RUST_LOG:-dec_party_manager=debug,tokio_noise=error,hyper_noise=error}"

LOG_DIR="$DEV_DIR/logs"
NOISE_ERR_PATTERN='Noise connection from .* failed: noise.*input error'
IDLE_SECONDS="${IDLE_SECONDS:-60}"

# Override env.sh's start_nodes so each dpm's stdout+stderr lands in its own
# log file. env.sh's version inherits the parent shell, which is fine for the
# governance e2e but useless for greppable smoke checks.
start_nodes() {
    local http_ports=($P1_HTTP $P2_HTTP $P3_HTTP)
    local canton_ledger_ports=($P1_CANTON_LEDGER $P2_CANTON_LEDGER $P3_CANTON_LEDGER)
    local canton_admin_ports=($P1_CANTON_ADMIN $P2_CANTON_ADMIN $P3_CANTON_ADMIN)
    local noise_ports=($P1_NOISE $P2_NOISE $P3_NOISE)

    mkdir -p "$LOG_DIR"

    for i in 1 2 3; do
        local idx=$((i - 1))
        echo "Starting participant-$i (log: $LOG_DIR/node-$i.log)..."
        RUST_LOG="$RUST_LOG" \
        DECPM_CANTON_ADMIN_HOST=127.0.0.1 \
        DECPM_CANTON_ADMIN_PORT="${canton_admin_ports[$idx]}" \
        DECPM_CANTON_LEDGER_HOST=127.0.0.1 \
        DECPM_CANTON_LEDGER_PORT="${canton_ledger_ports[$idx]}" \
        DECPM_CANTON_NETWORK=devnet \
        DECPM_NOISE_PORT="${noise_ports[$idx]}" \
        "$BINARY" -d "$DEV_DIR/participant-$i" serve \
            --host 0.0.0.0 --port "${http_ports[$idx]}" \
            > "$LOG_DIR/node-$i.log" 2>&1 &
        PIDS+=($!)
    done

    wait_for_server $P1_HTTP "participant-1" $P1_NOISE
    wait_for_server $P2_HTTP "participant-2" $P2_NOISE
    wait_for_server $P3_HTTP "participant-3" $P3_NOISE

    # Match env.sh's settle delay — see the comment there.
    sleep 5
}

trap cleanup EXIT

log_phase "Preflight"
check_prerequisites
check_dpm_ports_free

log_phase "Building release-ci binary (with test-mode feature)"
cargo build --profile release-ci --features test-mode
if [ ! -f "$BINARY" ]; then
    echo "ERROR: Binary not found at $BINARY"
    exit 1
fi

log_phase "Starting localnet"
download_localnet
start_localnet

log_phase "Starting 3 dec-party-manager instances"
setup_directories
start_nodes

log_phase "Configuring peers"
# configure_peers calls stop_nodes + start_nodes again; the second start_nodes
# resolves to our override so logs are captured for the post-restart lifetime.
configure_peers

log_phase "Idle ${IDLE_SECONDS}s and check logs for noise-error spam"
# Capture each log's current line count BEFORE sleeping. Anything written
# during the idle window is the only thing that counts — env.sh's
# wait_for_server uses a bare `echo >/dev/tcp/...` TCP probe during boot
# (and again during configure_peers' restart) which legitimately triggers
# one "Noise … input error" line per node. Those are test-harness
# artifacts, not regressions, and must be excluded.
declare -a baseline_lines
for i in 1 2 3; do
    if [ -f "$LOG_DIR/node-$i.log" ]; then
        baseline_lines[$i]=$(wc -l < "$LOG_DIR/node-$i.log")
    else
        baseline_lines[$i]=0
    fi
done
echo "Baseline log lines (pre-idle): node-1=${baseline_lines[1]} node-2=${baseline_lines[2]} node-3=${baseline_lines[3]}"

echo "Sleeping ${IDLE_SECONDS}s with no workflow active..."
sleep "$IDLE_SECONDS"

total=0
declare -a per_node_counts
for i in 1 2 3; do
    start=$((baseline_lines[$i] + 1))
    # `grep | wc -l` instead of `grep -c`: grep -c exits 1 when no matches,
    # which combined with `|| echo 0` would concatenate "0" + "0" inside the
    # command substitution. wc -l always exits 0 and prints exactly the count.
    count=$(tail -n "+$start" "$LOG_DIR/node-$i.log" 2>/dev/null \
        | grep -E "$NOISE_ERR_PATTERN" \
        | wc -l \
        | tr -d ' ')
    per_node_counts[$i]=$count
    total=$((total + count))
done

echo ""
echo "Pattern: $NOISE_ERR_PATTERN"
for i in 1 2 3; do
    echo "  node-$i: ${per_node_counts[$i]} match(es)  ($LOG_DIR/node-$i.log)"
done
echo "  total: $total"
echo ""

if [ "$total" -eq 0 ]; then
    echo "PASS  T8.1 — zero noise-error spam over ${IDLE_SECONDS}s idle"
    exit_code=0
else
    echo "FAIL  T8.1 — ${total} noise-error lines logged; expected 0"
    echo ""
    echo "Sample matches:"
    for i in 1 2 3; do
        if [ "${per_node_counts[$i]}" -gt 0 ]; then
            grep -E "$NOISE_ERR_PATTERN" "$LOG_DIR/node-$i.log" \
                | head -3 \
                | sed "s|^|  node-$i: |"
        fi
    done
    exit_code=1
fi

log_phase "T8.2 / T8.3 — manual"
cat <<'EOF'

T8.2  Peers stay active during a workflow
  - Run the full e2e (`integration-tests/run.sh`) and confirm the governance
    workflow completes successfully. The new bump in NoiseServer::handle_request
    keeps the workflow path well-behaved; a stuck or failing workflow would
    indicate a regression.
  - PackagesPanel's peer indicators read `peer.reachable` from the on-demand
    fetch_peer_packages probe (parties.rs:785), not from the heartbeat's
    peer_status map. So the panel test is more about UX continuity than the
    heartbeat itself.

T8.3  Stop a peer, observe <=30s detection latency
  - peer_status is currently stored in AppState but not exposed via HTTP.
  - To time the flip you would need either (a) a temporary debug endpoint that
    returns AppState.peer_status, or (b) RUST_LOG=trace inspection of the ping
    loop's per-tick outcomes.
  - Out of scope for this smoke script. If you want this verified before merge,
    say so and we can add a debug endpoint in a follow-up commit.

Acceptance summary
  - #1 (idle log silence): see T8.1 result above.
  - #2 (peer-status UX): exercise via run.sh + visual UI inspection if desired.
  - #3 (fmt/clippy/test): already verified by the implementation review.

EOF

exit $exit_code
