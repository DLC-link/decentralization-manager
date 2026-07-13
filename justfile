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

# Forward Canton devnet participant 1..3 Ledger/Admin ports. Each node lives in
# its own namespace (KUBE_NS_PREFIX=canton-node- by default -> canton-node-1..3).
[group('canton')]
port-forward:
    #!/usr/bin/env bash
    set -uo pipefail

    prefix="${KUBE_NS_PREFIX:-canton-node-}"
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
        local tag=$1 ns=$2 svc=$3; shift 3
        kubectl port-forward -n "$ns" "svc/$svc" "$@" 2>&1 \
            | sed -u "s/^/[$tag] /" &
        pids+=($!)
    }

    fwd p1 "${prefix}1" participant 5001:5001 5002:5002
    fwd p2 "${prefix}2" participant 5011:5001 5012:5002
    fwd p3 "${prefix}3" participant 5021:5001 5022:5002

    echo "[port-forward]   participant 1 (${prefix}1)  ->  localhost:5001 / 5002"
    echo "[port-forward]   participant 2 (${prefix}2)  ->  localhost:5011 / 5012"
    echo "[port-forward]   participant 3 (${prefix}3)  ->  localhost:5021 / 5022"
    echo "[port-forward] Ctrl-C to stop all."

    wait
