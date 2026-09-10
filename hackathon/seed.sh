#!/bin/bash
set -euo pipefail

. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

PREFIX_FROM_ENV="${PARTY_PREFIX-}"
load_state
PARTY_PREFIX="${PREFIX_FROM_ENV:-${PARTY_PREFIX:-demo-party}}"
DEC_PARTY_ID="${DEC_PARTY_ID-}"
RULES_CID="${RULES_CID-}"
DARS_DONE="${DARS_DONE-}"
MEMBER_1="${MEMBER_1-}"
MEMBER_2="${MEMBER_2-}"
MEMBER_3="${MEMBER_3-}"

read_participant_ids() {
    local idx
    for idx in 1 2 3; do
        eval "PID_$idx=\$(dm_get \"\$(http_port $idx)\" /node-config | jq -r '.node.participant_id')"
    done
    [ -n "$PID_1" ] && [ -n "$PID_2" ] && [ -n "$PID_3" ] || die "could not read the participant IDs"
}

find_party_id() {
    local attempt=0 found
    while [ "$attempt" -lt 60 ]; do
        found=$(dm_get 8081 /decentralized-parties \
            | jq -r --arg p "$PARTY_PREFIX" 'first(.parties[]? | select(.party_id | startswith($p + "::")) | .party_id) // empty')
        if [ -n "$found" ]; then
            printf '%s' "$found"
            return 0
        fi
        attempt=$((attempt + 1))
        sleep 2
    done
    return 1
}

create_party() {
    local existing
    if [ -n "$DEC_PARTY_ID" ]; then
        info "reusing the party from hackathon/.state: $DEC_PARTY_ID"
        return 0
    fi
    existing=$(dm_get 8081 /decentralized-parties \
        | jq -r --arg p "$PARTY_PREFIX" 'first(.parties[]? | select(.party_id | startswith($p + "::")) | .party_id) // empty')
    if [ -n "$existing" ]; then
        info "reusing the party that already exists at this prefix"
        state_set DEC_PARTY_ID "$existing"
        return 0
    fi

    say "Creating the decentralized party $PARTY_PREFIX with threshold 2"
    state_set PARTY_PREFIX "$PARTY_PREFIX"
    dm_post 8081 /onboarding "$(jq -n \
        --arg prefix "$PARTY_PREFIX" \
        --arg p2 "$PID_2" \
        --arg p3 "$PID_3" \
        '{party_id_prefix: $prefix, peer_ids: [$p2, $p3], threshold: 2}')" >/dev/null

    accept_invitation 2 Onboarding
    accept_invitation 3 Onboarding
    poll_workflow 8081 /onboarding/status "the onboarding workflow"

    local party_id
    party_id=$(find_party_id) || die "the party was created but $PARTY_PREFIX is not visible in /decentralized-parties"
    state_set DEC_PARTY_ID "$party_id"
    info "party: $DEC_PARTY_ID"
}

build_dar_payload() {
    local out=$1 peers=${2-}
    local tmpdir name path
    tmpdir=$(mktemp -d)
    : > "$tmpdir/entries.json"
    for name in $DAR_FILES; do
        path="$REPO_DIR/releases/v1/$name"
        [ -f "$path" ] || die "missing DAR: $path"
        base64 < "$path" | tr -d '\n' > "$tmpdir/b64"
        jq -n --arg filename "$name" --rawfile data "$tmpdir/b64" \
            '{filename: $filename, data: $data}' >> "$tmpdir/entries.json"
    done
    if [ -n "$peers" ]; then
        jq -s --argjson peers "$peers" '{dar_files: ., peer_ids: $peers}' "$tmpdir/entries.json" > "$out"
    else
        jq -s '{dar_files: .}' "$tmpdir/entries.json" > "$out"
    fi
    rm -rf "$tmpdir"
}

distribute_dars() {
    if [ -n "$DARS_DONE" ]; then
        info "the governance DARs are already distributed"
        return 0
    fi

    say "Uploading and distributing the governance DARs"
    local workdir
    workdir=$(mktemp -d)
    build_dar_payload "$workdir/upload.json"
    build_dar_payload "$workdir/distribute.json" "$(jq -n --arg p2 "$PID_2" --arg p3 "$PID_3" '[$p2, $p3]')"

    dm_post 8081 /dars/upload "@$workdir/upload.json" >/dev/null
    dm_post 8081 /dars/distribute "@$workdir/distribute.json" >/dev/null
    rm -rf "$workdir"

    accept_invitation 2 Dars
    accept_invitation 3 Dars
    poll_workflow 8081 /dars/distribute/status "the DAR distribution"
    state_set DARS_DONE 1
}

