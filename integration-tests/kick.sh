#!/bin/bash

# Kick participant-3 from the decentralized party.
# Sourced by run.sh — expects env.sh variables and PARTY_ID/PARTY_JSON
# from create-dec-party.sh to be available.

# Refetch party details
PARTIES_RESPONSE=$(curl -s "http://localhost:$P1_HTTP/decentralized-parties")
PARTY_JSON=$(echo "$PARTIES_RESPONSE" | jq --arg prefix "$PARTY_PREFIX" \
    '.parties[] | select(.party_id | startswith($prefix))')

PARTICIPANT_3_UID=$(echo "$PARTY_JSON" | jq -r '.participants[2].participant_uid // empty')
OWNER_KEY_3=$(echo "$PARTY_JSON" | jq -r '.owners[2] // empty')
CURRENT_THRESHOLD=$(echo "$PARTY_JSON" | jq -r '.threshold // 2')

if [ -z "$PARTICIPANT_3_UID" ]; then
    echo "ERROR: Could not find participant 3 UID"
    exit 1
fi

# 2 remaining participants → threshold 2
NEW_THRESHOLD=2

echo "Kicking participant: $PARTICIPANT_3_UID"
echo "Owner key: $OWNER_KEY_3"
echo "Current threshold: $CURRENT_THRESHOLD → $NEW_THRESHOLD"

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

# Only participant-2 votes (participant-3 is being kicked)
accept_invitation $P2_HTTP "participant-2" "Kick" &
PID_ACCEPT1=$!
wait $PID_ACCEPT1

poll_status $P1_HTTP "kick/status"
