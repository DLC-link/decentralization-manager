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

# The DecMan API needs a bearer token on devnet (real JwtValidator) so the
# probes below aren't rejected as "missing bearer token". On localnet the
# binary is built with `--features test-mode` (MockValidator), which accepts
# any token or none, so DECPM_IT_AUTH_TOKEN stays unset there. Each function
# below builds its own `auth_args` array from it.

# A node is ready when `/node-config` answers (bootstrap finished, participant
# id resolved) and `/healthz` answers 200. A node opens no other listener — it
# coordinates only through Canton.
wait_for_server() {
    local port=$1
    local name=$2
    local max_attempts=30
    local attempt=0

    local auth_args=()
    if [ -n "${DECPM_IT_AUTH_TOKEN:-}" ]; then
        auth_args=(-H "Authorization: Bearer ${DECPM_IT_AUTH_TOKEN}")
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

    attempt=0
    while ! curl -sf "${auth_args[@]+"${auth_args[@]}"}" "http://localhost:$port/healthz" > /dev/null 2>&1; do
        attempt=$((attempt + 1))
        if [ $attempt -ge $max_attempts ]; then
            echo "ERROR: $name /healthz not answering after $max_attempts attempts"
            exit 1
        fi
        sleep 1
    done

    echo "$name is ready"
}

# The coordination package carries every contract nodes coordinate through, so
# nothing works until each participant has vetted it. A startup task uploads
# and vets the embedded DAR (design D8); this waits for the result.
wait_for_coordination_dar() {
    local port=$1
    local name=$2
    local max_attempts=${3:-120}
    local attempt=0

    local auth_args=()
    if [ -n "${DECPM_IT_AUTH_TOKEN:-}" ]; then
        auth_args=(-H "Authorization: Bearer ${DECPM_IT_AUTH_TOKEN}")
    fi

    echo "Waiting for $name to vet decman-coordination-v1..."
    while true; do
        local seen
        seen=$(curl -s "${auth_args[@]+"${auth_args[@]}"}" \
            "http://localhost:$port/packages/vetted" \
            | jq -r '[.[] | select(.package_name == "decman-coordination-v1")] | length' 2>/dev/null \
            || echo 0)
        if [ "${seen:-0}" -ge 1 ]; then
            echo "$name has vetted decman-coordination-v1"
            return 0
        fi
        attempt=$((attempt + 1))
        if [ $attempt -ge $max_attempts ]; then
            echo "ERROR: $name did not vet decman-coordination-v1 after $max_attempts attempts"
            exit 1
        fi
        sleep 1
    done
}

