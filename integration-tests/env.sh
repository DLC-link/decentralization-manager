#!/bin/bash

# Integration test environment — shared variables and utility functions.
# Sourced by run.sh and all workflow scripts.

# Exit on error; treat unset variables as failure
set -eu

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

# ============================================================================
# Constants
# ============================================================================

# Resolve project root (parent of integration-tests/)
SCRIPT_DIR="${SCRIPT_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"

# Localnet's Canton is a single container, so topology settles in under a
# second and the 30s production default is pure sleep. devnet.env.sh
# deliberately does NOT set this — devnet is a real network.
export DECPM_TOPOLOGY_PROPAGATION_DELAY_SECS=3

# The reward-automation tick. Exported, not passed as a command prefix like
# common.sh's other node env, because the chaos phases respawn nodes from the
# Rust harness (tests/common/processes.rs), which inherits this process's
# environment. A prefix-only value silently left every respawned node on the
# 300s production default, so `coupon_reassignment` — which runs after the
# chaos block — waited on whichever tick that node happened to be on. That is
# the 48s/109s/219s spread this phase showed across runs.
export DECPM_REWARD_AUTOMATION_INTERVAL_SECS=3

# The peer's coordinator-poll cadence, which quantizes every multi-step
# workflow: 100% of the suite's waits >=2s floored to an even second on the 2s
# default, and "onboarding reaches completed" was exactly 22.0s (11 polls) in
# five runs out of five. 500ms rather than something smaller because each poll
# is a fresh Noise connection, and this runner is CPU-sensitive enough that
# handshake pressure has stalled the mesh before. devnet keeps the 2s default —
# there a coordinator step is a real Canton round trip.
export DECPM_PEER_WAIT_POLL_DELAY_MS=500

# Localnet: the bundle version, the download and the compose invocation come
# from hackathon/localnet.sh, which the hackathon quickstart sources too. One
# definition, one cached bundle under <repo>/.localnet, no drift between what
# CI boots and what a hackathon team boots.
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/../hackathon" && pwd)/localnet.sh"

# Canton ports (compose.yaml: prefix + suffix, e.g. "3" + "901" = 3901)
# dec-party-manager instance 1 → App Provider
P1_CANTON_LEDGER=3901
P1_CANTON_ADMIN=3902
# dec-party-manager instance 2 → App User
P2_CANTON_LEDGER=2901
P2_CANTON_ADMIN=2902
# dec-party-manager instance 3 → SV
P3_CANTON_LEDGER=4901
P3_CANTON_ADMIN=4902

# dec-party-manager HTTP and Noise ports
P1_HTTP=8081
P1_NOISE=9001
P2_HTTP=8082
P2_NOISE=9002
P3_HTTP=8083
P3_NOISE=9003

P1_METRICS=9464
P2_METRICS=9465
P3_METRICS=9466

# Paths
DEV_DIR=$(mktemp -d "${TMPDIR:-/tmp}/decman-it-XXXXXX")
DARS_DIR="$SCRIPT_DIR/releases/v0/rc4"
BINARY="$SCRIPT_DIR/target/release-ci/dec-party-manager"

# JWT token for Canton ledger access (HS256, secret "unsafe",
# aud "https://canton.network.global"). Shared by deploy-gov-core.sh and any
# workflow script that calls the JSON Ledger API or runs `dpm script` directly.
MOCK_TOKEN="eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJhdWQiOiJodHRwczovL2NhbnRvbi5uZXR3b3JrLmdsb2JhbCIsImlhdCI6MTc2Mzc0ODcwMiwic3ViIjoibGVkZ2VyLWFwaS11c2VyIn0.vpkfH4SoM9AZqbE38W4hrvl3xxy69jYs4u8gveskw9k"

# Process tracking
PIDS=()

# ============================================================================
# Cleanup
# ============================================================================

