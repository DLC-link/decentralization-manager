#!/bin/bash
# Devnet target's env-and-bring-up. Sourced by run.sh when --target devnet.
#
# DPM lifecycle: bare processes spawned by common.sh's start_nodes (same model
# as localnet, with Canton endpoints pointing at tunneled-localhost ports
# instead of localnet's docker-compose ports). Each DPM picks its
# DECPM_CANTON_* / DECPM_KEYCLOAK_* / DECPM_NOISE_PORT from the child env that
# start_nodes assembles.

set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

# ---------------------------------------------------------------------------
# Source per-participant .env files for the shared Keycloak vars + per-DPM
# member-party credentials (P{N}_MEMBER_*). The DECPM_CANTON_*_HOST/PORT and
# DECPM_NOISE_PORT keys are duplicated across .env files with per-participant
# values; sourcing all three sequentially leaves the last one's values in env,
# which is fine since we override them per-DPM via P{N}_CANTON_* exports below.
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
# Keycloak config validation.
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
# Per-participant member-party credentials validation.
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
# Per-participant admin-client credentials validation. The DPM's
# POST /auth/grant-rights handler uses these (passed per-call from the test
# runner) to mint an admin Keycloak token via client_credentials, then calls
# Canton's UserManagementService.GrantUserRights via gRPC to grant
# CoordinatorUser / attestorUserN the act_as + read_as rights on the freshly-
# created decentralized party. Replaces the JSON-Ledger-API grant_rights call
# that the localnet path uses.
# ---------------------------------------------------------------------------
ADMIN_VARS=(
    P1_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_ID  P1_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_SECRET
    P2_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_ID  P2_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_SECRET
    P3_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_ID  P3_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_SECRET
)
ADMIN_MISSING=()
for _v in "${ADMIN_VARS[@]}"; do
    [ -z "${!_v:-}" ] && ADMIN_MISSING+=("$_v")
done
if [ "${#ADMIN_MISSING[@]}" -gt 0 ]; then
    echo "ERROR: missing participant-admin env vars for devnet target:" >&2
    printf '  - %s
' "${ADMIN_MISSING[@]}" >&2
    echo "Add them to development/remote/participant-{1,2,3}/.env." >&2
    exit 1
fi
unset ADMIN_VARS ADMIN_MISSING _v

# ---------------------------------------------------------------------------
# Smoke-check Keycloak reachability via password grant.
# The Rust test runner manages its own token lifecycle via KeycloakRefresher;
# this is a fail-fast check that catches a misconfigured Keycloak client BEFORE
# we spend time on cargo build / DPM spawn. The fetched token is NOT used by
# the Rust runner.
# ---------------------------------------------------------------------------
# Normalize the base URL: strip a trailing slash, then strip a trailing
# `/auth` if present. This lets users configure DECPM_KEYCLOAK_URL either as
# `https://kc.example.com` or `https://kc.example.com/auth` — same shape as
# src/auth/mod.rs::token_url uses on the DPM side, so the smoke check
# composes the same endpoint the DPM's runtime auth path will reach.
_KC_BASE="${DECPM_KEYCLOAK_URL%/}"; _KC_BASE="${_KC_BASE%/auth}"
TOKEN_URL="${_KC_BASE}/auth/realms/${DECPM_KEYCLOAK_REALM}/protocol/openid-connect/token"
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
    echo "Check that DECPM_KEYCLOAK_USERNAME and DECPM_KEYCLOAK_PASSWORD are correct and Keycloak is reachable." >&2
    exit 1
fi
# Reused by common.sh:wait_for_server for the readiness probes against the
# real JwtValidator. Fresh token (~5min TTL) easily outlives start_nodes.
export DPM_IT_AUTH_TOKEN="$_SMOKE_TOKEN"
unset _SMOKE_TOKEN

# ---------------------------------------------------------------------------
# Target + run-id + DEV_DIR.
# ---------------------------------------------------------------------------
export DPM_IT_TARGET=devnet
export DPM_IT_RUN_ID="dpm-it-$(date -u +%Y%m%d-%H%M%S)-$$"
DEV_DIR="$(mktemp -d -t dpm-devnet-it-XXXXXX)"
export DEV_DIR

