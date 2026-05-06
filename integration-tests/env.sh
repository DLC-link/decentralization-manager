#!/bin/bash

# Integration test environment — shared variables and utility functions.
# Sourced by run.sh and all workflow scripts.

# Exit on error; treat unset variables as failure
set -eu

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
# Logging
# ============================================================================

log_phase() {
    echo ""
    echo "=========================================="
    echo "$1"
    echo "=========================================="
}

# ============================================================================
# Prerequisites
# ============================================================================

check_prerequisites() {
    local missing=()

    if ! command -v docker &>/dev/null; then
        missing+=("docker")
    fi

    if ! docker compose version &>/dev/null 2>&1; then
        missing+=("docker compose v2")
    fi

    if ! command -v jq &>/dev/null; then
        missing+=("jq")
    fi

    if ! command -v curl &>/dev/null; then
        missing+=("curl")
    fi

    if ! command -v lsof &>/dev/null; then
        missing+=("lsof")
    fi

    if [ ${#missing[@]} -gt 0 ]; then
        echo "ERROR: Missing required tools: ${missing[*]}"
        exit 1
    fi
}

# ============================================================================
# Port availability
# ============================================================================

# Checks that the dec-party-manager HTTP and Noise ports are free.
# A leftover process (e.g. a dpm started by a previous run that didn't clean up,
# or a different worktree's dpm still running) would silently steal one of these
# ports and the e2e would time out 60s into the first invitation accept.
# Failing fast here turns that into an instant, actionable error.
check_dpm_ports_free() {
    local ports=("$P1_HTTP" "$P2_HTTP" "$P3_HTTP" "$P1_NOISE" "$P2_NOISE" "$P3_NOISE")
    local busy=()

    for p in "${ports[@]}"; do
        if lsof -nP -i:"$p" -sTCP:LISTEN >/dev/null 2>&1; then
            busy+=("$p")
        fi
    done

    if [ ${#busy[@]} -gt 0 ]; then
        echo "ERROR: required port(s) already in use: ${busy[*]}"
        echo ""
        echo "Process(es) holding the port(s):"
        for p in "${busy[@]}"; do
            echo "  port $p:"
            lsof -nP -i:"$p" -sTCP:LISTEN 2>/dev/null | tail -n +2 | sed 's/^/    /'
        done
        echo ""
        echo "Stop the offending process(es) (often a dpm leftover from a previous run"
        echo "or another worktree), then re-run integration-tests/run.sh."
        exit 1
    fi
}

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
    localnet_compose up -d canton splice postgres
}

stop_localnet() {
    if [ -d "$LOCALNET_COMPOSE_DIR" ]; then
        echo "Stopping localnet..."
        localnet_compose down -v 2>/dev/null || true
    fi
}

wait_for_localnet() {
    local max_attempts=90
    local attempt

    echo "Waiting for localnet Canton nodes..."

    for port in $P1_CANTON_ADMIN $P2_CANTON_ADMIN $P3_CANTON_ADMIN; do
        attempt=0
        echo "  Waiting for Canton Admin API on port $port..."
        while ! (echo >/dev/tcp/localhost/"$port") 2>/dev/null; do
            attempt=$((attempt + 1))
            if [ $attempt -ge $max_attempts ]; then
                echo "ERROR: Canton node on port $port not ready after $max_attempts attempts"
                localnet_compose logs --tail=30
                exit 1
            fi
            sleep 2
        done
        echo "  Canton Admin API on port $port is ready"
    done

    # Allow time for Canton topology and synchronizer to fully initialize
    echo "Waiting for Canton to fully initialize..."
    sleep 15

    echo "Localnet is ready"
}

# ============================================================================
# Directory setup
# ============================================================================

setup_directories() {
    echo "Setting up test directories in $DEV_DIR..."
    for i in 1 2 3; do
        mkdir -p "$DEV_DIR/participant-$i"
    done
}

# ============================================================================
# dec-party-manager instance management
# ============================================================================

start_nodes() {
    local http_ports=($P1_HTTP $P2_HTTP $P3_HTTP)
    local canton_ledger_ports=($P1_CANTON_LEDGER $P2_CANTON_LEDGER $P3_CANTON_LEDGER)
    local canton_admin_ports=($P1_CANTON_ADMIN $P2_CANTON_ADMIN $P3_CANTON_ADMIN)
    local noise_ports=($P1_NOISE $P2_NOISE $P3_NOISE)

    for i in 1 2 3; do
        local idx=$((i - 1))
        echo "Starting participant-$i..."
        RUST_LOG="${RUST_LOG:-dec_party_manager=info,tokio_noise=error,hyper_noise=error}" \
        DECPM_CANTON_ADMIN_HOST=127.0.0.1 \
        DECPM_CANTON_ADMIN_PORT="${canton_admin_ports[$idx]}" \
        DECPM_CANTON_LEDGER_HOST=127.0.0.1 \
        DECPM_CANTON_LEDGER_PORT="${canton_ledger_ports[$idx]}" \
        DECPM_CANTON_NETWORK=devnet \
        DECPM_NOISE_PORT="${noise_ports[$idx]}" \
        "$BINARY" -d "$DEV_DIR/participant-$i" serve \
            --host 0.0.0.0 --port "${http_ports[$idx]}" &
        PIDS+=($!)
    done

    # Wait for all servers to be ready
    wait_for_server $P1_HTTP "participant-1" $P1_NOISE
    wait_for_server $P2_HTTP "participant-2" $P2_NOISE
    wait_for_server $P3_HTTP "participant-3" $P3_NOISE

    # Settle delay before returning. wait_for_server only checks "is the port
    # bound", not "are all peers reachable from each other through the Noise
    # mesh". Without this delay, configure_peers' restart cycle can leave the
    # workflow client and the parties handler hammering each freshly-restarted
    # peer for ~30-50s with Connection-refused / handshake-rejection log spam
    # while the cross-node Noise sessions converge. 5s catches the common case;
    # noisy networks will still produce some log lines but the storm is short.
    sleep 5
}

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

wait_for_server() {
    local port=$1
    local name=$2
    local noise_port=$3
    local max_attempts=30
    local attempt=0

    echo "Waiting for $name on port $port..."
    while ! curl -s "http://localhost:$port/node-config" > /dev/null 2>&1; do
        attempt=$((attempt + 1))
        if [ $attempt -ge $max_attempts ]; then
            echo "ERROR: $name failed to start after $max_attempts attempts"
            exit 1
        fi
        sleep 1
    done

    # Wait for keys to be generated
    attempt=0
    while true; do
        local key
        key=$(curl -s "http://localhost:$port/keys/status" | jq -r '.public_key // empty')
        if [ -n "$key" ] && [ "$key" != "null" ]; then
            break
        fi
        attempt=$((attempt + 1))
        if [ $attempt -ge $max_attempts ]; then
            echo "ERROR: $name keys not generated after $max_attempts attempts"
            exit 1
        fi
        sleep 1
    done

    # Wait for Noise listener
    if [ -n "$noise_port" ]; then
        attempt=0
        echo "Waiting for $name Noise listener on port $noise_port..."
        while ! (echo >/dev/tcp/localhost/"$noise_port") 2>/dev/null; do
            attempt=$((attempt + 1))
            if [ $attempt -ge $max_attempts ]; then
                echo "ERROR: $name Noise listener not ready after $max_attempts attempts"
                exit 1
            fi
            sleep 1
        done
    fi

    echo "$name is ready"
}

# ============================================================================
# Peer configuration
# ============================================================================

configure_peers() {
    echo "Fetching public keys and participant IDs..."

    P1_KEY=$(curl -s "http://localhost:$P1_HTTP/keys/status" | jq -r '.public_key')
    P2_KEY=$(curl -s "http://localhost:$P2_HTTP/keys/status" | jq -r '.public_key')
    P3_KEY=$(curl -s "http://localhost:$P3_HTTP/keys/status" | jq -r '.public_key')

    P1_PARTICIPANT_ID=$(curl -s "http://localhost:$P1_HTTP/node-config" | jq -r '.node.participant_id')
    P2_PARTICIPANT_ID=$(curl -s "http://localhost:$P2_HTTP/node-config" | jq -r '.node.participant_id')
    P3_PARTICIPANT_ID=$(curl -s "http://localhost:$P3_HTTP/node-config" | jq -r '.node.participant_id')

    echo "Participant 1: $P1_PARTICIPANT_ID (key: ${P1_KEY:0:16}...)"
    echo "Participant 2: $P2_PARTICIPANT_ID (key: ${P2_KEY:0:16}...)"
    echo "Participant 3: $P3_PARTICIPANT_ID (key: ${P3_KEY:0:16}...)"

    local peers_json
    peers_json=$(cat <<EOF
[
  {"participant_id": "$P1_PARTICIPANT_ID", "name": "Participant 1", "address": "127.0.0.1", "port": $P1_NOISE, "public_key": "$P1_KEY", "party": null},
  {"participant_id": "$P2_PARTICIPANT_ID", "name": "Participant 2", "address": "127.0.0.1", "port": $P2_NOISE, "public_key": "$P2_KEY", "party": null},
  {"participant_id": "$P3_PARTICIPANT_ID", "name": "Participant 3", "address": "127.0.0.1", "port": $P3_NOISE, "public_key": "$P3_KEY", "party": null}
]
EOF
    )

    for port in $P1_HTTP $P2_HTTP $P3_HTTP; do
        curl -s -X POST "http://localhost:$port/network-config" \
            -H "Content-Type: application/json" \
            -d "$peers_json" > /dev/null
    done

    echo "Peers configured on all participants"

    # Restart to reload peer config (peer_keys map is built at startup)
    sleep 1
    echo "Restarting nodes to reload peer configuration..."
    stop_nodes

    sleep 2
    start_nodes
    echo "Nodes restarted with peer configuration"
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
