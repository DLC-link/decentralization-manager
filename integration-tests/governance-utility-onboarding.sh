#!/bin/bash

# Test governance utility-onboarding plugin end-to-end: exercise
# ProvisionProviderService → SetupUtility → Mint → Burn through the HTTP
# governance flow (propose → confirm → execute) on localnet.
#
# Sourced by run.sh — expects env.sh variables, PARTY_ID, RULES_CONTRACT_ID,
# and P1/P2/P3_MEMBER_PARTY from deploy-gov-core.sh to be available.
#
# The ProviderService required by SetupUtility has `signatory operator,
# provider` and is also provisioned via the same governance flow
# (ProvisionProviderService action); the operator authority comes from the
# proposer of the action while the provider (decentralized-party) authority
# flows through the GovernanceRules exercise chain. Callers may pre-set
# PROVIDER_SERVICE_CID to bypass the provisioning step.

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
# Helpers
# ============================================================================

# Run one propose → confirm → execute flow through HTTP governance.
#
# Usage: run_proposal_flow "<label>" "<proposal_json>"
#
# Exports: PROPOSAL_CID (the cid of the proposal template created by P1)
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

# URL-encode the '#' in package symbolic names so curl passes it as a query
# value rather than treating it as a URL fragment.
UTILITY_APP_PKG="%23utility-registry-app-v0"
UTILITY_REGISTRY_PKG="%23utility-registry-v0"

# Count contracts of a given template visible to the governance party.
#
# Usage: count_contracts_by_template <url-encoded-package-id> <module_name> <entity_name>
count_contracts_by_template() {
    local package_id="$1"
    local module_name="$2"
    local entity_name="$3"
    curl -s "http://localhost:$P1_HTTP/contracts/query?party_id=$PARTY_ID&package_id=$package_id&module_name=$module_name&entity_name=$entity_name" \
        | jq '.contracts | length'
}

# Assert that at least `expected_min` contracts of the given template exist.
#
# Usage: assert_contract_count_at_least <package_id> <module_name> <entity_name> <expected_min> <label>
assert_contract_count_at_least() {
    local package_id="$1"
    local module_name="$2"
    local entity_name="$3"
    local expected_min="$4"
    local label="$5"
    local count
    count=$(count_contracts_by_template "$package_id" "$module_name" "$entity_name")
    if [ "$count" -lt "$expected_min" ]; then
        echo "ERROR: Expected at least $expected_min $label contract(s), found $count"
        exit 1
    fi
    echo "$label contracts found: $count"
}

# Return the first contract id for a given template, or empty if none.
#
# Usage: first_contract_id <url-encoded-package-id> <module_name> <entity_name>
first_contract_id() {
    local package_id="$1"
    local module_name="$2"
    local entity_name="$3"
    curl -s "http://localhost:$P1_HTTP/contracts/query?party_id=$PARTY_ID&package_id=$package_id&module_name=$module_name&entity_name=$entity_name" \
        | jq -r '.contracts[0].contract_id // empty'
}

# ============================================================================
# Auto-provision ProviderService (unless caller supplied one) via the
# governance flow. Produces a ProviderService with
# operator = P1_MEMBER_PARTY (proposer), provider = PARTY_ID.
# ============================================================================

if [ -z "$PROVIDER_SERVICE_CID" ]; then
    run_proposal_flow "ProvisionProviderService" '{"type": "provision_provider_service"}'

    sleep 2
    PROVIDER_SERVICE_CID=$(curl -s "http://localhost:$P1_HTTP/services/provider?party_id=$PARTY_ID" \
        | jq -r '.services[0].contract_id // empty')

    if [ -z "$PROVIDER_SERVICE_CID" ] || [ "$PROVIDER_SERVICE_CID" = "null" ]; then
        echo "ERROR: Failed to extract ProviderService contract ID after ProvisionProviderService execution"
        exit 1
    fi
    echo "Provisioned ProviderService: $PROVIDER_SERVICE_CID"
fi

echo "ProviderService CID: $PROVIDER_SERVICE_CID"

# ============================================================================
# Flow: SetupUtility — runs the full utility-registry onboarding chain in one
# vote. The createTransferRule and createAllocationFactory flags on the
# underlying RegistrarServiceRequest produce the TransferRule and
# AllocationFactory as side effects of the accept step.
# ============================================================================