# Canton topology poll budget for this run. Defaults baked into the binary
# are 30 attempts × 2s = 60s (src/consts.rs), tuned for localnet's
# docker-compose Canton where topology reads return in ms. On devnet the
# kubectl-tunneled Canton response time varies significantly across runs;
# one observed devnet run on 9fd91be reached identity_survives_dismiss's
# re-onboarding step where the P2P-topology poll exhausted 60s. Bumping
# to 90 attempts × 2s = 180s gives headroom while staying well under the
# scenario's outer deadline. Override on the CLI if you see further
# timeouts.
export DPM_TOPOLOGY_RETRY_MAX_ATTEMPTS=90

# ---------------------------------------------------------------------------
# Per-participant ports.
# - HTTP: 8081/8082/8083 (DPM's own HTTP API)
# - Noise: 9000/9001/9002 (per-DPM Noise listener; matches DECPM_NOISE_PORT
#   values in the per-participant .env files)
# - Canton ledger:  5001/5011/5021 (tunneled to participant-ibtc-devnet-{1,2,3}
#   service port 5001 via kubectl port-forward)
# - Canton admin:   5002/5012/5022 (tunneled the same way to service port 5002)
# These are exported (not just shell vars) so chaos phases that respawn DPM
# inherit them.
# ---------------------------------------------------------------------------
export P1_HTTP=8081  P2_HTTP=8082  P3_HTTP=8083
export P1_NOISE=9000 P2_NOISE=9001 P3_NOISE=9002
export P1_CANTON_LEDGER=5001 P1_CANTON_ADMIN=5002
export P2_CANTON_LEDGER=5011 P2_CANTON_ADMIN=5012
export P3_CANTON_LEDGER=5021 P3_CANTON_ADMIN=5022

# ---------------------------------------------------------------------------
# Binary path — same path the localnet builds also use. run.sh's cargo build
# produces this on both targets.
# ---------------------------------------------------------------------------
export BINARY="$SCRIPT_DIR/../target/release-ci/dec-party-manager"

# ---------------------------------------------------------------------------
# DPM's own Keycloak + Canton config: ensure these are exported so the DPM
# child processes spawned by start_nodes inherit them. (They were sourced via
# the .env files above, but `set -a` only auto-exports during the source; we
# re-export explicitly for clarity.)
#
# DECPM_CANTON_NETWORK=devnet drives DPM's Network::Devnet defaults (DSO URL,
# Keycloak URL fallback). The per-DPM CANTON_LEDGER_*/CANTON_ADMIN_* values
# are assembled by start_nodes from the P{N}_CANTON_* exports above.
# ---------------------------------------------------------------------------
export DECPM_KEYCLOAK_URL DECPM_KEYCLOAK_REALM DECPM_KEYCLOAK_CLIENT_ID
export DECPM_CANTON_NETWORK=devnet

# require_admin is a no-op when DECPM_ADMIN_ROLE is unset (single-user
# laptop setup; not running in shared CI yet).
unset DECPM_ADMIN_ROLE 2>/dev/null || true

