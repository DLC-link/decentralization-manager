#!/bin/bash

# G9: Restart while two concurrent workflow kinds are in flight resumes both.
#
# Start an Onboarding then immediately start a DARs distribution so both are
# InProgress simultaneously. Defer accept on both kinds so neither workflow
# can advance. Hard-kill P1, restart, accept on both kinds, and assert both
# reach Completed. Stalling is achieved by NOT calling accept_invitation, NOT
# by pausing attestor processes (start handlers pre-flight peer-mesh queries
# via Noise, so attestors must remain responsive).
#
# Sourced by run.sh.

PARTY_PREFIX="concurrent-kinds-$(date +%s)"
ONBOARDING_INSTANCE="$PARTY_PREFIX-creation"
P1_DB_FILE="$DEV_DIR/participant-1/data/decpm.db"

echo "[G9] starting onboarding on P1"
curl -s -X POST "http://localhost:$P1_HTTP/onboarding" \
    -H "Content-Type: application/json" \
    -d "{\"party_id_prefix\": \"$PARTY_PREFIX\", \"peer_ids\": [\"$P2_PARTICIPANT_ID\", \"$P3_PARTICIPANT_ID\"]}" \
    > /dev/null

echo "[G9] starting DARs distribution on P1 (in parallel with onboarding)"

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
curl -s -X POST "http://localhost:$P1_HTTP/dars/distribute" \
    -H "Content-Type: application/json" \
    -d @"$DARS_TMP" > /dev/null

# Wait for both InProgress coordinator rows AND for both kinds of invitations
# to land on attestors. The resume path doesn't re-send invites, so killing
# P1 before the spawned tasks finish their 500ms ListenerPauseGuard +
# send_*_invites sequence leaves attestors with nothing to accept.
WAIT=0
while true; do
    WAIT=$((WAIT + 1))
    if [ $WAIT -ge 60 ]; then
        echo "[G9] ERROR: both InProgress rows + invites not delivered in time"
        exit 1
    fi
    INPROGRESS_KINDS=$(sqlite3 "$P1_DB_FILE" \
        "SELECT COUNT(DISTINCT kind) FROM workflow_runs WHERE role='Coordinator' AND status='inprogress';" 2>/dev/null || echo 0)
    INV_KINDS_P2=$(curl -s "http://localhost:$P2_HTTP/invitations" \
        | jq -r '[.invitations[].invitation_type] | unique | length' 2>/dev/null || echo 0)
    INV_KINDS_P3=$(curl -s "http://localhost:$P3_HTTP/invitations" \
        | jq -r '[.invitations[].invitation_type] | unique | length' 2>/dev/null || echo 0)
    if [ "$INPROGRESS_KINDS" -ge 2 ] && [ "$INV_KINDS_P2" -ge 2 ] && [ "$INV_KINDS_P3" -ge 2 ]; then
        break
    fi
    sleep 1
done

# Hard-kill P1.
P1_PID="${PIDS[0]}"
echo "[G9] hard-killing P1 ($P1_PID)"
kill -9 "$P1_PID"
wait "$P1_PID" 2>/dev/null || true
PIDS=("${PIDS[1]}" "${PIDS[2]}")

# Restart P1.
echo "[G9] restarting P1"
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

# Accept invitations on both kinds.
accept_invitation $P2_HTTP "participant-2" "Onboarding" &
A1=$!
accept_invitation $P3_HTTP "participant-3" "Onboarding" &
A2=$!
accept_invitation $P2_HTTP "participant-2" "Dars" &
A3=$!
accept_invitation $P3_HTTP "participant-3" "Dars" &
A4=$!
wait $A1 $A2 $A3 $A4

# Both must reach Completed. Poll the persisted DB rows directly — after a
# P1 restart, the in-memory `<Kind>WorkflowState` is reset to a fresh
# instance and only catches transitions from the spawned task running in
# that fresh process; the DB row is the durable signal.
poll_workflow_run_status "$P1_DB_FILE" "$ONBOARDING_INSTANCE"
DARS_INSTANCE=$(sqlite3 "$P1_DB_FILE" \
    "SELECT instance_name FROM workflow_runs WHERE kind='Dars' AND role='Coordinator' ORDER BY created_at DESC LIMIT 1;")
poll_workflow_run_status "$P1_DB_FILE" "$DARS_INSTANCE"

# Sanity: each kind has a Completed coordinator row.
ON_COMPLETED=$(sqlite3 "$P1_DB_FILE" \
    "SELECT COUNT(*) FROM workflow_runs WHERE kind='Onboarding' AND role='Coordinator' AND status='completed' AND instance_name='$ONBOARDING_INSTANCE';")
DARS_COMPLETED=$(sqlite3 "$P1_DB_FILE" \
    "SELECT COUNT(*) FROM workflow_runs WHERE kind='Dars' AND role='Coordinator' AND status='completed';")

if [ "$ON_COMPLETED" != "1" ] || [ "$DARS_COMPLETED" -lt 1 ]; then
    echo "[G9] ERROR: completed counts wrong (Onboarding=$ON_COMPLETED, Dars=$DARS_COMPLETED)"
    exit 1
fi

echo "[G9] concurrent-kinds resume verified (Onboarding + Dars completed)"

# Cleanup: dismiss the rows we created.
curl -s -X POST "http://localhost:$P1_HTTP/workflows/$ONBOARDING_INSTANCE/dismiss" \
    -H "Content-Type: application/json" > /dev/null || true
DARS_INSTANCES=$(sqlite3 "$P1_DB_FILE" \
    "SELECT instance_name FROM workflow_runs WHERE kind='Dars' AND role='Coordinator' AND status='completed' AND dismissed=0;" 2>/dev/null || true)
for inst in $DARS_INSTANCES; do
    curl -s -X POST "http://localhost:$P1_HTTP/workflows/$inst/dismiss" \
        -H "Content-Type: application/json" > /dev/null || true
done
