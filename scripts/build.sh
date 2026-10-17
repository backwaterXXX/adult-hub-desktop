#!/usr/bin/env bash
# Universal build and package script for Adult Hub Desktop.
# Always outputs all packages and binaries to bin/
#
# Usage:
#   ./scripts/build.sh                   # Builds all release packages and signed manifest
#   ./scripts/build.sh --windows         # Builds Windows exe, zip, and NSIS installer
#   ./scripts/build.sh --linux           # Builds Linux binary, .deb, and AppImage
#   ./scripts/build.sh --deb             # Builds Debian / Ubuntu .deb package
#   ./scripts/build.sh --appimage        # Builds Linux AppImage
#   ./scripts/build.sh --installer       # Builds Windows NSIS installer
#   ./scripts/build.sh --manifest        # Generates and signs release manifest
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="${1:---all}"

case "${TARGET}" in
    --windows|windows|win)
        "${ROOT_DIR}/scripts/package-windows.sh"
        "${ROOT_DIR}/scripts/package-windows-installer.sh"
        ;;
    --installer|installer|nsis)
        "${ROOT_DIR}/scripts/package-windows-installer.sh"
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
    --manifest|manifest|sign)
        "${ROOT_DIR}/scripts/sign-release.sh"
        ;;
    --all|all|"")
        echo "=========================================="
        echo "1. Building Linux release binary..."
        echo "=========================================="
        "${ROOT_DIR}/scripts/build-linux.sh" release

        echo ""
        echo "=========================================="
        echo "2. Packaging Debian / Ubuntu .deb package..."
        echo "=========================================="
        "${ROOT_DIR}/scripts/package-deb.sh"

        echo ""
        echo "=========================================="
        echo "3. Packaging Linux AppImage..."
        echo "=========================================="
        "${ROOT_DIR}/scripts/package-appimage.sh"

        echo ""
        echo "=========================================="
        echo "4. Building Windows binary & zip package..."
        echo "=========================================="
        "${ROOT_DIR}/scripts/package-windows.sh"

        echo ""
        echo "=========================================="
        echo "5. Building Windows NSIS installer..."
        echo "=========================================="
        "${ROOT_DIR}/scripts/package-windows-installer.sh"

        echo ""
        echo "=========================================="
        echo "6. Generating & signing update manifest..."
        echo "=========================================="
        "${ROOT_DIR}/scripts/sign-release.sh"

        echo ""
        echo "=========================================="
        echo "All release artifacts ready in bin/:"
        echo "=========================================="
        ls -lh "${ROOT_DIR}/bin"
        ;;
    *)
        echo "Usage: $0 [--all | --windows | --linux | --deb | --appimage | --installer | --manifest]"
        exit 1
        ;;
esac
