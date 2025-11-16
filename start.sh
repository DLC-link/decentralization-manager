#!/bin/bash

set -eou pipefail

echo "cleaning up old files"
rm -rf workflow-data

echo "building..."
cargo build #--release

ONBOARDING() { ./target/debug/grpc-test "$@"; }

export RUST_LOG="grpc_test=debug"

# Configuration files
CONFIG_1="test-configs/node-1.toml"
CONFIG_2="test-configs/node-2.toml"
CONFIG_3="test-configs/node-3.toml"

echo "uploading dars"
ONBOARDING -c "${CONFIG_1}" upload-dars
ONBOARDING -c "${CONFIG_2}" upload-dars
ONBOARDING -c "${CONFIG_3}" upload-dars

echo "generating keys and ids"
ONBOARDING -c "${CONFIG_1}" generate-keys
ONBOARDING -c "${CONFIG_2}" generate-keys
ONBOARDING -c "${CONFIG_3}" generate-keys

sleep 5

echo "creating proposals"
ONBOARDING -c "${CONFIG_1}" create-proposals

echo "signing DNS proposals"
ONBOARDING -c "${CONFIG_1}" sign-dns-proposals
ONBOARDING -c "${CONFIG_2}" sign-dns-proposals
ONBOARDING -c "${CONFIG_3}" sign-dns-proposals

echo "submitting DNS proposals"
ONBOARDING -c "${CONFIG_1}" submit-dns-proposals

echo "signing P2P and PTK proposals"
ONBOARDING -c "${CONFIG_1}" sign-p2p-ptk-proposals
ONBOARDING -c "${CONFIG_2}" sign-p2p-ptk-proposals
ONBOARDING -c "${CONFIG_3}" sign-p2p-ptk-proposals

echo "submitting final P2P and PTK proposals"
ONBOARDING -c "${CONFIG_1}" submit-final-proposals

echo "preparing submissions"
ONBOARDING -c "${CONFIG_1}" prepare-submissions

echo "signing submissions from all attestors"
ONBOARDING -c "${CONFIG_1}" sign-submissions
ONBOARDING -c "${CONFIG_2}" sign-submissions
ONBOARDING -c "${CONFIG_3}" sign-submissions

echo "executing submissions"
ONBOARDING -c "${CONFIG_1}" execute-submissions