SETUP_PROPOSAL=$(cat <<EOF
{
  "type": "setup_utility",
  "provider_service_cid": "$PROVIDER_SERVICE_CID",
  "operator": "$P1_MEMBER_PARTY",
  "instrument_id_text": "TEST-E2E-TOKEN",
  "create_transfer_rule": true,
  "create_allocation_factory": true
}
EOF
)
run_proposal_flow "SetupUtility" "$SETUP_PROPOSAL"

sleep 2
ALLOCATION_FACTORY_CID=$(first_contract_id "$UTILITY_APP_PKG" "Utility.Registry.App.V0.Service.AllocationFactory" "AllocationFactory")
INSTRUMENT_CONFIGURATION_CID=$(first_contract_id "$UTILITY_REGISTRY_PKG" "Utility.Registry.V0.Configuration.Instrument" "InstrumentConfiguration")

if [ -z "$ALLOCATION_FACTORY_CID" ]; then
    echo "ERROR: SetupUtility did not produce an AllocationFactory"
    exit 1
fi
if [ -z "$INSTRUMENT_CONFIGURATION_CID" ]; then
    echo "ERROR: SetupUtility did not produce an InstrumentConfiguration"
    exit 1
fi
echo "AllocationFactory: $ALLOCATION_FACTORY_CID"
echo "InstrumentConfiguration: $INSTRUMENT_CONFIGURATION_CID"

# ============================================================================
# Flow: Mint — offers 100 TEST-E2E-TOKEN to P1_MEMBER_PARTY (self-mint for E2E;
# a real run would target an outsider, but self is a valid holder too).
# ============================================================================

MINT_PROPOSAL=$(cat <<EOF
{
  "type": "mint",
  "allocation_factory_cid": "$ALLOCATION_FACTORY_CID",
  "instrument_id": {"admin": "$PARTY_ID", "id": "TEST-E2E-TOKEN"},
  "instrument_configuration_cid": "$INSTRUMENT_CONFIGURATION_CID",
  "recipient": "$P1_MEMBER_PARTY",
  "amount": "100.0",
  "description": "E2E test mint"
}
EOF
)
run_proposal_flow "Mint" "$MINT_PROPOSAL"
assert_contract_count_at_least "$UTILITY_APP_PKG" "Utility.Registry.App.V0.Model.Mint" "MintOffer" 1 "MintOffer"

# ============================================================================
# Flow: Burn — offers to burn 10 tokens from the same holder.
# ============================================================================

BURN_PROPOSAL=$(cat <<EOF
{
  "type": "burn",
  "allocation_factory_cid": "$ALLOCATION_FACTORY_CID",
  "instrument_id": {"admin": "$PARTY_ID", "id": "TEST-E2E-TOKEN"},
  "instrument_configuration_cid": "$INSTRUMENT_CONFIGURATION_CID",
  "holder": "$P1_MEMBER_PARTY",
  "amount": "10.0",
  "description": "E2E test burn"
}
EOF
)
run_proposal_flow "Burn" "$BURN_PROPOSAL"
assert_contract_count_at_least "$UTILITY_APP_PKG" "Utility.Registry.App.V0.Model.Burn" "BurnOffer" 1 "BurnOffer"

# ============================================================================
# Summary
# ============================================================================

echo ""
echo "Governance utility-onboarding flow completed successfully!"
echo "  ProvisionProviderService → ProviderService: $PROVIDER_SERVICE_CID"
echo "  SetupUtility             → AllocationFactory: $ALLOCATION_FACTORY_CID"
echo "                              InstrumentConfiguration: $INSTRUMENT_CONFIGURATION_CID"
echo "  Mint                     → MintOffer created for $P1_MEMBER_PARTY (100 TEST-E2E-TOKEN)"
echo "  Burn                     → BurnOffer created for $P1_MEMBER_PARTY (10 TEST-E2E-TOKEN)"
echo "  Proposer:  participant-1 ($P1_MEMBER_PARTY)"
echo "  Confirmer: participant-2 ($P2_MEMBER_PARTY)"
echo "  Executor:  participant-3 ($P3_MEMBER_PARTY)"
