#!/bin/bash
set -euo pipefail

. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

load_state
DEC_PARTY_ID="${DEC_PARTY_ID-}"
RULES_CID="${RULES_CID-}"
VOTE_DESCRIPTION="${VOTE_DESCRIPTION:-Adopt the hackathon demo motion}"

# A core_domain confirm and execute build their choice from proposal_cid and
# ignore `action`, but the endpoint still requires a well-formed one. Both
# first-party clients send this exact zero placeholder, and the local audit log
# stores the request as sent — so anything else writes a governance action into
# the log that nobody proposed.
PLACEHOLDER_ACTION='{"type": "governance_set_threshold", "new_threshold": 0}'

list_proposal_cids() {
    local port=$1
    try_get "$port" "/governance/confirmations?party_id=$DEC_PARTY_ID" \
        | jq -r '.domain_actions[]?.proposal_cid' 2>/dev/null || true
}

propose() {
    say "P1 proposes a generic vote"
    info "\"$VOTE_DESCRIPTION\""
    dm_post 8081 /governance/propose "$(jq -n \
        --arg party "$DEC_PARTY_ID" \
        --arg rules "$RULES_CID" \
        --arg description "$VOTE_DESCRIPTION" \
        '{party_id: $party, rules_contract_id: $rules, proposal: {type: "generic_vote", description: $description}}')" >/dev/null
}

wait_for_new_proposal() {
    local before=$1 attempt=0 cid
    while [ "$attempt" -lt 60 ]; do
        for cid in $(list_proposal_cids 8081); do
            if ! printf '%s\n' "$before" | grep -Fxq "$cid"; then
                printf '%s' "$cid"
                return 0
            fi
        done
        attempt=$((attempt + 1))
        sleep 2
    done
    return 1
}

wait_for_proposal_on() {
    local idx=$1 cid=$2 port attempt=0
    port=$(http_port "$idx")
    while [ "$attempt" -lt 60 ]; do
        if list_proposal_cids "$port" | grep -Fxq "$cid"; then
            return 0
        fi
        attempt=$((attempt + 1))
        sleep 2
    done
    die "$(node_name "$idx") never saw proposal $cid"
}

wait_for_executable() {
    local idx=$1 cid=$2 port attempt=0 cids
    port=$(http_port "$idx")
    while [ "$attempt" -lt 60 ]; do
        cids=$(try_get "$port" "/governance/confirmations?party_id=$DEC_PARTY_ID" \
            | jq -c --arg cid "$cid" \
                'first(.domain_actions[]? | select(.proposal_cid == $cid and .can_execute)) | [.confirmations[]?.contract_id] // empty' 2>/dev/null || true)
        if [ -n "$cids" ] && [ "$cids" != "null" ]; then
            printf '%s' "$cids"
            return 0
        fi
        attempt=$((attempt + 1))
        sleep 2
    done
    return 1
}

wait_until_settled() {
    local cid=$1 attempt=0
    while [ "$attempt" -lt 60 ]; do
        if ! list_proposal_cids 8081 | grep -Fxq "$cid"; then
            info "the proposal is no longer pending"
            return 0
        fi
        attempt=$((attempt + 1))
        sleep 2
    done
    warn "the proposal is still listed as pending"
}

print_evidence() {
    local attempt=0 audit path
    path="/governance/chain-audit?party_id=$DEC_PARTY_ID&limit=20&refresh=true"
    say "Reading the on-chain evidence from P1"
    while [ "$attempt" -lt 30 ]; do
        audit=$(dm_get 8081 "$path")
        if [ "$(printf '%s' "$audit" | jq '[.entries[]? | select(.event_type == "execute")] | length')" != "0" ]; then
            printf '%s' "$audit" | jq '[.entries[] | {event_type, update_id, offset, timestamp, acting_parties, contract_id}]'
            say "Event types recorded"
            printf '%s' "$audit" | jq -r '.entries | group_by(.event_type)[] | "    \(.[0].event_type): \(length)"'
            return 0
        fi
        attempt=$((attempt + 1))
        sleep 2
    done
    die "the execute event did not appear in the audit trail"
}

require_stack_up
[ -n "$DEC_PARTY_ID" ] || die "no party found in hackathon/.state — run hackathon/seed.sh first"

# Read the live rules contract rather than the one seed.sh recorded: any
# executed threshold or timeout change archives it and creates a new one.
LIVE_RULES=$(try_get 8081 "/governance/state?party_id=$DEC_PARTY_ID" | jq -r '.state.contract_id // empty')
RULES_CID="${LIVE_RULES:-$RULES_CID}"
[ -n "$RULES_CID" ] || die "party $DEC_PARTY_ID has no governance rules contract — run hackathon/seed.sh first"

info "party $DEC_PARTY_ID"
info "rules $RULES_CID"
BEFORE=$(list_proposal_cids 8081)
propose
PROPOSAL_CID=$(wait_for_new_proposal "$BEFORE") || die "the proposal did not appear on P1"
info "proposal $PROPOSAL_CID"

say "P2 confirms"
wait_for_proposal_on 2 "$PROPOSAL_CID"
dm_post 8082 /governance/confirm "$(jq -n \
    --arg party "$DEC_PARTY_ID" \
    --arg rules "$RULES_CID" \
    --arg cid "$PROPOSAL_CID" \
    --argjson action "$PLACEHOLDER_ACTION" \
    '{party_id: $party, rules_contract_id: $rules, action: $action, governance_type: "core_domain", proposal_cid: $cid}')" >/dev/null

say "P3 executes"
wait_for_proposal_on 3 "$PROPOSAL_CID"
CONFIRMATION_CIDS=$(wait_for_executable 3 "$PROPOSAL_CID") \
    || die "the proposal never reached the confirmation threshold on P3"
info "confirmations $CONFIRMATION_CIDS"
dm_post 8083 /governance/execute "$(jq -n \
    --arg party "$DEC_PARTY_ID" \
    --arg rules "$RULES_CID" \
    --arg cid "$PROPOSAL_CID" \
    --argjson action "$PLACEHOLDER_ACTION" \
    --argjson confirmations "$CONFIRMATION_CIDS" \
    '{party_id: $party, rules_contract_id: $rules, action: $action, confirmation_cids: $confirmations, disclosed_contracts: [], governance_type: "core_domain", proposal_cid: $cid}')" >/dev/null

wait_until_settled "$PROPOSAL_CID"
print_evidence

say "Done"
info "the same trail is in the UI under Audit Trail on http://localhost:8081"
