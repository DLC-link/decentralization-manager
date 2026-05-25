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

# Localnet
LOCALNET_VERSION="0.5.17"
LOCALNET_BUNDLE_URL="https://github.com/digital-asset/decentralized-canton-sync/releases/download/v${LOCALNET_VERSION}/${LOCALNET_VERSION}_splice-node.tar.gz"
LOCALNET_CACHE_DIR="$SCRIPT_DIR/.localnet"
LOCALNET_COMPOSE_DIR="$LOCALNET_CACHE_DIR/splice-node/docker-compose/localnet"

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

# Paths
DEV_DIR=$(mktemp -d "${TMPDIR:-/tmp}/dpm-it-XXXXXX")
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

    # Stop localnet
    stop_localnet

    # Remove temp directory. In CI we keep it so the `upload-artifact` step
    # can grab `$DEV_DIR/participant-*/stderr.log` — the runner is torn down
    # after the job so leaving the dir behind has no cost, and it's the only
    # way to actually diagnose hangs that fire the trap before upload runs.
    if [ -z "${CI:-}" ] && [ -n "$DEV_DIR" ] && [ -d "$DEV_DIR" ]; then
        rm -rf "$DEV_DIR"
    fi

    wait 2>/dev/null || true
    echo "Cleanup complete"
}

# ============================================================================
# Localnet management
# ============================================================================

download_localnet() {
    if [ -d "$LOCALNET_CACHE_DIR/splice-node" ]; then
        echo "Localnet bundle already cached"
        return 0
    fi

    echo "Downloading localnet bundle v${LOCALNET_VERSION}..."
    mkdir -p "$LOCALNET_CACHE_DIR"
    curl -fSL "$LOCALNET_BUNDLE_URL" -o "$LOCALNET_CACHE_DIR/splice-node.tar.gz"

    echo "Extracting..."
    tar xzf "$LOCALNET_CACHE_DIR/splice-node.tar.gz" -C "$LOCALNET_CACHE_DIR"
    rm -f "$LOCALNET_CACHE_DIR/splice-node.tar.gz"

    echo "Localnet bundle ready"
}

localnet_compose() {
    export IMAGE_TAG="$LOCALNET_VERSION"
    docker compose \
        --env-file "$LOCALNET_COMPOSE_DIR/compose.env" \
        --env-file "$LOCALNET_COMPOSE_DIR/env/common.env" \
        -f "$LOCALNET_COMPOSE_DIR/compose.yaml" \
        -f "$LOCALNET_COMPOSE_DIR/resource-constraints.yaml" \
        --profile sv \
        --profile app-provider \
        --profile app-user \
        "$@"
}

start_localnet() {
    # Clean up any existing chain data from previous runs (keeps images)
    echo "Cleaning up previous localnet data..."
    localnet_compose down -v 2>/dev/null || true

    echo "Starting localnet..."
    # Only start the services our tests actually use. The 3 active profiles
    # (sv/app-provider/app-user) otherwise also bring up nginx + 7 web UI
    # containers (wallet/ans/scan/sv UIs) which are pure browser-facing UIs
    # — our tests hit Canton ledger/admin gRPC ports directly, never the
    # nginx-fronted UI ports. canton -> postgres and splice -> canton are
    # auto-started via depends_on; the UIs are not depended on by anything
    # we use, so naming the three core services here drops the rest.
    #
    # --wait blocks until canton + splice healthchecks pass. Splice healthy
    # means /api/validator/readyz returns OK, i.e. splice has registered the
    # global synchronizer with all 3 participants. Without it, dpm processes
    # race ahead and get "No participant ID returned" / "synchronizer with
    # alias global is unknown" — the UIs used to incidentally pad the wall
    # clock during compose start; trimming them exposed the race.
    localnet_compose up -d --wait canton splice postgres
}

stop_localnet() {
    if [ -d "$LOCALNET_COMPOSE_DIR" ]; then
        echo "Stopping localnet..."
        localnet_compose down -v 2>/dev/null || true
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
