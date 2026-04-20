#!/bin/bash

# Test governance token custody plugin: propose → confirm → execute.
# Sourced by run.sh — expects env.sh variables, PARTY_ID, RULES_CONTRACT_ID,
# and P1/P2/P3_MEMBER_PARTY from deploy-gov-core.sh to be available.

if [ -z "$RULES_CONTRACT_ID" ]; then
    echo "ERROR: RULES_CONTRACT_ID not set (deploy-gov-core.sh must run first)"
    exit 1
fi

echo "Using GovernanceRules contract: $RULES_CONTRACT_ID"
echo "Members: P1=$P1_MEMBER_PARTY, P2=$P2_MEMBER_PARTY, P3=$P3_MEMBER_PARTY"

# ============================================================================
# Step 1: Participant-1 proposes a SetupCcPreapproval action
# ============================================================================

# The propose endpoint creates the proposal contract AND auto-confirms as proposer.
# So after this step we have 1 confirmation from P1.

PROPOSE_REQUEST=$(cat <<EOF
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

echo ""
echo "Step 1: Participant-1 proposes SetupCcPreapproval..."
PROPOSE_RESPONSE=$(curl -s -X POST "http://localhost:$P1_HTTP/governance/propose" \
    -H "Content-Type: application/json" \
    -d "$PROPOSE_REQUEST")
echo "  Response: $PROPOSE_RESPONSE"

PROPOSE_ERROR=$(echo "$PROPOSE_RESPONSE" | jq -r '.error // empty')
if [ -n "$PROPOSE_ERROR" ]; then
    echo "ERROR: Proposal failed: $PROPOSE_ERROR"
    exit 1
fi

# ============================================================================
# Step 2: Query governance state to find the proposal
# ============================================================================

echo ""
echo "Step 2: Querying governance confirmations..."
sleep 2

GOVERNANCE_RESPONSE=$(curl -s "http://localhost:$P1_HTTP/governance/confirmations?party_id=$PARTY_ID")
echo "  Domain actions: $(echo "$GOVERNANCE_RESPONSE" | jq '.domain_actions | length')"
echo "  Threshold: $(echo "$GOVERNANCE_RESPONSE" | jq '.threshold')"

# Extract the proposal contract ID from domain_actions
PROPOSAL_CID=$(echo "$GOVERNANCE_RESPONSE" | jq -r '.domain_actions[0].proposal_cid // empty')
if [ -z "$PROPOSAL_CID" ]; then
    echo "ERROR: Could not find proposal in governance confirmations"
    echo "  Full response: $GOVERNANCE_RESPONSE"
    exit 1
fi

echo "  Proposal CID: $PROPOSAL_CID"

# Get the confirmation CID from P1's auto-confirm
P1_CONFIRMATION_CID=$(echo "$GOVERNANCE_RESPONSE" | jq -r \
    '.domain_actions[0].confirmations[0].contract_id // empty')
echo "  P1 confirmation CID: $P1_CONFIRMATION_CID"

CURRENT_CONFIRMATIONS=$(echo "$GOVERNANCE_RESPONSE" | jq '.domain_actions[0].confirmation_count')
echo "  Current confirmations: $CURRENT_CONFIRMATIONS"

# ============================================================================
# Step 3: Participant-2 confirms the action
# ============================================================================

echo ""
echo "Step 3: Participant-2 confirms the action..."

# For CoreDomain governance, confirm needs the proposal_cid
# The action type must match a governance_set_threshold or similar self-action,
# but for domain actions the confirm choice takes a different path.
# Actually for domain actions, confirm uses GovernanceRules_ConfirmAction with the proposal_cid.
# The confirm endpoint needs a "dummy" action type for CoreDomain — the real action
# is identified by the proposal_cid.
CONFIRM_REQUEST=$(cat <<EOF
{
  "party_id": "$PARTY_ID",
  "rules_contract_id": "$RULES_CONTRACT_ID",
  "action": {
    "type": "governance_set_threshold",
    "new_threshold": 0
  },
  "governance_type": "core_domain",
  "proposal_cid": "$PROPOSAL_CID"
}
EOF
)

CONFIRM_RESPONSE=$(curl -s -X POST "http://localhost:$P2_HTTP/governance/confirm" \
    -H "Content-Type: application/json" \
    -d "$CONFIRM_REQUEST")
echo "  Response: $CONFIRM_RESPONSE"

CONFIRM_ERROR=$(echo "$CONFIRM_RESPONSE" | jq -r '.error // empty')
if [ -n "$CONFIRM_ERROR" ]; then
    echo "ERROR: Confirmation failed: $CONFIRM_ERROR"
    exit 1
fi

# ============================================================================
# Step 4: Verify we now have 2 confirmations (threshold met)
# ============================================================================

echo ""
echo "Step 4: Verifying confirmations..."
sleep 2

GOVERNANCE_RESPONSE=$(curl -s "http://localhost:$P1_HTTP/governance/confirmations?party_id=$PARTY_ID")
CURRENT_CONFIRMATIONS=$(echo "$GOVERNANCE_RESPONSE" | jq '.domain_actions[0].confirmation_count')
CAN_EXECUTE=$(echo "$GOVERNANCE_RESPONSE" | jq '.domain_actions[0].can_execute')

echo "  Confirmations: $CURRENT_CONFIRMATIONS"
echo "  Can execute: $CAN_EXECUTE"

if [ "$CAN_EXECUTE" != "true" ]; then
    echo "ERROR: Expected can_execute=true after 2 confirmations (threshold=2)"
    echo "  Full response: $(echo "$GOVERNANCE_RESPONSE" | jq '.domain_actions[0]')"
    exit 1
fi

# Collect all confirmation CIDs
CONFIRMATION_CIDS=$(echo "$GOVERNANCE_RESPONSE" | jq '[.domain_actions[0].confirmations[].contract_id]')
echo "  Confirmation CIDs: $CONFIRMATION_CIDS"

# ============================================================================
# Step 5: Participant-3 executes the confirmed action
# ============================================================================

echo ""
echo "Step 5: Participant-3 executes the confirmed action..."

EXECUTE_REQUEST=$(cat <<EOF
{
  "party_id": "$PARTY_ID",
  "rules_contract_id": "$RULES_CONTRACT_ID",
  "action": {
    "type": "governance_set_threshold",
    "new_threshold": 0
  },
  "confirmation_cids": $CONFIRMATION_CIDS,
  "disclosed_contracts": [],
  "governance_type": "core_domain",
  "proposal_cid": "$PROPOSAL_CID"
}
EOF
)

EXECUTE_RESPONSE=$(curl -s -X POST "http://localhost:$P3_HTTP/governance/execute" \
    -H "Content-Type: application/json" \
    -d "$EXECUTE_REQUEST")
echo "  Response: $EXECUTE_RESPONSE"

EXECUTE_ERROR=$(echo "$EXECUTE_RESPONSE" | jq -r '.error // empty')
if [ -n "$EXECUTE_ERROR" ]; then
    echo "ERROR: Execution failed: $EXECUTE_ERROR"
    exit 1
fi

# ============================================================================
# Step 6: Verify the action was executed (no more pending domain actions)
# ============================================================================

echo ""
echo "Step 6: Verifying execution..."
sleep 2

GOVERNANCE_RESPONSE=$(curl -s "http://localhost:$P1_HTTP/governance/confirmations?party_id=$PARTY_ID")
REMAINING_ACTIONS=$(echo "$GOVERNANCE_RESPONSE" | jq '.domain_actions | length')

echo "  Remaining domain actions: $REMAINING_ACTIONS"

if [ "$REMAINING_ACTIONS" != "0" ]; then
    echo "ERROR: Expected 0 domain actions after execution, got $REMAINING_ACTIONS"
    exit 1
fi

# ============================================================================
# Step 7: Verify the governance audit trail
# ============================================================================

echo ""
echo "Step 7: Verifying governance audit trail..."
sleep 2

# P1 proposed (propose auto-confirms, so one "propose" entry)
P1_AUDIT=$(curl -s "http://localhost:$P1_HTTP/governance/audit?party_id=$PARTY_ID")
P1_AUDIT_COUNT=$(echo "$P1_AUDIT" | jq '.total_returned')
echo "  P1 audit entries: $P1_AUDIT_COUNT"

if [ "$P1_AUDIT_COUNT" -lt 1 ]; then
    echo "ERROR: Expected at least 1 audit entry for P1 (propose), got $P1_AUDIT_COUNT"
    echo "  Full response: $P1_AUDIT"
    exit 1
fi

P1_PROPOSE_EVENT=$(echo "$P1_AUDIT" | jq -r '[.entries[] | select(.event_type == "propose")][0].event_type // empty')
if [ "$P1_PROPOSE_EVENT" != "propose" ]; then
    echo "ERROR: Expected propose event in P1 audit trail"
    echo "  Entries: $(echo "$P1_AUDIT" | jq '[.entries[].event_type]')"
    exit 1
fi

P1_PROPOSE_STATUS=$(echo "$P1_AUDIT" | jq -r '[.entries[] | select(.event_type == "propose")][0].status // empty')
if [ "$P1_PROPOSE_STATUS" != "success" ]; then
    echo "ERROR: Expected propose status=success, got $P1_PROPOSE_STATUS"
    exit 1
fi

# P2 confirmed
P2_AUDIT=$(curl -s "http://localhost:$P2_HTTP/governance/audit?party_id=$PARTY_ID")
P2_AUDIT_COUNT=$(echo "$P2_AUDIT" | jq '.total_returned')
echo "  P2 audit entries: $P2_AUDIT_COUNT"

if [ "$P2_AUDIT_COUNT" -lt 1 ]; then
    echo "ERROR: Expected at least 1 audit entry for P2 (confirm), got $P2_AUDIT_COUNT"
    echo "  Full response: $P2_AUDIT"
    exit 1
fi

P2_CONFIRM_EVENT=$(echo "$P2_AUDIT" | jq -r '[.entries[] | select(.event_type == "confirm")][0].event_type // empty')
if [ "$P2_CONFIRM_EVENT" != "confirm" ]; then
    echo "ERROR: Expected confirm event in P2 audit trail"
    echo "  Entries: $(echo "$P2_AUDIT" | jq '[.entries[].event_type]')"
    exit 1
fi

# P3 executed
P3_AUDIT=$(curl -s "http://localhost:$P3_HTTP/governance/audit?party_id=$PARTY_ID")
P3_AUDIT_COUNT=$(echo "$P3_AUDIT" | jq '.total_returned')
echo "  P3 audit entries: $P3_AUDIT_COUNT"

if [ "$P3_AUDIT_COUNT" -lt 1 ]; then
    echo "ERROR: Expected at least 1 audit entry for P3 (execute), got $P3_AUDIT_COUNT"
    echo "  Full response: $P3_AUDIT"
    exit 1
fi

P3_EXECUTE_EVENT=$(echo "$P3_AUDIT" | jq -r '[.entries[] | select(.event_type == "execute")][0].event_type // empty')
if [ "$P3_EXECUTE_EVENT" != "execute" ]; then
    echo "ERROR: Expected execute event in P3 audit trail"
    echo "  Entries: $(echo "$P3_AUDIT" | jq '[.entries[].event_type]')"
    exit 1
fi

# Verify audit entry details are valid JSON
P1_DETAILS=$(echo "$P1_AUDIT" | jq -r '.entries[0].details // empty')
if [ -z "$P1_DETAILS" ] || [ "$P1_DETAILS" = "null" ]; then
    echo "ERROR: Audit entry details should contain request JSON"
    exit 1
fi

echo "  All audit entries verified!"

echo ""
echo "Governance token custody flow completed successfully!"
echo "  Proposer:  participant-1 ($P1_MEMBER_PARTY)"
echo "  Confirmer: participant-2 ($P2_MEMBER_PARTY)"
echo "  Executor:  participant-3 ($P3_MEMBER_PARTY)"
