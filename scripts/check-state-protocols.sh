#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --all -- --check
cargo test --locked -p host-state
cargo test --locked -p photoframe_proto --no-default-features --features desktop,slint/renderer-software
cargo clippy --locked -p photoframe_proto --no-default-features --features desktop,slint/renderer-software --all-targets -- -D warnings
