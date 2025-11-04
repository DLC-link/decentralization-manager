#!/bin/bash

set -eou pipefail

echo "cleaning up old files"
rm -rf out

echo "building..."
cargo build #--release

ONBOARDING() { ./target/release/grpc-test "$@"; }

export RUST_LOG="grpc_test=debug"

echo "uploading dars"
ONBOARDING -c configs/config-1.toml upload-dars
ONBOARDING -c configs/config-2.toml upload-dars
ONBOARDING -c configs/config-3.toml upload-dars

echo "generating keys and ids"
ONBOARDING -c configs/config-1.toml generate-keys
ONBOARDING -c configs/config-2.toml generate-keys
ONBOARDING -c configs/config-3.toml generate-keys

echo "creating proposals"
ONBOARDING -c configs/config-1.toml create-proposals
