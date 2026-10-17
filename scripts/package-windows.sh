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

Quick Start:
1. Launch adult-hub-desktop.exe.
2. Select a video from the catalog (Latest, Resume, Channels) or click "Sample" / enter URL / open local media.
3. Player & Remote Keyboard Shortcuts (Stage 9):
   - Space: Play / Pause
   - Up / Down Arrow: Adjust volume by +/- 5%
   - Left / Right Arrow: Seek backward / forward by configurable seek step (10s, 15s, 30s)
   - [ / ]: Seek backward / forward by 30 seconds
   - f / F: Toggle Fullscreen
   - Esc: Dismiss modal dialogs in hierarchy (Performer -> Studio -> Detail -> Fullscreen exit)
   - m / M / 0: Toggle Mute
   - i / I: Toggle Playback HUD & Statistics (also right-click on video canvas)
   - s / S / , / <: Cycle Playback Speed (1.0x, 1.25x, 1.5x, 2.0x)
   - b / B: Cycle Buffer Profile (Standard, FastStart, Large, LowMemory)
   - h / H: Toggle Hardware Decode (auto-safe with automatic software fallback)
   - n / N: Play next random catalog item
   - 1 / 2 / 3 / 4: Quick Navigation (1: Browse, 2: Channels, 3: Player, 4: Settings)
   - Ctrl + Shift + Q: Emergency Panic Exit (instantly hides window, mutes audio, checkpoints progress, exits)
   - Video Canvas Mouse Gestures: Double-click toggles fullscreen, mouse wheel adjusts volume (+/- 5%), right-click toggles HUD stats

Features & Capabilities:
- Visual Parity with Android TV: Dark OLED theme, category navigation, responsive video cards.
- Performer & Studio Profiles (Stage 7): Dedicated dialogs with biography, nationality, birth date, aliases, and filmography card lists; "More Like This" similar recommendations.
- Hierarchical Channels Browser (Stage 8): Left sidebar category filter, Telegram cursor pagination (next_offset_id), in-channel search.
- HTPC & Remote Navigation (Stage 9): Full keyboard shortcuts, canvas gestures, volume clamping, hierarchical Escape dismissal.
- Jellyfin Session Coordination: Start, progress updates every 10s (with 500ms coalescing), and stop reporting.
- Local Telegram Progress: Resume prompts for position >15s, watched badge upon >=90% playback.
- Resolver pHash Integration: Background video matching enqueue and status tracking.
- Settings Management: Non-secret JSON import/export compatible with Android TV version 1 schema.
- Content Filtering: Excluded tags and hidden channel management.
- Privacy & Diagnostics: Instant panic shutdown, history clearing, and redacted diagnostics export.
- Optimized Adult Content Player: Dedicated profile without subtitle or audio track selector overhead.
- Tonemapping: High quality HDR to SDR tonemapping targeting sRGB color space.

Note: libmpv-2.dll must reside in the same directory as adult-hub-desktop.exe.
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
