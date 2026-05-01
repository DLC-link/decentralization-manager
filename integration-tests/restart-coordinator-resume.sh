#!/bin/bash

# G1: Coordinator crash mid-workflow → auto-resume on restart.
#
# Starts an Onboarding on P1 and lets P2/P3 receive the invite WITHOUT
# accepting (so the coordinator stalls at WaitingForAttestors). Hard-kills P1,
# restarts it, then accepts the invitations on P2/P3. Asserts the run reaches
# Completed and exactly one coordinator workflow_runs row exists for the
# instance.
#
# NOTE: the start_onboarding handler does a peer-mesh pre-flight that queries
# P2/P3 over Noise — they MUST be alive when /onboarding is POSTed, so we
# can't pause them up front. Stalling is achieved purely by deferring
# accept_invitation until after the restart.
#
# Sourced by run.sh — expects env.sh to have started 3 nodes with peer config.

PARTY_PREFIX="resume-coord-$(date +%s)"
INSTANCE_NAME="$PARTY_PREFIX-creation"
DB_FILE="$DEV_DIR/participant-1/data/decpm.db"

echo "[G1] starting onboarding on P1 with prefix $PARTY_PREFIX"

P1_PID="${PIDS[0]}"
if [ -z "$P1_PID" ]; then
    echo "[G1] ERROR: missing P1 PID"
    exit 1
fi

curl -s -X POST "http://localhost:$P1_HTTP/onboarding" \
    -H "Content-Type: application/json" \
    -d "{\"party_id_prefix\": \"$PARTY_PREFIX\", \"peer_ids\": [\"$P2_PARTICIPANT_ID\", \"$P3_PARTICIPANT_ID\"]}" \
    > /dev/null

# Wait for the workflow_runs row to be persisted as inprogress AND for the
# Onboarding invite to have actually reached both attestors. The handler
# inserts the row almost immediately (<100ms) but the spawned task only
# sends invites after a 500ms ListenerPauseGuard sleep — killing P1 before
# the invite goes out leaves attestors with no pending invitation, and the
# resume path doesn't re-send (it assumes peers were already invited).
WAIT=0
while true; do
    WAIT=$((WAIT + 1))
    if [ $WAIT -ge 60 ]; then
        echo "[G1] ERROR: workflow_runs row + attestor invitations not ready in time"
        exit 1
    fi
    ROW_COUNT=$(sqlite3 "$DB_FILE" \
        "SELECT COUNT(*) FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Coordinator' AND status='inprogress';" 2>/dev/null || echo 0)
    INV_P2=$(curl -s "http://localhost:$P2_HTTP/invitations" \
        | jq -r '[.invitations[] | select(.invitation_type == "Onboarding")] | length' 2>/dev/null || echo 0)
    INV_P3=$(curl -s "http://localhost:$P3_HTTP/invitations" \
        | jq -r '[.invitations[] | select(.invitation_type == "Onboarding")] | length' 2>/dev/null || echo 0)
    if [ "$ROW_COUNT" = "1" ] && [ "$INV_P2" -ge 1 ] && [ "$INV_P3" -ge 1 ]; then
        break
    fi
    sleep 1
done

echo "[G1] coordinator row persisted + invites delivered; hard-killing P1 (pid $P1_PID)"
kill -9 "$P1_PID"
wait "$P1_PID" 2>/dev/null || true

# Drop P1 from PIDS so cleanup doesn't try to kill it again.
PIDS=("${PIDS[1]}" "${PIDS[2]}")

# Restart P1 with the same data dir.
echo "[G1] restarting P1"
RUST_LOG="${RUST_LOG:-dec_party_manager=info,tokio_noise=error,hyper_noise=error}" \
DECPM_CANTON_ADMIN_HOST=127.0.0.1 \
DECPM_CANTON_ADMIN_PORT="$P1_CANTON_ADMIN" \
DECPM_CANTON_LEDGER_HOST=127.0.0.1 \
DECPM_CANTON_LEDGER_PORT="$P1_CANTON_LEDGER" \
DECPM_CANTON_NETWORK=devnet \
DECPM_NOISE_PORT="$P1_NOISE" \
"$BINARY" -d "$DEV_DIR/participant-1" serve \
    --host 0.0.0.0 --port "$P1_HTTP" &
NEW_P1_PID=$!
PIDS=("$NEW_P1_PID" "${PIDS[@]}")
wait_for_server $P1_HTTP "participant-1" $P1_NOISE

# Now accept invitations on P2 and P3.
accept_invitation $P2_HTTP "participant-2" "Onboarding" &
ACC1=$!
accept_invitation $P3_HTTP "participant-3" "Onboarding" &
ACC2=$!
wait $ACC1 $ACC2

# Poll the persisted workflow_runs row directly. This is the source of truth
# for whether the resumed coordinator task finished — `mark_run_completed`
# flips it to "completed" once the workflow returns Ok. The in-memory
# `/onboarding/status` endpoint reads a freshly-constructed
# `OnboardingWorkflowState` after the restart, which has shown timing
# inconsistencies on slow runners; the DB row is the durable signal we
# actually care about for resume verification.
echo "[G1] waiting for resumed run to reach Completed in workflow_runs..."
WAIT=0
while true; do
    WAIT=$((WAIT + 1))
    if [ $WAIT -ge 120 ]; then
        ACTUAL=$(sqlite3 "$DB_FILE" \
            "SELECT status FROM workflow_runs WHERE instance_name='$INSTANCE_NAME';" 2>/dev/null \
            || echo "?")
        echo "[G1] ERROR: resumed run did not reach Completed (last status: $ACTUAL)"
        exit 1
    fi
    STATUS=$(sqlite3 "$DB_FILE" \
        "SELECT status FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Coordinator';" 2>/dev/null \
        || echo "")
    case "$STATUS" in
        completed)
            break
            ;;
        failed|cancelled)
            ERR=$(sqlite3 "$DB_FILE" \
                "SELECT error FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Coordinator';" \
                2>/dev/null || echo "")
            echo "[G1] ERROR: resumed run reached terminal status $STATUS: $ERR"
            exit 1
            ;;
    esac
    sleep 2
done

# Invariant: exactly one coordinator workflow_runs row for this instance.
ROW_COUNT=$(sqlite3 "$DB_FILE" \
    "SELECT COUNT(*) FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Coordinator';")
if [ "$ROW_COUNT" != "1" ]; then
    echo "[G1] ERROR: expected exactly 1 coordinator row, got $ROW_COUNT"
    exit 1
fi

echo "[G1] coordinator resume verified ($INSTANCE_NAME completed, single row)"

# Cleanup: dismiss the row so subsequent tests start clean.
curl -s -X POST "http://localhost:$P1_HTTP/workflows/$INSTANCE_NAME/dismiss" \
    -H "Content-Type: application/json" > /dev/null || true
