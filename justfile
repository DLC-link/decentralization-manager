default:
    @just --list

# Regenerate the frontend TypeScript wire types from the Rust DTOs via ts-rs.
# The output (crates/decman/frontend/src/types.generated.ts) is committed; CI
# checks it is up to date. Run this after changing any wire DTO.
[group('frontend')]
gen-types:
    #!/usr/bin/env bash
    set -euo pipefail
    # DECMAN_SKIP_FRONTEND so build.rs doesn't try to build the frontend (which
    # would need this very file) while we compile the generator.
    DECMAN_SKIP_FRONTEND=1 cargo run -q -p decman --features typegen --bin gen-types
    echo "Generated crates/decman/frontend/src/types.generated.ts"

# Forward Canton ibtc-devnet participant 1..3 Ledger/Admin ports (KUBE_NS=catalyst-canton by default).
[group('canton')]
port-forward:
    #!/usr/bin/env bash
    set -uo pipefail

    ns="${KUBE_NS:-catalyst-canton}"
    pids=()

    cleanup() {
        printf '\n[port-forward] stopping…\n' >&2
        for pid in "${pids[@]}"; do
            kill "$pid" 2>/dev/null || true
        done
        wait 2>/dev/null || true
    }
    trap cleanup INT TERM EXIT

    fwd() {
        local tag=$1 svc=$2; shift 2
        kubectl port-forward -n "$ns" "svc/$svc" "$@" 2>&1 \
            | sed -u "s/^/[$tag] /" &
        pids+=($!)
    }

    fwd p1 participant-ibtc-devnet-1 5001:5001 5002:5002
    fwd p2 participant-ibtc-devnet-2 5011:5001 5012:5002
    fwd p3 participant-ibtc-devnet-3 5021:5001 5022:5002

    echo "[port-forward] namespace: $ns"
    echo "[port-forward]   participant 1  ->  localhost:5001 / 5002"
    echo "[port-forward]   participant 2  ->  localhost:5011 / 5012"
    echo "[port-forward]   participant 3  ->  localhost:5021 / 5022"
    echo "[port-forward] Ctrl-C to stop all."

    wait
