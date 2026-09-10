#!/bin/bash

HACKATHON_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$HACKATHON_DIR/.." && pwd)"

# The LocalNet lifecycle, the bundle version and the compose invocation live in
# localnet.sh, which integration-tests/env.sh sources too, so the two paths
# cannot drift.
. "$HACKATHON_DIR/localnet.sh"

DECMAN_PROJECT=decman-hackathon
STATE_FILE="$HACKATHON_DIR/.state"

CANTON_TOKEN="eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJhdWQiOiJodHRwczovL2NhbnRvbi5uZXR3b3JrLmdsb2JhbCIsImlhdCI6MTc2Mzc0ODcwMiwic3ViIjoibGVkZ2VyLWFwaS11c2VyIn0.vpkfH4SoM9AZqbE38W4hrvl3xxy69jYs4u8gveskw9k"

DAR_FILES="governance-action-v1-0.1.0.dar
governance-core-v1-0.1.0.dar
governance-token-custody-v1-0.1.0.dar
governance-utility-onboarding-v1-0.4.0.dar
governance-rewards-automation-v1-0.1.0.dar"

say() { printf '\n==> %s\n' "$1"; }
info() { printf '    %s\n' "$1"; }
warn() { printf 'WARNING: %s\n' "$1" >&2; }
die() { printf 'ERROR: %s\n' "$1" >&2; exit 1; }

http_port() {
    case "$1" in
        1) echo 8081 ;;
        2) echo 8082 ;;
        3) echo 8083 ;;
        *) die "unknown node $1" ;;
    esac
}

noise_port() {
    case "$1" in
        1) echo 9001 ;;
        2) echo 9002 ;;
        3) echo 9003 ;;
        *) die "unknown node $1" ;;
    esac
}

json_api_port() {
    case "$1" in
        1) echo 3975 ;;
        2) echo 2975 ;;
        3) echo 4975 ;;
        *) die "unknown node $1" ;;
    esac
}

node_name() { echo "decman-$1"; }

require_tools() {
    local missing=""
    local t
    for t in docker curl jq base64 tar; do
        command -v "$t" >/dev/null 2>&1 || missing="$missing $t"
    done
    [ -z "$missing" ] || die "missing required tools:$missing"

    local compose_version
    compose_version=$(docker compose version --short 2>/dev/null || echo "")
    [ -n "$compose_version" ] || die "docker compose v2 is required (Docker Desktop ships it)"
    printf '2.1.1\n%s\n' "$compose_version" | sort -CV \
        || die "docker compose v2.1.1 or newer is required (found $compose_version)"

    docker info >/dev/null 2>&1 || die "the Docker daemon is not reachable — start Docker Desktop first"
}

check_docker_resources() {
    local mem cpus mem_gb
    mem=$(docker info --format '{{.MemTotal}}' 2>/dev/null || echo 0)
    cpus=$(docker info --format '{{.NCPU}}' 2>/dev/null || echo 0)
    mem_gb=$((mem / 1073741824))

    info "Docker reports ${mem_gb}GB memory and ${cpus} CPUs"
    if [ "$mem_gb" -lt 10 ]; then
        warn "LocalNet reserves about 9GB for Canton, Splice and Postgres. Raise the Docker memory limit to 12GB or more (Docker Desktop: Settings > Resources)."
    fi
    if [ "$cpus" -lt 4 ]; then
        warn "Give Docker 4 CPUs or more, or the stack starts very slowly."
    fi
}

ports_in_use() {
    local port used=""
    command -v lsof >/dev/null 2>&1 || return 0
    for port in "$@"; do
        if lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1; then
            used="$used $port"
        fi
    done
    printf '%s' "$used"
}

decman_compose() {
    docker compose -p "$DECMAN_PROJECT" -f "$HACKATHON_DIR/docker-compose.yml" "$@"
}

