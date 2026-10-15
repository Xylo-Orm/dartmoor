#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

cargo_runner=(cargo)
if command -v rtk >/dev/null 2>&1; then
    cargo_runner=(rtk cargo)
fi

"${cargo_runner[@]}" fmt --all -- --check
"${cargo_runner[@]}" clippy --locked --all-targets -- -D warnings
"${cargo_runner[@]}" test --locked --all-targets
"${cargo_runner[@]}" test --locked --doc
"${cargo_runner[@]}" test --manifest-path vendor/scap/Cargo.toml --lib

if [[ "$(uname -s)" == Linux ]]; then
    "${cargo_runner[@]}" fmt --manifest-path vendor/scap/Cargo.toml -- --check
    "${cargo_runner[@]}" clippy --manifest-path vendor/scap/Cargo.toml --all-targets -- -D warnings
fi

zigbee_python="${DARTMOOR_ZIGBEE_PYTHON:-target/zigbee-runtime/bin/python}"
if [[ -x "$zigbee_python" ]]; then
    "$zigbee_python" src/zigbee/test_helper.py
else
    printf '%s\n' 'Zigbee driver tests skipped: set DARTMOOR_ZIGBEE_PYTHON to an installed driver interpreter.'
fi

"${cargo_runner[@]}" build --locked --release --bins --examples