cleanup() {
    echo ""
    echo "Cleaning up..."

    # Kill dec-party-manager processes. The binary ignores SIGTERM today, so
    # plain `kill` without escalation leaks orphaned processes that hold the
    # Noise/HTTP ports until the host reboots. Send SIGTERM first (give the
    # process a chance to shut down cleanly if it ever starts honoring it),
    # wait briefly, then SIGKILL anything still alive.
    # Guard the array expansions: macOS ships bash 3.2, which treats
    # "${arr[@]}" on an EMPTY array as an unbound-variable error under `set -u`.
    # cleanup() runs as an EXIT trap, so on an early failure (before start_nodes
    # populates PIDS) an unguarded loop aborts here and masks the real error.
    if [ "${#PIDS[@]}" -gt 0 ]; then
        for pid in "${PIDS[@]}"; do
            if kill -0 "$pid" 2>/dev/null; then
                kill "$pid" 2>/dev/null || true
            fi
        done
        sleep 2
        for pid in "${PIDS[@]}"; do
            if kill -0 "$pid" 2>/dev/null; then
                kill -9 "$pid" 2>/dev/null || true
            fi
        done
    fi

    # Also kill any processes the Rust chaos phases respawned during the run.
    # Each restart appends one PID per line to $DEV_DIR/restarted-pids so the
    # cleanup() trap reaps them even if cargo test panics or aborts.
    if [ -n "${DEV_DIR:-}" ] && [ -f "$DEV_DIR/restarted-pids" ]; then
        while IFS= read -r pid; do
            if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
                kill -9 "$pid" 2>/dev/null || true
            fi
        done < "$DEV_DIR/restarted-pids"
    fi

    # run.sh may still have `docker compose up --wait` in flight (it backgrounds
    # the bring-up to overlap it with the build). Settle it first: stop_localnet's
    # `down -v` racing a live `up` leaves half-created containers and volumes
    # behind, which fails the next run's port checks.
    #
    # Deliberately NOT a plain `kill` on the pid: that pid is the subshell
    # wrapper, and `docker compose` is its child. Killing the wrapper orphans
    # compose and `down -v` still races a live `up` — the very thing this
    # guards. Signalling a process group is not an option either: a background
    # job in a non-interactive shell shares the script's own group (verified),
    # so `kill -- -$pid` would target this script.
    #
    # So: let it finish, bounded, and only then take down the whole subtree.
    if [ -n "${CANTON_BRINGUP_PID:-}" ] && kill -0 "$CANTON_BRINGUP_PID" 2>/dev/null; then
        echo "Letting the background Canton bring-up settle before teardown..."
        _bringup_waited=0
        while kill -0 "$CANTON_BRINGUP_PID" 2>/dev/null && [ "$_bringup_waited" -lt 240 ]; do
            sleep 1
            _bringup_waited=$((_bringup_waited + 1))
        done
        if kill -0 "$CANTON_BRINGUP_PID" 2>/dev/null; then
            echo "Bring-up overstayed ${_bringup_waited}s; terminating it and its compose child"
            pkill -P "$CANTON_BRINGUP_PID" 2>/dev/null || true
            sleep 2
            pkill -9 -P "$CANTON_BRINGUP_PID" 2>/dev/null || true
            kill -9 "$CANTON_BRINGUP_PID" 2>/dev/null || true
        fi
        wait "$CANTON_BRINGUP_PID" 2>/dev/null || true
    fi
    if [ -n "${CANTON_BRINGUP_LOG:-}" ] && [ -f "$CANTON_BRINGUP_LOG" ]; then
        cat "$CANTON_BRINGUP_LOG"
        rm -f "$CANTON_BRINGUP_LOG"
    fi

    # Stop localnet
    stop_localnet

    # Preserve the per-node stderr logs before wiping the temp directory —
    # they are the only record of node-side WARN/ERROR lines (each node's
    # output is redirected to $DEV_DIR/participant-N/stderr.log, invisible in
    # the runner's stdout). Set DECPM_IT_LOG_DIR (CI does, on failure-upload)
    # to copy them out; unset means the old wipe-everything behaviour.
    if [ -n "$DEV_DIR" ] && [ -d "$DEV_DIR" ] && [ -n "${DECPM_IT_LOG_DIR:-}" ]; then
        mkdir -p "$DECPM_IT_LOG_DIR"
        for i in 1 2 3; do
            cp "$DEV_DIR/participant-$i/stderr.log" \
                "$DECPM_IT_LOG_DIR/participant-$i.stderr.log" 2>/dev/null || true
        done
    fi

    # Remove temp directory
    if [ -n "$DEV_DIR" ] && [ -d "$DEV_DIR" ]; then
        rm -rf "$DEV_DIR"
    fi

    wait 2>/dev/null || true
    echo "Cleanup complete"
}

