#!/bin/bash
# Devnet target's env-and-bring-up. Sourced by run.sh when --target devnet.

set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

# ---------------------------------------------------------------------------
# Required env vars — fail fast if any are missing.
# ---------------------------------------------------------------------------
REQUIRED_VARS=(
    DECPM_KEYCLOAK_URL
    DECPM_KEYCLOAK_REALM
    DECPM_KEYCLOAK_CLIENT_ID
    DECPM_KEYCLOAK_CLIENT_SECRET
    P1_CANTON_LEDGER P1_CANTON_ADMIN P1_PARTICIPANT_ID
    P2_CANTON_LEDGER P2_CANTON_ADMIN P2_PARTICIPANT_ID
    P3_CANTON_LEDGER P3_CANTON_ADMIN P3_PARTICIPANT_ID
)
MISSING=()
for v in "${REQUIRED_VARS[@]}"; do
    if [ -z "${!v:-}" ]; then
        MISSING+=("$v")
    fi
done
if [ "${#MISSING[@]}" -gt 0 ]; then
    echo "ERROR: missing required env vars for devnet target:" >&2
    printf '  - %s\n' "${MISSING[@]}" >&2
    echo "See ~/.config/dec-party-manager/devnet.env or the devnet runbook." >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Fetch Keycloak token via client_credentials and export as MOCK_TOKEN.
# ---------------------------------------------------------------------------
TOKEN_URL="${DECPM_KEYCLOAK_URL%/}/realms/${DECPM_KEYCLOAK_REALM}/protocol/openid-connect/token"
MOCK_TOKEN=$(curl -s -f -X POST "$TOKEN_URL" \
    -d "grant_type=client_credentials" \
    -d "client_id=${DECPM_KEYCLOAK_CLIENT_ID}" \
    -d "client_secret=${DECPM_KEYCLOAK_CLIENT_SECRET}" \
    | jq -r .access_token)
if [ -z "$MOCK_TOKEN" ] || [ "$MOCK_TOKEN" = "null" ]; then
    echo "ERROR: Keycloak client_credentials grant failed" >&2
    exit 1
fi
export MOCK_TOKEN

# ---------------------------------------------------------------------------
# Target + run-id + paths.
# ---------------------------------------------------------------------------
export DPM_IT_TARGET=devnet
export DPM_IT_RUN_ID="dpm-it-$(date -u +%Y%m%d-%H%M%S)-$$"

DEV_DIR="$(mktemp -d -t dpm-devnet-it-XXXXXX)"
export DEV_DIR

# ---------------------------------------------------------------------------
# Localnet no-ops (run.sh calls these unconditionally).
# ---------------------------------------------------------------------------
download_localnet() { :; }
start_localnet()    { :; }
stop_localnet()     { :; }

# cleanup runs on EXIT via run.sh trap. Stop nodes if start_nodes ran.
cleanup() {
    stop_nodes 2>/dev/null || true
}

# Per-participant ports for the local DPM processes (separate from the
# tunneled Canton ports). Match env.sh defaults.
export P1_HTTP=${P1_HTTP:-8081}
export P2_HTTP=${P2_HTTP:-8082}
export P3_HTTP=${P3_HTTP:-8083}
export P1_NOISE=${P1_NOISE:-9001}
export P2_NOISE=${P2_NOISE:-9002}
export P3_NOISE=${P3_NOISE:-9003}

# Tell DPM the network so it picks Devnet defaults internally (DSO URL,
# Keycloak URL, etc.).
export DECPM_CANTON_NETWORK=devnet

# Deliberately do NOT set DECPM_ADMIN_ROLE. When unset, DPM's require_admin
# middleware treats every authenticated caller as admin (src/cli.rs:92-97).
# Phase 1 is single-user from a developer laptop, so role-gating adds no
# security and would require provisioning Keycloak role-mappers for the M2M
# client. Revisit if/when the IT moves to shared CI infra.
unset DECPM_ADMIN_ROLE
