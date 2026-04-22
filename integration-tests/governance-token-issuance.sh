#!/bin/bash

# Test governance token issuance plugin end-to-end: exercise all four
# GovernableAction templates (SetupIssuance → Mint → Burn → RotateFactory)
# through the HTTP governance flow (propose → confirm → execute) on localnet.
#
# Sourced by run.sh — expects env.sh variables, PARTY_ID, RULES_CONTRACT_ID,
# and P1/P2/P3_MEMBER_PARTY from deploy-gov-core.sh to be available.
#
# Two Utility-Registry contracts that the committee cannot create from on-chain
# governance (they need the operator's signature or a fresh registrar record)
# are auto-provisioned up front via small daml-scripts:
#   - `ProvisionProviderService`   → ProviderService cid for SetupIssuance
#   - `ProvisionSpareFactory`      → spare AllocationFactory cid for RotateFactory
# Callers may pre-set PROVIDER_SERVICE_CID / SPARE_FACTORY_CID to bypass
# the corresponding provisioning step.

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

echo "Using GovernanceRules contract: $RULES_CONTRACT_ID"
echo "Members: P1=$P1_MEMBER_PARTY, P2=$P2_MEMBER_PARTY, P3=$P3_MEMBER_PARTY"

# ============================================================================
# dpm / test-DAR setup (shared by all dpm-script invocations below)
# ============================================================================

TEST_DAR="$SCRIPT_DIR/daml/governance-token-issuance-test/.daml/dist/governance-token-issuance-test-0.1.0.dar"

ensure_dpm_and_test_dar() {
    if ! command -v dpm &>/dev/null; then
        echo "ERROR: dpm (Digital Asset Package Manager) is required for auto-provisioning."
        echo "  See https://docs.digitalasset.com/build/3.4/dpm/dpm.html for install instructions."
        exit 1
    fi

    if [ ! -f "$TEST_DAR" ]; then
        echo "Building test DAR (not found at $TEST_DAR)..."
        (cd "$SCRIPT_DIR/daml/governance-token-issuance-test" && dpm build) || {
            echo "ERROR: dpm build failed for governance-token-issuance-test"
            exit 1
        }
    fi
}

PROVISION_TOKEN="$DEV_DIR/mock-token.txt"
echo "$MOCK_TOKEN" > "$PROVISION_TOKEN"

# ============================================================================
# Auto-provision ProviderService (unless caller supplied one)
# ============================================================================

if [ -z "$PROVIDER_SERVICE_CID" ]; then
    echo ""
    echo "Auto-provisioning ProviderService via dpm script..."
    ensure_dpm_and_test_dar

    PS_INPUT="$DEV_DIR/provision-provider-input.json"
    PS_OUTPUT="$DEV_DIR/provision-provider-output.json"
    cat > "$PS_INPUT" <<EOF
{
  "governanceParty": "$PARTY_ID",
  "operator": "$P1_MEMBER_PARTY"
}
EOF

    dpm script \
        --dar "$TEST_DAR" \
        --script-name 'Governance.TokenIssuance.Scripts.ProvisionProviderService:provision' \
        --ledger-host localhost --ledger-port "$P1_CANTON_LEDGER" \
        --access-token-file "$PROVISION_TOKEN" \
        --input-file "$PS_INPUT" --output-file "$PS_OUTPUT" \
        --upload-dar yes --wall-clock-time

    PROVIDER_SERVICE_CID=$(jq -r '.providerServiceCid // empty' "$PS_OUTPUT")
    if [ -z "$PROVIDER_SERVICE_CID" ] || [ "$PROVIDER_SERVICE_CID" = "null" ]; then
        echo "ERROR: Failed to extract providerServiceCid"
        cat "$PS_OUTPUT"
        exit 1
    fi
    echo "Provisioned ProviderService: $PROVIDER_SERVICE_CID"
fi

echo "ProviderService CID: $PROVIDER_SERVICE_CID"

# ============================================================================
# Helper: run one propose → confirm → execute flow through HTTP governance.
#
# Usage: run_proposal_flow "<label>" "<proposal_json>"
#
# Exports: PROPOSAL_CID (the cid of the proposal template created by P1)
# ============================================================================

