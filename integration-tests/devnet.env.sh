#!/bin/bash
# Devnet target's env-and-bring-up. Sourced by run.sh when --target devnet.
#
# DPM lifecycle is managed via docker-compose (development/docker-compose.yml).
# Bare-process spawning via start_nodes is NOT used on this path.

set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

# ---------------------------------------------------------------------------
# Keycloak config — mandatory username and password, optional URL/realm/client_id.
# ---------------------------------------------------------------------------

# DECPM_KEYCLOAK_USERNAME and DECPM_KEYCLOAK_PASSWORD must always be provided
# explicitly in the environment; they are never read from .env files (not stored on disk).
if [ -z "${DECPM_KEYCLOAK_USERNAME:-}" ]; then
    echo "ERROR: DECPM_KEYCLOAK_USERNAME is not set." >&2
    echo "Export it before running: export DECPM_KEYCLOAK_USERNAME=<username>" >&2
    exit 1
fi

if [ -z "${DECPM_KEYCLOAK_PASSWORD:-}" ]; then
    echo "ERROR: DECPM_KEYCLOAK_PASSWORD is not set." >&2
    echo "Export it before running: export DECPM_KEYCLOAK_PASSWORD=<password>" >&2
    exit 1
fi

# URL / realm / client_id are shared across all three participants and live in
# development/remote/participant-1/.env. If not already in the environment,
# source them from there (participant-1 is picked by convention; all three are
# identical for these values).
PARTICIPANT_1_ENV="$SCRIPT_DIR/../development/remote/participant-1/.env"

_source_keycloak_var() {
    local var=$1
    if [ -z "${!var:-}" ]; then
        if [ ! -f "$PARTICIPANT_1_ENV" ]; then
            echo "ERROR: $var is not set and $PARTICIPANT_1_ENV does not exist." >&2
            echo "Either export $var or create the per-participant .env files." >&2
            exit 1
        fi
        local value
        value=$(grep "^${var}=" "$PARTICIPANT_1_ENV" | cut -d= -f2- | tr -d '\r')
        if [ -z "$value" ]; then
            echo "ERROR: $var not found in $PARTICIPANT_1_ENV and not set in environment." >&2
            exit 1
        fi
        export "$var=$value"
    fi
}

_source_keycloak_var DECPM_KEYCLOAK_URL
_source_keycloak_var DECPM_KEYCLOAK_REALM
_source_keycloak_var DECPM_KEYCLOAK_CLIENT_ID

# ---------------------------------------------------------------------------
# Smoke-check Keycloak reachability via password grant.
#
# The Rust test runner manages its own token lifecycle via KeycloakRefresher
# (reads DECPM_KEYCLOAK_* env vars directly and re-fetches proactively when the
# cached token is within 30s of expiry). We still perform a token fetch here as
# a fail-fast check: if Keycloak is unreachable or the credentials are wrong we
# want to discover that before spending time on `cargo build` / `docker compose`.
# The token value fetched here is NOT used by the Rust runner on devnet.
# ---------------------------------------------------------------------------

TOKEN_URL="${DECPM_KEYCLOAK_URL%/}/realms/${DECPM_KEYCLOAK_REALM}/protocol/openid-connect/token"
_SMOKE_TOKEN=$(curl -s -f -X POST "$TOKEN_URL" \
    -d "grant_type=password" \
    -d "client_id=${DECPM_KEYCLOAK_CLIENT_ID}" \
    -d "username=${DECPM_KEYCLOAK_USERNAME}" \
    -d "password=${DECPM_KEYCLOAK_PASSWORD}" \
    | jq -r .access_token)
if [ -z "$_SMOKE_TOKEN" ] || [ "$_SMOKE_TOKEN" = "null" ]; then
    echo "ERROR: Keycloak password grant failed." >&2
    echo "  TOKEN_URL: $TOKEN_URL" >&2
    echo "  CLIENT_ID: $DECPM_KEYCLOAK_CLIENT_ID" >&2
    echo "Check that DECPM_KEYCLOAK_USERNAME and DECPM_KEYCLOAK_PASSWORD are correct and the Keycloak server is reachable." >&2
    exit 1
fi
unset _SMOKE_TOKEN
# NOTE: MOCK_TOKEN is intentionally NOT exported on the devnet path.
# Fixture::from_env() only reads MOCK_TOKEN when DPM_IT_TARGET=localnet;
# on devnet it uses the DECPM_KEYCLOAK_* vars via KeycloakRefresher.

# ---------------------------------------------------------------------------
# Target + run-id.
# ---------------------------------------------------------------------------

export DPM_IT_TARGET=devnet
export DPM_IT_RUN_ID="dpm-it-$(date -u +%Y%m%d-%H%M%S)-$$"

# ---------------------------------------------------------------------------
# Per-participant ports.
# HTTP ports come from docker-compose.yml (8081/8082/8083).
# Noise ports come from the per-participant .env files:
#   participant-1: DECPM_NOISE_PORT=9000
#   participant-2: DECPM_NOISE_PORT=9001
#   participant-3: DECPM_NOISE_PORT=9002
# ---------------------------------------------------------------------------