# Wait until this node sees a registry entry for every configured peer
# (design D3). A node refuses to start a workflow before that.
wait_for_registry() {
    local port=$1
    local name=$2
    local expected=$3
    local max_attempts=${4:-120}
    local attempt=0

    local auth_args=()
    if [ -n "${DECPM_IT_AUTH_TOKEN:-}" ]; then
        auth_args=(-H "Authorization: Bearer ${DECPM_IT_AUTH_TOKEN}")
    fi

    echo "Waiting for $name to see $expected peer registry entries..."
    while true; do
        local seen
        seen=$(curl -s "${auth_args[@]+"${auth_args[@]}"}" "http://localhost:$port/registry" \
            | jq -r '[.peers[] | select(.hosting_verified)] | length' 2>/dev/null || echo 0)
        if [ "${seen:-0}" -ge "$expected" ]; then
            echo "$name sees $seen peer registry entries"
            return 0
        fi
        attempt=$((attempt + 1))
        if [ $attempt -ge $max_attempts ]; then
            echo "ERROR: $name saw $seen of $expected peer registry entries after $max_attempts attempts"
            exit 1
        fi
        sleep 1
    done
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

# Checks that the dec-party-manager HTTP and metrics ports are free.
# A leftover process (e.g. a DecMan started by a previous run that didn't clean up,
# or a different worktree's DecMan still running) would silently steal one of these
# ports and the e2e would time out 60s into the first invitation accept.
# Failing fast here turns that into an instant, actionable error.
check_decman_ports_free() {
    local ports=("$P1_HTTP" "$P2_HTTP" "$P3_HTTP" "$P1_METRICS" "$P2_METRICS" "$P3_METRICS")
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
        echo "Stop the offending process(es) (often a DecMan leftover from a previous run"
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
    local metrics_ports=($P1_METRICS $P2_METRICS $P3_METRICS)

    for i in 1 2 3; do
        local idx=$((i - 1))
        # Per-participant stderr file: makes "what did P1 see" answerable
        # without grepping a 3-way-interleaved unified log. Appended (>>) so a
        # chaos phase's respawn accumulates rather than truncates.
        local log_file="$DEV_DIR/participant-$i/stderr.log"
        echo "Starting participant-$i (log: $log_file)..."
        # The node log level is separate from the runner's. The runner stays
        # quiet so the scenario output reads cleanly; each node still records
        # what it did, because that file is the only evidence a CI failure
        # leaves behind. Override with DECPM_NODE_RUST_LOG.
        RUST_LOG="${DECPM_NODE_RUST_LOG:-dec_party_manager=info,dec_party_manager::onledger=debug}" \
        DECPM_CANTON_ADMIN_HOST=127.0.0.1 \
        DECPM_CANTON_ADMIN_PORT="${canton_admin_ports[$idx]}" \
        DECPM_CANTON_LEDGER_HOST=127.0.0.1 \
        DECPM_CANTON_LEDGER_PORT="${canton_ledger_ports[$idx]}" \
        DECPM_CANTON_NETWORK=devnet \
        DECPM_METRICS_PORT="${metrics_ports[$idx]}" \
        DECPM_PORT="${http_ports[$idx]}" \
        DECPM_REWARD_AUTOMATION_INTERVAL_SECS="${DECPM_REWARD_AUTOMATION_INTERVAL_SECS:-15}" \
        DECPM_HEARTBEAT_INTERVAL_SECS="${DECPM_HEARTBEAT_INTERVAL_SECS:-5}" \
        DECPM_HEARTBEAT_MIN_INTERVAL_SECS="${DECPM_HEARTBEAT_MIN_INTERVAL_SECS:-1}" \
        DECPM_OBSERVER_POLL_SECS="${DECPM_OBSERVER_POLL_SECS:-1}" \
        "$BINARY" -d "$DEV_DIR/participant-$i" serve \
            >> "$log_file" 2>&1 &
        PIDS+=($!)
    done

    # Wait for all servers to be ready
    wait_for_server $P1_HTTP "participant-1"
    wait_for_server $P2_HTTP "participant-2"
    wait_for_server $P3_HTTP "participant-3"
}

# ============================================================================
# Bare-process lifecycle: default stop_nodes
# ============================================================================
#
# Sends SIGTERM, waits 2s, then SIGKILL on anything still alive. Mirrors
# env.sh's localnet definition (which still overrides this) so devnet's
# bare-process path has a stop_nodes too without sourcing env.sh. Required by
# devnet.env.sh's cleanup trap.

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
    # Reap only the DecMan PIDs we just killed. A bare `wait` would block on
    # every active bg child of the script — on devnet that includes the
    # `_canton_forward_loop` subshells (while-true kubectl port-forwards),
    # so the caller would hang forever.
    wait "${PIDS[@]+"${PIDS[@]}"}" 2>/dev/null || true
    PIDS=()
}

# ============================================================================
# Peer configuration
# ============================================================================

configure_peers() {
    local auth_args=()
    if [ -n "${DECPM_IT_AUTH_TOKEN:-}" ]; then
        auth_args=(-H "Authorization: Bearer ${DECPM_IT_AUTH_TOKEN}")
    fi

    # Nothing coordinates before every participant has vetted the coordination
    # package: Canton refuses a create whose observer's participant has not.
    wait_for_coordination_dar "$P1_HTTP" "participant-1"
    wait_for_coordination_dar "$P2_HTTP" "participant-2"
    wait_for_coordination_dar "$P3_HTTP" "participant-3"

    echo "Fetching participant IDs..."
    P1_PARTICIPANT_ID=$(curl -s "${auth_args[@]+"${auth_args[@]}"}" "http://localhost:$P1_HTTP/node-config" | jq -r '.node.participant_id')
    P2_PARTICIPANT_ID=$(curl -s "${auth_args[@]+"${auth_args[@]}"}" "http://localhost:$P2_HTTP/node-config" | jq -r '.node.participant_id')
    P3_PARTICIPANT_ID=$(curl -s "${auth_args[@]+"${auth_args[@]}"}" "http://localhost:$P3_HTTP/node-config" | jq -r '.node.participant_id')

    setup_node_identities

    echo "Participant 1: $P1_PARTICIPANT_ID (node party: $P1_NODE_PARTY)"
    echo "Participant 2: $P2_PARTICIPANT_ID (node party: $P2_NODE_PARTY)"
    echo "Participant 3: $P3_PARTICIPANT_ID (node party: $P3_NODE_PARTY)"

    # Design D2: operators exchange `participant_id,node_party_id,name` and
    # nothing else. A peer without a node party cannot be invited.
    local peers_json
    peers_json=$(cat <<EOF
[
  {"participant_id": "$P1_PARTICIPANT_ID", "name": "Participant 1", "party": "$P1_NODE_PARTY"},
  {"participant_id": "$P2_PARTICIPANT_ID", "name": "Participant 2", "party": "$P2_NODE_PARTY"},
  {"participant_id": "$P3_PARTICIPANT_ID", "name": "Participant 3", "party": "$P3_NODE_PARTY"}
]
EOF
    )

    for port in $P1_HTTP $P2_HTTP $P3_HTTP; do
        curl -s -X POST "${auth_args[@]+"${auth_args[@]}"}" "http://localhost:$port/network-config" \
            -H "Content-Type: application/json" \
            -d "$peers_json" > /dev/null
    done

    echo "Peers configured on all participants"

    # Peers are read from the database per request, so no restart is needed.
    # Each node republishes its registry entry with the new observers; wait
    # until every node sees the other two before any workflow starts.
    wait_for_registry "$P1_HTTP" "participant-1" 2
    wait_for_registry "$P2_HTTP" "participant-2" 2
    wait_for_registry "$P3_HTTP" "participant-3" 2
}

