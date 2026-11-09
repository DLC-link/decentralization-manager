#!/bin/bash
# Turn Canton's key export off on the running localnet and restart Canton.
# The key_download_off phase of the Rust e2e runs this; see that phase.
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/env.sh"
disable_localnet_key_download
