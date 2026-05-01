#!/bin/bash

# Notification feed: assert /workflows and /governance/confirmations expose
# the right state to every node so the frontend feed renders correctly.
#
# Three sections:
#   1. /workflows JSON shape + role/coordinator_name resolution per node
#   2. dismiss filters the row from the feed (but DB row stays as dismissed)
#   3. /governance/confirmations is consistent across all 3 nodes for a
#      freshly-proposed action
#
# Sourced by run.sh after governance-generic-vote.sh and BEFORE kick.sh
# (governance assertions need P3 to still be a member of the dec_party).
# Expects PARTY_ID + RULES_CONTRACT_ID + P{1,2,3}_MEMBER_PARTY from the
# earlier gov tests to still be in scope.

if [ -z "$PARTY_ID" ] || [ -z "$RULES_CONTRACT_ID" ]; then
    echo "ERROR: PARTY_ID / RULES_CONTRACT_ID not set (deploy-gov-core.sh must run first)"
    exit 1
fi

# ============================================================================
# Section 1: /workflows JSON shape + role/coordinator_name on every node
# ============================================================================

assert_run_shape() {
    local label=$1
    local json=$2
    local missing
    missing=$(echo "$json" | jq -r '
        [
            "instance_name", "kind", "role", "status", "current_step",
            "step_index", "step_total", "expected_attestors",
            "completed_attestors", "dismissed", "created_at", "updated_at"
        ] - (. | keys)
        | .[]
    ')
    if [ -n "$missing" ]; then
        echo "[feed] ERROR: $label missing keys: $missing"
        exit 1
    fi
}

echo "[feed] verifying /workflows shape on every node"
for port in $P1_HTTP $P2_HTTP $P3_HTTP; do
    feed=$(curl -s "http://localhost:$port/workflows")
    if ! echo "$feed" | jq -e '.runs | type == "array"' > /dev/null; then
        echo "[feed] ERROR: port $port /workflows did not return a runs array"
        echo "$feed"
        exit 1
    fi
    count=$(echo "$feed" | jq '.runs | length')
    if [ "$count" -lt 1 ]; then
        echo "[feed] ERROR: port $port /workflows is empty (expected onboarding completed row)"
        exit 1
    fi
    # Validate the shape of the first run.
    first=$(echo "$feed" | jq '.runs[0]')
    assert_run_shape "port $port .runs[0]" "$first"
done

# Coordinator-side row on P1 should be Onboarding/Coordinator with both
# attestors in expected_attestors.
P1_ONBOARDING=$(curl -s "http://localhost:$P1_HTTP/workflows" \
    | jq -r '.runs[] | select(.kind == "Onboarding" and .role == "Coordinator")')
if [ -z "$P1_ONBOARDING" ]; then
    echo "[feed] ERROR: P1 has no Onboarding/Coordinator row in /workflows"
    exit 1
fi
EXPECTED_COUNT=$(echo "$P1_ONBOARDING" | jq '.expected_attestors | length')
if [ "$EXPECTED_COUNT" != "2" ]; then
    echo "[feed] ERROR: expected 2 attestors on P1's Onboarding row, got $EXPECTED_COUNT"
    exit 1
fi
P1_ONBOARDING_INSTANCE=$(echo "$P1_ONBOARDING" | jq -r '.instance_name')
echo "[feed] P1 Onboarding/Coordinator row: $P1_ONBOARDING_INSTANCE"

# Attestor-side row on P2 should resolve coordinator_pubkey + coordinator_name.
P2_ONBOARDING=$(curl -s "http://localhost:$P2_HTTP/workflows" \
    | jq -r '.runs[] | select(.kind == "Onboarding" and .role == "Attestor")')
if [ -z "$P2_ONBOARDING" ]; then
    echo "[feed] ERROR: P2 has no Onboarding/Attestor row in /workflows"
    exit 1
fi
P2_COORD_PUBKEY=$(echo "$P2_ONBOARDING" | jq -r '.coordinator_pubkey // empty')
P2_COORD_NAME=$(echo "$P2_ONBOARDING" | jq -r '.coordinator_name // empty')
if [ "$P2_COORD_PUBKEY" != "$P1_KEY" ]; then
    echo "[feed] ERROR: P2 coordinator_pubkey mismatch (got $P2_COORD_PUBKEY, want $P1_KEY)"
    exit 1
fi
if [ "$P2_COORD_NAME" != "Participant 1" ]; then
    echo "[feed] ERROR: P2 coordinator_name not resolved (got '$P2_COORD_NAME', want 'Participant 1')"
    exit 1
fi
echo "[feed] P2 Onboarding/Attestor row resolves coordinator_name='$P2_COORD_NAME'"

# ============================================================================
# Section 2: dismiss filters the row out of /workflows
# ============================================================================

echo "[feed] picking a Completed row on P1 to dismiss"
DISMISS_TARGET=$(curl -s "http://localhost:$P1_HTTP/workflows" \
    | jq -r '.runs[] | select(.status == "completed" and .dismissed == false) | .instance_name' \
    | head -1)
if [ -z "$DISMISS_TARGET" ]; then
    echo "[feed] ERROR: no completed+undismissed row on P1 to dismiss"
    exit 1
fi

BEFORE_COUNT=$(curl -s "http://localhost:$P1_HTTP/workflows" | jq '.runs | length')

echo "[feed] dismissing $DISMISS_TARGET"
HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' \
    -X POST "http://localhost:$P1_HTTP/workflows/$DISMISS_TARGET/dismiss" \
    -H "Content-Type: application/json")
if [ "$HTTP_CODE" != "200" ]; then
    echo "[feed] ERROR: dismiss returned $HTTP_CODE"
    exit 1
fi

AFTER_COUNT=$(curl -s "http://localhost:$P1_HTTP/workflows" | jq '.runs | length')
if [ "$AFTER_COUNT" -ge "$BEFORE_COUNT" ]; then
    echo "[feed] ERROR: feed count did not decrease (before=$BEFORE_COUNT, after=$AFTER_COUNT)"
    exit 1
fi

# Row must still exist in the DB but with dismissed=1.
P1_DB="$DEV_DIR/participant-1/data/decpm.db"
DB_DISMISSED=$(sqlite3 "$P1_DB" \
    "SELECT dismissed FROM workflow_runs WHERE instance_name='$DISMISS_TARGET';")
if [ "$DB_DISMISSED" != "1" ]; then
    echo "[feed] ERROR: row dismissed=$DB_DISMISSED in DB (expected 1)"
    exit 1
fi
echo "[feed] dismissed row vanished from /workflows but kept in DB (dismissed=1)"

# Confirm GET on the same instance via /workflows is no longer present.
PRESENT=$(curl -s "http://localhost:$P1_HTTP/workflows" \
    | jq -r --arg n "$DISMISS_TARGET" '[.runs[] | select(.instance_name == $n)] | length')
if [ "$PRESENT" != "0" ]; then
    echo "[feed] ERROR: dismissed row still surfaced by /workflows"
    exit 1
fi

# ============================================================================
# Section 3: /governance/confirmations consistent across all 3 nodes
# ============================================================================

echo "[feed] proposing a fresh action on P1 to verify multi-node visibility"
PROPOSE_BODY=$(cat <<EOF
{
  "party_id": "$PARTY_ID",
  "rules_contract_id": "$RULES_CONTRACT_ID",
  "proposal": {
    "type": "setup_cc_preapproval",
    "provider": "$P1_MEMBER_PARTY",
    "expected_dso": "$P1_MEMBER_PARTY"
  }
}
EOF
)
PROPOSE_RESP=$(curl -s -X POST "http://localhost:$P1_HTTP/governance/propose" \
    -H "Content-Type: application/json" \
    -d "$PROPOSE_BODY")
PROPOSE_ERR=$(echo "$PROPOSE_RESP" | jq -r '.error // empty')
if [ -n "$PROPOSE_ERR" ]; then
    echo "[feed] ERROR: propose failed: $PROPOSE_ERR"
    exit 1
fi

# Give Canton time to propagate to all 3 nodes.
sleep 3

# Pull the new proposal_cid from P1 (the proposer sees it first).
NEW_PROPOSAL=$(curl -s "http://localhost:$P1_HTTP/governance/confirmations?party_id=$PARTY_ID" \
    | jq -r '.domain_actions[] | select(.proposal_cid != null) | .proposal_cid' \
    | tail -1)
if [ -z "$NEW_PROPOSAL" ]; then
    echo "[feed] ERROR: P1 doesn't see the new proposal"
    exit 1
fi
echo "[feed] new proposal_cid: $NEW_PROPOSAL"

# Each node must see this same proposal_cid in its confirmations response.
for label in P1 P2 P3; do
    case "$label" in
        P1) port=$P1_HTTP ;;
        P2) port=$P2_HTTP ;;
        P3) port=$P3_HTTP ;;
    esac
    visible=$(curl -s "http://localhost:$port/governance/confirmations?party_id=$PARTY_ID" \
        | jq -r --arg cid "$NEW_PROPOSAL" \
            '[.domain_actions[] | select(.proposal_cid == $cid)] | length')
    if [ "$visible" != "1" ]; then
        echo "[feed] ERROR: $label does not surface proposal_cid $NEW_PROPOSAL (got $visible)"
        exit 1
    fi
