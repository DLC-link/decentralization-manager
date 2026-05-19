# shellcheck shell=bash
# Shared helpers between integration-tests/env.sh (localnet) and devnet.env.sh.
# Sourced by both. Behavior must be identical to the original env.sh definitions.

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
# Readiness polling
# ============================================================================

wait_for_server() {
    local port=$1
    local name=$2
    local noise_port=$3
    local max_attempts=30
    local attempt=0

    # Optional bearer token. Required on devnet (real JwtValidator) so the
    # readiness probes below aren't rejected as "missing bearer token". On
    # localnet the binary is built with `--features test-mode` (MockValidator)
    # which accepts any/no token, so DPM_IT_AUTH_TOKEN is left unset.
    local auth_args=()
    if [ -n "${DPM_IT_AUTH_TOKEN:-}" ]; then
        auth_args=(-H "Authorization: Bearer ${DPM_IT_AUTH_TOKEN}")
    fi

    echo "Waiting for $name on port $port..."
    while ! curl -s "${auth_args[@]+"${auth_args[@]}"}" "http://localhost:$port/node-config" > /dev/null 2>&1; do
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
        key=$(curl -s "${auth_args[@]+"${auth_args[@]}"}" "http://localhost:$port/keys/status" | jq -r '.public_key // empty')
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
# Prerequisites
# ============================================================================

check_prerequisites() {
    local missing=()

    if ! command -v docker &>/dev/null; then
        missing+=("docker")
    fi

    # `docker compose up --wait` was introduced in Compose v2.1.1
    # (Oct 2021). start_localnet relies on it to block until canton +
    # splice healthchecks pass, so an older v2 would fail mid-run with
    # an unrelated "unknown flag" error. Validate up front instead.
    local compose_version
    compose_version=$(docker compose version --short 2>/dev/null || echo "")
    if [ -z "$compose_version" ]; then
        missing+=("docker compose v2.1.1+")
    elif ! printf '2.1.1\n%s\n' "$compose_version" | sort -CV; then
        missing+=("docker compose v2.1.1+ (have $compose_version)")
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
        echo "or another worktree), then re-run the integration tests."
        exit 1
    fi
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
        # Per-participant stderr file: makes "what did P1 see" answerable
        # without grepping a 3-way-interleaved unified log. Appended (>>) so
        # configure_peers' restart cycle accumulates rather than truncates.
        local log_file="$DEV_DIR/participant-$i/stderr.log"
        echo "Starting participant-$i (log: $log_file)..."
        RUST_LOG="${RUST_LOG:-dec_party_manager=info,tokio_noise=error,hyper_noise=error}" \
        DECPM_CANTON_ADMIN_HOST=127.0.0.1 \
        DECPM_CANTON_ADMIN_PORT="${canton_admin_ports[$idx]}" \
        DECPM_CANTON_LEDGER_HOST=127.0.0.1 \
        DECPM_CANTON_LEDGER_PORT="${canton_ledger_ports[$idx]}" \
        DECPM_CANTON_NETWORK=devnet \
        DECPM_NOISE_PORT="${noise_ports[$idx]}" \
        "$BINARY" -d "$DEV_DIR/participant-$i" serve \
            --host 0.0.0.0 --port "${http_ports[$idx]}" \
            >> "$log_file" 2>&1 &
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

# ============================================================================
# Bare-process lifecycle: default stop_nodes
# ============================================================================
#
# Sends SIGTERM, waits 2s, then SIGKILL on anything still alive. Mirrors
# env.sh's localnet definition (which still overrides this) so devnet's
# bare-process path has a stop_nodes too without sourcing env.sh. Required
# by configure_peers' restart cycle and devnet.env.sh's cleanup trap.

stop_nodes() {
    for pid in "${PIDS[@]+"${PIDS[@]}"}"; do
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
        fi
    done
    sleep 2
    for pid in "${PIDS[@]+"${PIDS[@]}"}"; do
        if kill -0 "$pid" 2>/dev/null; then
            kill -9 "$pid" 2>/dev/null || true
        fi
    done
    # Reap only the DPM PIDs we just killed. A bare `wait` would block on
    # every active bg child of the script — on devnet that includes the
    # `_canton_forward_loop` subshells (while-true kubectl port-forwards),
    # so `configure_peers`' restart cycle would hang forever.
    wait "${PIDS[@]+"${PIDS[@]}"}" 2>/dev/null || true
    PIDS=()
}

# ============================================================================
# Peer configuration
# ============================================================================

configure_peers() {
    echo "Fetching public keys and participant IDs..."

    # See wait_for_server for the rationale: devnet's real JwtValidator
    # requires a bearer; localnet's test-mode MockValidator accepts no token.
    local auth_args=()
    if [ -n "${DPM_IT_AUTH_TOKEN:-}" ]; then
        auth_args=(-H "Authorization: Bearer ${DPM_IT_AUTH_TOKEN}")
    fi

    P1_KEY=$(curl -s "${auth_args[@]+"${auth_args[@]}"}" "http://localhost:$P1_HTTP/keys/status" | jq -r '.public_key')
    P2_KEY=$(curl -s "${auth_args[@]+"${auth_args[@]}"}" "http://localhost:$P2_HTTP/keys/status" | jq -r '.public_key')
    P3_KEY=$(curl -s "${auth_args[@]+"${auth_args[@]}"}" "http://localhost:$P3_HTTP/keys/status" | jq -r '.public_key')

    P1_PARTICIPANT_ID=$(curl -s "${auth_args[@]+"${auth_args[@]}"}" "http://localhost:$P1_HTTP/node-config" | jq -r '.node.participant_id')
    P2_PARTICIPANT_ID=$(curl -s "${auth_args[@]+"${auth_args[@]}"}" "http://localhost:$P2_HTTP/node-config" | jq -r '.node.participant_id')
    P3_PARTICIPANT_ID=$(curl -s "${auth_args[@]+"${auth_args[@]}"}" "http://localhost:$P3_HTTP/node-config" | jq -r '.node.participant_id')

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
        curl -s -X POST "${auth_args[@]+"${auth_args[@]}"}" "http://localhost:$port/network-config" \
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
