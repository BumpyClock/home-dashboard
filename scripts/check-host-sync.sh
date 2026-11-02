#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --all -- --check
cargo test --locked -p calendar-sync -p frame-host-sync -p host-state -p host-command --all-features
cargo clippy --locked -p calendar-sync -p frame-host-sync -p host-state -p host-command --all-targets --all-features -- -D warnings
cargo build --locked -p frame-host-sync
python3 scripts/test-host-privacy.py

if [[ "${1:-}" == "--with-frame" ]]; then
    if [[ "$(uname -s)" == "Linux" ]]; then
        # Receiver runs before Slint initialization, so no display backend is needed.
        cargo build --locked -p photoframe_proto --no-default-features
        cargo test --locked -p photoframe_proto --no-default-features --features slint/renderer-software calendar
    else
        cargo build --locked -p photoframe_proto
        cargo test --locked -p photoframe_proto calendar
    fi
    python3 scripts/test-calendar-sync.py
fi
