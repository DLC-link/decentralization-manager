#!/bin/bash

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEV_DIR="$SCRIPT_DIR/development"
DARS_DIR="$DEV_DIR/dars"

# Ports
P1_HTTP=8081
P1_NOISE=9001
P2_HTTP=8082
P2_NOISE=9002
P3_HTTP=8083
P3_NOISE=9003

# PIDs and temp files for cleanup
PIDS=()
TEMP_FILES=()

cleanup() {
    echo "Cleaning up..."
    for pid in "${PIDS[@]}"; do
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
        fi
    done
    for f in "${TEMP_FILES[@]}"; do
        rm -f "$f" 2>/dev/null || true
    done
    wait 2>/dev/null || true
    echo "Cleanup complete"
}

trap cleanup EXIT

wait_for_server() {
    local port=$1
    local name=$2
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
    echo "$name is ready"
}

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

        local response=$(curl -s "http://localhost:$port/$endpoint")
        local status=$(echo "$response" | jq -r '.status // empty')

        case "$status" in
            "completed"|"Completed")
                echo "$endpoint completed successfully"
                return 0
                ;;
            "failed"|"Failed")
                local error=$(echo "$response" | jq -r '.error // "Unknown error"')
                echo "ERROR: $endpoint failed: $error"
                exit 1
                ;;
            *)
                sleep 2
                ;;
        esac
    done
}

# ==============================================================================
# Phase 0: Build release
# ==============================================================================
echo "=========================================="
echo "Phase 0: Building release..."
echo "=========================================="
cargo build --release

BINARY="$SCRIPT_DIR/target/release/dec-party-manager"
if [ ! -f "$BINARY" ]; then
    echo "ERROR: Binary not found at $BINARY"
    exit 1
fi

# ==============================================================================
# Phase 1: Start all three nodes
# ==============================================================================
echo ""
echo "=========================================="
echo "Phase 1: Starting nodes..."
echo "=========================================="

# Clean up old data directories (keep config)
rm -rf "$DEV_DIR/participant-1/data"
rm -rf "$DEV_DIR/participant-2/data"
rm -rf "$DEV_DIR/participant-3/data"

# Start participant 1
echo "Starting participant-1..."
"$BINARY" -d "$DEV_DIR/participant-1" serve --host 0.0.0.0 --port $P1_HTTP &
PIDS+=($!)

# Start participant 2
echo "Starting participant-2..."
"$BINARY" -d "$DEV_DIR/participant-2" serve --host 0.0.0.0 --port $P2_HTTP &
PIDS+=($!)

# Start participant 3
echo "Starting participant-3..."
"$BINARY" -d "$DEV_DIR/participant-3" serve --host 0.0.0.0 --port $P3_HTTP &
PIDS+=($!)

# Wait for all servers to be ready
wait_for_server $P1_HTTP "participant-1"
wait_for_server $P2_HTTP "participant-2"
wait_for_server $P3_HTTP "participant-3"

sleep 2

# ==============================================================================
# Phase 2: Configure peers
# ==============================================================================
echo ""
echo "=========================================="
echo "Phase 2: Configuring peers..."
echo "=========================================="

# Get public keys from each participant
P1_KEY=$(curl -s "http://localhost:$P1_HTTP/keys/status" | jq -r '.public_key')
P2_KEY=$(curl -s "http://localhost:$P2_HTTP/keys/status" | jq -r '.public_key')
P3_KEY=$(curl -s "http://localhost:$P3_HTTP/keys/status" | jq -r '.public_key')

echo "Participant 1 key: $P1_KEY"
echo "Participant 2 key: $P2_KEY"
echo "Participant 3 key: $P3_KEY"

# Get node IDs from each participant
P1_NODE_ID=$(curl -s "http://localhost:$P1_HTTP/node-config" | jq -r '.node.node_id')
P2_NODE_ID=$(curl -s "http://localhost:$P2_HTTP/node-config" | jq -r '.node.node_id')
P3_NODE_ID=$(curl -s "http://localhost:$P3_HTTP/node-config" | jq -r '.node.node_id')

echo "Participant 1 node_id: $P1_NODE_ID"
echo "Participant 2 node_id: $P2_NODE_ID"
echo "Participant 3 node_id: $P3_NODE_ID"

# Create peer list JSON
PEERS_JSON=$(cat <<EOF
[
  {"id": "$P1_NODE_ID", "name": "Participant 1", "address": "127.0.0.1", "port": $P1_NOISE, "public_key": "$P1_KEY", "party": null},
  {"id": "$P2_NODE_ID", "name": "Participant 2", "address": "127.0.0.1", "port": $P2_NOISE, "public_key": "$P2_KEY", "party": null},
  {"id": "$P3_NODE_ID", "name": "Participant 3", "address": "127.0.0.1", "port": $P3_NOISE, "public_key": "$P3_KEY", "party": null}
]
EOF
)

# Update peers on all participants
echo "Updating peers on participant-1..."
curl -s -X POST "http://localhost:$P1_HTTP/network-config" \
    -H "Content-Type: application/json" \
    -d "$PEERS_JSON" > /dev/null

echo "Updating peers on participant-2..."
curl -s -X POST "http://localhost:$P2_HTTP/network-config" \
    -H "Content-Type: application/json" \
    -d "$PEERS_JSON" > /dev/null

echo "Updating peers on participant-3..."
curl -s -X POST "http://localhost:$P3_HTTP/network-config" \
    -H "Content-Type: application/json" \
    -d "$PEERS_JSON" > /dev/null

echo "Peers configured on all participants"