# ============================================================================
# Localnet management
# ============================================================================

start_localnet() {
    # Tests own the ledger: wipe the chain data from any previous run first, so
    # a suite never inherits parties, contracts or vetted DARs it did not
    # create. The hackathon bundle deliberately does the opposite and keeps its
    # state across a stop/start.
    echo "Cleaning up previous localnet data..."
    localnet_wipe

    echo "Starting localnet..."
    localnet_start
}

stop_localnet() {
    if [ -d "$LOCALNET_DIR" ]; then
        echo "Stopping localnet..."
        localnet_wipe
    fi
}

# ============================================================================
# dec-party-manager instance management
# ============================================================================

stop_nodes() {
    # Same SIGTERM-ignoring problem as in `cleanup`: plain `kill` leaves the
    # processes alive, holding their HTTP/Noise ports. When `configure_peers`
    # then calls `start_nodes` to reload peer config, the new processes can't
    # bind, the test silently runs against the old (peer-config-stale)
    # instances, and Noise calls fail later with "Connection refused".
    # Send SIGTERM, give a 2s grace, then SIGKILL anything still alive.
    for pid in "${PIDS[@]}"; do
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
        fi
    done
    sleep 2
    for pid in "${PIDS[@]}"; do
        if kill -0 "$pid" 2>/dev/null; then
            kill -9 "$pid" 2>/dev/null || true
        fi
    done
    wait 2>/dev/null || true
    PIDS=()
}

# ============================================================================
# Workflow helpers
# ============================================================================

poll_status() {
    local port=$1
    local endpoint=$2
    local max_attempts=120
    local attempt=0

    echo "Polling $endpoint on port $port..."
    while true; do
        attempt=$((attempt + 1))
        if [ $attempt -ge $max_attempts ]; then
            echo "ERROR: $endpoint timed out after $max_attempts attempts"
            exit 1
        fi

        local response
        response=$(curl -s "http://localhost:$port/$endpoint")
        local status
        status=$(echo "$response" | jq -r '.status // empty')

        case "$status" in
            "completed"|"Completed")
                echo "$endpoint completed successfully"
                return 0
                ;;
            "failed"|"Failed")
                local error
                error=$(echo "$response" | jq -r '.error // "Unknown error"')
                echo "ERROR: $endpoint failed: $error"
                exit 1
                ;;
            *)
                sleep 2
                ;;
        esac
    done
}

# Poll the `workflow_runs` row's persisted status until it reaches a terminal
# state (or timeout). This is the source of truth — preferred over
# `/<kind>/status` for restart/retry tests because the in-memory
# `<Kind>WorkflowState` is reset across a process restart and only catches
# updates from spawned tasks running in that fresh process. The DB row is
# durable across restarts.
#
# Args: db_file instance_name [max_attempts=120]
# Exits 1 with a clear message on timeout, "failed", or "cancelled".
poll_workflow_run_status() {
    local db_file=$1
    local instance_name=$2
    local max_attempts=${3:-120}
    local attempt=0

    echo "Polling workflow_runs row for $instance_name..."
    while true; do
        attempt=$((attempt + 1))
        if [ $attempt -ge $max_attempts ]; then
            local actual
            actual=$(sqlite3 "$db_file" \
                "SELECT status FROM workflow_runs WHERE instance_name='$instance_name';" \
                2>/dev/null || echo "?")
            echo "ERROR: workflow_runs row $instance_name did not reach a terminal status (last: $actual)"
            exit 1
        fi
        local status
        status=$(sqlite3 "$db_file" \
            "SELECT status FROM workflow_runs WHERE instance_name='$instance_name';" \
            2>/dev/null || echo "")
        case "$status" in
            completed)
                echo "workflow_runs $instance_name reached Completed"
                return 0
                ;;
            failed|cancelled)
                local err
                err=$(sqlite3 "$db_file" \
                    "SELECT error FROM workflow_runs WHERE instance_name='$instance_name';" \
                    2>/dev/null || echo "")
                echo "ERROR: workflow_runs $instance_name reached terminal $status: $err"
                exit 1
                ;;
        esac
        sleep 2
    done
}

