#!/bin/bash

# G2: Attestor crash mid-workflow → auto-resume re-fires trigger.
#
# Starts an Onboarding on P1 with P2+P3 as attestors. Both accept. Then
# hard-kills P2 only, restarts it, and asserts /onboarding/status reaches
# Completed and P2's attestor row is the same one (not a new instance).
#
# Sourced by run.sh.

PARTY_PREFIX="resume-attestor-$(date +%s)"
INSTANCE_NAME="$PARTY_PREFIX-creation"
P2_DB_FILE="$DEV_DIR/participant-2/data/decpm.db"
P1_DB_FILE="$DEV_DIR/participant-1/data/decpm.db"

echo "[G2] starting onboarding on P1 with prefix $PARTY_PREFIX"
curl -s -X POST "http://localhost:$P1_HTTP/onboarding" \
    -H "Content-Type: application/json" \
    -d "{\"party_id_prefix\": \"$PARTY_PREFIX\", \"peer_ids\": [\"$P2_PARTICIPANT_ID\", \"$P3_PARTICIPANT_ID\"]}" \
    > /dev/null

# Both attestors accept up front.
accept_invitation $P2_HTTP "participant-2" "Onboarding" &
ACC1=$!
accept_invitation $P3_HTTP "participant-3" "Onboarding" &
ACC2=$!
wait $ACC1 $ACC2

# Wait for P2's attestor row to be persisted as inprogress.
WAIT=0
while true; do
    WAIT=$((WAIT + 1))
    if [ $WAIT -ge 30 ]; then
        echo "[G2] ERROR: attestor row not persisted in time"
        exit 1
    fi
    ROW_COUNT=$(sqlite3 "$P2_DB_FILE" \
        "SELECT COUNT(*) FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Attestor' AND status='inprogress';" 2>/dev/null || echo 0)
    if [ "$ROW_COUNT" = "1" ]; then
        break
    fi
    sleep 1
done

# Capture P2's row created_at so we can assert it survives the restart.
P2_CREATED_AT=$(sqlite3 "$P2_DB_FILE" \
    "SELECT created_at FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Attestor';")

P2_PID="${PIDS[1]}"
echo "[G2] hard-killing P2 (pid $P2_PID)"
kill -9 "$P2_PID"
wait "$P2_PID" 2>/dev/null || true
PIDS=("${PIDS[0]}" "${PIDS[2]}")

# Restart P2.
echo "[G2] restarting P2"
RUST_LOG="${RUST_LOG:-dec_party_manager=info,tokio_noise=error,hyper_noise=error}" \
DECPM_CANTON_ADMIN_HOST=127.0.0.1 \
DECPM_CANTON_ADMIN_PORT="$P2_CANTON_ADMIN" \
DECPM_CANTON_LEDGER_HOST=127.0.0.1 \
DECPM_CANTON_LEDGER_PORT="$P2_CANTON_LEDGER" \
DECPM_CANTON_NETWORK=devnet \
DECPM_NOISE_PORT="$P2_NOISE" \
"$BINARY" -d "$DEV_DIR/participant-2" serve \
    --host 0.0.0.0 --port "$P2_HTTP" &
NEW_P2_PID=$!
PIDS=("${PIDS[0]}" "$NEW_P2_PID" "${PIDS[1]}")
wait_for_server $P2_HTTP "participant-2" $P2_NOISE

# Coordinator still running on P1, should drive run to completion.
poll_status $P1_HTTP "onboarding/status"

# Invariant: exactly one attestor row for this instance on P2 and same created_at.
ROW_COUNT=$(sqlite3 "$P2_DB_FILE" \
    "SELECT COUNT(*) FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Attestor';")
if [ "$ROW_COUNT" != "1" ]; then
    echo "[G2] ERROR: expected exactly 1 attestor row on P2, got $ROW_COUNT"
    exit 1
fi

NEW_CREATED_AT=$(sqlite3 "$P2_DB_FILE" \
    "SELECT created_at FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Attestor';")
if [ "$NEW_CREATED_AT" != "$P2_CREATED_AT" ]; then
    echo "[G2] ERROR: created_at changed ($P2_CREATED_AT → $NEW_CREATED_AT) — row was not reused"
    exit 1
fi

FINAL_STATUS=$(sqlite3 "$P2_DB_FILE" \
    "SELECT status FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Attestor';")
if [ "$FINAL_STATUS" != "completed" ]; then
    echo "[G2] ERROR: P2 attestor row expected completed, got $FINAL_STATUS"
    exit 1
fi

echo "[G2] attestor resume verified ($INSTANCE_NAME row reused, completed)"

# Cleanup: dismiss the coordinator row so subsequent tests start clean.
curl -s -X POST "http://localhost:$P1_HTTP/workflows/$INSTANCE_NAME/dismiss" \
    -H "Content-Type: application/json" > /dev/null || true
