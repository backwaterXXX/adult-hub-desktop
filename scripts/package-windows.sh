#!/usr/bin/env bash
# Package Adult Hub Desktop for Windows x64.
# Builds the stripped release binary, bundles libmpv-2.dll, sample media,
# and outputs dist/adult-hub-desktop-windows-x64/ and dist/adult-hub-desktop-windows-x64.zip.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="${ROOT_DIR}/dist"
OUT_DIR="${DIST_DIR}/adult-hub-desktop-windows-x64"
CACHE_DIR="${ROOT_DIR}/.cache/windows"
TARGET="x86_64-pc-windows-gnu"
MPV_VER="20261011-git-accb7b2821"
MPV_URL="https://sourceforge.net/projects/mpv-player-windows/files/libmpv/mpv-dev-x86_64-${MPV_VER}.7z/download"

echo "==> Building Windows release binary (${TARGET})..."
"${ROOT_DIR}/dev/run.sh" cargo build --manifest-path desktop/Cargo.toml --target "${TARGET}" --release

EXE_PATH="${ROOT_DIR}/desktop/target/${TARGET}/release/adult-hub-desktop.exe"
if [[ ! -f "${EXE_PATH}" ]]; then
    echo "Error: ${EXE_PATH} not found!" >&2
    exit 1
fi

echo "==> Stripping debug symbols..."
"${ROOT_DIR}/dev/run.sh" x86_64-w64-mingw32-strip "${EXE_PATH}"

mkdir -p "${CACHE_DIR}"
if [[ ! -f "${CACHE_DIR}/libmpv-2.dll" ]]; then
    echo "==> Downloading libmpv for Windows (${MPV_VER})..."
    curl -sL "${MPV_URL}" -o "${CACHE_DIR}/mpv-dev.7z"
    7z e "${CACHE_DIR}/mpv-dev.7z" libmpv-2.dll -o"${CACHE_DIR}" -y
    rm -f "${CACHE_DIR}/mpv-dev.7z"
fi

echo "==> Staging distribution package in ${OUT_DIR}..."
rm -rf "${OUT_DIR}"
mkdir -p "${OUT_DIR}"
cp "${EXE_PATH}" "${OUT_DIR}/"
cp "${CACHE_DIR}/libmpv-2.dll" "${OUT_DIR}/"

if [[ -f "${ROOT_DIR}/desktop/sample.mp4" ]]; then
    cp "${ROOT_DIR}/desktop/sample.mp4" "${OUT_DIR}/"
fi

cat << 'EOF' > "${OUT_DIR}/README.txt"
Adult Hub Desktop - Player Prototype (Windows x64)

Uruchomienie:
1. Uruchom adult-hub-desktop.exe
2. Kliknij "Sample" (załadowanie lokalnego pliku sample.mp4) lub wpisz własny URL/ścieżkę i kliknij "Load".
3. Skróty klawiszowe:
   - Spacja: Play / Pause
   - Strzałka w lewo / w prawo: -5s / +5s
   - f: Pełny ekran (Fullscreen) / Esc: Wyjście z pełnego ekranu
   - m: Wyciszenie (Mute / Unmute)
   - i: Statystyki odtwarzania (HUD)

Plik libmpv-2.dll musi znajdować się w tym samym folderze co adult-hub-desktop.exe.
EOF

echo "==> Creating ZIP archive..."
(cd "${DIST_DIR}" && rm -f adult-hub-desktop-windows-x64.zip && zip -r adult-hub-desktop-windows-x64.zip adult-hub-desktop-windows-x64)

echo "==> Done! Output: ${DIST_DIR}/adult-hub-desktop-windows-x64.zip"
