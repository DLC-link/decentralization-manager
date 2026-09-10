#!/bin/bash
set -euo pipefail

. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

if [ "${1-}" != "--yes" ]; then
    printf 'This deletes the ledger, the demo party and all three DecMan databases.\n'
    printf 'The downloaded LocalNet bundle stays in %s.\n\n' "$LOCALNET_CACHE_DIR"
    printf 'Continue? [y/N] '
    read -r answer
    case "$answer" in
        y | Y | yes | YES) ;;
        *) die "cancelled" ;;
    esac
fi

say "Removing the DecMan nodes and their volumes"
decman_compose down -v

if [ -d "$LOCALNET_DIR" ]; then
    say "Removing LocalNet and its volumes"
    localnet_wipe
fi

rm -f "$STATE_FILE"

say "Reset done"
info "run hackathon/up.sh for a fresh stack"