# ---------------------------------------------------------------------------
# Canton tunnel lifecycle (kubectl port-forward against the catalyst-canton
# namespace in the ieu-devnet EKS cluster).
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

    if ! kubectl config get-contexts "$KUBE_CONTEXT_DEVNET" >/dev/null 2>&1; then
        echo "ERROR: kubectl context '$KUBE_CONTEXT_DEVNET' not found." >&2
        echo "Run 'aws eks update-kubeconfig --name devnet-cluster --region us-east-1 --profile <profile>' first." >&2
        exit 1
    fi

    # Fail fast on AWS-SSO / kubectl auth issues so retry loops don't silently
    # restart kubectl forever and only surface 30s later as a port timeout.
    #
    # `if ! cmd` is required here (rather than `cmd; if [ $? -ne 0 ]`) because
    # run.sh runs under `set -eu`. The old form caused the script to exit
    # immediately on kubectl failure, before reaching the helpful error
    # message — the user would see only the "Opening kubectl port-forwards"
    # header and nothing else. Confirmed lived experience on a run that hit
    # `aws sso login --profile ieu-sysadmin` token expiry. Originally flagged
    # by Copilot's review on #142 ("comments suppressed due to low confidence
    # #2") — this is the followup landing that fix.
    local auth_probe
    if ! auth_probe=$(kubectl --context="$KUBE_CONTEXT_DEVNET" -n "$KUBE_NS_CANTON" \
        get svc -o name 2>&1); then
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

    for port in 5001 5002 5011 5012 5021 5022; do
        local deadline=$(( $(date +%s) + 30 ))
        until nc -z localhost "$port" >/dev/null 2>&1; do
            if [ "$(date +%s)" -ge "$deadline" ]; then
                echo "ERROR: localhost:$port did not open within 30s of starting port-forwards." >&2
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
        # Stop the retry loops first so they don't respawn kubectl while
        # we're trying to clean it up.
        for pid in "${CANTON_TUNNEL_PIDS[@]}"; do
            kill -TERM "$pid" 2>/dev/null || true
        done
        # Then kill the kubectl port-forward processes themselves. They are
        # GRANDCHILDREN of run.sh's $$ (subshell -> kubectl), so the
        # previous `pkill -P $$ -f ...` form required both `-P $$` (direct
        # children of $$) AND the kubectl-matching command pattern to
        # match — kubectl isn't a direct child, so nothing ever matched
        # and the forwards leaked between runs (observed 2026-05-19 after
        # an EXIT_RUNSH=0 run #9 left three svc/participant-ibtc-devnet-*
        # port-forwards alive). When the loop subshell above dies, kubectl
        # is reparented to init and stays alive until killed explicitly.
        # The -f pattern is scoped to our devnet context + ibtc-devnet
        # service so it won't affect unrelated kubectl invocations.
        pkill -f "kubectl --context=$KUBE_CONTEXT_DEVNET port-forward svc/participant-ibtc-devnet" 2>/dev/null || true
        CANTON_TUNNEL_PIDS=()
    fi
}

# ---------------------------------------------------------------------------
# Lifecycle hooks invoked by run.sh.
# - start_localnet / stop_localnet: the no-op slots that run.sh always calls.
#   We piggyback on them to start/stop the Canton tunnels, so the tunnels are
#   up *before* start_nodes spawns DPM and torn down on exit.
# - download_localnet: no-op (no Splice bundle to fetch).
# - start_nodes, setup_directories, configure_peers: use common.sh's bare-
#   process versions unchanged. start_nodes propagates the DECPM_KEYCLOAK_*
#   we've exported above to each DPM child.
# - cleanup: stop the DPM processes (stop_nodes from common.sh) AND the
#   tunnels.
# ---------------------------------------------------------------------------
download_localnet() { :; }
start_localnet()    { start_canton_tunnels; }
stop_localnet()     { stop_canton_tunnels; }

cleanup() {
    stop_nodes 2>/dev/null || true

    # Also reap any PIDs that Rust chaos phases respawned during the run.
    # `tests/common/processes.rs::spawn_node` appends each respawned PID to
    # `$DEV_DIR/restarted-pids`. The localnet `cleanup` in `env.sh` already
    # does this; devnet's override missed the equivalent step, leaking the
    # chaos-respawned DPMs after every run that exercised G1-P2. Those
    # leftovers then trip `check_dpm_ports_free` on the next bringup or
    # (worse, pre-port-check fix) silently steal `wait_for_server`'s TCP
    # readiness probes.
    if [ -n "${DEV_DIR:-}" ] && [ -f "$DEV_DIR/restarted-pids" ]; then
        while IFS= read -r pid; do
            if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
                kill -9 "$pid" 2>/dev/null || true
            fi
        done < "$DEV_DIR/restarted-pids"
    fi

    stop_canton_tunnels 2>/dev/null || true
}
