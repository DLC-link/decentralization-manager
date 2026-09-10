#!/bin/bash
set -euo pipefail

. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

download_bundle() {
    say "Fetching the Splice LocalNet bundle $LOCALNET_VERSION"
    download_localnet || die "the bundle is not ready. Delete $LOCALNET_CACHE_DIR and run this script again."
}

start_localnet() {
    say "Starting LocalNet (Canton, Splice, Postgres)"
    info "the first start also pulls several GB of images"
    localnet_start
}

pull_decman_image() {
    say "Pulling $DECMAN_IMAGE"
    if docker pull --platform linux/amd64 "$DECMAN_IMAGE" >/dev/null; then
        return 0
    fi
    if [ -f "$HOME/.docker/config.json" ] \
        && jq -e '.auths | has("public.ecr.aws")' "$HOME/.docker/config.json" >/dev/null 2>&1; then
        die "the pull failed and your Docker config holds a public.ecr.aws login, which is probably expired. The image is public, so drop the login and try again: docker logout public.ecr.aws"
    fi
    die "could not pull $DECMAN_IMAGE. Check the network, then run this script again."
}

start_decman() {
    say "Starting three DecMan nodes"
    decman_compose up -d
    wait_for_all_nodes
}

configure_peers() {
    local idx port key participant_id peers

    if peers_connected; then
        info "the peer mesh is already configured"
        return 0
    fi

    say "Configuring the peer mesh"
    peers="[]"
    for idx in 1 2 3; do
        port=$(http_port "$idx")
        key=$(dm_get "$port" /keys/status | jq -r '.public_key')
        participant_id=$(dm_get "$port" /node-config | jq -r '.node.participant_id')
        [ -n "$key" ] && [ "$key" != "null" ] || die "$(node_name "$idx") has no Noise public key yet"
        [ -n "$participant_id" ] && [ "$participant_id" != "null" ] \
            || die "$(node_name "$idx") could not read its participant ID from Canton"
        info "$(node_name "$idx") is $participant_id"
        peers=$(printf '%s' "$peers" | jq \
            --arg participant_id "$participant_id" \
            --arg name "Participant $idx" \
            --arg address "$(node_name "$idx")" \
            --argjson port "$(noise_port "$idx")" \
            --arg public_key "$key" \
            '. + [{participant_id: $participant_id, name: $name, address: $address, port: $port, public_key: $public_key, party: null}]')
    done

    for idx in 1 2 3; do
        dm_post "$(http_port "$idx")" /network-config "$peers" >/dev/null
    done

    say "Restarting the nodes so they load the peer keys"
    decman_compose restart
    wait_for_all_nodes

    local attempt=0
    while [ "$attempt" -lt 45 ]; do
        if peers_connected; then
            info "all three nodes see each other"
            return 0
        fi
        attempt=$((attempt + 1))
        sleep 2
    done
    warn "the peer mesh did not converge yet. Check it with: curl -s localhost:8081/participants-status | jq . — and read the logs with: docker compose -p $DECMAN_PROJECT logs"
}

print_summary() {
    say "LocalNet is up"
    cat <<SUMMARY
    DecMan 1  http://localhost:8081
    DecMan 2  http://localhost:8082
    DecMan 3  http://localhost:8083
    API docs  http://localhost:8081/swagger-ui/

    Next:  hackathon/seed.sh    create a demo party and deploy the governance core
           hackathon/demo.sh    run one propose, confirm and execute
           hackathon/down.sh    stop everything and keep the data
           hackathon/reset.sh   delete all data and start from zero

    Walkthrough: hackathon/WALKTHROUGH.md
SUMMARY
}

require_tools
check_docker_resources

if [ -z "$(decman_compose ps -q 2>/dev/null)" ]; then
    used=$(ports_in_use 8081 8082 8083)
    [ -z "$used" ] || die "these ports are taken by another process:$used. Free them and try again."
fi

download_bundle
pull_decman_image
start_localnet
start_decman
configure_peers
print_summary
