#!/bin/bash
# Devnet target's env-and-bring-up. Sourced by run.sh when --target devnet.
#
# DPM lifecycle is managed via docker-compose (development/docker-compose.yml).
# Bare-process spawning via start_nodes is NOT used on this path.

set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

# ---------------------------------------------------------------------------
# Source per-participant .env files. These hold the shared Keycloak URL/realm/
# client_id, DPM's username + password for password-grant token fetching, and
# the per-participant P{N}_MEMBER_* credentials for the member-party Canton
# ledger calls. Sourced first so subsequent validation can rely on the values
# being present.
# ---------------------------------------------------------------------------
PARTICIPANT_1_ENV="$SCRIPT_DIR/../development/remote/participant-1/.env"
PARTICIPANT_2_ENV="$SCRIPT_DIR/../development/remote/participant-2/.env"
PARTICIPANT_3_ENV="$SCRIPT_DIR/../development/remote/participant-3/.env"

for _penv in "$PARTICIPANT_1_ENV" "$PARTICIPANT_2_ENV" "$PARTICIPANT_3_ENV"; do
    if [ -f "$_penv" ]; then
        set -a; . "$_penv"; set +a
    fi
done
unset _penv

# ---------------------------------------------------------------------------
# Keycloak config validation (now that .env files have been sourced).
# Calling-shell values take precedence over .env values; missing in BOTH = error.
# ---------------------------------------------------------------------------
for _v in DECPM_KEYCLOAK_URL DECPM_KEYCLOAK_REALM DECPM_KEYCLOAK_CLIENT_ID \
          DECPM_KEYCLOAK_USERNAME DECPM_KEYCLOAK_PASSWORD; do
    if [ -z "${!_v:-}" ]; then
        echo "ERROR: $_v is not set." >&2
        echo "Add it to one of development/remote/participant-{1,2,3}/.env, or export it." >&2
        exit 1
    fi
done
unset _v

# ---------------------------------------------------------------------------
# Validate per-participant member-party credentials (defense-in-depth;
# must be set in development/remote/participant-{1,2,3}/.env before running).
# ---------------------------------------------------------------------------

MEMBER_VARS=(
    P1_MEMBER_PARTY_ID  P1_MEMBER_USER_ID  P1_MEMBER_KEYCLOAK_CLIENT_ID  P1_MEMBER_KEYCLOAK_CLIENT_SECRET
    P2_MEMBER_PARTY_ID  P2_MEMBER_USER_ID  P2_MEMBER_KEYCLOAK_CLIENT_ID  P2_MEMBER_KEYCLOAK_CLIENT_SECRET
    P3_MEMBER_PARTY_ID  P3_MEMBER_USER_ID  P3_MEMBER_KEYCLOAK_CLIENT_ID  P3_MEMBER_KEYCLOAK_CLIENT_SECRET
)
MEMBER_MISSING=()
for _v in "${MEMBER_VARS[@]}"; do
    [ -z "${!_v:-}" ] && MEMBER_MISSING+=("$_v")
done
if [ "${#MEMBER_MISSING[@]}" -gt 0 ]; then
    echo "ERROR: missing member-party env vars for devnet target:" >&2
    printf '  - %s\n' "${MEMBER_MISSING[@]}" >&2
    echo "Add them to development/remote/participant-{1,2,3}/.env." >&2
    exit 1
fi
unset MEMBER_VARS MEMBER_MISSING _v

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

    # Fail fast on AWS-SSO/kubectl auth issues before kicking off retry loops
    # that would silently restart kubectl forever and only surface as a
    # port-forward-timeout 30s later. A single API call probes whether the
    # current SSO token can reach the cluster.
    local auth_probe
    auth_probe=$(kubectl --context="$KUBE_CONTEXT_DEVNET" -n "$KUBE_NS_CANTON" \
        get svc -o name 2>&1)
    if [ $? -ne 0 ]; then
        echo "ERROR: kubectl auth probe failed for context '$KUBE_CONTEXT_DEVNET':" >&2
        echo "$auth_probe" | sed 's/^/  /' >&2
        echo "If you see 'Token has expired', refresh AWS SSO: 'aws sso login --profile <profile>'." >&2
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
