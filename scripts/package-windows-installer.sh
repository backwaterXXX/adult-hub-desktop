#!/usr/bin/env bash
# Build per-user Windows x64 NSIS Installer: bin/adult-hub-desktop-setup.exe
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${ROOT_DIR}/bin"
mkdir -p "${BIN_DIR}"

if [[ ! -f "${BIN_DIR}/adult-hub-desktop.exe" || ! -f "${BIN_DIR}/libmpv-2.dll" ]]; then
    echo "==> Windows binary missing, running package-windows.sh first..."
    "${ROOT_DIR}/scripts/package-windows.sh"
fi

echo "==> Compiling Windows NSIS installer with makensis..."
"${ROOT_DIR}/dev/run.sh" makensis \
    -DSRCDIR="/work" \
    -DBINDIR="/work/bin" \
    -DOUTDIR="/work/bin" \
    "/work/scripts/installer.nsi"

if [[ ! -f "${BIN_DIR}/adult-hub-desktop-setup.exe" ]]; then
    echo "Error: ${BIN_DIR}/adult-hub-desktop-setup.exe was not created!" >&2
    exit 1
fi

echo "==> Windows installer successfully created: ${BIN_DIR}/adult-hub-desktop-setup.exe"
ls -lh "${BIN_DIR}/adult-hub-desktop-setup.exe"
