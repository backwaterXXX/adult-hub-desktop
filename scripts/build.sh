#!/usr/bin/env bash
# Universal build script for Adult Hub Desktop.
# Always outputs binaries to bin/
#
# Usage:
#   ./scripts/build.sh           # Builds both Windows and Linux release binaries
#   ./scripts/build.sh --windows # Builds Windows release binary and zip package
#   ./scripts/build.sh --linux   # Builds Linux release binary
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="${1:---all}"

case "${TARGET}" in
    --windows|windows|win)
        "${ROOT_DIR}/scripts/package-windows.sh"
        ;;
    --linux|linux)
        "${ROOT_DIR}/scripts/build-linux.sh" release
        ;;
    --all|all|"")
        echo "=========================================="
        echo "Building Linux binary into bin/..."
        echo "=========================================="
        "${ROOT_DIR}/scripts/build-linux.sh" release

        echo ""
        echo "=========================================="
        echo "Building Windows binary into bin/..."
        echo "=========================================="
        "${ROOT_DIR}/scripts/package-windows.sh"
        ;;
    *)
        echo "Usage: $0 [--all | --windows | --linux]"
        exit 1
        ;;
esac
