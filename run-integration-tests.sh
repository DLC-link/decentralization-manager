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

    # Also wait for keys to be generated (keypair is generated in background task)
    attempt=0
    while true; do
        local key=$(curl -s "http://localhost:$port/keys/status" | jq -r '.public_key // empty')
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
    echo "$name is ready (keys generated)"
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

# Wait for invitation and accept it
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

        local response=$(curl -s "http://localhost:$port/invitations")
        local invitation_id=$(echo "$response" | jq -r --arg type "$invitation_type" '.invitations[] | select(.invitation_type == $type) | .id // empty' | head -1)

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

# Get participant IDs from each participant
P1_PARTICIPANT_ID=$(curl -s "http://localhost:$P1_HTTP/node-config" | jq -r '.node.participant_id')
P2_PARTICIPANT_ID=$(curl -s "http://localhost:$P2_HTTP/node-config" | jq -r '.node.participant_id')
P3_PARTICIPANT_ID=$(curl -s "http://localhost:$P3_HTTP/node-config" | jq -r '.node.participant_id')

echo "Participant 1 ID: $P1_PARTICIPANT_ID"
echo "Participant 2 ID: $P2_PARTICIPANT_ID"
echo "Participant 3 ID: $P3_PARTICIPANT_ID"

# Create peer list JSON
PEERS_JSON=$(cat <<EOF
[
  {"participant_id": "$P1_PARTICIPANT_ID", "name": "Participant 1", "address": "127.0.0.1", "port": $P1_NOISE, "public_key": "$P1_KEY", "party": null},
  {"participant_id": "$P2_PARTICIPANT_ID", "name": "Participant 2", "address": "127.0.0.1", "port": $P2_NOISE, "public_key": "$P2_KEY", "party": null},
  {"participant_id": "$P3_PARTICIPANT_ID", "name": "Participant 3", "address": "127.0.0.1", "port": $P3_NOISE, "public_key": "$P3_KEY", "party": null}
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

# Small delay to ensure writes are flushed
sleep 1

# Restart servers to reload peer_keys (the peer_keys map is built at startup and not reloaded on config change)
echo "Restarting servers to reload peer configuration..."

for pid in "${PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true
done
wait 2>/dev/null || true
PIDS=()

sleep 2

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

echo "Servers restarted with peer configuration"
sleep 2

# ==============================================================================
# Phase 3: Run onboarding workflow
# ==============================================================================
echo ""
echo "=========================================="
echo "Phase 3: Running onboarding workflow..."
echo "=========================================="

# Find the next available test-network index
EXISTING_PARTIES=$(curl -s "http://localhost:$P1_HTTP/decentralized-parties")
MAX_INDEX=$(echo "$EXISTING_PARTIES" | jq -r '[.parties[].party_id | select(startswith("test-network-")) | split("::")[0] | split("-")[2] | tonumber] | max // 0')
NEXT_INDEX=$((MAX_INDEX + 1))
PARTY_PREFIX="test-network-$NEXT_INDEX"
echo "Using party ID prefix: $PARTY_PREFIX (next available index)"

ONBOARDING_REQUEST=$(cat <<EOF
{
  "party_id_prefix": "$PARTY_PREFIX",
  "peer_ids": ["$P2_PARTICIPANT_ID", "$P3_PARTICIPANT_ID"]
}
EOF
)

echo "Starting onboarding on participant-1..."
curl -s -X POST "http://localhost:$P1_HTTP/onboarding" \
    -H "Content-Type: application/json" \
    -d "$ONBOARDING_REQUEST"
echo ""

# Accept invitations on attestors
accept_invitation $P2_HTTP "participant-2" "Onboarding" &
PID1=$!
accept_invitation $P3_HTTP "participant-3" "Onboarding" &
PID2=$!
wait $PID1 $PID2

poll_status $P1_HTTP "onboarding/status"

# Get the created party ID (find by prefix since there may be old parties from previous runs)
echo "Fetching created party..."
sleep 2
PARTIES_RESPONSE=$(curl -s "http://localhost:$P1_HTTP/decentralized-parties")
PARTY_ID=$(echo "$PARTIES_RESPONSE" | jq -r --arg prefix "$PARTY_PREFIX" '.parties[] | select(.party_id | startswith($prefix)) | .party_id' | head -1)

if [ -z "$PARTY_ID" ]; then
    echo "ERROR: No party found after onboarding"
    exit 1
fi

echo "Created party: $PARTY_ID"

# Get participant UIDs from the party for later use
PARTY_JSON=$(echo "$PARTIES_RESPONSE" | jq --arg prefix "$PARTY_PREFIX" '.parties[] | select(.party_id | startswith($prefix))')
PARTICIPANT_UIDS=$(echo "$PARTY_JSON" | jq -r '.participants[].participant_uid')
echo "Participant UIDs in party:"
echo "$PARTICIPANT_UIDS"

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

# Get participant UIDs as JSON array for the request
PARTICIPANT_IDS_JSON=$(echo "$PARTY_JSON" | jq '[.participants[].participant_uid]')

# Write JSON to temp file (avoids "argument list too long" error)
cat > "$CONTRACTS_REQUEST_FILE" <<EOF
{
  "decentralized_party_id": "$PARTY_ID",
  "participant_ids": $PARTICIPANT_IDS_JSON,
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

# Accept invitations on attestors
accept_invitation $P2_HTTP "participant-2" "Contracts" &
PID1=$!
accept_invitation $P3_HTTP "participant-3" "Contracts" &
PID2=$!
wait $PID1 $PID2

poll_status $P1_HTTP "contracts/status"

# ==============================================================================
# Phase 5: Run kick workflow (kick participant 3)
# ==============================================================================
echo ""
echo "=========================================="
echo "Phase 5: Running kick workflow (removing participant-3)..."
echo "=========================================="

# Refetch party details to find participant 3's UID and owner key
PARTIES_RESPONSE=$(curl -s "http://localhost:$P1_HTTP/decentralized-parties")
PARTY_JSON=$(echo "$PARTIES_RESPONSE" | jq --arg prefix "$PARTY_PREFIX" '.parties[] | select(.party_id | startswith($prefix))')
PARTICIPANT_3_UID=$(echo "$PARTY_JSON" | jq -r '.participants[2].participant_uid // empty')
OWNER_KEY_3=$(echo "$PARTY_JSON" | jq -r '.owners[2] // empty')
CURRENT_THRESHOLD=$(echo "$PARTY_JSON" | jq -r '.threshold // 2')

if [ -z "$PARTICIPANT_3_UID" ]; then
    echo "ERROR: Could not find participant 3 UID"
    exit 1
fi

# Calculate new threshold (majority of remaining participants: 2 participants -> threshold 2)
NEW_THRESHOLD=2

echo "Kicking participant: $PARTICIPANT_3_UID"
echo "Owner key: $OWNER_KEY_3"
echo "Current threshold: $CURRENT_THRESHOLD"
echo "New threshold: $NEW_THRESHOLD"

KICK_REQUEST=$(cat <<EOF
{
  "decentralized_party_id": "$PARTY_ID",
  "participant_id": "$PARTICIPANT_3_UID",
  "namespace_fingerprint": "$OWNER_KEY_3",
  "new_threshold": $NEW_THRESHOLD
}
EOF
)

echo "Starting kick workflow on participant-1..."
curl -s -X POST "http://localhost:$P1_HTTP/kick" \
    -H "Content-Type: application/json" \
    -d "$KICK_REQUEST"
echo ""

# Accept invitation on participant-2 (participant-3 is being kicked, won't participate)
accept_invitation $P2_HTTP "participant-2" "Kick" &
PID1=$!
wait $PID1

poll_status $P1_HTTP "kick/status"

# ==============================================================================
# Complete
# ==============================================================================
echo ""
echo "=========================================="
echo "Integration tests completed successfully!"
echo "=========================================="

# Keep running for inspection (optional)
# echo "Press Ctrl+C to stop all nodes..."
# wait
