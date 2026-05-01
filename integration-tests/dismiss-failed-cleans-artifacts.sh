#!/bin/bash

# G4: Dismiss of a Failed run cascades artifact cleanup.
#
# Force a failure (kill an attestor before completion → coordinator times out,
# or the partial unique index / abort path fires). Confirm workflow_artifacts
# rows exist for the failed instance. POST /workflows/{instance}/dismiss.
# Assert: artifacts gone; run row stays (with dismissed=1); a fresh run of the
# same kind succeeds (proves the unique index is not blocked).
#
# Sourced by run.sh.

PARTY_PREFIX="dismiss-fail-$(date +%s)"
INSTANCE_NAME="$PARTY_PREFIX-creation"
P1_DB_FILE="$DEV_DIR/participant-1/data/decpm.db"

echo "[G4] starting onboarding on P1 with prefix $PARTY_PREFIX"
curl -s -X POST "http://localhost:$P1_HTTP/onboarding" \
    -H "Content-Type: application/json" \
    -d "{\"party_id_prefix\": \"$PARTY_PREFIX\", \"peer_ids\": [\"$P2_PARTICIPANT_ID\", \"$P3_PARTICIPANT_ID\"]}" \
    > /dev/null

accept_invitation $P2_HTTP "participant-2" "Onboarding" &
ACC1=$!
accept_invitation $P3_HTTP "participant-3" "Onboarding" &
ACC2=$!
wait $ACC1 $ACC2

# Wait for at least one workflow_artifacts row before forcing failure.
WAIT=0
while true; do
    WAIT=$((WAIT + 1))
    if [ $WAIT -ge 60 ]; then
        echo "[G4] ERROR: no artifacts persisted before kill"
        exit 1
    fi
    ARTIFACT_COUNT=$(sqlite3 "$P1_DB_FILE" \
        "SELECT COUNT(*) FROM workflow_artifacts WHERE instance_name='$INSTANCE_NAME';" 2>/dev/null || echo 0)
    if [ "$ARTIFACT_COUNT" -gt 0 ]; then
        break
    fi
    sleep 1
done

# Hard-kill both attestors → coordinator should fail.
P2_PID="${PIDS[1]}"
P3_PID="${PIDS[2]}"
echo "[G4] hard-killing P2/P3 to force coordinator failure"
kill -9 "$P2_PID" "$P3_PID"
wait "$P2_PID" "$P3_PID" 2>/dev/null || true
PIDS=("${PIDS[0]}")

# Wait for Failed.
WAIT=0
while true; do
    WAIT=$((WAIT + 1))
    if [ $WAIT -ge 90 ]; then
        echo "[G4] ERROR: coordinator did not mark Failed within bound"
        exit 1
    fi
    P1_STATUS=$(sqlite3 "$P1_DB_FILE" \
        "SELECT status FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Coordinator';" 2>/dev/null || echo "")
    if [ "$P1_STATUS" = "failed" ]; then
        break
    fi
    sleep 2
done

# Confirm artifacts still exist for the failed run (PR's invariant: failed runs
# keep artifacts until dismiss).
ARTIFACT_COUNT_BEFORE=$(sqlite3 "$P1_DB_FILE" \
    "SELECT COUNT(*) FROM workflow_artifacts WHERE instance_name='$INSTANCE_NAME';")
if [ "$ARTIFACT_COUNT_BEFORE" -lt 1 ]; then
    echo "[G4] ERROR: failed run should keep artifacts, found $ARTIFACT_COUNT_BEFORE"
    exit 1
fi
echo "[G4] $ARTIFACT_COUNT_BEFORE artifact rows present pre-dismiss"

# Dismiss the run.
HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' \
    -X POST "http://localhost:$P1_HTTP/workflows/$INSTANCE_NAME/dismiss" \
    -H "Content-Type: application/json")
if [ "$HTTP_CODE" != "200" ]; then
    echo "[G4] ERROR: dismiss returned $HTTP_CODE (expected 200)"
    exit 1
fi

