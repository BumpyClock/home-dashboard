#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

ignore_test=$(mktemp -d)
trap 'rm -rf "$ignore_test"' EXIT
git init --quiet --template= "$ignore_test"
cp .gitignore "$ignore_test/.gitignore"
git -C "$ignore_test" -c core.excludesFile=/dev/null check-ignore --no-index .local/calendar-sync/fixture.json >/dev/null
printf '%s\n' 'PASS repository-local privacy ignore'

cargo fmt --all -- --check
cargo test --locked -p calendar-sync -p frame-host-sync -p host-state -p host-command --all-features
cargo clippy --locked -p calendar-sync -p frame-host-sync -p host-state -p host-command --all-targets --all-features -- -D warnings
cargo build --locked -p frame-host-sync
python3 scripts/test-host-termination.py
python3 scripts/test-calendar-concurrency.py
python3 scripts/test-hot-path-tooling.py
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
