#!/usr/bin/env bash
# Run the gate CI applies to desktop changes: format, lints, tests, and a
# Windows cross-compile so cfg(windows) code is checked on a Linux machine.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="${ROOT_DIR}/desktop/Cargo.toml"
WINDOWS_TARGET="x86_64-pc-windows-gnu"

echo "==> cargo fmt --check"
cargo fmt --manifest-path "${MANIFEST}" --all -- --check

echo "==> cargo clippy (warnings are errors)"
cargo clippy --locked --manifest-path "${MANIFEST}" --all-targets -- -D warnings

echo "==> cargo test"
cargo test --locked --manifest-path "${MANIFEST}"

# The updater's replacement helper only exists under cfg(windows); a Linux-only
# run cannot see it, so compile the Windows target when the toolchain has it.
if rustup target list --installed 2>/dev/null | grep -qx "${WINDOWS_TARGET}"; then
    echo "==> cargo check (${WINDOWS_TARGET})"
    cargo check --locked --manifest-path "${MANIFEST}" \
        --package adult-hub-desktop --target "${WINDOWS_TARGET}"
else
    echo "==> skipping the Windows target check (${WINDOWS_TARGET} is not installed)"
fi

echo "All desktop checks passed."