allocate_member_parties() {
    local idx port hint current
    say "Allocating one member party per participant"
    for idx in 1 2 3; do
        eval "current=\${MEMBER_$idx}"
        port=$(json_api_port "$idx")
        hint="gov-member-p$idx"
        if [ -z "$current" ]; then
            current=$(canton_post "$port" /v2/parties "$(jq -n --arg hint "$hint" \
                '{party_id_hint: $hint, local_metadata: {annotations: {}}}')" \
                | jq -r '.partyDetails.party')
            [ -n "$current" ] && [ "$current" != "null" ] || die "could not allocate $hint on $(node_name "$idx")"
            state_set "MEMBER_$idx" "$current"
        fi
        info "$(node_name "$idx") member party: $current"
        grant_rights "$port" "$current"
    done
}

grant_rights() {
    local port=$1 party=$2
    canton_post "$port" /v2/users/ledger-api-user/rights "$(jq -n --arg party "$party" \
        '{userId: "ledger-api-user", identityProviderId: "", rights: [
            {kind: {CanActAs: {value: {party: $party}}}},
            {kind: {CanReadAs: {value: {party: $party}}}}
        ]}')" >/dev/null
}

configure_party_on_nodes() {
    local idx port member
    say "Registering the party on all three nodes"
    for idx in 1 2 3; do
        grant_rights "$(json_api_port "$idx")" "$DEC_PARTY_ID"
    done
    for idx in 1 2 3; do
        port=$(http_port "$idx")
        eval "member=\${MEMBER_$idx}"
        dm_put "$port" /party-config "$(jq -n \
            --arg dec "$DEC_PARTY_ID" \
            --arg member "$member" \
            '{
                dec_party_id: $dec,
                member_party_id: $member,
                user_id: "ledger-api-user",
                keycloak_url: "",
                keycloak_realm: "",
                keycloak_client_id: "",
                packages: {
                    governance_action: "#governance-action-v1",
                    governance_core: "#governance-core-v1",
                    governance_token_custody: "#governance-token-custody-v1",
                    governance_utility_onboarding: "#governance-utility-onboarding-v1"
                }
            }')" >/dev/null
        info "$(node_name "$idx") is configured"
    done
}

find_rules_cid() {
    local attempt=0 cid
    while [ "$attempt" -lt 30 ]; do
        cid=$(dm_get 8081 /decentralized-parties | jq -r --arg id "$DEC_PARTY_ID" \
            'first(.parties[]? | select(.party_id == $id) | .contracts[]? | select(.template_id | contains("GovernanceRules")) | .contract_id) // empty')
        if [ -z "$cid" ]; then
            cid=$(dm_get 8081 "/governance/state?party_id=$DEC_PARTY_ID" | jq -r '.state.contract_id // empty')
        fi
        if [ -n "$cid" ]; then
            printf '%s' "$cid"
            return 0
        fi
        attempt=$((attempt + 1))
        sleep 2
    done
    return 1
}

deploy_governance_core() {
    if [ -n "$RULES_CID" ]; then
        info "the governance core is already deployed: $RULES_CID"
        return 0
    fi

    say "Deploying the governance core contract"
    dm_get 8081 /decentralized-parties >/dev/null
    dm_post 8081 /contracts "$(jq -n \
        --arg dec "$DEC_PARTY_ID" \
        --arg p1 "$PID_1" --arg p2 "$PID_2" --arg p3 "$PID_3" \
        --arg m1 "$MEMBER_1" --arg m2 "$MEMBER_2" --arg m3 "$MEMBER_3" \
        '{
            decentralized_party_id: $dec,
            participant_ids: [$p1, $p2, $p3],
            participant_parties: [$m1, $m2, $m3],
            operator_party: $m1,
            contracts: [{
                id: "governance-rules",
                name: "GovernanceRules",
                package_id: "#governance-core-v1",
                module_name: "Governance.Rules",
                entity_name: "GovernanceRules",
                fields: [
                    {type: "decentralized_party"},
                    {type: "party_set", parties: [$m1, $m2, $m3]},
                    {type: "int64", value: 2},
                    {type: "rel_time", microseconds: 1800000000},
                    {type: "none"}
                ]
            }]
        }')" >/dev/null

    accept_invitation 2 Contracts
    accept_invitation 3 Contracts
    poll_workflow 8081 /contracts/status "the contract deployment"

    local cid
    cid=$(find_rules_cid) || die "the workflow completed but no GovernanceRules contract is visible"
    state_set RULES_CID "$cid"
}

print_summary() {
    say "The demo party is ready"
    cat <<SUMMARY
    party            $DEC_PARTY_ID
    rules contract   $RULES_CID
    member parties   $MEMBER_1
                     $MEMBER_2
                     $MEMBER_3

    State is recorded in hackathon/.state, so hackathon/demo.sh picks it up.
    Next:  hackathon/demo.sh    run one propose, confirm and execute
SUMMARY
}

require_stack_up
read_participant_ids
create_party
distribute_dars
allocate_member_parties
configure_party_on_nodes
deploy_governance_core
print_summary
