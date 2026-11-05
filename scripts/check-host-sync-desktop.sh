#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

# Linux needs the Tauri 2 system packages documented in host-sync/README.md.
cargo fmt --all -- --check
python3 scripts/sync-host-sync-design.py --check
node host-sync/desktop/tests/frontend.cjs
cargo test --locked -p frame-host-sync -p frame-host-sync-desktop
cargo clippy --locked -p frame-host-sync-desktop --all-targets -- -D warnings
cargo build --locked -p frame-host-sync-desktop
