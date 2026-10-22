#!/usr/bin/env bash
# Build Adult Hub Desktop.
# The default Windows build emits the EXE and adjacent libmpv-2.dll.
#
# Usage:
#   ./scripts/build.sh                   # Builds Windows EXE + runtime DLL
#   ./scripts/build.sh --windows         # Builds Windows EXE + runtime DLL
#   ./scripts/build.sh --linux           # Builds Linux binary, .deb, and AppImage
#   ./scripts/build.sh --deb             # Builds Debian / Ubuntu .deb package
#   ./scripts/build.sh --appimage        # Builds Linux AppImage
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="${1:---windows}"

case "${TARGET}" in
    --windows|windows|win)
        "${ROOT_DIR}/scripts/package-windows.sh"
        ;;
    --linux|linux)
        "${ROOT_DIR}/scripts/build-linux.sh" release
        "${ROOT_DIR}/scripts/package-deb.sh"
        "${ROOT_DIR}/scripts/package-appimage.sh"
        ;;
    --deb|deb)
        "${ROOT_DIR}/scripts/package-deb.sh"
        ;;
    --appimage|appimage)
        "${ROOT_DIR}/scripts/package-appimage.sh"
        ;;
    *)
        echo "Usage: $0 [--windows | --linux | --deb | --appimage]"
        exit 1
        ;;
esac
