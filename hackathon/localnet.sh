#!/bin/bash

LOCALNET_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

set -a
. "$LOCALNET_LIB_DIR/versions.env"
set +a

LOCALNET_CACHE_DIR="$(cd "$LOCALNET_LIB_DIR/.." && pwd)/.localnet"
LOCALNET_DIR="$LOCALNET_CACHE_DIR/splice-node/docker-compose/localnet"
LOCALNET_BUNDLE_URL="https://github.com/digital-asset/decentralized-canton-sync/releases/download/v${LOCALNET_VERSION}/${LOCALNET_VERSION}_splice-node.tar.gz"

export DOCKER_NETWORK="${DOCKER_NETWORK:-localnet}"

# LocalNet's unsafe-auth JWT: HS256 over the dev secret, sub=ledger-api-user,
# aud=https://canton.network.global, no exp. It is what Canton's JSON Ledger
# API accepts here and what DecMan mints for itself in insecure mode. LocalNet
# only — it authorizes nothing anywhere else.
LOCALNET_CANTON_TOKEN="eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJhdWQiOiJodHRwczovL2NhbnRvbi5uZXR3b3JrLmdsb2JhbCIsImlhdCI6MTc2Mzc0ODcwMiwic3ViIjoibGVkZ2VyLWFwaS11c2VyIn0.vpkfH4SoM9AZqbE38W4hrvl3xxy69jYs4u8gveskw9k"

# The stamp is written only after the layout check passes, so it certifies
# "this exact version, fully extracted". A version bump or an interrupted
# extraction leaves no matching stamp and the bundle is fetched again —
# without it, a stale splice-node/ would boot new images against old compose
# files, or a half-extracted tree would be reused forever.
LOCALNET_STAMP="$LOCALNET_CACHE_DIR/.version"

localnet_cached() {
    [ -f "$LOCALNET_DIR/compose.yaml" ] || return 1
    [ -f "$LOCALNET_STAMP" ] || return 1
    [ "$(cat "$LOCALNET_STAMP")" = "$LOCALNET_VERSION" ]
}

download_localnet() {
    if localnet_cached; then
        echo "Localnet bundle $LOCALNET_VERSION already cached"
        return 0
    fi

    if [ -d "$LOCALNET_CACHE_DIR/splice-node" ]; then
        echo "Cached bundle is incomplete or not $LOCALNET_VERSION; fetching it again..."
        rm -rf "$LOCALNET_CACHE_DIR/splice-node"
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
    printf '%s\n' "$LOCALNET_VERSION" > "$LOCALNET_STAMP"
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
