#!/bin/bash

# P1: Attestor signature mismatch surfaces as Failed (not stuck).
#
# Force a coordinator-side failure that the resume path cannot indefinitely
# loop on: kill both attestors after they've accepted, leave them dead, and
# assert the coordinator row reaches Failed within a bounded time
# (90 seconds) and that /workflows still surfaces it for operator inspection.
#
# Sourced by run.sh.

PARTY_PREFIX="bounded-fail-$(date +%s)"
INSTANCE_NAME="$PARTY_PREFIX-creation"
P1_DB_FILE="$DEV_DIR/participant-1/data/decpm.db"

echo "[P1] starting onboarding on P1 with prefix $PARTY_PREFIX"
curl -s -X POST "http://localhost:$P1_HTTP/onboarding" \
    -H "Content-Type: application/json" \
    -d "{\"party_id_prefix\": \"$PARTY_PREFIX\", \"peer_ids\": [\"$P2_PARTICIPANT_ID\", \"$P3_PARTICIPANT_ID\"]}" \
    > /dev/null

accept_invitation $P2_HTTP "participant-2" "Onboarding" &
ACC1=$!
accept_invitation $P3_HTTP "participant-3" "Onboarding" &
ACC2=$!
wait $ACC1 $ACC2

# Hard-kill both attestors and DO NOT restart them — coordinator must give up.
P2_PID="${PIDS[1]}"
P3_PID="${PIDS[2]}"
echo "[P1] hard-killing P2 ($P2_PID) and P3 ($P3_PID); leaving them dead"
kill -9 "$P2_PID" "$P3_PID"
wait "$P2_PID" "$P3_PID" 2>/dev/null || true
PIDS=("${PIDS[0]}")

START=$(date +%s)
DEADLINE=120

# Bounded wait for failure.
while true; do
    NOW=$(date +%s)
    ELAPSED=$((NOW - START))
    if [ $ELAPSED -ge $DEADLINE ]; then
        echo "[P1] ERROR: coordinator did not mark Failed within ${DEADLINE}s"
        # Still try to clean up before exiting.
        break
    fi

    P1_STATUS=$(sqlite3 "$P1_DB_FILE" \
        "SELECT status FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Coordinator';" 2>/dev/null || echo "")
    if [ "$P1_STATUS" = "failed" ]; then
        echo "[P1] coordinator row marked Failed after ${ELAPSED}s"
        break
    fi
    sleep 2
done

if [ "$P1_STATUS" != "failed" ]; then
    # restart attestors so subsequent tests can run, then exit 1.
    echo "[P1] restarting attestors before exiting"
    RUST_LOG="${RUST_LOG:-dec_party_manager=info,tokio_noise=error,hyper_noise=error}" \
    DECPM_CANTON_ADMIN_HOST=127.0.0.1 \
    DECPM_CANTON_ADMIN_PORT="$P2_CANTON_ADMIN" \
    DECPM_CANTON_LEDGER_HOST=127.0.0.1 \
    DECPM_CANTON_LEDGER_PORT="$P2_CANTON_LEDGER" \
    DECPM_CANTON_NETWORK=devnet \
    DECPM_NOISE_PORT="$P2_NOISE" \
    "$BINARY" -d "$DEV_DIR/participant-2" serve --host 0.0.0.0 --port "$P2_HTTP" &
    PIDS+=("$!")
    RUST_LOG="${RUST_LOG:-dec_party_manager=info,tokio_noise=error,hyper_noise=error}" \
    DECPM_CANTON_ADMIN_HOST=127.0.0.1 \
    DECPM_CANTON_ADMIN_PORT="$P3_CANTON_ADMIN" \
    DECPM_CANTON_LEDGER_HOST=127.0.0.1 \
    DECPM_CANTON_LEDGER_PORT="$P3_CANTON_LEDGER" \
    DECPM_CANTON_NETWORK=devnet \
    DECPM_NOISE_PORT="$P3_NOISE" \
    "$BINARY" -d "$DEV_DIR/participant-3" serve --host 0.0.0.0 --port "$P3_HTTP" &
    PIDS+=("$!")
    exit 1
fi

# Confirm /workflows still surfaces the failed run for the operator.
WORKFLOWS_JSON=$(curl -s "http://localhost:$P1_HTTP/workflows")
SURFACED=$(echo "$WORKFLOWS_JSON" | \
    jq --arg i "$INSTANCE_NAME" '[.runs[] | select(.instance_name == $i and .status == "failed")] | length')
if [ "$SURFACED" != "1" ]; then
    echo "[P1] ERROR: /workflows did not surface the failed run (got $SURFACED)"
    exit 1
fi
echo "[P1] /workflows surfaces the failed run"

# Restart attestors so subsequent tests aren't poisoned.
echo "[P1] restarting P2 and P3"
RUST_LOG="${RUST_LOG:-dec_party_manager=info,tokio_noise=error,hyper_noise=error}" \
DECPM_CANTON_ADMIN_HOST=127.0.0.1 \
DECPM_CANTON_ADMIN_PORT="$P2_CANTON_ADMIN" \
DECPM_CANTON_LEDGER_HOST=127.0.0.1 \
DECPM_CANTON_LEDGER_PORT="$P2_CANTON_LEDGER" \
DECPM_CANTON_NETWORK=devnet \
DECPM_NOISE_PORT="$P2_NOISE" \
"$BINARY" -d "$DEV_DIR/participant-2" serve --host 0.0.0.0 --port "$P2_HTTP" &
PIDS+=("$!")
wait_for_server $P2_HTTP "participant-2" $P2_NOISE

RUST_LOG="${RUST_LOG:-dec_party_manager=info,tokio_noise=error,hyper_noise=error}" \
DECPM_CANTON_ADMIN_HOST=127.0.0.1 \
DECPM_CANTON_ADMIN_PORT="$P3_CANTON_ADMIN" \
DECPM_CANTON_LEDGER_HOST=127.0.0.1 \
DECPM_CANTON_LEDGER_PORT="$P3_CANTON_LEDGER" \
DECPM_CANTON_NETWORK=devnet \
DECPM_NOISE_PORT="$P3_NOISE" \
"$BINARY" -d "$DEV_DIR/participant-3" serve --host 0.0.0.0 --port "$P3_HTTP" &
PIDS+=("$!")
wait_for_server $P3_HTTP "participant-3" $P3_NOISE

# Cleanup: dismiss the failed coordinator row.
curl -s -X POST "http://localhost:$P1_HTTP/workflows/$INSTANCE_NAME/dismiss" \
    -H "Content-Type: application/json" > /dev/null || true

echo "[P1] failed-step bounded-time verified"