# ============================================================================
# Node identity (design D1)
# ============================================================================

# Allocate one node party per node, grant `ledger-api-user` CanActAs/CanReadAs
# on it, and PUT it as the node identity. Localnet only: it drives the Canton
# JSON Ledger API as `ledger-api-user`, which devnet's real IdP does not allow.
#
# TODO(onledger-phases): devnet needs the same three steps through its own admin
# path (allocate on the participant, grant through /auth/grant-rights, PUT with
# the participant's Keycloak client), and exported P{1,2,3}_NODE_PARTY.
setup_node_identities() {
    if [ -z "${P1_JSON_API:-}" ]; then
        local missing=()
        for v in P1_NODE_PARTY P2_NODE_PARTY P3_NODE_PARTY; do
            [ -z "${!v:-}" ] && missing+=("$v")
        done
        if [ "${#missing[@]}" -gt 0 ]; then
            echo "ERROR: no JSON Ledger API ports and no ${missing[*]}; cannot set node identities"
            exit 1
        fi
        echo "Using the node parties from the environment"
        return 0
    fi

    P1_NODE_PARTY=$(setup_node_identity "$P1_JSON_API" "$P1_HTTP" "participant-1" "decman-node-1")
    P2_NODE_PARTY=$(setup_node_identity "$P2_JSON_API" "$P2_HTTP" "participant-2" "decman-node-2")
    P3_NODE_PARTY=$(setup_node_identity "$P3_JSON_API" "$P3_HTTP" "participant-3" "decman-node-3")
    export P1_NODE_PARTY P2_NODE_PARTY P3_NODE_PARTY
}

# Allocate, grant, PUT — for one node. Echoes the allocated node party id.
setup_node_identity() {
    local json_port=$1
    local http_port=$2
    local name=$3
    local hint=$4

    local auth_args=()
    if [ -n "${DECPM_IT_AUTH_TOKEN:-}" ]; then
        auth_args=(-H "Authorization: Bearer ${DECPM_IT_AUTH_TOKEN}")
    fi

    local party
    party=$(curl -s -X POST "http://localhost:$json_port/v2/parties" \
        -H "Authorization: Bearer $MOCK_TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"party_id_hint\": \"$hint\", \"local_metadata\": {\"annotations\": {}}}" \
        | jq -r '.partyDetails.party // empty')
    if [ -z "$party" ]; then
        echo "ERROR: failed to allocate the node party '$hint' on $name" >&2
        exit 1
    fi

    # The node party submits every coordination command, so its Ledger API
    # user needs both rights on it.
    curl -s -X POST "http://localhost:$json_port/v2/users/ledger-api-user/rights" \
        -H "Authorization: Bearer $MOCK_TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"userId\": \"ledger-api-user\",
             \"rights\": [
               {\"kind\": {\"CanActAs\":  {\"value\": {\"party\": \"$party\"}}}},
               {\"kind\": {\"CanReadAs\": {\"value\": {\"party\": \"$party\"}}}}
             ],
             \"identityProviderId\": \"\"}" > /dev/null

    local response
    response=$(curl -s -w '\n%{http_code}' -X PUT "${auth_args[@]+"${auth_args[@]}"}" \
        "http://localhost:$http_port/node-identity" \
        -H "Content-Type: application/json" \
        -d "{\"node_party_id\": \"$party\", \"user_id\": \"ledger-api-user\"}")
    local code
    code=$(printf '%s' "$response" | tail -1)
    if [ "$code" != "200" ]; then
        echo "ERROR: PUT /node-identity on $name returned $code: $(printf '%s' "$response" | sed '$d')" >&2
        exit 1
    fi

    printf '%s' "$party"
}
