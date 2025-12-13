#!/bin/bash

set -eou pipefail

NUM_PARTICIPANTS=3
HTTP_PORTS=(8081 8082 8083)
CONFIG_DIR="development/config"
DATA_DIR="development/data"
LOG_DIR="${DATA_DIR}/logs"
PIDS=()

echo "=================================================="
echo "  Starting Decentralized Party Participants"
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

echo "Checking for existing processes on HTTP ports..."
for port in "${HTTP_PORTS[@]}"; do
    kill_port "$port"
done
sleep 1

echo "Cleaning up logs and workflow data..."
rm -rf "${DATA_DIR}/logs"
rm -rf "${DATA_DIR}/workflow-data"
mkdir -p "${DATA_DIR}/keys"
mkdir -p "${DATA_DIR}/logs"
mkdir -p "${DATA_DIR}/workflow-data"

echo "Building..."
cargo build

export RUST_LOG="dec_party_onboarding=info"

echo ""
echo "Starting participants..."
echo "Logs will be written to ${LOG_DIR}/"
echo ""

for i in $(seq 1 $NUM_PARTICIPANTS); do
    PORT=${HTTP_PORTS[$i-1]}
    echo "[${i}/${NUM_PARTICIPANTS}] Starting Participant ${i} on port ${PORT}..."

    ./target/debug/dec-party-onboarding \
        -c "${CONFIG_DIR}/node-${i}.toml" \
        serve --port "${PORT}" \
        > "${LOG_DIR}/participant-${i}.log" 2>&1 &

    PIDS+=($!)
    echo "       PID: ${PIDS[$i-1]}, URL: http://localhost:${PORT}, Log: ${LOG_DIR}/participant-${i}.log"
    sleep 1
done

echo ""
echo "All participants started!"
echo ""
echo "=================================================="
echo "  Access Points"
echo "=================================================="
echo ""
echo "  Participant 1: http://localhost:8081"
echo "  Participant 2: http://localhost:8082"
echo "  Participant 3: http://localhost:8083"
echo ""
echo "Use 'tail -f ${LOG_DIR}/*.log' to watch all logs"
echo "Press Ctrl+C to stop all processes"
echo ""

wait
