#!/usr/bin/env bash
# Package Adult Hub Desktop for Windows x64.
# Builds the stripped release binary, bundles libmpv-2.dll, sample media,
# and outputs directly to bin/ and bin/adult-hub-desktop-windows-x64.zip.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${ROOT_DIR}/bin"
DIST_DIR="${ROOT_DIR}/dist"
OUT_DIR="${BIN_DIR}/adult-hub-desktop-windows-x64"
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
"${ROOT_DIR}/dev/run.sh" x86_64-w64-mingw32-strip "desktop/target/${TARGET}/release/adult-hub-desktop.exe"

mkdir -p "${CACHE_DIR}"
if [[ ! -f "${CACHE_DIR}/libmpv-2.dll" ]]; then
    echo "==> Downloading libmpv for Windows (${MPV_VER})..."
    curl -sL "${MPV_URL}" -o "${CACHE_DIR}/mpv-dev.7z"
    7z e "${CACHE_DIR}/mpv-dev.7z" libmpv-2.dll -o"${CACHE_DIR}" -y
    rm -f "${CACHE_DIR}/mpv-dev.7z"
fi

echo "==> Staging distribution package in ${BIN_DIR}..."
mkdir -p "${BIN_DIR}"
rm -rf "${OUT_DIR}"
mkdir -p "${OUT_DIR}"

cp "${EXE_PATH}" "${OUT_DIR}/"
cp "${CACHE_DIR}/libmpv-2.dll" "${OUT_DIR}/"

# Also place directly in bin/ for quick access
cp "${EXE_PATH}" "${BIN_DIR}/adult-hub-desktop.exe"
cp "${CACHE_DIR}/libmpv-2.dll" "${BIN_DIR}/libmpv-2.dll"

if [[ -f "${ROOT_DIR}/desktop/sample.mp4" ]]; then
    cp "${ROOT_DIR}/desktop/sample.mp4" "${OUT_DIR}/"
    cp "${ROOT_DIR}/desktop/sample.mp4" "${BIN_DIR}/sample.mp4"
fi

cat << 'EOF' > "${OUT_DIR}/README.txt"
Adult Hub Desktop - Player (Windows x64)

Uruchomienie:
1. Uruchom adult-hub-desktop.exe
2. Wybierz wideo z katalogu (Najnowsze, Wznów, Kanały) lub kliknij "Sample" / wpisz URL / otwórz plik lokalny.
3. Skróty klawiszowe odtwarzacza:
   - Spacja: Play / Pause
   - Strzałka w lewo / w prawo: -5s / +5s
   - f: Pełny ekran (Fullscreen) / Esc: Wyjście z pełnego ekranu
   - m: Wyciszenie (Mute / Unmute)
   - i: Statystyki odtwarzania (HUD)
   - s: Zmiana prędkości odtwarzania (1.0x, 1.25x, 1.5x, 2.0x)
   - b: Profil buforowania (Standard, FastStart, Large, LowMemory)
   - h: Przełącznik dekodera sprzętowego (HW Decode: auto-safe / no z auto-fallbackiem)
   - n: Odtwarzaj losowy następny film z katalogu

Funkcje odtwarzania (Stage 3):
- Pełna synchronizacja sesji Jellyfin (Start, Progress co 10s / coalesce 500ms, Stop)
- Lokalne zapamiętywanie postępu wideo Telegram (wznowienie od >15s, zaliczenie obejrzenia przy >=90%)
- Autoplay: automatyczne odtwarzanie kolejnego filmu po zakończeniu
- Dedykowany profil bez napisów i wyboru ścieżek dźwiękowych (zoptymalizowany dla treści adult)
- Mapowanie tonów HDR do SDR (target sRGB)
- Automatyczny fallback na dekoder programowy przy błędzie akceleracji GPU

Plik libmpv-2.dll musi znajdować się w tym samym folderze co adult-hub-desktop.exe.
EOF
cp "${OUT_DIR}/README.txt" "${BIN_DIR}/README.txt"

echo "==> Creating ZIP archive in ${BIN_DIR}..."
(cd "${BIN_DIR}" && rm -f adult-hub-desktop-windows-x64.zip && zip -r adult-hub-desktop-windows-x64.zip adult-hub-desktop-windows-x64)

# Also maintain dist/ for backwards compatibility
mkdir -p "${DIST_DIR}"
cp "${BIN_DIR}/adult-hub-desktop-windows-x64.zip" "${DIST_DIR}/"
rm -rf "${DIST_DIR}/adult-hub-desktop-windows-x64"
cp -r "${OUT_DIR}" "${DIST_DIR}/"

echo "==> Done! Binaries available in: ${BIN_DIR}/"
echo "    - ${BIN_DIR}/adult-hub-desktop.exe"
echo "    - ${BIN_DIR}/adult-hub-desktop-windows-x64.zip"