run_proposal_flow() {
    local label="$1"
    local proposal_json="$2"

    echo ""
    echo "=========================================="
    echo "Flow: $label"
    echo "=========================================="

    # --- Propose (P1 auto-confirms as proposer) ---
    local propose_req
    propose_req=$(cat <<EOF
{
  "party_id": "$PARTY_ID",
  "rules_contract_id": "$RULES_CONTRACT_ID",
  "proposal": $proposal_json
}
EOF
)
    echo "Step 1: Participant-1 proposes $label..."
    local propose_resp
    propose_resp=$(curl -s -X POST "http://localhost:$P1_HTTP/governance/propose" \
        -H "Content-Type: application/json" -d "$propose_req")
    echo "  Response: $propose_resp"
    local propose_err
    propose_err=$(echo "$propose_resp" | jq -r '.error // empty')
    if [ -n "$propose_err" ]; then
        echo "ERROR: Proposal failed: $propose_err"
        exit 1
    fi

    # --- Query confirmations, extract proposal cid ---
    echo "Step 2: Querying governance confirmations..."
    sleep 2
    local gov_resp
    gov_resp=$(curl -s "http://localhost:$P1_HTTP/governance/confirmations?party_id=$PARTY_ID")
    local action_count
    action_count=$(echo "$gov_resp" | jq '.domain_actions | length')
    if [ "$action_count" != "1" ]; then
        echo "ERROR: Expected 1 domain action after proposal, got $action_count"
        echo "  Full response: $gov_resp"
        exit 1
    fi
    PROPOSAL_CID=$(echo "$gov_resp" | jq -r '.domain_actions[0].proposal_cid // empty')
    if [ -z "$PROPOSAL_CID" ]; then
        echo "ERROR: Could not find proposal in governance confirmations"
        echo "  Full response: $gov_resp"
        exit 1
    fi
    echo "  Proposal CID: $PROPOSAL_CID"

    # --- P2 confirms ---
    echo "Step 3: Participant-2 confirms..."
    local confirm_req
    confirm_req=$(cat <<EOF
{
  "party_id": "$PARTY_ID",
  "rules_contract_id": "$RULES_CONTRACT_ID",
  "action": {"type": "governance_set_threshold", "new_threshold": 0},
  "governance_type": "core_domain",
  "proposal_cid": "$PROPOSAL_CID"
}
EOF
)
    local confirm_resp
    confirm_resp=$(curl -s -X POST "http://localhost:$P2_HTTP/governance/confirm" \
        -H "Content-Type: application/json" -d "$confirm_req")
    local confirm_err
    confirm_err=$(echo "$confirm_resp" | jq -r '.error // empty')
    if [ -n "$confirm_err" ]; then
        echo "ERROR: Confirmation failed: $confirm_err"
        exit 1
    fi

    # --- Verify can_execute, collect confirmation cids ---
    echo "Step 4: Verifying 2 confirmations..."
    sleep 2
    gov_resp=$(curl -s "http://localhost:$P1_HTTP/governance/confirmations?party_id=$PARTY_ID")
    local can_execute
    can_execute=$(echo "$gov_resp" | jq '.domain_actions[0].can_execute')
    if [ "$can_execute" != "true" ]; then
        echo "ERROR: Expected can_execute=true after 2 confirmations"
        echo "  Full response: $(echo "$gov_resp" | jq '.domain_actions[0]')"
        exit 1
    fi
    local confirmation_cids
    confirmation_cids=$(echo "$gov_resp" | jq '[.domain_actions[0].confirmations[].contract_id]')

    # --- P3 executes ---
    echo "Step 5: Participant-3 executes..."
    local execute_req
    execute_req=$(cat <<EOF
{
  "party_id": "$PARTY_ID",
  "rules_contract_id": "$RULES_CONTRACT_ID",
  "action": {"type": "governance_set_threshold", "new_threshold": 0},
  "confirmation_cids": $confirmation_cids,
  "disclosed_contracts": [],
  "governance_type": "core_domain",
  "proposal_cid": "$PROPOSAL_CID"
}
EOF
)
    local execute_resp
    execute_resp=$(curl -s -X POST "http://localhost:$P3_HTTP/governance/execute" \
        -H "Content-Type: application/json" -d "$execute_req")
    local execute_err
    execute_err=$(echo "$execute_resp" | jq -r '.error // empty')
    if [ -n "$execute_err" ]; then
        echo "ERROR: Execution failed: $execute_err"
        echo "  Full response: $execute_resp"
        exit 1
    fi

    # --- Verify cleanup ---
    echo "Step 6: Verifying cleanup..."
    sleep 2
    gov_resp=$(curl -s "http://localhost:$P1_HTTP/governance/confirmations?party_id=$PARTY_ID")
    local remaining
    remaining=$(echo "$gov_resp" | jq '.domain_actions | length')
    if [ "$remaining" != "0" ]; then
        echo "ERROR: Expected 0 domain actions after execution, got $remaining"
        exit 1
    fi

    echo "Flow '$label' completed."
}

# Query the current IssuanceConfig contract id visible to the governance party.
# Expects exactly one live config; errors otherwise.
current_issuance_config_cid() {
    local resp
    resp=$(curl -s "http://localhost:$P1_HTTP/contracts?party_id=$PARTY_ID")
    local count
    count=$(echo "$resp" | jq '[.contracts[] | select(.template_id | contains("IssuanceConfig"))] | length')
    if [ "$count" -lt 1 ]; then
        echo "ERROR: No IssuanceConfig contract visible to $PARTY_ID" >&2
        return 1
    fi
    echo "$resp" | jq -r '[.contracts[] | select(.template_id | contains("IssuanceConfig"))][0].contract_id'
}

# ============================================================================
# Flow 1: SetupIssuance — creates the IssuanceConfig
# ============================================================================

