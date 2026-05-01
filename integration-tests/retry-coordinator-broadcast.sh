#!/bin/bash

# G3: Coordinator-initiated retry of a Failed run flips attestor rows back.
#
# Force a coordinator-side failure by killing both attestor processes after
# they accept the onboarding invite — coordinator will time out at SubmitDns /
# SubmitFinal and the run goes Failed. Restart attestors. POST
# /workflows/{instance}/retry on P1. Assert: P1 row Failed→InProgress, P2/P3
# attestor rows that had been Failed flip back to InProgress, run completes.
#
# Sourced by run.sh.

PARTY_PREFIX="retry-coord-$(date +%s)"
INSTANCE_NAME="$PARTY_PREFIX-creation"
P1_DB_FILE="$DEV_DIR/participant-1/data/decpm.db"
P2_DB_FILE="$DEV_DIR/participant-2/data/decpm.db"
P3_DB_FILE="$DEV_DIR/participant-3/data/decpm.db"

echo "[G3] starting onboarding on P1 with prefix $PARTY_PREFIX"
curl -s -X POST "http://localhost:$P1_HTTP/onboarding" \
    -H "Content-Type: application/json" \
    -d "{\"party_id_prefix\": \"$PARTY_PREFIX\", \"peer_ids\": [\"$P2_PARTICIPANT_ID\", \"$P3_PARTICIPANT_ID\"]}" \
    > /dev/null

accept_invitation $P2_HTTP "participant-2" "Onboarding" &
ACC1=$!
accept_invitation $P3_HTTP "participant-3" "Onboarding" &
ACC2=$!
wait $ACC1 $ACC2

# Wait for both attestor rows persisted.
for db in "$P2_DB_FILE" "$P3_DB_FILE"; do
    WAIT=0
    while true; do
        WAIT=$((WAIT + 1))
        if [ $WAIT -ge 30 ]; then
            echo "[G3] ERROR: attestor row not persisted in $db"
            exit 1
        fi
        ROW_COUNT=$(sqlite3 "$db" \
            "SELECT COUNT(*) FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Attestor' AND status='inprogress';" 2>/dev/null || echo 0)
        if [ "$ROW_COUNT" = "1" ]; then
            break
        fi
        sleep 1
    done
done

# Hard-kill BOTH attestor processes — coordinator should eventually fail.
P2_PID="${PIDS[1]}"
P3_PID="${PIDS[2]}"
echo "[G3] hard-killing P2 ($P2_PID) and P3 ($P3_PID) so coordinator times out"
kill -9 "$P2_PID" "$P3_PID"
wait "$P2_PID" "$P3_PID" 2>/dev/null || true
PIDS=("${PIDS[0]}")

# Wait until coordinator marks the run Failed (or reasonable bound).
WAIT=0
while true; do
    WAIT=$((WAIT + 1))
    if [ $WAIT -ge 90 ]; then
        echo "[G3] ERROR: coordinator did not mark Failed within bound"
        exit 1
    fi
    P1_STATUS=$(sqlite3 "$P1_DB_FILE" \
        "SELECT status FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Coordinator';" 2>/dev/null || echo "")
    if [ "$P1_STATUS" = "failed" ]; then
        break
    fi
    sleep 2
done
echo "[G3] coordinator row marked Failed"

# Restart attestors so they're reachable for retry.
echo "[G3] restarting P2"
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
PIDS+=("$NEW_P2_PID")
wait_for_server $P2_HTTP "participant-2" $P2_NOISE

echo "[G3] restarting P3"
RUST_LOG="${RUST_LOG:-dec_party_manager=info,tokio_noise=error,hyper_noise=error}" \
DECPM_CANTON_ADMIN_HOST=127.0.0.1 \
DECPM_CANTON_ADMIN_PORT="$P3_CANTON_ADMIN" \
DECPM_CANTON_LEDGER_HOST=127.0.0.1 \
DECPM_CANTON_LEDGER_PORT="$P3_CANTON_LEDGER" \
DECPM_CANTON_NETWORK=devnet \
DECPM_NOISE_PORT="$P3_NOISE" \
"$BINARY" -d "$DEV_DIR/participant-3" serve \
    --host 0.0.0.0 --port "$P3_HTTP" &
NEW_P3_PID=$!
PIDS+=("$NEW_P3_PID")
wait_for_server $P3_HTTP "participant-3" $P3_NOISE

# Wait for attestors to mark their rows Failed (auto-resume sees coordinator
# unreachable / cancel / etc; bounded). They may already be Failed if their own
# step bombed during the kill — either way we just want them off InProgress.
sleep 5

# POST retry on P1.
echo "[G3] POSTing retry to /workflows/$INSTANCE_NAME/retry"
HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' \
    -X POST "http://localhost:$P1_HTTP/workflows/$INSTANCE_NAME/retry" \
    -H "Content-Type: application/json")
if [ "$HTTP_CODE" != "200" ]; then
    echo "[G3] ERROR: retry POST returned $HTTP_CODE (expected 200)"
    exit 1
fi

# Assert: P1 row goes Failed→InProgress (then completes).
WAIT=0
while true; do
    WAIT=$((WAIT + 1))
    if [ $WAIT -ge 10 ]; then
        echo "[G3] ERROR: P1 row did not flip to inprogress after retry"
        exit 1
    fi
    P1_STATUS=$(sqlite3 "$P1_DB_FILE" \
        "SELECT status FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Coordinator';")
    if [ "$P1_STATUS" = "inprogress" ] || [ "$P1_STATUS" = "completed" ]; then
        break
    fi
    sleep 1
done

# Poll the persisted row, not /onboarding/status. The retry handler swaps
# in a fresh respawn task; in-memory <Kind>WorkflowState transitions can
# lag behind the DB on slow runners.
poll_workflow_run_status "$P1_DB_FILE" "$INSTANCE_NAME"

# Final assertions: all three rows Completed.
P1_FINAL=$(sqlite3 "$P1_DB_FILE" \
    "SELECT status FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Coordinator';")
P2_FINAL=$(sqlite3 "$P2_DB_FILE" \
    "SELECT status FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Attestor';")
P3_FINAL=$(sqlite3 "$P3_DB_FILE" \
    "SELECT status FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Attestor';")

if [ "$P1_FINAL" != "completed" ] || [ "$P2_FINAL" != "completed" ] || [ "$P3_FINAL" != "completed" ]; then
    echo "[G3] ERROR: rows not all completed (P1=$P1_FINAL, P2=$P2_FINAL, P3=$P3_FINAL)"
    exit 1
fi

echo "[G3] retry-broadcast verified (all three rows completed)"

# Cleanup: dismiss the coordinator row.
curl -s -X POST "http://localhost:$P1_HTTP/workflows/$INSTANCE_NAME/dismiss" \
    -H "Content-Type: application/json" > /dev/null || true
