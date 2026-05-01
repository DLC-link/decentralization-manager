#!/bin/bash

# G5: dec_party_identity survives onboarding completion + dismiss.
#
# Run onboarding to completion. Snapshot dec_party_identity row count for the
# new dec_party. Dismiss the onboarding workflow_runs row. Re-read the rows
# and assert dec_party_identity is preserved across the dismiss.
#
# Sourced by run.sh.

PARTY_PREFIX="identity-keep-$(date +%s)"
INSTANCE_NAME="$PARTY_PREFIX-creation"
P1_DB_FILE="$DEV_DIR/participant-1/data/decpm.db"

echo "[G5] running onboarding to completion with prefix $PARTY_PREFIX"
curl -s -X POST "http://localhost:$P1_HTTP/onboarding" \
    -H "Content-Type: application/json" \
    -d "{\"party_id_prefix\": \"$PARTY_PREFIX\", \"peer_ids\": [\"$P2_PARTICIPANT_ID\", \"$P3_PARTICIPANT_ID\"]}" \
    > /dev/null

accept_invitation $P2_HTTP "participant-2" "Onboarding" &
ACC1=$!
accept_invitation $P3_HTTP "participant-3" "Onboarding" &
ACC2=$!
wait $ACC1 $ACC2

poll_status $P1_HTTP "onboarding/status"

# Resolve the dec_party_id created by this run.
sleep 2
DEC_PARTY_ID=$(curl -s "http://localhost:$P1_HTTP/decentralized-parties?prefix=$PARTY_PREFIX" \
    | jq -r --arg p "$PARTY_PREFIX" '.parties[] | select(.party_id | startswith($p)) | .party_id' | head -1)

if [ -z "$DEC_PARTY_ID" ] || [ "$DEC_PARTY_ID" = "null" ]; then
    echo "[G5] ERROR: dec_party_id not resolved"
    exit 1
fi

# Count identity rows pre-dismiss.
IDENTITY_BEFORE=$(sqlite3 "$P1_DB_FILE" \
    "SELECT COUNT(*) FROM dec_party_identity WHERE dec_party_id='$DEC_PARTY_ID';")
if [ "$IDENTITY_BEFORE" -lt 1 ]; then
    echo "[G5] ERROR: expected dec_party_identity rows for $DEC_PARTY_ID, got $IDENTITY_BEFORE"
    exit 1
fi
echo "[G5] $IDENTITY_BEFORE dec_party_identity rows pre-dismiss"

# Dismiss the onboarding row.
HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' \
    -X POST "http://localhost:$P1_HTTP/workflows/$INSTANCE_NAME/dismiss" \
    -H "Content-Type: application/json")
if [ "$HTTP_CODE" != "200" ]; then
    echo "[G5] ERROR: dismiss returned $HTTP_CODE"
    exit 1
fi

# Identity rows must be unchanged.
IDENTITY_AFTER=$(sqlite3 "$P1_DB_FILE" \
    "SELECT COUNT(*) FROM dec_party_identity WHERE dec_party_id='$DEC_PARTY_ID';")
if [ "$IDENTITY_AFTER" != "$IDENTITY_BEFORE" ]; then
    echo "[G5] ERROR: dec_party_identity rows changed across dismiss ($IDENTITY_BEFORE → $IDENTITY_AFTER)"
    exit 1
fi

echo "[G5] dec_party_identity preserved across dismiss ($IDENTITY_AFTER rows)"
