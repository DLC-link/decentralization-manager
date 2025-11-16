#!/bin/bash

set -eou pipefail

echo "=================================================="
echo "  Noise Protocol Communication Test"
echo "=================================================="
echo ""

# Kill any existing processes on our ports
echo "Checking for existing processes..."
lsof -ti:9001 | xargs kill -9 2>/dev/null || true
lsof -ti:9002 | xargs kill -9 2>/dev/null || true
lsof -ti:9003 | xargs kill -9 2>/dev/null || true
sleep 1

# Cleanup function
cleanup() {
    echo ""
    echo "Cleaning up processes..."
    if [ ! -z "${PARTICIPANT_1_PID:-}" ]; then
        kill ${PARTICIPANT_1_PID} 2>/dev/null || true
    fi
    if [ ! -z "${PARTICIPANT_2_PID:-}" ]; then
        kill ${PARTICIPANT_2_PID} 2>/dev/null || true
    fi
    if [ ! -z "${PARTICIPANT_3_PID:-}" ]; then
        kill ${PARTICIPANT_3_PID} 2>/dev/null || true
    fi
    wait 2>/dev/null || true
}

# Set up trap to cleanup on exit
trap cleanup EXIT INT TERM

echo "Cleaning up old files..."
rm -rf workflow-data
rm -rf logs
rm -rf keys

echo "Building..."
cargo build

ONBOARDING() { ./target/debug/dec-party-onboarding "$@"; }

export RUST_LOG="dec_party_onboarding=info"

echo ""
echo "Generating Noise protocol keys..."
mkdir -p keys

# Generate keys and capture public keys
echo "  - Generating key for participant-1..."
PUBKEY_1=$(ONBOARDING keygen -o keys/participant-1.key 2>&1 | grep "Public key (hex):" | awk '{print $NF}')

echo "  - Generating key for participant-2..."
PUBKEY_2=$(ONBOARDING keygen -o keys/participant-2.key 2>&1 | grep "Public key (hex):" | awk '{print $NF}')

echo "  - Generating key for participant-3..."
PUBKEY_3=$(ONBOARDING keygen -o keys/participant-3.key 2>&1 | grep "Public key (hex):" | awk '{print $NF}')

echo "Keys generated!"
echo "  - participant-1 public key: ${PUBKEY_1}"
echo "  - participant-2 public key: ${PUBKEY_2}"
echo "  - participant-3 public key: ${PUBKEY_3}"

# Update network.toml with new public keys
echo ""
echo "Updating test-configs/network.toml with new public keys..."
sed -i.bak "s/public_key = \".*\" # participant-1/public_key = \"${PUBKEY_1}\" # participant-1/" test-configs/network.toml
sed -i.bak "s/public_key = \".*\" # participant-2/public_key = \"${PUBKEY_2}\" # participant-2/" test-configs/network.toml
sed -i.bak "s/public_key = \".*\" # participant-3/public_key = \"${PUBKEY_3}\" # participant-3/" test-configs/network.toml
rm -f test-configs/network.toml.bak
echo "network.toml updated successfully!"

# Configuration files
CONFIG_1="test-configs/node-1.toml"
CONFIG_2="test-configs/node-2.toml"
CONFIG_3="test-configs/node-3.toml"

# Log files (named by participant ID, not role - role is determined at runtime)
LOG_DIR="logs"
mkdir -p ${LOG_DIR}
PARTICIPANT_1_LOG="${LOG_DIR}/participant-1.log"
PARTICIPANT_2_LOG="${LOG_DIR}/participant-2.log"
PARTICIPANT_3_LOG="${LOG_DIR}/participant-3.log"

echo ""
echo "Starting Noise protocol communication test..."
echo "Logs will be written to ${LOG_DIR}/"
echo ""

# Start participant-1 in background
echo "[1/3] Starting Participant 1..."
ONBOARDING -c "${CONFIG_1}" start > "${PARTICIPANT_1_LOG}" 2>&1 &
PARTICIPANT_1_PID=$!
echo "       PID: ${PARTICIPANT_1_PID}"
echo "       Log: ${PARTICIPANT_1_LOG}"

# Give first participant time to start listening
sleep 2

# Start participant-2 in background
echo "[2/3] Starting Participant 2..."
ONBOARDING -c "${CONFIG_2}" start > "${PARTICIPANT_2_LOG}" 2>&1 &
PARTICIPANT_2_PID=$!
echo "       PID: ${PARTICIPANT_2_PID}"
echo "       Log: ${PARTICIPANT_2_LOG}"

# Start participant-3 in background
echo "[3/3] Starting Participant 3..."
ONBOARDING -c "${CONFIG_3}" start > "${PARTICIPANT_3_LOG}" 2>&1 &
PARTICIPANT_3_PID=$!
echo "       PID: ${PARTICIPANT_3_PID}"
echo "       Log: ${PARTICIPANT_3_LOG}"

echo ""
echo "All processes started!"
echo ""
echo "=================================================="
echo "  Monitoring Progress"
echo "=================================================="
echo ""
echo "Use 'tail -f ${LOG_DIR}/*.log' to watch all logs"
echo "Or monitor individual logs:"
echo "  - Participant 1: tail -f ${PARTICIPANT_1_LOG}"
echo "  - Participant 2: tail -f ${PARTICIPANT_2_LOG}"
echo "  - Participant 3: tail -f ${PARTICIPANT_3_LOG}"
echo ""
echo "Press Ctrl+C to stop all processes"
echo ""

# Monitor processes
echo "Waiting for processes to complete..."
echo "(This will run until workflow completes or you press Ctrl+C)"
echo ""

# Function to check if process is still running
is_running() {
    kill -0 "$1" 2>/dev/null
}

# Monitor loop
while true; do
    # Check if any participant has stopped
    PARTICIPANT_1_RUNNING=$(is_running ${PARTICIPANT_1_PID} && echo "yes" || echo "no")
    PARTICIPANT_2_RUNNING=$(is_running ${PARTICIPANT_2_PID} && echo "yes" || echo "no")
    PARTICIPANT_3_RUNNING=$(is_running ${PARTICIPANT_3_PID} && echo "yes" || echo "no")

    # If all have stopped, exit
    if [ "$PARTICIPANT_1_RUNNING" = "no" ] && [ "$PARTICIPANT_2_RUNNING" = "no" ] && [ "$PARTICIPANT_3_RUNNING" = "no" ]; then
        echo "All participants have stopped"
        break
    fi

    # If any one stopped, report it (but keep monitoring others)
    if [ "$PARTICIPANT_1_RUNNING" = "no" ]; then
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
echo "Check the logs for details:"
echo "  - ${PARTICIPANT_1_LOG}"
echo "  - ${PARTICIPANT_2_LOG}"
echo "  - ${PARTICIPANT_3_LOG}"
echo ""
echo "Check workflow-data/ for generated files"
