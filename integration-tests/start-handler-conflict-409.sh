#!/bin/bash

# G10: Start handler rejects a second InProgress run of same (kind, role).
#
# Start an onboarding without accepting on either attestor — the coordinator
# sits at WaitingForAttestors. POST another /onboarding immediately. Assert
# HTTP 409. Same shape for /dars/distribute. Stalling is achieved by deferring
# accept_invitation, NOT by pausing attestor processes (the start handlers
# pre-flight peer-mesh queries via Noise, so attestors must remain
# responsive).
#
# Sourced by run.sh.

PARTY_PREFIX="conflict-409-$(date +%s)"
P1_DB_FILE="$DEV_DIR/participant-1/data/decpm.db"

echo "[G10] starting first onboarding"
HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' \
    -X POST "http://localhost:$P1_HTTP/onboarding" \
    -H "Content-Type: application/json" \
    -d "{\"party_id_prefix\": \"$PARTY_PREFIX\", \"peer_ids\": [\"$P2_PARTICIPANT_ID\", \"$P3_PARTICIPANT_ID\"]}")
if [ "$HTTP_CODE" -lt 200 ] || [ "$HTTP_CODE" -ge 300 ]; then
    echo "[G10] ERROR: first onboarding rejected ($HTTP_CODE)"
    exit 1
fi

# Wait for the first onboarding's row.
WAIT=0
while true; do
    WAIT=$((WAIT + 1))
    if [ $WAIT -ge 30 ]; then
        echo "[G10] ERROR: first onboarding row not persisted in time"
        exit 1
    fi
    R=$(sqlite3 "$P1_DB_FILE" \
        "SELECT COUNT(*) FROM workflow_runs WHERE kind='Onboarding' AND role='Coordinator' AND status='inprogress';" 2>/dev/null || echo 0)
    if [ "$R" -ge 1 ]; then
        break
    fi
    sleep 1
done

# Second onboarding should be rejected with 409.
echo "[G10] starting second onboarding (expect 409)"
HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' \
    -X POST "http://localhost:$P1_HTTP/onboarding" \
    -H "Content-Type: application/json" \
    -d "{\"party_id_prefix\": \"$PARTY_PREFIX-second\", \"peer_ids\": [\"$P2_PARTICIPANT_ID\", \"$P3_PARTICIPANT_ID\"]}")
if [ "$HTTP_CODE" != "409" ]; then
    echo "[G10] ERROR: expected 409 for duplicate /onboarding, got $HTTP_CODE"
    exit 1
fi
echo "[G10] /onboarding duplicate correctly rejected (409)"

# /dars/distribute duplicate. Start one and immediately try a second — the
# first cannot complete because P2/P3 will not accept the invite during this
# test.
DAR1_B64=$(base64 -i "$DARS_DIR/governance-core-v0-rc3-0.1.0.dar")
DARS_TMP=$(mktemp)
TEMP_FILES+=("$DARS_TMP")
cat > "$DARS_TMP" <<EOF
{
  "dar_files": [
    {"filename": "governance-core-v0-rc3-0.1.0.dar", "data": "$DAR1_B64"}
  ],
  "peer_ids": ["$P2_PARTICIPANT_ID", "$P3_PARTICIPANT_ID"]
}
EOF
echo "[G10] starting first DARs distribute"
curl -s -X POST "http://localhost:$P1_HTTP/dars/distribute" \
    -H "Content-Type: application/json" \
    -d @"$DARS_TMP" > /dev/null

# Wait for the Dars row.
WAIT=0
while true; do
    WAIT=$((WAIT + 1))
    if [ $WAIT -ge 30 ]; then
        echo "[G10] ERROR: first DARs row not persisted in time"
        exit 1
    fi
    R=$(sqlite3 "$P1_DB_FILE" \
        "SELECT COUNT(*) FROM workflow_runs WHERE kind='Dars' AND role='Coordinator' AND status='inprogress';" 2>/dev/null || echo 0)
    if [ "$R" -ge 1 ]; then
        break
    fi
    sleep 1
done

echo "[G10] starting second DARs distribute (expect 409)"
HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' \
    -X POST "http://localhost:$P1_HTTP/dars/distribute" \
    -H "Content-Type: application/json" \
    -d @"$DARS_TMP")
if [ "$HTTP_CODE" != "409" ]; then
    echo "[G10] ERROR: expected 409 for duplicate /dars/distribute, got $HTTP_CODE"
    exit 1
fi
echo "[G10] /dars/distribute duplicate correctly rejected (409)"

# Cancel the in-flight workflows so the run.sh suite isn't poisoned.
curl -s -X POST "http://localhost:$P1_HTTP/onboarding/cancel" > /dev/null || true
curl -s -X POST "http://localhost:$P1_HTTP/dars/cancel" > /dev/null || true

# Decline the pending invitations so attestor pending-invitation list resets.
for port in $P2_HTTP $P3_HTTP; do
    INVS=$(curl -s "http://localhost:$port/invitations" \
        | jq -r '.invitations[] | .id' 2>/dev/null || true)
    for inv in $INVS; do
        curl -s -X POST "http://localhost:$port/invitations/decline" \
            -H "Content-Type: application/json" \
            -d "{\"id\": \"$inv\"}" > /dev/null || true
    done
done

# Wait for the workflows to actually settle into a terminal state.
sleep 3

# Dismiss any leftover rows of the kinds we touched.
LEFTOVER_INSTANCES=$(sqlite3 "$P1_DB_FILE" \
    "SELECT instance_name FROM workflow_runs WHERE kind IN ('Onboarding','Dars') AND role='Coordinator' AND status IN ('cancelled','failed') AND dismissed=0;" 2>/dev/null || true)
for inst in $LEFTOVER_INSTANCES; do
    curl -s -X POST "http://localhost:$P1_HTTP/workflows/$inst/dismiss" \
        -H "Content-Type: application/json" > /dev/null || true
done

echo "[G10] start-handler conflict (409) verified"