req() {
    local method=$1 url=$2 data=${3-} token=${4-}
    local args raw code body
    args=(-sS -X "$method" -w '\n%{http_code}' --max-time "${REQ_TIMEOUT:-300}")
    if [ -n "$data" ]; then
        args+=(-H 'Content-Type: application/json' -d "$data")
    fi
    if [ -n "$token" ]; then
        args+=(-H "Authorization: Bearer $token")
    fi
    raw=$(curl "${args[@]}" "$url") || die "$method $url could not be reached"
    code=${raw##*$'\n'}
    body=${raw%$'\n'*}
    case "$code" in
        2*) printf '%s' "$body" ;;
        *) die "$method $url returned $code: $body" ;;
    esac
}

dm_get() { req GET "http://localhost:$1$2"; }
dm_post() { req POST "http://localhost:$1$2" "$3"; }
dm_put() { req PUT "http://localhost:$1$2" "$3"; }
canton_post() { req POST "http://localhost:$1$2" "$3" "$CANTON_TOKEN"; }

try_get() { curl -sSf --max-time 30 "http://localhost:$1$2" 2>/dev/null; }

wait_for_node() {
    local idx=$1 port attempt=0
    port=$(http_port "$idx")
    while [ "$attempt" -lt 120 ]; do
        if try_get "$port" /healthz >/dev/null; then
            info "$(node_name "$idx") is up on port $port"
            return 0
        fi
        attempt=$((attempt + 1))
        sleep 2
    done
    die "$(node_name "$idx") did not answer on port $port. Read its log with: docker compose -p $DECMAN_PROJECT logs $(node_name "$idx")"
}

wait_for_all_nodes() {
    local idx
    for idx in 1 2 3; do
        wait_for_node "$idx"
    done
}

peers_connected() {
    local idx port json total bad
    for idx in 1 2 3; do
        port=$(http_port "$idx")
        json=$(try_get "$port" /participants-status) || return 1
        total=$(printf '%s' "$json" | jq '.statuses | length')
        bad=$(printf '%s' "$json" | jq '[.statuses[] | select(.status != "Connected" and .status != "CurrentNode")] | length')
        [ "$total" = "3" ] || return 1
        [ "$bad" = "0" ] || return 1
    done
    return 0
}

poll_workflow() {
    local port=$1 endpoint=$2 label=$3 attempt=0 response status error
    info "waiting for $label"
    while [ "$attempt" -lt 180 ]; do
        response=$(try_get "$port" "$endpoint" || echo '{}')
        status=$(printf '%s' "$response" | jq -r '.status // empty')
        case "$status" in
            completed | Completed)
                info "$label completed"
                return 0
                ;;
            failed | Failed)
                error=$(printf '%s' "$response" | jq -r '.error // "unknown error"')
                die "$label failed: $error"
                ;;
        esac
        attempt=$((attempt + 1))
        sleep 2
    done
    die "$label did not complete in time"
}

accept_invitation() {
    local idx=$1 kind=$2 port attempt=0 response id
    port=$(http_port "$idx")
    info "waiting for a $kind invitation on $(node_name "$idx")"
    while [ "$attempt" -lt 120 ]; do
        response=$(try_get "$port" /invitations || echo '{}')
        id=$(printf '%s' "$response" | jq -r --arg kind "$kind" \
            'first(.invitations[]? | select(.invitation_type == $kind) | .id) // empty')
        if [ -n "$id" ]; then
            dm_post "$port" /invitations/accept "$(jq -n --arg id "$id" '{id: $id}')" >/dev/null
            info "$(node_name "$idx") accepted the $kind invitation"
            return 0
        fi
        attempt=$((attempt + 1))
        sleep 2
    done
    die "no $kind invitation arrived on $(node_name "$idx")"
}

load_state() {
    [ -f "$STATE_FILE" ] && . "$STATE_FILE"
    return 0
}

state_set() {
    local key=$1 value=$2
    touch "$STATE_FILE"
    grep -v "^$key=" "$STATE_FILE" > "$STATE_FILE.tmp" || true
    printf '%s=%s\n' "$key" "$value" >> "$STATE_FILE.tmp"
    mv "$STATE_FILE.tmp" "$STATE_FILE"
    eval "$key=\$value"
}

require_stack_up() {
    local idx
    for idx in 1 2 3; do
        try_get "$(http_port "$idx")" /healthz >/dev/null \
            || die "$(node_name "$idx") is not answering. Start the stack with hackathon/up.sh"
    done
}
