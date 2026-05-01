#!/bin/bash

# P2: Cross-node RetryWorkflow is best-effort for unreachable peers.
#
# Force a coordinator-side failure, leave one attestor (P3) offline, then call
# /workflows/{instance}/retry. Assert that /workflows on P1 surfaces the
# asymmetric state honestly (P3 never advances; coordinator does not block
# the dashboard).
#
# Sourced by run.sh.

PARTY_PREFIX="retry-offline-$(date +%s)"
INSTANCE_NAME="$PARTY_PREFIX-creation"
P1_DB_FILE="$DEV_DIR/participant-1/data/decpm.db"

echo "[P2] starting onboarding on P1 with prefix $PARTY_PREFIX"
curl -s -X POST "http://localhost:$P1_HTTP/onboarding" \
    -H "Content-Type: application/json" \
    -d "{\"party_id_prefix\": \"$PARTY_PREFIX\", \"peer_ids\": [\"$P2_PARTICIPANT_ID\", \"$P3_PARTICIPANT_ID\"]}" \
    > /dev/null

accept_invitation $P2_HTTP "participant-2" "Onboarding" &
ACC1=$!
accept_invitation $P3_HTTP "participant-3" "Onboarding" &
ACC2=$!
wait $ACC1 $ACC2

# Wait for both attestor rows to be persisted, then kill both.
sleep 3
P2_PID="${PIDS[1]}"
P3_PID="${PIDS[2]}"
echo "[P2] hard-killing P2 and P3 to force coordinator failure"
kill -9 "$P2_PID" "$P3_PID"
wait "$P2_PID" "$P3_PID" 2>/dev/null || true
PIDS=("${PIDS[0]}")

# Wait for Failed.
WAIT=0
while true; do
    WAIT=$((WAIT + 1))
    if [ $WAIT -ge 90 ]; then
        echo "[P2] ERROR: coordinator did not mark Failed in time"
        # restart so suite isn't poisoned
        for i in 2 3; do
            idx=$((i - 1))
            ports=("$P1_CANTON_ADMIN" "$P2_CANTON_ADMIN" "$P3_CANTON_ADMIN")
            ledger=("$P1_CANTON_LEDGER" "$P2_CANTON_LEDGER" "$P3_CANTON_LEDGER")
            noise=("$P1_NOISE" "$P2_NOISE" "$P3_NOISE")
            http=("$P1_HTTP" "$P2_HTTP" "$P3_HTTP")
            DECPM_CANTON_ADMIN_HOST=127.0.0.1 \
            DECPM_CANTON_ADMIN_PORT="${ports[$idx]}" \
            DECPM_CANTON_LEDGER_HOST=127.0.0.1 \
            DECPM_CANTON_LEDGER_PORT="${ledger[$idx]}" \
            DECPM_CANTON_NETWORK=devnet \
            DECPM_NOISE_PORT="${noise[$idx]}" \
            "$BINARY" -d "$DEV_DIR/participant-$i" serve --host 0.0.0.0 --port "${http[$idx]}" &
            PIDS+=("$!")
        done
        exit 1
    fi
    P1_STATUS=$(sqlite3 "$P1_DB_FILE" \
        "SELECT status FROM workflow_runs WHERE instance_name='$INSTANCE_NAME' AND role='Coordinator';" 2>/dev/null || echo "")
    if [ "$P1_STATUS" = "failed" ]; then
        break
    fi
    sleep 2
done

# Restart ONLY P2; leave P3 offline for the retry.
echo "[P2] restarting P2 only (P3 stays offline)"
RUST_LOG="${RUST_LOG:-dec_party_manager=info,tokio_noise=error,hyper_noise=error}" \
DECPM_CANTON_ADMIN_HOST=127.0.0.1 \
DECPM_CANTON_ADMIN_PORT="$P2_CANTON_ADMIN" \
DECPM_CANTON_LEDGER_HOST=127.0.0.1 \
DECPM_CANTON_LEDGER_PORT="$P2_CANTON_LEDGER" \
DECPM_CANTON_NETWORK=devnet \
DECPM_NOISE_PORT="$P2_NOISE" \
"$BINARY" -d "$DEV_DIR/participant-2" serve --host 0.0.0.0 --port "$P2_HTTP" &
NEW_P2_PID=$!
PIDS+=("$NEW_P2_PID")
wait_for_server $P2_HTTP "participant-2" $P2_NOISE

# POST retry. The coordinator broadcasts RetryWorkflow but P3 is unreachable.
echo "[P2] posting retry to /workflows/$INSTANCE_NAME/retry (with P3 offline)"
HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' \
    -X POST "http://localhost:$P1_HTTP/workflows/$INSTANCE_NAME/retry" \
    -H "Content-Type: application/json")
if [ "$HTTP_CODE" != "200" ]; then
    echo "[P2] ERROR: retry returned $HTTP_CODE (expected 200, even with offline peer)"
    exit 1
fi
echo "[P2] retry POST accepted (HTTP 200) with P3 offline"

# Give the coordinator a few seconds to react, then assert /workflows reflects
# state honestly. The run will not Complete (P3 never accepts); P1 should
# either be inprogress (still trying) or failed again. We require it to show
# up in /workflows for the operator either way.
sleep 5

WF_JSON=$(curl -s "http://localhost:$P1_HTTP/workflows")
PRESENT=$(echo "$WF_JSON" | \
    jq --arg i "$INSTANCE_NAME" '[.runs[] | select(.instance_name == $i)] | length')
if [ "$PRESENT" != "1" ]; then
    echo "[P2] ERROR: /workflows hides the run after retry (got count=$PRESENT)"
    exit 1
fi
echo "[P2] /workflows surfaces the post-retry run for operator inspection"

# Now restart P3 so subsequent tests aren't poisoned and let the coordinator
# settle (it may still be inprogress for some time).
echo "[P2] restarting P3 to unblock subsequent tests"
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

# Try to cancel the in-flight run, then dismiss whatever terminal state it
# settles into so the suite is clean.
curl -s -X POST "http://localhost:$P1_HTTP/onboarding/cancel" > /dev/null || true
sleep 3
curl -s -X POST "http://localhost:$P1_HTTP/workflows/$INSTANCE_NAME/dismiss" \
    -H "Content-Type: application/json" > /dev/null || true

echo "[P2] retry-with-offline-peer verified"
