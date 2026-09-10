#!/bin/bash

LOCALNET_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

set -a
. "$LOCALNET_LIB_DIR/versions.env"
set +a

LOCALNET_CACHE_DIR="$(cd "$LOCALNET_LIB_DIR/.." && pwd)/.localnet"
LOCALNET_DIR="$LOCALNET_CACHE_DIR/splice-node/docker-compose/localnet"
LOCALNET_BUNDLE_URL="https://github.com/digital-asset/decentralized-canton-sync/releases/download/v${LOCALNET_VERSION}/${LOCALNET_VERSION}_splice-node.tar.gz"

export DOCKER_NETWORK="${DOCKER_NETWORK:-localnet}"

download_localnet() {
    if [ -d "$LOCALNET_CACHE_DIR/splice-node" ]; then
        echo "Localnet bundle $LOCALNET_VERSION already cached"
        return 0
    fi

    echo "Downloading localnet bundle v${LOCALNET_VERSION} (about 760MB, once)..."
    mkdir -p "$LOCALNET_CACHE_DIR"
    curl -fSL "$LOCALNET_BUNDLE_URL" -o "$LOCALNET_CACHE_DIR/splice-node.tar.gz"

    echo "Extracting..."
    tar xzf "$LOCALNET_CACHE_DIR/splice-node.tar.gz" -C "$LOCALNET_CACHE_DIR"
    rm -f "$LOCALNET_CACHE_DIR/splice-node.tar.gz"

    if [ ! -f "$LOCALNET_DIR/compose.yaml" ]; then
        echo "ERROR: unexpected bundle layout: $LOCALNET_DIR/compose.yaml is missing" >&2
        return 1
    fi
    echo "Localnet bundle ready"
}

localnet_compose() {
    export IMAGE_TAG="$LOCALNET_VERSION"
    docker compose \
        --env-file "$LOCALNET_DIR/compose.env" \
        --env-file "$LOCALNET_DIR/env/common.env" \
        -f "$LOCALNET_DIR/compose.yaml" \
        -f "$LOCALNET_DIR/resource-constraints.yaml" \
        --profile sv \
        --profile app-provider \
        --profile app-user \
        "$@"
}

# Only the services the DecMan nodes use. The 3 active profiles otherwise also
# bring up nginx + 7 web UI containers (wallet/ans/scan/sv UIs) which nothing
# here talks to — DecMan reaches the Canton ledger/admin gRPC ports directly.
# canton -> postgres and splice -> canton come along via depends_on.
#
# --wait blocks until canton + splice healthchecks pass. Splice healthy means
# /api/validator/readyz returns OK, i.e. splice has registered the global
# synchronizer with all 3 participants. Without it, DecMan races ahead and gets
# "No participant ID returned" / "synchronizer with alias global is unknown".
localnet_start() {
    localnet_compose up -d --wait canton splice postgres
}

localnet_stop() {
    [ -d "$LOCALNET_DIR" ] || return 0
    localnet_compose stop
}

localnet_wipe() {
    [ -d "$LOCALNET_DIR" ] || return 0
    localnet_compose down -v 2>/dev/null || true
}
