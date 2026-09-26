#!/bin/bash
set -euo pipefail

. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

say "Stopping the three DecMan nodes"
decman_compose stop

if [ -d "$LOCALNET_DIR" ]; then
    say "Stopping LocalNet"
    localnet_stop
fi

say "Stopped"
info "the ledger, the party and the DecMan databases are kept"
info "start again with hackathon/up.sh, or wipe everything with hackathon/reset.sh"
