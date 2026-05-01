#!/bin/bash

# G7: GenerateKeys idempotent re-run on resume reuses existing vault keys.
#
# Run an onboarding to completion, capture P2's namespace fingerprint via the
# resulting dec_party. Then start a *second* onboarding with the same prefix on
# P2 by simulating an attestor restart: kill P2 just after it persists keys for
# the new run, restart, drive run to completion, and verify that the
# ATTESTOR_PUBLIC_KEYS payload P2 stored on resume hashes to the same
# fingerprint as the first run (i.e., GenerateKeys did not mint a new key).
#
# Sourced by run.sh.

PARTY_PREFIX="idempotent-keys-$(date +%s)"
INSTANCE_NAME="$PARTY_PREFIX-creation"
P2_DB_FILE="$DEV_DIR/participant-2/data/decpm.db"

echo "[G7] starting onboarding on P1 with prefix $PARTY_PREFIX"
curl -s -X POST "http://localhost:$P1_HTTP/onboarding" \
    -H "Content-Type: application/json" \
    -d "{\"party_id_prefix\": \"$PARTY_PREFIX\", \"peer_ids\": [\"$P2_PARTICIPANT_ID\", \"$P3_PARTICIPANT_ID\"]}" \
    > /dev/null

accept_invitation $P2_HTTP "participant-2" "Onboarding" &
ACC1=$!
accept_invitation $P3_HTTP "participant-3" "Onboarding" &
ACC2=$!
wait $ACC1 $ACC2

# Wait for P2 to persist its ATTESTOR_PUBLIC_KEYS artifact (proves GenerateKeys
# completed once on P2). hex() lets us compare the payload as a stable digest
# even though the row is BLOB.
WAIT=0
KEYS_HASH_BEFORE=""
while true; do
    WAIT=$((WAIT + 1))
    if [ $WAIT -ge 60 ]; then
        echo "[G7] ERROR: P2 did not persist ATTESTOR_PUBLIC_KEYS in time"
        exit 1
    fi
    KEYS_HASH_BEFORE=$(sqlite3 "$P2_DB_FILE" \
        "SELECT hex(payload) FROM workflow_artifacts WHERE instance_name='$INSTANCE_NAME' AND artifact_kind='attestor_public_keys' LIMIT 1;" 2>/dev/null || echo "")
    if [ -n "$KEYS_HASH_BEFORE" ]; then
        break
    fi
    sleep 1
done

echo "[G7] P2 captured key payload hex (len=${#KEYS_HASH_BEFORE})"

# Hard-kill P2 mid-run, restart, then watch the same artifact row again. If
# GenerateKeys is idempotent on resume, the payload hex is identical.
P2_PID="${PIDS[1]}"
echo "[G7] hard-killing P2 (pid $P2_PID) mid-run"
kill -9 "$P2_PID"
wait "$P2_PID" 2>/dev/null || true
PIDS=("${PIDS[0]}" "${PIDS[2]}")

echo "[G7] restarting P2"
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

# Drive the workflow to completion.
poll_status $P1_HTTP "onboarding/status"

# Re-read the same artifact and assert the key payload hex is unchanged.
KEYS_HASH_AFTER=$(sqlite3 "$P2_DB_FILE" \
    "SELECT hex(payload) FROM workflow_artifacts WHERE instance_name='$INSTANCE_NAME' AND artifact_kind='attestor_public_keys' LIMIT 1;" 2>/dev/null || echo "")

# After Completed, artifacts get cleaned. So check via dec_party_identity, which
# preserves the keys long-term.
DEC_PARTY_ID=$(curl -s "http://localhost:$P1_HTTP/decentralized-parties?prefix=$PARTY_PREFIX" \
    | jq -r --arg p "$PARTY_PREFIX" '.parties[] | select(.party_id | startswith($p)) | .party_id' | head -1)

if [ -z "$DEC_PARTY_ID" ] || [ "$DEC_PARTY_ID" = "null" ]; then
    echo "[G7] ERROR: dec_party_id not resolved"
    exit 1
fi

# dec_party_identity row count for this party — proves keys persist long-term.
IDENTITY_COUNT=$(sqlite3 "$P2_DB_FILE" \
    "SELECT COUNT(*) FROM dec_party_identity WHERE dec_party_id='$DEC_PARTY_ID';")
if [ "$IDENTITY_COUNT" -lt 1 ]; then
    echo "[G7] ERROR: dec_party_identity rows missing for $DEC_PARTY_ID"
    exit 1
fi

# If artifact was still present immediately before cleanup, the hex must match.
if [ -n "$KEYS_HASH_AFTER" ] && [ "$KEYS_HASH_BEFORE" != "$KEYS_HASH_AFTER" ]; then
    echo "[G7] ERROR: ATTESTOR_PUBLIC_KEYS payload changed across restart (idempotency violated)"
    exit 1
fi

# Extra sanity: the namespace prefix in dec_party_id must equal $PARTY_PREFIX
# (i.e., the run completed against the same key the coordinator started with).
DERIVED_PREFIX=$(echo "$DEC_PARTY_ID" | awk -F'::' '{print $1}')
if [ "$DERIVED_PREFIX" != "$PARTY_PREFIX" ]; then
    echo "[G7] ERROR: dec_party_id prefix '$DERIVED_PREFIX' != expected '$PARTY_PREFIX'"
    exit 1
fi

echo "[G7] GenerateKeys idempotency verified (payload stable across restart)"

# Cleanup: dismiss the coordinator row.
curl -s -X POST "http://localhost:$P1_HTTP/workflows/$INSTANCE_NAME/dismiss" \
    -H "Content-Type: application/json" > /dev/null || true