# Wait for peers to connect
echo "Waiting for peers to connect..."
sleep 5

# ==============================================================================
# Phase 3: Run onboarding workflow
# ==============================================================================
echo ""
echo "=========================================="
echo "Phase 3: Running onboarding workflow..."
echo "=========================================="

ONBOARDING_REQUEST=$(cat <<EOF
{
  "party_id_prefix": "test-network"
}
EOF
)

echo "Starting onboarding on participant-1..."
curl -s -X POST "http://localhost:$P1_HTTP/onboarding" \
    -H "Content-Type: application/json" \
    -d "$ONBOARDING_REQUEST"
echo ""

poll_status $P1_HTTP "onboarding/status"

# Get the created party ID
echo "Fetching created party..."
sleep 2
PARTIES_RESPONSE=$(curl -s "http://localhost:$P1_HTTP/decentralized-parties")
PARTY_ID=$(echo "$PARTIES_RESPONSE" | jq -r '.parties[0].party_id // empty')

if [ -z "$PARTY_ID" ]; then
    echo "ERROR: No party found after onboarding"
    exit 1
fi

echo "Created party: $PARTY_ID"

# ==============================================================================
# Phase 4: Run contracts deployment workflow
# ==============================================================================
echo ""
echo "=========================================="
echo "Phase 4: Running contracts deployment..."
echo "=========================================="

# Create temp file for the large JSON payload
CONTRACTS_REQUEST_FILE=$(mktemp)
TEMP_FILES+=("$CONTRACTS_REQUEST_FILE")

# Read and base64 encode DAR files
DAR1_B64=$(base64 -i "$DARS_DIR/cbtc-1.0.0.dar")
DAR2_B64=$(base64 -i "$DARS_DIR/cbtc-governance-1.0.0.dar")

# Write JSON to temp file (avoids "argument list too long" error)
cat > "$CONTRACTS_REQUEST_FILE" <<EOF
{
  "decentralized_party_id": "$PARTY_ID",
  "operator_party_hint": "operator",
  "dar_files": [
    {"filename": "cbtc-1.0.0.dar", "data": "$DAR1_B64"},
    {"filename": "cbtc-governance-1.0.0.dar", "data": "$DAR2_B64"}
  ],
  "contracts": [
    {
      "id": "create-govR",
      "name": "CBTCGovernanceRules",
      "package_id": "#cbtc-governance",
      "module_name": "CBTC.Governance",
      "entity_name": "CBTCGovernanceRules",
      "fields": [
        {"type": "decentralized_party"},
        {"type": "operator_party"},
        {"type": "instrument", "id": "CBTC"},
        {"type": "record", "fields": [{"type": "attestors_set"}]},
        {"type": "optional", "inner": {"type": "governance_threshold"}}
      ]
    },
    {
      "id": "create-daR",
      "name": "CBTCDepositAccountRules",
      "package_id": "#cbtc",
      "module_name": "CBTC.DepositAccount",
      "entity_name": "CBTCDepositAccountRules",
      "fields": [
        {"type": "decentralized_party"},
        {"type": "operator_party"},
        {"type": "instrument", "id": "CBTC"}
      ]
    },
    {
      "id": "create-waR",
      "name": "CBTCWithdrawAccountRules",
      "package_id": "#cbtc",
      "module_name": "CBTC.WithdrawAccount",
      "entity_name": "CBTCWithdrawAccountRules",
      "fields": [
        {"type": "decentralized_party"},
        {"type": "operator_party"},
        {"type": "instrument", "id": "CBTC"}
      ]
    }
  ]
}
EOF

echo "Starting contracts deployment on participant-1..."
curl -s -X POST "http://localhost:$P1_HTTP/contracts" \
    -H "Content-Type: application/json" \
    -d @"$CONTRACTS_REQUEST_FILE"
echo ""

poll_status $P1_HTTP "contracts/status"

# ==============================================================================
# Phase 5: Run kick workflow (kick participant 3)
# ==============================================================================
echo ""
echo "=========================================="
echo "Phase 5: Running kick workflow (removing participant-3)..."
echo "=========================================="

# Get party details to find participant 3's UID and owner key
PARTIES_RESPONSE=$(curl -s "http://localhost:$P1_HTTP/decentralized-parties")
PARTICIPANT_3_UID=$(echo "$PARTIES_RESPONSE" | jq -r '.parties[0].participants[2].participant_uid // empty')
OWNER_KEY_3=$(echo "$PARTIES_RESPONSE" | jq -r '.parties[0].owners[2] // empty')

if [ -z "$PARTICIPANT_3_UID" ]; then
    echo "ERROR: Could not find participant 3 UID"
    exit 1
fi

echo "Kicking participant: $PARTICIPANT_3_UID"
echo "Owner key: $OWNER_KEY_3"

KICK_REQUEST=$(cat <<EOF
{
  "decentralized_party_id": "$PARTY_ID",
  "participant_id": "$PARTICIPANT_3_UID",
  "namespace_fingerprint": "$OWNER_KEY_3"
}
EOF
)

echo "Starting kick workflow on participant-1..."
curl -s -X POST "http://localhost:$P1_HTTP/kick" \
    -H "Content-Type: application/json" \
    -d "$KICK_REQUEST"
echo ""

poll_status $P1_HTTP "kick/status"

# ==============================================================================
# Complete
# ==============================================================================
echo ""
echo "=========================================="
echo "Integration tests completed successfully! "
echo "=========================================="

# Keep running for inspection (optional)
# echo "Press Ctrl+C to stop all nodes..."
# wait
