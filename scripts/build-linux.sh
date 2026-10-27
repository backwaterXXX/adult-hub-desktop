#!/usr/bin/env bash
# Build Adult Hub Desktop for Linux x86_64 and output to bin/
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${ROOT_DIR}/bin"
mkdir -p "${BIN_DIR}"

# Automatically increment patch version once per application build
if [[ "${SKIP_VERSION_BUMP:-0}" != "1" && "${ADULT_HUB_VERSION_BUMPED:-0}" != "1" && "${CI:-false}" != "true" && "${GITHUB_ACTIONS:-false}" != "true" ]]; then
    "${ROOT_DIR}/scripts/bump-version.sh"
    export ADULT_HUB_VERSION_BUMPED=1
fi

MODE="${1:-release}"
CARGO_FLAGS=()
if [[ "${MODE}" == "release" ]]; then
    CARGO_FLAGS+=(--release)
    TARGET_SUBDIR="release"
else
    TARGET_SUBDIR="debug"
fi

"${ROOT_DIR}/scripts/warn-target-size.sh"

echo "==> Building Linux (${MODE}) binary..."
"${ROOT_DIR}/dev/run.sh" cargo build --manifest-path desktop/Cargo.toml "${CARGO_FLAGS[@]}"

"${ROOT_DIR}/scripts/warn-target-size.sh"

SRC_BIN="${ROOT_DIR}/desktop/target/${TARGET_SUBDIR}/adult-hub-desktop"
if [[ ! -f "${SRC_BIN}" ]]; then
    echo "Error: ${SRC_BIN} not found!" >&2
    exit 1
fi

echo "==> Copying binary to ${BIN_DIR}/adult-hub-desktop..."
cp "${SRC_BIN}" "${BIN_DIR}/adult-hub-desktop"
chmod +x "${BIN_DIR}/adult-hub-desktop"

if [[ -f "${ROOT_DIR}/desktop/sample.mp4" && ! -f "${BIN_DIR}/sample.mp4" ]]; then
    cp "${ROOT_DIR}/desktop/sample.mp4" "${BIN_DIR}/sample.mp4"
fi

echo "==> Done! Linux binary: ${BIN_DIR}/adult-hub-desktop"
