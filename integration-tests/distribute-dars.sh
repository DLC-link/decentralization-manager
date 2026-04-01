#!/bin/bash

# Distribute DAR files to all participants.
# Sourced by run.sh — expects env.sh variables to be available.

# Create temp file for the large JSON payload
DARS_REQUEST_FILE=$(mktemp)
TEMP_FILES+=("$DARS_REQUEST_FILE")

# Read and base64 encode DAR files
DAR1_B64=$(base64 -i "$DARS_DIR/governance-core-v0-rc1-0.1.0.dar")
DAR2_B64=$(base64 -i "$DARS_DIR/governance-token-custody-v0-rc1-0.1.0.dar")

# Write JSON to temp file (avoids "argument list too long" error)
cat > "$DARS_REQUEST_FILE" <<EOF
{
  "dar_files": [
    {"filename": "governance-core-v0-rc1-0.1.0.dar", "data": "$DAR1_B64"},
    {"filename": "governance-token-custody-v0-rc1-0.1.0.dar", "data": "$DAR2_B64"}
  ]
}
EOF

echo "Starting DARs upload on participant-1..."
curl -s -X POST "http://localhost:$P1_HTTP/dars" \
    -H "Content-Type: application/json" \
    -d @"$DARS_REQUEST_FILE"
echo ""

# Accept invitations on attestors in parallel
accept_invitation $P2_HTTP "participant-2" "Dars" &
PID_ACCEPT1=$!
accept_invitation $P3_HTTP "participant-3" "Dars" &
PID_ACCEPT2=$!
wait $PID_ACCEPT1 $PID_ACCEPT2

poll_status $P1_HTTP "dars/status"
