#!/bin/bash

# Helper script to watch all participant logs in real-time

LOG_DIR="development/data/logs"

if [ ! -d "$LOG_DIR" ]; then
    echo "No logs directory found. Run ./start.sh first."
    exit 1
fi

echo "=================================================="
echo "  Watching Participant Logs"
echo "=================================================="
echo ""
echo "Press Ctrl+C to exit"
echo ""

# Use tail -f with color coding if available
if command -v multitail &> /dev/null; then
    multitail \
        -l "tail -f ${LOG_DIR}/participant-1.log" \
        -l "tail -f ${LOG_DIR}/participant-2.log" \
        -l "tail -f ${LOG_DIR}/participant-3.log"
else
    # Fallback to regular tail
    tail -f ${LOG_DIR}/*.log
fi
