#!/bin/bash

# Helper script to watch all Noise protocol test logs in real-time

if [ ! -d "logs" ]; then
    echo "No logs directory found. Run ./start.sh first."
    exit 1
fi

echo "=================================================="
echo "  Watching Noise Protocol Logs"
echo "=================================================="
echo ""
echo "Press Ctrl+C to exit"
echo ""

# Use tail -f with color coding if available
if command -v multitail &> /dev/null; then
    multitail \
        -l "tail -f logs/coordinator.log" \
        -l "tail -f logs/attestor-2.log" \
        -l "tail -f logs/attestor-3.log"
else
    # Fallback to regular tail
    tail -f logs/*.log
fi