export P1_HTTP=8081
export P2_HTTP=8082
export P3_HTTP=8083
export P1_NOISE=9000
export P2_NOISE=9001
export P3_NOISE=9002

# ---------------------------------------------------------------------------
# Localnet no-ops (run.sh calls these unconditionally for the localnet path).
# ---------------------------------------------------------------------------

download_localnet() { :; }
start_localnet()    { :; }
stop_localnet()     { :; }

# ---------------------------------------------------------------------------
# DPM lifecycle via docker-compose.
# ---------------------------------------------------------------------------

KUBE_CONTEXT_DEVNET=${KUBE_CONTEXT_DEVNET:-ieu-devnet}
KUBE_NS_CANTON=${KUBE_NS_CANTON:-catalyst-canton}
CANTON_TUNNEL_PIDS=()

_canton_forward_loop() {
    local idx=$1 local_ledger=$2 local_admin=$3
    while true; do
        kubectl --context="$KUBE_CONTEXT_DEVNET" port-forward \
            "svc/participant-ibtc-devnet-$idx" -n "$KUBE_NS_CANTON" \
            "${local_ledger}:5001" "${local_admin}:5002" >/dev/null 2>&1
        echo "[P$idx canton-tunnel] port-forward disconnected, restarting in 5s..." >&2
        sleep 5
    done
}

start_canton_tunnels() {
    log_phase "Opening kubectl port-forwards to Canton participants"

    # Sanity-check the kubectl context exists. If it doesn't, the user
    # probably hasn't logged in to AWS / hasn't set up kubeconfig yet.
    if ! kubectl config get-contexts "$KUBE_CONTEXT_DEVNET" >/dev/null 2>&1; then
        echo "ERROR: kubectl context '$KUBE_CONTEXT_DEVNET' not found." >&2
        echo "Run 'aws eks update-kubeconfig --name devnet-cluster --region us-east-1 --profile <profile>' first." >&2
        exit 1
    fi

    _canton_forward_loop 1 5001 5002 &
    CANTON_TUNNEL_PIDS+=($!)
    _canton_forward_loop 2 5011 5012 &
    CANTON_TUNNEL_PIDS+=($!)
    _canton_forward_loop 3 5021 5022 &
    CANTON_TUNNEL_PIDS+=($!)

    # Wait for all 6 ports to actually accept connections.
    for port in 5001 5002 5011 5012 5021 5022; do
        local deadline=$(( $(date +%s) + 30 ))
        until nc -z localhost "$port" >/dev/null 2>&1; do
            if [ "$(date +%s)" -ge "$deadline" ]; then
                echo "ERROR: localhost:$port did not open within 30s of starting port-forwards." >&2
                echo "Check that the kubectl context '$KUBE_CONTEXT_DEVNET' is reachable and svc/participant-ibtc-devnet-* exist in namespace '$KUBE_NS_CANTON'." >&2
                stop_canton_tunnels
                exit 1
            fi
            sleep 0.5
        done
    done
    echo "All 6 Canton ports forwarded (5001/5002, 5011/5012, 5021/5022 → svc/participant-ibtc-devnet-{1,2,3})."
}

stop_canton_tunnels() {
    if [ "${#CANTON_TUNNEL_PIDS[@]}" -gt 0 ]; then
        log_phase "Stopping Canton port-forwards"
        for pid in "${CANTON_TUNNEL_PIDS[@]}"; do
            kill -TERM "$pid" 2>/dev/null || true
        done
        # Also kill the kubectl children spawned by the forward loops
        pkill -P $$ -f "kubectl --context=$KUBE_CONTEXT_DEVNET port-forward" 2>/dev/null || true
        CANTON_TUNNEL_PIDS=()
    fi
}

start_nodes() {
    start_canton_tunnels
    log_phase "Starting DPM containers via docker compose"
    # NOTE: docker-compose up --build runs `cargo build --release` inside the
    # container. The local `cargo build --profile release-ci` (in run.sh) also
    # runs for the test crate. This means the DPM binary is compiled twice on
    # the first run (once locally for tests, once inside the container). After
    # the first run the container layer cache keeps subsequent builds fast.
    (cd "${SCRIPT_DIR}/../development" && docker compose up -d --build)
    wait_for_server "$P1_HTTP" "participant-1" "$P1_NOISE"
    wait_for_server "$P2_HTTP" "participant-2" "$P2_NOISE"
    wait_for_server "$P3_HTTP" "participant-3" "$P3_NOISE"
}

stop_nodes() {
    log_phase "Stopping DPM containers"
    (cd "${SCRIPT_DIR}/../development" && docker compose down) || true
}

# setup_directories: data is persisted via docker-compose volumes mounted at
# ./remote/participant-N/data; no temp dirs needed.
setup_directories() { :; }

# ---------------------------------------------------------------------------
# Cleanup — called on EXIT by run.sh's trap.
# ---------------------------------------------------------------------------

cleanup() {
    stop_nodes 2>/dev/null || true
    stop_canton_tunnels 2>/dev/null || true
}