SETUP_PROPOSAL=$(cat <<EOF
{
  "type": "setup_issuance",
  "provider_service_cid": "$PROVIDER_SERVICE_CID",
  "operator": "$P1_MEMBER_PARTY",
  "instrument_id_text": "TEST-E2E-TOKEN",
  "display_name": "Test E2E Token",
  "symbol": "TEE",
  "decimals": 8
}
EOF
)
run_proposal_flow "SetupIssuance" "$SETUP_PROPOSAL"

ISSUANCE_CONFIG_CID=$(current_issuance_config_cid) || exit 1
echo "IssuanceConfig created: $ISSUANCE_CONFIG_CID"

# ============================================================================
# Flow 2: Mint — offers 100 TEST-E2E-TOKEN to the governance party (self-mint
# for E2E; a real run would target an outsider, but self is a valid holder too)
# ============================================================================

MINT_PROPOSAL=$(cat <<EOF
{
  "type": "mint",
  "issuance_config_cid": "$ISSUANCE_CONFIG_CID",
  "recipient": "$P1_MEMBER_PARTY",
  "amount": "100.0",
  "description": "E2E test mint"
}
EOF
)
run_proposal_flow "Mint" "$MINT_PROPOSAL"

# ============================================================================
# Flow 3: Burn — offers to burn 10 tokens from the same holder
# ============================================================================

BURN_PROPOSAL=$(cat <<EOF
{
  "type": "burn",
  "issuance_config_cid": "$ISSUANCE_CONFIG_CID",
  "holder": "$P1_MEMBER_PARTY",
  "amount": "10.0",
  "description": "E2E test burn"
}
EOF
)
run_proposal_flow "Burn" "$BURN_PROPOSAL"

# ============================================================================
# Flow 4: RotateFactory — swaps the AllocationFactory on the IssuanceConfig
# ============================================================================

if [ -z "$SPARE_FACTORY_CID" ]; then
    echo ""
    echo "Auto-provisioning spare AllocationFactory via dpm script..."
    ensure_dpm_and_test_dar

    SF_INPUT="$DEV_DIR/provision-factory-input.json"
    SF_OUTPUT="$DEV_DIR/provision-factory-output.json"
    cat > "$SF_INPUT" <<EOF
{
  "governanceParty": "$PARTY_ID",
  "operator": "$P1_MEMBER_PARTY"
}
EOF

    dpm script \
        --dar "$TEST_DAR" \
        --script-name 'Governance.TokenIssuance.Scripts.ProvisionSpareFactory:provision' \
        --ledger-host localhost --ledger-port "$P1_CANTON_LEDGER" \
        --access-token-file "$PROVISION_TOKEN" \
        --input-file "$SF_INPUT" --output-file "$SF_OUTPUT" \
        --upload-dar yes --wall-clock-time

    SPARE_FACTORY_CID=$(jq -r '.allocationFactoryCid // empty' "$SF_OUTPUT")
    if [ -z "$SPARE_FACTORY_CID" ] || [ "$SPARE_FACTORY_CID" = "null" ]; then
        echo "ERROR: Failed to extract allocationFactoryCid"
        cat "$SF_OUTPUT"
        exit 1
    fi
    echo "Provisioned spare AllocationFactory: $SPARE_FACTORY_CID"
fi

OLD_ISSUANCE_CONFIG_CID="$ISSUANCE_CONFIG_CID"

ROTATE_PROPOSAL=$(cat <<EOF
{
  "type": "rotate_factory",
  "issuance_config_cid": "$OLD_ISSUANCE_CONFIG_CID",
  "new_factory_cid": "$SPARE_FACTORY_CID"
}
EOF
)
run_proposal_flow "RotateFactory" "$ROTATE_PROPOSAL"

NEW_ISSUANCE_CONFIG_CID=$(current_issuance_config_cid) || exit 1
echo "IssuanceConfig after rotate: $NEW_ISSUANCE_CONFIG_CID"

if [ "$NEW_ISSUANCE_CONFIG_CID" = "$OLD_ISSUANCE_CONFIG_CID" ]; then
    echo "ERROR: RotateFactory did not produce a new IssuanceConfig cid"
    echo "  old: $OLD_ISSUANCE_CONFIG_CID"
    echo "  new: $NEW_ISSUANCE_CONFIG_CID"
    exit 1
fi

# ============================================================================
# Summary
# ============================================================================

echo ""
echo "Governance token issuance flow completed successfully!"
echo "  SetupIssuance → IssuanceConfig: $OLD_ISSUANCE_CONFIG_CID"
echo "  Mint          → offered 100 TEE to $P1_MEMBER_PARTY"
echo "  Burn          → offered to burn 10 TEE from $P1_MEMBER_PARTY"
echo "  RotateFactory → new IssuanceConfig: $NEW_ISSUANCE_CONFIG_CID"
echo "  Proposer:  participant-1 ($P1_MEMBER_PARTY)"
echo "  Confirmer: participant-2 ($P2_MEMBER_PARTY)"
echo "  Executor:  participant-3 ($P3_MEMBER_PARTY)"