# Assert: artifacts gone, row stays with dismissed=1.
ARTIFACT_COUNT_AFTER=$(sqlite3 "$P1_DB_FILE" \
    "SELECT COUNT(*) FROM workflow_artifacts WHERE instance_name='$INSTANCE_NAME';")
if [ "$ARTIFACT_COUNT_AFTER" != "0" ]; then
    echo "[G4] ERROR: artifacts not cleaned (got $ARTIFACT_COUNT_AFTER)"
    exit 1
fi

DISMISSED=$(sqlite3 "$P1_DB_FILE" \
    "SELECT dismissed FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Coordinator';")
if [ "$DISMISSED" != "1" ]; then
    echo "[G4] ERROR: row not marked dismissed (got $DISMISSED)"
    exit 1
fi
echo "[G4] artifacts cleaned, run row preserved as dismissed"

# Restart attestors so a fresh start succeeds.
echo "[G4] restarting P2"
RUST_LOG="${RUST_LOG:-dec_party_manager=info,tokio_noise=error,hyper_noise=error}" \
DECPM_CANTON_ADMIN_HOST=127.0.0.1 \
DECPM_CANTON_ADMIN_PORT="$P2_CANTON_ADMIN" \
DECPM_CANTON_LEDGER_HOST=127.0.0.1 \
DECPM_CANTON_LEDGER_PORT="$P2_CANTON_LEDGER" \
DECPM_CANTON_NETWORK=devnet \
DECPM_NOISE_PORT="$P2_NOISE" \
"$BINARY" -d "$DEV_DIR/participant-2" serve \
    --host 0.0.0.0 --port "$P2_HTTP" &
PIDS+=("$!")
wait_for_server $P2_HTTP "participant-2" $P2_NOISE

echo "[G4] restarting P3"
RUST_LOG="${RUST_LOG:-dec_party_manager=info,tokio_noise=error,hyper_noise=error}" \
DECPM_CANTON_ADMIN_HOST=127.0.0.1 \
DECPM_CANTON_ADMIN_PORT="$P3_CANTON_ADMIN" \
DECPM_CANTON_LEDGER_HOST=127.0.0.1 \
DECPM_CANTON_LEDGER_PORT="$P3_CANTON_LEDGER" \
DECPM_CANTON_NETWORK=devnet \
DECPM_NOISE_PORT="$P3_NOISE" \
"$BINARY" -d "$DEV_DIR/participant-3" serve \
    --host 0.0.0.0 --port "$P3_HTTP" &
PIDS+=("$!")
wait_for_server $P3_HTTP "participant-3" $P3_NOISE

# Fresh onboarding of same kind should now succeed (proves unique index released).
NEXT_PREFIX="dismiss-fresh-$(date +%s)"
NEXT_INSTANCE="$NEXT_PREFIX-creation"
echo "[G4] starting fresh onboarding $NEXT_PREFIX to prove (kind, role) slot freed"
HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' \
    -X POST "http://localhost:$P1_HTTP/onboarding" \
    -H "Content-Type: application/json" \
    -d "{\"party_id_prefix\": \"$NEXT_PREFIX\", \"peer_ids\": [\"$P2_PARTICIPANT_ID\", \"$P3_PARTICIPANT_ID\"]}")
if [ "$HTTP_CODE" -lt 200 ] || [ "$HTTP_CODE" -ge 300 ]; then
    echo "[G4] ERROR: fresh onboarding rejected ($HTTP_CODE)"
    exit 1
fi

# Drive it to completion so we don't leave an InProgress hanging.
accept_invitation $P2_HTTP "participant-2" "Onboarding" &
ACC1=$!
accept_invitation $P3_HTTP "participant-3" "Onboarding" &
ACC2=$!
wait $ACC1 $ACC2
poll_status $P1_HTTP "onboarding/status"

echo "[G4] dismiss + fresh-start path verified"

# Cleanup: dismiss the new row too.
curl -s -X POST "http://localhost:$P1_HTTP/workflows/$NEXT_INSTANCE/dismiss" \
    -H "Content-Type: application/json" > /dev/null || true
