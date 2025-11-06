#!/bin/bash

set -eou pipefail

echo "cleaning up old files"
rm -rf workflow-data

echo "building..."
cargo build #--release

ONBOARDING() { ./target/debug/grpc-test "$@"; }

export RUST_LOG="grpc_test=debug"

echo "uploading dars"
ONBOARDING -c configs/config-1.toml upload-dars
ONBOARDING -c configs/config-2.toml upload-dars
ONBOARDING -c configs/config-3.toml upload-dars

echo "generating keys and ids"
ONBOARDING -c configs/config-1.toml generate-keys
ONBOARDING -c configs/config-2.toml generate-keys
ONBOARDING -c configs/config-3.toml generate-keys

sleep 5

echo "creating proposals"
ONBOARDING -c configs/config-1.toml create-proposals

echo "signing DNS proposals"
ONBOARDING -c configs/config-1.toml sign-dns-proposals
ONBOARDING -c configs/config-2.toml sign-dns-proposals
ONBOARDING -c configs/config-3.toml sign-dns-proposals

echo "submitting DNS proposals"
ONBOARDING -c configs/config-1.toml submit-dns-proposals

echo "signing P2P and PTK proposals"
ONBOARDING -c configs/config-1.toml sign-p2p-ptk-proposals
ONBOARDING -c configs/config-2.toml sign-p2p-ptk-proposals
ONBOARDING -c configs/config-3.toml sign-p2p-ptk-proposals

echo "submitting final P2P and PTK proposals"
ONBOARDING -c configs/config-1.toml submit-final-proposals

echo "preparing submissions"
ONBOARDING -c configs/config-1.toml prepare-submissions

echo "signing submissions from all attestors"
ONBOARDING -c configs/config-1.toml sign-submissions
ONBOARDING -c configs/config-2.toml sign-submissions
ONBOARDING -c configs/config-3.toml sign-submissions

echo "executing submissions"
ONBOARDING -c configs/config-1.toml execute-submissions