# Assert that a Completed workflow run of the given kind is visible in the
# unified notification feed (`GET /workflows`) on every relevant node:
# - exactly one Coordinator row on the coordinator's port
# - exactly one Peer row on each peer's port
#
# Args: kind coord_port [peer_port ...]
# Example: assert_workflow_completed_visible "Onboarding" $P1_HTTP $P2_HTTP $P3_HTTP
assert_workflow_completed_visible() {
    local kind=$1
    local coord_port=$2
    shift 2

    local coord_count
    coord_count=$(curl -s "http://localhost:$coord_port/workflows" \
        | jq -r --arg k "$kind" \
            '[.runs[] | select(.kind == $k and .role == "Coordinator" and .status == "completed")] | length')
    if [ "$coord_count" -lt 1 ]; then
        echo "ERROR: $kind/Coordinator completed row missing from /workflows on port $coord_port"
        exit 1
    fi

    local peer_port
    for peer_port in "$@"; do
        local peer_count
        peer_count=$(curl -s "http://localhost:$peer_port/workflows" \
            | jq -r --arg k "$kind" \
                '[.runs[] | select(.kind == $k and .role == "Peer" and .status == "completed")] | length')
        if [ "$peer_count" -lt 1 ]; then
            echo "ERROR: $kind/Peer completed row missing from /workflows on port $peer_port"
            exit 1
        fi
    done

    echo "$kind run visible in /workflows on coordinator + ${#} peer(s)"
}

# Assert that a governance proposal_cid is visible in
# `GET /governance/confirmations?party_id=...` on every listed node.
#
# Args: party_id proposal_cid port [port ...]
assert_governance_action_visible_on_all_nodes() {
    local party_id=$1
    local proposal_cid=$2
    shift 2

    local port
    for port in "$@"; do
        local seen
        seen=$(curl -s "http://localhost:$port/governance/confirmations?party_id=$party_id" \
            | jq -r --arg cid "$proposal_cid" \
                '[.domain_actions[] | select(.proposal_cid == $cid)] | length')
        if [ "$seen" -lt 1 ]; then
            echo "ERROR: proposal $proposal_cid not visible in /governance/confirmations on port $port"
            exit 1
        fi
    done

    echo "Proposal $proposal_cid visible on $# node(s)"
}

accept_invitation() {
    local port=$1
    local name=$2
    local invitation_type=$3
    local max_attempts=30
    local attempt=0

    echo "Waiting for $invitation_type invitation on $name..."
    while true; do
        attempt=$((attempt + 1))
        if [ $attempt -ge $max_attempts ]; then
            echo "ERROR: No $invitation_type invitation received on $name after $max_attempts attempts"
            exit 1
        fi

        local response
        response=$(curl -s "http://localhost:$port/invitations")
        local invitation_id
        invitation_id=$(echo "$response" | jq -r --arg type "$invitation_type" \
            '.invitations[] | select(.invitation_type == $type) | .id // empty' | head -1)

        if [ -n "$invitation_id" ]; then
            echo "Accepting $invitation_type invitation on $name (id: $invitation_id)..."
            curl -s -X POST "http://localhost:$port/invitations/accept" \
                -H "Content-Type: application/json" \
                -d "{\"id\": \"$invitation_id\"}" > /dev/null
            echo "$invitation_type invitation accepted on $name"
            return 0
        fi

        sleep 1
    done
}