done
echo "[feed] all 3 nodes see the new proposal in /governance/confirmations"

# Threshold must agree across nodes.
P1_THRESHOLD=$(curl -s "http://localhost:$P1_HTTP/governance/confirmations?party_id=$PARTY_ID" \
    | jq -r '.threshold')
P2_THRESHOLD=$(curl -s "http://localhost:$P2_HTTP/governance/confirmations?party_id=$PARTY_ID" \
    | jq -r '.threshold')
P3_THRESHOLD=$(curl -s "http://localhost:$P3_HTTP/governance/confirmations?party_id=$PARTY_ID" \
    | jq -r '.threshold')
if [ "$P1_THRESHOLD" != "$P2_THRESHOLD" ] || [ "$P2_THRESHOLD" != "$P3_THRESHOLD" ]; then
    echo "[feed] ERROR: threshold mismatch (P1=$P1_THRESHOLD P2=$P2_THRESHOLD P3=$P3_THRESHOLD)"
    exit 1
fi
echo "[feed] threshold consistent across nodes ($P1_THRESHOLD)"

# Cleanup: cancel P1's auto-confirmation so the action goes to 0/threshold and
# stays out of the way of subsequent tests (kick.sh).
P1_CONF_CID=$(curl -s "http://localhost:$P1_HTTP/governance/confirmations?party_id=$PARTY_ID" \
    | jq -r --arg cid "$NEW_PROPOSAL" \
        '.domain_actions[] | select(.proposal_cid == $cid) | .confirmations[0].contract_id // empty')
if [ -n "$P1_CONF_CID" ]; then
    curl -s -X POST "http://localhost:$P1_HTTP/governance/cancel" \
        -H "Content-Type: application/json" \
        -d "{\"party_id\": \"$PARTY_ID\", \"confirmation_cid\": \"$P1_CONF_CID\"}" \
        > /dev/null || true
fi

echo "[feed] notification-feed test complete"
