#!/bin/bash

# Create a decentralized party via the onboarding workflow.
# Sourced by run.sh — expects env.sh variables to be available.
#
# Exports: PARTY_ID, PARTY_JSON

# Find next available test-network index
EXISTING_PARTIES=$(curl -s "http://localhost:$P1_HTTP/decentralized-parties")
MAX_INDEX=$(echo "$EXISTING_PARTIES" | jq -r \
    '[.parties[].party_id | select(startswith("test-network-")) | split("::")[0] | split("-")[2] | tonumber] | max // 0')
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

# Accept invitations on attestors in parallel
accept_invitation $P2_HTTP "participant-2" "Onboarding" &
PID_ACCEPT1=$!
accept_invitation $P3_HTTP "participant-3" "Onboarding" &
PID_ACCEPT2=$!
wait $PID_ACCEPT1 $PID_ACCEPT2

poll_status $P1_HTTP "onboarding/status"

# Extract the created party
sleep 2
PARTIES_RESPONSE=$(curl -s "http://localhost:$P1_HTTP/decentralized-parties")
PARTY_ID=$(echo "$PARTIES_RESPONSE" | jq -r --arg prefix "$PARTY_PREFIX" \
    '.parties[] | select(.party_id | startswith($prefix)) | .party_id' | head -1)

if [ -z "$PARTY_ID" ]; then
    echo "ERROR: No party found after onboarding"
    exit 1
fi

PARTY_JSON=$(echo "$PARTIES_RESPONSE" | jq --arg prefix "$PARTY_PREFIX" \
    '.parties[] | select(.party_id | startswith($prefix))')

echo "Created party: $PARTY_ID"
echo "Participant UIDs:"
echo "$PARTY_JSON" | jq -r '.participants[].participant_uid'
