#!/bin/bash

# Test governance token issuance plugin end-to-end: exercise all four
# GovernableAction templates (SetupIssuance → Mint → Burn → RotateFactory)
# through the HTTP governance flow (propose → confirm → execute) on localnet.
#
# Sourced by run.sh — expects env.sh variables, PARTY_ID, RULES_CONTRACT_ID,
# and P1/P2/P3_MEMBER_PARTY from deploy-gov-core.sh to be available.
#
# The ProviderService required by SetupIssuance is also provisioned via the
# same governance flow (ProvisionProviderService action), since it has
# `signatory operator, provider` and the operator authority comes from the
# proposer of the action while the provider (decentralized-party) authority
# flows through the GovernanceRules exercise chain. The spare
# AllocationFactory consumed by RotateFactory only has the decentralized
# party as signatory (operator is an observer), so it is created via the
# single-signatory contracts workflow. Callers may pre-set
# PROVIDER_SERVICE_CID / SPARE_FACTORY_CID to bypass either provisioning
# step.

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
ISSUANCE_PKG="%23governance-token-issuance-v0-rc3"
UTILITY_APP_PKG="%23utility-registry-app-v0"

# Query the current IssuanceConfig contract id visible to the governance party.
# Uses /contracts/query (live Canton ACS) rather than /decentralized-parties
# (which is 60s-cached). Expects exactly one live config; errors otherwise.
current_issuance_config_cid() {
    local resp
    resp=$(curl -s "http://localhost:$P1_HTTP/contracts/query?party_id=$PARTY_ID&package_id=$ISSUANCE_PKG&module_name=Governance.TokenIssuance.IssuanceConfig&entity_name=IssuanceConfig")
    local count
    count=$(echo "$resp" | jq '.contracts | length')
    if [ "$count" -lt 1 ]; then
        echo "ERROR: No IssuanceConfig contract visible to $PARTY_ID" >&2
        return 1
    fi
    echo "$resp" | jq -r '.contracts[0].contract_id'
}

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

# ============================================================================
# Auto-provision ProviderService (unless caller supplied one) via the
# governance flow. Produces a ProviderService with
# operator = P1_MEMBER_PARTY (proposer), provider = PARTY_ID.
# ============================================================================

if [ -z "$PROVIDER_SERVICE_CID" ]; then
    run_proposal_flow "ProvisionProviderService" '{"type": "provision_provider_service"}'

    # Use /services/provider (live Canton ACS query) rather than
    # /decentralized-parties, which has a 60s cache that stays stale right
    # after deploy-gov-core.sh populated it.
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
assert_contract_count_at_least "$UTILITY_APP_PKG" "Utility.Registry.App.V0.Model.Mint" "MintOffer" 1 "MintOffer"

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
assert_contract_count_at_least "$UTILITY_APP_PKG" "Utility.Registry.App.V0.Model.Burn" "BurnOffer" 1 "BurnOffer"

# ============================================================================
# Flow 4: RotateFactory — swaps the AllocationFactory on the IssuanceConfig
# ============================================================================

if [ -z "$SPARE_FACTORY_CID" ]; then
    echo ""
    echo "Auto-provisioning spare AllocationFactory via contracts workflow..."

    ALLOCATION_FACTORY_QUERY="http://localhost:$P1_HTTP/contracts/query?party_id=$PARTY_ID&package_id=$UTILITY_APP_PKG&module_name=Utility.Registry.App.V0.Service.AllocationFactory&entity_name=AllocationFactory"

    # Snapshot existing AllocationFactory cids so we can diff out the new one
    # after the create. (SetupIssuance already produced one, and both the old
    # and the new share all the same signatories and fields, so querying
    # after-the-fact can't tell them apart without this diff.)
    EXISTING_FACTORY_CIDS=$(curl -s "$ALLOCATION_FACTORY_QUERY" | jq -r '.contracts[].contract_id' | sort)

    FACTORY_REQUEST=$(cat <<EOF
{
  "decentralized_party_id": "$PARTY_ID",
  "participant_ids": ["$P1_UID", "$P2_UID", "$P3_UID"],
  "participant_parties": ["$P1_MEMBER_PARTY", "$P2_MEMBER_PARTY", "$P3_MEMBER_PARTY"],
  "operator_party": "$P1_MEMBER_PARTY",
  "contracts": [
    {
      "id": "spare-allocation-factory",
      "name": "AllocationFactory",
      "package_id": "#utility-registry-app-v0",
      "module_name": "Utility.Registry.App.V0.Service.AllocationFactory",
      "entity_name": "AllocationFactory",
      "fields": [
        {"type": "decentralized_party"},
        {"type": "decentralized_party"},
        {"type": "operator_party"}
      ]
    }
  ]
}
EOF
)

    curl -s -X POST "http://localhost:$P1_HTTP/contracts" \
        -H "Content-Type: application/json" \
        -d "$FACTORY_REQUEST"
    echo ""

    # Accept invitations on attestors in parallel
    accept_invitation $P2_HTTP "participant-2" "Contracts" &
    PID_ACCEPT1=$!
    accept_invitation $P3_HTTP "participant-3" "Contracts" &
    PID_ACCEPT2=$!
    wait $PID_ACCEPT1 $PID_ACCEPT2

    poll_status $P1_HTTP "contracts/status"

    sleep 2
    AFTER_FACTORY_CIDS=$(curl -s "$ALLOCATION_FACTORY_QUERY" | jq -r '.contracts[].contract_id' | sort)

    # Pick the cid that appears in `after` but not in `before` — that's the spare
    SPARE_FACTORY_CID=$(comm -13 <(echo "$EXISTING_FACTORY_CIDS") <(echo "$AFTER_FACTORY_CIDS") | head -1)

    if [ -z "$SPARE_FACTORY_CID" ] || [ "$SPARE_FACTORY_CID" = "null" ]; then
        echo "ERROR: Failed to extract AllocationFactory contract ID"
        echo "  Before: $EXISTING_FACTORY_CIDS"
        echo "  After:  $AFTER_FACTORY_CIDS"
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
echo "  Mint          → MintOffer created for $P1_MEMBER_PARTY (100 TEE)"
echo "  Burn          → BurnOffer created for $P1_MEMBER_PARTY (10 TEE)"
echo "  RotateFactory → new IssuanceConfig: $NEW_ISSUANCE_CONFIG_CID"
echo "  Proposer:  participant-1 ($P1_MEMBER_PARTY)"
echo "  Confirmer: participant-2 ($P2_MEMBER_PARTY)"
echo "  Executor:  participant-3 ($P3_MEMBER_PARTY)"
