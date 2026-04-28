#!/bin/bash

# Distribute DAR files to all participants.
# Sourced by run.sh — expects env.sh variables to be available.

# Create temp file for the large JSON payload
DARS_REQUEST_FILE=$(mktemp)
TEMP_FILES+=("$DARS_REQUEST_FILE")

# Read and base64 encode DAR files
DAR1_B64=$(base64 -i "$DARS_DIR/governance-core-v0-rc3-0.1.0.dar")
DAR2_B64=$(base64 -i "$DARS_DIR/governance-token-custody-v0-rc3-0.1.0.dar")
DAR3_B64=$(base64 -i "$DARS_DIR/governance-utility-onboarding-v0-rc3-0.1.0.dar")

# Write JSON to temp file (avoids "argument list too long" error)
cat > "$DARS_REQUEST_FILE" <<EOF
{
  "dar_files": [
    {"filename": "governance-core-v0-rc3-0.1.0.dar", "data": "$DAR1_B64"},
    {"filename": "governance-token-custody-v0-rc3-0.1.0.dar", "data": "$DAR2_B64"},
    {"filename": "governance-utility-onboarding-v0-rc3-0.1.0.dar", "data": "$DAR3_B64"}
  ]
}
EOF

echo "Uploading DARs to participant-1 (local)..."
UPLOAD_RESPONSE=$(curl -sf -X POST "http://localhost:$P1_HTTP/dars/upload" \
    -H "Content-Type: application/json" \
    -d @"$DARS_REQUEST_FILE")
echo "  Response: $UPLOAD_RESPONSE"

echo "Distributing DARs to all participants..."
curl -sf -X POST "http://localhost:$P1_HTTP/dars/distribute" \
    -H "Content-Type: application/json" \
    -d @"$DARS_REQUEST_FILE"
echo ""

# Accept invitations on attestors in parallel
accept_invitation $P2_HTTP "participant-2" "Dars" &
PID_ACCEPT1=$!
accept_invitation $P3_HTTP "participant-3" "Dars" &
PID_ACCEPT2=$!
wait $PID_ACCEPT1 $PID_ACCEPT2

poll_status $P1_HTTP "dars/distribute/status"
