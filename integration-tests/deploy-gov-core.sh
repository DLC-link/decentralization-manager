#!/bin/bash

# Deploy governance core contract via the contracts workflow.
# Sourced by run.sh — expects env.sh variables and PARTY_ID/PARTY_JSON
# from create-dec-party.sh to be available (DARs already distributed).
#
# Exports: RULES_CONTRACT_ID, P1_MEMBER_PARTY, P2_MEMBER_PARTY, P3_MEMBER_PARTY


# Canton HTTP JSON API ports (participant port prefix + 975)
P1_JSON_API=3975
P2_JSON_API=2975
P3_JSON_API=4975

# ============================================================================
# Allocate member parties on each Canton participant
# ============================================================================

# Each participant needs a unique member party for governance to work.
# We allocate a party on each Canton participant via the JSON Ledger API,
# then grant actAs/readAs rights to ledger-api-user.

# Allocate a party and print only the party ID to stdout (logs go to stderr).
allocate_party() {
    local json_api_port=$1
    local party_hint=$2
    local name=$3

    echo "Allocating party '$party_hint' on $name (port $json_api_port)..." >&2
    local response
    response=$(curl -s -X POST "http://localhost:$json_api_port/v2/parties" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer $MOCK_TOKEN" \
        -d "{\"party_id_hint\": \"$party_hint\", \"local_metadata\": {\"annotations\": {}}}")

    local party_id
    party_id=$(echo "$response" | jq -r '.partyDetails.party // empty')

    if [ -z "$party_id" ]; then
        echo "ERROR: Failed to allocate party on $name: $response" >&2
        return 1
    fi

    echo "  Allocated: $party_id" >&2
    echo "$party_id"
}

grant_user_rights() {
    local json_api_port=$1
    local party_id=$2
    local name=$3

    echo "Granting actAs+readAs on '$party_id' to ledger-api-user on $name..."
    local response http_code
    response=$(curl -s -w "\n%{http_code}" -X POST "http://localhost:$json_api_port/v2/users/ledger-api-user/rights" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer $MOCK_TOKEN" \
        -d "{
            \"userId\": \"ledger-api-user\",
            \"rights\": [
                {\"kind\": {\"CanActAs\": {\"value\": {\"party\": \"$party_id\"}}}},
                {\"kind\": {\"CanReadAs\": {\"value\": {\"party\": \"$party_id\"}}}}
            ],
            \"identityProviderId\": \"\"
        }")
    http_code=$(echo "$response" | tail -1)
    local body
    body=$(echo "$response" | sed '$d')

    if [ "$http_code" -lt 200 ] || [ "$http_code" -ge 300 ]; then
        echo "ERROR: Failed to grant rights on $name (HTTP $http_code): $body"
        exit 1
    fi
}

# Extract participant UIDs from the party JSON
P1_UID=$(echo "$PARTY_JSON" | jq -r '.participants[0].participant_uid')
P2_UID=$(echo "$PARTY_JSON" | jq -r '.participants[1].participant_uid')
P3_UID=$(echo "$PARTY_JSON" | jq -r '.participants[2].participant_uid')

echo "Participant UIDs:"
echo "  P1: $P1_UID"
echo "  P2: $P2_UID"
echo "  P3: $P3_UID"

# Allocate member parties (stdout = party ID only, logs go to stderr)
P1_MEMBER_PARTY=$(allocate_party $P1_JSON_API "gov-member-p1" "participant-1") || exit 1
P2_MEMBER_PARTY=$(allocate_party $P2_JSON_API "gov-member-p2" "participant-2") || exit 1
P3_MEMBER_PARTY=$(allocate_party $P3_JSON_API "gov-member-p3" "participant-3") || exit 1

# Grant rights on member parties
grant_user_rights $P1_JSON_API "$P1_MEMBER_PARTY" "participant-1"
grant_user_rights $P2_JSON_API "$P2_MEMBER_PARTY" "participant-2"
grant_user_rights $P3_JSON_API "$P3_MEMBER_PARTY" "participant-3"

# Also grant rights on the decentralized party
grant_user_rights $P1_JSON_API "$PARTY_ID" "participant-1"
grant_user_rights $P2_JSON_API "$PARTY_ID" "participant-2"
grant_user_rights $P3_JSON_API "$PARTY_ID" "participant-3"

echo ""
echo "Member parties:"
echo "  P1: $P1_MEMBER_PARTY"
echo "  P2: $P2_MEMBER_PARTY"
echo "  P3: $P3_MEMBER_PARTY"

# ============================================================================
# Update party config on each participant with the correct member party ID
# ============================================================================

update_party_config() {
    local port=$1
    local member_party=$2
    local name=$3

    echo "Updating party config on $name..."
    local response http_code
    response=$(curl -s -w "\n%{http_code}" -X PUT "http://localhost:$port/party-config" \
        -H "Content-Type: application/json" \
        -d "{
            \"dec_party_id\": \"$PARTY_ID\",
            \"member_party_id\": \"$member_party\",
            \"user_id\": \"ledger-api-user\",
            \"keycloak_url\": \"\",
            \"keycloak_realm\": \"\",
            \"keycloak_client_id\": \"\",
            \"packages\": {
                \"governance_core\": \"#governance-core-v0-rc3\",
                \"governance_token_custody\": \"#governance-token-custody-v0-rc3\",
                \"governance_token_issuance\": \"#governance-token-issuance-v0-rc3\",
                \"utility_registry\": \"#utility-registry-app-v0\"
            }
        }")
    http_code=$(echo "$response" | tail -1)
    if [ "$http_code" -lt 200 ] || [ "$http_code" -ge 300 ]; then
        local body
        body=$(echo "$response" | sed '$d')
        echo "ERROR: Failed to update party config on $name (HTTP $http_code): $body"
        exit 1
    fi
}

update_party_config $P1_HTTP "$P1_MEMBER_PARTY" "participant-1"
update_party_config $P2_HTTP "$P2_MEMBER_PARTY" "participant-2"
update_party_config $P3_HTTP "$P3_MEMBER_PARTY" "participant-3"

# ============================================================================
# Deploy GovernanceRules contract
# ============================================================================

CONTRACTS_REQUEST=$(cat <<EOF
{
  "decentralized_party_id": "$PARTY_ID",
  "participant_ids": ["$P1_UID", "$P2_UID", "$P3_UID"],
  "participant_parties": ["$P1_MEMBER_PARTY", "$P2_MEMBER_PARTY", "$P3_MEMBER_PARTY"],
  "operator_party": "$P1_MEMBER_PARTY",
  "contracts": [
    {
      "id": "governance-rules",
      "name": "GovernanceRules",
      "package_id": "#governance-core-v0-rc3",
      "module_name": "Governance.Rules",
      "entity_name": "GovernanceRules",
      "fields": [
        {"type": "decentralized_party"},
        {"type": "party_set", "parties": ["$P1_MEMBER_PARTY", "$P2_MEMBER_PARTY", "$P3_MEMBER_PARTY"]},
        {"type": "int64", "value": 2},
        {"type": "rel_time", "microseconds": 1800000000}
      ]
    }
  ]
}
EOF
)

echo ""
echo "Starting contracts deployment on participant-1..."
curl -s -X POST "http://localhost:$P1_HTTP/contracts" \
    -H "Content-Type: application/json" \
    -d "$CONTRACTS_REQUEST"
echo ""

# Accept invitations on attestors in parallel
accept_invitation $P2_HTTP "participant-2" "Contracts" &
PID_ACCEPT1=$!
accept_invitation $P3_HTTP "participant-3" "Contracts" &
PID_ACCEPT2=$!
wait $PID_ACCEPT1 $PID_ACCEPT2

poll_status $P1_HTTP "contracts/status"

# ============================================================================
# Extract the GovernanceRules contract ID
# ============================================================================

sleep 2
PARTIES_RESPONSE=$(curl -s "http://localhost:$P1_HTTP/decentralized-parties")
RULES_CONTRACT_ID=$(echo "$PARTIES_RESPONSE" | jq -r --arg pid "$PARTY_ID" \
    '.parties[] | select(.party_id == $pid) | .contracts[] | select(.template_id | contains("GovernanceRules")) | .contract_id' | head -1)

if [ -z "$RULES_CONTRACT_ID" ] || [ "$RULES_CONTRACT_ID" = "null" ]; then
    # Try governance state endpoint as fallback
    RULES_CONTRACT_ID=$(curl -s "http://localhost:$P1_HTTP/governance/state?party_id=$PARTY_ID" \
        | jq -r '.state.contract_id // empty')
fi

if [ -z "$RULES_CONTRACT_ID" ]; then
    echo "ERROR: Could not extract GovernanceRules contract ID"
    exit 1
fi

echo "GovernanceRules contract deployed: $RULES_CONTRACT_ID"
