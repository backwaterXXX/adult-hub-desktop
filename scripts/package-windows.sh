#!/usr/bin/env bash
# Build the Windows x64 executable and its adjacent libmpv runtime in bin/.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${ROOT_DIR}/bin"
CACHE_DIR="${ROOT_DIR}/.cache/windows"
TARGET="x86_64-pc-windows-gnu"
MPV_VER="20261011-git-accb7b2821"
MPV_URL="https://sourceforge.net/projects/mpv-player-windows/files/libmpv/mpv-dev-x86_64-${MPV_VER}.7z/download"
MPV_DLL="${CACHE_DIR}/libmpv-2.dll"
EXE_PATH="${ROOT_DIR}/desktop/target/${TARGET}/release/adult-hub-desktop.exe"

mkdir -p "${CACHE_DIR}" "${BIN_DIR}"
if [[ ! -s "${MPV_DLL}" ]]; then
    echo "==> Downloading the pinned libmpv runtime..."
    archive="${CACHE_DIR}/mpv-dev.7z"
    curl --fail --location --silent --show-error "${MPV_URL}" -o "${archive}"
    7z e "${archive}" libmpv-2.dll -o"${CACHE_DIR}" -y
    rm -f "${archive}"
fi

echo "==> Building the Windows executable (${TARGET})..."
"${ROOT_DIR}/dev/run.sh" cargo build --manifest-path desktop/Cargo.toml --target "${TARGET}" --release

if [[ ! -s "${EXE_PATH}" ]]; then
    echo "Error: ${EXE_PATH} was not created." >&2
    exit 1
fi

echo "==> Stripping debug symbols..."
"${ROOT_DIR}/dev/run.sh" x86_64-w64-mingw32-strip "desktop/target/${TARGET}/release/adult-hub-desktop.exe"

cp "${EXE_PATH}" "${BIN_DIR}/adult-hub-desktop.exe"
cp "${MPV_DLL}" "${BIN_DIR}/libmpv-2.dll"
echo "==> Created the Windows app files (no installer or archive):"
file "${BIN_DIR}/adult-hub-desktop.exe"
file "${BIN_DIR}/libmpv-2.dll"
du -h "${BIN_DIR}/adult-hub-desktop.exe" "${BIN_DIR}/libmpv-2.dll"
sha256sum "${BIN_DIR}/adult-hub-desktop.exe"
sha256sum "${BIN_DIR}/libmpv-2.dll"
