#!/bin/bash

# Test governance token issuance plugin: propose SetupIssuance → confirm → execute.
# Sourced by run.sh — expects env.sh variables, PARTY_ID, RULES_CONTRACT_ID,
# and P1/P2/P3_MEMBER_PARTY from deploy-gov-core.sh to be available.
#
# Prerequisite: PROVIDER_SERVICE_CID must be set to a pre-existing ProviderService
# contract ID. If unset, this script prints a skip notice and exits 0.

# ============================================================================
# Guard clauses
# ============================================================================

if [ -z "$RULES_CONTRACT_ID" ]; then
    echo "ERROR: RULES_CONTRACT_ID not set (deploy-gov-core.sh must run first)"
    exit 1
fi

if [ -z "$PARTY_ID" ]; then
    echo "ERROR: PARTY_ID not set (create-dec-party.sh must run first)"
    exit 1
fi

if [ -z "$P1_MEMBER_PARTY" ] || [ -z "$P2_MEMBER_PARTY" ] || [ -z "$P3_MEMBER_PARTY" ]; then
    echo "ERROR: P1/P2/P3_MEMBER_PARTY not set (deploy-gov-core.sh must run first)"
    exit 1
fi

if [ -z "$PROVIDER_SERVICE_CID" ]; then
    cat <<'SKIP'
SKIP: governance-token-issuance.sh — PROVIDER_SERVICE_CID env var not set.

This E2E requires a pre-existing ProviderService contract for the governance
party. Auto-provisioning one on localnet requires a daml-script run (tracked
as a follow-up). To run this test now, provision a ProviderService externally
and export PROVIDER_SERVICE_CID before invoking run.sh.
SKIP
    exit 0
fi

echo "Using GovernanceRules contract: $RULES_CONTRACT_ID"
echo "Members: P1=$P1_MEMBER_PARTY, P2=$P2_MEMBER_PARTY, P3=$P3_MEMBER_PARTY"
echo "ProviderService CID: $PROVIDER_SERVICE_CID"

# ============================================================================
# Step 1: Participant-1 proposes a SetupIssuance action
# ============================================================================

# The propose endpoint creates the proposal contract AND auto-confirms as proposer.
# So after this step we have 1 confirmation from P1.

PROPOSE_REQUEST=$(cat <<EOF
{
  "party_id": "$PARTY_ID",
  "rules_contract_id": "$RULES_CONTRACT_ID",
  "proposal": {
    "type": "setup_issuance",
    "provider_service_cid": "$PROVIDER_SERVICE_CID",
    "operator": "$P1_MEMBER_PARTY",
    "instrument_id_text": "TEST-E2E-TOKEN",
    "display_name": "Test E2E Token",
    "symbol": "TEE",
    "decimals": 8
  }
}
EOF
)

echo ""
echo "Step 1: Participant-1 proposes SetupIssuance..."
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

ACTION_COUNT=$(echo "$GOVERNANCE_RESPONSE" | jq '.domain_actions | length')
if [ "$ACTION_COUNT" != "1" ]; then
    echo "ERROR: Expected 1 domain action after proposal, got $ACTION_COUNT"
    echo "  Full response: $GOVERNANCE_RESPONSE"
    exit 1
fi

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

# For CoreDomain governance, confirm needs the proposal_cid.
# The action type must match a valid action shape; for domain actions the confirm
# choice uses the proposal_cid to identify the action — the action payload is a dummy.
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
# Step 6: Verify cleanup AND IssuanceConfig creation
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

# Verify IssuanceConfig contract was created
CONTRACTS_RESPONSE=$(curl -s "http://localhost:$P1_HTTP/contracts?party_id=$PARTY_ID")
ISSUANCE_CONFIG_COUNT=$(echo "$CONTRACTS_RESPONSE" | jq \
    '[.contracts[] | select(.template_id | contains("IssuanceConfig"))] | length')

echo "  IssuanceConfig contracts found: $ISSUANCE_CONFIG_COUNT"

if [ "$ISSUANCE_CONFIG_COUNT" -lt 1 ]; then
    echo "ERROR: Expected at least 1 IssuanceConfig contract after execution, found $ISSUANCE_CONFIG_COUNT"
    echo "  Contracts response: $CONTRACTS_RESPONSE"
    exit 1
fi

ISSUANCE_CONFIG_CID=$(echo "$CONTRACTS_RESPONSE" | jq -r \
    '[.contracts[] | select(.template_id | contains("IssuanceConfig"))][0].contract_id')
echo "  IssuanceConfig contract ID: $ISSUANCE_CONFIG_CID"

echo ""
echo "Governance token issuance flow completed successfully!"
echo "  Proposer:  participant-1 ($P1_MEMBER_PARTY)"
echo "  Confirmer: participant-2 ($P2_MEMBER_PARTY)"
echo "  Executor:  participant-3 ($P3_MEMBER_PARTY)"
