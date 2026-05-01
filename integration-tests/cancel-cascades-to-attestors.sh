#!/bin/bash

# G6: Cancel during in-flight attestor run cancels accepted-but-running rows.
#
# Start onboarding P1→{P2,P3}. P2 accepts (attestor row InProgress). Before P3
# accepts, P1 calls /onboarding/cancel. Assert: P2 attestor row is now
# Cancelled with appropriate error message; P3 has no leftover pending invite.
#
# Sourced by run.sh.

PARTY_PREFIX="cancel-cascade-$(date +%s)"
INSTANCE_NAME="$PARTY_PREFIX-creation"
P1_DB_FILE="$DEV_DIR/participant-1/data/decpm.db"
P2_DB_FILE="$DEV_DIR/participant-2/data/decpm.db"
P3_DB_FILE="$DEV_DIR/participant-3/data/decpm.db"

echo "[G6] starting onboarding on P1 with prefix $PARTY_PREFIX"
curl -s -X POST "http://localhost:$P1_HTTP/onboarding" \
    -H "Content-Type: application/json" \
    -d "{\"party_id_prefix\": \"$PARTY_PREFIX\", \"peer_ids\": [\"$P2_PARTICIPANT_ID\", \"$P3_PARTICIPANT_ID\"]}" \
    > /dev/null

# P2 accepts only.
accept_invitation $P2_HTTP "participant-2" "Onboarding"

# Wait for P2 attestor row to be persisted as inprogress.
WAIT=0
while true; do
    WAIT=$((WAIT + 1))
    if [ $WAIT -ge 30 ]; then
        echo "[G6] ERROR: P2 attestor row not persisted in time"
        exit 1
    fi
    P2_STATUS=$(sqlite3 "$P2_DB_FILE" \
        "SELECT status FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Attestor';" 2>/dev/null || echo "")
    if [ "$P2_STATUS" = "inprogress" ]; then
        break
    fi
    sleep 1
done

# P1 cancels before P3 accepts.
echo "[G6] cancelling onboarding on P1 (P3 still has pending invite)"
HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' \
    -X POST "http://localhost:$P1_HTTP/onboarding/cancel")
if [ "$HTTP_CODE" -lt 200 ] || [ "$HTTP_CODE" -ge 300 ]; then
    echo "[G6] ERROR: cancel returned $HTTP_CODE"
    exit 1
fi

# Assert P2 attestor row flips to cancelled.
WAIT=0
while true; do
    WAIT=$((WAIT + 1))
    if [ $WAIT -ge 30 ]; then
        echo "[G6] ERROR: P2 attestor row not flipped to cancelled in time"
        exit 1
    fi
    P2_STATUS=$(sqlite3 "$P2_DB_FILE" \
        "SELECT status FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Attestor';")
    if [ "$P2_STATUS" = "cancelled" ]; then
        break
    fi
    sleep 1
done
echo "[G6] P2 attestor row flipped to cancelled"

# Assert P2 row carries an error message about cancellation.
P2_ERROR=$(sqlite3 "$P2_DB_FILE" \
    "SELECT COALESCE(error,'') FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Attestor';")
if ! echo "$P2_ERROR" | grep -qi "cancel"; then
    echo "[G6] ERROR: P2 cancelled row should mention cancellation, got '$P2_ERROR'"
    exit 1
fi

# Assert P3 has no leftover Onboarding pending invitation.
P3_PENDING=$(curl -s "http://localhost:$P3_HTTP/invitations" | \
    jq '[.invitations[] | select(.invitation_type == "Onboarding")] | length')
if [ "$P3_PENDING" != "0" ]; then
    echo "[G6] ERROR: P3 still has $P3_PENDING pending Onboarding invitations"
    exit 1
fi
echo "[G6] P3 pending invitation removed"

# Cleanup: dismiss both rows so subsequent tests start clean.
curl -s -X POST "http://localhost:$P1_HTTP/workflows/$INSTANCE_NAME/dismiss" \
    -H "Content-Type: application/json" > /dev/null || true
curl -s -X POST "http://localhost:$P2_HTTP/workflows/$INSTANCE_NAME/dismiss" \
    -H "Content-Type: application/json" > /dev/null || true

echo "[G6] cancel cascade verified"
