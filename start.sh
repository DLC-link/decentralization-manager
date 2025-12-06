#!/bin/bash

set -eou pipefail

WORKFLOW="${1:-onboarding}"
PARTY_ID="${2:-}"
PARTICIPANT_ID="${3:-}"
NAMESPACE_FP="${4:-}"
NUM_PARTICIPANTS=3
PORTS=(9001 9002 9003)
LOG_DIR="logs"
KEYS_DIR="keys"
PIDS=()

echo "=================================================="
echo "  Workflow Test: ${WORKFLOW}"
echo "=================================================="
echo ""

kill_port() {
    lsof -ti:"$1" | xargs kill -9 2>/dev/null || true
}

cleanup() {
    echo ""
    echo "Cleaning up processes..."
    for pid in "${PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
    done
    wait 2>/dev/null || true
}

trap cleanup EXIT INT TERM

echo "Checking for existing processes..."
for port in "${PORTS[@]}"; do
    kill_port "$port"
done
sleep 1

echo "Cleaning up old files..."
rm -rf logs keys
if [ "$WORKFLOW" = "onboarding" ]; then
    rm -rf workflow-data
elif [ ! -d "workflow-data" ]; then
    echo "Error: workflow-data directory does not exist. Run onboarding first."
    exit 1
fi

echo "Building..."
cargo build

ONBOARDING() { ./target/debug/dec-party-onboarding "$@"; }

export RUST_LOG="dec_party_onboarding=debug"

echo ""
echo "Generating Noise protocol keys..."
mkdir -p "$KEYS_DIR" "$LOG_DIR"

declare -a PUBKEYS
for i in $(seq 1 $NUM_PARTICIPANTS); do
    echo "  - Generating key for participant-${i}..."
    PUBKEYS[$i]=$(ONBOARDING keygen -o "${KEYS_DIR}/participant-${i}.key" 2>&1 | grep "Public key (hex):" | awk '{print $NF}')
    echo "    Public key: ${PUBKEYS[$i]}"
done

echo ""
echo "Updating test-configs/network.toml with new public keys..."
for i in $(seq 1 $NUM_PARTICIPANTS); do
    sed -i.bak "s/public_key = \".*\" # participant-${i}/public_key = \"${PUBKEYS[$i]}\" # participant-${i}/" test-configs/network.toml
done
rm -f test-configs/network.toml.bak

# If kick workflow, validate required parameters
if [ "$WORKFLOW" = "kick" ]; then
    if [ -z "$PARTY_ID" ] || [ -z "$PARTICIPANT_ID" ] || [ -z "$NAMESPACE_FP" ]; then
        echo "Error: Kick workflow requires all three parameters"
        echo ""
        echo "Usage: ./start.sh kick <party-id> <participant-id> <namespace-fingerprint>"
        echo ""
        echo "Example:"
        echo "  ./start.sh kick \\"
        echo "    'cbtc-network::1220abc...' \\"
        echo "    'participant::1220def...' \\"
        echo "    '1220ghi...'"
        echo ""
        echo "Tip: Run './target/debug/dec-party-onboarding -c test-configs/node-1.toml query-parties'"
        echo "     to see available parties, participants, and namespace fingerprints"
        exit 1
    fi

    echo ""
    echo "Kick configuration:"
    echo "  Party ID:              $PARTY_ID"
    echo "  Participant ID:        $PARTICIPANT_ID"
    echo "  Namespace Fingerprint: $NAMESPACE_FP"

    KICK_ARGS="--decentralized-party-id $PARTY_ID --participant-id $PARTICIPANT_ID --namespace-fingerprint $NAMESPACE_FP"
fi

echo ""
echo "Starting ${WORKFLOW} workflow..."
echo "Logs will be written to ${LOG_DIR}/"
echo ""

for i in $(seq 1 $NUM_PARTICIPANTS); do
    echo "[${i}/${NUM_PARTICIPANTS}] Starting Participant ${i}..."

    if [ "$WORKFLOW" = "kick" ]; then
        ONBOARDING -c "test-configs/node-${i}.toml" kick $KICK_ARGS > "${LOG_DIR}/participant-${i}.log" 2>&1 &
    else
        ONBOARDING -c "test-configs/node-${i}.toml" "$WORKFLOW" > "${LOG_DIR}/participant-${i}.log" 2>&1 &
    fi

    PIDS+=($!)
    echo "       PID: ${PIDS[$i-1]}, Log: ${LOG_DIR}/participant-${i}.log"
    [ "$i" -eq 1 ] && sleep 2
done

echo ""
echo "All processes started!"
echo ""
echo "=================================================="
echo "  Monitoring Progress"
echo "=================================================="
echo ""
echo "Use 'tail -f ${LOG_DIR}/*.log' to watch all logs"
echo "Press Ctrl+C to stop all processes"
echo ""

echo "Waiting for processes to complete..."

while true; do
    all_stopped=true
    for i in $(seq 0 $((NUM_PARTICIPANTS - 1))); do
        if kill -0 "${PIDS[$i]}" 2>/dev/null; then
            all_stopped=false
        fi
    done

    if $all_stopped; then
        echo "All participants have stopped"
        break
    fi

    if ! kill -0 "${PIDS[0]}" 2>/dev/null; then
        echo "Participant 1 has stopped"
        break
    fi

    sleep 1
done

echo ""
echo "=================================================="
echo "  Test Complete"
echo "=================================================="
echo ""
echo "Check workflow-data/ for generated files"
